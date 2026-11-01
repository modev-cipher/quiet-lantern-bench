//! Unified Provider Error Policy (Shared Engine).
//!
//! Provides centralized, strict fault classification and patience budgeting
//! across all cloud pipeline stages (Scripting, TTS Synthesis, Vision).
//!
//! Enforces the following core invariants:
//! 1. Status-first inspection: HTTP status codes (500, 502, 503, 504) are
//!    inspected before JSON deserialization to safely handle non-JSON/HTML error pages.
//! 2. Credential faults (401, 403, 429, and 400 API_KEY_INVALID) belong to the specific
//!    key and trigger immediate zero-sleep rotation to the next candidate in the pool.
//! 3. Cluster faults (500, 502, 503, 504, 529, UNAVAILABLE) belong to upstream infrastructure:
//!    the key remains Active, and mandatory exponential backoff with full jitter is enforced.
//! 4. Terminal faults (400 without auth signatures, 404, 422) fail immediately without retrying.
//! 5. Execution mode differentiation: Interactive (at-desk) mode allows up to 10 retries (60s cap);
//!    Queued Overnight mode allows up to 120 retries (300s / 5-min cap).

use rand::Rng;
use std::time::Duration;

use super::overload::{
    contains_auth_signatures, contains_overload_signatures, contains_quota_signatures,
    parse_retry_delay_secs,
};
use crate::providers::registry::{ScriptError, ScriptErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultClass {
    /// Credential rejected (401, 403, or 400 with API_KEY_INVALID).
    /// Action: Demote key to Invalid permanently; rotate immediately with zero sleep.
    KeyAuthInvalid,

    /// Rate limit or quota exhausted (429 or RESOURCE_EXHAUSTED).
    /// Action: Demote key to OutOfQuota until retry_after; rotate immediately with zero sleep.
    KeyRateLimited { retry_after_secs: Option<u64> },

    /// Upstream cluster overload (500, 502, 503, 504, 529, UNAVAILABLE).
    /// Invariant: Key remains Active (never demoted).
    /// Action: Mandatory exponential backoff with full jitter on the SAME key.
    ClusterOverload { retry_after_secs: Option<u64> },

    /// Unrecoverable client error (400 without auth signatures, 404, 422).
    /// Action: Fail immediately; do not retry or cycle keys.
    Terminal,

    /// Network drop, connection reset, or DNS failure.
    /// Invariant: Key remains Active. Short backoff with jitter on same key.
    TransientTransport,
}

#[derive(Debug, Clone)]
pub struct PatienceBudget {
    pub max_cluster_attempts: usize,
    pub initial_backoff_secs: f64,
    pub max_backoff_cap_secs: f64,
    pub is_overnight: bool,
}

impl PatienceBudget {
    pub fn for_mode(is_overnight: bool) -> Self {
        if is_overnight {
            Self {
                max_cluster_attempts: 120,
                initial_backoff_secs: 2.0,
                max_backoff_cap_secs: 300.0,
                is_overnight: true,
            }
        } else {
            Self {
                max_cluster_attempts: 10,
                initial_backoff_secs: 2.0,
                max_backoff_cap_secs: 60.0,
                is_overnight: false,
            }
        }
    }

    pub fn for_test() -> Self {
        Self {
            max_cluster_attempts: 10,
            initial_backoff_secs: 0.0,
            max_backoff_cap_secs: 0.0,
            is_overnight: false,
        }
    }

    /// Computes full jitter backoff: Uniform(1.0, min(cap, initial * 2^(attempt-1))).
    /// If the provider specified an explicit retry-after delay, respects it as a floor.
    pub fn calculate_delay(&self, attempt: usize, explicit_retry_after: Option<u64>) -> Duration {
        if self.max_backoff_cap_secs <= 0.0 {
            return Duration::ZERO;
        }
        let base =
            self.initial_backoff_secs * 2.0_f64.powi((attempt.saturating_sub(1)).min(10) as i32);
        let capped = base.min(self.max_backoff_cap_secs);
        let jittered = if capped > 1.0 {
            rand::rng().random_range(1.0..=capped)
        } else {
            1.0
        };

        let mut final_secs = jittered;
        if let Some(explicit) = explicit_retry_after {
            final_secs = final_secs
                .max(explicit as f64)
                .min(self.max_backoff_cap_secs);
        }
        Duration::from_secs_f64(final_secs.max(1.0))
    }
}

pub struct ProviderErrorPolicy;

impl ProviderErrorPolicy {
    /// Classifies an error status and body into a strict FaultClass.
    /// Follows a status-first discipline so non-JSON/HTML proxy pages (502/503)
    /// are safely routed to ClusterOverload without failing JSON parsing.
    pub fn classify(status: Option<u16>, body: &str, kind: Option<ScriptErrorKind>) -> FaultClass {
        let explicit_delay = parse_retry_delay_secs(body);

        // 1. Status-First checks:
        if let Some(code) = status {
            match code {
                // Cluster overloads & server errors:
                500 | 502 | 503 | 504 | 529 => {
                    return FaultClass::ClusterOverload {
                        retry_after_secs: explicit_delay,
                    };
                }
                // Key-level rate limits:
                429 => {
                    return FaultClass::KeyRateLimited {
                        retry_after_secs: explicit_delay.or(Some(60)),
                    };
                }
                // Key-level auth failures:
                401 | 403 => {
                    return FaultClass::KeyAuthInvalid;
                }
                // Google Gemini returns HTTP 400 with API_KEY_INVALID for bad keys:
                400 => {
                    if contains_auth_signatures(body) {
                        return FaultClass::KeyAuthInvalid;
                    }
                    return FaultClass::Terminal;
                }
                // Model not found or unprocessable:
                404 | 422 => {
                    return FaultClass::Terminal;
                }
                _ => {}
            }
        }

        // 2. Body-signature fallbacks (for proxies or synthetic messages):
        if contains_auth_signatures(body) {
            return FaultClass::KeyAuthInvalid;
        }
        if contains_quota_signatures(body) {
            return FaultClass::KeyRateLimited {
                retry_after_secs: explicit_delay.or(Some(60)),
            };
        }
        if contains_overload_signatures(body) {
            return FaultClass::ClusterOverload {
                retry_after_secs: explicit_delay,
            };
        }

        // 3. Kind-based check:
        if let Some(k) = kind {
            match k {
                ScriptErrorKind::Auth => return FaultClass::KeyAuthInvalid,
                ScriptErrorKind::Quota => {
                    return FaultClass::KeyRateLimited {
                        retry_after_secs: explicit_delay.or(Some(60)),
                    };
                }
                ScriptErrorKind::Transport | ScriptErrorKind::Timeout => {
                    return FaultClass::TransientTransport;
                }
                ScriptErrorKind::ModelNotFound
                | ScriptErrorKind::Malformed
                | ScriptErrorKind::Cancelled
                | ScriptErrorKind::Provider => return FaultClass::Terminal,
            }
        }

        // 4. Terminal payload signatures:
        let lower = body.to_ascii_lowercase();
        if lower.contains("context length")
            || lower.contains("maximum context")
            || lower.contains("too many tokens")
            || lower.contains("prompt is too long")
        {
            return FaultClass::Terminal;
        }

        // Default: If status is 4xx or unrecognized, treat as Terminal.
        FaultClass::Terminal
    }

    /// Helper to classify a ScriptError directly.
    pub fn classify_script_error(err: &ScriptError) -> FaultClass {
        let mut class = Self::classify(err.status, &err.message, Some(err.kind));
        if let FaultClass::KeyRateLimited {
            ref mut retry_after_secs,
        } = class
        {
            if retry_after_secs.is_none() && err.retry_after_secs.is_some() {
                *retry_after_secs = err.retry_after_secs;
            }
        } else if let FaultClass::ClusterOverload {
            ref mut retry_after_secs,
        } = class
        {
            if retry_after_secs.is_none() && err.retry_after_secs.is_some() {
                *retry_after_secs = err.retry_after_secs;
            }
        }
        class
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_first_502_503_html_response() {
        let html_body = "<html><head><title>503 Service Unavailable</title></head><body>Server busy</body></html>";
        let fault = ProviderErrorPolicy::classify(Some(503), html_body, None);
        assert_eq!(
            fault,
            FaultClass::ClusterOverload {
                retry_after_secs: None
            }
        );

        let bad_gateway = "<html><title>502 Bad Gateway</title></html>";
        let fault = ProviderErrorPolicy::classify(Some(502), bad_gateway, None);
        assert_eq!(
            fault,
            FaultClass::ClusterOverload {
                retry_after_secs: None
            }
        );
    }

    #[test]
    fn test_gemini_400_api_key_invalid_routes_to_auth() {
        let fixture = r#"{
            "error": {
                "code": 400,
                "message": "API key not valid. Please pass a valid API key.",
                "status": "INVALID_ARGUMENT",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                        "reason": "API_KEY_INVALID"
                    }
                ]
            }
        }"#;
        let fault = ProviderErrorPolicy::classify(Some(400), fixture, None);
        assert_eq!(fault, FaultClass::KeyAuthInvalid);
    }

    #[test]
    fn test_gemini_400_bad_parameter_routes_to_terminal() {
        let fixture = r#"{
            "error": {
                "code": 400,
                "message": "Invalid temperature parameter value: 5.0",
                "status": "INVALID_ARGUMENT"
            }
        }"#;
        let fault = ProviderErrorPolicy::classify(Some(400), fixture, None);
        assert_eq!(fault, FaultClass::Terminal);
    }

    #[test]
    fn test_gemini_429_quota_routes_to_key_rate_limited_with_delay() {
        let fixture = r#"{
            "error": {
                "code": 429,
                "message": "Resource has been exhausted (e.g. check quota).",
                "status": "RESOURCE_EXHAUSTED",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.RetryInfo",
                        "retryDelay": "45s"
                    }
                ]
            }
        }"#;
        let fault = ProviderErrorPolicy::classify(Some(429), fixture, None);
        assert_eq!(
            fault,
            FaultClass::KeyRateLimited {
                retry_after_secs: Some(45)
            }
        );
    }

    #[test]
    fn test_patience_budget_modes() {
        let interactive = PatienceBudget::for_mode(false);
        assert_eq!(interactive.max_cluster_attempts, 10);
        assert_eq!(interactive.max_backoff_cap_secs, 60.0);

        let overnight = PatienceBudget::for_mode(true);
        assert_eq!(overnight.max_cluster_attempts, 120);
        assert_eq!(overnight.max_backoff_cap_secs, 300.0);

        // Verify full jitter backoff calculation produces bounded positive durations
        for attempt in 1..=15 {
            let delay = interactive.calculate_delay(attempt, None);
            assert!(delay.as_secs_f64() >= 1.0);
            assert!(delay.as_secs_f64() <= 60.0);
        }
    }
}
