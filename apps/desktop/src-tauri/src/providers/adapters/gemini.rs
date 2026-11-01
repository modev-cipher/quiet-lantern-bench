//! Gemini preset Script-LLM adapter — NATIVE `generateContent`.
//!
//! A17 protocol decision, from the docs fetched for this slice (owner law:
//! current docs, never memory):
//! - Native reference: https://ai.google.dev/api/generate-content (checked
//!   2026-08-28) — POST `/v1beta/models/{model}:generateContent`,
//!   `systemInstruction`, `contents[].parts[].text`,
//!   `generationConfig.maxOutputTokens`, response
//!   `candidates[].content.parts[].text` + `usageMetadata`.
//! - Compat layer: https://ai.google.dev/gemini-api/docs/openai (checked
//!   2026-08-28) — self-described as "still in beta while we extend feature
//!   support" and recommending developers not already on OpenAI libraries
//!   "call the Gemini API directly".
//!
//! Verdict: the preset speaks native generateContent. The compat layer's own
//! doc disqualifies it as the production path, and native auth rides the
//! `x-goog-api-key` HEADER (the VA3 probe precedent) instead of the query
//! parameter the REST samples show — a key must never appear in a URL
//! (charter §4.2). Choice is adapter-local; nothing outside this module
//! knows which protocol runs (charter A17).

use async_trait::async_trait;

use crate::providers::registry::{
    ScriptError, ScriptLlm, ScriptRequest, ScriptResponse, ScriptUsage,
};
use crate::voice::keys::ApiKey;

use super::{classify_status, http_client, malformed, retry_after_of, transport_error};

pub const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com";

pub struct GeminiScriptLlm {
    client: reqwest::Client,
    /// MF1 success-body bound; `MAX_SCRIPT_RESPONSE_BYTES` in production,
    /// overridable only in tests so the oversized fixture stays small.
    response_cap: usize,
    base_url: String,
    key: ApiKey,
    model: String,
}

impl GeminiScriptLlm {
    pub fn new(key: ApiKey, model: String) -> Result<GeminiScriptLlm, ScriptError> {
        Self::with_base_url(GEMINI_BASE_URL.to_string(), key, model)
    }

    /// Fixture seam: tests point the fixed host at a loopback server. The
    /// connection store never exposes this — the Gemini preset's URL is
    /// adapter-owned (charter §6.3).
    pub fn with_base_url(
        base_url: String,
        key: ApiKey,
        model: String,
    ) -> Result<GeminiScriptLlm, ScriptError> {
        Ok(GeminiScriptLlm {
            client: http_client()?,
            response_cap: super::MAX_SCRIPT_RESPONSE_BYTES,
            base_url,
            key,
            model,
        })
    }
}

impl GeminiScriptLlm {
    #[cfg(test)]
    fn with_response_cap(mut self, cap: usize) -> Self {
        self.response_cap = cap;
        self
    }
}

