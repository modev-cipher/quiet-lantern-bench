//! Cartesia TTS adapter — the S-3 native-word-timing provider (owner
//! ruling 2026-08-28: Cartesia primary, Azure deferred).
//!
//! Provenance (owner law: current docs, never memory):
//! - SSE endpoint: https://docs.cartesia.ai/api-reference/tts/sse (checked
//!   2026-08-28): POST `/tts/sse`, `Authorization: Bearer sk_car_...` plus
//!   required `Cartesia-Version: 2026-08-14`; required `model_id`
//!   (`sonic-3.6` et al.), `transcript`, `voice` (id string), and
//!   `output_format` — this endpoint's container is `raw` ONLY (no wav/mp3),
//!   encodings `pcm_s16le`/`pcm_f32le`/…, sample rates incl. 24000.
//!   `add_timestamps: true` yields `timestamps` events carrying parallel
//!   `words`/`start`/`end` arrays in SECONDS; `use_normalized_timestamps`
//!   selects normalized vs original text — set false here so words match
//!   the transcript we sent (the §3.5 text-compatibility gate depends on
//!   that). Stream events are `data: <json>` frames discriminated by
//!   `type`: `chunk` (base64 audio `data`), `timestamps`, `done`, `error`
//!   (`status_code`, `title`, `message`, `request_id`).
//! - Bytes endpoint (context for the container choice):
//!   https://docs.cartesia.ai/api-reference/tts/bytes (checked 2026-08-28).
//!
//! §3.6 conformance record (mirrored in fixtures/providers/tts_conformance.json):
//! - Protocol version: `Cartesia-Version: 2026-08-14` (required header; the
//!   only documented accepted value at check time).
//! - Error/retry semantics: HTTP statuses classify per the shared §5.5
//!   table with Retry-After preserved; in-stream `error` events carry
//!   `status_code`/`title`/`message`/`request_id` and classify by their
//!   status code. Retries stay within the same recorded binding.
//! - Cheap probe/model-list: `GET /voices` (List Voices,
//!   https://docs.cartesia.ai/api-reference/voices/list, adapter does not
//!   call it — S-4's picker surface will).
//! - Retention/disclosure: https://cartesia.ai/legal/privacy (verified
//!   2026-08-28; no concrete retention timeframes published) and
//!   https://cartesia.ai/legal/terms (acceptable-use limits on cloned/
//!   impersonated voices).
//! - AI-voice disclosure: Cartesia's terms restrict impersonation but do
//!   not mandate AI-generation disclosure; the app-level disclosure duty
//!   (charter §6) stands regardless of provider.
//!
//! Wire quirks kept adapter-local (charter §3.3 law 4): raw PCM is wrapped
//! into a mono 16-bit WAV here (hound), because everything downstream —
//! the wav cache, align, the mp3 render — speaks WAV; second-based
//! timestamps are normalized to the app's `{w, s, e}` millisecond shape.
//! Audio is bounded (A23): a stream exceeding `MAX_PCM_BYTES` aborts as a
//! provider error rather than filling the disk, and the WAV is written to
//! a temp path then renamed — a crash never leaves a half-written artifact
//! behind under its final name.

use std::io::Write;
use std::path::PathBuf;

use async_trait::async_trait;
use base64::Engine as _;

use crate::providers::registry::{ScriptError, ScriptErrorKind, TtsArtifact, TtsSynth};
use crate::voice::keys::ApiKey;

use super::{classify_status, http_client, malformed, retry_after_of, transport_error};

const CARTESIA_BASE_URL: &str = "https://api.cartesia.ai";
const CARTESIA_VERSION: &str = "2026-08-14";
const SAMPLE_RATE: u32 = 24_000;
/// A23 bound: ~10 minutes of mono 24kHz s16le. A single script segment is a
/// sentence-scale utterance; anything past this is a runaway stream.
const MAX_PCM_BYTES: usize = 24_000 * 2 * 600;
/// The WIRE-layer bound (A23): the SSE stream is read chunk-by-chunk and
/// aborted the moment it passes this, never accumulated first and measured
/// after. Base64 inflates PCM by 4/3 and the stream also carries timestamp
/// and framing JSON, so the wire budget is 2× the decoded PCM bound.
const MAX_STREAM_BYTES: usize = MAX_PCM_BYTES * 2;

