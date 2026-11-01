//! OpenAI TTS adapter — the S-3 deliberately timestamp-less provider,
//! proving the universal local-align fallback path (charter §1.4, T16:
//! never exclude a provider for lacking native timestamps, and never claim
//! native timing when fallback generated it — this adapter returns
//! `native_words: None` always).
//!
//! Provenance (owner law: current docs, never memory):
//! - Create speech: https://developers.openai.com/api/docs/api-reference/audio
//!   (checked 2026-08-28): POST `/v1/audio/speech`; required `input` (max
//!   4096 characters), `model` (`gpt-4o-mini-tts`, `tts-1`, `tts-1-hd`, …),
//!   `voice` (`alloy`, `ash`, …); `response_format` one of mp3/opus/aac/
//!   flac/wav/pcm — `wav` requested here because everything downstream
//!   speaks WAV; the response body is the audio file content.
//! - Auth: https://developers.openai.com/api/docs/api-reference (checked
//!   2026-08-28): `Authorization: Bearer` on `https://api.openai.com/v1`.
//!
//! §3.6 conformance record (mirrored in fixtures/providers/tts_conformance.json):
//! - Protocol version: none — the API versions through dated model ids
//!   (`gpt-4o-mini-tts-2025-12-15`); no version header exists to pin.
//! - Error/retry semantics: HTTP statuses classify per the shared §5.5
//!   table with Retry-After preserved; retries stay within the same
//!   recorded binding.
//! - Cheap probe/model-list: `GET /v1/models` (standard OpenAI listing;
//!   adapter does not call it — S-4's picker surface will).
//! - Retention/disclosure: https://developers.openai.com/api/docs/guides/your-data
//!   (verified 2026-08-28: API data not used for training by default, abuse
//!   logs up to 30 days); usage policies at
//!   https://openai.com/policies/usage-policies (linked policy; page is
//!   bot-gated, cited as the disclosure-duty source).
//! - AI-voice disclosure: OpenAI's usage policies require disclosure of
//!   AI-generated content to users; the app-level disclosure duty (charter
//!   §6) stands regardless.
//!
//! A23 bounds: the response body is capped at `MAX_AUDIO_BYTES`, written to
//! a temp path and renamed — no half-written artifact ever wears its final
//! name. The 4096-character input limit is enforced before the wire so the
//! refusal names the real constraint instead of echoing a provider 400.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::providers::registry::{ScriptError, ScriptErrorKind, TtsArtifact, TtsSynth};
use crate::voice::keys::ApiKey;

use super::{classify_status, http_client, read_bounded, retry_after_of, transport_error};

const OPENAI_BASE_URL: &str = "https://api.openai.com";
const MAX_INPUT_CHARS: usize = 4096;
/// ~10 minutes of 48kHz stereo 16-bit WAV — far above any sentence segment.
/// Enforced at the WIRE layer (A23): the body is read chunk-by-chunk through
/// `read_bounded` and aborted mid-stream past the cap.
const MAX_AUDIO_BYTES: usize = 48_000 * 4 * 600;

pub struct OpenaiTts {
    client: reqwest::Client,
    base_url: String,
    key: ApiKey,
    model: String,
    staging_dir: PathBuf,
}

impl OpenaiTts {
    pub fn new(key: ApiKey, model: String, staging_dir: PathBuf) -> Result<OpenaiTts, ScriptError> {
        Self::with_base_url(OPENAI_BASE_URL.to_string(), key, model, staging_dir)
    }

    #[cfg(test)]
    fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub fn with_base_url(
        base_url: String,
        key: ApiKey,
        model: String,
        staging_dir: PathBuf,
    ) -> Result<OpenaiTts, ScriptError> {
        Ok(OpenaiTts {
            client: http_client()?,
            base_url,
            key,
            model,
            staging_dir,
        })
    }
}

fn provider_error(detail: String) -> ScriptError {
    ScriptError {
        kind: ScriptErrorKind::Provider,
        retryable: false,
        message: detail,
        retry_after_secs: None,
        status: None,
    }
}

