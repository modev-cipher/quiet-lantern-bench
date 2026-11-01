//! Deepgram streaming-STT adapter (VA5 — BYOK external provider,
//! VOICE_AGENT §3.2 STT slot; this unit is struct + config + capabilities +
//! the connect/close lifecycle, the read-loop transcript decode (`Results`
//! → `TranscriptDelta`/`WordTimestamps`, `speech_final`/`UtteranceEnd` →
//! one deduped `UtteranceEnd`), the binary audio hot path (`send_audio`),
//! the NET-0001 KeepAlive ticker, and the STT-slot posture of the four
//! LLM-facing session methods — `send_text`/`send_tool_result` reject
//! non-retryably, `update_context`/`interrupt` are state-gated no-ops;
//! each decision is documented with its spec grounds at the method).
//!
//! Wire protocol provenance (all accessed 2026-08-21):
//!
//! - Endpoint & auth: `wss://api.deepgram.com/v1/listen`
//!   (https://developers.deepgram.com/reference/speech-to-text-api/listen-streaming),
//!   query params `model`, `encoding` (`linear16` for PCM16), `sample_rate`,
//!   `channels`, `interim_results`, `endpointing`, `utterance_end_ms`,
//!   `smart_format`. The credential rides the `Authorization` request header
//!   on the WS upgrade (same reference; header form `Authorization: Token …`
//!   per the key-probe provenance in `voice::keys::probe`) — connect URLs
//!   stay credential-free (VOICE_AGENT §3.2
//!   `auth-hygiene-no-query-param-creds`).
//!
//! - Framing (https://developers.deepgram.com/docs/lower-level-websockets):
//!   audio is **raw binary WS frames** (PCM16 bytes, no envelope — the
//!   opposite of Gemini's base64-in-JSON, and exactly the adapter detail the
//!   harness never sees, VOICE_AGENT §1); control messages are **JSON text
//!   frames**. The doc warns that sending a control message as a binary/data
//!   frame will "cause the audio processing to choke or hiccup", so the two
//!   frame kinds must never be mixed.
//!
//! - KeepAlive — the NET-0001 timeout class
//!   (https://developers.deepgram.com/docs/keep-alive): "If no audio data or
//!   `KeepAlive` messages are sent within a 10-second window, the connection
//!   will close with a `NET-0001` error." The frame is the text message
//!   `{"type": "KeepAlive"}` — text, not binary ("sending it as binary may
//!   result in incorrect handling") — sent every 3–5 s while the mic is not
//!   flowing; the server never acknowledges it. This is the
//!   `deepgram-keepalive` (NET-0001) entry of the VA5 conformance-fixture
//!   roster (VOICE_AGENT §3.2).
//!
//! - Finalize (https://developers.deepgram.com/docs/finalize): text frame
//!   `{"type": "Finalize"}` (optional zero-indexed `"channel"`) forces the
//!   server to flush buffered audio and promote interim results to final;
//!   the flushed result may carry `"from_finalize": true`, but that
//!   confirmation is not guaranteed when little audio is buffered.
//!
//! - CloseStream (https://developers.deepgram.com/docs/close-stream): text
//!   frame `{"type": "CloseStream"}`; the server finishes processing cached
//!   audio, sends the remaining results plus a summary `{"type": "Metadata"}`
//!   object, then terminates the connection — the clean-`close` path.
//!
//! - Results envelope
//!   (https://developers.deepgram.com/docs/lower-level-websockets): inbound
//!   `{"type": "Results"}` messages carry `channel_index`, `start`,
//!   `duration`, `is_final`, `speech_final`, and
//!   `channel.alternatives[]` with `transcript`, `confidence`, and `words[]`
//!   — each word a `{word, start, end, confidence}` record, i.e. real
//!   word-aligned timing for `TimedWord`/follow-along (VOICE_AGENT §11).
//!
//! - UtteranceEnd (https://developers.deepgram.com/docs/utterance-end):
//!   enabled by `utterance_end_ms` (requires `interim_results=true`; minimum
//!   1000 ms), emitted as `{"type": "UtteranceEnd", "channel": [A, B],
//!   "last_word_end": …}` when a word-timing gap of the configured length
//!   follows the last finalized word. `last_word_end: -1` means the result
//!   was already finalized before the gap condition was met and the message
//!   should be ignored. `speech_final: true` (silence endpointing) and
//!   `UtteranceEnd` (word-gap analysis) both signal end-of-speech, so the
//!   adapter dedupes them into one end-of-utterance signal — resolved in
//!   [`read_loop`] (rule documented there), pinned by the
//!   `endpoint-race-dedupe` fixture of the VA5 roster (VOICE_AGENT §3.2).
//!
//! Capability posture: Deepgram owns end-of-utterance server-side
//! (`speech_final`/`UtteranceEnd` — the VOICE_AGENT §3.2 slot rule names
//! exactly this pair as authoritative endpointing), so `vad_mode` is
//! `Server` and a client VAD is at most a cost optimizer (§3.1 double-VAD
//! rule); `words[]` carries per-word `start`/`end`, so `word_timestamps` is
//! `true` — no §11 sentence-level degrade on this channel.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::voice::audio::generation::Generation;
use crate::voice::audio::PROVIDER_INPUT_RATE;
use crate::voice::keys::KeyManager;

use super::composed::{SttSlot, SttSlotConfig};
use super::{
    ChannelCapabilities, ChannelError, ChannelEvent, ContextUpdate, DelimitedExcerpt, ErrorKind,
    LifecycleEvent, Role, SessionChannel, SessionContext, TimedWord, ToolResult, VadMode,
};

/// Default listen endpoint (see module header provenance);
/// [`DeepgramConfig::endpoint_override`] replaces it for proxies and
/// recorded-fixture servers.
pub const DEFAULT_LISTEN_ENDPOINT: &str = "wss://api.deepgram.com/v1/listen";

/// The canonical base URL the user's Deepgram key is stored under
/// (`Provider::Deepgram.default_base_url()`); the key lookup is by this
/// identity even when [`DeepgramConfig::endpoint_override`] points the
/// socket elsewhere — the same split gemini.rs uses.
const DEEPGRAM_KEY_BASE_URL: &str = "https://api.deepgram.com";

const EVENT_CAPACITY: usize = 4096;

/// The graceful-shutdown control frame (module header, close-stream
/// provenance): a JSON **text** frame — the doc warns control messages sent
/// as binary choke the audio pipeline — after which the server flushes cached
/// audio, sends the remaining results plus a summary `Metadata`, and
/// terminates the connection.
const CLOSE_STREAM_FRAME: &str = r#"{"type":"CloseStream"}"#;

/// How long `close` waits for the server's post-`CloseStream` flush to end in
/// its connection termination before giving up on the drain.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

