//! Gemini Live API adapter (VA5 — BYOK external provider, VOICE_AGENT §3.1).
//!
//! Wire protocol provenance: https://ai.google.dev/api/live (official Live API
//! WebSockets reference, accessed 2026-08-20). The session rides one WebSocket
//! to `wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent`;
//! every client message is a JSON object carrying exactly one of `setup`,
//! `clientContent`, `realtimeInput`, or `toolResponse`, and audio travels
//! inline as base64 bytes (`Blob`) inside those JSON messages — the base64-JSON
//! framing stays an adapter detail the harness never sees (VOICE_AGENT §1).
//!
//! Session lifecycle (this unit): `connect` dials the WS with the key in the
//! `x-goog-api-key` request header (header form per
//! https://ai.google.dev/gemini-api/docs/api-key — connect URLs stay
//! credential-free, VOICE_AGENT §3.2 `auth-hygiene-no-query-param-creds`),
//! sends `BidiGenerateContentSetup` (model + audio response modality), and
//! waits for the server's `setupComplete` acknowledgement before reporting
//! `Lifecycle(Connected)` — the doc instructs clients to wait for
//! `BidiGenerateContentSetupComplete` before sending anything else. `close`
//! runs the WS close handshake.
//!
//! Audio path (this unit): `send_audio` frames each 16 kHz mono PCM16 mic
//! chunk (the VA1 up-resampler output, `voice::audio::PROVIDER_INPUT_RATE`)
//! as a base64 `Blob` inside `realtimeInput.audio` with
//! `mimeType: audio/pcm;rate=16000` (https://ai.google.dev/api/live, accessed
//! 2026-08-20 — `BidiGenerateContentRealtimeInput.audio`). Inbound, a read
//! loop decodes `serverContent.modelTurn` inline-data audio parts (PCM16 at
//! 24 kHz, `voice::audio::PROVIDER_OUTPUT_RATE`) into `AudioDelta` events
//! tagged by response turn, not by read time: the turn's first `modelTurn`
//! content snapshots the barge-in generation, every audio delta of that turn
//! carries the snapshot, and the wire's own turn delimiters retire it —
//! `serverContent.interrupted` ("a client message has interrupted current
//! model generation") or `serverContent.turnComplete` (generation done), both
//! per https://ai.google.dev/api/live, accessed 2026-08-21. A turn superseded
//! mid-flight by a barge-in bump keeps its now-stale snapshot, so every
//! remaining delta of that response dies at the playback gate
//! (`TaggedPlaybackWriter::push_block`) — read-time tagging would launder
//! them as live — while the next turn snapshots the bumped generation afresh
//! and plays.
//!
//! Turn boundary (issue #201): this channel is `VadMode::Server`, so it owes
//! the harness one `UtteranceEnd` per utterance (event.rs §3.2 rule), but the
//! Live API vocabulary has no user-utterance-end frame. The read loop maps
//! the obligation to the fresh-turn open — the first model event of each
//! response turn (a surfaced top-level `toolCall` entry or the turn's first
//! `modelTurn` content, whichever the wire delivers first), emitted before
//! that event — see `read_loop`'s doc for why that is the commit-time the
//! event pump's `response_gen` snapshot wants.
//!
//! Text path (this unit): `send_text` frames the no-mic fallback turn
//! (VOICE_AGENT §3) as `clientContent` — one user `Content` in `turns[]` with
//! `turnComplete: true` so generation starts on the accumulated prompt
//! (https://ai.google.dev/api/live, accessed 2026-08-21 —
//! `BidiGenerateContentClientContent`).
//!
//! Context refresh (this unit): `update_context` frames both §4 variants —
//! chapter-boundary instruction refresh and playback-position update — as
//! `clientContent` with `turnComplete: false`, the doc's incremental-update
//! path ("all of the content here is unconditionally appended to the
//! conversation history") where leaving the turn open means "the server
//! awaits additional messages before starting generation"
//! (https://ai.google.dev/api/live, accessed 2026-08-21 —
//! `BidiGenerateContentClientContent.turnComplete`): the refresh feeds the
//! accumulated prompt without triggering a spoken response, which is
//! VOICE_AGENT §4's "never a blind mid-turn re-prompt" on this wire. The
//! same doc warns a `clientContent` message "will interrupt any current
//! model generation" — harmless here because the harness schedules refreshes
//! at chapter boundaries, never mid-turn (§4). Re-sending `setup` is not an
//! alternative: it is valid "in the first (and only in the first)" client
//! message and "you cannot update the configuration while the connection is
//! open" (same doc), so the incremental path is the only mid-session context
//! mechanism the protocol offers.
//!
//! Transcripts (this unit): setup opts into transcription of both directions
//! with `outputAudioTranscription: {}` and an `inputAudioTranscription`
//! carrying language hints + excerpt-term vocabulary (F4; see
//! [`GeminiLiveChannel::setup_message`] and [`TRANSCRIPTION_LANGUAGE_HINTS`])
//! (https://ai.google.dev/api/live, accessed 2026-08-29 —
//! `BidiGenerateContentSetup.outputAudioTranscription` /
//! `.inputAudioTranscription`, each an `AudioTranscriptionConfig` whose
//! presence is the on switch); the read loop turns each
//! `serverContent.outputTranscription.text` payload into a
//! `TranscriptDelta { role: Assistant, .. }` and each
//! `serverContent.inputTranscription.text` payload into a
//! `TranscriptDelta { role: User, .. }`
//! (https://ai.google.dev/api/live, accessed 2026-08-21 —
//! `BidiGenerateContentServerContent` carries both, each a
//! `BidiGenerateContentTranscription { text }`). Gemini transcript deltas
//! are not word-aligned in either direction, so
//! [`SessionChannel::capabilities`] reports `word_timestamps: false` — the
//! VOICE_AGENT §11 sentence-level degrade flag the harness keys follow-along
//! off, one channel-wide flag covering input and output alike (conformance
//! check 2 pins ordering and payload for both roles and makes no per-role
//! timestamp distinction).
//!
//! Session cap (this unit): the server's advance disconnect notice
//! (`BidiGenerateContentServerMessage.goAway`, same doc) surfaces uniformly
//! as `Lifecycle(Renewing { time_left })`, with `GoAway.timeLeft` (a proto3
//! JSON `Duration`, decimal seconds with an `s` suffix) parsed when present.
//!
//! Tool calls (this unit): inbound `BidiGenerateContentToolCall` messages
//! (https://ai.google.dev/api/live, accessed 2026-08-21 — a server message
//! carrying `functionCalls[]`, each a `FunctionCall { id, name, args }`)
//! surface as `ChannelEvent::ToolCall`, one event per entry. The adapter
//! never interprets a call — even `teacher_anchor` passes through untouched:
//! the harness owns the §11.1 normalization (`super::anchor`), including the
//! stale-generation dropped-but-acked rule, and answers every call through
//! `send_tool_result`, which frames the ack as
//! `BidiGenerateContentToolResponse.functionResponses` (same doc; the wire
//! `FunctionResponse` wants `name` alongside the matching `id`, so the
//! adapter remembers each pending call's name until it is answered).
//!
//! Barge-in (this unit): the Live API has no client cancel frame — the client
//! message set is exactly `setup`, `clientContent`, `realtimeInput`, and
//! `toolResponse` (https://ai.google.dev/api/live, accessed 2026-08-21 —
//! `BidiGenerateContentClientMessage`, "must have exactly one" of those), and
//! interruption belongs to the server's own VAD
//! (`RealtimeInputConfig.automaticActivityDetection`, default
//! `START_OF_ACTIVITY_INTERRUPTS`), announced back as
//! `serverContent.interrupted`. `interrupt` therefore sends nothing: it bumps
//! the shared barge-in generation, and the gen-tag drop IS the cancel
//! mechanism — every delta of the superseded response still arrives tagged
//! with the dead id and dies at the playback gate
//! (`TaggedPlaybackWriter::push_block`). When the server cuts a turn it may
//! also retract in-flight tool calls with
//! `BidiGenerateContentToolCallCancellation` (`toolCallCancellation.ids[]`,
//! same doc — the retracted calls "should not have been executed"): the read
//! loop retires those pending ids, and a late `send_tool_result` for a
//! retired id is a silent no-op — the cancellation made silence the correct
//! answer, so it is not an `InvalidState` like an unknown or twice-answered
//! id.
//!
//! Renewal recycle (this unit): the `goAway` that surfaces as `Renewing` also
//! flips the channel into a renewing state, and while renewing every
//! submission (`send_audio`, `send_text`, `send_tool_result`) is rejected
//! immediately with `SessionExpired`, `retryable: true` — never queued: the
//! harness owns replay of rejected payloads (VOICE_AGENT §2 rule 3), and
//! conformance check 6 pins that a renewal-rejected payload reaches the
//! provider never. The read loop then recycles in place: it closes the old
//! connection (the write half comes out of the shared slot, so sends were
//! already cut off), dials the same endpoint again, repeats the setup
//! handshake — key in the header, `setupComplete` awaited — swaps the renewed
//! write half into the slot, and carries on as the new connection's read loop
//! on the same event receiver, clearing the renewing phase before emitting
//! `Reconnected` (whoever observes the event finds sends accepted again).
//! A recycle that cannot complete is terminal: the loop stores the expired
//! phase before emitting `Lifecycle(Expired)`, and from then on every
//! submission fails `InvalidState`, `retryable: false` — MockChannel's
//! `State::Expired` semantics (conformance check 9): the session is not
//! coming back, so the harness must not replay against it.
//! Renewal epochs are what conformance check 6 reads from the rig: the
//! fixture server groups frames by connection
//! ([`super::fixture::WireHandle::received_by_connection`]), so the
//! replacement dial itself is the epoch bump — connection *n* is epoch *n*.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine as _;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Bytes, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::voice::audio::generation::Generation;
use crate::voice::audio::PROVIDER_INPUT_RATE;
use crate::voice::keys::manager::DialCandidate;
use crate::voice::keys::{ApiKey, KeyManager};

use super::{
    ChannelCapabilities, ChannelError, ChannelEvent, ContextUpdate, ErrorKind, LifecycleEvent,
    Role, SessionChannel, SessionContext, ToolResult, VadMode,
};

/// Default Live API base; [`GeminiLiveConfig::endpoint_override`] replaces it
/// for proxies and recorded-fixture servers.
pub const DEFAULT_LIVE_ENDPOINT: &str =
    "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

/// The canonical base URL the user's Gemini key is stored under
/// (`Provider::Gemini.default_base_url()`); the key lookup is by this
/// identity even when [`GeminiLiveConfig::endpoint_override`] points the
/// socket elsewhere.
const GEMINI_KEY_BASE_URL: &str = "https://generativelanguage.googleapis.com";

const EVENT_CAPACITY: usize = 4096;

/// Send-gate phases ([`GeminiLiveChannel::phase`]), written by the read loop
/// before it emits the matching lifecycle event: live from connect (and again
/// after `Reconnected`), renewing from the server's `goAway`, expired when a
/// recycle fails — terminal, there is no way back out.
const PHASE_LIVE: u8 = 0;
const PHASE_RENEWING: u8 = 1;
const PHASE_EXPIRED: u8 = 2;
/// How long connect waits for the server's `setupComplete`, and close for the
/// peer's close acknowledgement, before giving up.
const HANDSHAKE_BUDGET: Duration = Duration::from_secs(10);

/// How long `close()` waits for the read loop to drain after the close
/// frame is sent BEFORE aborting it (2026-09-28 D2: the session-end teardown
/// budget chain — close joins × 2 channels + transcribe flush — must fit
/// inside `stop_and_wait`'s window; a 10 s join per channel made any live
/// server close-ack slower than 5 s trip the UI's "did not shut down"
/// banner while the teardown finished in the background. The close frame
/// is already on the wire; a silent peer at session end needs no further
/// draining — abort at 2 s). OWNER-TUNABLE: raise for exotic servers that
/// must finish long trailing drains before the socket may die.
pub(crate) const CLOSE_JOIN_BUDGET: Duration = Duration::from_secs(2);

/// Abrupt-loss recycle (R2-3, owner feedback 2026-08-30): redial backoff per
/// attempt (first is immediate), and the crash-loop guard — a replacement
/// connection that lives under [`ABRUPT_SHORT_LIFE`] counts as a short life,
/// and more than [`ABRUPT_SHORT_LIFE_LIMIT`] consecutive short lives expires
/// the session instead of dialing forever.
const ABRUPT_REDIAL_BACKOFF_MS: [u64; 3] = [0, 500, 2_000];
const ABRUPT_SHORT_LIFE: Duration = Duration::from_secs(5);
const ABRUPT_SHORT_LIFE_LIMIT: u32 = 2;

/// Keep-alive timers (hardening against 60s idle socket collapse during prolonged mute):
/// RFC 6455 Ping control frames sent every 25s at 0 token cost to satisfy edge proxy liveness.
pub const WS_PING_INTERVAL: Duration = Duration::from_secs(25);
/// Sparse media keep-alive interval: checked every 50s.
pub const SPARSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(50);
/// 100ms of 16kHz mono PCM16 silence (1600 samples) for application-level media keep-alive.
const SPARSE_KEEPALIVE_SAMPLES: usize = 1600;
static SPARSE_KEEPALIVE_SILENCE: [i16; SPARSE_KEEPALIVE_SAMPLES] = [0i16; SPARSE_KEEPALIVE_SAMPLES];

/// OWNER-TUNABLE (F4): BCP-47 hints for the input-audio transcriber — the
/// languages actually spoken at this rig (Egyptian Arabic code-switched with
/// English). Omitting the field means pure auto-detection
/// (https://ai.google.dev/api/live, checked 2026-08-29), which is what
/// produced the mangled code-switch transcripts.
const TRANSCRIPTION_LANGUAGE_HINTS: [&str; 2] = ["ar-EG", "en-US"];

/// Server-side sliding-window compression target for the 3.8 family
/// (2026-09-24 context diet + migration). DOCS: https://ai.google.dev/api/live
/// (BidiGenerateContentSetup.contextWindowCompression, checked 2026-09-24):
/// the server discards oldest-context turns once history exceeds the
/// trigger (default 80% of the 128 K window — too late for latency); the
/// sliding window keeps `targetTokens` (default trigger/2).
/// WHY 24,576: with the 131,072-token window, 24 K keeps prompt-TTFB in the
/// §2 latency budget for long teaching sessions while leaving the recent
/// two-plus turns of window-sized tool results verbatim; compression
/// itself costs a temporary latency bump, so the target must not sit so
/// low it fires every turn. OWNER-TUNABLE. Failure direction if too low:
/// compression fires often (latency spikes); if too high: bloat degrades
/// attention before the window slides.
pub(crate) const LIVE_COMPRESSION_TARGET_TOKENS: i64 = 24_576;

/// Adapter configuration. Credentials are deliberately absent: the key is
/// resolved at connect time through the VA3 [`KeyManager`] (VOICE_AGENT §7) —
/// no key bytes live outside `voice::keys::ApiKey`.
#[derive(Clone, Debug)]
pub struct GeminiLiveConfig {
    /// Full WS endpoint replacing [`DEFAULT_LIVE_ENDPOINT`] when set.
    pub endpoint_override: Option<String>,
    /// Live-capable model name, e.g. `models/gemini-2.5-flash-native-audio-preview-12-2025`.
    pub model: String,
}

/// Session state, the uniform contract the trait documents: connect is valid
/// exactly once from `New`; a closed session never reopens (the renewal
/// recycle happens inside `Connected` — the channel never leaves it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    New,
    Connected,
    Closed,
}

type LiveSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type LiveWriter = SplitSink<LiveSocket, Message>;

/// How long one outgoing frame may take to reach the socket (review
/// 2026-10-09, L8). A send that stalls (a half-open TCP connection whose
/// buffer filled) used to hold the actor in `.await` with no bound — Stop
/// then missed its budget. Past this, the send is a retryable transport
/// failure and the renewal machinery takes over.
const LIVE_SEND_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// `writer.send` bounded by [`LIVE_SEND_BUDGET`]; `what` names the frame
/// in the error.
async fn send_bounded(
    writer: &mut LiveWriter,
    message: Message,
    what: &str,
) -> Result<(), ChannelError> {
    match tokio::time::timeout(LIVE_SEND_BUDGET, writer.send(message)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(ChannelError::new(
            ErrorKind::Transport,
            true,
            format!("live ws dropped sending {what}: {err}"),
        )),
        Err(_) => Err(ChannelError::new(
            ErrorKind::Transport,
            true,
            format!("live ws send of {what} stalled past {LIVE_SEND_BUDGET:?}"),
        )),
    }
}

#[derive(Default)]
struct StallSignal {
    raised: AtomicBool,
    wake: tokio::sync::Notify,
}

impl StallSignal {
    fn raise(&self) {
        self.raised.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    fn take(&self) -> bool {
        self.raised.swap(false, Ordering::SeqCst)
    }

    fn clear(&self) {
        self.raised.store(false, Ordering::SeqCst);
    }
}

/// The Gemini Live session channel.
pub struct GeminiLiveChannel {
    config: GeminiLiveConfig,
    keys: Arc<KeyManager>,
    /// The rotation ring `build_channel` resolved for this start (RCA §9):
    /// dialed in order by `connect`, empty on the single-key path (fixtures,
    /// direct constructions), where connect falls back to the VA3 lookup.
    candidates: Vec<DialCandidate>,
    /// Clone of the session's shared barge-in counter
    /// (`voice::audio::generation`): the read loop reads it to tag
    /// `AudioDelta`s, and `interrupt` bumps it — on this channel the bump is
    /// the whole cancel (see the module doc's barge-in section).
    generation: Generation,
    /// Transcription-only session (`gemini-3.5-transcribe-live`): the setup
    /// frame becomes the lean TEXT-modality transcriber shape — no tools, no
    /// system instruction, no thinking config, no VAD tuning
    /// (https://ai.google.dev/gemini-api/docs/live-api/live-transcribe,
    /// checked 2026-08-29). Everything else (dialing, custody, read loop)
    /// is shared: finals arrive on the same `serverContent.inputTranscription`
    /// field the dialog parser already forwards as user deltas, and interim
    /// partials ride `interimInputTranscription`, a key this parser
    /// deliberately never reads.
    transcribe_mode: bool,
    state: SessionState,
    /// Write half of the split socket, live while `Connected`. A shared slot
    /// (the same shape as the fixture server's live-connection slot) rather
    /// than an owned field: the renewal recycle swaps the renewed connection's
    /// write half in from the read loop, and the slot is the one place both
    /// sides reach without fighting over ownership.
    writer: Arc<tokio::sync::Mutex<Option<LiveWriter>>>,
    /// The inbound decode loop; joined (budgeted) at `close`.
    reader: Option<JoinHandle<()>>,
    /// The latest server-issued session-resumption handle
    /// (`SessionResumptionUpdate.newHandle`, ai.google.dev/api/live checked
    /// 2026-09-24), shared with the read loop (it both records updates and
    /// consumes the handle at a recycle's redial). `None` until the server
    /// issues one. 3.8-family sessions only: the setup frame opts in
    /// (`sessionResumption: {}`), the legacy family never does.
    resumption_handle: Arc<Mutex<Option<String>>>,
    /// Tool names this session declared `behavior: NON_BLOCKING` (recorded at
    /// connect from the setup schemas). Only their answers carry the async
    /// `scheduling` field; empty for every Tutor session.
    #[cfg(feature = "book-agent")]
    nonblocking_tools: std::collections::HashSet<String>,
    /// Kept for send-side units that surface events outside the read loop;
    /// the read loop holds its own clone.
    events_tx: Option<mpsc::Sender<ChannelEvent>>,
    events: Option<mpsc::Receiver<ChannelEvent>>,
    /// Names of tool calls surfaced but not yet answered, by call id. The read
    /// loop records each inbound `functionCalls[]` entry; `send_tool_result`
    /// needs the name because the wire `FunctionResponse` carries it alongside
    /// the matching `id` (https://ai.google.dev/api/live, accessed 2026-08-21).
    pending_tool_calls: Arc<Mutex<HashMap<String, String>>>,
    /// Ids retired by an inbound `toolCallCancellation` (never answered). Kept
    /// so `send_tool_result` can tell a cancelled call (late result: silent
    /// no-op) from an unknown or already-answered one (`InvalidState`).
    cancelled_tool_calls: Arc<Mutex<HashSet<String>>>,
    /// The send-gate phase ([`PHASE_LIVE`] / [`PHASE_RENEWING`] /
    /// [`PHASE_EXPIRED`]): the read loop stores it (before emitting the
    /// matching lifecycle event, so anyone who saw the event sees the phase),
    /// and every send checks it through [`Self::phase_gate`].
    phase: Arc<AtomicU8>,
    /// Set by `close()` before it closes the socket: the read loop checks it
    /// when the socket ends so an INTENTIONAL close never triggers the
    /// abrupt-loss recycle (R2-3 fix, owner feedback 2026-08-30).
    closing: Arc<AtomicBool>,
    /// Timestamp of last audio media sent over the wire (either user speech or sparse keep-alive).
    last_media_sent: Arc<Mutex<tokio::time::Instant>>,
    ping_interval: Duration,
    sparse_keepalive_interval: Duration,
    send_stall: Arc<StallSignal>,
}

impl GeminiLiveChannel {
    pub fn new(config: GeminiLiveConfig, keys: Arc<KeyManager>, generation: Generation) -> Self {
        // The event pair exists from construction: take_events must yield the
        // receiver pre-connect (conformance check 1, matching MockChannel's
        // build-time pair); connect and the read loop only clone this sender.
        let (events_tx, events) = mpsc::channel(EVENT_CAPACITY);
        Self {
            config,
            keys,
            candidates: Vec::new(),
            generation,
            transcribe_mode: false,
            state: SessionState::New,
            writer: Arc::new(tokio::sync::Mutex::new(None)),
            reader: None,
            events_tx: Some(events_tx),
            events: Some(events),
            pending_tool_calls: Arc::new(Mutex::new(HashMap::new())),
            cancelled_tool_calls: Arc::new(Mutex::new(HashSet::new())),
            resumption_handle: Arc::new(Mutex::new(None)),
            phase: Arc::new(AtomicU8::new(PHASE_LIVE)),
            closing: Arc::new(AtomicBool::new(false)),
            last_media_sent: Arc::new(Mutex::new(tokio::time::Instant::now())),
            ping_interval: WS_PING_INTERVAL,
            sparse_keepalive_interval: SPARSE_KEEPALIVE_INTERVAL,
            send_stall: Arc::new(StallSignal::default()),
            #[cfg(feature = "book-agent")]
            nonblocking_tools: std::collections::HashSet::new(),
        }
    }