pub struct CartesiaTts {
    client: reqwest::Client,
    base_url: String,
    key: ApiKey,
    model: String,
    staging_dir: PathBuf,
}

impl CartesiaTts {
    pub fn new(
        key: ApiKey,
        model: String,
        staging_dir: PathBuf,
    ) -> Result<CartesiaTts, ScriptError> {
        Self::with_base_url(CARTESIA_BASE_URL.to_string(), key, model, staging_dir)
    }

    #[cfg(test)]
    fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Loopback-fixture constructor; production is the fixed official host
    /// (charter §6.3).
    pub fn with_base_url(
        base_url: String,
        key: ApiKey,
        model: String,
        staging_dir: PathBuf,
    ) -> Result<CartesiaTts, ScriptError> {
        Ok(CartesiaTts {
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

/// Seconds → whole milliseconds, refusing non-finite input at the seam.
fn to_ms(seconds: f64) -> Option<i64> {
    (seconds.is_finite() && seconds >= 0.0).then(|| (seconds * 1000.0).round() as i64)
}

#[derive(Default)]
struct StreamState {
    pcm: Vec<u8>,
    words: Vec<serde_json::Value>,
    done: bool,
}

/// The ONE event classifier, applied identically to events completed on the
/// wire and to the EOF tail a held CR terminator releases — a terminal
/// `error` event classifies by its status code from either path, never as
/// generic truncation.
fn apply_event(state: &mut StreamState, payload: &str) -> Result<(), ScriptError> {
    let event: serde_json::Value =
        serde_json::from_str(payload).map_err(|err| malformed(&err.to_string()))?;
    match event["type"].as_str().unwrap_or_default() {
        "chunk" => {
            let data = event["data"]
                .as_str()
                .ok_or_else(|| malformed("chunk event without base64 data"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|err| malformed(&format!("chunk base64: {err}")))?;
            if state.pcm.len() + bytes.len() > MAX_PCM_BYTES {
                return Err(provider_error(format!(
                    "audio stream exceeded the {MAX_PCM_BYTES}-byte bound for one                      segment; aborting rather than filling the cache"
                )));
            }
            state.pcm.extend_from_slice(&bytes);
        }
        "timestamps" => {
            let stamps = &event["word_timestamps"];
            let (Some(ws), Some(starts), Some(ends)) = (
                stamps["words"].as_array(),
                stamps["start"].as_array(),
                stamps["end"].as_array(),
            ) else {
                return Err(malformed("timestamps event without parallel arrays"));
            };
            if ws.len() != starts.len() || ws.len() != ends.len() {
                return Err(malformed("timestamps arrays are not parallel"));
            }
            for ((w, s), e) in ws.iter().zip(starts).zip(ends) {
                let (Some(w), Some(s), Some(e)) = (
                    w.as_str(),
                    s.as_f64().and_then(to_ms),
                    e.as_f64().and_then(to_ms),
                ) else {
                    return Err(malformed("timestamp entry is not (string, f64, f64)"));
                };
                state
                    .words
                    .push(serde_json::json!({"w": w, "s": s, "e": e}));
            }
        }
        "done" => {
            state.done = true;
        }
        "error" => {
            let status = event["status_code"].as_u64().unwrap_or(0) as u16;
            let detail = format!(
                "{}: {} (request {})",
                event["title"].as_str().unwrap_or("provider error"),
                event["message"].as_str().unwrap_or(""),
                event["request_id"].as_str().unwrap_or("unknown"),
            );
            let mut error =
                classify_status(status, None).unwrap_or_else(|| provider_error(detail.clone()));
            error.message = format!("{} — {detail}", error.message);
            return Err(error);
        }
        _ => {}
    }
    Ok(())
}

#[async_trait]
impl TtsSynth for CartesiaTts {
    async fn synthesize(&mut self, text: &str, voice: &str) -> Result<TtsArtifact, ScriptError> {
        let body = serde_json::json!({
            "model_id": self.model,
            "transcript": text,
            "voice": voice,
            "output_format": {
                "container": "raw",
                "encoding": "pcm_s16le",
                "sample_rate": SAMPLE_RATE,
            },
            "add_timestamps": true,
            "use_normalized_timestamps": false,
        });
        let response = self
            .client
            .post(format!("{}/tts/sse", self.base_url))
            .header("authorization", format!("Bearer {}", self.key.expose()))
            .header("cartesia-version", CARTESIA_VERSION)
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
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if !content_type.starts_with("text/event-stream") {
            return Err(provider_error(format!(
                "expected a text/event-stream response, got {content_type:?}"
            )));
        }

        // Incremental, bounded SSE consumption (WHATWG framing via
        // `super::sse`, Hermes S-3 MF8): events are parsed as the wire
        // delivers them, under a total wire budget plus the scanner's own
        // per-event and event-count bounds — never buffered whole first.
        let mut response = response;
        let mut scanner = super::sse::SseScanner::new();
        let mut wire_bytes = 0usize;
        let mut state = StreamState::default();
        'wire: while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| transport_error(&err))?
        {
            wire_bytes += chunk.len();
            if wire_bytes > MAX_STREAM_BYTES {
                return Err(provider_error(format!(
                    "sse stream exceeded the {MAX_STREAM_BYTES}-byte wire bound mid-stream"
                )));
            }
            for payload in scanner.push(&chunk)? {
                apply_event(&mut state, &payload)?;
                if state.done {
                    break 'wire;
                }
            }
        }
        if !state.done {
            // A CR-framed stream can complete its final event only at EOF;
            // drain the scanner and classify those events IDENTICALLY —
            // a terminal error event must classify as itself, and a done
            // event missing its blank-line terminator must NOT count.
            let (tail, leftover) = scanner.finish();
            for payload in tail {
                apply_event(&mut state, &payload)?;
            }
            if !state.done {
                let detail = leftover.map(|d| format!(" ({d})")).unwrap_or_default();
                return Err(malformed(&format!(
                    "stream ended without a done event{detail}"
                )));
            }
        }
        let pcm = state.pcm;
        let words = state.words;
        if pcm.is_empty() {
            return Err(malformed("stream carried no audio chunks"));
        }

        // Atomic WAV materialization (A23): temp write, then rename.
        std::fs::create_dir_all(&self.staging_dir)
            .map_err(|err| provider_error(format!("staging dir: {err}")))?;
        let final_path = self
            .staging_dir
            .join(format!("cartesia-{}.wav", crate::storage::new_id()));
        let tmp_path = final_path.with_extension("wav.tmp");
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let file = std::fs::File::create(&tmp_path)
                .map_err(|err| provider_error(format!("staging file: {err}")))?;
            let mut writer = std::io::BufWriter::new(file);
            let mut wav = hound::WavWriter::new(&mut writer, spec)
                .map_err(|err| provider_error(format!("wav writer: {err}")))?;
            for sample in pcm.chunks_exact(2) {
                wav.write_sample(i16::from_le_bytes([sample[0], sample[1]]))
                    .map_err(|err| provider_error(format!("wav write: {err}")))?;
            }
            wav.finalize()
                .map_err(|err| provider_error(format!("wav finalize: {err}")))?;
            writer
                .flush()
                .map_err(|err| provider_error(format!("wav flush: {err}")))?;
        }
        std::fs::rename(&tmp_path, &final_path)
            .map_err(|err| provider_error(format!("atomic rename: {err}")))?;

        Ok(TtsArtifact {
            audio_path: final_path,
            native_words: (!words.is_empty()).then_some(serde_json::Value::Array(words)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    fn sse(events: &[serde_json::Value]) -> String {
        events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect()
    }

    fn chunk_event(pcm: &[u8]) -> serde_json::Value {
        serde_json::json!({
            "type": "chunk",
            "data": base64::engine::general_purpose::STANDARD.encode(pcm),
            "done": false,
            "status_code": 206,
        })
    }

    fn timestamps_event() -> serde_json::Value {
        serde_json::json!({
            "type": "timestamps",
            "done": false,
            "status_code": 206,
            "word_timestamps": {
                "words": ["Hello", "world"],
                "start": [0.0, 0.5],
                "end": [0.4, 0.9],
            }
        })
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
                &[("Content-Type", "text/event-stream"), ("Retry-After", "30")],
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

    fn adapter(base: String, dir: &std::path::Path) -> CartesiaTts {
        CartesiaTts::with_base_url(
            base,
            ApiKey::new("sk_car_fixture0000".to_string()),
            "sonic-3.6".to_string(),
            dir.to_path_buf(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_success_round_trip_yields_a_wav_and_ms_words() {
        let dir = tempfile::tempdir().unwrap();
        let pcm: Vec<u8> = (0..480i16).flat_map(|s| s.to_le_bytes()).collect();
        let body = sse(&[
            chunk_event(&pcm[..480]),
            chunk_event(&pcm[480..]),
            timestamps_event(),
            serde_json::json!({"type": "done", "done": true, "status_code": 200}),
        ]);
        let (base, server) = one_shot("200 OK", body).await;
        let mut tts = adapter(base, dir.path());
        let artifact = tts.synthesize("Hello world", "voice-uuid-1").await.unwrap();

        let reader = hound::WavReader::open(&artifact.audio_path).expect("wav opens");
        assert_eq!(reader.spec().sample_rate, 24_000);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.len(), 480);
        let file_name = artifact
            .audio_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            file_name.ends_with(".wav") && !file_name.contains(".tmp"),
            "the artifact carries its final name, not the staging one: {file_name}"
        );
        let words = artifact.native_words.expect("timestamps arrived");
        assert_eq!(
            words,
            serde_json::json!([
                {"w": "Hello", "s": 0, "e": 400},
                {"w": "world", "s": 500, "e": 900},
            ])
        );

        let captured = server.await.unwrap();
        assert!(captured.contains("POST /tts/sse"));
        assert!(captured.contains("authorization: Bearer sk_car_fixture0000"));
        assert!(captured.contains("cartesia-version: 2026-08-14"));
        let body_start = captured.find("\r\n\r\n").unwrap() + 4;
        let body: serde_json::Value = serde_json::from_str(&captured[body_start..]).unwrap();
        assert_eq!(body["model_id"], "sonic-3.6");
        assert_eq!(body["voice"], "voice-uuid-1");
        assert_eq!(body["transcript"], "Hello world");
        assert_eq!(body["output_format"]["container"], "raw");
        assert_eq!(body["output_format"]["encoding"], "pcm_s16le");
        assert_eq!(body["output_format"]["sample_rate"], 24000);
        assert_eq!(body["add_timestamps"], true);
        assert_eq!(
            body["use_normalized_timestamps"], false,
            "original-text words are what the §3.5 text gate validates against"
        );
    }

    #[tokio::test]
    async fn a_stream_error_event_classifies_by_its_status_code() {
        let dir = tempfile::tempdir().unwrap();
        let body = sse(&[serde_json::json!({
            "type": "error", "done": true, "status_code": 429,
            "title": "rate limited", "message": "slow down", "request_id": "req_9",
        })]);
        let (base, _server) = one_shot("200 OK", body).await;
        let mut tts = adapter(base, dir.path());
        let error = tts.synthesize("Hello", "v").await.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Quota);
        assert!(error.message.contains("req_9"));
    }

    #[tokio::test]
    async fn http_statuses_and_truncated_streams_classify_honestly() {
        let dir = tempfile::tempdir().unwrap();
        for (status, kind) in [
            ("401 Unauthorized", ScriptErrorKind::Auth),
            ("429 Too Many Requests", ScriptErrorKind::Quota),
            ("500 Internal Server Error", ScriptErrorKind::Provider),
        ] {
            let (base, _server) = one_shot(status, "{}".to_string()).await;
            let mut tts = adapter(base, dir.path());
            assert_eq!(
                tts.synthesize("Hello", "v").await.unwrap_err().kind,
                kind,
                "status {status}"
            );
        }
        // A stream that dies before its done event must not yield audio.
        let body = sse(&[chunk_event(&[0u8; 4])]);
        let (base, _server) = one_shot("200 OK", body).await;
        let mut tts = adapter(base, dir.path());
        assert_eq!(
            tts.synthesize("Hello", "v").await.unwrap_err().kind,
            ScriptErrorKind::Malformed
        );
    }

    #[tokio::test]
    async fn quota_preserves_retry_after_and_dead_or_tls_mismatched_hosts_are_transport() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _server) = one_shot("429 Too Many Requests", "{}".to_string()).await;
        let mut tts = adapter(base, dir.path());
        let error = tts.synthesize("Hello", "v").await.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Quota);
        assert_eq!(
            error.retry_after_secs,
            Some(30),
            "Retry-After survives the adapter"
        );

        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = dead.local_addr().unwrap().port();
        drop(dead);
        let mut tts = adapter(format!("http://127.0.0.1:{port}"), dir.path());
        assert_eq!(
            tts.synthesize("Hello", "v").await.unwrap_err().kind,
            ScriptErrorKind::Transport
        );

        // TLS against a plaintext listener: the handshake fails as
        // transport, never as an auth/key verdict (§5.5).
        let plain = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = plain.local_addr().unwrap().port();
        std::mem::forget(plain);
        let mut tts = adapter(format!("https://127.0.0.1:{port}"), dir.path()).with_client(
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(300))
                .build()
                .unwrap(),
        );
        let kind = tts.synthesize("Hello", "v").await.unwrap_err().kind;
        assert!(
            matches!(kind, ScriptErrorKind::Transport | ScriptErrorKind::Timeout),
            "a TLS handshake against a plaintext socket is transport-class, never a key \
             verdict: {kind:?}"
        );
    }

    #[tokio::test]
    async fn a_stalled_response_times_out_as_timeout_not_auth() {
        let dir = tempfile::tempdir().unwrap();
        // A server that accepts and never answers, against a client with a
        // short total timeout: the classified kind is Timeout.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::mem::forget(listener);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap();
        let mut tts = adapter(format!("http://127.0.0.1:{port}"), dir.path()).with_client(client);
        assert_eq!(
            tts.synthesize("Hello", "v").await.unwrap_err().kind,
            ScriptErrorKind::Timeout
        );
    }

    #[test]
    fn the_conformance_record_is_present_and_matches_the_wire_constants() {
        let record: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/providers/tts_conformance.json"
        ))
        .unwrap();
        let cartesia = &record["cartesia_tts"];
        assert_eq!(cartesia["protocol_version"], CARTESIA_VERSION);
        for field in [
            "error_retry",
            "probe_model_list",
            "retention_disclosure",
            "terms",
            "ai_voice_disclosure",
        ] {
            assert!(
                cartesia[field].as_str().is_some_and(|v| !v.is_empty()),
                "conformance record missing {field}"
            );
        }
    }

    #[tokio::test]
    async fn a_cr_framed_terminal_error_event_classifies_by_its_status() {
        // The error event completes only via the EOF-drained held-CR tail;
        // it must classify as Quota (429), never as generic truncation.
        let dir = tempfile::tempdir().unwrap();
        let body = "data: {\"type\":\"error\",\"status_code\":429,\"title\":\"rate limited\",\"message\":\"slow down\",\"request_id\":\"req_cr\"}\r\r".to_string();
        let (base, _server) = one_shot("200 OK", body).await;
        let mut tts = adapter(base, dir.path());
        let error = tts.synthesize("Hello", "v").await.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Quota);
        assert!(error.message.contains("req_cr"));
    }

    #[tokio::test]
    async fn a_done_event_missing_its_blank_line_is_truncation_not_success() {
        let dir = tempfile::tempdir().unwrap();
        let pcm = [0u8; 96];
        let body = format!(
            "data: {}\n\ndata: {{\"type\":\"done\",\"done\":true,\"status_code\":200}}\n",
            chunk_event(&pcm)
        );
        let (base, _server) = one_shot("200 OK", body).await;
        let mut tts = adapter(base, dir.path());
        let error = tts.synthesize("Hello", "v").await.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Malformed);
        assert!(
            error.message.contains("without a done event"),
            "{}",
            error.message
        );
    }

    #[tokio::test]
    async fn timestampless_success_still_yields_audio_with_no_words() {
        let dir = tempfile::tempdir().unwrap();
        let body = sse(&[
            chunk_event(&[0u8; 96]),
            serde_json::json!({"type": "done", "done": true, "status_code": 200}),
        ]);
        let (base, _server) = one_shot("200 OK", body).await;
        let mut tts = adapter(base, dir.path());
        let artifact = tts.synthesize("Hello", "v").await.unwrap();
        assert!(artifact.native_words.is_none(), "no words is not an error");
        assert!(artifact.audio_path.exists());
    }
}