/// The NET-0001 heartbeat (module header, keep-alive provenance): a JSON
/// **text** frame — the doc warns that sending it as binary "may result in
/// incorrect handling" — that the server never acknowledges.
const KEEPALIVE_FRAME: &str = r#"{"type":"KeepAlive"}"#;

/// Idle gap after this side's last frame before the KeepAlive ticker fires:
/// 5 s, the top of the "every 3-5 seconds" cadence the doc prescribes against
/// the 10-second NET-0001 close window
/// (https://developers.deepgram.com/docs/keep-alive, accessed 2026-08-21) —
/// half the window, so even a whole missed beat cannot reach the timeout.
const KEEPALIVE_IDLE_INTERVAL: Duration = Duration::from_secs(5);

/// Adapter configuration. Credentials are deliberately absent: the key is
/// resolved at connect time through the VA3 [`KeyManager`] (VOICE_AGENT §7) —
/// no key bytes live outside `voice::keys::ApiKey`.
#[derive(Clone, Debug)]
pub struct DeepgramConfig {
    /// Full WS endpoint replacing [`DEFAULT_LISTEN_ENDPOINT`] when set.
    pub endpoint_override: Option<String>,
    /// Streaming model name for the `model` query param, e.g. `nova-3`
    /// (VOICE_AGENT §3.2: Flux vs Nova is a query-param change only,
    /// evaluated within the slot at implementation time).
    pub model: String,
}

/// Session state, the uniform contract the trait documents: connect is valid
/// exactly once from `New`; a closed session never reopens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    New,
    Connected,
    Closed,
}

type ListenSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type ListenWriter = SplitSink<ListenSocket, Message>;

/// The Deepgram streaming-STT session channel — connect/close lifecycle and
/// the read-loop shell; audio, transcript decode, and KeepAlive land in later
/// VA5 strides.
pub struct DeepgramChannel {
    config: DeepgramConfig,
    keys: Arc<KeyManager>,
    /// Snapshotted into the `gen` tag of user-speech `WordTimestamps` events
    /// (the read loop clones it at connect); the barge-in stride adds the
    /// bump-side use.
    generation: Generation,
    state: SessionState,
    intentional_close: Arc<std::sync::atomic::AtomicBool>,
    /// Write half of the split socket, live while `Connected`. The same
    /// shared-slot shape as gemini.rs; the KeepAlive ticker writes off the
    /// session task through this slot, and finding it empty is the ticker's
    /// stop signal.
    writer: Arc<tokio::sync::Mutex<Option<ListenWriter>>>,
    /// The inbound decode loop; joined (budgeted) at `close`.
    reader: Option<JoinHandle<()>>,
    /// When this side last put a frame on the wire (audio or KeepAlive); the
    /// KeepAlive ticker sleeps against this, so every `send_audio` resets the
    /// idle countdown for free.
    last_send: Arc<std::sync::Mutex<tokio::time::Instant>>,
    /// The NET-0001 ticker (module header, keep-alive provenance), spawned at
    /// connect and aborted at `close`.
    keepalive: Option<JoinHandle<()>>,
    /// [`KEEPALIVE_IDLE_INTERVAL`] in production; tests shrink it so the idle
    /// threshold is crossable inside a test budget.
    keepalive_interval: Duration,
    /// The event pair exists from construction: take_events must yield the
    /// receiver pre-connect (conformance check 1, matching MockChannel's
    /// build-time pair); connect and the read loop only clone this sender.
    events_tx: Option<mpsc::Sender<ChannelEvent>>,
    events: Option<mpsc::Receiver<ChannelEvent>>,
}

impl DeepgramChannel {
    pub fn new(config: DeepgramConfig, keys: Arc<KeyManager>, generation: Generation) -> Self {
        let (events_tx, events) = mpsc::channel(EVENT_CAPACITY);
        Self {
            config,
            keys,
            generation,
            state: SessionState::New,
            intentional_close: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            writer: Arc::new(tokio::sync::Mutex::new(None)),
            reader: None,
            last_send: Arc::new(std::sync::Mutex::new(tokio::time::Instant::now())),
            keepalive: None,
            keepalive_interval: KEEPALIVE_IDLE_INTERVAL,
            events_tx: Some(events_tx),
            events: Some(events),
        }
    }

    /// Shrinks the KeepAlive idle threshold so a test can cross it in real
    /// time; must be called before `connect` (the ticker snapshots it there).
    #[cfg(test)]
    fn set_keepalive_interval(&mut self, interval: Duration) {
        self.keepalive_interval = interval;
    }

    /// The URL the session dials: the override when present (else
    /// [`DEFAULT_LISTEN_ENDPOINT`]) plus the session query params — the wire
    /// has no setup message, so query params ARE the session config (module
    /// header, listen-streaming provenance): `model` from the config;
    /// `encoding=linear16` because the pipeline speaks raw PCM16; and
    /// `sample_rate` pinned to [`PROVIDER_INPUT_RATE`], the 16 kHz mono the
    /// VA1 up-resampler feeds `send_audio`. The credential never rides here
    /// (VOICE_AGENT §3.2 `auth-hygiene-no-query-param-creds`).
    fn listen_url(&self) -> String {
        let endpoint = self
            .config
            .endpoint_override
            .as_deref()
            .unwrap_or(DEFAULT_LISTEN_ENDPOINT);
        let separator = if endpoint.contains('?') { '&' } else { '?' };
        format!(
            "{endpoint}{separator}model={}&encoding=linear16&sample_rate={PROVIDER_INPUT_RATE}",
            self.config.model
        )
    }
}

/// Decodes a `Results` alternative's `words[]` — `{word, start, end,
/// confidence}` records with seconds-float timing (module header, results
/// provenance) — into the event vocabulary's millisecond [`TimedWord`]s.
/// Records missing a field are skipped, not errors: a word the UI cannot
/// place is worth less than the words it can.
fn decode_words(alternative: Option<&serde_json::Value>) -> Vec<TimedWord> {
    let Some(words) = alternative
        .and_then(|alt| alt.get("words"))
        .and_then(|words| words.as_array())
    else {
        return Vec::new();
    };
    words
        .iter()
        .filter_map(|word| {
            Some(TimedWord {
                w: word.get("word")?.as_str()?.to_string(),
                start_ms: (word.get("start")?.as_f64()? * 1000.0).round() as u32,
                end_ms: (word.get("end")?.as_f64()? * 1000.0).round() as u32,
            })
        })
        .collect()
}

