//! Gemini TTS adapter — native `generateContent` with audio modality.
//! Current 3.8 request contract reviewed 2026-10-27:
//! https://ai.google.dev/gemini-api/docs/generate-content/speech-generation
//! Conformance: current_speech_request_keeps_transcript_separate_from_delivery below.
//!
//! Provenance (owner law: current docs, never memory):
//! - Gemini Speech Generation: https://ai.google.dev/gemini-api/docs/speech-generation (checked 2026-09-13)
//! - Gemini 3.1 Flash TTS Preview: https://ai.google.dev/gemini-api/docs/models/gemini-3.1-flash-tts-preview (checked 2026-09-13)
//! - Endpoint: `POST /v1beta/models/{model}:generateContent` with header `x-goog-api-key`
//! - Default Model: `gemini-3.1-flash-tts-preview`
//! - Supported prebuilt voices: `Puck`, `Charon`, `Kore`, `Fenrir`, `Aoede`, `Zephyr`,
//!   `Leda`, `Orus`, `Callirrhoe`, `Autonoe`, `Enceladus`, `Iapetus`, `Achernar`, `Alnilam`, `Schedar`

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::path::PathBuf;

use crate::providers::registry::{ScriptError, ScriptErrorKind, TtsArtifact, TtsSynth};
use crate::voice::keys::ApiKey;

use super::{classify_status, http_client, malformed, retry_after_of, transport_error};

pub const DEFAULT_GEMINI_TTS_MODEL: &str = "gemini-3.1-flash-tts-preview";
pub const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com";
const MAX_AUDIO_BYTES: usize = 50 * 1024 * 1024; // 50 MB cap

/// Resolves the model slug for Gemini speech generation.
/// If empty or not containing "tts" or "audio", falls back to `DEFAULT_GEMINI_TTS_MODEL`.
pub fn resolve_model(model: &str) -> &str {
    let trimmed = model.trim();
    if trimmed.is_empty() || (!trimmed.contains("tts") && !trimmed.contains("audio")) {
        DEFAULT_GEMINI_TTS_MODEL
    } else {
        trimmed
    }
}

/// Normalizes voice names against Google Gemini's 15 official prebuilt voices.
/// Defaults unknown voices to "Puck".
pub fn normalize_voice_name(voice: &str) -> &'static str {
    match voice.trim().to_ascii_lowercase().as_str() {
        "puck" => "Puck",
        "charon" => "Charon",
        "kore" => "Kore",
        "fenrir" => "Fenrir",
        "aoede" => "Aoede",
        "zephyr" => "Zephyr",
        "leda" => "Leda",
        "orus" => "Orus",
        "callirrhoe" => "Callirrhoe",
        "autonoe" => "Autonoe",
        "enceladus" => "Enceladus",
        "iapetus" => "Iapetus",
        "achernar" => "Achernar",
        "alnilam" => "Alnilam",
        "schedar" => "Schedar",
        _ => "Puck",
    }
}

/// Builds the JSON request payload for Gemini `generateContent` speech generation.
pub fn build_speech_request(text: &str, voice_name: &str) -> serde_json::Value {
    serde_json::json!({
        "contents": [{
            "role": "user",
            "parts": [{
                "text": format!("Please read this text aloud verbatim with natural cadence and tone, without commentary:\n\n{text}")
            }]
        }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": voice_name
                    }
                }
            }
        }
    })
}

pub fn build_current_speech_request(text: &str, voice: &str) -> serde_json::Value {
    serde_json::json!({
        "contents":[{"role":"user","parts":[{"text":text}]}],
        "generationConfig":{
            "responseModalities":["AUDIO"],
            "responseFormat":{"audio":{"mimeType":"AUDIO_WAV","sampleRate":24000}},
            "speechConfig":{"voiceConfig":{"voice":voice}}
        }
    })
}

#[cfg(test)]
mod current_request_tests {
    use super::*;

