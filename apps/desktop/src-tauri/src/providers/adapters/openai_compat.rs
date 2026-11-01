//! Generic OpenAI-compatible Script-LLM adapter.
//! GPT-6 Luna effort reviewed 2026-10-27:
//! https://developers.openai.com/api/docs/models/gpt-6-luna
//! https://developers.openai.com/api/docs/guides/latest-model
//! https://api-docs.deepseek.com/api/create-chat-completion/
//! https://console.groq.com/docs/reasoning
//! https://docs.together.ai/docs/inference/chat/reasoning
//! Conformance: parameter_profiles_use_documented_host_and_model_gates.
//! Conformance: reasoning_effort_is_gated_to_the_documented_host_and_model_family.
//!
//! Provenance (owner law: current docs, never memory):
//! - Chat Completions reference: https://developers.openai.com/api/docs/api-reference/chat/create
//!   (checked 2026-08-28): POST `{base}/chat/completions`, `Authorization:
//!   Bearer`, required `model` + `messages`; `max_completion_tokens` is
//!   current on OpenAI, `max_tokens` deprecated there.
//! - Ollama compatibility: https://docs.ollama.com/api/openai-compatibility
//!   (checked 2026-08-28): `/v1/chat/completions`, key required-but-ignored,
//!   supports `max_tokens` only.
//! - OpenRouter: https://openrouter.ai/docs/api-reference/chat-completion
//!   (checked 2026-08-28): OpenAI-conformant, `max_completion_tokens`
//!   current with `max_tokens` deprecated.
//!
//! Quirk inference (charter A25 — canonical host, never a user knob): hosts
//! documented as deprecating `max_tokens` (api.openai.com, openrouter.ai)
//! get `max_completion_tokens`; the baseline everywhere else is
//! `max_tokens`, the parameter local endpoints (Ollama, llama.cpp,
//! LM Studio) actually implement. Non-streaming, no tools, no
//! response_format — the batch contract stays on the widest common
//! denominator (charter S-1 gate: no optional wire features).

use async_trait::async_trait;

use crate::providers::registry::{
    ScriptError, ScriptLlm, ScriptRequest, ScriptResponse, ScriptUsage,
};
use crate::voice::keys::ApiKey;

use super::{classify_status, http_client, malformed, retry_after_of, transport_error};

fn max_tokens_param(base_url: &str) -> &'static str {
    match canonical_host(base_url).as_str() {
        "api.openai.com" | "openrouter.ai" => "max_completion_tokens",
        _ => "max_tokens",
    }
}

fn canonical_host(base_url: &str) -> String {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default()
}

/// Where `reasoning_effort` is documented AND lawful: the api.openai.com
/// chat-completions endpoint (https://developers.openai.com/api/docs/api-reference/chat/create,
/// checked 2026-08-29: values none..max, model-dependent) on the GPT-5
/// reasoning family (GPT-5.6 defaults to medium when omitted — the owner
/// ruling of 2026-08-29 makes LOW the default instead). Non-reasoning
/// models (gpt-4o and kin) 400 on the parameter, and every other compat
/// host gets no knob — an unverified parameter risks a 400 on strict
/// servers.
fn reasoning_effort_documented(base_url: &str, model: &str) -> bool {
    canonical_host(base_url) == "api.openai.com"
        && (model.starts_with("gpt-5") || model.starts_with("gpt-6-luna"))
}

fn apply_parameter_profile(
    base: &str,
    model: &str,
    effort: crate::providers::registry::ReasoningEffort,
    body: &mut serde_json::Value,
) {
    let host = canonical_host(base);
    if reasoning_effort_documented(base, model) {
        if let Some(object) = body.as_object_mut() {
            object.remove("temperature");
        }
        body["reasoning_effort"] = serde_json::json!(effort.wire());
    } else if host == "api.deepseek.com" && matches!(model, "deepseek-flash" | "deepseek-v4-pro") {
        body["reasoning_effort"] = serde_json::json!(effort.wire());
    } else if host == "api.groq.com"
        && matches!(model, "openai/gpt-oss-20b" | "openai/gpt-oss-120b")
    {
        body["reasoning_effort"] = serde_json::json!(effort.wire());
        body["include_reasoning"] = serde_json::json!(false);
    } else if host == "api.together.ai" && model == "Qwen/Qwen3.5-9B" {
        body["reasoning"] = serde_json::json!({"enabled": effort != crate::providers::registry::ReasoningEffort::Low});
    }
}