#[async_trait]
impl ScriptLlm for GeminiScriptLlm {
    async fn generate(&mut self, request: ScriptRequest) -> Result<ScriptResponse, ScriptError> {
        let temperature = if self.model.starts_with("gemini-3") {
            1.0
        } else {
            request.temperature
        };
        let mut body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [{ "text": request.user }],
            }],
            "generationConfig": {
                "maxOutputTokens": request.max_output_tokens,
                "temperature": temperature,
                "responseMimeType": "application/json",
            },
        });
        if !request.system.is_empty() {
            body["systemInstruction"] =
                serde_json::json!({ "parts": [{ "text": request.system }] });
        }
        // Thinking depth (owner ruling 2026-08-29, default LOW; verified at
        // https://ai.google.dev/gemini-api/docs/generate-content/thinking,
        // checked 2026-08-29): Gemini 3.x takes
        // generationConfig.thinkingConfig.thinkingLevel (values minimal/
        // low/medium/high; "low" is lawful on every 3.x model, "minimal"
        // errors on 3.7-flash/3.1-pro, and some pro models document only
        // low/high — an F6 override sending Medium must re-verify its
        // model's supported subset before widening this mapping). The 2.5
        // Flash family instead disables via thinkingBudget: 0; 2.5 Pro
        // cannot disable, and other/unknown families get no parameter —
        // never a guessed knob on an unverified model.
        match self.model.as_str() {
            model if model.starts_with("gemini-3") => {
                body["generationConfig"]["thinkingConfig"] =
                    serde_json::json!({ "thinkingLevel": request.effort.wire() });
            }
            model
                if model.starts_with("gemini-2.5-flash")
                    && request.effort == crate::providers::registry::ReasoningEffort::Low =>
            {
                body["generationConfig"]["thinkingConfig"] =
                    serde_json::json!({ "thinkingBudget": 0 });
            }
            _ => {}
        }
        let response = self
            .client
            .post(format!(
                "{}/v1beta/models/{}:generateContent",
                self.base_url, self.model
            ))
            .header("x-goog-api-key", self.key.expose())
            .json(&body)
            .send()
            .await
            .map_err(|err| transport_error(&err))?;
        let status = response.status().as_u16();
        let retry_after = retry_after_of(&response);
        // Success bodies stream through the shared bound (MF1); EVERY error
        // status — the 400 API_KEY_INVALID sniff included — only needs the
        // truncated diagnostic quote (the marker sits at the head of the
        // error JSON), never a success-sized buffer.
        let text_body = if classify_status(status, None).is_none() {
            let bytes = super::read_bounded(response, self.response_cap).await?;
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            super::bounded_error_quote(response).await
        };
        // VA3 precedent: Gemini reports a bad key as HTTP 400 with
        // API_KEY_INVALID in the body — classified as Auth before the
        // generic status mapping can call it a provider error — gated on the
        // 400 status so generated TEXT containing the marker can never
        // misclassify a success as an auth failure.
        if status == 400 && text_body.contains("API_KEY_INVALID") {
            return Err(ScriptError {
                kind: crate::providers::registry::ScriptErrorKind::Auth,
                retryable: false,
                message: "provider rejected the API key".to_string(),
                retry_after_secs: None,
                status: Some(400),
            });
        }
        if let Some(mut error) = classify_status(status, retry_after) {
            let snippet: String = text_body.chars().take(512).collect();
            if !snippet.is_empty() {
                error.message = format!("{} — {snippet}", error.message);
            }
            if error.retry_after_secs.is_none() {
                error.retry_after_secs =
                    crate::voice::channel::gemini::retry_after_secs_in(&text_body);
            }
            return Err(error);
        }
        let payload: serde_json::Value =
            serde_json::from_str(&text_body).map_err(|err| malformed(&err.to_string()))?;
        let parts = payload["candidates"][0]["content"]["parts"]
            .as_array()
            .ok_or_else(|| {
                let block = payload["promptFeedback"]["blockReason"]
                    .as_str()
                    .unwrap_or("no candidates");
                malformed(&format!("no candidate content ({block})"))
            })?;
        let text: String = parts
            .iter()
            .filter(|part| {
                !part
                    .get("thought")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        if text.is_empty() {
            return Err(malformed("candidate carried no text parts"));
        }
        Ok(ScriptResponse {
            text,
            usage: ScriptUsage {
                input_tokens: payload["usageMetadata"]["promptTokenCount"].as_u64(),
                output_tokens: payload["usageMetadata"]["candidatesTokenCount"].as_u64(),
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
            crate::test_support::mock_http::write_response(
                &mut sock,
                &status,
                "application/json",
                body.as_bytes(),
            )
            .await
            .unwrap();
            request
        });
        (base, server)
    }

    fn key() -> ApiKey {
        ApiKey::new("fixture-key-0000".to_string())
    }

    fn request() -> ScriptRequest {
        ScriptRequest {
            system: "SYSTEM".to_string(),
            user: "PROMPT".to_string(),
            max_output_tokens: 256,
            temperature: 0.0,
            effort: Default::default(),
        }
    }

    /// Owner ruling pin (2026-08-29): the default-assembled request
    /// carries LOW effort — thinkingLevel "low" on Gemini 3.x,
    /// thinkingBudget 0 on the 2.5 Flash family, and NO thinking
    /// parameter on 2.5 Pro (which cannot disable thinking).
    #[tokio::test]
    async fn default_requests_carry_low_thinking_per_model_family() {
        let fixture = include_str!("../../../fixtures/providers/gemini_generate_200.json");
        for (model, needle, expected) in [
            ("gemini-3.7-flash", "\"thinkingLevel\":\"low\"", true),
            ("gemini-2.5-flash", "\"thinkingBudget\":0", true),
            ("gemini-2.5-pro", "thinkingConfig", false),
        ] {
            let (base, server) = one_shot("200 OK", fixture).await;
            let mut llm = GeminiScriptLlm::with_base_url(base, key(), model.to_string()).unwrap();
            llm.generate(request()).await.unwrap();
            let captured = server.await.unwrap();
            assert_eq!(
                captured.contains(needle),
                expected,
                "{model}: {needle} expected={expected}"
            );
        }
    }

    #[tokio::test]
    async fn a_success_round_trip_uses_native_generate_content_with_header_auth() {
        let fixture = include_str!("../../../fixtures/providers/gemini_generate_200.json");
        let (base, server) = one_shot("200 OK", fixture).await;
        let mut llm =
            GeminiScriptLlm::with_base_url(base, key(), "gemini-2.5-flash".to_string()).unwrap();
        let response = llm.generate(request()).await.unwrap();
        assert_eq!(response.text, "[{\"text\": \"Native lecture.\"}]");
        assert_eq!(response.usage.input_tokens, Some(21));
        assert_eq!(response.usage.output_tokens, Some(9));
        let captured = server.await.unwrap();
        assert!(captured.contains("POST /v1beta/models/gemini-2.5-flash:generateContent"));
        assert!(captured.contains("x-goog-api-key: fixture-key-0000"));
        assert!(
            !captured.contains("key=fixture"),
            "the key must never ride the URL"
        );
        assert!(captured.contains("maxOutputTokens"));
        assert!(captured.contains("systemInstruction"));
    }

    #[tokio::test]
    async fn api_key_invalid_400_classifies_as_auth_like_the_va3_probe() {
        let (base, _server) = one_shot(
            "400 Bad Request",
            "{\"error\": {\"status\": \"INVALID_ARGUMENT\", \"message\": \"API_KEY_INVALID: nope\"}}",
        )
        .await;
        let mut llm = GeminiScriptLlm::with_base_url(base, key(), "m".to_string()).unwrap();
        let error = llm.generate(request()).await.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Auth);
    }

    #[tokio::test]
    async fn an_oversized_success_body_aborts_at_the_wire_bound() {
        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(4096));
        let (base, _server) = one_shot("200 OK", &big).await;
        let mut llm = GeminiScriptLlm::with_base_url(base, key(), "m".to_string())
            .unwrap()
            .with_response_cap(512);
        let error = llm.generate(request()).await.unwrap_err();
        assert!(error.message.contains("byte bound"), "{}", error.message);
    }

    #[tokio::test]
    async fn blocked_prompts_and_error_statuses_classify_distinctly() {
        let (base, _server) = one_shot(
            "200 OK",
            "{\"promptFeedback\": {\"blockReason\": \"SAFETY\"}}",
        )
        .await;
        let mut llm = GeminiScriptLlm::with_base_url(base, key(), "m".to_string()).unwrap();
        let error = llm.generate(request()).await.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Malformed);
        assert!(error.message.contains("SAFETY"));

        for (status, kind) in [
            ("403 Forbidden", ScriptErrorKind::Auth),
            ("429 Too Many Requests", ScriptErrorKind::Quota),
            ("404 Not Found", ScriptErrorKind::ModelNotFound),
            ("503 Service Unavailable", ScriptErrorKind::Provider),
        ] {
            let (base, _server) = one_shot(status, "{}").await;
            let mut llm = GeminiScriptLlm::with_base_url(base, key(), "m".to_string()).unwrap();
            assert_eq!(llm.generate(request()).await.unwrap_err().kind, kind);
        }
    }

    #[tokio::test]
    async fn gemini_3_clamps_temperature_to_one_and_sets_json_mimetype() {
        let fixture = include_str!("../../../fixtures/providers/gemini_generate_200.json");
        let (base, server) = one_shot("200 OK", fixture).await;
        let mut llm =
            GeminiScriptLlm::with_base_url(base, key(), "gemini-3.8-flash".to_string()).unwrap();
        let mut req = request();
        req.temperature = 0.0;
        llm.generate(req).await.unwrap();
        let captured = server.await.unwrap();
        assert!(captured.contains("\"temperature\":1.0"));
        assert!(captured.contains("\"responseMimeType\":\"application/json\""));
    }

    #[tokio::test]
    async fn thought_parts_are_excluded_from_response_text() {
        let body = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [
                        { "text": "I am thinking about the lecture...", "thought": true },
                        { "text": "[{\"text\": \"Actual lecture.\"}]" }
                    ]
                }
            }]
        });
        let (base, _server) = one_shot("200 OK", &body.to_string()).await;
        let mut llm =
            GeminiScriptLlm::with_base_url(base, key(), "gemini-3.8-flash".to_string()).unwrap();
        let response = llm.generate(request()).await.unwrap();
        assert_eq!(response.text, "[{\"text\": \"Actual lecture.\"}]");
        assert!(!response.text.contains("thinking"));
    }
}