    #[test]
    fn current_speech_request_keeps_transcript_separate_from_delivery() {
        let body = build_current_speech_request("مرحبا SQL 42", "voice_library_1");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "مرحبا SQL 42");
        assert_eq!(
            body["generationConfig"]["speechConfig"]["voiceConfig"]["voice"],
            "voice_library_1"
        );
        assert_eq!(
            body["generationConfig"]["responseFormat"]["audio"]["mimeType"],
            "AUDIO_WAV"
        );
        assert!(body["generationConfig"]["speechConfig"]["voiceConfig"]
            .get("prebuiltVoiceConfig")
            .is_none());
    }
}

pub struct GeminiTts {
    client: reqwest::Client,
    base_url: String,
    key: ApiKey,
    model: String,
    staging_dir: PathBuf,
}

impl GeminiTts {
    pub fn new(key: ApiKey, model: String, staging_dir: PathBuf) -> Result<GeminiTts, ScriptError> {
        Self::with_base_url(GEMINI_BASE_URL.to_string(), key, model, staging_dir)
    }

    pub fn with_base_url(
        base_url: String,
        key: ApiKey,
        model: String,
        staging_dir: PathBuf,
    ) -> Result<GeminiTts, ScriptError> {
        Ok(GeminiTts {
            client: http_client()?,
            base_url,
            key,
            model,
            staging_dir,
        })
    }
}