/// Keys resolve into the adapter and nowhere else (charter §3.3 law 3).
/// `None` is legal only for the loopback credential-not-required case the
/// connection store enforces (A1).
pub struct OpenAiCompatScriptLlm {
    client: reqwest::Client,
    /// MF1 success-body bound; `MAX_SCRIPT_RESPONSE_BYTES` in production,
    /// overridable only in tests so the oversized fixture stays small.
    response_cap: usize,
    base_url: String,
    key: Option<ApiKey>,
    model: String,
}

impl OpenAiCompatScriptLlm {
    pub fn new(
        base_url: String,
        key: Option<ApiKey>,
        model: String,
    ) -> Result<OpenAiCompatScriptLlm, ScriptError> {
        Ok(OpenAiCompatScriptLlm {
            client: http_client()?,
            response_cap: super::MAX_SCRIPT_RESPONSE_BYTES,
            base_url,
            key,
            model,
        })
    }
}

impl OpenAiCompatScriptLlm {
    #[cfg(test)]
    fn with_response_cap(mut self, cap: usize) -> Self {
        self.response_cap = cap;
        self
    }
}

#[async_trait]
impl ScriptLlm for OpenAiCompatScriptLlm {
    async fn generate(&mut self, request: ScriptRequest) -> Result<ScriptResponse, ScriptError> {
        let mut messages = Vec::new();
        if !request.system.is_empty() {
            messages.push(serde_json::json!({"role": "system", "content": request.system}));
        }
        messages.push(serde_json::json!({"role": "user", "content": request.user}));
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "temperature": request.temperature,
        });
        body[max_tokens_param(&self.base_url)] = serde_json::json!(request.max_output_tokens);
        apply_parameter_profile(&self.base_url, &self.model, request.effort, &mut body);

        let mut http = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        if let Some(key) = &self.key {
            http = http.header("authorization", format!("Bearer {}", key.expose()));
        }
        let response = http.send().await.map_err(|err| transport_error(&err))?;
        let status = response.status().as_u16();
        let retry_after = retry_after_of(&response);
        if let Some(mut error) = classify_status(status, retry_after) {
            let body = super::bounded_error_quote(response).await;
            if !body.is_empty() {
                error.message = format!("{} — {body}", error.message);
            }
            return Err(error);
        }
        let bytes = super::read_bounded(response, self.response_cap).await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|err| malformed(&err.to_string()))?;
        let text = payload["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| malformed("no choices[0].message.content string"))?
            .to_string();
        Ok(ScriptResponse {
            text,
            usage: ScriptUsage {
                input_tokens: payload["usage"]["prompt_tokens"].as_u64(),
                output_tokens: payload["usage"]["completion_tokens"].as_u64(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::registry::ScriptErrorKind;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Captured {
        head_and_body: String,
    }

    async fn one_shot(status: &str, body: &str) -> (String, tokio::task::JoinHandle<Captured>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://127.0.0.1:{}/v1",
            listener.local_addr().unwrap().port()
        );
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
            sock.write_all(body.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
            Captured {
                head_and_body: request,
            }
        });
        (base, server)
    }

    fn request() -> ScriptRequest {
        ScriptRequest {
            system: String::new(),
            user: "PROMPT".to_string(),
            max_output_tokens: 512,
            temperature: 0.0,
            effort: Default::default(),
        }
    }

    /// Owner ruling pin (2026-08-29): reasoning_effort "low" rides only to
    /// the host AND model family that document the parameter; generic
    /// hosts and non-reasoning models (gpt-4o) get no effort parameter —
    /// OpenAI 400s the knob on models that do not support it.
    #[test]
    fn reasoning_effort_is_gated_to_the_documented_host_and_model_family() {
        assert!(reasoning_effort_documented(
            "https://api.openai.com/v1",
            "gpt-5.6-luna"
        ));
        assert!(!reasoning_effort_documented(
            "https://api.openai.com/v1",
            "gpt-4o"
        ));
        assert!(!reasoning_effort_documented(
            "http://localhost:11434/v1",
            "gpt-5.6-luna"
        ));
        assert!(!reasoning_effort_documented(
            "https://openrouter.ai/api/v1",
            "gpt-5.6-luna"
        ));
        assert!(!reasoning_effort_documented(
            "https://api.groq.com/openai/v1",
            "gpt-5.6-luna"
        ));
    }

    fn fixture_200() -> &'static str {
        include_str!("../../../fixtures/providers/openai_compat_chat_200.json")
    }

    #[tokio::test]
    async fn a_success_round_trip_parses_text_and_usage_keyless_on_loopback() {
        let (base, server) = one_shot("200 OK", fixture_200()).await;
        let mut llm = OpenAiCompatScriptLlm::new(base, None, "llama3".to_string()).unwrap();
        let response = llm.generate(request()).await.unwrap();
        assert_eq!(response.text, "[{\"text\": \"Lecture.\"}]");
        assert_eq!(response.usage.input_tokens, Some(12));
        assert_eq!(response.usage.output_tokens, Some(7));
        let captured = server.await.unwrap();
        assert!(captured.head_and_body.contains("POST /v1/chat/completions"));
        assert!(
            !captured
                .head_and_body
                .to_lowercase()
                .contains("authorization"),
            "loopback keyless sends no credential header"
        );
        assert!(
            captured.head_and_body.contains("\"max_tokens\":512"),
            "generic hosts get the baseline max_tokens parameter"
        );
        assert!(!captured.head_and_body.contains("stream"));
        assert!(!captured.head_and_body.contains("tools"));
        assert!(
            !captured.head_and_body.contains("reasoning_effort"),
            "undocumented hosts get no effort knob"
        );
    }

    #[tokio::test]
    async fn auth_quota_model_and_server_errors_classify_distinctly() {
        for (status, kind) in [
            ("401 Unauthorized", ScriptErrorKind::Auth),
            ("403 Forbidden", ScriptErrorKind::Auth),
            ("429 Too Many Requests", ScriptErrorKind::Quota),
            ("404 Not Found", ScriptErrorKind::ModelNotFound),
            ("500 Internal Server Error", ScriptErrorKind::Provider),
        ] {
            let (base, _server) = one_shot(status, "{\"error\":{\"message\":\"x\"}}").await;
            let mut llm = OpenAiCompatScriptLlm::new(base, None, "m".to_string()).unwrap();
            let error = llm.generate(request()).await.unwrap_err();
            assert_eq!(error.kind, kind, "status {status}");
            if kind == ScriptErrorKind::Quota {
                assert_eq!(error.retry_after_secs, Some(30));
            }
        }
    }

    #[tokio::test]
    async fn an_oversized_success_body_aborts_at_the_wire_bound() {
        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(4096));
        let (base, _server) = one_shot("200 OK", &big).await;
        let mut llm = OpenAiCompatScriptLlm::new(base, None, "m".to_string())
            .unwrap()
            .with_response_cap(512);
        let error = llm.generate(request()).await.unwrap_err();
        assert!(error.message.contains("byte bound"), "{}", error.message);
    }

    #[tokio::test]
    async fn malformed_bodies_and_dead_endpoints_classify_honestly() {
        let (base, _server) = one_shot("200 OK", "{\"choices\": []}").await;
        let mut llm = OpenAiCompatScriptLlm::new(base, None, "m".to_string()).unwrap();
        assert_eq!(
            llm.generate(request()).await.unwrap_err().kind,
            ScriptErrorKind::Malformed
        );

        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}/v1", dead.local_addr().unwrap().port());
        drop(dead);
        let mut llm = OpenAiCompatScriptLlm::new(base, None, "m".to_string()).unwrap();
        assert_eq!(
            llm.generate(request()).await.unwrap_err().kind,
            ScriptErrorKind::Transport
        );
    }

    #[test]
    fn the_max_tokens_quirk_keys_off_the_canonical_host_only() {
        assert_eq!(
            max_tokens_param("https://api.openai.com/v1"),
            "max_completion_tokens"
        );
        assert_eq!(
            max_tokens_param("https://openrouter.ai/api/v1"),
            "max_completion_tokens"
        );
        assert_eq!(max_tokens_param("http://localhost:11434/v1"), "max_tokens");
        assert_eq!(
            max_tokens_param("https://api.groq.com/openai/v1"),
            "max_tokens"
        );
    }
}

#[cfg(test)]
mod current_profile_tests {
    use super::*;

    #[test]
    fn parameter_profiles_use_documented_host_and_model_gates() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/providers/parameter_profiles.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let mut body = serde_json::json!({"temperature":0});
            apply_parameter_profile(
                case["base"].as_str().unwrap(),
                case["model"].as_str().unwrap(),
                Default::default(),
                &mut body,
            );
            assert_eq!(body, case["expected"], "{}", case["base"]);
        }
    }
}