    /// Arms the rotation ring for `connect`. Candidates carry sealed keys,
    /// which is why they ride the channel rather than the Clone+Debug
    /// config — the config stays credential-free by construction.
    pub fn with_candidates(mut self, candidates: Vec<DialCandidate>) -> Self {
        self.candidates = candidates;
        self
    }

    /// Switches the setup frame to the transcription-only shape (see the
    /// `transcribe_mode` field doc). Everything after setup is unchanged.
    pub fn with_transcribe_mode(mut self) -> Self {
        self.transcribe_mode = true;
        self
    }

    #[cfg(test)]
    pub fn with_keepalive_intervals(mut self, ping: Duration, sparse: Duration) -> Self {
        self.ping_interval = ping;
        self.sparse_keepalive_interval = sparse;
        self
    }

    /// The one rejection seam every submission passes. Mid-renewal:
    /// `SessionExpired`, `retryable: true` — the session is coming back, and
    /// the harness replays the payload against the renewed connection
    /// (conformance check 6). Post-expiry: `InvalidState`, `retryable: false`,
    /// MockChannel's `State::Expired` semantics (conformance check 9) — the
    /// session is not coming back, so a replay has nothing to land on.
    fn phase_gate(&self, what: &str) -> Result<(), ChannelError> {
        match self.phase.load(Ordering::SeqCst) {
            PHASE_RENEWING => Err(ChannelError::new(
                ErrorKind::SessionExpired,
                true,
                format!("{what} while the session is renewing; retry after Reconnected"),
            )),
            PHASE_EXPIRED => Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("{what} on an expired session"),
            )),
            _ => Ok(()),
        }
    }

    fn renewal_recycle(what: &str) -> ChannelError {
        ChannelError::new(
            ErrorKind::SessionExpired,
            true,
            format!("{what} raced the renewal recycle; retry after Reconnected"),
        )
    }

    async fn send_frame(&self, message: Message, what: &str) -> Result<(), ChannelError> {
        let sent = timeout(LIVE_SEND_BUDGET, async {
            let mut writer = self.writer.lock().await;
            let Some(writer) = writer.as_mut() else {
                return Err(Self::renewal_recycle(what));
            };
            writer.send(message).await.map_err(|err| {
                ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("live ws dropped sending {what}: {err}"),
                )
            })
        })
        .await;
        match sent {
            Ok(result) => result,
            Err(_) => {
                self.send_stall.raise();
                Err(ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("live ws send of {what} stalled past {LIVE_SEND_BUDGET:?}"),
                ))
            }
        }
    }

    /// The endpoint the session will dial: the override when present, else
    /// [`DEFAULT_LIVE_ENDPOINT`].
    pub fn endpoint(&self) -> &str {
        self.config
            .endpoint_override
            .as_deref()
            .unwrap_or(DEFAULT_LIVE_ENDPOINT)
    }

    /// The first client message: `BidiGenerateContentSetup` with the model,
    /// the audio response modality, and transcription of both directions
    /// switched on (https://ai.google.dev/api/live, accessed 2026-08-20 —
    /// `model` is required, `generationConfig.responseModalities` selects
    /// audio-out, and `outputAudioTranscription` / `inputAudioTranscription`
    /// are each an `AudioTranscriptionConfig` whose presence enables
    /// transcription of the model's audio and the user's audio respectively;
    /// the input side carries [`TRANSCRIPTION_LANGUAGE_HINTS`] plus a
    /// `customVocabulary` of the excerpt's Latin terms — F4) —
    /// plus everything the [`SessionContext`] owes the wire (issue #200; the
    /// setup message is the Live API's only slot for either):
    ///
    /// - `systemInstruction`: the tutor system prompt, the sentinel-delimited
    ///   excerpt window, and the rolling summary when present, one `Content`
    ///   part each ("Only text should be used in parts and content in each
    ///   part will be in a separate paragraph" —
    ///   https://ai.google.dev/api/live, accessed 2026-08-23). The excerpt
    ///   arrives already delimited by the selector (VOICE_AGENT §4); this
    ///   adapter never touches book text itself.
    /// - `tools`: one `Tool` carrying every session tool as a
    ///   `functionDeclarations[]` entry ([`function_declaration`]), translated
    ///   from the crate's neutral OpenAI-shape schemas
    ///   (`SessionContext::tool_schemas`). Absent when the session offers no
    ///   tools — an empty `tools` array is not the same statement on this
    ///   wire.
    fn setup_message(&self, ctx: &SessionContext) -> String {
        if self.transcribe_mode {
            return self.transcribe_setup_message(ctx);
        }
        // The wire contract forks on the model family (model_pins carries
        // the docs): 3.8 omits thinking entirely (interleaved reasoning is
        // native; a thinking_config is an unsupported-field setup error)
        // and gains server-side compression; the 2.5 family keeps its
        // dated thinkingBudget=0 pin and its exact prior frame.
        let family = crate::model_pins::live_family(&self.config.model);
        let generation_config = serde_json::json!({
            "responseModalities": ["AUDIO"],
            // F7: page stills at the lowest documented resolution —
            // enough to read a diagram, cheapest in tokens (owner
            // ruling: mediaResolution LOW). OWNER-TUNABLE.
            "mediaResolution": "MEDIA_RESOLUTION_LOW",
        });
        let mut setup = serde_json::json!({
            "model": self.config.model,
            "generationConfig": generation_config,
        });
        match family {
            crate::model_pins::LiveFamily::Legacy25 => {
                // Live voice loop = thinking OFF (owner ruling 2026-08-29;
                // crate::model_pins::LIVE_THINKING_BUDGET carries the
                // docs): the 2.5 Live family enables dynamic thinking by
                // default, and every thinking token is audible silence.
                setup["generationConfig"]["thinkingConfig"] = serde_json::json!({
                    "thinkingBudget": crate::model_pins::LIVE_THINKING_BUDGET,
                });
            }
            crate::model_pins::LiveFamily::Gemini38 => {
                // 2026-09-24 context diet: the server slides its own
                // history window — long sessions cannot bloat the prompt
                // past the target. Never sent to the legacy family.
                setup["contextWindowCompression"] = serde_json::json!({
                    "slidingWindow": { "targetTokens": LIVE_COMPRESSION_TARGET_TOKENS }
                });
                // Session resumption opt-in (owner-approved 2026-09-24):
                // presence makes the server issue
                // `sessionResumptionUpdate` handles the read loop stores;
                // a recycle's redial presents the latest handle so the
                // replacement connection RESUMES server-side history
                // (ai.google.dev/api/live — `SessionResumptionConfig`,
                // checked 2026-09-24). No handle on the initial dial: "If
                // not present then a new session is created".
                setup["sessionResumption"] = serde_json::json!({});
            }
        }
        // Server-VAD tuning (https://ai.google.dev/api/live, checked
        // 2026-08-26: setup.realtimeInputConfig.automaticActivityDetection,
        // both sensitivities default HIGH when unset). LOW start + 300 ms
        // prefix suppress false barge-ins from residual echo; LOW end +
        // 800 ms silence stop mid-question end-of-turn commits (owner
        // journal 2026-08-27 04:59: an Arabic question truncated at
        // ~2.2 s into garbage the model then answered).
        setup["realtimeInputConfig"] = serde_json::json!({
            // F7 (owner ruling 2026-08-30): page stills ride the video
            // stream, and the 2.5 Live default turn coverage
            // (TURN_INCLUDES_ONLY_ACTIVITY) may exclude video frames
            // from the model's turn — ALL_INPUT makes a sent page part
            // of the conversation (docs: 3.1 includes all video by
            // default; 2.5 needs this set). OWNER-TUNABLE.
            "turnCoverage": "TURN_INCLUDES_ALL_INPUT",
            "automaticActivityDetection": {
                "startOfSpeechSensitivity": "START_SENSITIVITY_LOW",
                "endOfSpeechSensitivity": "END_SENSITIVITY_LOW",
                "prefixPaddingMs": 300,
                "silenceDurationMs": 800,
            },
        });
        setup["outputAudioTranscription"] = serde_json::json!({});
        // Input transcription with recognition steering (F4, docs
        // https://ai.google.dev/api/live checked 2026-08-29 —
        // `AudioTranscriptionConfig.languageCodes` are "BCP-47 language
        // codes providing hints about the languages present in the
        // audio" and `customVocabulary` "bias[es] recognition toward
        // specific terms"; both previously absent, the config was `{}`
        // = pure auto-detect, which mangled code-switched
        // Egyptian-Arabic-plus-English sessions ("do you see current
        // sql code" → "20 current sequel code", owner 2026-08-29).
        // The vocabulary re-lists Latin terms of the excerpt the wire
        // already carries — never new book text. Setup is a one-shot
        // frame (protocol limit): a chapter-boundary excerpt refresh
        // cannot re-bias the transcriber mid-session, so the bias
        // tracks the SESSION-START excerpt until the next renewal
        // rebuilds setup.
        setup["inputAudioTranscription"] = serde_json::json!({
            "languageCodes": TRANSCRIPTION_LANGUAGE_HINTS,
        });
        let vocabulary = crate::voice::excerpt::latin_terms(&ctx.excerpt.delimited_text);
        if !vocabulary.is_empty() {
            setup["inputAudioTranscription"]["customVocabulary"] = serde_json::json!(vocabulary);
        }
        let mut parts: Vec<serde_json::Value> = Vec::new();
        for text in [
            Some(ctx.system_prompt.as_str()),
            Some(ctx.excerpt.delimited_text.as_str()),
            ctx.rolling_summary.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
        {
            parts.push(serde_json::json!({ "text": text }));
        }
        if !parts.is_empty() {
            setup["systemInstruction"] = serde_json::json!({ "parts": parts });
        }
        let declarations: Vec<serde_json::Value> = ctx
            .tool_schemas
            .iter()
            .filter_map(function_declaration)
            .collect();
        if !declarations.is_empty() {
            setup["tools"] = serde_json::json!([{ "functionDeclarations": declarations }]);
        }
        serde_json::json!({ "setup": setup }).to_string()
    }

    /// The transcription-only setup frame, pinned to the documented shape
    /// (https://ai.google.dev/gemini-api/docs/live-api/live-transcribe,
    /// checked 2026-08-29): TEXT modality, the same language hints +
    /// excerpt-term vocabulary the dialog session declares (the transcribe
    /// model documents `customVocabulary` up to 1,000 terms, best ≤ 100 —
    /// our excerpt cap is 40), `mode` left at the documented VERBATIM
    /// default (owner toggle candidate: SMART cleans disfluencies but stops
    /// being a literal record). Deliberately absent: tools, system
    /// instruction, thinking config, VAD tuning — the transcriber owns its
    /// segmentation; only the dialog session owns turn-taking. The connect
    /// handshake still awaits `setupComplete` (the protocol-level server
    /// hello, BidiGenerateContentServerMessage); the live smoke is the
    /// evidence gate for this model honoring it.
    fn transcribe_setup_message(&self, ctx: &SessionContext) -> String {
        let mut transcription = serde_json::json!({
            "languageCodes": TRANSCRIPTION_LANGUAGE_HINTS,
        });
        let vocabulary = crate::voice::excerpt::latin_terms(&ctx.excerpt.delimited_text);
        if !vocabulary.is_empty() {
            transcription["customVocabulary"] = serde_json::json!(vocabulary);
        }
        serde_json::json!({
            "setup": {
                "model": self.config.model,
                "generationConfig": { "responseModalities": ["TEXT"] },
                "inputAudioTranscription": transcription,
            }
        })
        .to_string()
    }

    /// `realtimeInput.audioStreamEnd` — the documented finalization prompt
    /// for a transcription session's open segment ("an immediate turn
    /// finalization prompt", https://ai.google.dev/gemini-api/docs/live-api/
    /// live-transcribe, checked 2026-08-29): the server stops waiting for
    /// trailing silence and emits the final `inputTranscription` for audio
    /// it holds. Meaningful on transcribe-mode channels; harmless elsewhere.
    pub async fn send_audio_stream_end(&mut self) -> Result<(), ChannelError> {
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("audioStreamEnd on a channel in state {:?}", self.state),
            ));
        }
        self.phase_gate("audioStreamEnd")?;
        let payload =
            serde_json::json!({ "realtimeInput": { "audioStreamEnd": true } }).to_string();
        self.send_frame(Message::text(payload), "audioStreamEnd")
            .await
            .inspect_err(|err| {
                tracing::warn!(
                    seam = "gemini_live",
                    error = %err.message,
                    "gemini ws: failed sending audioStreamEnd"
                );
            })?;
        tracing::info!(
            seam = "gemini_live",
            "gemini ws: sent realtimeInput.audioStreamEnd"
        );
        Ok(())
    }

    /// Dials the ring in order: the first key that reaches `setupComplete`
    /// wins and is promoted; a key-specific refusal (Auth — the #218
    /// 1007/1008 mapping — or a quota close) marks its slot and moves to the
    /// next candidate; anything else (transport, protocol) says nothing
    /// about the key and aborts the start unchanged. The ring is already
    /// bounded by the resolver, so a pool of dead keys costs at most
    /// ring-many dials before the honest failure that names what was tried
    /// — labels and slot numbers, never bytes.
    async fn dial_first_usable(
        endpoint: &str,
        keys: &KeyManager,
        candidates: &[DialCandidate],
        setup_message: &str,
    ) -> Result<(LiveSocket, ActiveKey), ChannelError> {
        let mut tried: Vec<String> = Vec::new();
        for candidate in candidates {
            match Self::dial_and_setup(endpoint, &candidate.key, setup_message).await {
                Ok(socket) => {
                    keys.record_dial_success(&candidate.account);
                    return Ok((
                        socket,
                        ActiveKey {
                            account: Some(candidate.account.clone()),
                            key: candidate.key.clone(),
                        },
                    ));
                }
                Err(err) if err.kind == ErrorKind::Auth => {
                    let code = close_code_in(&err.message);
                    tracing::info!(
                        slot = %candidate.display_name(),
                        close_code = code,
                        verdict = "auth",
                        "gemini dial candidate refused"
                    );
                    keys.record_dial_auth_failure(&candidate.account, code);
                    tried.push(match code {
                        Some(code) => {
                            format!("{} (rejected, close {code})", candidate.display_name())
                        }
                        None => format!("{} (rejected)", candidate.display_name()),
                    });
                }
                Err(err) if err.kind == ErrorKind::RateLimited => {
                    tracing::info!(
                        slot = %candidate.display_name(),
                        close_code = close_code_in(&err.message),
                        verdict = "quota",
                        "gemini dial candidate refused"
                    );
                    keys.record_dial_quota(
                        &candidate.account,
                        close_code_in(&err.message),
                        retry_after_secs_in(&err.message),
                    );
                    tried.push(format!("{} (rate limited)", candidate.display_name()));
                }
                Err(err) => return Err(err),
            }
        }
        Err(ChannelError::new(
            ErrorKind::Auth,
            false,
            format!(
                "no stored Gemini key could start the session; tried {}",
                tried.join(", ")
            ),
        ))
    }

    /// The Live-WS capability probe (the P2 rig's pattern): dial, send a
    /// minimal setup, wait for `setupComplete`, close — never a session. An
    /// HTTP-valid key can still lack Live capability (the P2 RCA's 44-of-47
    /// finding), so this is the only probe that answers "can this key start
    /// a voice session". Non-billable by shape: the setup handshake carries
    /// no content, no audio, no tool results.
    pub(crate) async fn probe_live_handshake(
        endpoint: &str,
        model: &str,
        key: &ApiKey,
    ) -> Result<(), ChannelError> {
        let setup_message = serde_json::json!({ "setup": { "model": model } }).to_string();
        let mut socket = Self::dial_and_setup(endpoint, key, &setup_message).await?;
        let _ = socket.close(None).await;
        Ok(())
    }

    /// The whole session-opening handshake, shared by `connect` and the
    /// renewal recycle: dial with the given key in the `x-goog-api-key`
    /// header (the URL stays credential-free), send the setup message, and
    /// wait for `setupComplete`. The key arrives resolved — `connect` picks
    /// it through the rotation ring (or the VA3 single-key path), and the
    /// renewal recycle passes the session's own key back in verbatim.
    async fn dial_and_setup(
        endpoint: &str,
        key: &ApiKey,
        setup_message: &str,
    ) -> Result<LiveSocket, ChannelError> {
        let dial_started = std::time::Instant::now();
        let mut request = endpoint.into_client_request().map_err(|err| {
            ChannelError::new(ErrorKind::Protocol, false, format!("bad endpoint: {err}"))
        })?;
        // Header auth only; the connect URL stays credential-free and the
        // sensitive flag keeps the value out of any Debug rendering.
        let mut key_header = HeaderValue::from_str(key.expose()).map_err(|_| {
            ChannelError::new(
                ErrorKind::Auth,
                false,
                "the stored key contains characters no header can carry",
            )
        })?;
        key_header.set_sensitive(true);
        request.headers_mut().insert("x-goog-api-key", key_header);

        let (mut socket, _response) = connect_async(request).await.map_err(|err| {
            ChannelError::new(
                ErrorKind::Transport,
                true,
                format!("live ws connect failed: {err}"),
            )
        })?;

        socket
            .send(Message::text(setup_message))
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("live ws dropped sending setup: {err}"),
                )
            })?;
        Self::await_setup_complete(&mut socket).await?;
        tracing::info!(
            elapsed_ms = dial_started.elapsed().as_millis() as u64,
            "gemini setupComplete"
        );
        Ok(socket)
    }

    /// Reads frames until the server's `setupComplete` acknowledgement.
    async fn await_setup_complete(socket: &mut LiveSocket) -> Result<(), ChannelError> {
        let wait = async {
            while let Some(frame) = socket.next().await {
                let message = frame.map_err(|err| {
                    ChannelError::new(
                        ErrorKind::Transport,
                        true,
                        format!("live ws dropped before setupComplete: {err}"),
                    )
                })?;
                // Same frame rule as the read loop: the live server ships its
                // JSON in binary frames (observed live 2026-08-22 —
                // `setupComplete` arrived binary), text stays accepted too.
                let text = match message {
                    Message::Text(text) => text.to_string(),
                    Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(_) => continue,
                    },
                    // An explicit close during setup is the provider refusing
                    // the session, and its close frame carries the only
                    // actionable reason (observed live 2026-08-25: a bad key
                    // is code 1007, "API key not valid. Please pass a valid
                    // API key."). 1007/1008 are the provider's key/policy
                    // refusals — the same key will be refused again; anything
                    // else is a provider-side condition worth retrying.
                    Message::Close(frame) => {
                        let (code, reason) = frame
                            .map(|f| (u16::from(f.code), f.reason.to_string()))
                            .unwrap_or((1005, String::new()));
                        let refusal = matches!(code, 1007 | 1008);
                        // A quota refusal is key-specific too (this key's
                        // allowance, not this connection): RateLimited, so
                        // the rotation loop can mark the slot and move on.
                        let quota = !refusal && is_quota_reason(&reason);
                        return Err(ChannelError::new(
                            if refusal {
                                ErrorKind::Auth
                            } else if quota {
                                ErrorKind::RateLimited
                            } else {
                                ErrorKind::Provider
                            },
                            !refusal,
                            format!(
                                "the provider closed the session during setup (close {code}): {reason}"
                            ),
                        ));
                    }
                    _ => continue,
                };
                let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|err| {
                    ChannelError::new(
                        ErrorKind::Protocol,
                        false,
                        format!("non-JSON frame during session setup: {err}"),
                    )
                })?;
                if parsed.get("setupComplete").is_some() {
                    return Ok(());
                }
                return Err(ChannelError::new(
                    ErrorKind::Protocol,
                    false,
                    "the first server message was not setupComplete",
                ));
            }
            Err(ChannelError::new(
                ErrorKind::Transport,
                true,
                "live ws closed before setupComplete",
            ))
        };
        timeout(HANDSHAKE_BUDGET, wait).await.map_err(|_| {
            ChannelError::new(
                ErrorKind::Provider,
                true,
                "the provider never acknowledged session setup",
            )
        })?
    }
}

/// One mic chunk as the wire message: PCM16 little-endian bytes, base64, inside
/// `realtimeInput.audio` (https://ai.google.dev/api/live, accessed 2026-08-20 —
/// `BidiGenerateContentRealtimeInput.audio` is a `Blob { mimeType, data }`).
/// Translate one neutral OpenAI-shape function schema
/// ([`SessionContext::tool_schemas`], the crate's schema convention per
/// `voice::tools::registry`) into a Live API `FunctionDeclaration`
/// (https://ai.google.dev/api/live and /api/generate-content, accessed
/// 2026-08-23: `{name, description, parameters: Schema}`). A schema whose
/// `parameters.properties` is empty omits `parameters` entirely — the wire
/// rejects an `OBJECT` schema with no properties, and an absent field already
/// means "no arguments". Entries that do not carry the expected shape are
/// skipped rather than guessed at.
pub(super) fn function_declaration(schema: &serde_json::Value) -> Option<serde_json::Value> {
    let function = schema.get("function")?;
    let name = function.get("name").and_then(|name| name.as_str())?;
    let mut declaration = serde_json::json!({ "name": name });
    if let Some(description) = function.get("description").and_then(|d| d.as_str()) {
        declaration["description"] = serde_json::json!(description);
    }
    // Asynchronous function calling (ai.google.dev/gemini-api/docs/live-api/
    // tools + models/gemini-3.8-live migration guide, checked 2026-09-24):
    // an explicit `behavior` (the agent's NON_BLOCKING declarations) is
    // forwarded verbatim. ABSENT now pins `behavior: "BLOCKING"` — the
    // 3.8 family made NON_BLOCKING the DEFAULT, and the tutor harness's
    // turn semantics (at-most-once acks, D4 turn sealing, the dead-air
    // watch) are all blocking-turn contracts; on the 2.5 family the
    // explicit value matches its native default, changing nothing.
    match function.get("behavior").and_then(|b| b.as_str()) {
        Some(behavior) => {
            declaration["behavior"] = serde_json::json!(behavior);
        }
        None => {
            declaration["behavior"] = serde_json::json!("BLOCKING");
        }
    }
    if let Some(parameters) = function.get("parameters") {
        let has_properties = parameters
            .get("properties")
            .and_then(|properties| properties.as_object())
            .is_some_and(|properties| !properties.is_empty());
        if has_properties {
            declaration["parameters"] = gemini_schema(parameters);
        }
    }
    Some(declaration)
}

