// Google Gemini API Error Semantics & Handling:
// Documentation: https://ai.google.dev/gemini-api/docs/troubleshooting (accessed 2026-09-15)
// Documentation: https://ai.google.dev/gemini-api/docs/api-errors (accessed 2026-09-15)
// Google Cloud API Design: https://cloud.google.com/apis/design/errors (accessed 2026-09-15)
// Google AIP-193: https://google.aip.dev/193 (accessed 2026-09-15)
// Conformance fixture: fixtures/providers/gemini_errors.json

use crate::providers::{ConnectionRecord, ProviderKind};

pub const PAUSED_KIND_SERVICE_BUSY: &str = "service_busy";
pub const PAUSED_KIND_RATE_LIMITED: &str = "rate_limited";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorClass {
    Overload,
    Quota,
    Auth,
    Other,
}

pub fn classify_provider_error(status: Option<u16>, body: &str) -> ProviderErrorClass {
    match status {
        Some(429) => return ProviderErrorClass::Quota,
        Some(401 | 403) => return ProviderErrorClass::Auth,
        Some(500 | 502 | 503 | 504 | 529) => return ProviderErrorClass::Overload,
        Some(400) if contains_auth_signatures(body) => return ProviderErrorClass::Auth,
        _ => {}
    }

    if contains_quota_signatures(body) {
        return ProviderErrorClass::Quota;
    }
    if contains_auth_signatures(body) {
        return ProviderErrorClass::Auth;
    }
    if contains_overload_signatures(body) {
        return ProviderErrorClass::Overload;
    }

    ProviderErrorClass::Other
}

pub(crate) fn contains_overload_signatures(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    body.contains("UNAVAILABLE")
        || body.contains("DEADLINE_EXCEEDED")
        || body.contains("INTERNAL")
        || lower.contains("high demand")
        || lower.contains("overloaded")
        || lower.contains("temporarily unavailable")
        || lower.contains("try again later")
        || lower.contains("service unavailable")
        || lower.contains("internal error")
}

pub(crate) fn contains_quota_signatures(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    body.contains("RESOURCE_EXHAUSTED")
        || lower.contains("rate limit")
        || lower.contains("quota")
        || lower.contains("too many requests")
}

pub(crate) fn contains_auth_signatures(body: &str) -> bool {
    body.contains("API_KEY_INVALID")
        || body.contains("UNAUTHENTICATED")
        || body.contains("PERMISSION_DENIED")
        || body.to_ascii_lowercase().contains("invalid api key")
}

pub fn is_overload(status: Option<u16>, body: &str) -> bool {
    classify_provider_error(status, body) == ProviderErrorClass::Overload
}

pub fn is_script_error_overload(err: &crate::providers::registry::ScriptError) -> bool {
    is_overload(err.status, &err.message)
}

pub fn is_provider_failure_overload(fail: &crate::providers::registry::ProviderFailure) -> bool {
    is_overload(fail.status, &fail.detail)
}

pub const MAX_COOLDOWN_SECS: u64 = 120;

pub fn is_retryable_rate_limit(err: &crate::providers::registry::ScriptError) -> Option<u64> {
    if err.kind == crate::providers::registry::ScriptErrorKind::Quota || err.status == Some(429) {
        let delay = err
            .retry_after_secs
            .or_else(|| parse_retry_delay_secs(&err.message));
        if let Some(d) = delay {
            if d <= MAX_COOLDOWN_SECS {
                return Some(d);
            }
        }
    }
    None
}

