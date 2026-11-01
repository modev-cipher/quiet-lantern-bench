//! Base-URL admission policy (VOICE_AGENT §7, ARCHITECTURE §8.1): HTTPS-only
//! with an explicit localhost exemption, no embedded credentials, no
//! query/fragment. The canonical form produced here is the identity keys are
//! stored under, so two spellings of one endpoint cannot yield two entries.

use std::fmt;

use reqwest::Url;

#[derive(Debug, PartialEq, Eq)]
pub enum UrlPolicyError {
    NotAUrl,
    NotHttps,
    EmbeddedCredentials,
    QueryOrFragment,
}

impl fmt::Display for UrlPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrlPolicyError::NotAUrl => f.write_str("that is not a valid URL"),
            UrlPolicyError::NotHttps => f.write_str(
                "custom endpoints must use https:// (plain http is allowed only for localhost)",
            ),
            UrlPolicyError::EmbeddedCredentials => f.write_str(
                "URLs with embedded credentials are not accepted; keys are stored separately",
            ),
            UrlPolicyError::QueryOrFragment => {
                f.write_str("a base URL cannot carry a query string or fragment")
            }
        }
    }
}

pub fn canonical_base_url(raw: &str) -> Result<String, UrlPolicyError> {
    let url = Url::parse(raw.trim()).map_err(|_| UrlPolicyError::NotAUrl)?;
    match url.scheme() {
        "https" | "http" => {}
        _ => return Err(UrlPolicyError::NotHttps),
    }
    let host = url.host_str().ok_or(UrlPolicyError::NotAUrl)?;
    if url.scheme() == "http" && !is_loopback_host(host) {
        return Err(UrlPolicyError::NotHttps);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlPolicyError::EmbeddedCredentials);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(UrlPolicyError::QueryOrFragment);
    }
    let mut canonical = format!("{}://{}", url.scheme(), host.to_ascii_lowercase());
    if let Some(port) = url.port() {
        canonical.push_str(&format!(":{port}"));
    }
    let path = url.path().trim_end_matches('/');
    canonical.push_str(path);
    Ok(canonical)
}

/// Loopback classification over an already-canonical base URL (charter T14:
/// parsed canonical address only, never display name; exact loopback, never
/// arbitrary LAN/private ranges).
pub fn is_loopback_base_url(canonical: &str) -> bool {
    Url::parse(canonical)
        .ok()
        .and_then(|url| url.host_str().map(is_loopback_host))
        .unwrap_or(false)
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
    {
        Ok(addr) => addr.is_loopback(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_urls_canonicalize_with_case_folded_host_and_no_trailing_slash() {
        assert_eq!(
            canonical_base_url(" https://API.Example.dev/openai/v1/ ").unwrap(),
            "https://api.example.dev/openai/v1"
        );
        assert_eq!(
            canonical_base_url("https://api.deepgram.com").unwrap(),
            "https://api.deepgram.com"
        );
    }

    #[test]
    fn plain_http_is_refused_except_for_loopback() {
        assert_eq!(
            canonical_base_url("http://api.example.dev/v1"),
            Err(UrlPolicyError::NotHttps),
            "a key sent over plain http crosses the network in cleartext"
        );
        assert_eq!(
            canonical_base_url("http://localhost:11434/v1").unwrap(),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            canonical_base_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            canonical_base_url("http://[::1]:8080/v1").unwrap(),
            "http://[::1]:8080/v1"
        );
    }

    #[test]
    fn a_public_ip_is_not_loopback_no_matter_how_local_it_looks() {
        assert_eq!(
            canonical_base_url("http://192.168.1.10:11434/v1"),
            Err(UrlPolicyError::NotHttps),
            "LAN addresses still cross a wire someone else can be on"
        );
    }

    #[test]
    fn embedded_credentials_are_rejected_outright() {
        assert_eq!(
            canonical_base_url("https://user:pass@api.example.dev/v1"),
            Err(UrlPolicyError::EmbeddedCredentials)
        );
    }

    #[test]
    fn query_strings_and_fragments_cannot_ride_along() {
        assert_eq!(
            canonical_base_url("https://api.example.dev/v1?key=abc"),
            Err(UrlPolicyError::QueryOrFragment),
            "a query param is where providers hide tokens; a base URL carrying one would \
             smuggle a credential into plain-text metadata"
        );
        assert_eq!(
            canonical_base_url("https://api.example.dev/v1#frag"),
            Err(UrlPolicyError::QueryOrFragment)
        );
    }

    #[test]
    fn garbage_is_not_a_url() {
        assert_eq!(
            canonical_base_url("not a url"),
            Err(UrlPolicyError::NotAUrl)
        );
        assert_eq!(
            canonical_base_url("file:///etc/passwd"),
            Err(UrlPolicyError::NotHttps)
        );
    }

    #[test]
    fn two_spellings_of_one_endpoint_collapse_to_one_identity() {
        let a = canonical_base_url("https://Api.Example.dev/v1/").unwrap();
        let b = canonical_base_url("https://api.example.dev/v1").unwrap();
        assert_eq!(
            a, b,
            "distinct identities for one endpoint would split the stored key from the \
             consent recorded for it"
        );
    }
}