/// Inbound loop for the life of the session, decoding server messages into the
/// uniform event stream. Deepgram's server-to-client messages are JSON text
/// frames only (module header, framing provenance).
///
/// - `Results` (module header, results provenance): the transcript is
///   `channel.alternatives[0].transcript`, always the **user's** speech —
///   Deepgram is STT-only, nothing on this wire narrates an assistant.
///   `is_final: false` frames are interim deltas, `is_final: true` frames
///   final ones; both surface as `TranscriptDelta { role: User }`.
///   Empty-transcript frames are skipped per the doc. Final frames also carry
///   settled `words[]` timing, emitted as `WordTimestamps` (the
///   `word_timestamps: true` capability); interim word lists still fluctuate
///   and are not emitted.
///
/// - End-of-utterance dedupe (the module header's `endpoint-race-dedupe`
///   note, resolved here). The rule: **exactly one `UtteranceEnd` event per
///   utterance — `speech_final` wins when both wire signals fire.**
///   `speech_final: true` on a `Results` frame (silence endpointing) is the
///   primary signal and closes the utterance immediately; the wire's
///   `UtteranceEnd` message (word-gap analysis) can also fire after a
///   `speech_final` already closed the same utterance, so it emits only
///   while an utterance is still open — i.e. speech arrived since the last
///   close. A wire `UtteranceEnd` with `last_word_end: -1` is ignored
///   unconditionally: the doc's already-finalized sentinel
///   (https://developers.deepgram.com/docs/utterance-end, accessed
///   2026-08-21).
async fn read_loop(
    mut reader: SplitStream<ListenSocket>,
    events: mpsc::Sender<ChannelEvent>,
    generation: Generation,
    intentional_close: Arc<std::sync::atomic::AtomicBool>,
) {
    // True while the current utterance carries speech that no end-of-utterance
    // signal has committed yet — the whole dedupe state.
    let mut utterance_open = false;
    while let Some(Ok(message)) = reader.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        match value.get("type").and_then(|kind| kind.as_str()) {
            // The first server frame may be a `Metadata` envelope, and the
            // CloseStream flush ends with a summary one: tolerated, nothing
            // downstream consumes it.
            Some("Metadata") => continue,
            Some("Results") => {
                let alternative = value
                    .get("channel")
                    .and_then(|channel| channel.get("alternatives"))
                    .and_then(|alternatives| alternatives.get(0));
                let transcript = alternative
                    .and_then(|alt| alt.get("transcript"))
                    .and_then(|text| text.as_str())
                    .unwrap_or("");
                let is_final = value
                    .get("is_final")
                    .and_then(|flag| flag.as_bool())
                    .unwrap_or(false);
                let speech_final = value
                    .get("speech_final")
                    .and_then(|flag| flag.as_bool())
                    .unwrap_or(false);
                if !transcript.is_empty() {
                    utterance_open = true;
                    if events
                        .send(ChannelEvent::TranscriptDelta {
                            role: Role::User,
                            text: transcript.to_string(),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if is_final {
                        let words = decode_words(alternative);
                        if !words.is_empty()
                            && events
                                .send(ChannelEvent::WordTimestamps {
                                    words,
                                    // User speech is not response-scoped;
                                    // tagging the live generation keeps the
                                    // §2 gates treating it as current.
                                    gen: generation.current(),
                                })
                                .await
                                .is_err()
                        {
                            return;
                        }
                    }
                }
                // The endpointing signal is honored even on an
                // empty-transcript frame: the words may have arrived in an
                // earlier final, with only trailing silence left here.
                if speech_final && utterance_open {
                    utterance_open = false;
                    if events.send(ChannelEvent::UtteranceEnd).await.is_err() {
                        return;
                    }
                }
            }
            Some("UtteranceEnd") => {
                if value.get("last_word_end").and_then(|end| end.as_f64()) == Some(-1.0) {
                    continue;
                }
                if utterance_open {
                    utterance_open = false;
                    if events.send(ChannelEvent::UtteranceEnd).await.is_err() {
                        return;
                    }
                }
            }
            _ => continue,
        }
    }
    if !intentional_close.load(std::sync::atomic::Ordering::SeqCst) {
        let _ = events
            .send(ChannelEvent::Lifecycle(LifecycleEvent::Expired))
            .await;
    }
}

/// The NET-0001 ticker (module header, keep-alive provenance): whenever
/// `interval` passes with nothing sent, put the [`KEEPALIVE_FRAME`] text frame
/// on the wire. Sleeps against the shared `last_send` mark — every
/// `send_audio` pushes the deadline out, so the frame only rides an idle wire.
/// Stops when the writer slot is empty (`close` took it) or the send fails
/// (the socket ended); `close` also aborts it outright.
async fn keepalive_loop(
    writer: Arc<tokio::sync::Mutex<Option<ListenWriter>>>,
    last_send: Arc<std::sync::Mutex<tokio::time::Instant>>,
    interval: Duration,
) {
    loop {
        let deadline = *last_send.lock().unwrap() + interval;
        if tokio::time::Instant::now() < deadline {
            // Not a busy loop: this parks until the deadline that was current
            // when we looked; audio moving it further just means one more
            // park on wake.
            tokio::time::sleep_until(deadline).await;
            continue;
        }
        let mut guard = writer.lock().await;
        let Some(writer) = guard.as_mut() else {
            return;
        };
        if writer.send(Message::text(KEEPALIVE_FRAME)).await.is_err() {
            return;
        }
        *last_send.lock().unwrap() = tokio::time::Instant::now();
    }
}

#[async_trait]
impl SessionChannel for DeepgramChannel {
    // The STT wire takes no session context: there is no setup message to
    // carry it, and the prompt-side context belongs to the LLM slot
    // (VOICE_AGENT §3.2).
    async fn connect(&mut self, _ctx: SessionContext) -> Result<(), ChannelError> {
        if self.state != SessionState::New {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("connect on a channel in state {:?}", self.state),
            ));
        }

        let key = self
            .keys
            .session_key(DEEPGRAM_KEY_BASE_URL)
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Auth,
                    false,
                    format!("no usable Deepgram key: {err}"),
                )
            })?;
        let mut request = self.listen_url().into_client_request().map_err(|err| {
            ChannelError::new(ErrorKind::Protocol, false, format!("bad endpoint: {err}"))
        })?;
        // Header auth only, in the doc's scheme — `Authorization: Token <key>`
        // (module header provenance, matching `voice::keys::probe`): the
        // connect URL stays credential-free (VOICE_AGENT §3.2
        // `auth-hygiene-no-query-param-creds`) and the sensitive flag keeps
        // the value out of any Debug rendering.
        let mut auth = HeaderValue::from_str(&format!("Token {}", key.expose())).map_err(|_| {
            ChannelError::new(
                ErrorKind::Auth,
                false,
                "the stored key contains characters no header can carry",
            )
        })?;
        auth.set_sensitive(true);
        request.headers_mut().insert("authorization", auth);

        let (socket, _response) = connect_async(request).await.map_err(|err| {
            ChannelError::new(
                ErrorKind::Transport,
                true,
                format!("listen ws connect failed: {err}"),
            )
        })?;

        // No setup/setupComplete exchange on this wire (unlike Gemini): the
        // session is configured entirely by the connect query params, so the
        // open connection IS ready.
        let tx = self
            .events_tx
            .clone()
            .expect("a New channel still holds its construction-time sender");
        let _ = tx.try_send(ChannelEvent::Lifecycle(LifecycleEvent::Connected));
        self.intentional_close
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let (writer, reader) = socket.split();
        self.reader = Some(tokio::spawn(read_loop(
            reader,
            tx,
            self.generation.clone(),
            Arc::clone(&self.intentional_close),
        )));
        *self.writer.lock().await = Some(writer);
        // The 10-second NET-0001 window opens the moment the socket does
        // (module header, keep-alive provenance), so the idle mark and the
        // ticker start now — not at the first audio frame.
        *self.last_send.lock().unwrap() = tokio::time::Instant::now();
        self.keepalive = Some(tokio::spawn(keepalive_loop(
            Arc::clone(&self.writer),
            Arc::clone(&self.last_send),
            self.keepalive_interval,
        )));
        self.state = SessionState::Connected;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), ChannelError> {
        if self.state == SessionState::Closed {
            return Ok(());
        }
        self.state = SessionState::Closed;
        self.intentional_close
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.events_tx = None;
        // The ticker dies before the writer leaves the slot so no KeepAlive
        // can race in after the CloseStream below.
        if let Some(keepalive) = self.keepalive.take() {
            keepalive.abort();
        }
        // The doc's graceful shutdown (module header, close-stream
        // provenance): send the `CloseStream` text frame, let the server
        // flush cached audio — remaining results plus the summary `Metadata`
        // — and terminate the connection (the read loop exiting is that
        // termination), then complete the WS close handshake from our side.
        // A peer that already vanished is not a close failure.
        let mut writer = self.writer.lock().await.take();
        if let Some(writer) = writer.as_mut() {
            let _ = writer.send(Message::text(CLOSE_STREAM_FRAME)).await;
        }
        if let Some(mut reader) = self.reader.take() {
            if timeout(SHUTDOWN_BUDGET, &mut reader).await.is_err() {
                reader.abort();
            }
        }
        if let Some(mut writer) = writer {
            let _ = writer.close().await;
        }
        Ok(())
    }

    async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
        // The whole gate: this wire has no renewal, so unlike gemini.rs there
        // is no phase between Connected and gone — New and Closed reject with
        // the trait's InvalidState (MockChannel's send-on-closed semantics),
        // and nothing is ever retryable-buffered.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_audio on a channel in state {:?}", self.state),
            ));
        }
        // The hot path (module header, framing provenance): one raw binary WS
        // frame per chunk — `encoding=linear16` is little-endian signed PCM16
        // bytes, no envelope, and mixing in a text/JSON wrapper is exactly
        // what the doc warns chokes the audio processing.
        let mut bytes = Vec::with_capacity(pcm_16k_mono.len() * 2);
        for sample in pcm_16k_mono {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let mut writer = self.writer.lock().await;
        writer
            .as_mut()
            .expect("Connected implies a live write half")
            .send(Message::binary(bytes))
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("listen ws dropped sending audio: {err}"),
                )
            })?;
        *self.last_send.lock().unwrap() = tokio::time::Instant::now();
        Ok(())
    }

    async fn send_text(&mut self, _text: &str) -> Result<(), ChannelError> {
        // The trait's uniform state contract first, like every other method.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_text on a channel in state {:?}", self.state),
            ));
        }
        // Rejected on a live session too: the listen wire has no text-turn
        // input — its client frames are binary audio plus the KeepAlive/
        // Finalize/CloseStream control set (module header, framing
        // provenance). A text turn is LLM-slot business: the STT slot's whole
        // contract is "stream PCM in → transcript deltas + end-of-utterance"
        // (VOICE_AGENT §3.2), and `CompatComposedChannel` routes `send_text`
        // to its LLM slot, never here. Failing loudly (not silently
        // swallowing) keeps the §3 no-mic fallback honest: a typed user turn
        // that can never produce a response must not vanish. Non-retryable —
        // the wire will not grow the capability within this session.
        // `InvalidState` is the §3 error vocabulary's fit for "this channel
        // can never answer this call" (the same stretch gemini.rs applies to
        // a tool result for an unknown call).
        Err(ChannelError::new(
            ErrorKind::InvalidState,
            false,
            "send_text on the STT-only Deepgram listen wire: text turns belong \
             to the LLM slot (VOICE_AGENT §3.2)",
        ))
    }

    async fn send_tool_result(
        &mut self,
        call_id: &str,
        _result: ToolResult,
    ) -> Result<(), ChannelError> {
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_tool_result on a channel in state {:?}", self.state),
            ));
        }
        // The listen wire never surfaces a `ToolCall` — the read loop's whole
        // inbound vocabulary is `Results`/`UtteranceEnd`/`Metadata`; tools
        // are LLM-slot business (VOICE_AGENT §5, §3.2). So every `call_id`
        // here answers a call that cannot exist, and the answer is the same
        // rejection gemini.rs gives a result for an unknown or retired call:
        // `InvalidState`, non-retryable.
        Err(ChannelError::new(
            ErrorKind::InvalidState,
            false,
            format!(
                "tool result for {call_id:?} on the STT-only Deepgram listen \
                 wire: no ToolCall is ever emitted here (tools are LLM-slot \
                 business, VOICE_AGENT §3.2, §5)"
            ),
        ))
    }

    async fn update_context(&mut self, _update: ContextUpdate) -> Result<(), ChannelError> {
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("update_context on a channel in state {:?}", self.state),
            ));
        }
        // Accepted and discarded, exactly like the `SessionContext` at
        // `connect`: STT consumes no prompt context — both §4 variants
        // (chapter-boundary instruction refresh, playback position) address
        // the reasoning prompt, which lives in the LLM slot (VOICE_AGENT
        // §3.2, §4). Ok rather than Err because `ChannelError` kinds are the
        // failure classes the harness's §9 degrade ladder reacts to
        // (types.rs), and a healthy STT session handed a context update has
        // failed in no §9 sense — rejecting would push a working session
        // into error handling over nothing.
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<(), ChannelError> {
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("interrupt on a channel in state {:?}", self.state),
            ));
        }
        // The trait names `interrupt` the network half of barge-in: cancel
        // the in-flight response (VOICE_AGENT §2 rule 2). This channel never
        // has one — it emits no `AudioDelta`; the response being barged in on
        // lives in the LLM/TTS slots — so the network half is trivially
        // complete: Ok, no wire frame. Deliberately no local generation bump
        // either, unlike gemini.rs (whose bump substitutes for a missing wire
        // cancel of its own audio stream): the bump-then-cancel order is
        // owned by [`super::barge_in`]/the harness, and this channel has no
        // superseded stream of its own to kill. Composition note: during
        // barge-in the STT slot must keep transcribing — the user's
        // interrupting speech is exactly what it exists to hear — so
        // "succeed, change nothing, stay listening" is the correct slot
        // behavior, not a shortcut.
        Ok(())
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
        self.events.take()
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            // Deepgram owns end-of-utterance server-side: `speech_final`
            // (silence endpointing) and `UtteranceEnd` (word-gap analysis) —
            // the pair VOICE_AGENT §3.2 names as authoritative endpointing
            // (docs cited in the module header, accessed 2026-08-21); client
            // VAD is at most a cost optimizer (§3.1 double-VAD rule).
            vad_mode: VadMode::Server,
            // Results carry `words[]` with per-word `start`/`end`
            // (https://developers.deepgram.com/docs/lower-level-websockets,
            // accessed 2026-08-21): real word-aligned timing, no §11
            // sentence-level degrade.
            word_timestamps: true,
            // /v1/listen has no end-of-session announcement in its server
            // vocabulary (module header, accessed 2026-08-21): nothing on
            // this wire can surface `Renewing`.
            session_renewal: false,
            text_only: false,
            stt_only: true,
        }
    }
}

