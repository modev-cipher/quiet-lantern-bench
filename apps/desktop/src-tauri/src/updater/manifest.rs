//! The signed manifest. On the wire it is one JSON envelope
//! `{ "payload": "<base64 of the manifest JSON>", "signature": "<base64>" }`
//! so the bytes that were signed are the bytes that are parsed, atomically.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::error::UpdateError;
use super::signing::PublicKey;

/// Hard cap on the envelope body; a manifest is a few hundred bytes.
pub const MANIFEST_MAX_BYTES: usize = 64 * 1024;

#[derive(Deserialize, Serialize, Debug)]
pub struct Envelope {
    pub payload: String,
    pub signature: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub url: String,
    /// Hex sha256 of the artifact bytes.
    pub sha256: String,
    pub size: u64,
    /// Ed25519 over the raw digest under the artifact prefix.
    pub signature: String,
    /// "nsis" | "appimage" | "deb" — decides the install handoff.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub pub_date: Option<String>,
    /// Publisher kill-switch: true = every client shows nothing and logs once.
    #[serde(default)]
    pub pause_updates: bool,
    /// Where "Get the update" sends a user whose install cannot self-replace.
    #[serde(default)]
    pub release_url: Option<String>,
    #[serde(default)]
    pub platforms: BTreeMap<String, Artifact>,
}

/// Decode the envelope, verify the payload with the pinned key, then parse.
/// Parsing happens strictly after verification: unsigned bytes never reach serde.
pub fn verify_and_parse(envelope_bytes: &[u8], key: &PublicKey) -> Result<Manifest, UpdateError> {
    if envelope_bytes.len() > MANIFEST_MAX_BYTES {
        return Err(UpdateError::ManifestTooLarge {
            bytes: envelope_bytes.len(),
            cap: MANIFEST_MAX_BYTES,
        });
    }
    let envelope: Envelope =
        serde_json::from_slice(envelope_bytes).map_err(|_| UpdateError::ManifestSignature)?;
    let payload = B64
        .decode(envelope.payload.trim())
        .map_err(|_| UpdateError::ManifestSignature)?;
    key.verify_manifest(&payload, &envelope.signature)?;
    let manifest: Manifest = serde_json::from_slice(&payload)
        .map_err(|e| UpdateError::ManifestMalformed(e.to_string()))?;
    semver::Version::parse(&manifest.version)
        .map_err(|_| UpdateError::BadVersion(manifest.version.clone()))?;
    Ok(manifest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Newer,
    Same,
    Older,
}

/// Strict semver PRECEDENCE comparison (build metadata ignored, as the spec says):
/// `1.2.3+b2` is not newer than `1.2.3+b1`. Only `Newer` may proceed to a download.
pub fn compare(current: &str, offered: &str) -> Result<Offer, UpdateError> {
    let mut cur = semver::Version::parse(current)
        .map_err(|_| UpdateError::BadVersion(current.to_string()))?;
    let mut off = semver::Version::parse(offered)
        .map_err(|_| UpdateError::BadVersion(offered.to_string()))?;
    cur.build = semver::BuildMetadata::EMPTY;
    off.build = semver::BuildMetadata::EMPTY;
    Ok(match off.cmp_precedence(&cur) {
        std::cmp::Ordering::Greater => Offer::Newer,
        std::cmp::Ordering::Equal => Offer::Same,
        std::cmp::Ordering::Less => Offer::Older,
    })
}

/// Key into `platforms`; the artifact `kind` is chosen by the install planner.
pub fn platform_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// https everywhere; plaintext http only to loopback (test fixtures).
pub fn url_is_acceptable(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => matches!(
            url.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("[::1]")
        ),
        _ => false,
    }
}

/// Hosts a GitHub Releases `latest/download` redirect chain legitimately lands on.
pub const GITHUB_ASSET_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

fn is_loopback(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("[::1]")
    )
}

/// Redirect policy (review finding F3): at most 5 hops; every hop must be https (loopback
/// http only when the ORIGIN itself is loopback, i.e. a fixture) and its host must
/// be the origin's host or one of the fixed GitHub asset hosts. Anything else is a
/// typed refusal, never a silent "offline".
pub fn redirect_policy(origin: &reqwest::Url) -> reqwest::redirect::Policy {
    let origin_host = origin.host_str().unwrap_or_default().to_string();
    let origin_is_loopback = is_loopback(origin);
    reqwest::redirect::Policy::custom(move |attempt| {
        let url = attempt.url().clone();
        let host = url.host_str().unwrap_or_default();
        let scheme_ok = url.scheme() == "https"
            || (origin_is_loopback && url.scheme() == "http" && is_loopback(&url));
        let host_ok = host == origin_host || GITHUB_ASSET_HOSTS.contains(&host);
        let hops = attempt.previous().len();
        if hops >= 5 {
            attempt.error("too many redirects")
        } else if scheme_ok && host_ok {
            attempt.follow()
        } else {
            attempt.error(format!("redirect to disallowed url {url}"))
        }
    })
}