pub fn parse_retry_delay_secs(body: &str) -> Option<u64> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(details) = value.pointer("/error/details").and_then(|d| d.as_array()) {
            for detail in details {
                if let Some(delay_str) = detail.get("retryDelay").and_then(|d| d.as_str()) {
                    let s = delay_str.trim_end_matches('s');
                    if let Ok(f) = s.parse::<f64>() {
                        if f > 0.0 {
                            return Some(f.ceil() as u64);
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn friendly_provider_name(provider: &str) -> String {
    let lower = provider.trim().to_ascii_lowercase();
    match lower.as_str() {
        "gemini" | "google" | "google_gemini" | "google gemini" => "Google Gemini".to_string(),
        "openai" | "openai_compatible" | "openai-compatible" => "OpenAI".to_string(),
        "anthropic" => "Anthropic".to_string(),
        "cartesia" | "cartesia_tts" => "Cartesia".to_string(),
        "openai_tts" => "OpenAI".to_string(),
        "elevenlabs" => "ElevenLabs".to_string(),
        other => {
            if other.is_empty() {
                "Provider".to_string()
            } else {
                provider.trim().to_string()
            }
        }
    }
}

pub fn connection_label(connection: &ConnectionRecord) -> String {
    match connection.provider {
        ProviderKind::OpenaiCompatible => match connection.name.trim() {
            "" => "your provider".to_string(),
            name => name.to_string(),
        },
        kind => friendly_provider_name(kind.id()),
    }
}

pub fn friendly_provider_error(provider: &str, status: Option<u16>, body: &str) -> String {
    let name = friendly_provider_name(provider);
    match classify_provider_error(status, body) {
        ProviderErrorClass::Overload => {
            format!("{name} is busy right now — your job is saved and will keep retrying automatically.")
        }
        ProviderErrorClass::Quota => {
            format!("{name} rate limit reached — your job is saved and will keep retrying automatically.")
        }
        ProviderErrorClass::Auth => {
            format!("{name} rejected your API key — please check your credentials.")
        }
        ProviderErrorClass::Other => {
            if body.trim().starts_with('{') || body.contains("\"error\":") {
                if let Some(code) = status {
                    format!("{name} returned an error ({code}) — your job is saved and will retry.")
                } else {
                    format!("{name} returned a temporary error — your job is saved and will retry.")
                }
            } else if !body.is_empty() {
                body.to_string()
            } else if let Some(code) = status {
                format!("{name} encountered an error ({code}) — retrying.")
            } else {
                format!("{name} encountered a temporary error — retrying.")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct PatienceConfig {
    pub backoff_delays_secs: Vec<u64>,
    pub max_window_secs: u64,
    pub pause_wait_secs: u64,
}

impl Default for PatienceConfig {
    fn default() -> Self {
        Self {
            backoff_delays_secs: vec![30, 60, 120, 300],
            max_window_secs: 900,
            pause_wait_secs: 300,
        }
    }
}

impl PatienceConfig {
    pub fn for_test() -> Self {
        Self {
            backoff_delays_secs: vec![0, 0],
            max_window_secs: 60,
            pause_wait_secs: 1,
        }
    }

    pub fn delay_for_attempt(&self, attempt: usize) -> u64 {
        if attempt < self.backoff_delays_secs.len() {
            self.backoff_delays_secs[attempt]
        } else {
            self.backoff_delays_secs.last().copied().unwrap_or(300)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(provider: ProviderKind, name: &str) -> ConnectionRecord {
        ConnectionRecord {
            id: "c-1".to_string(),
            name: name.to_string(),
            provider,
            base_url: None,
            credential_not_required: false,
            default_model: "m".to_string(),
            enabled: true,
            created_unix: 0,
            updated_unix: 0,
        }
    }

    #[test]
    fn a_fixed_provider_is_named_by_its_brand_not_the_connection_name() {
        assert_eq!(
            connection_label(&connection(ProviderKind::Gemini, "work key")),
            "Google Gemini"
        );
        assert_eq!(
            connection_label(&connection(ProviderKind::CartesiaTts, "voices")),
            "Cartesia"
        );
        assert_eq!(
            connection_label(&connection(ProviderKind::OpenaiTts, "tts")),
            "OpenAI"
        );
        assert_eq!(
            connection_label(&connection(ProviderKind::Anthropic, "claude")),
            "Anthropic"
        );
    }

    #[test]
    fn a_compatible_endpoint_is_named_by_its_connection() {
        assert_eq!(
            connection_label(&connection(ProviderKind::OpenaiCompatible, " LM Studio ")),
            "LM Studio"
        );
        assert_eq!(
            connection_label(&connection(ProviderKind::OpenaiCompatible, "  ")),
            "your provider"
        );
    }

    #[test]
    fn test_gemini_fixture_conformance() {
        let fixture_raw = include_str!("../../fixtures/providers/gemini_errors.json");
        let parsed: serde_json::Value =
            serde_json::from_str(fixture_raw).expect("valid fixture json");
        let errors = parsed["errors"].as_object().expect("errors object");

        let high_demand = &errors["overload_503_high_demand"];
        let status = high_demand["http_status"].as_u64().map(|s| s as u16);
        let body = high_demand["body"].as_str().unwrap();
        assert_eq!(
            classify_provider_error(status, body),
            ProviderErrorClass::Overload
        );
        assert!(is_overload(status, body));
        let friendly = friendly_provider_error("gemini", status, body);
        assert_eq!(
            friendly,
            "Google Gemini is busy right now — your job is saved and will keep retrying automatically."
        );

        let retry_info = &errors["overload_503_retry_info"];
        let status = retry_info["http_status"].as_u64().map(|s| s as u16);
        let body = retry_info["body"].as_str().unwrap();
        assert_eq!(
            classify_provider_error(status, body),
            ProviderErrorClass::Overload
        );
        assert_eq!(parse_retry_delay_secs(body), Some(30));

        let internal = &errors["overload_500_internal"];
        let status = internal["http_status"].as_u64().map(|s| s as u16);
        let body = internal["body"].as_str().unwrap();
        assert_eq!(
            classify_provider_error(status, body),
            ProviderErrorClass::Overload
        );

        let deadline = &errors["overload_504_deadline_exceeded"];
        let status = deadline["http_status"].as_u64().map(|s| s as u16);
        let body = deadline["body"].as_str().unwrap();
        assert_eq!(
            classify_provider_error(status, body),
            ProviderErrorClass::Overload
        );

        let quota_exhausted = &errors["quota_429_resource_exhausted"];
        let status = quota_exhausted["http_status"].as_u64().map(|s| s as u16);
        let body = quota_exhausted["body"].as_str().unwrap();
        assert_eq!(
            classify_provider_error(status, body),
            ProviderErrorClass::Quota
        );
        assert_eq!(parse_retry_delay_secs(body), Some(60));
        let friendly = friendly_provider_error("Google Gemini", status, body);
        assert_eq!(
            friendly,
            "Google Gemini rate limit reached — your job is saved and will keep retrying automatically."
        );

        let quota_rpm = &errors["quota_429_rate_limit_exceeded"];
        let status = quota_rpm["http_status"].as_u64().map(|s| s as u16);
        let body = quota_rpm["body"].as_str().unwrap();
        assert_eq!(
            classify_provider_error(status, body),
            ProviderErrorClass::Quota
        );

        let api_key_invalid = &errors["auth_400_api_key_invalid"];
        let status = api_key_invalid["http_status"].as_u64().map(|s| s as u16);
        let body = api_key_invalid["body"].as_str().unwrap();
        assert_eq!(
            classify_provider_error(status, body),
            ProviderErrorClass::Auth
        );
        let friendly = friendly_provider_error("Google Gemini", status, body);
        assert_eq!(
            friendly,
            "Google Gemini rejected your API key — please check your credentials."
        );

        let unauth = &errors["auth_401_unauthenticated"];
        let status = unauth["http_status"].as_u64().map(|s| s as u16);
        let body = unauth["body"].as_str().unwrap();
        assert_eq!(
            classify_provider_error(status, body),
            ProviderErrorClass::Auth
        );

        let perm_denied = &errors["auth_403_permission_denied"];
        let status = perm_denied["http_status"].as_u64().map(|s| s as u16);
        let body = perm_denied["body"].as_str().unwrap();
        assert_eq!(
            classify_provider_error(status, body),
            ProviderErrorClass::Auth
        );
    }

    #[test]
    fn test_patience_config_progression() {
        let config = PatienceConfig::default();
        assert_eq!(config.delay_for_attempt(0), 30);
        assert_eq!(config.delay_for_attempt(1), 60);
        assert_eq!(config.delay_for_attempt(2), 120);
        assert_eq!(config.delay_for_attempt(3), 300);
        assert_eq!(config.delay_for_attempt(4), 300);
        assert_eq!(config.delay_for_attempt(10), 300);
    }

    #[test]
    fn test_is_retryable_rate_limit() {
        use crate::providers::registry::{ScriptError, ScriptErrorKind};

        let err_with_delay = ScriptError {
            kind: ScriptErrorKind::Quota,
            retryable: true,
            message: "rate limit".to_string(),
            retry_after_secs: Some(90),
            status: Some(429),
        };
        assert_eq!(is_retryable_rate_limit(&err_with_delay), Some(90));

        let err_too_long = ScriptError {
            kind: ScriptErrorKind::Quota,
            retryable: true,
            message: "rate limit".to_string(),
            retry_after_secs: Some(180),
            status: Some(429),
        };
        assert_eq!(is_retryable_rate_limit(&err_too_long), None);

        let err_body_delay = ScriptError {
            kind: ScriptErrorKind::Quota,
            retryable: true,
            message: "{\"error\":{\"details\":[{\"retryDelay\":\"45s\"}]}}".to_string(),
            retry_after_secs: None,
            status: Some(429),
        };
        assert_eq!(is_retryable_rate_limit(&err_body_delay), Some(45));
    }
}
