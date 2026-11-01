//! Key-validation probes (VOICE_AGENT §7): one capability-cheap authenticated
//! request per provider, mapped to exactly three key states — active /
//! invalid / out-of-quota — plus `Unchecked` when nothing could be learned.
//!
//! Non-billable by request shape, provably: every probe is a bodyless GET
//! against a metadata endpoint ([`ProbeRequest`] has no body field, and the
//! tests below pin each URL to its fixture and reject synthesis /
//! transcription / completion path shapes). Probes run only from the two
//! spec-approved triggers (settings entry, explicit refresh) — the manager
//! owns that discipline and `hygiene` greps this module for timers.
//!
//! Provider provenance (AGENTS.md hard rule — endpoints verified against live
//! official docs, never memory; re-verified 2026-08-20):
//! - Gemini: GET /v1beta/models — https://ai.google.dev/api/models; header
//!   auth `x-goog-api-key` per https://ai.google.dev/gemini-api/docs/api-key
//!   (header form chosen over the query-param form: connect URLs must stay
//!   credential-free).
//! - Deepgram: GET /v1/projects, `Authorization: Token` —
//!   https://developers.deepgram.com/reference/manage/projects/list
//! - Cartesia: GET /voices, `Authorization: Bearer` + required dated
//!   `Cartesia-Version` header (single allowed value 2026-08-14) —
//!   https://docs.cartesia.ai/api-reference/voices/list
//! - OpenAI-compatible (protocol family, custom base URLs): GET {base}/models,
//!   `Authorization: Bearer` — shape verified via a covered example at
//!   https://console.groq.com/docs/api-reference
//! - Anthropic: GET /v1/models?limit=1, `x-api-key` + `anthropic-version: 2023-06-01` —
//!   https://platform.claude.com/docs/en/api/models-list (verified 2026-08-28).
//!
//! Fixtures: `fixtures/voice_probes/*.json` pin each endpoint and the
//! 200/401/403/429-quota response shapes the classifier is held to.

use std::fmt;
use std::time::Duration;

use serde::Serialize;

use super::secret::ApiKey;
use zeroize::Zeroizing;

pub const CARTESIA_VERSION: &str = "2026-08-14";
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const BODY_CAP_CHARS: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Gemini,
    Deepgram,
    Cartesia,
    OpenaiCompat,
    /// Batch Script LLM provider (provider-flexibility S-0). Native Messages
    /// adapter lands in its own slice; this row exists so key custody,
    /// probes, and connections share the one VA3 store from day one.
    Anthropic,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Llm,
    Tts,
    Stt,
}

impl Provider {
    pub const ALL: [Provider; 5] = [
        Provider::Gemini,
        Provider::Deepgram,
        Provider::Cartesia,
        Provider::OpenaiCompat,
        Provider::Anthropic,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::Deepgram => "deepgram",
            Provider::Cartesia => "cartesia",
            Provider::OpenaiCompat => "openai_compat",
            Provider::Anthropic => "anthropic",
        }
    }

    pub fn from_id(id: &str) -> Option<Provider> {
        Provider::ALL.into_iter().find(|p| p.id() == id)
    }

    pub fn default_base_url(self) -> Option<&'static str> {
        match self {
            Provider::Gemini => Some("https://generativelanguage.googleapis.com"),
            Provider::Deepgram => Some("https://api.deepgram.com"),
            Provider::Cartesia => Some("https://api.cartesia.ai"),
            Provider::OpenaiCompat => None,
            Provider::Anthropic => Some("https://api.anthropic.com"),
        }
    }

    pub fn capabilities(self) -> &'static [Capability] {
        match self {
            Provider::Gemini => &[Capability::Llm],
            Provider::Deepgram => &[Capability::Stt],
            Provider::Cartesia => &[Capability::Tts],
            Provider::OpenaiCompat => &[Capability::Llm],
            Provider::Anthropic => &[Capability::Llm],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    Active,
    Invalid,
    OutOfQuota,
    Unchecked,
}

impl KeyState {
    pub fn from_fixture_label(label: &str) -> Option<KeyState> {
        match label {
            "active" => Some(KeyState::Active),
            "invalid" => Some(KeyState::Invalid),
            "out_of_quota" => Some(KeyState::OutOfQuota),
            "unchecked" => Some(KeyState::Unchecked),
            _ => None,
        }
    }
}