#[async_trait]
impl TtsSynth for OpenaiTts {
    async fn synthesize(&mut self, text: &str, voice: &str) -> Result<TtsArtifact, ScriptError> {
        let chars = text.chars().count();
        if chars > MAX_INPUT_CHARS {
            return Err(provider_error(format!(
                "segment text is {chars} characters; the provider caps input at \
                 {MAX_INPUT_CHARS} — this segment cannot be synthesized on this connection"
            )));
        }
        let body = serde_json::json!({
            "model": self.model,
            "voice": voice,
            "input": text,
            "response_format": "wav",
        });
        let response = self
            .client
            .post(format!("{}/v1/audio/speech", self.base_url))
            .header("authorization", format!("Bearer {}", self.key.expose()))
            .json(&body)
            .send()
            .await
            .map_err(|err| transport_error(&err))?;
        let status = response.status().as_u16();
        let retry_after = retry_after_of(&response);
        if let Some(mut error) = classify_status(status, retry_after) {
            let body = super::bounded_error_quote(response).await;
            if !body.is_empty() {
                error.message = format!("{} — {body}", error.message);
            }
            return Err(error);
        }
        let bytes = read_bounded(response, MAX_AUDIO_BYTES).await?;
        if bytes.is_empty() {
            return Err(provider_error("provider returned no audio".to_string()));
        }

        std::fs::create_dir_all(&self.staging_dir)
            .map_err(|err| provider_error(format!("staging dir: {err}")))?;
        let final_path = self
            .staging_dir
            .join(format!("openai-{}.wav", crate::storage::new_id()));
        let tmp_path = final_path.with_extension("wav.tmp");
        std::fs::write(&tmp_path, &bytes)
            .map_err(|err| provider_error(format!("staging file: {err}")))?;
        std::fs::rename(&tmp_path, &final_path)
            .map_err(|err| provider_error(format!("atomic rename: {err}")))?;

        Ok(TtsArtifact {
            audio_path: final_path,
            // Deliberate: this provider has no native timing; the align
            // phase is the supported path (T16), and claiming otherwise
            // here would be the exact lie the charter bans.
            native_words: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    fn tiny_wav() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 24_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut wav = hound::WavWriter::new(std::io::Cursor::new(&mut bytes), spec).unwrap();
            for sample in 0..48i16 {
                wav.write_sample(sample).unwrap();
            }
            wav.finalize().unwrap();
        }
        bytes
    }

    async fn one_shot(status: &str, body: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let status = status.to_string();
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
                &[("Content-Type", "audio/wav"), ("Retry-After", "30")],
                Some(body.len()),
            )
            .await
            .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut sock, &body)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut sock).await.unwrap();
            request
        });
        (base, server)
    }

    fn adapter(base: String, dir: &std::path::Path) -> OpenaiTts {
        OpenaiTts::with_base_url(
            base,
            ApiKey::new("sk-fixture-0000".to_string()),
            "gpt-4o-mini-tts".to_string(),
            dir.to_path_buf(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_success_round_trip_yields_audio_and_never_claims_timing() {
        let dir = tempfile::tempdir().unwrap();
        let (base, server) = one_shot("200 OK", tiny_wav()).await;
        let mut tts = adapter(base, dir.path());
        let artifact = tts.synthesize("Hello world", "alloy").await.unwrap();
        assert!(artifact.audio_path.exists());
        assert!(
            artifact.native_words.is_none(),
            "timestamp-less provider must never claim native timing (T16)"
        );
        hound::WavReader::open(&artifact.audio_path).expect("bytes are the wav verbatim");

        let captured = server.await.unwrap();
        assert!(captured.contains("POST /v1/audio/speech"));
        assert!(captured.contains("authorization: Bearer sk-fixture-0000"));
        let body_start = captured.find("\r\n\r\n").unwrap() + 4;
        let body: serde_json::Value = serde_json::from_str(&captured[body_start..]).unwrap();
        assert_eq!(body["model"], "gpt-4o-mini-tts");
        assert_eq!(body["voice"], "alloy");
        assert_eq!(body["input"], "Hello world");
        assert_eq!(body["response_format"], "wav");
    }

    #[tokio::test]
    async fn error_statuses_classify_to_the_charter_taxonomy() {
        let dir = tempfile::tempdir().unwrap();
        for (status, kind) in [
            ("401 Unauthorized", ScriptErrorKind::Auth),
            ("403 Forbidden", ScriptErrorKind::Auth),
            ("404 Not Found", ScriptErrorKind::ModelNotFound),
            ("429 Too Many Requests", ScriptErrorKind::Quota),
            ("500 Internal Server Error", ScriptErrorKind::Provider),
        ] {
            let (base, _server) = one_shot(status, br#"{"error":{"message":"x"}}"#.to_vec()).await;
            let mut tts = adapter(base, dir.path());
            assert_eq!(
                tts.synthesize("Hello", "alloy").await.unwrap_err().kind,
                kind,
                "status {status}"
            );
        }
    }

    #[tokio::test]
    async fn the_input_cap_refuses_before_the_wire() {
        let dir = tempfile::tempdir().unwrap();
        // Dead base URL: if the guard leaks, the wire attempt fails as
        // Transport instead of Provider and the assertion catches it.
        let mut tts = adapter("http://127.0.0.1:1".to_string(), dir.path());
        let long = "x".repeat(MAX_INPUT_CHARS + 1);
        let error = tts.synthesize(&long, "alloy").await.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Provider);
        assert!(error.message.contains("4096"));
    }

    #[tokio::test]
    async fn the_wire_bound_aborts_mid_stream_not_after_buffering() {
        // read_bounded with a tiny limit against an oversized body: the
        // abort happens during the chunked read (the A23 wire-layer law),
        // and the error names the bound.
        let (base, _server) = one_shot("200 OK", vec![0u8; 8192]).await;
        let client = super::http_client().unwrap();
        let response = client
            .get(format!("{base}/v1/audio/speech"))
            .send()
            .await
            .unwrap();
        let error = super::super::read_bounded(response, 1024)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Provider);
        assert!(
            error.message.contains("1024-byte bound"),
            "{}",
            error.message
        );
    }

    #[tokio::test]
    async fn quota_preserves_retry_after_and_a_stall_times_out_as_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _server) = one_shot("429 Too Many Requests", b"{}".to_vec()).await;
        let mut tts = adapter(base, dir.path());
        let error = tts.synthesize("Hello", "alloy").await.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Quota);
        assert_eq!(
            error.retry_after_secs,
            Some(30),
            "Retry-After survives the adapter"
        );

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::mem::forget(listener);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap();
        let mut tts = adapter(format!("http://127.0.0.1:{port}"), dir.path()).with_client(client);
        assert_eq!(
            tts.synthesize("Hello", "alloy").await.unwrap_err().kind,
            ScriptErrorKind::Timeout
        );
    }

    #[test]
    fn the_conformance_record_is_present_and_complete() {
        let record: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/providers/tts_conformance.json"
        ))
        .unwrap();
        let openai = &record["openai_tts"];
        for field in [
            "protocol_version",
            "error_retry",
            "probe_model_list",
            "retention_disclosure",
            "terms",
            "ai_voice_disclosure",
        ] {
            assert!(
                openai[field].as_str().is_some_and(|v| !v.is_empty()),
                "conformance record missing {field}"
            );
        }
    }

    #[tokio::test]
    async fn an_empty_audio_body_is_a_provider_error_not_a_success() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _server) = one_shot("200 OK", Vec::new()).await;
        let mut tts = adapter(base, dir.path());
        assert_eq!(
            tts.synthesize("Hello", "alloy").await.unwrap_err().kind,
            ScriptErrorKind::Provider
        );
    }
}
