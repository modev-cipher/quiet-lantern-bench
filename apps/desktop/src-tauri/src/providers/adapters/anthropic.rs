//! Native Anthropic Messages Script-LLM adapter (charter S-2; §2.2: the
//! OpenAI compatibility layer is not the production path).
//!
//! Provenance (owner law: current docs, never memory):
//! - Messages reference: https://platform.claude.com/docs/en/api/messages
//!   (checked 2026-08-28): POST `https://api.anthropic.com/v1/messages`;
//!   auth is `x-api-key` plus `anthropic-version: 2023-06-01` and
//!   `content-type: application/json`; `model`, `max_tokens`, and
//!   `messages` are required; `system` is a TOP-LEVEL parameter, never a
//!   message role; the response is `content` blocks (text concatenated
//!   here, as with Gemini parts) with `usage.input_tokens` /
//!   `usage.output_tokens`.
//! - Errors: https://platform.claude.com/docs/en/api/errors (checked
//!   2026-08-28): JSON envelope `{"type":"error","error":{type,message},
//!   "request_id"}`; 400 invalid_request_error, 401 authentication_error,
//!   402 billing_error, 403 permission_error, 404 not_found_error,
//!   413 request_too_large, 429 rate_limit_error (retry-after present
//!   except tier spend-cap), 500 api_error, 504 timeout_error,
//!   529 overloaded_error (transient, retryable).
//!
//! Wire quirks kept adapter-local (charter §3.3 law 4):
//! - `temperature` is DEPRECATED on the wire — models after Claude Opus 4.6
//!   reject any value other than 1.0 with a 400 (messages doc above). The
//!   script contract's fixed 0.0/0.3 values therefore cannot be sent; the
//!   parameter is omitted entirely and the contract's determinism intent is
//!   carried by the prompts alone. The request-shape test pins the absence.
//! - 529 (`overloaded_error`) sits outside the shared classifier's
//!   500..=599 retryable band and 504 is a documented `timeout_error`;
//!   both are classified here before deferring to the shared table.
//! - Non-streaming POST, matching the batch trait (charter §3.3: no
//!   streaming/tools/turns in batch phases). The streaming Messages API
//!   (https://platform.claude.com/docs/en/build-with-claude/streaming,
//!   checked 2026-08-28) exists and the errors doc recommends it for
//!   long requests; the script contract's chunk output budgets stay far
//!   under the 10-minute guidance, so the plain POST is lawful.

use async_trait::async_trait;

use crate::providers::registry::{
    ScriptError, ScriptErrorKind, ScriptLlm, ScriptRequest, ScriptResponse, ScriptUsage,
};
use crate::voice::keys::ApiKey;

use super::{classify_status, http_client, malformed, retry_after_of, transport_error};

const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
/// The versioning header the API requires on every request
/// (https://platform.claude.com/docs/en/api/errors sample requests, checked
/// 2026-08-28 — same value the VA3 probe has pinned since S-0).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Keys resolve into the adapter and nowhere else (charter §3.3 law 3);
/// Anthropic is `CredentialPolicy::KeyRequired`, so unlike the
/// OpenAI-compatible adapter there is no keyless arm at all.
pub struct AnthropicScriptLlm {
    client: reqwest::Client,
    /// MF1 success-body bound; `MAX_SCRIPT_RESPONSE_BYTES` in production,
    /// overridable only in tests so the oversized fixture stays small.
    response_cap: usize,
    base_url: String,
    key: ApiKey,
    model: String,
}

impl AnthropicScriptLlm {
    pub fn new(key: ApiKey, model: String) -> Result<AnthropicScriptLlm, ScriptError> {
        Self::with_base_url(ANTHROPIC_BASE_URL.to_string(), key, model)
    }