/// The `Schema` half of [`function_declaration`]: keep the fields the Live
/// API's proto accepts (`type` — uppercase enum values on the raw wire —
/// `description`, `enum`, `required`, `minimum`, `maximum`, `properties`,
/// `items`), recurse into `properties`/`items`, and drop the JSON-Schema
/// strictness vocabulary the proto does not know (`additionalProperties`,
/// `minLength`). Dropping those loses nothing: §5 strictness is enforced by
/// the dispatcher's own validation on every inbound call, never delegated to
/// the provider.
fn gemini_schema(schema: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let Some(kind) = schema.get("type").and_then(|kind| kind.as_str()) {
        out.insert(
            "type".to_string(),
            serde_json::json!(kind.to_ascii_uppercase()),
        );
    }
    for key in ["description", "enum", "required", "minimum", "maximum"] {
        if let Some(value) = schema.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        out.insert(
            "properties".to_string(),
            serde_json::Value::Object(
                properties
                    .iter()
                    .map(|(name, sub)| (name.clone(), gemini_schema(sub)))
                    .collect(),
            ),
        );
    }
    if let Some(items) = schema.get("items") {
        out.insert("items".to_string(), gemini_schema(items));
    }
    serde_json::Value::Object(out)
}

fn realtime_audio_message(pcm_16k_mono: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(pcm_16k_mono.len() * 2);
    for sample in pcm_16k_mono {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    serde_json::json!({
        "realtimeInput": {
            "audio": {
                "mimeType": format!("audio/pcm;rate={PROVIDER_INPUT_RATE}"),
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            }
        }
    })
    .to_string()
}

/// One still image as a realtime video frame (`BidiGenerateContentRealtimeInput
/// .video`, a `Blob` — https://ai.google.dev/api/live, checked 2026-08-30).
fn realtime_image_message(image: &[u8], mime: &str) -> String {
    serde_json::json!({
        "realtimeInput": {
            "video": {
                "mimeType": mime,
                "data": base64::engine::general_purpose::STANDARD.encode(image),
            }
        }
    })
    .to_string()
}

/// One text turn as the wire message: a `BidiGenerateContentClientContent`
/// appending a single user `Content` to the conversation, with
/// `turnComplete: true` so "the server content generation should start with
/// the currently accumulated prompt" (https://ai.google.dev/api/live, accessed
/// 2026-08-21 — `clientContent.turns[]` / `clientContent.turnComplete`).
fn client_content_text_message(text: &str) -> String {
    serde_json::json!({
        "clientContent": {
            "turns": [{
                "role": "user",
                "parts": [{ "text": text }],
            }],
            "turnComplete": true,
        }
    })
    .to_string()
}

/// One mid-session context refresh as the wire message: a
/// `BidiGenerateContentClientContent` whose single user `Content` carries the
/// update rendered as text, with `turnComplete: false` so the content joins
/// the accumulated prompt while "the server awaits additional messages before
/// starting generation" (https://ai.google.dev/api/live, accessed 2026-08-21
/// — `clientContent.turnComplete`): a refresh informs the next turn, it never
/// triggers a spoken response (VOICE_AGENT §4, "not a re-prompt"). The wire
/// `Content.role` only offers user/model, so the text names itself a context
/// update rather than a spoken turn; the delimited excerpt travels inside
/// verbatim — the adapter never parses it (VOICE_AGENT §4).
///
/// `None` = the update has no place on this wire. The JIT code-switch
/// fragment is one (investigation 2026-10-09, §3.4): the native-audio model
/// understands the user from the AUDIO, never from its own input
/// transcription, so a note about how that transcription renders English
/// terms changes nothing it hears — the terms already ride the system
/// instruction's excerpt and the transcriber's `customVocabulary`. And the
/// frame itself is harmful here: "a message here will interrupt any current
/// model generation" (https://ai.google.dev/api/live, checked 2026-10-09 —
/// `BidiGenerateContentClientContent`), while the harness sends the retire
/// on the reply's FIRST assistant delta, i.e. mid-generation. The composed
/// and Deepgram adapters keep the fragment: their LLMs read the transcript,
/// and they apply it to the instruction slot, not the wire.
fn client_content_context_message(update: &ContextUpdate) -> Option<String> {
    let text = match update {
        ContextUpdate::ChapterBoundary {
            chapter_idx,
            excerpt,
        } => format!(
            "Context update (not a spoken user turn): the reader is now at \
             chapter {chapter_idx}. The current excerpt is:\n{}",
            excerpt.delimited_text
        ),
        ContextUpdate::PlaybackPosition { pos_ms } => {
            format!("Context update (not a spoken user turn): playback position is {pos_ms} ms.")
        }
        ContextUpdate::RenewalRecap { recap, switched_to } => format!(
            "Context update (not a spoken user turn): {}\n{}",
            super::types::renewal_recap_framing(*switched_to),
            recap.delimited_text
        ),
        ContextUpdate::ResumeRecap {
            recap,
            origin,
            location,
            topic,
            lang,
        } => format!(
            "Context update (not a spoken user turn): {}\n{}",
            super::types::resume_recap_framing(origin, location.as_ref(), topic.as_deref(), *lang),
            recap.delimited_text
        ),
        ContextUpdate::CodeSwitchFragment { .. } => return None,
    };
    serde_json::json!({
        "clientContent": {
            "turns": [{
                "role": "user",
                "parts": [{ "text": text }],
            }],
            "turnComplete": false,
        }
    })
    .to_string()
    .into()
}

/// Decodes every audio part of a `serverContent` message: `modelTurn.parts[]`
/// entries whose `inlineData` carries `audio/pcm` (24 kHz PCM16 LE per the doc)
/// become sample buffers, in part order. Non-audio parts and malformed base64
/// are skipped — transcripts and protocol-error surfacing are later VA5 units.
fn server_content_audio(server_content: &serde_json::Value) -> Vec<Vec<i16>> {
    let Some(parts) = server_content
        .get("modelTurn")
        .and_then(|turn| turn.get("parts"))
        .and_then(|parts| parts.as_array())
    else {
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|part| {
            let inline = part.get("inlineData")?;
            if !inline.get("mimeType")?.as_str()?.starts_with("audio/pcm") {
                return None;
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(inline.get("data")?.as_str()?)
                .ok()?;
            Some(
                bytes
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect(),
            )
        })
        .collect()
}

/// One tool-call answer as the wire message: a `BidiGenerateContentToolResponse`
/// whose single `FunctionResponse` matches the call by `id` and carries the
/// call's `name` (https://ai.google.dev/api/live, accessed 2026-08-21). The
/// wire `response` field is a JSON object, so a non-object `ToolResult::Ok`
/// payload is wrapped as `{"result": …}`; a `ToolResult::Err` becomes
/// `{"error": {code, message}}` — the harness-side ack vocabulary
/// (`super::anchor` codes included) travels inside that envelope.
pub(super) fn tool_response_body(result: &ToolResult) -> serde_json::Value {
    match result {
        ToolResult::Ok(value) if value.is_object() => value.clone(),
        ToolResult::Ok(value) => serde_json::json!({ "result": value }),
        ToolResult::Err { code, message } => serde_json::json!({
            "error": { "code": code, "message": message }
        }),
    }
}

#[cfg_attr(not(feature = "book-agent"), allow(unused_variables))]
fn tool_response_message(
    call_id: &str,
    name: &str,
    result: &ToolResult,
    nonblocking: bool,
) -> String {
    #[allow(unused_mut)]
    let mut response = tool_response_body(result);
    // Async response scheduling for Agent tools (ai.google.dev Live API
    // tools, accessed 2026-08-29: `scheduling` rides inside the
    // FunctionResponse `response` object). Selection rationale:
    // - Evidence reads (`search_book`, `open_passage`) and every error ack:
    //   WHEN_IDLE — the model must fold the outcome into its answer (a
    //   SILENT failure ack would let it keep claiming an effect that was
    //   refused, a grounding hazard).
    // - Successful mutation acks (navigation/citation/playback): SILENT —
    //   the state change is the effect; the model already narrates its
    //   action and must not be forced into a second response per ack.
    // Tutor tools carry no scheduling field: their calls are blocking, and
    // the wire shape stays byte-identical to before.
    #[cfg(feature = "book-agent")]
    if nonblocking {
        response["scheduling"] = serde_json::json!(agent_scheduling(name, result));
    }
    serde_json::json!({
        "toolResponse": {
            "functionResponses": [{
                "id": call_id,
                "name": name,
                "response": response,
            }]
        }
    })
    .to_string()
}

/// The `scheduling` value for one NON_BLOCKING tool answer. Callers gate on
/// the session's declared set. Interim owner-tunable law (flagged, not
/// silently settled): evidence reads, error acks, AND successful navigation/
/// playback acks are WHEN_IDLE — the docs define WHEN_IDLE as "respond once
/// finished with the current utterance", which is exactly charter §5.2.4's
/// "acts first and is confirmed briefly"; SILENT would suppress the spoken
/// confirmation entirely. The one SILENT case is a successful
/// `highlight_passage`: the visible highlight IS the citation (§1.2.3) and
/// the model is already speaking the answer it cites — forcing a second
/// response per citation would double-speak every evidence chain.
#[cfg(feature = "book-agent")]
fn agent_scheduling(name: &str, result: &ToolResult) -> &'static str {
    use crate::voice::tools::registry as tools;
    match (name, result) {
        (tools::HIGHLIGHT_PASSAGE, ToolResult::Ok(_)) => "SILENT",
        _ => "WHEN_IDLE",
    }
}

/// Parses a proto3-JSON `google.protobuf.Duration` — decimal seconds with an
/// `s` suffix, e.g. `"584.32s"` (the encoding `GoAway.timeLeft` arrives in).
/// Anything malformed or negative parses to `None`: a garbled deadline must
/// not suppress the session-ending signal itself.
fn parse_proto_duration(raw: &str) -> Option<Duration> {
    let seconds: f64 = raw.strip_suffix('s')?.parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds).ok()
}

/// The key a connected session dials with, remembered for its whole
/// lifetime: renewal REUSES it verbatim — rotating mid-session would hand
/// the recycle a key the provider has no session affinity with and
/// desynchronize the G2/H1 renewal machinery from the ledger. `account` is
/// `None` on the single-key path (fixtures, pre-rotation callers), where
/// there is no slot to mark.
struct ActiveKey {
    account: Option<String>,
    key: ApiKey,
}

/// Everything the renewal recycle needs to dial the replacement connection:
/// the same endpoint, the same setup message, and the SAME key the session
/// started with (never re-resolved — see [`ActiveKey`]). The manager rides
/// along only so a renewal-time key refusal can mark the slot's health.
struct Redial {
    endpoint: String,
    keys: Arc<KeyManager>,
    setup_message: String,
    active: ActiveKey,
    /// The channel's [`GeminiLiveChannel::resumption_handle`] clone: the
    /// read loop records `sessionResumptionUpdate.newHandle` here and reads
    /// it back at every recycle's redial (owner-approved 2026-09-24).
    resumption_handle: Arc<Mutex<Option<String>>>,
}

/// One renewal redial under session resumption (owner-approved
/// 2026-09-24; ai.google.dev/api/live — `SessionResumptionConfig.handle`,
/// checked 2026-09-24). With a stored handle the replacement dial presents
/// it — a success RESUMES the server's history and the caller must skip the
/// renewal recap re-inject (`Reconnected { resumed: true }`). Any resumption
/// dial failure falls back to the ORIGINAL fresh dial unchanged (a stale
/// handle must degrade to today's behavior, never to an expired session):
/// the fallback's error is the one the caller's failure path sees. `None` in
/// means no handle was ever issued — the plain dial, `resumed: false`.
async fn renew_dial(redial: &Redial) -> Result<(LiveSocket, bool), ChannelError> {
    let stored = redial
        .resumption_handle
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    if let Some(handle) = stored {
        let mut setup = serde_json::from_str::<serde_json::Value>(&redial.setup_message)
            .unwrap_or_else(|_| serde_json::json!({}));
        setup["setup"]["sessionResumption"]["handle"] = serde_json::json!(handle);
        let setup_message = setup.to_string();
        match GeminiLiveChannel::dial_and_setup(
            &redial.endpoint,
            &redial.active.key,
            &setup_message,
        )
        .await
        {
            Ok(socket) => return Ok((socket, true)),
            Err(err) => {
                tracing::warn!(
                    seam = "wire",
                    reason = ?err.kind,
                    "gemini resumption redial refused; dropping the handle and \
                     falling back to a fresh session"
                );
                // Owner review note (validated 2026-09-24): the refused
                // handle is dead state by direct evidence. The store only
                // ever overwrites on a NON-EMPTY `newHandle` — if the
                // fallback's fresh session never issues one (the server's
                // `resumable: false` windows), a kept handle would be
                // re-presented at EVERY later recycle, each costing a
                // doomed dial before the same fallback. Clearing trades a
                // possibly-still-good-but-unproven handle for the proven
                // recap path — the asymmetry favors clearing. Best effort:
                // a poisoned lock only delays the overwrite, never breaks
                // the fallback below.
                if let Ok(mut stored) = redial.resumption_handle.lock() {
                    *stored = None;
                }
            }
        }
    }
    GeminiLiveChannel::dial_and_setup(&redial.endpoint, &redial.active.key, &redial.setup_message)
        .await
        .map(|socket| (socket, false))
}