#[async_trait]
impl TtsSynth for GeminiTts {
    async fn synthesize(&mut self, text: &str, voice: &str) -> Result<TtsArtifact, ScriptError> {
        if text.trim().chars().all(|c| !c.is_alphanumeric()) {
            std::fs::create_dir_all(&self.staging_dir).map_err(|e| ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: false,
                message: format!("failed to create staging dir: {e}"),
                retry_after_secs: None,
                status: None,
            })?;
            let final_path = self
                .staging_dir
                .join(format!("gemini-{}.wav", crate::storage::new_id()));
            let silence = crate::render::mp3::silence_wav(250);
            std::fs::write(&final_path, silence).map_err(|e| ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: false,
                message: format!("failed to write WAV file: {e}"),
                retry_after_secs: None,
                status: None,
            })?;
            return Ok(TtsArtifact {
                audio_path: final_path,
                native_words: None,
            });
        }

        let model = resolve_model(&self.model);
        let body = if model.starts_with("gemini-3.8-") {
            build_current_speech_request(text, voice)
        } else {
            build_speech_request(text, normalize_voice_name(voice))
        };

        let response = self
            .client
            .post(format!(
                "{}/v1beta/models/{}:generateContent",
                self.base_url, model
            ))
            .header("x-goog-api-key", self.key.expose())
            .json(&body)
            .send()
            .await
            .map_err(|err| transport_error(&err))?;

        let status = response.status().as_u16();
        let retry_after = retry_after_of(&response);

        if status != 200 {
            let error_text = super::bounded_error_quote(response).await;
            if status == 400 && error_text.contains("API_KEY_INVALID") {
                return Err(ScriptError {
                    kind: ScriptErrorKind::Auth,
                    retryable: false,
                    message: "provider rejected the API key".to_string(),
                    retry_after_secs: None,
                    status: Some(400),
                });
            }
            if let Some(mut err) = classify_status(status, retry_after) {
                if !error_text.is_empty() {
                    err.message = format!("{} — {}", err.message, error_text);
                }
                if err.retry_after_secs.is_none() {
                    err.retry_after_secs =
                        crate::voice::channel::gemini::retry_after_secs_in(&error_text);
                }
                return Err(err);
            }
            return Err(ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: false,
                message: format!("provider HTTP {status} — {error_text}"),
                retry_after_secs: None,
                status: Some(status),
            });
        }

        let bytes = super::read_bounded(response, MAX_AUDIO_BYTES).await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| malformed(&e.to_string()))?;

        let inline_data = payload["candidates"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|cand| cand["content"]["parts"].as_array())
            .and_then(|parts| parts.iter().find_map(|p| p.get("inlineData")))
            .ok_or_else(|| ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: true,
                message: "Gemini returned no inline audio data in candidate parts".to_string(),
                retry_after_secs: None,
                status: None,
            })?;

        let mime_type = inline_data["mimeType"].as_str().unwrap_or("");
        let b64_data = inline_data["data"].as_str().ok_or_else(|| ScriptError {
            kind: ScriptErrorKind::Provider,
            retryable: true,
            message: "inlineData missing base64 audio data".to_string(),
            retry_after_secs: None,
            status: None,
        })?;

        let raw_audio = B64.decode(b64_data.trim()).map_err(|e| ScriptError {
            kind: ScriptErrorKind::Provider,
            retryable: false,
            message: format!("failed to base64-decode audio: {e}"),
            retry_after_secs: None,
            status: None,
        })?;

        std::fs::create_dir_all(&self.staging_dir).map_err(|e| ScriptError {
            kind: ScriptErrorKind::Provider,
            retryable: false,
            message: format!("failed to create staging dir: {e}"),
            retry_after_secs: None,
            status: None,
        })?;

        let final_path = self
            .staging_dir
            .join(format!("gemini-{}.wav", crate::storage::new_id()));

        let staging_path = tempfile::NamedTempFile::new_in(&self.staging_dir)
            .map_err(|_| malformed("cannot stage speech audio"))?
            .into_temp_path();
        if raw_audio.is_empty()
            || (!raw_audio.starts_with(b"RIFF")
                && !mime_type.to_ascii_lowercase().starts_with("audio/l16"))
        {
            return Err(malformed("unsupported speech audio format"));
        }
        if raw_audio.starts_with(b"RIFF") {
            hound::WavReader::new(std::io::Cursor::new(&raw_audio))
                .map_err(|_| malformed("invalid speech WAV"))?;
            std::fs::write(&staging_path, &raw_audio).map_err(|e| ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: false,
                message: format!("failed to write WAV file: {e}"),
                retry_after_secs: None,
                status: None,
            })?;
        } else {
            if raw_audio.len() % 2 != 0 {
                return Err(malformed("invalid speech PCM length"));
            }
            let sample_rate = if mime_type.contains("rate=") {
                mime_type
                    .split("rate=")
                    .nth(1)
                    .and_then(|s| s.split(';').next())
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(24000)
            } else {
                24000
            };
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer =
                hound::WavWriter::create(&staging_path, spec).map_err(|e| ScriptError {
                    kind: ScriptErrorKind::Provider,
                    retryable: false,
                    message: format!("failed to initialize WavWriter: {e}"),
                    retry_after_secs: None,
                    status: None,
                })?;
            for chunk in raw_audio.chunks_exact(2) {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                writer.write_sample(sample).map_err(|e| ScriptError {
                    kind: ScriptErrorKind::Provider,
                    retryable: false,
                    message: format!("failed to write WAV sample: {e}"),
                    retry_after_secs: None,
                    status: None,
                })?;
            }
            writer.finalize().map_err(|e| ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: false,
                message: format!("failed to finalize WAV file: {e}"),
                retry_after_secs: None,
                status: None,
            })?;
        }

        staging_path
            .persist_noclobber(&final_path)
            .map_err(|_| malformed("cannot publish speech audio"))?;

        Ok(TtsArtifact {
            audio_path: final_path,
            native_words: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    async fn one_shot(status: &str, body: String) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let status = status.to_string();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let mut request = String::new();
            loop {
                let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf)
                    .await
                    .unwrap();
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

    #[test]
    fn model_fallback_resolves_to_default_tts_model() {
        assert_eq!(resolve_model("gemini-3.8-flash"), DEFAULT_GEMINI_TTS_MODEL);
        assert_eq!(resolve_model("gemini-2.5-flash"), DEFAULT_GEMINI_TTS_MODEL);
        assert_eq!(resolve_model(""), DEFAULT_GEMINI_TTS_MODEL);
        assert_eq!(resolve_model("   "), DEFAULT_GEMINI_TTS_MODEL);
    }

    #[test]
    fn explicit_tts_model_is_preserved() {
        assert_eq!(
            resolve_model("gemini-2.5-flash-preview-tts"),
            "gemini-2.5-flash-preview-tts"
        );
        assert_eq!(
            resolve_model("gemini-3.1-flash-tts-preview"),
            "gemini-3.1-flash-tts-preview"
        );
        assert_eq!(resolve_model("custom-audio-model"), "custom-audio-model");
        assert_eq!(
            resolve_model("  gemini-3.1-flash-tts-preview  "),
            "gemini-3.1-flash-tts-preview"
        );
    }

    #[test]
    fn voice_name_resolution_and_casings() {
        let expected_voices = [
            ("Puck", "puck"),
            ("Charon", "charon"),
            ("Kore", "kore"),
            ("Fenrir", "fenrir"),
            ("Aoede", "aoede"),
            ("Zephyr", "zephyr"),
            ("Leda", "leda"),
            ("Orus", "orus"),
            ("Callirrhoe", "callirrhoe"),
            ("Autonoe", "autonoe"),
            ("Enceladus", "enceladus"),
            ("Iapetus", "iapetus"),
            ("Achernar", "achernar"),
            ("Alnilam", "alnilam"),
            ("Schedar", "schedar"),
        ];

        for (expected, input) in expected_voices {
            assert_eq!(normalize_voice_name(input), expected);
            assert_eq!(normalize_voice_name(&input.to_ascii_uppercase()), expected);
            assert_eq!(normalize_voice_name(&format!("  {}  ", input)), expected);
        }

        // Mixed casings
        assert_eq!(normalize_voice_name("pUcK"), "Puck");
        assert_eq!(normalize_voice_name("cHaRoN"), "Charon");
        assert_eq!(normalize_voice_name("ZePhYr"), "Zephyr");

        // Unknown fallbacks default to Puck
        assert_eq!(normalize_voice_name("unknown"), "Puck");
        assert_eq!(normalize_voice_name("alloy"), "Puck");
        assert_eq!(normalize_voice_name(""), "Puck");
        assert_eq!(normalize_voice_name("   "), "Puck");
    }

    #[test]
    fn speech_config_json_payload_serialization() {
        let payload = build_speech_request("Test speech text", "Aoede");
        assert_eq!(payload["contents"][0]["role"], "user");
        assert_eq!(
            payload["contents"][0]["parts"][0]["text"],
            "Please read this text aloud verbatim with natural cadence and tone, without commentary:\n\nTest speech text"
        );
        assert_eq!(
            payload["generationConfig"]["responseModalities"],
            serde_json::json!(["AUDIO"])
        );
        assert_eq!(
            payload["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
                ["voiceName"],
            "Aoede"
        );
    }

    #[tokio::test]
    async fn round_trip_synthesize_with_model_fallback_and_voice() {
        let wav_data = tiny_wav();
        let b64_wav = B64.encode(&wav_data);
        let response_body = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "inlineData": {
                            "mimeType": "audio/wav",
                            "data": b64_wav
                        }
                    }]
                }
            }]
        })
        .to_string();

        let (base, server) = one_shot("200 OK", response_body).await;
        let dir = tempfile::tempdir().unwrap();
        let mut tts = GeminiTts::with_base_url(
            base,
            ApiKey::new("test-gemini-key".to_string()),
            "gemini-3.8-flash".to_string(), // should fallback to DEFAULT_GEMINI_TTS_MODEL
            dir.path().to_path_buf(),
        )
        .unwrap();

        let artifact = tts.synthesize("Hello world", "charon").await.unwrap();
        assert!(artifact.audio_path.exists());
        assert!(artifact.native_words.is_none());

        let captured = server.await.unwrap();
        assert!(
            captured.contains("POST /v1beta/models/gemini-3.1-flash-tts-preview:generateContent")
        );
        assert!(captured.contains("x-goog-api-key: test-gemini-key"));
        let body_start = captured.find("\r\n\r\n").unwrap() + 4;
        let body: serde_json::Value = serde_json::from_str(&captured[body_start..]).unwrap();
        assert_eq!(
            body["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
                ["voiceName"],
            "Charon"
        );
    }

    #[tokio::test]
    async fn non_alphanumeric_text_synthesizes_silence_without_http() {
        let dir = tempfile::tempdir().unwrap();
        // Use an invalid dummy URL to guarantee that if an HTTP call were attempted, it would fail
        let mut tts = GeminiTts::with_base_url(
            "http://127.0.0.1:1".to_string(),
            ApiKey::new("test-gemini-key".to_string()),
            DEFAULT_GEMINI_TTS_MODEL.to_string(),
            dir.path().to_path_buf(),
        )
        .unwrap();

        for non_alpha in [".", "...", "   ", " - -- ", "!?", "\n\t."] {
            let artifact = tts.synthesize(non_alpha, "puck").await.unwrap();
            assert!(artifact.audio_path.exists());
            assert!(artifact.native_words.is_none());

            // Validate that the produced WAV is a valid 250ms silence WAV
            let reader = hound::WavReader::open(&artifact.audio_path).unwrap();
            let spec = reader.spec();
            assert_eq!(spec.sample_rate, 24_000);
            let duration_ms = (reader.duration() as i64 * 1000) / i64::from(spec.sample_rate);
            assert_eq!(duration_ms, 250);
        }
    }

    #[tokio::test]
    async fn missing_inline_audio_returns_retryable_provider_error() {
        let response_body = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "text": "Sorry, I cannot read that aloud."
                    }]
                }
            }]
        })
        .to_string();

        let (base, _server) = one_shot("200 OK", response_body).await;
        let dir = tempfile::tempdir().unwrap();
        let mut tts = GeminiTts::with_base_url(
            base,
            ApiKey::new("test-gemini-key".to_string()),
            DEFAULT_GEMINI_TTS_MODEL.to_string(),
            dir.path().to_path_buf(),
        )
        .unwrap();

        let err = tts
            .synthesize("Valid speakable text", "puck")
            .await
            .unwrap_err();
        assert_eq!(err.kind, ScriptErrorKind::Provider);
        assert!(
            err.retryable,
            "missing audio in candidate parts must be retryable"
        );
        assert!(err.message.contains("no inline audio data"));
    }

    #[tokio::test]
    async fn rest_429_quota_carries_retry_after_from_body() {
        let error_body = serde_json::json!({
            "error": {
                "code": 429,
                "message": "You exceeded your current quota, please check your plan \
                    and billing details. Please retry in 1.746835114s.",
                "status": "RESOURCE_EXHAUSTED"
            }
        })
        .to_string();

        let (base, _server) = one_shot("429 Too Many Requests", error_body).await;
        let dir = tempfile::tempdir().unwrap();
        let mut tts = GeminiTts::with_base_url(
            base,
            ApiKey::new("test-gemini-key".to_string()),
            DEFAULT_GEMINI_TTS_MODEL.to_string(),
            dir.path().to_path_buf(),
        )
        .unwrap();

        let err = tts.synthesize("Hello world", "puck").await.unwrap_err();
        assert_eq!(err.kind, ScriptErrorKind::Quota);
        assert!(err.retryable);
        assert_eq!(
            err.retry_after_secs,
            Some(1),
            "REST 429 body with 'Please retry in 1.7s' must parse to 1s, not None"
        );
    }
}