    /// Loopback-fixture constructor, mirroring the other adapters; the
    /// production path is the fixed official host (charter §6.3).
    pub fn with_base_url(
        base_url: String,
        key: ApiKey,
        model: String,
    ) -> Result<AnthropicScriptLlm, ScriptError> {
        Ok(AnthropicScriptLlm {
            client: http_client()?,
            response_cap: super::MAX_SCRIPT_RESPONSE_BYTES,
            base_url,
            key,
            model,
        })
    }
}

/// Adapter-local status classes the shared table cannot express (module
/// head: 529 is retryable overload above the 5xx band; 504 is a documented
/// timeout).
fn classify_anthropic_status(status: u16, retry_after_secs: Option<u64>) -> Option<ScriptError> {
    match status {
        529 => Some(ScriptError {
            kind: ScriptErrorKind::Provider,
            retryable: true,
            message: "provider returned HTTP 529 (overloaded_error)".to_string(),
            retry_after_secs,
            status: Some(529),
        }),
        504 => Some(ScriptError {
            kind: ScriptErrorKind::Timeout,
            retryable: true,
            message: "provider returned HTTP 504 (timeout_error)".to_string(),
            retry_after_secs,
            status: Some(504),
        }),
        _ => classify_status(status, retry_after_secs),
    }
}

impl AnthropicScriptLlm {
    #[cfg(test)]
    fn with_response_cap(mut self, cap: usize) -> Self {
        self.response_cap = cap;
        self
    }
}

/// The effort-supporting model families, verbatim from the compatibility
/// list at https://platform.claude.com/docs/en/build-with-claude/effort
/// (checked 2026-08-29). Prefix-matched so dated aliases of a listed
/// family stay covered; families absent from the doc get no parameter.
fn effort_supported(model: &str) -> bool {
    [
        "claude-fable-5",
        "claude-mythos-5",
        "claude-mythos-preview",
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-opus-4-5",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
    ]
    .iter()
    .any(|family| model.starts_with(family))
}