/// The connect-time [`SessionContext`] the slot adapter hands the underlying
/// channel: empty by construction. [`SttSlot::connect`] deliberately takes no
/// context — everything a `SessionContext` carries is prompt-side LLM traffic
/// the STT role never sees (VOICE_AGENT §3.2) — and the listen wire discards
/// its connect context anyway (documented at this channel's
/// [`SessionChannel::connect`]).
fn stt_role_context() -> SessionContext {
    SessionContext {
        book_id: None,
        session_id: String::new(),
        system_prompt: String::new(),
        excerpt: DelimitedExcerpt {
            delimited_text: String::new(),
        },
        rolling_summary: None,
        playback_pos_ms: 0,
        tool_schemas: Vec::new(),
    }
}

/// The launch [`SttSlot`] (VOICE_AGENT §3.2 slot table: "STT — Deepgram Nova
/// (WS streaming)"): [`DeepgramChannel`] narrowed to the STT role. The
/// channel already IS that role — its wire carries PCM up and user transcript
/// deltas plus one deduped end-of-utterance down, and its four LLM-facing
/// session methods are rejections/no-ops (each documented at the method) —
/// so the adapter holds no state of its own and every slot method delegates
/// to the channel method implementing that part of the role.
pub struct DeepgramSttSlot {
    channel: DeepgramChannel,
}