pub struct ProbeRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(&'static str, Zeroizing<String>)>,
}

pub fn probe_request(provider: Provider, base_url: &str, key: &ApiKey) -> ProbeRequest {
    let (path_and_query, headers): (&str, Vec<(&'static str, Zeroizing<String>)>) = match provider {
        Provider::Gemini => (
            "/v1beta/models?pageSize=1",
            vec![("x-goog-api-key", Zeroizing::new(key.expose().to_owned()))],
        ),
        Provider::Deepgram => (
            "/v1/projects",
            vec![(
                "authorization",
                Zeroizing::new(format!("Token {}", key.expose())),
            )],
        ),
        Provider::Cartesia => (
            "/voices?limit=1",
            vec![
                (
                    "authorization",
                    Zeroizing::new(format!("Bearer {}", key.expose())),
                ),
                (
                    "cartesia-version",
                    Zeroizing::new(CARTESIA_VERSION.to_owned()),
                ),
            ],
        ),
        Provider::OpenaiCompat => (
            "/models",
            vec![(
                "authorization",
                Zeroizing::new(format!("Bearer {}", key.expose())),
            )],
        ),
        // Non-billable models list (https://platform.claude.com/docs/en/api/models-list,
        // checked 2026-08-28): GET /v1/models with x-api-key + anthropic-version.
        Provider::Anthropic => (
            "/v1/models?limit=1",
            vec![
                ("x-api-key", Zeroizing::new(key.expose().to_owned())),
                ("anthropic-version", Zeroizing::new("2023-06-01".to_owned())),
            ],
        ),
    };
    ProbeRequest {
        method: "GET",
        url: format!(
            "{}{path_and_query}",
            if provider == Provider::OpenaiCompat && base_url == "https://api.openai.com" {
                "https://api.openai.com/v1"
            } else {
                base_url
            }
        ),
        headers,
    }
}

pub fn classify(provider: Provider, status: u16, body: &str) -> KeyState {
    match status {
        200..=299 => KeyState::Active,
        401 | 403 => KeyState::Invalid,
        402 | 429 => KeyState::OutOfQuota,
        400 if provider == Provider::Gemini && body.contains("API_KEY_INVALID") => {
            KeyState::Invalid
        }
        _ => KeyState::Unchecked,
    }
}

pub struct ProbeOutcome {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeError {
    Timeout,
    Connect,
    Transport,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::Timeout => f.write_str("the validation check timed out"),
            ProbeError::Connect => f.write_str("could not connect to the provider"),
            ProbeError::Transport => f.write_str("the validation check failed in transport"),
        }
    }
}

pub fn probe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PROBE_TIMEOUT)
        .build()
        .expect("a client with static, valid options must build")
}