#[async_trait]
impl ScriptLlm for AnthropicScriptLlm {
    async fn generate(&mut self, request: ScriptRequest) -> Result<ScriptResponse, ScriptError> {
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": request.max_output_tokens,
            "messages": [{"role": "user", "content": request.user}],
        });
        if !request.system.is_empty() {
            body["system"] = serde_json::json!(request.system);
        }
        // Effort (owner ruling 2026-08-29, default LOW; verified at
        // https://platform.claude.com/docs/en/build-with-claude/effort,
        // checked 2026-08-29): output_config.effort, values low..max,
        // API default high. Sent only to the models the doc lists as
        // supporting it — unsupported models (e.g. haiku-4-5) get no
        // parameter rather than a 400. Thinking itself stays off: this
        // adapter never sends a `thinking` object, which is the lowest
        // billable configuration on every model.
        if effort_supported(&self.model) {
            body["output_config"] = serde_json::json!({ "effort": request.effort.wire() });
        }
        // request.temperature is deliberately not sent — see the module head.
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", self.key.expose())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|err| transport_error(&err))?;
        let status = response.status().as_u16();
        let retry_after = retry_after_of(&response);
        if let Some(mut error) = classify_anthropic_status(status, retry_after) {
            let body = super::bounded_error_quote(response).await;
            if !body.is_empty() {
                error.message = format!("{} — {body}", error.message);
            }
            return Err(error);
        }
        let bytes = super::read_bounded(response, self.response_cap).await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|err| malformed(&err.to_string()))?;
        let blocks = payload["content"]
            .as_array()
            .ok_or_else(|| malformed("no content block array"))?;
        let text: String = blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        if text.is_empty() {
            let stop = payload["stop_reason"].as_str().unwrap_or("no stop_reason");
            return Err(malformed(&format!("no text blocks ({stop})")));
        }
        Ok(ScriptResponse {
            text,
            usage: ScriptUsage {
                input_tokens: payload["usage"]["input_tokens"].as_u64(),
                output_tokens: payload["usage"]["output_tokens"].as_u64(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::registry::ScriptErrorKind;
    use tokio::io::AsyncReadExt;

    async fn one_shot(status: &str, body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let status = status.to_string();
        let body = body.to_string();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let mut request = String::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                request.push_str(&String::from_utf8_lossy(&buf[..n]));
                if let Some(head_end) = request.find("\r\n\r\n") {
                    let content_length = request
                        .lines()
                        .find(|line| line.to_lowercase().starts_with("content-length:"))
                        .and_then(|line| line.split(':').nth(1))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= head_end + 4 + content_length {
                        break;
                    }
                }
            }
            crate::test_support::mock_http::write_head(
                &mut sock,
                &status,
                &[("Content-Type", "application/json"), ("Retry-After", "30")],
                Some(body.len()),
            )
            .await
            .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut sock, body.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut sock).await.unwrap();
            request
        });
        (base, server)
    }

    fn key() -> ApiKey {
        ApiKey::new("fixture-key-0000".to_string())
    }

    fn request() -> ScriptRequest {
        ScriptRequest {
            effort: Default::default(),
            system: "SYSTEM".to_string(),
            user: "PROMPT".to_string(),
            max_output_tokens: 256,
            temperature: 0.0,
        }
    }

    #[tokio::test]
    async fn a_success_round_trip_uses_native_messages_with_header_auth() {
        let fixture = include_str!("../../../fixtures/providers/anthropic_messages_200.json");
        let (base, server) = one_shot("200 OK", fixture).await;
        let mut llm =
            AnthropicScriptLlm::with_base_url(base, key(), "claude-sonnet-5".to_string()).unwrap();
        let response = llm.generate(request()).await.unwrap();
        assert_eq!(response.text, "[{\"text\": \"Native lecture.\"}]");
        assert_eq!(response.usage.input_tokens, Some(21));
        assert_eq!(response.usage.output_tokens, Some(9));
        let captured = server.await.unwrap();
        assert!(captured.contains("POST /v1/messages"));
        assert!(captured.contains("x-api-key: fixture-key-0000"));
        assert!(captured.contains("anthropic-version: 2023-06-01"));
        assert!(
            !captured.to_lowercase().contains("authorization:"),
            "auth is x-api-key, never a bearer header"
        );
        let body_start = captured.find("\r\n\r\n").unwrap() + 4;
        let body: serde_json::Value = serde_json::from_str(&captured[body_start..]).unwrap();
        assert_eq!(body["model"], "claude-sonnet-5");
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["system"], "SYSTEM", "system is top-level, not a role");
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(
            body.get("temperature").is_none(),
            "temperature is deprecated wire-side (non-1.0 is a 400 on current \
             models) and must not be sent"
        );
    }

    /// Owner ruling pin (2026-08-29): the default-assembled request
    /// carries output_config.effort "low" on the doc-listed models, and
    /// NO effort parameter (and never a thinking object) elsewhere.
    #[tokio::test]
    async fn default_requests_carry_low_effort_only_where_documented() {
        let fixture = include_str!("../../../fixtures/providers/anthropic_messages_200.json");
        for (model, expects_effort) in [("claude-sonnet-5", true), ("claude-haiku-4-5", false)] {
            let (base, server) = one_shot("200 OK", fixture).await;
            let mut llm =
                AnthropicScriptLlm::with_base_url(base, key(), model.to_string()).unwrap();
            llm.generate(request()).await.unwrap();
            let captured = server.await.unwrap();
            let body_start = captured.find("\r\n\r\n").unwrap() + 4;
            let body: serde_json::Value = serde_json::from_str(&captured[body_start..]).unwrap();
            if expects_effort {
                assert_eq!(body["output_config"]["effort"], "low", "{model}");
            } else {
                assert!(body.get("output_config").is_none(), "{model}");
            }
            assert!(
                body.get("thinking").is_none(),
                "thinking stays off — the lowest billable configuration"
            );
        }
    }

    #[tokio::test]
    async fn error_statuses_classify_to_the_charter_taxonomy() {
        let envelope =
            r#"{"type":"error","error":{"type":"x","message":"y"},"request_id":"req_1"}"#;
        for (status, kind, retryable) in [
            ("400 Bad Request", ScriptErrorKind::Provider, false),
            ("401 Unauthorized", ScriptErrorKind::Auth, false),
            ("402 Payment Required", ScriptErrorKind::Quota, true),
            ("403 Forbidden", ScriptErrorKind::Auth, false),
            ("404 Not Found", ScriptErrorKind::ModelNotFound, false),
            ("429 Too Many Requests", ScriptErrorKind::Quota, true),
            ("500 Internal Server Error", ScriptErrorKind::Provider, true),
            ("504 Gateway Timeout", ScriptErrorKind::Timeout, true),
            ("529 Overloaded", ScriptErrorKind::Provider, true),
        ] {
            let (base, _server) = one_shot(status, envelope).await;
            let mut llm = AnthropicScriptLlm::with_base_url(base, key(), "m".to_string()).unwrap();
            let error = llm.generate(request()).await.unwrap_err();
            assert_eq!(error.kind, kind, "status {status}");
            assert_eq!(error.retryable, retryable, "status {status}");
        }
    }

    #[tokio::test]
    async fn quota_carries_retry_after_and_the_error_body_is_quoted() {
        let envelope =
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let (base, _server) = one_shot("429 Too Many Requests", envelope).await;
        let mut llm = AnthropicScriptLlm::with_base_url(base, key(), "m".to_string()).unwrap();
        let error = llm.generate(request()).await.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Quota);
        assert_eq!(error.retry_after_secs, Some(30));
        assert!(error.message.contains("rate_limit_error"));
    }

    #[tokio::test]
    async fn an_oversized_success_body_aborts_at_the_wire_bound() {
        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(4096));
        let (base, _server) = one_shot("200 OK", &big).await;
        let mut llm = AnthropicScriptLlm::with_base_url(base, key(), "m".to_string())
            .unwrap()
            .with_response_cap(512);
        let error = llm.generate(request()).await.unwrap_err();
        assert!(error.message.contains("byte bound"), "{}", error.message);
    }

    #[tokio::test]
    async fn malformed_bodies_and_dead_endpoints_classify_honestly() {
        for body in [
            r#"{"content": []}"#,
            r#"{"content": [{"type":"thinking","thinking":"..."}], "stop_reason":"refusal"}"#,
            "not json",
        ] {
            let (base, _server) = one_shot("200 OK", body).await;
            let mut llm = AnthropicScriptLlm::with_base_url(base, key(), "m".to_string()).unwrap();
            assert_eq!(
                llm.generate(request()).await.unwrap_err().kind,
                ScriptErrorKind::Malformed,
                "body {body}"
            );
        }

        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", dead.local_addr().unwrap().port());
        drop(dead);
        let mut llm = AnthropicScriptLlm::with_base_url(base, key(), "m".to_string()).unwrap();
        assert_eq!(
            llm.generate(request()).await.unwrap_err().kind,
            ScriptErrorKind::Transport
        );
    }

    #[tokio::test]
    async fn an_empty_system_prompt_is_omitted_not_sent_blank() {
        let fixture = include_str!("../../../fixtures/providers/anthropic_messages_200.json");
        let (base, server) = one_shot("200 OK", fixture).await;
        let mut llm = AnthropicScriptLlm::with_base_url(base, key(), "m".to_string()).unwrap();
        let mut req = request();
        req.system = String::new();
        llm.generate(req).await.unwrap();
        let captured = server.await.unwrap();
        let body_start = captured.find("\r\n\r\n").unwrap() + 4;
        let body: serde_json::Value = serde_json::from_str(&captured[body_start..]).unwrap();
        assert!(body.get("system").is_none());
    }
}