impl DeepgramSttSlot {
    /// Builds the slot from the composed channel's credential-free STT config
    /// ([`SttSlotConfig`] → [`DeepgramConfig`] is field-for-field; the key is
    /// resolved at connect through the [`KeyManager`] under the Deepgram
    /// base-URL identity, VOICE_AGENT §3.2/§7). No slot registry exists yet —
    /// the composed-channel skeleton created none — so until a registry
    /// stride lands, this constructor is the §3.2 "one registry entry" for
    /// the launch STT slot.
    pub fn new(config: SttSlotConfig, keys: Arc<KeyManager>, generation: Generation) -> Self {
        Self {
            channel: DeepgramChannel::new(
                DeepgramConfig {
                    endpoint_override: config.endpoint_override,
                    model: config.model,
                },
                keys,
                generation,
            ),
        }
    }
}

#[async_trait]
impl SttSlot for DeepgramSttSlot {
    fn owns_endpointing(&self) -> bool {
        // The §3.2 turn-detection rule names this wire's
        // `speech_final`/`UtteranceEnd` pair as authoritative endpointing —
        // the same grounds as the channel's `VadMode::Server` capability.
        true
    }

    async fn connect(&mut self) -> Result<(), ChannelError> {
        self.channel.connect(stt_role_context()).await
    }

    async fn close(&mut self) -> Result<(), ChannelError> {
        self.channel.close().await
    }

    async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
        self.channel.send_audio(pcm_16k_mono).await
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
        // Handed through unfiltered: the listen wire's read loop emits only
        // the slot vocabulary — `TranscriptDelta { role: User }`,
        // `WordTimestamps`, one deduped `UtteranceEnd`, `Lifecycle` (see
        // [`read_loop`]) — so the trait's narrowed-vocabulary bound holds
        // structurally. Nothing LLM-ish is reachable on this wire; there is
        // no runtime filter to maintain or get wrong.
        self.channel.take_events()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;
    use crate::voice::channel::conformance::{run_stt, ConformanceFixture, ProviderSim};
    use crate::voice::channel::fixture::{load_fixture, replay, WireFrame, WireHandle, WireServer};
    use crate::voice::channel::DelimitedExcerpt;
    use crate::voice::keys::store::{Backend, KeyStore};
    use crate::voice::keys::ApiKey;

    /// Deliberately echoed in the fixture's `absent` needles: no frame the
    /// adapter sends — and no connect URL — may carry the key.
    const FIXTURE_KEY: &str = "fixture-deepgram-key-9c41";

