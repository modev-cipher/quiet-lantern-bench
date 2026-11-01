//! Script-LLM provider adapters (charter §3.3 law 3-4: keys resolve only
//! here, wire quirks live only here). Each module header records its
//! official-doc provenance per §3.6 and the owner's fetch-current-docs law.
//!
//! Shared status table checked 2026-11-01 against
//! https://ai.google.dev/gemini-api/docs/troubleshooting: 408, 429 and 5xx are
//! transient; 400 and 403 never recover by retrying. 402 (depleted credits) stays a
//! key-level quota so a pool can move to its next key.

pub mod anthropic;
pub mod cartesia_tts;
pub mod gemini;
pub mod gemini_tts;
pub mod openai_compat;
pub mod openai_realtime_tts;
pub mod openai_tts;
pub(crate) mod sse;

use crate::providers::registry::{ScriptError, ScriptErrorKind};

/// Shared HTTP failure classification (charter §5.5: classes stay distinct,
/// transport proves nothing about the key).
pub(crate) fn classify_status(status: u16, retry_after_secs: Option<u64>) -> Option<ScriptError> {
    let (kind, retryable) = match status {
        200..=299 => return None,
        401 | 403 => (ScriptErrorKind::Auth, false),
        402 | 429 => (ScriptErrorKind::Quota, true),
        404 => (ScriptErrorKind::ModelNotFound, false),
        408 => (ScriptErrorKind::Timeout, true),
        500..=599 => (ScriptErrorKind::Provider, true),
        _ => (ScriptErrorKind::Provider, false),
    };
    Some(ScriptError {
        kind,
        retryable,
        message: format!("provider returned HTTP {status}"),
        retry_after_secs,
        status: Some(status),
    })
}

pub(crate) fn transport_error(err: &reqwest::Error) -> ScriptError {
    let kind = if err.is_timeout() {
        ScriptErrorKind::Timeout
    } else {
        ScriptErrorKind::Transport
    };
    ScriptError {
        kind,
        retryable: true,
        message: format!("provider request failed: {err}"),
        retry_after_secs: None,
        status: None,
    }
}

/// A23 wire-layer bound: read the response body chunk by chunk, aborting
/// the moment the accumulated size would pass `limit` — the cap holds
/// DURING the read; bytes past the bound are never accumulated first and
/// measured after.
pub(crate) async fn read_bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ScriptError> {
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| transport_error(&err))?
    {
        if bytes.len() + chunk.len() > limit {
            return Err(ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: false,
                message: format!(
                    "provider response exceeded the {limit}-byte bound mid-stream; aborted \
                     at the wire rather than buffered"
                ),
                retry_after_secs: None,
                status: None,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Bound on ANY provider success body a Script adapter deserializes: a
/// lecture response is a few thousand tokens of JSON; 8 MiB is orders of
/// magnitude of slack, and past it the read aborts mid-stream (A23 applies
/// to Script bodies exactly as to audio — Hermes MF1).
pub(crate) const MAX_SCRIPT_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Error bodies are quoted into messages at 512 chars; reading more than
/// 4 KiB off the wire for that quote is waste, and reading unboundedly is
/// the MF1 defect. Truncation here is silent (an error body is diagnostic
/// text, not a contract payload).
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 4096;

/// Read at most `limit` bytes of the body, TRUNCATING there (drop the
/// rest) — for diagnostic error bodies where over-length is not itself an
/// error. Contrast `read_bounded`, which aborts as a classified failure.
pub(crate) async fn read_truncated(mut response: reqwest::Response, limit: usize) -> Vec<u8> {
    let mut bytes: Vec<u8> = Vec::new();
    while bytes.len() < limit {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let take = chunk.len().min(limit - bytes.len());
                bytes.extend_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    break;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    bytes
}

pub fn sanitize_error_message(msg: &str) -> String {
    static AIZA_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static SK_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static KEY_PARAM_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

    let aiza = AIZA_RE.get_or_init(|| regex::Regex::new(r"AIza[0-9A-Za-z_-]{20,}").unwrap());
    let sk = SK_RE.get_or_init(|| regex::Regex::new(r"sk-[0-9a-zA-Z_-]{20,}").unwrap());
    let key_param = KEY_PARAM_RE.get_or_init(|| regex::Regex::new(r"([?&]key=)[^&\s]+").unwrap());

    let sanitized = aiza.replace_all(msg, "[REDACTED_API_KEY]");
    let sanitized = sk.replace_all(&sanitized, "[REDACTED_API_KEY]");
    let sanitized = key_param.replace_all(&sanitized, "${1}[REDACTED]");
    sanitized.to_string()
}

/// The uniform bounded error-body quote (chars-capped after a byte-capped
/// wire read).
pub(crate) async fn bounded_error_quote(response: reqwest::Response) -> String {
    let bytes = read_truncated(response, MAX_ERROR_BODY_BYTES).await;
    let text: String = String::from_utf8_lossy(&bytes).chars().take(512).collect();
    sanitize_error_message(&text)
}

pub(crate) fn malformed(detail: &str) -> ScriptError {
    ScriptError {
        kind: ScriptErrorKind::Malformed,
        retryable: false,
        message: format!("provider response malformed: {detail}"),
        retry_after_secs: None,
        status: None,
    }
}

/// RFC 9110: Retry-After carries delta-seconds OR an HTTP-date; both map to
/// remaining whole seconds (never negative, None when unusable).
pub(crate) fn retry_after_of(response: &reqwest::Response) -> Option<u64> {
    let raw = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())?
        .trim()
        .to_string();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(secs);
    }
    let at = httpdate::parse_http_date(&raw).ok()?;
    Some(
        at.duration_since(std::time::SystemTime::now())
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

/// Fixed internal transport policy (charter §3.4: application-owned values,
/// never user knobs).
pub(crate) fn http_client() -> Result<reqwest::Client, ScriptError> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| ScriptError {
            kind: ScriptErrorKind::Transport,
            retryable: false,
            message: format!("http client: {err}"),
            retry_after_secs: None,
            status: None,
        })
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn documented_transient_statuses_are_retryable_and_client_errors_are_not() {
        let timeout = classify_status(408, None).expect("408");
        assert_eq!(timeout.kind, ScriptErrorKind::Timeout);
        assert!(timeout.retryable);
        for status in [429, 500, 503] {
            assert!(classify_status(status, None).expect("status").retryable);
        }
        for status in [400, 401, 403, 404] {
            assert!(!classify_status(status, None).expect("status").retryable);
        }
        assert!(classify_status(200, None).is_none());
    }
}