pub async fn run_probe(
    client: &reqwest::Client,
    request: &ProbeRequest,
) -> Result<ProbeOutcome, ProbeError> {
    let mut builder = client.get(&request.url);
    for (name, value) in &request.headers {
        builder = builder.header(*name, value.as_str());
    }
    let response = builder.send().await.map_err(|err| {
        if err.is_timeout() {
            ProbeError::Timeout
        } else if err.is_connect() {
            ProbeError::Connect
        } else {
            ProbeError::Transport
        }
    })?;
    let status = response.status().as_u16();
    let body: String = response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(BODY_CAP_CHARS)
        .collect();
    Ok(ProbeOutcome { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: [(&str, Provider); 5] = [
        (
            include_str!("../../../fixtures/voice_probes/gemini.json"),
            Provider::Gemini,
        ),
        (
            include_str!("../../../fixtures/voice_probes/deepgram.json"),
            Provider::Deepgram,
        ),
        (
            include_str!("../../../fixtures/voice_probes/cartesia.json"),
            Provider::Cartesia,
        ),
        (
            include_str!("../../../fixtures/voice_probes/openai_compat.json"),
            Provider::OpenaiCompat,
        ),
        (
            include_str!("../../../fixtures/voice_probes/anthropic.json"),
            Provider::Anthropic,
        ),
    ];

    const BILLABLE_PATH_SHAPES: [&str; 15] = [
        "/speak",
        "/listen",
        "/agent",
        "generatecontent",
        "/chat/completions",
        "/completions",
        "/responses",
        "/audio/",
        "/transcriptions",
        "/translations",
        "/tts",
        "/synthesize",
        "/realtime",
        "/embeddings",
        "/images",
    ];

    const KEY: &str = "PROBE-SECRET-KEY-90ef";

    fn fixture(provider: Provider) -> serde_json::Value {
        let (raw, _) = FIXTURES
            .iter()
            .find(|(_, p)| *p == provider)
            .expect("every provider has a fixture");
        serde_json::from_str(raw).expect("probe fixtures must be valid JSON")
    }

    fn request_for(provider: Provider, fixture: &serde_json::Value) -> ProbeRequest {
        let base = provider
            .default_base_url()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                fixture["probe"]["base_url_example"]
                    .as_str()
                    .expect("custom-base providers pin an example base URL")
                    .to_owned()
            });
        probe_request(provider, &base, &ApiKey::new(KEY.into()))
    }

    #[test]
    fn every_probe_matches_its_fixture_pinned_endpoint_exactly() {
        for (_, provider) in FIXTURES {
            let fixture = fixture(provider);
            let request = request_for(provider, &fixture);
            assert_eq!(
                request.url,
                fixture["probe"]["url"].as_str().unwrap(),
                "{}: the probe endpoint is pinned by fixture (with the doc URL and check date \
                 beside it); drift here means someone repointed the probe without re-verifying \
                 the docs",
                provider.id()
            );
            assert_eq!(request.method, fixture["probe"]["method"].as_str().unwrap());
        }
    }

    #[test]
    fn every_probe_is_a_bodyless_get_with_no_billable_path_shape() {
        for (_, provider) in FIXTURES {
            let fixture = fixture(provider);
            let request = request_for(provider, &fixture);
            assert_eq!(
                request.method,
                "GET",
                "{}: anything but GET could carry a synthesis/transcription/completion body",
                provider.id()
            );
            let url = request.url.to_ascii_lowercase();
            for shape in BILLABLE_PATH_SHAPES {
                assert!(
                    !url.contains(shape),
                    "{}: probe URL {url} matches billable path shape `{shape}` — a probe must \
                     never be able to bill the user's key",
                    provider.id()
                );
            }
        }
    }

    #[test]
    fn the_key_travels_only_in_the_fixture_named_auth_headers_never_the_url() {
        for (_, provider) in FIXTURES {
            let fixture = fixture(provider);
            let request = request_for(provider, &fixture);
            assert!(
                !request.url.contains(KEY),
                "{}: connect URLs are never logged precisely because some providers put tokens \
                 in them; our own probes must not",
                provider.id()
            );
            let pinned: Vec<&str> = fixture["probe"]["auth_headers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let sent: Vec<&str> = request.headers.iter().map(|(name, _)| *name).collect();
            assert_eq!(
                sent,
                pinned,
                "{}: the exact header set is part of the pinned probe shape",
                provider.id()
            );
            assert!(
                request.headers.iter().any(|(_, value)| value.contains(KEY)),
                "{}: a probe that never sends the key validates nothing",
                provider.id()
            );
        }
    }

    #[test]
    fn every_fixture_response_classifies_to_its_expected_state() {
        for (_, provider) in FIXTURES {
            let fixture = fixture(provider);
            for (name, response) in fixture["responses"].as_object().unwrap() {
                let status = response["status"].as_u64().unwrap() as u16;
                let body = response["body"].as_str().unwrap();
                let expected = KeyState::from_fixture_label(response["expect"].as_str().unwrap())
                    .expect("fixture expectations use the canonical state labels");
                assert_eq!(
                    classify(provider, status, body),
                    expected,
                    "{}: response fixture `{name}` (HTTP {status}) must map to {expected:?}",
                    provider.id()
                );
            }
        }
    }

    #[test]
    fn statuses_that_prove_nothing_leave_the_key_unchecked() {
        for outcome in [(500, ""), (503, ""), (302, ""), (400, "malformed pageSize")] {
            assert_eq!(
                classify(Provider::OpenaiCompat, outcome.0, outcome.1),
                KeyState::Unchecked,
                "HTTP {} says nothing about the key itself; claiming invalid would tell the \
                 user to rotate a working key",
                outcome.0
            );
        }
    }

    #[test]
    fn gemini_reports_invalid_keys_as_400_and_only_that_shape_maps_to_invalid() {
        assert_eq!(
            classify(Provider::Gemini, 400, "\"reason\":\"API_KEY_INVALID\""),
            KeyState::Invalid
        );
        assert_eq!(
            classify(Provider::Gemini, 400, "\"reason\":\"INVALID_ARGUMENT\""),
            KeyState::Unchecked
        );
        assert_eq!(
            classify(
                Provider::OpenaiCompat,
                400,
                "\"reason\":\"API_KEY_INVALID\""
            ),
            KeyState::Unchecked,
            "the 400-as-invalid quirk is Gemini's wire shape, not a general truth"
        );
    }

    #[test]
    fn probe_errors_render_without_url_host_or_key_material() {
        for err in [
            ProbeError::Timeout,
            ProbeError::Connect,
            ProbeError::Transport,
        ] {
            for rendered in [format!("{err}"), format!("{err:?}")] {
                for forbidden in [KEY, "http", "://", "api."] {
                    assert!(
                        !rendered.contains(forbidden),
                        "probe errors surface in UI toasts and diagnostics; hosts and keys \
                         must not ride along: {rendered}"
                    );
                }
            }
        }
    }

    async fn one_shot_server(
        status: &'static str,
        headers: &'static [(&'static str, &'static str)],
        body: &'static str,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut sock, _) = listener.accept().await.unwrap();
            // Drain the request first: closing a socket with unread inbound
            // data is an abortive close (RST), and on Windows the RST
            // discards the response before the client reads it.
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            let head_end = loop {
                let read = sock.read(&mut chunk).await.unwrap();
                assert!(read > 0, "the client closed before sending its request");
                request.extend_from_slice(&chunk[..read]);
                if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let body_len = String::from_utf8_lossy(&request[..head_end])
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len() < head_end + body_len {
                let read = sock.read(&mut chunk).await.unwrap();
                assert!(read > 0, "the client closed mid-body");
                request.extend_from_slice(&chunk[..read]);
            }
            crate::test_support::mock_http::write_head(
                &mut sock,
                status,
                headers,
                Some(body.len()),
            )
            .await
            .unwrap();
            sock.write_all(body.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_replayed_fixture_response_flows_through_the_executor_into_invalid() {
        let fixture = fixture(Provider::OpenaiCompat);
        let body: &'static str = Box::leak(
            fixture["responses"]["unauthorized_401"]["body"]
                .as_str()
                .unwrap()
                .to_owned()
                .into_boxed_str(),
        );
        let base = one_shot_server("401 Unauthorized", &[], body).await;
        let request = probe_request(Provider::OpenaiCompat, &base, &ApiKey::new(KEY.into()));
        let outcome = run_probe(&probe_client(), &request).await.unwrap();
        assert_eq!(outcome.status, 401);
        assert_eq!(
            classify(Provider::OpenaiCompat, outcome.status, &outcome.body),
            KeyState::Invalid
        );
    }

    #[tokio::test]
    async fn redirects_are_never_followed_so_a_key_cannot_leak_to_another_host() {
        let base = one_shot_server(
            "302 Found",
            &[("Location", "https://attacker.example/collect")],
            "",
        )
        .await;
        let request = probe_request(Provider::OpenaiCompat, &base, &ApiKey::new(KEY.into()));
        let outcome = run_probe(&probe_client(), &request).await.unwrap();
        assert_eq!(
            outcome.status, 302,
            "a followed redirect would replay the auth header at whatever host the response \
             names; the status must surface un-followed"
        );
        assert_eq!(
            classify(Provider::OpenaiCompat, outcome.status, &outcome.body),
            KeyState::Unchecked
        );
    }

    #[test]
    fn the_cartesia_version_header_is_the_pinned_dated_value() {
        let fixture = fixture(Provider::Cartesia);
        assert_eq!(
            fixture["cartesia_version"].as_str().unwrap(),
            CARTESIA_VERSION,
            "Cartesia requires the dated version header and the docs allow exactly one value; \
             the fixture and the constant must move together"
        );
    }
}