    fn keys() -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        Arc::new(KeyManager::with_store(store))
    }

    fn keys_with_deepgram_key() -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(DEEPGRAM_KEY_BASE_URL, &ApiKey::new(FIXTURE_KEY.into()))
            .unwrap();
        Arc::new(KeyManager::with_store(store))
    }

    fn ctx() -> SessionContext {
        SessionContext {
            book_id: Some("book-1".into()),
            session_id: "session-1".into(),
            system_prompt: "You are the reading tutor.".into(),
            excerpt: DelimitedExcerpt {
                delimited_text: "<<x>>excerpt<</x>>".into(),
            },
            rolling_summary: None,
            playback_pos_ms: 0,
            tool_schemas: Vec::new(),
        }
    }

    #[tokio::test]
    async fn deepgram_connect_close_lifecycle_fixture_replays_green() {
        let handle = WireServer::start().await;
        let mut channel = DeepgramChannel::new(
            DeepgramConfig {
                endpoint_override: Some(handle.url("/v1/listen")),
                model: "nova-3".into(),
            },
            keys_with_deepgram_key(),
            Generation::new(),
        );

        let fixture = load_fixture("deepgram-connect-close");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");

        // The fixture pins the header's presence and the key's absence from
        // the URL; this pins the exact scheme the doc names (module header,
        // listen-streaming provenance): `Authorization: Token <key>`.
        let record = handle.wait_for_connection(1).await;
        let expected_auth = format!("Token {FIXTURE_KEY}");
        assert_eq!(
            record.header("authorization"),
            Some(expected_auth.as_str()),
            "the key rides the Authorization header in the doc's Token scheme"
        );

        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "no setup exchange on this wire: the open connection is ready"
        );
        assert!(
            channel.take_events().is_none(),
            "take_events yields the receiver exactly once"
        );

        // close() returns only after the fixture, having seen CloseStream
        // (the expect step before its server_close), terminated the
        // connection — the doc's graceful-shutdown order.
        channel.close().await.expect("clean close");
        replayer.await.expect("the recorded fixture replays green");
        channel
            .close()
            .await
            .expect("closing a closed channel is a no-op");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned: \
             CloseStream, then nothing"
        );
    }

    /// The `endpoint-race-dedupe` roster entry (VOICE_AGENT §3.2): the
    /// scripted wire fires both end-of-speech signals for the first utterance
    /// (`speech_final: true`, then a duplicate `UtteranceEnd`) and only the
    /// word-gap `UtteranceEnd` for the second — the event stream must carry
    /// exactly one `UtteranceEnd` per utterance, payload-pinned, with the
    /// interim/final transcript deltas and the final's word timing around it.
    #[tokio::test]
    async fn deepgram_results_decode_emits_one_end_of_utterance_per_utterance() {
        let handle = WireServer::start().await;
        let mut channel = DeepgramChannel::new(
            DeepgramConfig {
                endpoint_override: Some(handle.url("/v1/listen")),
                model: "nova-3".into(),
            },
            keys_with_deepgram_key(),
            Generation::new(),
        );

        let fixture = load_fixture("deepgram-endpoint-race-dedupe");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::User,
                text: "what does".into(),
            }),
            "an interim Results frame (is_final: false) surfaces as a user delta"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::User,
                text: "what does figure three show".into(),
            }),
            "the final Results frame surfaces the settled transcript"
        );
        let timed = |w: &str, start_ms: u32, end_ms: u32| TimedWord {
            w: w.into(),
            start_ms,
            end_ms,
        };
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::WordTimestamps {
                words: vec![
                    timed("what", 80, 240),
                    timed("does", 240, 460),
                    timed("figure", 520, 900),
                    timed("three", 900, 1280),
                    timed("show", 1440, 1860),
                ],
                gen: 0,
            }),
            "the final's words[] decode to millisecond TimedWords (the \
             word_timestamps capability); interim word lists are not emitted"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::UtteranceEnd),
            "speech_final: true is the primary end-of-utterance signal"
        );

        // The wire's duplicate UtteranceEnd and the empty-transcript interim
        // both arrive next and must produce nothing: the next event is the
        // second utterance's interim delta.
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::User,
                text: "and the axes".into(),
            }),
            "a wire UtteranceEnd after speech_final already closed the \
             utterance is suppressed, and empty-transcript frames are skipped"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::UtteranceEnd),
            "with no speech_final closing the second utterance, the word-gap \
             UtteranceEnd is the one that commits it"
        );

        channel.close().await.expect("clean close");
        replayer.await.expect("the recorded fixture replays green");
        assert_eq!(
            events.recv().await,
            None,
            "the stream ends with exactly the events above: two utterances, \
             two UtteranceEnds"
        );
    }

    /// The audio hot path plus the `deepgram-keepalive` (NET-0001) roster
    /// entry (VOICE_AGENT §3.2), one recorded script: the mic chunk must reach
    /// the wire as exactly its little-endian PCM16 bytes in one raw binary
    /// frame (payload-pinned — no JSON wrapper, no base64), and once the wire
    /// idles past the keepalive threshold the `{"type":"KeepAlive"}` text
    /// frame must follow before anything else, then the CloseStream shutdown.
    #[tokio::test]
    async fn deepgram_audio_rides_raw_binary_frames_and_idle_sends_keepalive() {
        let handle = WireServer::start().await;
        let mut channel = DeepgramChannel::new(
            DeepgramConfig {
                endpoint_override: Some(handle.url("/v1/listen")),
                model: "nova-3".into(),
            },
            keys_with_deepgram_key(),
            Generation::new(),
        );
        // Real-time test: 200 ms crosses the idle threshold quickly while
        // staying far above scheduler jitter (the production 5 s would sail
        // past the fixture step budget).
        channel.set_keepalive_interval(Duration::from_millis(200));

        let fixture = load_fixture("deepgram-audio-keepalive");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");

        // The fixture's pinned payload: these samples, little-endian —
        // e8 03 | 18 fc | 01 01 | fe ff | 00 00 | ff 7f | 00 80 — including
        // both i16 extremes so a byte-order slip cannot pass.
        let mic_chunk: [i16; 7] = [1000, -1000, 257, -2, 0, i16::MAX, i16::MIN];
        channel.send_audio(&mic_chunk).await.expect("audio sends");

        // Hold the session open (no explicit sleep) until the idle ticker
        // demonstrably fired: the server's per-connection record keeps every
        // frame even after the replayer consumed it from the queue.
        timeout(Duration::from_secs(5), async {
            loop {
                let seen = handle.received_by_connection()[0]
                    .iter()
                    .any(|frame| matches!(frame, WireFrame::Text(t) if t.contains("KeepAlive")));
                if seen {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the idle wire produces a KeepAlive within the test budget");

        channel.close().await.expect("clean close");
        replayer.await.expect("the recorded fixture replays green");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the session sent exactly the pinned frames: audio, KeepAlive, \
             CloseStream, nothing else"
        );

        let rejected = channel.send_audio(&mic_chunk).await.unwrap_err();
        assert_eq!(
            rejected.kind,
            ErrorKind::InvalidState,
            "send-on-closed is InvalidState (MockChannel semantics)"
        );
        assert!(
            !rejected.retryable,
            "a closed session is not coming back: nothing to retry against"
        );
    }

    /// The STT-slot posture of the four LLM-facing session methods, one
    /// behavior pin each (decision + spec grounds documented at the methods):
    /// text and tool-result reject non-retryably, context updates and
    /// interrupt are accepted no-ops — and none of the four puts a frame on
    /// the wire, proven by the fixture's frame-exactness (connect, then
    /// CloseStream, nothing else).
    #[tokio::test]
    async fn deepgram_llm_facing_methods_follow_the_stt_slot_contract() {
        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = DeepgramChannel::new(
            DeepgramConfig {
                endpoint_override: Some(handle.url("/v1/listen")),
                model: "nova-3".into(),
            },
            keys_with_deepgram_key(),
            generation.clone(),
        );

        let fixture = load_fixture("deepgram-connect-close");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");

        let text = channel.send_text("typed question").await.unwrap_err();
        assert_eq!(
            (text.kind, text.retryable),
            (ErrorKind::InvalidState, false),
            "no text-turn input exists on the listen wire: the §3 no-mic \
             fallback must fail loudly here, not vanish"
        );

        let tool = channel
            .send_tool_result("call-1", ToolResult::Ok(serde_json::json!({})))
            .await
            .unwrap_err();
        assert_eq!(
            (tool.kind, tool.retryable),
            (ErrorKind::InvalidState, false),
            "the listen wire never emits a ToolCall, so every call_id is an \
             unknown call (the gemini.rs retired-id rejection)"
        );

        channel
            .update_context(ContextUpdate::PlaybackPosition { pos_ms: 41_000 })
            .await
            .expect(
                "STT consumes no prompt context: accepted and discarded, like \
                 the SessionContext at connect",
            );

        channel.interrupt().await.expect(
            "no in-flight response to cancel: the network half of barge-in \
             is trivially complete",
        );
        assert_eq!(
            generation.current(),
            0,
            "interrupt does not bump: the local half of barge-in belongs to \
             barge_in()/the harness, and this channel has no superseded stream"
        );

        channel.close().await.expect("clean close");
        replayer.await.expect("the recorded fixture replays green");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "none of the four methods put a frame on the wire: the session \
             sent exactly what the fixture pinned — CloseStream, nothing else"
        );
    }

    /// The launch [`SttSlot`] over the listen wire, one recorded session
    /// driven entirely through the SLOT interface — the fixture is composed
    /// from the already-recorded deepgram scripts (the connect-close
    /// handshake/shutdown pins, the audio-keepalive pinned PCM payload, the
    /// endpoint-race-dedupe first utterance): slot connect (header auth,
    /// credential-free URL), the pinned mic chunk reaching the wire as one
    /// raw binary frame, the interim + final user deltas and the final's
    /// word timing on the slot's event stream, exactly one `UtteranceEnd`
    /// for an utterance whose wire fires both end-of-speech signals, then
    /// the CloseStream shutdown.
    #[tokio::test]
    async fn deepgram_stt_slot_runs_a_session_through_the_slot_interface() {
        let handle = WireServer::start().await;
        let mut slot = DeepgramSttSlot::new(
            SttSlotConfig {
                endpoint_override: Some(handle.url("/v1/listen")),
                model: "nova-3".into(),
            },
            keys_with_deepgram_key(),
            Generation::new(),
        );

        let fixture = load_fixture("deepgram-stt-slot-session");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        // The slot's connect takes no SessionContext: prompt context is LLM
        // traffic the STT role never sees (VOICE_AGENT §3.2).
        slot.connect().await.expect("slot connect");
        let mut events = slot
            .take_events()
            .expect("the slot hands over the receiver");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "the slot surfaces the wire's lifecycle on its own stream"
        );

        // The audio-keepalive script's pinned payload (both i16 extremes, so
        // a byte-order slip cannot pass): the slot's PCM feed must reach the
        // fixture as exactly these little-endian bytes in one binary frame.
        let mic_chunk: [i16; 7] = [1000, -1000, 257, -2, 0, i16::MAX, i16::MIN];
        slot.send_audio(&mic_chunk).await.expect("slot PCM feed");

        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::User,
                text: "what does".into(),
            }),
            "interim transcript deltas surface through the slot interface"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::User,
                text: "what does figure three show".into(),
            })
        );
        let timed = |w: &str, start_ms: u32, end_ms: u32| TimedWord {
            w: w.into(),
            start_ms,
            end_ms,
        };
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::WordTimestamps {
                words: vec![
                    timed("what", 80, 240),
                    timed("does", 240, 460),
                    timed("figure", 520, 900),
                    timed("three", 900, 1280),
                    timed("show", 1440, 1860),
                ],
                gen: 0,
            }),
            "word timing passes through the slot untouched"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::UtteranceEnd),
            "speech_final commits the utterance through the slot"
        );

        // Slot close is the CloseStream shutdown; the wire's duplicate
        // UtteranceEnd (sent before the shutdown) must have produced nothing.
        slot.close().await.expect("slot close");
        replayer.await.expect("the recorded fixture replays green");
        assert_eq!(
            events.recv().await,
            None,
            "exactly one UtteranceEnd: the wire fired both end-of-speech \
             signals and the stream ends without a second one"
        );
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the slot session put exactly the pinned frames on the wire: \
             the binary PCM chunk, CloseStream, nothing else"
        );
    }

    #[test]
    fn capabilities_declare_server_endpointing_and_word_timestamps() {
        let channel = DeepgramChannel::new(
            DeepgramConfig {
                endpoint_override: None,
                model: "nova-3".into(),
            },
            keys(),
            Generation::new(),
        );
        assert_eq!(
            channel.capabilities(),
            ChannelCapabilities {
                vad_mode: VadMode::Server,
                word_timestamps: true,
                session_renewal: false,
                text_only: false,
                stt_only: true,
            },
            "Deepgram endpointing is authoritative (Server VAD), words[] \
             carries per-word start/end (word_timestamps), and /v1/listen \
             has no end-of-session announcement (no session_renewal)"
        );
    }

    /// The VA5 live smoke against the real free-tier endpoint (docs/TASKS.md
    /// VA5 acceptance: "live smoke on Deepgram + Gemini Live (free tier),
    /// executed only when free-tier API keys are provided (clean non-failing
    /// skip when absent)"). Key-optional and manual-only, the loopback-rig
    /// posture (`voice::audio::cpal_backend`): `#[ignore]` keeps it out of
    /// CI's bare `cargo test`, and a missing `DEEPGRAM_API_KEY` is a printed
    /// skip, never a failure. Run with:
    ///
    /// ```text
    /// DEEPGRAM_API_KEY=… cargo test --lib smoke_deepgram_live -- --ignored --nocapture
    /// ```
    ///
    /// The key rides only the VA3 [`KeyManager`] → `Authorization` header
    /// path the adapter already owns; this test never prints or asserts on
    /// key bytes.
    ///
    /// What "the contract shape completes" means here — a deterministic tone
    /// is not speech, and [`read_loop`] deliberately drops empty-transcript
    /// `Results` frames, so no transcript event is guaranteed and none is
    /// asserted verbatim (any that do arrive are printed):
    ///
    /// 1. connect succeeds against the production endpoint (real TLS, real
    ///    auth) and `Connected` is the first event;
    /// 2. every 20 ms tone chunk is accepted as a binary frame;
    /// 3. the session then survives an idle window longer than the 10-second
    ///    NET-0001 budget — the event stream staying open is live proof the
    ///    KeepAlive ticker holds a real connection, and nothing on it is an
    ///    `Error`;
    /// 4. close completes the doc's CloseStream flush-and-terminate handshake
    ///    inside [`SHUTDOWN_BUDGET`] (the server, not our abort fallback,
    ///    ended the session) and the event stream ends.
    #[tokio::test]
    #[ignore]
    async fn smoke_deepgram_live() {
        let Some(key) = std::env::var("DEEPGRAM_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
        else {
            println!(
                "SKIP smoke_deepgram_live: DEEPGRAM_API_KEY is not set — \
                 the free-tier live smoke is key-optional (docs/TASKS.md VA5)"
            );
            return;
        };
        let model = std::env::var("DEEPGRAM_LIVE_MODEL").unwrap_or_else(|_| "nova-3".to_string());

        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(DEEPGRAM_KEY_BASE_URL, &ApiKey::new(key))
            .expect("session-backend store never fails");
        let mut channel = DeepgramChannel::new(
            DeepgramConfig {
                endpoint_override: None,
                model,
            },
            Arc::new(KeyManager::with_store(store)),
            Generation::new(),
        );

        channel.connect(ctx()).await.expect("live connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "the open connection is ready — no setup exchange on this wire"
        );

        // 1.5 s deterministic 440 Hz tone, streamed as the pipeline would:
        // 20 ms chunks of 16 kHz mono PCM16, paced in real time.
        let chunk_frames = crate::voice::audio::frames_per_chunk(
            PROVIDER_INPUT_RATE,
            crate::voice::audio::MIC_CHUNK_MS,
        );
        let mut sample_index = 0u32;
        for _ in 0..75 {
            let chunk: Vec<i16> = (0..chunk_frames)
                .map(|_| {
                    let t = f64::from(sample_index) / f64::from(PROVIDER_INPUT_RATE);
                    sample_index += 1;
                    ((t * 440.0 * std::f64::consts::TAU).sin() * 8_000.0) as i16
                })
                .collect();
            channel
                .send_audio(&chunk)
                .await
                .expect("tone chunk accepted");
            tokio::time::sleep(Duration::from_millis(u64::from(
                crate::voice::audio::MIC_CHUNK_MS,
            )))
            .await;
        }

        // Idle listen window past the NET-0001 budget: transcripts (if the
        // server heard anything in the tone) are printed, Errors fail, and
        // the stream ending early means the server dropped us — i.e. the
        // KeepAlive ticker failed on a real wire.
        let window = Duration::from_secs(12);
        let opened = tokio::time::Instant::now();
        while let Some(remaining) = window.checked_sub(opened.elapsed()) {
            match timeout(remaining, events.recv()).await {
                Ok(Some(ChannelEvent::Error(err))) => panic!("live session error: {err:?}"),
                Ok(Some(event)) => println!("live event: {event:?}"),
                Ok(None) => panic!(
                    "the server closed the session during the idle window — \
                     the NET-0001 KeepAlive contract did not hold live"
                ),
                Err(_) => break,
            }
        }

        let closing = tokio::time::Instant::now();
        channel.close().await.expect("clean close");
        assert!(
            closing.elapsed() < SHUTDOWN_BUDGET,
            "close joined inside the budget: the server completed the \
             CloseStream flush-and-terminate handshake itself"
        );
        while let Some(event) = events.recv().await {
            assert!(
                !matches!(event, ChannelEvent::Error(_)),
                "the CloseStream flush surfaced an error: {event:?}"
            );
            println!("flush event: {event:?}");
        }
        println!(
            "smoke_deepgram_live: Connected → tone streamed → idle past NET-0001 → clean close"
        );
    }

    pub struct DeepgramSim {
        handle: WireHandle,
        interrupts: Arc<Mutex<Vec<Option<u64>>>>,
    }

    #[async_trait]
    impl ProviderSim for DeepgramSim {
        async fn emit_audio(&mut self, _gen: u64, _pcm: Vec<i16>) {}
        async fn emit_transcript(&mut self, role: Role, text: String) {
            let msg = serde_json::json!({
                "type": "Results",
                "channel": {
                    "alternatives": [{
                        "transcript": text,
                        "confidence": 0.99,
                        "words": []
                    }]
                }
            });
            self.handle.send(WireFrame::Text(msg.to_string()));
            if role == Role::User {
                let end = serde_json::json!({
                    "type": "UtteranceEnd"
                });
                self.handle.send(WireFrame::Text(end.to_string()));
            }
        }
        async fn emit_tool_call(
            &mut self,
            _call_id: String,
            _name: String,
            _args: serde_json::Value,
        ) {
        }
        async fn announce_session_end(&mut self) {
            panic!("conformance rig: deepgram declares session_renewal: false");
        }
        async fn complete_renewal(&mut self) {
            panic!("conformance rig: deepgram declares session_renewal: false");
        }
        async fn expire(&mut self) {
            self.handle.close_current();
        }
        async fn received_audio_by_epoch(&mut self) -> Vec<Vec<Vec<i16>>> {
            let mut epochs = Vec::new();
            for connection in self.handle.received_by_connection() {
                let mut epoch = Vec::new();
                for frame in connection {
                    if let WireFrame::Binary(bytes) = frame {
                        let samples: Vec<i16> = bytes
                            .chunks_exact(2)
                            .map(|b| i16::from_le_bytes([b[0], b[1]]))
                            .collect();
                        epoch.push(samples);
                    }
                }
                epochs.push(epoch);
            }
            epochs
        }
        async fn received_texts(&mut self) -> Vec<String> {
            Vec::new()
        }
        async fn received_context_updates(&mut self) -> Vec<ContextUpdate> {
            Vec::new()
        }
        async fn received_tool_results(&mut self) -> Vec<(String, ToolResult)> {
            Vec::new()
        }
        async fn received_interrupts(&mut self) -> Vec<Option<u64>> {
            self.interrupts.lock().unwrap().clone()
        }
    }

    pub struct DeepgramFixture {
        pub rigs: usize,
    }

    #[async_trait]
    impl ConformanceFixture for DeepgramFixture {
        type Channel = DeepgramChannel;
        type Sim = DeepgramSim;

        async fn rig(&mut self, generation: &Generation) -> (DeepgramChannel, DeepgramSim) {
            self.rigs += 1;
            let handle = WireServer::start().await;
            let channel = DeepgramChannel::new(
                DeepgramConfig {
                    endpoint_override: Some(handle.url("/v1/listen")),
                    model: "nova-3".into(),
                },
                keys_with_deepgram_key(),
                generation.clone(),
            );
            (
                channel,
                DeepgramSim {
                    handle,
                    interrupts: Arc::new(Mutex::new(Vec::new())),
                },
            )
        }
    }

    #[tokio::test]
    async fn deepgram_channel_passes_stt_conformance() {
        let mut fixture = DeepgramFixture { rigs: 0 };
        run_stt(&mut fixture).await;
        assert_eq!(fixture.rigs, 2);
    }

    #[tokio::test]
    async fn deepgram_surfaces_expired_on_unexpected_server_close() {
        let handle = WireServer::start().await;
        let mut channel = DeepgramChannel::new(
            DeepgramConfig {
                endpoint_override: Some(handle.url("/v1/listen")),
                model: "nova-3".into(),
            },
            keys_with_deepgram_key(),
            Generation::new(),
        );
        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("events");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        // Server abruptly closes the socket without channel.close() having run.
        handle.close_current();

        // The read loop should surface LifecycleEvent::Expired.
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Expired))
        );
    }
}