/// Extracts the close code from the setup-refusal message shape
/// `await_setup_complete` renders ("… (close {code}): {reason}").
pub(crate) fn close_code_in(message: &str) -> Option<u16> {
    let after = message.split("(close ").nth(1)?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Whether a non-1007/1008 setup close is a quota refusal. The Live API
/// words these as e.g. "You exceeded your current quota" or carries the
/// gRPC status name RESOURCE_EXHAUSTED in the reason.
fn is_quota_reason(reason: &str) -> bool {
    reason.to_ascii_lowercase().contains("quota") || reason.contains("RESOURCE_EXHAUSTED")
}

/// Pulls a provider-given retry delay (seconds) out of a quota refusal, if
/// one rides along. Two wire formats exist:
/// - Gemini Live WS: `"retryDelay":"32s"` (JSON key in the error payload)
/// - Gemini REST:    `"Please retry in 1.746835114s"` (prose in the message)
pub(crate) fn retry_after_secs_in(message: &str) -> Option<u64> {
    if let Some(after) = message.split("retryDelay").nth(1) {
        let digits: String = after
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(secs) = digits.parse::<u64>() {
            return Some(secs);
        }
    }
    if let Some(after) = message.split("retry in ").nth(1) {
        let num_str: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Ok(secs) = num_str.parse::<f64>() {
            return Some((secs as u64).max(1));
        }
    }
    None
}

/// Inbound loop for the life of the session, decoding server messages into the
/// uniform event stream:
///
/// - `serverContent.modelTurn` audio parts become `AudioDelta` events tagged
///   with the generation snapshotted when their response turn opened (the
///   turn's first `modelTurn` content); `serverContent.interrupted` and
///   `serverContent.turnComplete` retire the open turn. After a barge-in bump,
///   deltas of the superseded turn keep its pre-bump snapshot — the dead id —
///   and die at the playback gate (`TaggedPlaybackWriter::push_block`), while
///   the next turn snapshots the bumped generation and plays.
/// - `serverContent.outputTranscription.text` becomes
///   `TranscriptDelta { role: Assistant, .. }` and
///   `serverContent.inputTranscription.text` becomes
///   `TranscriptDelta { role: User, .. }`, in arrival order (the doc
///   guarantees no ordering between transcription and the audio it narrates —
///   sentence-level follow-along is timed against playback offset, §11).
/// - one `UtteranceEnd` per response turn, at the fresh-turn open (issue
///   #201): the Live API has no user-utterance-end frame, so this Server-VAD
///   channel discharges the §3.2 obligation at the first model event of each
///   turn — a surfaced `toolCall` entry or the first `modelTurn` content,
///   whichever arrives first — emitted *before* that event so the pump's
///   `response_gen` snapshot (harness.rs) is re-armed before it judges the
///   turn's anchors. Transcription-only frames commit nothing: they narrate
///   speech, they do not prove the server committed the utterance and started
///   responding. The same delimiters that retire the audio snapshot
///   (`interrupted` / `turnComplete`, or a renewal recycle) re-arm the
///   emission for the next turn.
/// - a top-level `goAway` stores the renewing phase (send-side rejections
///   start before the event lands), becomes `Lifecycle(Renewing { time_left })`,
///   and starts the recycle: the loop closes the old connection, dials and
///   sets up the replacement, swaps its write half into the shared slot, and
///   continues as the new connection's read loop — restoring the live phase
///   before emitting `Reconnected`. A recycle that cannot complete stores the
///   terminal expired phase and surfaces as `Lifecycle(Expired)`: the session
///   ended without recovery, and every later send fails non-retryably.
/// - a top-level `toolCall` becomes one `ToolCall` event per `functionCalls[]`
///   entry, id/name/args passed through uninterpreted (the harness normalizes
///   `teacher_anchor` — `super::anchor` — and answers via `send_tool_result`);
///   each entry's name is remembered so the answer's `FunctionResponse` can
///   carry it. Entries missing `id` or `name` are skipped: unanswerable calls
///   must not surface.
/// - a top-level `toolCallCancellation` retires every named id: out of
///   `pending_tool_calls`, into `cancelled_tool_calls`. No event is emitted —
///   the server retracted the calls, so the harness-visible outcome is that a
///   late `send_tool_result` for those ids becomes a silent no-op.
///
/// Frames this unit does not handle (resumption updates) are skipped, not
/// errors: the event enum consumers must tolerate growth, and those frames
/// get their decode in later VA5 units. The loop ends when the socket does.
// One spawn site, and each arg is a distinct shared handle — a bundling
// struct would exist only to appease the count.
#[allow(clippy::too_many_arguments)]
async fn read_loop(
    mut reader: SplitStream<LiveSocket>,
    redial: Redial,
    writer: Arc<tokio::sync::Mutex<Option<LiveWriter>>>,
    events: mpsc::Sender<ChannelEvent>,
    generation: Generation,
    pending_tool_calls: Arc<Mutex<HashMap<String, String>>>,
    cancelled_tool_calls: Arc<Mutex<HashSet<String>>>,
    phase: Arc<AtomicU8>,
    closing: Arc<AtomicBool>,
    transcribe_mode: bool,
    last_media_sent: Arc<Mutex<tokio::time::Instant>>,
    ping_interval_dur: Duration,
    sparse_interval_dur: Duration,
    send_stall: Arc<StallSignal>,
) {
    // The open response turn's generation snapshot, `None` between turns:
    // taken when a turn's first `modelTurn` content arrives, carried by every
    // audio delta of the turn, retired by the wire's turn delimiters
    // (`interrupted` / `turnComplete`) — see the module doc's audio path.
    let mut turn_gen: Option<u64> = None;
    // Whether the open response turn has already committed the user's
    // utterance (issue #201). The Live API has no user-utterance-end frame,
    // so the adapter maps the §3.2 Server-VAD `UtteranceEnd` obligation to
    // the fresh-turn open: the first model event of a response turn — a
    // surfaced top-level `toolCall` entry or the turn's first `modelTurn`
    // content, whichever the wire delivers first (tool calls often precede
    // the turn's first audio) — proves the server committed the utterance
    // and started responding, which is exactly the commit-time the pump's
    // `response_gen` snapshot wants (harness.rs: the composed loop's
    // `reply_gen` discipline). Emitted at most once per turn, re-armed by
    // the same delimiters that retire `turn_gen`.
    let mut utterance_committed = false;
    // One `TranscriptActivity` per finalization cycle (transcribe mode):
    // armed by the first interim, re-armed when the final is forwarded.
    let mut interim_signalled = false;
    // Kritias round 4: the MONOTONIC finalization-cycle index this activity
    // and the final it precedes are identified by. One serial read loop over
    // one ordered WebSocket ⇒ cycles cannot reorder; the counter survives
    // in-place recycles (goAway and abrupt) so identity stays unique for the
    // channel's whole life — a cycle whose socket died before its final
    // simply never settles, which the session ledger detects and surfaces.
    let mut finalize_cycle: u64 = 0;
    // Abrupt-loss recycle state (R2-3): when this connection was dialed and
    // how many consecutive replacements died young — the crash-loop guard.
    let mut connected_at = std::time::Instant::now();
    let mut short_lives: u32 = 0;
    'conn: loop {
        send_stall.clear();
        let mut ping_interval = tokio::time::interval_at(
            tokio::time::Instant::now() + ping_interval_dur,
            ping_interval_dur,
        );
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let mut sparse_keepalive_interval = tokio::time::interval_at(
            tokio::time::Instant::now() + sparse_interval_dur,
            sparse_interval_dur,
        );
        sparse_keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        'msg: loop {
            let message = tokio::select! {
                biased;

                maybe_msg = reader.next() => {
                    match maybe_msg {
                        Some(Ok(msg)) => msg,
                        Some(Err(err)) => {
                            tracing::debug!(err = %err, "gemini websocket stream error");
                            break 'msg;
                        }
                        None => {
                            tracing::debug!("gemini websocket stream closed by peer");
                            break 'msg;
                        }
                    }
                }

                _ = send_stall.wake.notified() => {
                    if send_stall.take() {
                        tracing::warn!(seam = "wire", "gemini send stalled; recycling the connection");
                        break 'msg;
                    }
                    continue 'msg;
                }

                _ = ping_interval.tick() => {
                    if phase.load(Ordering::SeqCst) == PHASE_LIVE && !closing.load(Ordering::SeqCst) {
                        let mut guard = writer.lock().await;
                        if let Some(w) = guard.as_mut() {
                            if let Err(err) = send_bounded(w, Message::Ping(Bytes::new()), "ping").await {
                                tracing::debug!(err = %err.message, "failed to send ws ping; recycling the connection");
                                break 'msg;
                            }
                        }
                    }
                    continue 'msg;
                }

                _ = sparse_keepalive_interval.tick() => {
                    if phase.load(Ordering::SeqCst) == PHASE_LIVE && !closing.load(Ordering::SeqCst) {
                        let should_send = {
                            let last = match last_media_sent.lock() {
                                Ok(guard) => *guard,
                                Err(poisoned) => *poisoned.into_inner(),
                            };
                            last.elapsed() >= sparse_interval_dur
                        };
                        if should_send {
                            let mut guard = writer.lock().await;
                            if let Some(w) = guard.as_mut() {
                                let msg = Message::text(realtime_audio_message(&SPARSE_KEEPALIVE_SILENCE));
                                match send_bounded(w, msg, "keep-alive").await {
                                    Ok(()) => {
                                        if let Ok(mut last) = last_media_sent.lock() {
                                            *last = tokio::time::Instant::now();
                                        }
                                        tracing::debug!("gemini sparse media keep-alive sent (100ms silence)");
                                    }
                                    Err(err) => {
                                        tracing::debug!(err = %err.message, "failed to send sparse keep-alive; recycling the connection");
                                        break 'msg;
                                    }
                                }
                            }
                        }
                    }
                    continue 'msg;
                }
            };
            // The Live API delivers its JSON in text frames, but real servers have
            // shipped JSON in binary frames too — accept both, skip the rest.
            let text = match message {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                    Ok(text) => text,
                    Err(_) => continue,
                },
                _ => continue,
            };
            // Smoke-rig frame dump (`SMOKE_FRAME_DUMP=1`, hand-dispatched live
            // smokes only): raw server frames to stdout, capped per frame.
            // Debug builds only — frames carry verbatim session content, and a
            // release binary must have no env-triggered dump of it; the env is
            // read once.
            #[cfg(debug_assertions)]
            {
                static FRAME_DUMP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                if *FRAME_DUMP.get_or_init(|| std::env::var("SMOKE_FRAME_DUMP").is_ok()) {
                    println!("FRAME: {}", text.chars().take(400).collect::<String>());
                }
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            // Context-diet telemetry (2026-09-24): the server reports exact
            // prompt token counts on its messages
            // (BidiGenerateContentServerMessage.usageMetadata.promptTokenCount
            // — https://ai.google.dev/api/live, checked 2026-09-24). Logged
            // only — long-session context growth becomes visible per turn
            // without keying any behavior on it (the honest fix for the
            // estimated-char-ratio blind spot).
            if let Some(prompt_tokens) = value
                .get("usageMetadata")
                .and_then(|usage| usage.get("promptTokenCount"))
                .and_then(|count| count.as_i64())
            {
                tracing::debug!(prompt_tokens, "gemini prompt token count");
            }
            // Session-resumption bookkeeping (owner-approved 2026-09-24;
            // ai.google.dev/api/live — `SessionResumptionUpdate`, checked
            // 2026-09-24): every non-empty `newHandle` the server issues is
            // the state a recycle's redial may resume. `resumable: false`
            // with an empty handle means the server cannot snapshot now —
            // the last good handle stays; a stale one falls back to a
            // fresh dial at recycle time.
            if let Some(update) = value.get("sessionResumptionUpdate") {
                let new_handle = update
                    .get("newHandle")
                    .and_then(|raw| raw.as_str())
                    .filter(|handle| !handle.is_empty());
                if let Some(handle) = new_handle {
                    if let Ok(mut stored) = redial.resumption_handle.lock() {
                        *stored = Some(handle.to_string());
                    }
                    tracing::debug!("gemini session resumption handle issued");
                }
            }
            if let Some(go_away) = value.get("goAway") {
                let time_left = go_away
                    .get("timeLeft")
                    .and_then(|raw| raw.as_str())
                    .and_then(parse_proto_duration);
                // Phase first, event second: whoever observes `Renewing` is
                // guaranteed the send-side rejections are already in force.
                let renewal_started = std::time::Instant::now();
                tracing::info!(time_left = ?time_left, "gemini goAway, renewing");
                phase.store(PHASE_RENEWING, Ordering::SeqCst);
                if events
                    .send(ChannelEvent::Lifecycle(LifecycleEvent::Renewing {
                        time_left,
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
                // The recycle, in place. Close the old connection first: its write
                // half comes out of the shared slot (sends are already rejected
                // through the phase gate, so nothing races the empty slot), and the
                // close frame lets the provider retire the old session before the
                // replacement dial — the fixture server holds adapters to that
                // order by accepting connections strictly sequentially.
                let old_writer = writer.lock().await.take();
                if let Some(mut old_writer) = old_writer {
                    let _ = timeout(CLOSE_JOIN_BUDGET, old_writer.close()).await;
                }
                // The renewal redial reuses the session's own key verbatim —
                // never the rotation ring (see ActiveKey). A key-specific
                // refusal here still marks the slot's health so the NEXT start
                // skips it, but this session expires exactly as before.
                // Resumption first when the server issued a handle — the
                // fallback inside [`renew_dial`] is today's plain dial.
                let (socket, resumed) = match renew_dial(&redial).await {
                    Ok(dialed) => dialed,
                    Err(err) => {
                        tracing::warn!(
                            seam = "wire",
                            reason = ?err.kind,
                            close_code = close_code_in(&err.message),
                            "gemini renewal redial failed, session expiring"
                        );
                        if let Some(account) = &redial.active.account {
                            match err.kind {
                                ErrorKind::Auth => redial
                                    .keys
                                    .record_dial_auth_failure(account, close_code_in(&err.message)),
                                ErrorKind::RateLimited => redial.keys.record_dial_quota(
                                    account,
                                    close_code_in(&err.message),
                                    retry_after_secs_in(&err.message),
                                ),
                                _ => {}
                            }
                        }
                        // No replacement connection: the session ended without
                        // recovery. Terminal phase first, event second — whoever
                        // observes `Expired` is guaranteed sends already fail
                        // `InvalidState`, non-retryable (conformance check 9).
                        phase.store(PHASE_EXPIRED, Ordering::SeqCst);
                        let _ = events
                            .send(ChannelEvent::Lifecycle(LifecycleEvent::Expired))
                            .await;
                        return;
                    }
                };
                let (new_writer, new_reader) = socket.split();
                *writer.lock().await = Some(new_writer);
                // Hand over: this same task carries on as the new connection's
                // read loop (the old read half drops here, ending the old socket),
                // so the JoinHandle `close` joins keeps tracking the live loop.
                reader = new_reader;
                // The replacement connection starts between turns: whatever turn
                // the old connection had open died with it.
                turn_gen = None;
                utterance_committed = false;
                // A transcription cycle interrupted by the recycle finalizes
                // never; re-arm so the replacement socket's first interim
                // announces a FRESH cycle instead of hiding behind the dead one.
                interim_signalled = false;
                connected_at = std::time::Instant::now();
                if let Ok(mut last) = last_media_sent.lock() {
                    *last = tokio::time::Instant::now();
                }
                ping_interval.reset();
                sparse_keepalive_interval.reset();
                send_stall.clear();
                // Phase first, event second again: whoever observes `Reconnected`
                // finds sends accepted.
                tracing::info!(
                    elapsed_ms = renewal_started.elapsed().as_millis() as u64,
                    "gemini reconnected"
                );
                phase.store(PHASE_LIVE, Ordering::SeqCst);
                if events
                    .send(ChannelEvent::Lifecycle(LifecycleEvent::Reconnected {
                        resumed,
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
                continue 'msg;
            }
            if let Some(tool_call) = value.get("toolCall") {
                let Some(calls) = tool_call
                    .get("functionCalls")
                    .and_then(|calls| calls.as_array())
                else {
                    continue;
                };
                for call in calls {
                    let (Some(id), Some(name)) = (
                        call.get("id").and_then(|id| id.as_str()),
                        call.get("name").and_then(|name| name.as_str()),
                    ) else {
                        continue;
                    };
                    let args = call
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    // The first surfaced call of a fresh turn commits the
                    // utterance (#201) — before the `ToolCall` itself, so the
                    // pump re-arms its snapshot before judging the call.
                    if !utterance_committed {
                        utterance_committed = true;
                        if events.send(ChannelEvent::UtteranceEnd).await.is_err() {
                            return;
                        }
                    }
                    pending_tool_calls
                        .lock()
                        .unwrap()
                        .insert(id.to_string(), name.to_string());
                    if events
                        .send(ChannelEvent::ToolCall {
                            call_id: id.to_string(),
                            name: name.to_string(),
                            args,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                continue;
            }
            if let Some(cancellation) = value.get("toolCallCancellation") {
                let Some(ids) = cancellation.get("ids").and_then(|ids| ids.as_array()) else {
                    continue;
                };
                let mut pending = pending_tool_calls.lock().unwrap();
                let mut cancelled = cancelled_tool_calls.lock().unwrap();
                for id in ids.iter().filter_map(|id| id.as_str()) {
                    pending.remove(id);
                    cancelled.insert(id.to_string());
                }
                continue;
            }
            let Some(server_content) = value.get("serverContent") else {
                continue;
            };
            if server_content
                .get("waitingForInput")
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                tracing::debug!(gen = turn_gen, "gemini server waiting for more input");
            }
            // A turn opens on its first `modelTurn` content, and the snapshot is
            // taken exactly once: the whole response is live or stale as a unit,
            // however late its tail frames are read.
            if turn_gen.is_none() && server_content.get("modelTurn").is_some() {
                turn_gen = Some(generation.current());
                tracing::debug!(gen = generation.current(), "gemini turn open");
                // An audio-led turn open commits the utterance too (#201), unless
                // a surfaced `toolCall` of the same turn already did.
                if !utterance_committed {
                    utterance_committed = true;
                    if events.send(ChannelEvent::UtteranceEnd).await.is_err() {
                        return;
                    }
                }
            }
            for pcm in server_content_audio(server_content) {
                let gen = turn_gen.expect("audio parts ride modelTurn, which opened the turn");
                tracing::trace!(gen, frames = pcm.len(), "gemini audio delta");
                if events
                    .send(ChannelEvent::AudioDelta { pcm, gen })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // Transcription-mode existence signal (Kritias round 3 finding
            // 3): the FIRST interim hypothesis of each finalization cycle
            // becomes one `TranscriptActivity` — proof the server is hearing
            // THIS utterance, re-armed when its final is forwarded below.
            // The interim TEXT stays deliberately unread (speculative by
            // contract).
            if transcribe_mode
                && server_content.get("interimInputTranscription").is_some()
                && !interim_signalled
            {
                interim_signalled = true;
                finalize_cycle += 1;
                if events
                    .send(ChannelEvent::TranscriptActivity {
                        cycle: finalize_cycle,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // Both transcription directions ride `serverContent`, each a
            // `BidiGenerateContentTranscription { text }`: `inputTranscription`
            // narrates the user, `outputTranscription` the model
            // (https://ai.google.dev/api/live, accessed 2026-08-21).
            for (field, role) in [
                ("inputTranscription", Role::User),
                ("outputTranscription", Role::Assistant),
            ] {
                if let Some(delta) = server_content
                    .get(field)
                    .and_then(|transcription| transcription.get("text"))
                    .and_then(|text| text.as_str())
                {
                    if field == "inputTranscription" {
                        // Kritias round 4: EVERY user final is preceded by
                        // its cycle's activity — a cycle whose final arrived
                        // with no interim (short utterances can finalize
                        // without a hypothesis frame) announces itself here,
                        // so the ledger always knows which cycle this final
                        // closes before it reads the text.
                        if transcribe_mode && !interim_signalled {
                            finalize_cycle += 1;
                            if events
                                .send(ChannelEvent::TranscriptActivity {
                                    cycle: finalize_cycle,
                                })
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        interim_signalled = false;
                    }
                    tracing::trace!(role = ?role, len = delta.len(), "gemini transcript delta");
                    if events
                        .send(ChannelEvent::TranscriptDelta {
                            role,
                            text: delta.to_string(),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            // The wire's turn delimiters retire the open turn — and its snapshot
            // — after any content the same message carried: `interrupted` when
            // the server's VAD cut the response, `turnComplete` when it finished
            // (https://ai.google.dev/api/live, accessed 2026-08-21). The next
            // `modelTurn` snapshots afresh.
            // Only a reply that opened (surfaced model content or a call)
            // completes: a delimiter on transcription-only frames closes
            // nothing the harness saw.
            let opened = turn_gen.is_some() || utterance_committed;
            let mut delimited = false;
            for field in ["interrupted", "turnComplete"] {
                if server_content.get(field).and_then(|v| v.as_bool()) == Some(true) {
                    tracing::debug!(gen = turn_gen, which = field, "gemini turn delimiter");
                    turn_gen = None;
                    utterance_committed = false;
                    delimited = true;
                }
            }
            // The reply is over — finished or cut — and no more of it will
            // stream. Say so (`ChannelEvent::TurnComplete`, event.rs: "no
            // more deltas follow and no tool call is pending"), unless a
            // surfaced call still awaits its answer: the continuation after
            // that answer is the same reply. Without this the harness never
            // learned a voice reply ended: the panel stayed on "writing" and
            // the journal row waited for the user's next utterance (review
            // 2026-10-09, J2).
            if delimited
                && opened
                && pending_tool_calls.lock().unwrap().is_empty()
                && events.send(ChannelEvent::TurnComplete).await.is_err()
            {
                return;
            }
        }
        // The socket ended WITHOUT a goAway — an abrupt loss (R2-3, owner
        // feedback 2026-08-30): provider hard connection caps (the
        // transcribe-live 10-minute continuous-streaming cap,
        // https://ai.google.dev/gemini-api/docs/live-api/live-transcribe,
        // checked 2026-08-30) and transport failures land here. The old
        // silent return made the loss invisible: no event fired (the channel
        // keeps its own sender clone, so the receiver never closed) and every
        // later send failed at debug level while the session limped deaf.
        // Recycle exactly like a goAway — Renewing first (the phase gate
        // rejects sends retryably), bounded redial, Reconnected on success —
        // and Expired only when the redial truly fails. An intentional close
        // (the `closing` flag), an already-expired session, and a torn-down
        // consumer never recycle.
        if closing.load(Ordering::SeqCst)
            || phase.load(Ordering::SeqCst) == PHASE_EXPIRED
            || events.is_closed()
        {
            return;
        }
        // Crash-loop guard: replacements that keep dying young mean the
        // provider is refusing this session's shape — expire honestly rather
        // than dial forever.
        if connected_at.elapsed() < ABRUPT_SHORT_LIFE {
            short_lives += 1;
        } else {
            short_lives = 0;
        }
        if short_lives > ABRUPT_SHORT_LIFE_LIMIT {
            tracing::warn!(
                seam = "wire",
                short_lives,
                "gemini connection crash-looping after abrupt losses, session expiring"
            );
            phase.store(PHASE_EXPIRED, Ordering::SeqCst);
            let _ = events
                .send(ChannelEvent::Lifecycle(LifecycleEvent::Expired))
                .await;
            return;
        }
        let renewal_started = std::time::Instant::now();
        tracing::warn!(
            seam = "wire",
            lived_ms = connected_at.elapsed().as_millis() as u64,
            "gemini socket ended without goAway; attempting recycle"
        );
        phase.store(PHASE_RENEWING, Ordering::SeqCst);
        if events
            .send(ChannelEvent::Lifecycle(LifecycleEvent::Renewing {
                time_left: None,
            }))
            .await
            .is_err()
        {
            // Review round F4: a torn-down consumer still deserves phase
            // truth — the goAway path's failure exits leave Expired, so
            // does this one (a stray sender must see non-retryable, not
            // renewing-forever).
            phase.store(PHASE_EXPIRED, Ordering::SeqCst);
            return;
        }
        let old_writer = writer.lock().await.take();
        if let Some(mut old_writer) = old_writer {
            let _ = timeout(CLOSE_JOIN_BUDGET, old_writer.close()).await;
        }
        // Same key custody as the goAway recycle: the session's own key
        // verbatim, never the rotation ring; refusals still mark slot health.
        let mut dialed = None;
        let mut abrupt_resumed = false;
        for (attempt, delay_ms) in ABRUPT_REDIAL_BACKOFF_MS.iter().enumerate() {
            if *delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
            }
            // Review round F2: a close() racing the backoff must win — no
            // stray billable dial after the user already left.
            if closing.load(Ordering::SeqCst) || events.is_closed() {
                return;
            }
            // Resumption first (the [`renew_dial`] fallback is today's plain
            // dial); the loop keeps its backoff for total-dial failures.
            match renew_dial(&redial).await {
                Ok((socket, resumed)) => {
                    abrupt_resumed = resumed;
                    dialed = Some(socket);
                    break;
                }
                Err(err) => {
                    tracing::warn!(
                        seam = "wire",
                        attempt,
                        reason = ?err.kind,
                        close_code = close_code_in(&err.message),
                        "gemini abrupt-loss redial failed"
                    );
                    if let Some(account) = &redial.active.account {
                        match err.kind {
                            ErrorKind::Auth => redial
                                .keys
                                .record_dial_auth_failure(account, close_code_in(&err.message)),
                            ErrorKind::RateLimited => redial.keys.record_dial_quota(
                                account,
                                close_code_in(&err.message),
                                retry_after_secs_in(&err.message),
                            ),
                            _ => {}
                        }
                    }
                }
            }
        }
        let Some(socket) = dialed else {
            phase.store(PHASE_EXPIRED, Ordering::SeqCst);
            let _ = events
                .send(ChannelEvent::Lifecycle(LifecycleEvent::Expired))
                .await;
            return;
        };
        // Review round F2, the install edge: a close() that landed after the
        // successful dial must not have the recycled writer re-fill the slot
        // it just emptied (close() takes the writer lock after setting the
        // flag, so either order leaves the socket closed).
        if closing.load(Ordering::SeqCst) {
            return;
        }
        let (new_writer, new_reader) = socket.split();
        *writer.lock().await = Some(new_writer);
        reader = new_reader;
        turn_gen = None;
        utterance_committed = false;
        // As in the goAway recycle: the interrupted transcription cycle is
        // dead; the replacement's first interim opens a fresh one.
        interim_signalled = false;
        connected_at = std::time::Instant::now();
        if let Ok(mut last) = last_media_sent.lock() {
            *last = tokio::time::Instant::now();
        }
        tracing::info!(
            elapsed_ms = renewal_started.elapsed().as_millis() as u64,
            "gemini reconnected after abrupt loss"
        );
        phase.store(PHASE_LIVE, Ordering::SeqCst);
        if events
            .send(ChannelEvent::Lifecycle(LifecycleEvent::Reconnected {
                resumed: abrupt_resumed,
            }))
            .await
            .is_err()
        {
            return;
        }
        continue 'conn;
    }
}

#[async_trait]
impl SessionChannel for GeminiLiveChannel {
    async fn connect(&mut self, ctx: SessionContext) -> Result<(), ChannelError> {
        if self.state != SessionState::New {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("connect on a channel in state {:?}", self.state),
            ));
        }

        // Rendered once from the context and reused verbatim by every renewal
        // redial: the renewed connection must carry the same instruction and
        // tool vocabulary as the one it replaces, or the dispatcher's view of
        // the session desynchronizes from the provider's (registry doc: the
        // tool list is immutable for the session's lifetime).
        #[cfg(feature = "book-agent")]
        {
            // Remember which tools this session declared NON_BLOCKING: only
            // their answers carry a `scheduling` field. Keyed on the actual
            // declaration, not the name — the reused playback verbs are
            // async in Agent sessions and blocking in Tutor sessions, and a
            // Tutor answer's wire shape must stay byte-identical.
            self.nonblocking_tools = ctx
                .tool_schemas
                .iter()
                .filter(|s| s["function"]["behavior"].as_str() == Some("NON_BLOCKING"))
                .filter_map(|s| s["function"]["name"].as_str().map(str::to_string))
                .collect();
        }
        let setup_message = self.setup_message(&ctx);
        let (socket, active) = if self.candidates.is_empty() {
            // Single-key path (fixtures, direct constructions): the VA3
            // lookup, once — and the resolved key is still remembered so
            // renewal reuses it instead of re-resolving mid-session.
            let key = self
                .keys
                .session_key(GEMINI_KEY_BASE_URL)
                .await
                .map_err(|err| {
                    ChannelError::new(
                        ErrorKind::Auth,
                        false,
                        format!("no usable Gemini key: {err}"),
                    )
                })?;
            let socket = Self::dial_and_setup(self.endpoint(), &key, &setup_message).await?;
            (socket, ActiveKey { account: None, key })
        } else {
            Self::dial_first_usable(
                self.endpoint(),
                &self.keys,
                &self.candidates,
                &setup_message,
            )
            .await?
        };

        let tx = self
            .events_tx
            .clone()
            .expect("a New channel still holds its construction-time sender");
        let _ = tx.try_send(ChannelEvent::Lifecycle(LifecycleEvent::Connected));
        let (writer, reader) = socket.split();
        self.reader = Some(tokio::spawn(read_loop(
            reader,
            Redial {
                endpoint: self.endpoint().to_string(),
                keys: Arc::clone(&self.keys),
                setup_message,
                active,
                resumption_handle: Arc::clone(&self.resumption_handle),
            },
            Arc::clone(&self.writer),
            tx,
            self.generation.clone(),
            Arc::clone(&self.pending_tool_calls),
            Arc::clone(&self.cancelled_tool_calls),
            Arc::clone(&self.phase),
            Arc::clone(&self.closing),
            self.transcribe_mode,
            Arc::clone(&self.last_media_sent),
            self.ping_interval,
            self.sparse_keepalive_interval,
            Arc::clone(&self.send_stall),
        )));
        *self.writer.lock().await = Some(writer);
        self.state = SessionState::Connected;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), ChannelError> {
        if self.state == SessionState::Closed {
            return Ok(());
        }
        self.state = SessionState::Closed;
        // Before the socket close: the read loop must see the intent, or it
        // would treat our own close as an abrupt loss and redial (R2-3).
        self.closing.store(true, Ordering::SeqCst);
        self.events_tx = None;
        let writer = self.writer.lock().await.take();
        if let Some(mut writer) = writer {
            // Best-effort clean shutdown: the sink close sends the close frame;
            // the read loop drains until the peer acknowledges and exits. A
            // peer that already vanished is not a close failure.
            let _ = timeout(CLOSE_JOIN_BUDGET, writer.close()).await;
        }
        if let Some(mut reader) = self.reader.take() {
            if timeout(CLOSE_JOIN_BUDGET, &mut reader).await.is_err() {
                reader.abort();
            }
        }
        Ok(())
    }

    async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_audio on a channel in state {:?}", self.state),
            ));
        }
        self.phase_gate("send_audio")?;
        let send_started = std::time::Instant::now();
        let sent = self
            .send_frame(Message::text(realtime_audio_message(pcm_16k_mono)), "audio")
            .await;
        if sent.is_ok() {
            if let Ok(mut last) = self.last_media_sent.lock() {
                *last = tokio::time::Instant::now();
            }
        }
        tracing::trace!(
            frames = pcm_16k_mono.len(),
            send_ms = send_started.elapsed().as_millis() as u64,
            ok = sent.is_ok(),
            "gemini realtime audio sent"
        );
        sent
    }

    async fn finalize_input(&mut self) -> Result<(), ChannelError> {
        self.send_audio_stream_end().await
    }

    /// One page still as a realtime video frame — the documented mid-session
    /// image path ("Video frames are sent as individual images (e.g., JPEG
    /// or PNG) at a specific frame rate (max 1 frame per second)",
    /// https://ai.google.dev/gemini-api/docs/live-guide, checked 2026-08-30).
    /// Deliberately NOT `clientContent`: that mechanism is restricted on
    /// newer Live models, this one is not.
    async fn send_image(&mut self, image: &[u8], mime: &str) -> Result<(), ChannelError> {
        self.phase_gate("send_image")?;
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_image on a channel in state {:?}", self.state),
            ));
        }
        self.send_frame(Message::text(realtime_image_message(image, mime)), "image")
            .await
    }

    async fn send_text(&mut self, text: &str) -> Result<(), ChannelError> {
        // The phase gate stays first (m1 pinned this ordering): a mid-renewal
        // text turn must fail retryably, whatever else is wrong.
        self.phase_gate("send_text")?;
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_text on a channel in state {:?}", self.state),
            ));
        }
        self.send_frame(Message::text(client_content_text_message(text)), "text")
            .await
    }

    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: ToolResult,
    ) -> Result<(), ChannelError> {
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_tool_result on a channel in state {:?}", self.state),
            ));
        }
        // Gated before the pending lookup: a mid-renewal rejection leaves the
        // call pending, so the harness's post-renewal retry finds its name.
        self.phase_gate("send_tool_result")?;
        let pending_name = self
            .pending_tool_calls
            .lock()
            .unwrap()
            .get(call_id)
            .cloned();
        let Some(name) = pending_name else {
            // A `toolCallCancellation` retired this call: the server retracted
            // it, so silence — not a `FunctionResponse`, not an error — is the
            // correct answer to a result that finished executing too late.
            if self.cancelled_tool_calls.lock().unwrap().contains(call_id) {
                return Ok(());
            }
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("tool result for unknown or already-answered call {call_id:?}"),
            ));
        };
        #[cfg(feature = "book-agent")]
        let nonblocking = self.nonblocking_tools.contains(&name);
        #[cfg(not(feature = "book-agent"))]
        let nonblocking = false;
        self.send_frame(
            Message::text(tool_response_message(call_id, &name, &result, nonblocking)),
            "tool response",
        )
        .await?;
        // Answered exactly once: only a delivered response retires the call —
        // a transport failure above leaves it pending for the retry.
        self.pending_tool_calls.lock().unwrap().remove(call_id);
        Ok(())
    }

    async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError> {
        // The phase gate stays first (m1 pinned this ordering): a mid-renewal
        // refresh must fail retryably, whatever else is wrong.
        self.phase_gate("update_context")?;
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("update_context on a channel in state {:?}", self.state),
            ));
        }
        let Some(message) = client_content_context_message(&update) else {
            return Ok(());
        };
        self.send_frame(Message::text(message), "context update")
            .await
    }

    async fn interrupt(&mut self) -> Result<(), ChannelError> {
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("interrupt on a channel in state {:?}", self.state),
            ));
        }
        // The Live API has no client cancel frame: a client message carries
        // exactly one of `setup`, `clientContent`, `realtimeInput`, or
        // `toolResponse` (https://ai.google.dev/api/live, accessed 2026-08-21
        // — `BidiGenerateContentClientMessage`), and interruption is owned by
        // the server's VAD (`RealtimeInputConfig.automaticActivityDetection`,
        // default `START_OF_ACTIVITY_INTERRUPTS`). So the "network half" of
        // barge-in is empty here and the generation bump IS the cancel: every
        // remaining delta of the superseded response carries the dead gen and
        // dies at the playback gate. Bumping locally keeps `interrupt`
        // self-sufficient; through [`super::barge_in`] it is a second bump,
        // which is harmless — the gate is a staleness comparison, not a step
        // count.
        self.generation.bump();
        Ok(())
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
        self.events.take()
    }

    fn tool_call_retracted(&self, call_id: &str) -> bool {
        self.cancelled_tool_calls.lock().unwrap().contains(call_id)
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            // The Live API detects turns server-side; the client VAD is at
            // most a cost optimizer (VOICE_AGENT §3.1 double-VAD rule).
            vad_mode: VadMode::Server,
            // Gemini transcript deltas are not word-aligned: the harness
            // degrades follow-along to sentence-level (VOICE_AGENT §11).
            word_timestamps: false,
            // The Live API announces end-of-session ahead of time (`goAway`),
            // which this adapter surfaces as `Renewing` and recycles — the
            // uniform renewal contract (VOICE_AGENT §3, §9).
            session_renewal: true,
            text_only: false,
            stt_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::voice::channel::fixture::{load_fixture, replay, WireFrame, WireHandle, WireServer};
    use crate::voice::channel::DelimitedExcerpt;
    use crate::voice::keys::store::{Backend, KeyStore};
    use crate::voice::keys::ApiKey;

    /// Deliberately echoed in the fixture's `absent` needles: no frame the
    /// adapter sends may carry the key.
    const FIXTURE_KEY: &str = "fixture-gemini-key-7d2f";

    fn keys_with_gemini_key() -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(GEMINI_KEY_BASE_URL, &ApiKey::new(FIXTURE_KEY.into()))
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

    /// Issue #200: the setup message must carry the whole [`SessionContext`]
    /// — the system instruction (prompt, delimited excerpt, rolling summary,
    /// one part each, in that order) and every session tool as a
    /// `functionDeclarations[]` entry in the Live API's own schema dialect.
    /// Investigation 2026-10-09 §3.4: the code-switch fragment never
    /// reaches the Live wire — neither half. A `clientContent` frame
    /// interrupts generation, and the retire half fires on the reply's first
    /// delta; the native-audio model does not read its own transcription.
    #[test]
    fn code_switch_fragment_never_reaches_the_live_wire() {
        for active in [true, false] {
            assert_eq!(
                client_content_context_message(&ContextUpdate::CodeSwitchFragment {
                    terms: vec!["page".to_string(), "scalability".to_string()],
                    active,
                }),
                None,
                "active={active}: the fragment must not be framed for the wire"
            );
        }
    }

    #[test]
    fn setup_message_carries_the_session_context_onto_the_wire() {
        let channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: None,
                model: "models/gemini-2.5-flash-native-audio-preview-12-2025".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );
        let mut context = ctx();
        context.rolling_summary = Some("earlier chapters, summarized".into());
        context.tool_schemas = crate::voice::tools::registry::ToolRegistry::new(true).schemas();
        let setup: serde_json::Value =
            serde_json::from_str(&channel.setup_message(&context)).unwrap();
        let setup = &setup["setup"];

        let parts = setup["systemInstruction"]["parts"].as_array().unwrap();
        assert_eq!(
            parts
                .iter()
                .map(|part| part["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "You are the reading tutor.",
                "<<x>>excerpt<</x>>",
                "earlier chapters, summarized",
            ],
            "prompt, delimited excerpt, rolling summary — one part each, in order"
        );

        assert_eq!(
            setup["generationConfig"]["thinkingConfig"]["thinkingBudget"], 0,
            "owner ruling 2026-08-29: the live voice loop never thinks — \
             dynamic thinking is the provider default and reads as dead air"
        );
        assert_eq!(
            setup["inputAudioTranscription"]["languageCodes"],
            serde_json::json!(["ar-EG", "en-US"]),
            "F4: the transcriber gets the rig's BCP-47 hints, never bare \
             auto-detect"
        );
        assert_eq!(
            setup["inputAudioTranscription"]["customVocabulary"],
            serde_json::json!(["excerpt"]),
            "F4: recognition is biased toward the excerpt's Latin terms — \
             words the wire already carries, never new book text"
        );
        assert_eq!(
            setup["outputAudioTranscription"],
            serde_json::json!({}),
            "output transcription stays the plain on switch"
        );

        let vad = &setup["realtimeInputConfig"]["automaticActivityDetection"];
        assert_eq!(
            vad["startOfSpeechSensitivity"], "START_SENSITIVITY_LOW",
            "false barge-ins from residual echo need the LOW start threshold"
        );
        assert_eq!(
            vad["endOfSpeechSensitivity"], "END_SENSITIVITY_LOW",
            "owner journal 04:59: HIGH end sensitivity truncated a question mid-utterance"
        );
        assert_eq!(vad["prefixPaddingMs"], 300);
        assert_eq!(vad["silenceDurationMs"], 800);
        let declarations = setup["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap();
        // Owner-approved capability F8-D1b grew the roster by one
        // (get_context_for_range).
        assert_eq!(
            declarations.len(),
            16,
            "every offered tool is declared, teacher_anchor and look_at_page included"
        );
        let by_name = |name: &str| -> &serde_json::Value {
            declarations
                .iter()
                .find(|declaration| declaration["name"] == name)
                .unwrap_or_else(|| panic!("{name} is declared"))
        };
        assert!(
            by_name("pause_playback").get("parameters").is_none(),
            "a no-argument tool omits `parameters` — the wire rejects an \
             OBJECT schema with no properties"
        );
        let seek = &by_name("seek_relative")["parameters"];
        assert_eq!(seek["type"], "OBJECT", "raw-wire enum values are uppercase");
        assert_eq!(seek["properties"]["seconds"]["type"], "NUMBER");
        assert_eq!(seek["properties"]["seconds"]["minimum"], -600.0);
        assert_eq!(seek["properties"]["seconds"]["maximum"], 600.0);
        assert_eq!(seek["required"], serde_json::json!(["seconds"]));
        let anchor = &by_name("teacher_anchor")["parameters"];
        assert_eq!(
            anchor["properties"]["kind"]["enum"],
            serde_json::json!(["figure", "equation", "table"]),
            "the live wire must not advertise section anchors (#205 descope)"
        );
        let rendered = channel.setup_message(&context);
        assert!(
            !rendered.contains("additionalProperties") && !rendered.contains("minLength"),
            "JSON-Schema strictness vocabulary the proto does not know is \
             dropped in translation"
        );
        assert!(
            !rendered.contains(FIXTURE_KEY),
            "the setup frame stays credential-free"
        );
    }

    /// The 2026-09-24 migration contract, pinned on the CURRENT tutor pin:
    /// the 3.8-live setup frame omits thinking entirely (interleaved
    /// reasoning is native; thinking_config is unsupported and must be
    /// omitted — migration guide), pins `behavior: BLOCKING` on every
    /// declaration that does not opt out explicitly (3.8's new default is
    /// NON_BLOCKING; the tutor harness's turn semantics are blocking), and
    /// enables server-side sliding-window compression so long sessions
    /// cannot bloat server history (context diet). Proactivity is never
    /// sent (3.8: permanently enabled; `false` errors).
    #[test]
    fn the_current_tutor_pin_setup_omits_thinking_pins_blocking_and_compresses() {
        let channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: None,
                model: crate::model_pins::TUTOR_LIVE_MODEL.to_string(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );
        let mut context = ctx();
        context.tool_schemas = crate::voice::tools::registry::ToolRegistry::new(true).schemas();
        let setup: serde_json::Value =
            serde_json::from_str(&channel.setup_message(&context)).unwrap();
        let setup = &setup["setup"];
        assert_eq!(setup["model"], crate::model_pins::TUTOR_LIVE_MODEL);
        assert!(
            setup["generationConfig"].get("thinkingConfig").is_none(),
            "3.8-live: omit thinking_config entirely — interleaved reasoning \
             is native: {}",
            setup["generationConfig"]
        );
        assert_eq!(
            setup["contextWindowCompression"]["slidingWindow"]["targetTokens"],
            crate::voice::channel::gemini::LIVE_COMPRESSION_TARGET_TOKENS,
            "server-side compression keeps long-session history bounded"
        );
        assert!(
            setup.get("proactivity").is_none(),
            "proactive audio is permanently enabled on 3.8 — sending the \
             field (especially false) errors"
        );
        let declarations = setup["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap();
        assert!(
            declarations
                .iter()
                .all(|declaration| declaration["behavior"] == "BLOCKING"),
            "every tutor declaration pins BLOCKING against the 3.8 \
             NON_BLOCKING default"
        );
        assert_eq!(
            setup["realtimeInputConfig"]["turnCoverage"], "TURN_INCLUDES_ALL_INPUT",
            "the owner-tunable coverage pin rides the 3.8 frame unchanged"
        );
    }

    /// Zero-regression pin for the AGENT channel (still the 2.5 family):
    /// thinkingBudget stays 0, no compression field is sent to the legacy
    /// family, and the agent's explicitly-NON_BLOCKING declarations are
    /// preserved verbatim.
    #[test]
    fn the_agent_pin_setup_keeps_the_legacy_2_5_wire_contract() {
        let channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: None,
                model: crate::model_pins::AGENT_LIVE_MODEL.to_string(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );
        let mut context = ctx();
        context.tool_schemas = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "agent_tool",
                "behavior": "NON_BLOCKING",
                "parameters": {"type": "object", "properties": {}}
            }
        })];
        let setup: serde_json::Value =
            serde_json::from_str(&channel.setup_message(&context)).unwrap();
        let setup = &setup["setup"];
        assert_eq!(
            setup["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            crate::model_pins::LIVE_THINKING_BUDGET,
            "the 2.5 family keeps its thinking-off pin"
        );
        assert!(
            setup.get("contextWindowCompression").is_none(),
            "the 3.8-era compression field is never sent to the legacy family"
        );
        assert_eq!(
            setup["tools"][0]["functionDeclarations"][0]["behavior"], "NON_BLOCKING",
            "an explicitly-declared behavior is never overridden"
        );
    }

    /// The transcription-only setup frame, pinned to the documented shape
    /// (ai.google.dev/gemini-api/docs/live-api/live-transcribe, checked
    /// 2026-08-29): TEXT modality + steered transcription, and NOTHING of
    /// the dialog frame — no tools, no system instruction, no thinking, no
    /// VAD tuning.
    #[test]
    fn transcribe_mode_builds_the_lean_text_frame() {
        let channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: None,
                model: "models/gemini-3.5-transcribe-live".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        )
        .with_transcribe_mode();
        let mut context = ctx();
        context.tool_schemas = crate::voice::tools::registry::ToolRegistry::new(true).schemas();
        let setup: serde_json::Value =
            serde_json::from_str(&channel.setup_message(&context)).unwrap();
        let setup = &setup["setup"];
        assert_eq!(setup["model"], "models/gemini-3.5-transcribe-live");
        assert_eq!(
            setup["generationConfig"],
            serde_json::json!({ "responseModalities": ["TEXT"] }),
            "TEXT modality and nothing else — no thinking config on a \
             transcriber"
        );
        assert_eq!(
            setup["inputAudioTranscription"]["languageCodes"],
            serde_json::json!(["ar-EG", "en-US"]),
            "the same rig hints as the dialog session"
        );
        assert_eq!(
            setup["inputAudioTranscription"]["customVocabulary"],
            serde_json::json!(["excerpt"]),
            "the same excerpt-term bias as the dialog session"
        );
        for absent in [
            "tools",
            "systemInstruction",
            "realtimeInputConfig",
            "outputAudioTranscription",
        ] {
            assert!(
                setup.get(absent).is_none(),
                "{absent} must not ride the transcriber's frame"
            );
        }
    }

    /// The other direction of the same rule: a context with nothing to say
    /// (no prompt, no excerpt, no summary, no tools) adds neither field, so
    /// the frame stays the VA5 minimal setup.
    #[test]
    fn an_empty_session_context_adds_nothing_to_the_setup_frame() {
        let channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: None,
                model: "models/gemini-2.5-flash-native-audio-preview-12-2025".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );
        let mut context = ctx();
        context.system_prompt = String::new();
        context.excerpt = DelimitedExcerpt {
            delimited_text: String::new(),
        };
        let setup: serde_json::Value =
            serde_json::from_str(&channel.setup_message(&context)).unwrap();
        assert!(setup["setup"].get("systemInstruction").is_none());
        assert!(setup["setup"].get("tools").is_none());
        assert!(
            setup["setup"]["inputAudioTranscription"]
                .get("customVocabulary")
                .is_none(),
            "no excerpt terms, no vocabulary field — never an empty array"
        );
    }

    #[tokio::test]
    async fn gemini_live_session_lifecycle_fixture_replays_green() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );

        let fixture = load_fixture("gemini-live-session-lifecycle");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");

        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "connect reports Connected only after the provider's setupComplete"
        );
        assert!(
            channel.take_events().is_none(),
            "take_events yields the receiver exactly once"
        );

        let second = channel.connect(ctx()).await;
        assert!(
            matches!(second, Err(ref err) if err.kind == ErrorKind::InvalidState),
            "a second connect is invalid, got {second:?}"
        );

        channel.close().await.expect("clean close");
        channel
            .close()
            .await
            .expect("closing a closed channel is a no-op");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned"
        );
    }

    /// The real provider refuses a bad key by closing the socket during setup
    /// with an explicit close frame (observed live 2026-08-25: code 1007,
    /// reason "API key not valid. Please pass a valid API key."). That reason
    /// is the only actionable fact the user gets — connect must surface it as
    /// an Auth failure, not swallow it into a generic transport drop.
    #[tokio::test]
    async fn a_provider_close_during_setup_surfaces_its_code_and_reason() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );
        let server = handle.clone();
        let closer = tokio::spawn(async move {
            let _setup = server.next_from_adapter().await;
            server.close_current_with(1007, "API key not valid. Please pass a valid API key.");
        });
        let err = channel
            .connect(ctx())
            .await
            .expect_err("a provider close during setup must fail connect");
        closer.await.expect("the closer task ran");
        assert_eq!(
            err.kind,
            ErrorKind::Auth,
            "an explicit key-refusal close is an Auth failure, not transport: {err:?}"
        );
        assert!(
            err.message.contains("API key not valid"),
            "the provider's stated reason is the one actionable fact and must survive: {err}"
        );
        assert!(
            err.message.contains("1007"),
            "the close code anchors the reason to the wire: {err}"
        );
        assert!(!err.retryable, "the same key will be refused again");
    }

    // ---- Key rotation S3: the dial ring (RCA §9) ----

    use crate::voice::keys::KeyState;

    const RING_KEY_A: &str = "ring-key-alpha-11aa";
    const RING_KEY_B: &str = "ring-key-beta-22bb";
    const RING_KEY_C: &str = "ring-key-gamma-33cc";

    fn ring(keys: &[(&str, &str)]) -> Vec<DialCandidate> {
        keys.iter()
            .enumerate()
            .map(|(slot, (label, key))| DialCandidate {
                account: format!("{GEMINI_KEY_BASE_URL}#{slot}"),
                base_url: GEMINI_KEY_BASE_URL.to_string(),
                slot: slot as u32,
                label: Some((*label).to_string()),
                key: ApiKey::new((*key).to_string()),
            })
            .collect()
    }

    fn empty_keys() -> Arc<KeyManager> {
        use crate::voice::keys::store::{Backend, KeyStore};
        Arc::new(KeyManager::with_store(KeyStore::with_forced_backend(
            std::path::PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        )))
    }

    fn ring_channel(
        handle: &WireHandle,
        keys: Arc<KeyManager>,
        candidates: Vec<DialCandidate>,
    ) -> GeminiLiveChannel {
        GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys,
            Generation::new(),
        )
        .with_candidates(candidates)
    }

    /// THE dead-key auto-advance: the provider refuses the first key with the
    /// observed 1007 close, and connect dials the next candidate on its own —
    /// same setup message, next key — succeeding without surfacing an error.
    #[tokio::test]
    async fn a_key_refusal_at_dial_advances_to_the_next_candidate() {
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(
            &handle,
            Arc::clone(&keys),
            ring(&[("KEY_BAD", RING_KEY_A), ("KEY_GOOD", RING_KEY_B)]),
        );
        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup1 = server.next_from_adapter().await;
            server.close_current_with(1007, "API key not valid. Please pass a valid API key.");
            let _setup2 = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
        });
        channel
            .connect(ctx())
            .await
            .expect("the second key carries the start");
        script.await.expect("the fixture script ran");

        let dials = handle.handshakes();
        assert_eq!(dials.len(), 2, "exactly one extra dial, not a storm");
        assert_eq!(dials[0].header("x-goog-api-key"), Some(RING_KEY_A));
        assert_eq!(
            dials[1].header("x-goog-api-key"),
            Some(RING_KEY_B),
            "the advance swapped the key, nothing else"
        );
        assert_eq!(
            keys.state_of(&format!("{GEMINI_KEY_BASE_URL}#0")),
            KeyState::Invalid,
            "the refused slot is marked dead so the next start skips it"
        );
        assert_eq!(
            keys.take_dial_success().as_deref(),
            Some(format!("{GEMINI_KEY_BASE_URL}#1").as_str()),
            "the winning slot leaves the last_used cue"
        );
        channel.close().await.expect("clean close");
    }

    /// The bound and the honesty: a ring of three dead keys fails after
    /// exactly three dials, as Auth non-retryable, naming every tried key by
    /// its label — and never by its bytes.
    #[tokio::test]
    async fn a_ring_of_dead_keys_fails_honestly_after_the_bounded_attempts() {
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(
            &handle,
            Arc::clone(&keys),
            ring(&[
                ("GEMINI_KEY_4", RING_KEY_A),
                ("GEMINI_KEY_5", RING_KEY_B),
                ("GEMINI_KEY_6", RING_KEY_C),
            ]),
        );
        let server = handle.clone();
        let script = tokio::spawn(async move {
            for _ in 0..3 {
                let _setup = server.next_from_adapter().await;
                server.close_current_with(1007, "API key not valid. Please pass a valid API key.");
            }
        });
        let err = channel
            .connect(ctx())
            .await
            .expect_err("three dead keys cannot start a session");
        script.await.expect("the fixture script ran");

        assert_eq!(handle.handshakes().len(), 3, "the ring IS the bound");
        assert_eq!(err.kind, ErrorKind::Auth);
        assert!(!err.retryable);
        for label in ["GEMINI_KEY_4", "GEMINI_KEY_5", "GEMINI_KEY_6"] {
            assert!(
                err.message.contains(label),
                "the honest failure names {label}: {err}"
            );
        }
        assert!(
            err.message.contains("close 1007"),
            "each refusal keeps its close code: {err}"
        );
        for key in [RING_KEY_A, RING_KEY_B, RING_KEY_C] {
            assert!(
                !err.message.contains(key),
                "tried keys are named by label, never by bytes"
            );
        }
        assert_eq!(
            keys.take_dial_success(),
            None,
            "no success cue when nothing dialed through"
        );
    }

    /// A quota refusal is key-specific too: the slot is marked out-of-quota
    /// (with the provider's retryDelay when it ships one) and the ring
    /// advances exactly as for a dead key.
    #[tokio::test]
    async fn a_quota_close_marks_the_slot_out_of_quota_and_advances() {
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(
            &handle,
            Arc::clone(&keys),
            ring(&[("KEY_DRAINED", RING_KEY_A), ("KEY_FRESH", RING_KEY_B)]),
        );
        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup1 = server.next_from_adapter().await;
            server.close_current_with(
                1011,
                "You exceeded your current quota. {\"retryDelay\":\"30s\"}",
            );
            let _setup2 = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
        });
        channel
            .connect(ctx())
            .await
            .expect("the fresh key carries the start");
        script.await.expect("the fixture script ran");

        assert_eq!(
            keys.state_of(&format!("{GEMINI_KEY_BASE_URL}#0")),
            KeyState::OutOfQuota,
            "quota is its own mark — a probe or the retry deadline can lift it"
        );
        assert_eq!(
            keys.state_of(&format!("{GEMINI_KEY_BASE_URL}#1")),
            KeyState::Active
        );
        channel.close().await.expect("clean close");
    }

    /// R2-3/R2 defect 2 (owner feedback 2026-08-30): a socket that ends
    /// WITHOUT a goAway — the transcribe-live 10-minute cap, transport
    /// blips — recycles exactly like a goAway: `Renewing` (sends refused
    /// retryably), a redial on the session's own key, `Reconnected`. The
    /// old read loop returned silently: no event, sends failing at debug
    /// level forever — the invisible mid-session death the owner hit.
    #[tokio::test]
    async fn an_abrupt_socket_end_recycles_like_a_goaway() {
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(
            &handle,
            Arc::clone(&keys),
            ring(&[("KEY_FIRST", RING_KEY_A), ("KEY_SECOND", RING_KEY_B)]),
        );
        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup1 = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
            // The provider's hard connection cap: a close with NO goAway.
            server.close_current_with(1000, "connection lifetime exceeded");
            let _setup2 = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
        });
        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        let mut saw_renewing = false;
        // Deadline-wrapped: the DEFECT this pins was a silent, eventless
        // death — a regression must fail loudly here, not hang the suite.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let event = tokio::time::timeout_at(deadline, events.recv())
                .await
                .expect("an abrupt loss must surface events, never die silently")
                .expect("the recycle must complete");
            match event {
                ChannelEvent::Lifecycle(LifecycleEvent::Renewing { time_left }) => {
                    assert_eq!(time_left, None, "an abrupt loss announces no deadline");
                    saw_renewing = true;
                }
                ChannelEvent::Lifecycle(LifecycleEvent::Reconnected { .. }) => break,
                ChannelEvent::Lifecycle(LifecycleEvent::Expired) => {
                    panic!("an abrupt loss with a willing server must recycle, not expire")
                }
                _ => continue,
            }
        }
        assert!(saw_renewing, "whoever watches the events saw the renewal");
        script.await.expect("the fixture script ran");
        let dials = handle.handshakes();
        assert_eq!(dials.len(), 2, "one start dial, one abrupt-loss redial");
        assert_eq!(
            dials[1].header("x-goog-api-key"),
            Some(RING_KEY_A),
            "the redial reused the session's own key, never the ring"
        );
        channel
            .send_audio(&[1i16; 320])
            .await
            .expect("sends are accepted on the recycled connection");
        channel.close().await.expect("clean close");
    }

    #[tokio::test]
    async fn a_stalled_send_recycles_the_connection_instead_of_wedging() {
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(
            &handle,
            Arc::clone(&keys),
            ring(&[("KEY_FIRST", RING_KEY_A)]),
        );
        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup1 = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
            server.stall_current_reads();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            while server.handshakes().len() < 2 {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "a stalled send must lead to a redial"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            while server.next_from_adapter_indexed().await.0 == 0 {}
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
        });
        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        let image = vec![0u8; 4 << 20];
        let mut refused = None;
        for _ in 0..64 {
            if let Err(err) = channel.send_image(&image, "image/png").await {
                refused = Some(err);
                break;
            }
        }
        let refused = refused.expect("an unread socket must stall a send");
        assert_eq!(refused.kind, ErrorKind::Transport, "{refused:?}");
        assert!(refused.retryable, "{refused:?}");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut saw_renewing = false;
        loop {
            let event = tokio::time::timeout_at(deadline, events.recv())
                .await
                .expect("a stalled send must end in a recycle, never a silent wedge")
                .expect("the recycle must complete");
            match event {
                ChannelEvent::Lifecycle(LifecycleEvent::Renewing { .. }) => saw_renewing = true,
                ChannelEvent::Lifecycle(LifecycleEvent::Reconnected { .. }) => break,
                ChannelEvent::Lifecycle(LifecycleEvent::Expired) => {
                    panic!("a stalled socket with a willing server must recycle, not expire")
                }
                _ => continue,
            }
        }
        assert!(saw_renewing);
        script.await.expect("the fixture script ran");
        channel
            .send_audio(&[1i16; 320])
            .await
            .expect("sends are accepted on the recycled connection");
        channel.close().await.expect("clean close");
    }

    /// The abrupt-loss recycle's guard rails: an INTENTIONAL client close
    /// never redials (one dial total), and a server that keeps killing
    /// young replacement connections gets the crash-loop guard — the
    /// session expires instead of dialing forever.
    #[tokio::test]
    async fn client_close_never_recycles_and_crash_loops_expire() {
        // Half 1: client close — no redial.
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(&handle, Arc::clone(&keys), ring(&[("K", RING_KEY_A)]));
        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
        });
        channel.connect(ctx()).await.expect("connect");
        script.await.expect("script");
        channel.close().await.expect("clean close");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            handle.handshakes().len(),
            1,
            "an intentional close never triggers the abrupt-loss redial"
        );

        // Half 2: crash-looping server — recycles are bounded, then Expired.
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(&handle, Arc::clone(&keys), ring(&[("K", RING_KEY_A)]));
        let server = handle.clone();
        let script = tokio::spawn(async move {
            // The start plus ABRUPT_SHORT_LIFE_LIMIT tolerated short-lived
            // replacements, each killed at birth.
            for _ in 0..=(ABRUPT_SHORT_LIFE_LIMIT as usize) {
                let _setup = server.next_from_adapter().await;
                server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
                server.close_current_with(1000, "dies young");
            }
        });
        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("receiver");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let event = tokio::time::timeout_at(deadline, events.recv())
                .await
                .expect("the guard must resolve within the backoff budget")
                .expect("events flow until the terminal state");
            if let ChannelEvent::Lifecycle(LifecycleEvent::Expired) = event {
                break;
            }
        }
        script.await.expect("script");
        assert_eq!(
            handle.handshakes().len(),
            1 + ABRUPT_SHORT_LIFE_LIMIT as usize,
            "the crash-loop guard bounds the dials"
        );
        channel.close().await.ok();
    }

    /// THE mid-session pin: renewal reuses the session's own key verbatim.
    /// With a two-key ring armed, the goAway recycle must dial with the SAME
    /// key the session started on — rotating here would desynchronize the
    /// renewal machinery from the provider's session affinity.
    #[tokio::test]
    async fn a_renewal_redial_reuses_the_sessions_key_never_the_ring() {
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(
            &handle,
            Arc::clone(&keys),
            ring(&[("KEY_FIRST", RING_KEY_A), ("KEY_SECOND", RING_KEY_B)]),
        );
        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup1 = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
            server.send(WireFrame::Text(r#"{"goAway":{"timeLeft":"10s"}}"#.into()));
            let _setup2 = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
        });
        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        loop {
            match events.recv().await.expect("the recycle must complete") {
                ChannelEvent::Lifecycle(LifecycleEvent::Reconnected { .. }) => break,
                _ => continue,
            }
        }
        script.await.expect("the fixture script ran");

        let dials = handle.handshakes();
        assert_eq!(dials.len(), 2, "one start dial, one renewal dial");
        assert_eq!(dials[0].header("x-goog-api-key"), Some(RING_KEY_A));
        assert_eq!(
            dials[1].header("x-goog-api-key"),
            Some(RING_KEY_A),
            "the renewal reused the session's key; the ring's second key \
             stayed untouched"
        );
        channel.close().await.expect("clean close");
    }

    /// The unpinned dial-mark path (Role B): a renewal-time REFUSAL. The
    /// session expires exactly as any failed recycle — no third dial, the
    /// ring is never consulted — but the refused slot's health mark must
    /// land so the NEXT start skips it. Without the mark, a key revoked
    /// mid-session would be redialed first by every later start.
    #[tokio::test]
    async fn a_renewal_refusal_expires_the_session_and_marks_the_slot_for_the_next_start() {
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(
            &handle,
            Arc::clone(&keys),
            ring(&[("KEY_FIRST", RING_KEY_A), ("KEY_SECOND", RING_KEY_B)]),
        );
        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup1 = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
            server.send(WireFrame::Text(r#"{"goAway":{"timeLeft":"10s"}}"#.into()));
            let _setup2 = server.next_from_adapter().await;
            server.close_current_with(1007, "API key not valid. Please pass a valid API key.");
        });
        channel.connect(ctx()).await.expect("connect");
        let account = format!("{GEMINI_KEY_BASE_URL}#0");
        assert_eq!(
            keys.state_of(&account),
            KeyState::Active,
            "the start dial promoted the slot"
        );
        let mut events = channel.take_events().expect("the receiver is handed over");
        loop {
            match events
                .recv()
                .await
                .expect("the failed recycle must end the session")
            {
                ChannelEvent::Lifecycle(LifecycleEvent::Expired) => break,
                _ => continue,
            }
        }
        script.await.expect("the fixture script ran");

        assert_eq!(
            keys.state_of(&account),
            KeyState::Invalid,
            "the renewal-time refusal marks the slot so the NEXT start skips it"
        );
        assert_eq!(
            keys.state_of(&format!("{GEMINI_KEY_BASE_URL}#1")),
            KeyState::Unchecked,
            "the ring's second key was never consulted, let alone marked"
        );
        let dials = handle.handshakes();
        assert_eq!(
            dials.len(),
            2,
            "start dial + one renewal dial; a refusal mid-session never rotates"
        );
        assert_eq!(
            dials[1].header("x-goog-api-key"),
            Some(RING_KEY_A),
            "the refused renewal dial carried the session's own key"
        );
    }

    /// The quota arm of the same path: a renewal-time quota close marks the
    /// slot OutOfQuota (with the provider's retryDelay parsed out) — again
    /// for the next start's benefit; this session still just expires.
    #[tokio::test]
    async fn a_renewal_quota_close_marks_the_slot_out_of_quota() {
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(
            &handle,
            Arc::clone(&keys),
            ring(&[("KEY_ONLY", RING_KEY_A)]),
        );
        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup1 = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
            server.send(WireFrame::Text(r#"{"goAway":{"timeLeft":"10s"}}"#.into()));
            let _setup2 = server.next_from_adapter().await;
            server.close_current_with(
                1011,
                "You exceeded your current quota. {\"retryDelay\":\"45s\"}",
            );
        });
        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        loop {
            match events
                .recv()
                .await
                .expect("the failed recycle must end the session")
            {
                ChannelEvent::Lifecycle(LifecycleEvent::Expired) => break,
                _ => continue,
            }
        }
        script.await.expect("the fixture script ran");
        assert_eq!(
            keys.state_of(&format!("{GEMINI_KEY_BASE_URL}#0")),
            KeyState::OutOfQuota,
            "a quota refusal at renewal is key-specific knowledge the next start needs"
        );
    }

    /// The 2026-09-24 migration pin at the WIRE level: a session on the
    /// current tutor pin dials with the 3.8 setup contract (no
    /// thinkingConfig, BLOCKING declarations, server-side compression, no
    /// proactivity field) and the read loop TOLERATES the 3.8-era frames it
    /// has no behavior for (`usageMetadata` telemetry, `interactionStatus`,
    /// `waitingForInput`, `interimInputTranscription` in dialog mode) —
    /// the audio turn the new fields ride decodes sample-for-sample.
    #[tokio::test]
    async fn the_38_wire_fixture_replays_the_new_frames_green() {
        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: crate::model_pins::TUTOR_LIVE_MODEL.to_string(),
            },
            keys_with_gemini_key(),
            generation.clone(),
        );

        let fixture = load_fixture("gemini-live-38-tolerance");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        let mut context = ctx();
        context.tool_schemas = crate::voice::tools::registry::ToolRegistry::new(true).schemas();
        channel.connect(context).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");

        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::UtteranceEnd),
            "the audio-led turn open commits the utterance (#201) even when \
             the server's 3.8-era companion fields ride the same frames"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: vec![111, -111],
                gen: 0,
            }),
            "the audio part under the new fields decodes byte-for-byte"
        );
    }

    /// Session resumption end-to-end on the wire (owner-approved
    /// 2026-09-24): the 3.8 setup opts in, the server issues a handle, and
    /// the goAway recycle's redial PRESENTS that handle — the renewed
    /// connection reports `Reconnected { resumed: true }`, the signal the
    /// runtime uses to skip the recap re-inject (pinned at the runtime
    /// rig by `a_resumed_renewal_skips_the_recap_a_fresh_one_delivers_it`).
    #[tokio::test]
    async fn the_38_resumption_fixture_dials_the_renewal_with_the_stored_handle() {
        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: crate::model_pins::TUTOR_LIVE_MODEL.to_string(),
            },
            keys_with_gemini_key(),
            generation.clone(),
        );

        let fixture = load_fixture("gemini-live-resumption");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");

        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Renewing {
                time_left: Some(Duration::from_secs(10)),
            })),
            "the goAway surfaces as Renewing with its deadline"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Reconnected {
                resumed: true,
            })),
            "the redial presented the stored handle — the runtime must skip \
             the recap re-inject"
        );
    }

    /// Owner review note (validated 2026-09-24): a resumption dial the
    /// server REFUSES must drop the stored handle — otherwise the exact
    /// scenario the owner flagged replays forever: the fallback fresh dial
    /// succeeds, the server never issues a new handle on it, and every
    /// later recycle burns a doomed dial re-presenting the stale handle
    /// before falling back. The refused handle is dead state by direct
    /// evidence; keeping it only buys latency on the renewal path. A
    /// transient dial failure loses one resumption opportunity — bounded
    /// by the proven recap fallback — an asymmetry the clear wins.
    #[tokio::test]
    async fn a_refused_resumption_handle_is_dropped_not_retried() {
        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: crate::model_pins::TUTOR_LIVE_MODEL.to_string(),
            },
            keys_with_gemini_key(),
            generation.clone(),
        );

        let fixture = load_fixture("gemini-live-resumption-fallback");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");

        let mut events = channel.take_events().expect("the receiver is handed over");
        let mut reconnected_resumed = Vec::new();
        // Bounded: the replay has already pinned the conn-4 setup carried no
        // handle (the strongest evidence); the stream stays open afterward,
        // so draining to `None` would await forever.
        while reconnected_resumed.len() < 2 {
            match events.recv().await.expect("the stream stays open") {
                ChannelEvent::Lifecycle(LifecycleEvent::Reconnected { resumed }) => {
                    reconnected_resumed.push(resumed);
                }
                _ => continue,
            }
        }
        assert_eq!(
            reconnected_resumed,
            vec![false, false],
            "the refused handle fell back to a fresh session (false), and \
             the post-fallback recycle — whose server issued no new handle \
             — dialed fresh again instead of retrying the stale one"
        );
    }

    /// D2 (2026-09-28): `close()` on a SILENT peer (no close-ack) aborts the
    /// read loop at [`CLOSE_JOIN_BUDGET`] — bounded real time, huge margin
    /// (the pre-fix 10 s join fails this by 2.5×). The 38-tolerance fixture
    /// ends without a server close, so the socket stays open: exactly the
    /// live-server shape that made the session stop banner fire while the
    /// teardown finished in the background.
    #[tokio::test]
    async fn close_on_a_silent_peer_aborts_at_the_short_join_budget() {
        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: crate::model_pins::TUTOR_LIVE_MODEL.to_string(),
            },
            keys_with_gemini_key(),
            generation.clone(),
        );
        let fixture = load_fixture("gemini-live-38-tolerance");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });
        let mut context = ctx();
        context.tool_schemas = crate::voice::tools::registry::ToolRegistry::new(true).schemas();
        channel.connect(context).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");
        // The server holds the socket open — no close-ack is coming.
        let started = std::time::Instant::now();
        channel.close().await.expect("close is Ok either way");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the close join must abort at {:?}, took {:?}",
            CLOSE_JOIN_BUDGET,
            started.elapsed()
        );
    }

    #[test]
    fn refusal_message_parsers_extract_exactly_the_wire_facts() {
        assert_eq!(
            close_code_in("the provider closed the session during setup (close 1007): nope"),
            Some(1007)
        );
        assert_eq!(close_code_in("no code here"), None);
        assert_eq!(
            retry_after_secs_in("quota exceeded {\"retryDelay\":\"30s\"}"),
            Some(30)
        );
        assert_eq!(retry_after_secs_in("quota exceeded, no delay"), None);
        assert_eq!(
            retry_after_secs_in("Please retry in 1.746835114s."),
            Some(1)
        );
        assert_eq!(retry_after_secs_in("Please retry in 30s."), Some(30));
        assert_eq!(retry_after_secs_in("Please retry in 0.5s."), Some(1));
        assert_eq!(
            retry_after_secs_in(
                "provider returned HTTP 429 — { \"error\": { \"code\": 429, \
                 \"message\": \"You exceeded your current quota. \
                 Please retry in 1.746835114s.\", \"status\": \"RESOURCE_EXHAUSTED\" } }"
            ),
            Some(1)
        );
        assert!(is_quota_reason("You exceeded your current Quota, plan X"));
        assert!(is_quota_reason("status RESOURCE_EXHAUSTED"));
        assert!(!is_quota_reason("API key not valid"));
    }

    /// The mic chunks the round-trip test streams up; the fixture pins the
    /// exact base64 of their little-endian bytes, so the outbound framing is
    /// byte-matched, not just shape-matched.
    const MIC_CHUNK_A: [i16; 8] = [100, -100, 200, -200, 300, -300, 400, -400];
    const MIC_CHUNK_B: [i16; 6] = [1000, -1000, 2000, -2000, 3000, -3000];
    /// The response audio the fixture's recorded `serverContent` frames carry
    /// (base64 in the fixture file); the decoded `AudioDelta`s must equal these
    /// sample-for-sample.
    const RESPONSE_PART_1: [i16; 5] = [1111, -1111, 2222, -2222, 3333];
    const RESPONSE_PART_2: [i16; 4] = [4444, -4444, 5555, -5555];

    #[tokio::test]
    async fn gemini_live_audio_roundtrip_fixture_replays_green() {
        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            generation.clone(),
        );

        let fixture = load_fixture("gemini-live-audio-roundtrip");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        // Two prior barge-ins left the generation at 2 before the response
        // turn opens: its deltas must carry the turn's snapshot of that
        // counter, not a constant.
        generation.bump();
        generation.bump();

        channel.send_audio(&MIC_CHUNK_A).await.expect("first chunk");
        channel
            .send_audio(&MIC_CHUNK_B)
            .await
            .expect("second chunk");
        replayer.await.expect("the recorded fixture replays green");

        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::UtteranceEnd),
            "the fresh-turn open commits the utterance before the turn's \
             first content (issue #201)"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: RESPONSE_PART_1.to_vec(),
                gen: 2,
            }),
            "the first serverContent audio part decodes byte-for-byte and is \
             tagged with the generation its turn opened under"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: RESPONSE_PART_2.to_vec(),
                gen: 2,
            }),
            "the second part too: every delta of the turn carries its snapshot"
        );

        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned"
        );
    }

    /// The exact wire frame one user text turn must produce (serde_json
    /// serializes map keys in alphabetical order, so the rendering is
    /// deterministic): a `clientContent` with a single user `Content` and
    /// `turnComplete: true`, byte-for-byte.
    const TEXT_TURN_FRAME: &str = "{\"clientContent\":{\"turnComplete\":true,\"turns\":[{\"parts\":[{\"text\":\"what changed between figure two and figure three?\"}],\"role\":\"user\"}]}}";

    #[tokio::test]
    async fn gemini_live_text_turn_fixture_replays_green() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );

        // This fixture delivers `setupComplete` as a *binary* frame — the
        // framing observed live 2026-08-22 — so a regression restricting
        // `await_setup_complete` to text frames reds here.
        let fixture = load_fixture("gemini-live-text-turn");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        channel
            .send_text("what changed between figure two and figure three?")
            .await
            .expect("a text turn on a live session succeeds");
        replayer.await.expect("the recorded fixture replays green");

        let by_connection = handle.received_by_connection();
        assert_eq!(
            by_connection[0].last(),
            Some(&WireFrame::Text(TEXT_TURN_FRAME.to_string())),
            "the text turn goes out as exactly the pinned clientContent frame"
        );

        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned"
        );
    }

    /// The exact wire frame one chapter-boundary refresh must produce (serde_json
    /// serializes map keys in alphabetical order, so the rendering is
    /// deterministic): a `clientContent` carrying the rendered update as a single
    /// user `Content` with `turnComplete: false` — appended to the prompt, never
    /// starting generation — byte-for-byte, delimited excerpt untouched inside.
    const CONTEXT_REFRESH_FRAME: &str = "{\"clientContent\":{\"turnComplete\":false,\"turns\":[{\"parts\":[{\"text\":\"Context update (not a spoken user turn): the reader is now at chapter 9. The current excerpt is:\\n<<x>>chapter nine begins<</x>>\"}],\"role\":\"user\"}]}}";

    #[tokio::test]
    async fn gemini_live_context_update_fixture_replays_green() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );

        let fixture = load_fixture("gemini-live-context-update");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        channel
            .update_context(ContextUpdate::ChapterBoundary {
                chapter_idx: 9,
                excerpt: DelimitedExcerpt {
                    delimited_text: "<<x>>chapter nine begins<</x>>".into(),
                },
            })
            .await
            .expect("a chapter-boundary refresh on a live session succeeds");
        replayer.await.expect("the recorded fixture replays green");

        let by_connection = handle.received_by_connection();
        assert_eq!(
            by_connection[0].last(),
            Some(&WireFrame::Text(CONTEXT_REFRESH_FRAME.to_string())),
            "the refresh goes out as exactly the pinned turnComplete:false \
             clientContent frame"
        );

        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned"
        );
    }

    /// The renewal-recap rendering, pinned at the same surface the other two
    /// `ContextUpdate` variants are: the framing rides the instruction line
    /// OUTSIDE the delimited body (§4 — the recap body is untrusted
    /// transcript text and travels verbatim), and the frame stays
    /// `turnComplete: false` so re-injection never starts generation.
    #[test]
    fn the_renewal_recap_renders_framed_outside_the_delimiters() {
        let rendered = client_content_context_message(&ContextUpdate::RenewalRecap {
            recap: DelimitedExcerpt {
                delimited_text: "<<s>>user: what is inertia?<</s>>".into(),
            },
            switched_to: None,
        })
        .expect("a renewal recap is framed for the wire");
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["clientContent"]["turnComplete"], false);
        let text = value["clientContent"]["turns"][0]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(
            text,
            format!(
                "Context update (not a spoken user turn): {}\n<<s>>user: what is inertia?<</s>>",
                crate::voice::channel::RENEWAL_RECAP_FRAMING
            ),
            "framing outside, delimited body verbatim inside"
        );
    }

    #[tokio::test]
    async fn gemini_live_transcripts_and_goaway_fixture_replays_green() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );

        assert!(
            !channel.capabilities().word_timestamps,
            "Gemini transcript deltas are not word-aligned: the channel must \
             advertise the §11 sentence-level degrade"
        );
        assert_eq!(channel.capabilities().vad_mode, VadMode::Server);

        let fixture = load_fixture("gemini-live-transcripts-goaway");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");

        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "The figure shows ".to_string(),
            }),
            "the first outputTranscription payload arrives first, intact"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "a rising curve.".to_string(),
            }),
            "deltas surface in arrival order, one event per payload"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Renewing {
                time_left: Some(Duration::from_secs(10)),
            })),
            "goAway surfaces as the uniform session-ending lifecycle event, \
             carrying the provider's timeLeft"
        );

        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned"
        );
    }

    #[tokio::test]
    async fn gemini_live_tool_call_roundtrip_and_stale_anchor_fixture_replays_green() {
        use crate::voice::channel::anchor::{
            normalize_teacher_anchor, AnchorDecision, STALE_GENERATION_CODE, TEACHER_ANCHOR_TOOL,
        };

        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            generation.clone(),
        );

        let fixture = load_fixture("gemini-live-tool-calls");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        // Round trip: the recorded toolCall surfaces intact, and the locally
        // executed result reaches the wire as a toolResponse under the same id
        // (the fixture pins id, name, and the result payload).
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::UtteranceEnd),
            "the first surfaced call of a fresh turn commits the utterance \
             before the ToolCall itself (issue #201) — and only the first: \
             the later calls of this un-delimited turn ride the same boundary"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::ToolCall {
                call_id: "call-7".to_string(),
                name: "seek_relative".to_string(),
                args: serde_json::json!({"seconds": -30}),
            }),
            "the tool call surfaces with id, name and args passed through untouched"
        );
        channel
            .send_tool_result(
                "call-7",
                ToolResult::Ok(serde_json::json!({"position_ms": 41_000})),
            )
            .await
            .expect("answering a surfaced call succeeds");

        // A barge-in: on this channel `interrupt` is exactly the shared
        // generation bump (no client cancel frame exists — the final
        // wire-empty assertion below proves nothing was sent for it). The
        // teacher_anchor the fixture injects next belongs to the superseded
        // response (gen 0).
        assert_eq!(generation.current(), 0);
        channel
            .interrupt()
            .await
            .expect("interrupt is valid while connected");
        assert_eq!(
            generation.current(),
            1,
            "interrupt bumps the shared barge-in generation"
        );

        let anchor_call = events.recv().await.expect("the anchor tool call surfaces");
        let ChannelEvent::ToolCall {
            call_id,
            name,
            args,
        } = anchor_call
        else {
            panic!("expected the anchor tool call, got {anchor_call:?}");
        };
        assert_eq!(name, TEACHER_ANCHOR_TOOL);
        let decision = normalize_teacher_anchor(&args, 0, &generation);
        let AnchorDecision::Stale { ack } = decision else {
            panic!("a superseded-generation anchor must be dropped (§11.1), got {decision:?}");
        };
        assert!(
            matches!(&ack, ToolResult::Err { code, .. } if code == STALE_GENERATION_CODE),
            "the dropped anchor's ack carries {STALE_GENERATION_CODE}, got {ack:?}"
        );
        channel
            .send_tool_result(&call_id, ack)
            .await
            .expect("dropped-but-acked: the stale anchor is still answered on the wire");
        replayer.await.expect("the recorded fixture replays green");

        // The server retracts call-9 with a toolCallCancellation right after
        // surfacing it. The transcript sentinel proves the read loop is past
        // the cancellation frame (events decode in arrival order), so the
        // "late" result below really races nothing.
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::ToolCall {
                call_id: "call-9".to_string(),
                name: "pause_playback".to_string(),
                args: serde_json::json!({}),
            }),
            "the soon-to-be-cancelled call surfaces like any other"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "after the cancellation".to_string(),
            })
        );
        channel
            .send_tool_result(
                "call-9",
                ToolResult::Ok(serde_json::json!({"paused": true})),
            )
            .await
            .expect(
                "a late result for a cancelled call is a silent no-op: the \
                 cancellation made silence the correct answer",
            );

        let unknown = channel
            .send_tool_result("call-7", ToolResult::Ok(serde_json::json!({})))
            .await;
        assert!(
            matches!(unknown, Err(ref err) if err.kind == ErrorKind::InvalidState),
            "an already-answered call cannot be answered twice, got {unknown:?}"
        );

        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned"
        );
    }

    #[tokio::test]
    async fn gemini_live_goaway_rejects_sends_retryably_and_never_queues() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );

        let fixture = load_fixture("gemini-live-transcripts-goaway");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");

        // Drain to the Renewing announcement: the read loop sets the renewing
        // flag before emitting it, so seeing the event guarantees the
        // send-side rejections below are already in force.
        let mut events = channel.take_events().expect("the receiver is handed over");
        loop {
            match events.recv().await.expect("the goAway must surface") {
                ChannelEvent::Lifecycle(LifecycleEvent::Renewing { .. }) => break,
                _ => continue,
            }
        }

        let rejected = channel
            .send_audio(&MIC_CHUNK_A)
            .await
            .expect_err("a mid-renewal send must be rejected, never queued");
        assert_eq!(
            rejected.kind,
            ErrorKind::SessionExpired,
            "the rejection is the uniform session-cap classification"
        );
        assert!(
            rejected.retryable,
            "the rejection is retryable — the session is coming back and the \
             harness owns replay (VOICE_AGENT §2 rule 3)"
        );
        let text_rejected = channel.send_text("late turn").await;
        assert!(
            matches!(text_rejected, Err(ref err)
                if err.kind == ErrorKind::SessionExpired && err.retryable),
            "send_text rejects the same way, got {text_rejected:?}"
        );
        let tool_rejected = channel
            .send_tool_result("call-1", ToolResult::Ok(serde_json::json!({})))
            .await;
        assert!(
            matches!(tool_rejected, Err(ref err)
                if err.kind == ErrorKind::SessionExpired && err.retryable),
            "send_tool_result rejects the same way, got {tool_rejected:?}"
        );

        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "past the recycle dial's own setup (pinned by the fixture), no \
             frame reached the wire after goAway: rejected payloads are \
             dropped at the channel boundary, not queued"
        );
    }

    #[tokio::test]
    async fn gemini_live_renewal_recycle_fixture_replays_green() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );

        let fixture = load_fixture("gemini-live-renewal-recycle");
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
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Renewing {
                time_left: Some(Duration::from_secs(10)),
            }))
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Reconnected {
                resumed: false,
            })),
            "the recycle dials a second connection, repeats the setup \
             handshake, and reports Reconnected on the same receiver"
        );

        channel
            .send_audio(&MIC_CHUNK_A)
            .await
            .expect("a post-Reconnected send succeeds against the renewed connection");
        replayer.await.expect("the recorded fixture replays green");

        // Epoch pinning (conformance check 6 vocabulary): the fixture server
        // groups frames by connection, and the renewal's second connection is
        // epoch 1 — the post-reconnect payload must land there, byte-matched,
        // and nowhere else.
        let pinned_payload = "ZACc/8gAOP8sAdT+kAFw/g==";
        let by_epoch = handle.received_by_connection();
        assert_eq!(
            by_epoch.len(),
            2,
            "one renewal produces exactly two connections (epochs)"
        );
        let carries_payload = |frames: &[WireFrame]| {
            frames.iter().any(
                |frame| matches!(frame, WireFrame::Text(text) if text.contains(pinned_payload)),
            )
        };
        assert!(
            carries_payload(&by_epoch[1]),
            "the post-reconnect payload lands in epoch 1"
        );
        assert!(
            !carries_payload(&by_epoch[0]),
            "…and never on the pre-renewal connection"
        );

        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned"
        );
    }

    #[test]
    fn proto_duration_parses_seconds_and_rejects_garbage() {
        assert_eq!(parse_proto_duration("10s"), Some(Duration::from_secs(10)));
        assert_eq!(
            parse_proto_duration("584.32s"),
            Some(Duration::from_secs_f64(584.32))
        );
        for bad in ["", "10", "s", "-3s", "NaNs", "10 s"] {
            assert_eq!(
                parse_proto_duration(bad),
                None,
                "{bad:?} is not a proto3 JSON Duration"
            );
        }
    }

    /// The VA2 conformance suite against a fixture-backed [`GeminiLiveChannel`]
    /// (VA5 m3): the same [`crate::voice::channel::conformance`] checks the
    /// MockChannel passes, driven over a real socket. The rig mirrors the
    /// mock's harness shape — a [`ConformanceFixture`] whose `rig` yields a
    /// fresh channel plus a [`ProviderSim`] — but the sim plays the provider
    /// by scripting wire frames into the loopback [`WireServer`] and observing
    /// what the adapter's socket carried, exactly as the suite's doc requires
    /// of an adapter's replay rig. The per-connection session opening
    /// (handshake hygiene, setup, setupComplete) is the recorded
    /// `gemini-live-conformance-session` fixture, replayed verbatim for
    /// connection 1 and re-applied to every renewal dial.
    mod conformance_rig {
        use super::*;
        use crate::voice::channel::conformance::{run_all, ConformanceFixture, ProviderSim};
        use crate::voice::channel::fixture::{
            load_fixture, replay, WireFixture, WireHandle, WireServer, WireStep,
        };

        /// Serves the session opening for every connection the adapter dials:
        /// the recorded fixture verbatim for connection 1, then — gated on the
        /// suite calling `complete_renewal`, so the mid-renewal window stays
        /// open until the check chooses to close it — the same script
        /// re-applied to each renewal dial.
        async fn concierge(handle: WireHandle, mut renewal_permits: mpsc::UnboundedReceiver<()>) {
            let script = load_fixture("gemini-live-conformance-session");
            replay(&script, &handle).await;
            for connection in 2.. {
                if renewal_permits.recv().await.is_none() {
                    return; // rig dropped: no further dials to serve
                }
                serve_opening(&script, &handle, connection).await;
            }
        }

        /// The recorded opening re-applied to connection `connection`: same
        /// handshake hygiene, same setup expectation, same setupComplete reply.
        async fn serve_opening(script: &WireFixture, handle: &WireHandle, connection: usize) {
            let record = handle.wait_for_connection(connection).await;
            let WireStep::ExpectHandshake {
                target_absent,
                headers_present,
                ..
            } = &script.steps[0]
            else {
                panic!("the conformance session script must open with its handshake check");
            };
            for needle in target_absent {
                assert!(
                    !record.target.contains(needle.as_str()),
                    "renewal dial leaked `{needle}` into the connect URL"
                );
            }
            for header in headers_present {
                assert!(
                    record.header(header).is_some(),
                    "renewal dial lacks the `{header}` header"
                );
            }
            // Frames of earlier connections may still be queued; the first
            // frame on the new connection is the adapter's setup message.
            let setup = loop {
                let (idx, frame) = handle.next_from_adapter_indexed().await;
                if idx + 1 == connection {
                    break frame;
                }
            };
            let WireFrame::Text(setup) = setup else {
                panic!("the renewal dial's first frame must be the JSON setup, got {setup:?}");
            };
            assert!(setup.contains("\"setup\"") && !setup.contains(FIXTURE_KEY));
            let Some(WireStep::SendText { text }) = script.steps.last() else {
                panic!("the conformance session script must end with the setupComplete reply");
            };
            handle.send(WireFrame::Text(text.clone()));
        }

        fn frame_json(frame: &WireFrame) -> Option<serde_json::Value> {
            let WireFrame::Text(text) = frame else {
                return None;
            };
            serde_json::from_str(text).ok()
        }

        fn decode_audio(frame: &WireFrame) -> Option<Vec<i16>> {
            let value = frame_json(frame)?;
            let data = value.get("realtimeInput")?.get("audio")?.get("data")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.as_str()?)
                .ok()?;
            Some(
                bytes
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect(),
            )
        }

        /// The single user-part text of a `clientContent` frame whose
        /// `turnComplete` matches — `true` selects text turns, `false` the
        /// context refreshes.
        fn client_content_text(frame: &WireFrame, turn_complete: bool) -> Option<String> {
            let value = frame_json(frame)?;
            let content = value.get("clientContent")?;
            if content.get("turnComplete")?.as_bool()? != turn_complete {
                return None;
            }
            let text = content
                .get("turns")?
                .get(0)?
                .get("parts")?
                .get(0)?
                .get("text")?;
            Some(text.as_str()?.to_string())
        }

        /// Reverses [`client_content_context_message`]'s rendering back into
        /// the [`ContextUpdate`] the provider was given.
        fn decode_context_update(rendered: &str) -> Option<ContextUpdate> {
            const PREFIX: &str = "Context update (not a spoken user turn): ";
            let rest = rendered.strip_prefix(PREFIX)?;
            if let Some(rest) = rest.strip_prefix("the reader is now at chapter ") {
                let (chapter_idx, excerpt) = rest.split_once(". The current excerpt is:\n")?;
                return Some(ContextUpdate::ChapterBoundary {
                    chapter_idx: chapter_idx.parse().ok()?,
                    excerpt: DelimitedExcerpt {
                        delimited_text: excerpt.to_string(),
                    },
                });
            }
            if let Some(recap) = rest
                .strip_prefix(crate::voice::channel::RENEWAL_RECAP_FRAMING)
                .and_then(|rest| rest.strip_prefix('\n'))
            {
                return Some(ContextUpdate::RenewalRecap {
                    recap: DelimitedExcerpt {
                        delimited_text: recap.to_string(),
                    },
                    switched_to: None,
                });
            }
            let pos_ms = rest
                .strip_prefix("playback position is ")?
                .strip_suffix(" ms.")?;
            Some(ContextUpdate::PlaybackPosition {
                pos_ms: pos_ms.parse().ok()?,
            })
        }

        /// Reverses [`tool_response_message`]: the `{"error": {code, message}}`
        /// envelope back to `ToolResult::Err`, anything else to the `Ok`
        /// payload as the wire carried it.
        fn decode_tool_result(frame: &WireFrame) -> Option<(String, ToolResult)> {
            let value = frame_json(frame)?;
            let response = value
                .get("toolResponse")?
                .get("functionResponses")?
                .get(0)?;
            let id = response.get("id")?.as_str()?.to_string();
            let payload = response.get("response")?;
            let result = match payload.get("error") {
                Some(err) => ToolResult::Err {
                    code: err.get("code")?.as_str()?.to_string(),
                    message: err.get("message")?.as_str()?.to_string(),
                },
                None => ToolResult::Ok(payload.clone()),
            };
            Some((id, result))
        }

        struct GeminiSim {
            handle: WireHandle,
            renewal_permits: mpsc::UnboundedSender<()>,
            /// The cancel record shared with [`BargeObservedChannel`]: the
            /// Live API has no client cancel frame, so the socket cannot
            /// carry it — see the wrapper's doc for where the observation
            /// actually happens.
            interrupts: Arc<Mutex<Vec<Option<u64>>>>,
            /// The `gen` of the last scripted response audio, so `emit_audio`
            /// can tell a continuation of the same response from the start of
            /// a new one — see its doc for the wire frame that difference
            /// scripts.
            last_audio_gen: Option<u64>,
        }

        impl GeminiSim {
            fn send_json(&self, frame: serde_json::Value) {
                self.handle.send(WireFrame::Text(frame.to_string()));
            }

            /// Adapter frames, snapshot only once two consecutive reads agree:
            /// a send the suite awaited is flushed to the socket but not
            /// necessarily read by the server yet.
            async fn settled_frames(&self) -> Vec<Vec<WireFrame>> {
                let mut last = self.handle.received_by_connection();
                loop {
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    let now = self.handle.received_by_connection();
                    if now == last {
                        return now;
                    }
                    last = now;
                }
            }
        }

        #[async_trait]
        impl ProviderSim for GeminiSim {
            /// The Live API wire carries no generation id — `gen` itself never
            /// reaches a frame. What the wire does carry is the turn boundary:
            /// a response only ever starts under a different generation because
            /// the previous response was superseded, and the server announces
            /// that as `serverContent.interrupted` before the replacement
            /// turn's audio (https://ai.google.dev/api/live, accessed
            /// 2026-08-21 — set when "a client message has interrupted current
            /// model generation"; the suite's barge-in mic activity is exactly
            /// such a message). So a `gen` change scripts that announcement
            /// first; same-`gen` audio is a continuation of the open turn and
            /// scripts nothing extra. The adapter's turn-scoped tagging
            /// reconstructs the suite's `gen` from those delimiters.
            async fn emit_audio(&mut self, gen: u64, pcm: Vec<i16>) {
                if self.last_audio_gen.is_some_and(|last| last != gen) {
                    self.send_json(serde_json::json!({"serverContent": {"interrupted": true}}));
                }
                self.last_audio_gen = Some(gen);
                let mut bytes = Vec::with_capacity(pcm.len() * 2);
                for sample in &pcm {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
                self.send_json(serde_json::json!({
                    "serverContent": {"modelTurn": {"parts": [{"inlineData": {
                        "mimeType": "audio/pcm;rate=24000",
                        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                    }}]}}
                }));
            }

            /// `outputTranscription` narrates the model, `inputTranscription`
            /// the user (https://ai.google.dev/api/live, accessed 2026-08-21 —
            /// `BidiGenerateContentServerContent` carries both).
            async fn emit_transcript(&mut self, role: Role, text: String) {
                match role {
                    Role::Assistant => {
                        self.send_json(serde_json::json!({
                            "serverContent": {
                                "modelTurn": {
                                    "parts": [{"text": &text}]
                                },
                                "outputTranscription": {"text": text}
                            }
                        }));
                    }
                    Role::User => {
                        self.send_json(serde_json::json!({
                            "serverContent": {
                                "inputTranscription": {"text": text}
                            }
                        }));
                    }
                }
            }

            async fn emit_tool_call(
                &mut self,
                call_id: String,
                name: String,
                args: serde_json::Value,
            ) {
                self.send_json(serde_json::json!({
                    "toolCall": {"functionCalls": [{"id": call_id, "name": name, "args": args}]}
                }));
            }

            async fn announce_session_end(&mut self) {
                // No timeLeft: the suite's uniform announcement carries none.
                self.send_json(serde_json::json!({"goAway": {}}));
            }

            async fn complete_renewal(&mut self) {
                let _ = self.renewal_permits.send(());
            }

            async fn expire(&mut self) {
                // The adapter recycles immediately after goAway, so wait for
                // its renewal dial and drop that connection while its
                // setupComplete is still unserved (the concierge holds it
                // behind the permit): the recycle becomes unrecoverable.
                self.handle.wait_for_connection(2).await;
                self.handle.close_current();
            }

            async fn received_audio_by_epoch(&mut self) -> Vec<Vec<Vec<i16>>> {
                self.settled_frames()
                    .await
                    .iter()
                    .map(|frames| frames.iter().filter_map(decode_audio).collect())
                    .collect()
            }

            async fn received_texts(&mut self) -> Vec<String> {
                self.settled_frames()
                    .await
                    .iter()
                    .flatten()
                    .filter_map(|frame| client_content_text(frame, true))
                    .collect()
            }

            async fn received_context_updates(&mut self) -> Vec<ContextUpdate> {
                self.settled_frames()
                    .await
                    .iter()
                    .flatten()
                    .filter_map(|frame| client_content_text(frame, false))
                    .filter_map(|text| decode_context_update(&text))
                    .collect()
            }

            async fn received_tool_results(&mut self) -> Vec<(String, ToolResult)> {
                self.settled_frames()
                    .await
                    .iter()
                    .flatten()
                    .filter_map(decode_tool_result)
                    .collect()
            }

            async fn received_interrupts(&mut self) -> Vec<Option<u64>> {
                self.interrupts.lock().unwrap().clone()
            }
        }

        /// The channel under test with the rig's cancel observation wrapped
        /// around it. On this adapter `interrupt` sends no frame — the Live
        /// API has no client cancel message, so the generation-tag drop IS
        /// the cancel — which means the socket cannot carry the suite's
        /// cancel record ([`ProviderSim::received_interrupts`]). The
        /// channel's real interrupt behavior is still observable: it advances
        /// the shared barge-in counter, the same counter its read loop tags
        /// deltas with. So the rig records a cancel only when the delegated
        /// call demonstrably performed that advance — an interrupt that left
        /// the counter untouched records nothing and the suite reds; the
        /// record is tied to what the channel did, not to the call — and the
        /// recorded value is the counter the cancel observed on entry, the
        /// same snapshot-at-cancel-time the mock records when its cancel op
        /// reaches the provider.
        struct BargeObservedChannel {
            inner: GeminiLiveChannel,
            generation: Generation,
            interrupts: Arc<Mutex<Vec<Option<u64>>>>,
        }

        #[async_trait]
        impl SessionChannel for BargeObservedChannel {
            async fn connect(&mut self, ctx: SessionContext) -> Result<(), ChannelError> {
                self.inner.connect(ctx).await
            }
            async fn close(&mut self) -> Result<(), ChannelError> {
                self.inner.close().await
            }
            async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
                self.inner.send_audio(pcm_16k_mono).await
            }
            async fn send_text(&mut self, text: &str) -> Result<(), ChannelError> {
                self.inner.send_text(text).await
            }
            async fn send_tool_result(
                &mut self,
                call_id: &str,
                result: ToolResult,
            ) -> Result<(), ChannelError> {
                self.inner.send_tool_result(call_id, result).await
            }
            async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError> {
                self.inner.update_context(update).await
            }
            async fn interrupt(&mut self) -> Result<(), ChannelError> {
                let observed = self.generation.current();
                self.inner.interrupt().await?;
                if self.generation.current() > observed {
                    self.interrupts.lock().unwrap().push(Some(observed));
                }
                Ok(())
            }
            fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
                self.inner.take_events()
            }
            fn capabilities(&self) -> ChannelCapabilities {
                self.inner.capabilities()
            }
        }

        struct GeminiFixture;

        #[async_trait]
        impl ConformanceFixture for GeminiFixture {
            type Channel = BargeObservedChannel;
            type Sim = GeminiSim;

            async fn rig(&mut self, generation: &Generation) -> (BargeObservedChannel, GeminiSim) {
                let handle = WireServer::start().await;
                let (renewal_permits, permits) = mpsc::unbounded_channel();
                tokio::spawn(concierge(handle.clone(), permits));
                let channel = GeminiLiveChannel::new(
                    GeminiLiveConfig {
                        endpoint_override: Some(handle.url("/ws/live")),
                        model: "models/gemini-2.5-flash-native-audio-preview".into(),
                    },
                    keys_with_gemini_key(),
                    generation.clone(),
                );
                let interrupts = Arc::new(Mutex::new(Vec::new()));
                (
                    BargeObservedChannel {
                        inner: channel,
                        generation: generation.clone(),
                        interrupts: interrupts.clone(),
                    },
                    GeminiSim {
                        handle,
                        renewal_permits,
                        interrupts,
                        last_audio_gen: None,
                    },
                )
            }
        }

        #[tokio::test]
        async fn gemini_live_passes_session_channel_conformance() {
            run_all(&mut GeminiFixture).await;
        }
    }

    #[tokio::test]
    async fn gemini_live_sends_ping_frames_and_sparse_keepalive() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        )
        .with_keepalive_intervals(Duration::from_millis(40), Duration::from_millis(80));

        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
        });
        channel.connect(ctx()).await.expect("connect");
        script.await.expect("script finished");

        // Every wait is a condition with a deadline, never a fixed sleep: the
        // scheduler may run late (Windows timers tick at ~15.6 ms), so only
        // "at least" bounds are asserted on elapsed time.
        const DEADLINE: Duration = Duration::from_secs(5);
        const SPARSE_IDLE: Duration = Duration::from_millis(80);
        fn audio_bytes(frame: &WireFrame) -> Vec<u8> {
            let WireFrame::Text(text) = frame else {
                panic!("expected a text media frame");
            };
            let v: serde_json::Value = serde_json::from_str(text).expect("valid json");
            let data_b64 = v["realtimeInput"]["audio"]["data"]
                .as_str()
                .expect("audio data");
            base64::engine::general_purpose::STANDARD
                .decode(data_b64)
                .expect("valid b64")
        }
        fn assert_silent_keepalive(frame: WireFrame) {
            let bytes = audio_bytes(&frame);
            assert_eq!(bytes.len(), 3200, "100ms PCM16 mono at 16kHz is 3200 bytes");
            assert!(
                bytes.iter().all(|&b| b == 0),
                "sparse keep-alive chunk is pure silence"
            );
        }

        timeout(DEADLINE, async {
            while handle.pings_received() == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("an RFC 6455 Ping control frame arrives on the 40ms interval");

        let first_keepalive = timeout(DEADLINE, handle.next_from_adapter())
            .await
            .expect("the sparse keep-alive arrives once the socket idles");
        assert_silent_keepalive(first_keepalive);

        // Real user audio resets the idle timer: the next keep-alive may
        // come no sooner than the idle threshold after it. Speaking mid-way
        // through a ticker period makes the rule observable — an adapter
        // that ignored it would fire on the very next tick, about half a
        // period later. (This sleep only positions the speech; nothing
        // asserts on its length.)
        tokio::time::sleep(SPARSE_IDLE / 2).await;
        let spoke_at = std::time::Instant::now();
        channel
            .send_audio(&[1000i16; 320])
            .await
            .expect("send audio");
        timeout(DEADLINE, async {
            while audio_bytes(&handle.next_from_adapter().await)
                .iter()
                .all(|&b| b == 0)
            {}
        })
        .await
        .expect("the user audio reaches the wire behind any earlier keep-alive");
        let next_keepalive = timeout(DEADLINE, handle.next_from_adapter())
            .await
            .expect("the keep-alive resumes after the socket idles again");
        assert!(
            spoke_at.elapsed() >= SPARSE_IDLE,
            "no sparse keep-alive while the last media is under 80ms old"
        );
        assert_silent_keepalive(next_keepalive);

        channel.close().await.expect("clean close");
    }

    /// The VA5 live smoke against the real Live API free tier (docs/TASKS.md
    /// VA5 acceptance: "live smoke on Deepgram + Gemini Live (free tier),
    /// executed only when free-tier API keys are provided (clean non-failing
    /// skip when absent)"). Key-optional and manual-only, the loopback-rig
    /// posture (`voice::audio::cpal_backend`): `#[ignore]` keeps it out of
    /// CI's bare `cargo test`, and a missing `GEMINI_API_KEY` is a printed
    /// skip, never a failure. Run with:
    ///
    /// ```text
    /// GEMINI_API_KEY=… cargo test --lib smoke_gemini_live -- --ignored --nocapture
    /// ```
    ///
    /// The key rides only the VA3 [`KeyManager`] → `x-goog-api-key` header
    /// path the adapter already owns; this test never prints or asserts on
    /// key bytes. `GEMINI_LIVE_MODEL` overrides the model if the free-tier
    /// Live roster shifts under the default.
    ///
    /// The contract shape asserted: connect completes the real
    /// setup/setupComplete handshake and `Connected` is the first event; one
    /// short text turn elicits **any server turn** (audio delta or transcript
    /// delta — content is model output and never asserted verbatim, only
    /// printed); close completes inside [`HANDSHAKE_BUDGET`] (the peer
    /// acknowledged, not our abort fallback) and the event stream ends with
    /// no `Error` surfaced anywhere.
    #[tokio::test]
    #[ignore]
    async fn smoke_gemini_live() {
        let Some(key) = std::env::var("GEMINI_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
        else {
            println!(
                "SKIP smoke_gemini_live: GEMINI_API_KEY is not set — \
                 the free-tier live smoke is key-optional (docs/TASKS.md VA5)"
            );
            return;
        };
        let model = std::env::var("GEMINI_LIVE_MODEL")
            .unwrap_or_else(|_| "models/gemini-2.5-flash-native-audio-preview-12-2025".to_string());

        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(GEMINI_KEY_BASE_URL, &ApiKey::new(key))
            .expect("session-backend store never fails");
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: None,
                model,
            },
            Arc::new(KeyManager::with_store(store)),
            Generation::new(),
        );

        // Connect is itself half the smoke: it only returns Ok after the
        // server answered the setup message with setupComplete on a real
        // authenticated socket.
        channel.connect(ctx()).await.expect("live connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "setupComplete arrived — the session is live"
        );

        channel
            .send_text("Please reply with one short spoken sentence.")
            .await
            .expect("one text turn");

        // Any server turn completes the shape: the first content event —
        // response audio or a transcript delta — is the proof; lifecycle
        // events keep the wait alive, an Error or a dead stream fails it.
        let budget = Duration::from_secs(30);
        let asked = tokio::time::Instant::now();
        loop {
            let remaining = budget
                .checked_sub(asked.elapsed())
                .expect("no server turn within 30 s of the text turn");
            match timeout(remaining, events.recv()).await {
                Ok(Some(ChannelEvent::Error(err))) => panic!("live session error: {err:?}"),
                Ok(Some(ChannelEvent::Lifecycle(event))) => {
                    println!("live lifecycle: {event:?}");
                }
                Ok(Some(ChannelEvent::AudioDelta { pcm, gen })) => {
                    println!("server turn: AudioDelta ({} samples, gen {gen})", pcm.len());
                    break;
                }
                Ok(Some(ChannelEvent::TranscriptDelta { role, text })) => {
                    println!("server turn: TranscriptDelta ({role:?}): {text}");
                    break;
                }
                Ok(Some(event)) => {
                    println!("server turn: {event:?}");
                    break;
                }
                Ok(None) => panic!("the server closed the session before any turn"),
                Err(_) => panic!("no server turn within 30 s of the text turn"),
            }
        }

        let closing = tokio::time::Instant::now();
        channel.close().await.expect("clean close");
        assert!(
            closing.elapsed() < HANDSHAKE_BUDGET,
            "close joined inside the budget: the peer acknowledged the \
             close handshake itself"
        );
        while let Some(event) = events.recv().await {
            assert!(
                !matches!(event, ChannelEvent::Error(_)),
                "the close drain surfaced an error: {event:?}"
            );
        }
        println!("smoke_gemini_live: Connected → server turn → clean close");
    }
    /// Issue #201, the placement/dedupe/re-arm pins across MULTIPLE turns
    /// (the single-turn placements are pinned by the audio-roundtrip and
    /// tool-calls fixtures above). One recorded session walks every commit
    /// and every non-commit the mapping owes:
    ///
    /// - transcription-only frames narrate, they do not prove a response
    ///   started: nothing commits before the first surfaced model event;
    /// - a toolCall frame whose entries are all unanswerable (no id/name)
    ///   commits nothing — a turnComplete follows it, so a wrongful commit
    ///   would leak an extra UtteranceEnd into the stream;
    /// - the first SURFACED call of a fresh turn commits, before the
    ///   ToolCall itself, even when an unanswerable entry precedes it in
    ///   the same frame;
    /// - the turn's later modelTurn content rides the same boundary (no
    ///   second UtteranceEnd mid-turn);
    /// - `turnComplete` AND `interrupted` both re-arm: the next fresh turn
    ///   commits again.
    ///
    /// The assertion is the exact event sequence via `recv` — deliberately
    /// not `recv_content` — so a phantom or missing boundary cannot hide.
    #[tokio::test]
    async fn gemini_live_turn_boundaries_fixture_replays_green() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );

        let fixture = load_fixture("gemini-live-turn-boundaries");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");

        let mut events = channel.take_events().expect("the receiver is handed over");
        let mut sequence = Vec::new();
        for idx in 0..10 {
            let event = timeout(Duration::from_secs(5), events.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!("event {idx} never arrived — a boundary went missing: {sequence:?}")
                })
                .expect("the event stream stays open");
            sequence.push(event);
        }
        assert_eq!(
            sequence,
            vec![
                ChannelEvent::Lifecycle(LifecycleEvent::Connected),
                ChannelEvent::TranscriptDelta {
                    role: Role::User,
                    text: "what is figure two".to_string(),
                },
                ChannelEvent::TranscriptDelta {
                    role: Role::Assistant,
                    text: "narrating speech".to_string(),
                },
                ChannelEvent::UtteranceEnd,
                ChannelEvent::ToolCall {
                    call_id: "call-1".to_string(),
                    name: "pause_playback".to_string(),
                    args: serde_json::json!({}),
                },
                ChannelEvent::AudioDelta {
                    pcm: RESPONSE_PART_1.to_vec(),
                    gen: 0,
                },
                ChannelEvent::UtteranceEnd,
                ChannelEvent::AudioDelta {
                    pcm: RESPONSE_PART_2.to_vec(),
                    gen: 0,
                },
                ChannelEvent::UtteranceEnd,
                ChannelEvent::AudioDelta {
                    pcm: RESPONSE_PART_1.to_vec(),
                    gen: 0,
                },
            ],
            "exactly one UtteranceEnd per fresh turn, before the turn's first \
             surfaced event; transcriptions and unanswerable toolCall frames \
             commit nothing; turnComplete and interrupted both re-arm (#201)"
        );

        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned"
        );
    }

    /// Review 2026-10-09, J2: the harness learns when a voice reply is over.
    /// A finished (`turnComplete`) or cut (`interrupted`) reply that opened
    /// ends in `TurnComplete`; one whose `toolCall` still awaits an answer
    /// does not — the continuation after the answer is the same reply.
    #[tokio::test]
    async fn an_opened_voice_reply_reports_its_end_unless_a_tool_answer_is_owed() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );
        let fixture = load_fixture("gemini-live-reply-complete");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });
        channel.connect(ctx()).await.expect("connect");
        replayer.await.expect("the fixture replays green");

        let mut events = channel.take_events().expect("the receiver is handed over");
        let mut sequence = Vec::new();
        for idx in 0..9 {
            let event = timeout(Duration::from_secs(5), events.recv())
                .await
                .unwrap_or_else(|_| panic!("event {idx} never arrived: {sequence:?}"))
                .expect("the event stream stays open");
            sequence.push(event);
        }
        assert_eq!(
            sequence,
            vec![
                ChannelEvent::Lifecycle(LifecycleEvent::Connected),
                ChannelEvent::UtteranceEnd,
                ChannelEvent::AudioDelta {
                    pcm: RESPONSE_PART_1.to_vec(),
                    gen: 0,
                },
                ChannelEvent::TurnComplete,
                ChannelEvent::UtteranceEnd,
                ChannelEvent::AudioDelta {
                    pcm: RESPONSE_PART_2.to_vec(),
                    gen: 0,
                },
                ChannelEvent::TurnComplete,
                ChannelEvent::UtteranceEnd,
                ChannelEvent::ToolCall {
                    call_id: "call-2".to_string(),
                    name: "pause_playback".to_string(),
                    args: serde_json::json!({}),
                },
            ]
        );
        assert!(
            timeout(Duration::from_millis(300), events.recv())
                .await
                .is_err(),
            "no TurnComplete while the call's answer is owed"
        );
        channel.close().await.expect("clean close");
    }

    /// Issue #201's third re-arm path: the renewal recycle. The replacement
    /// connection starts between turns, so whatever boundary state the old
    /// connection accumulated died with it — the first fresh turn after
    /// `Reconnected` must commit the utterance again.
    #[tokio::test]
    async fn gemini_live_renewal_recycle_rearms_the_utterance_boundary() {
        let handle = WireServer::start().await;
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: "models/gemini-2.5-flash-native-audio-preview".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );

        let fixture = load_fixture("gemini-live-renewal-rearm");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");

        let mut events = channel.take_events().expect("the receiver is handed over");
        let mut sequence = Vec::new();
        for idx in 0..7 {
            let event = timeout(Duration::from_secs(5), events.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!("event {idx} never arrived — a boundary went missing: {sequence:?}")
                })
                .expect("the event stream stays open");
            sequence.push(event);
        }
        assert_eq!(
            sequence,
            vec![
                ChannelEvent::Lifecycle(LifecycleEvent::Connected),
                ChannelEvent::UtteranceEnd,
                ChannelEvent::AudioDelta {
                    pcm: RESPONSE_PART_1.to_vec(),
                    gen: 0,
                },
                ChannelEvent::Lifecycle(LifecycleEvent::Renewing {
                    time_left: Some(Duration::from_secs(10)),
                }),
                ChannelEvent::Lifecycle(LifecycleEvent::Reconnected { resumed: false }),
                ChannelEvent::UtteranceEnd,
                ChannelEvent::AudioDelta {
                    pcm: RESPONSE_PART_2.to_vec(),
                    gen: 0,
                },
            ],
            "a mid-turn recycle re-arms the boundary: the renewed \
             connection's first fresh turn commits again (#201)"
        );

        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned"
        );
    }

    /// B-2b (charter §6.1/§10): an Agent session's wire contract, replayed
    /// against the recorded fixture — NON_BLOCKING declarations for all
    /// eight agent tools ride the setup, Tutor vocabulary is absent, and
    /// every answer carries the async `scheduling` law (evidence reads and
    /// error acks WHEN_IDLE; successful mutation acks SILENT).
    #[cfg(feature = "book-agent")]
    #[tokio::test]
    async fn gemini_live_agent_tools_fixture_replays_green() {
        use crate::voice::tools::ToolRegistry;

        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: Some(handle.url("/ws/live")),
                model: crate::agent_session::AGENT_LIVE_MODEL.into(),
            },
            keys_with_gemini_key(),
            generation.clone(),
        );

        let fixture = load_fixture("gemini-live-agent-tools");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        let mut context = ctx();
        context.system_prompt = crate::agent_session::agent_system_prompt();
        context.excerpt = DelimitedExcerpt {
            delimited_text: String::new(),
        };
        context.tool_schemas = ToolRegistry::agent().schemas();
        channel.connect(context).await.expect("connect");
        let mut events = channel.take_events().expect("receiver handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        // search_book: the answer is the sentinel-wrapped payload shape and
        // must schedule WHEN_IDLE (the model folds evidence into its answer).
        async fn next_call(
            events: &mut tokio::sync::mpsc::Receiver<ChannelEvent>,
        ) -> (String, String) {
            loop {
                match events.recv().await.expect("event stream open") {
                    ChannelEvent::ToolCall { call_id, name, .. } => return (call_id, name),
                    ChannelEvent::UtteranceEnd => continue,
                    other => panic!("unexpected event before tool call: {other:?}"),
                }
            }
        }
        let (id, name) = next_call(&mut events).await;
        assert_eq!((id.as_str(), name.as_str()), ("agent-1", "search_book"));
        channel
            .send_tool_result(
                "agent-1",
                ToolResult::Ok(serde_json::json!({
                    "untrusted_book_data": "<<untrusted-book-excerpt t>>hits<<end-untrusted-book-excerpt t>>",
                })),
            )
            .await
            .expect("search answer sends");

        let (id, name) = next_call(&mut events).await;
        assert_eq!((id.as_str(), name.as_str()), ("agent-2", "go_to_passage"));
        channel
            .send_tool_result(
                "agent-2",
                ToolResult::Ok(serde_json::json!({"position_ms": 2000, "page": 1, "chapter": 0})),
            )
            .await
            .expect("navigation ack sends");

        let (id, name) = next_call(&mut events).await;
        assert_eq!(
            (id.as_str(), name.as_str()),
            ("agent-3", "highlight_passage")
        );
        channel
            .send_tool_result(
                "agent-3",
                ToolResult::Err {
                    code: "highlight_not_applied".into(),
                    message: "no reading surface".into(),
                },
            )
            .await
            .expect("honest refusal sends");

        let (id, name) = next_call(&mut events).await;
        assert_eq!((id.as_str(), name.as_str()), ("agent-4", "pause_playback"));
        channel
            .send_tool_result(
                "agent-4",
                ToolResult::Ok(serde_json::json!({"paused": true})),
            )
            .await
            .expect("reused-verb ack sends");

        channel.close().await.expect("close");
        replayer.await.expect("the recorded fixture replays green");
    }

    /// Tutor answers stay byte-identical: no `scheduling` field ever rides a
    /// blocking tool's response, whatever the feature state.
    #[test]
    fn tutor_tool_responses_carry_no_scheduling_field() {
        let wire = tool_response_message(
            "call-1",
            "pause_playback",
            &ToolResult::Ok(serde_json::json!({"paused": true})),
            false,
        );
        assert!(!wire.contains("scheduling"));
        let wire = tool_response_message(
            "call-2",
            "seek_relative",
            &ToolResult::Err {
                code: "rate_limited".into(),
                message: "budget".into(),
            },
            false,
        );
        assert!(!wire.contains("scheduling"));
    }

    /// The scheduling law itself, pinned per tool class: everything is
    /// WHEN_IDLE (act-then-confirm-briefly, §5.2.4) except a successful
    /// citation, whose confirmation is the visible highlight itself.
    #[cfg(feature = "book-agent")]
    #[test]
    fn agent_scheduling_law_confirms_briefly_and_keeps_citations_silent() {
        let ok = ToolResult::Ok(serde_json::json!({}));
        let err = ToolResult::Err {
            code: "x".into(),
            message: "y".into(),
        };
        for tool in [
            "search_book",
            "open_passage",
            "go_to_passage",
            "pause_playback",
            "jump_to_page",
        ] {
            assert_eq!(agent_scheduling(tool, &ok), "WHEN_IDLE", "{tool}");
            assert_eq!(agent_scheduling(tool, &err), "WHEN_IDLE", "{tool}");
        }
        assert_eq!(agent_scheduling("highlight_passage", &ok), "SILENT");
        assert_eq!(agent_scheduling("highlight_passage", &err), "WHEN_IDLE");
    }

    /// F11-U3: the resume recap renders exactly like the renewal recap — the
    /// composed framing on the instruction line, the delimited body verbatim
    /// after it, `turnComplete: false` so the seed never starts generation.
    #[test]
    fn resume_recap_renders_the_composed_framing_outside_the_delimiters() {
        use crate::voice::channel::types::{Location, RecapOrigin, UiLang};
        let origin = RecapOrigin::Continue;
        let location = Location {
            chapter: 2,
            page: Some(7),
        };
        let rendered = client_content_context_message(&ContextUpdate::ResumeRecap {
            recap: DelimitedExcerpt {
                delimited_text: "<<s>>user: what is inertia?<</s>>".into(),
            },
            origin: origin.clone(),
            location: Some(location.clone()),
            topic: Some("inertia".to_string()),
            lang: UiLang::En,
        })
        .expect("a resume recap is framed for the wire");
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["clientContent"]["turnComplete"], false);
        let text = value["clientContent"]["turns"][0]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(
            text,
            format!(
                "Context update (not a spoken user turn): {}\n<<s>>user: what is inertia?<</s>>",
                crate::voice::channel::types::resume_recap_framing(
                    &origin,
                    Some(&location),
                    Some("inertia"),
                    UiLang::En
                )
            )
        );
    }

    #[tokio::test]
    async fn gemini_live_finalize_input_transmits_audio_stream_end_over_wire() {
        let handle = WireServer::start().await;
        let keys = empty_keys();
        let mut channel = ring_channel(&handle, Arc::clone(&keys), ring(&[("KEY_A", RING_KEY_A)]));
        let server = handle.clone();
        let script = tokio::spawn(async move {
            let _setup = server.next_from_adapter().await;
            server.send(WireFrame::Text(r#"{"setupComplete":{}}"#.into()));
            server.next_from_adapter().await
        });
        channel.connect(ctx()).await.expect("connect");
        channel.finalize_input().await.expect("finalize_input");
        let frame = script.await.expect("script finished");
        match frame {
            WireFrame::Text(text) => {
                let v: serde_json::Value = serde_json::from_str(&text).expect("valid json");
                assert_eq!(
                    v.get("realtimeInput")
                        .and_then(|r| r.get("audioStreamEnd"))
                        .and_then(|e| e.as_bool()),
                    Some(true),
                    "wire frame must contain realtimeInput.audioStreamEnd: true"
                );
            }
            WireFrame::Binary(_) => panic!("expected text wire frame for audioStreamEnd"),
        }
        channel.close().await.ok();
    }

    #[tokio::test]
    async fn a_send_that_races_the_renewal_recycle_answers_retryable_instead_of_panicking() {
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: None,
                model: "models/gemini-2.5-flash-native-audio-preview-12-2025".into(),
            },
            keys_with_gemini_key(),
            Generation::new(),
        );
        channel.state = SessionState::Connected;
        *channel.writer.lock().await = None;
        let err = channel
            .send_text("hi")
            .await
            .expect_err("an empty writer slot must degrade, never panic");
        assert_eq!(err.kind, ErrorKind::SessionExpired);
        assert!(err.retryable);
    }
}
