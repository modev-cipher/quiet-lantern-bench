//! `CompatComposedChannel` — the slot-composition adapter (VA5, VOICE_AGENT
//! §3.1/§3.2; this unit carries the slot traits, credential-free config
//! types, construction, the slot-derived capability declaration, and — as of
//! the composition-loop skeleton stride — the composed connect/close
//! lifecycle, the STT-side composition loop, and the mic path; the LLM-turn
//! wiring stride added the loop's utterance→turn hand-off (finals
//! accumulated, `UtteranceEnd` commits the turn to [`LlmSlot`]), the
//! response-surfacing stride (w1b-i) the LLM-side return path (assistant
//! text deltas surface on the composed stream), and the tool-loop stride
//! (w1b-ii) the §5 round trip (`ToolCall` events forward verbatim;
//! [`SessionChannel::send_tool_result`] delegates the continuation to the
//! LLM slot, whose follow-up deltas ride the same return path); the TTS-feed
//! stride (w2) closes the speaking leg — the loop buffers the assistant's
//! reply and flushes it into [`TtsSlot::speak`] on the reply boundary
//! (`TurnComplete`), and the TTS slot's `AudioDelta`/`WordTimestamps`
//! surface out of the composed stream; and the barge-in stride (w3) wired
//! [`SessionChannel::interrupt`] — the LLM-abort-plus-TTS-cancel pair
//! documented at its impl below).
//!
//! Slot architecture (the decision this skeleton pins). VOICE_AGENT §3.2
//! defines the composition as three **per-role slot traits** — its normative
//! pseudocode is literally `trait SttSlot`, `trait LlmSlot`, `trait TtsSlot`
//! — so the slots here are exactly that: one narrow trait per role, held as
//! trait objects (`Box<dyn SttSlot>` etc.). Not `Box<dyn SessionChannel>`:
//! `SessionChannel` is a full-duplex conversation surface (tool results,
//! context refresh, barge-in), while a slot is one role of it — an STT slot
//! streams PCM in and transcript deltas out and never sees a tool result
//! (§3.2 pseudocode role comments). Not concrete-typed slots either: the
//! slot set is open — "adding a new slot implementation must require no
//! changes beyond its module, one registry entry, and one recorded
//! conformance fixture" (§3.2 slot rules) — which needs dynamic dispatch at
//! the composition point, not slot types baked into the channel's type.
//!
//! Wire provenance — the three slot wire families (per the §3.2
//! implementation rule: live official docs, URL + date in the module header):
//!
//! - **STT slot (launch: Deepgram `/v1/listen`)**: the wire is already built
//!   and fully documented in the [`super::deepgram`] module header (all
//!   provenance URLs accessed 2026-08-21): binary PCM16 WS frames upstream,
//!   JSON text control frames, `Results`/`words[]` word timing,
//!   `speech_final`/`UtteranceEnd` authoritative endpointing, NET-0001
//!   KeepAlive. The launch `SttSlot` implementation (later stride) adapts
//!   that wire family into the slot contract.
//!
//! - **LLM slot (baseline: OpenAI-compatible streaming chat completions)**:
//!   canonical reference
//!   https://platform.openai.com/docs/api-reference/chat/streaming, verified
//!   2026-08-21 against OpenAI's official machine-readable API spec (the
//!   reference site is bot-gated; the spec repo carries the same schemas):
//!   https://github.com/openai/openai-openapi (raw `master` `openapi.yaml`,
//!   accessed 2026-08-21). `POST {base_url}/chat/completions` with
//!   `"stream": true` returns data-only server-sent events; each event is an
//!   `"object": "chat.completion.chunk"` (`CreateChatCompletionStreamResponse`)
//!   whose `choices[].delta` carries `content` text fragments and
//!   `tool_calls[]` chunks (`ChatCompletionMessageToolCallChunk`: required
//!   `index`; `id`, `type: "function"`, and `function.name` arrive on a tool
//!   call's first fragment; `function.arguments` streams as JSON-string
//!   fragments the client concatenates). `choices[].finish_reason` is `null`
//!   mid-stream and ends as one of `stop | length | tool_calls |
//!   content_filter | function_call` (deprecated); the stream terminates
//!   with the `data: [DONE]` sentinel. The credential rides the
//!   `Authorization: Bearer` request header — never a query param
//!   (VOICE_AGENT §3.2 `auth-hygiene-no-query-param-creds`). Baseline this
//!   stride is the plain OpenAI-compatible shape; known per-provider quirks
//!   are banked for later strides, not built here (VA5 bank U4; §3.2
//!   LLM-slot notes — the clause is a protocol-shape term, not an
//!   allowlist): Qwen strict-JSON, DeepSeek `/beta` prefix, Groq
//!   unknown-field rejections, OpenRouter per-model variance.
//!
//! - **TTS slot (launch: Cartesia)**:
//!   https://docs.cartesia.ai/api-reference/tts/tts (accessed 2026-08-21) —
//!   WS endpoint `wss://api.cartesia.ai/tts/websocket` with the **required
//!   dated API version** `cartesia_version`, e.g. `2026-08-14` — the same
//!   single allowed value the VA3 probe already pins
//!   ([`crate::voice::keys::probe::CARTESIA_VERSION`], `Cartesia-Version`
//!   header provenance there); the pin is fixture `cartesia-version-pin` of
//!   the VA5 roster (TASKS VA5, VOICE_AGENT §3.2 provider notes: "the dated
//!   version header is required — pin it"). Audio arrives
//!   **base64-inside-JSON** `"type": "chunk"` messages (`data`, `done`,
//!   `status_code`, `step_time`, `context_id`) — the §2 jitter-buffer
//!   inflation note applies to this slot exactly as to JSON-audio channels
//!   (§3.2 Cartesia note). `"add_timestamps": true` yields
//!   `"type": "timestamps"` messages carrying `word_timestamps`
//!   (`words[]`/`start[]`/`end[]`, seconds) → [`super::TimedWord`], the §11
//!   word-level follow-along source and this channel's `word_timestamps`
//!   capability. Cancellation is `{"context_id": …, "cancel": true}` — the
//!   barge-in mapping — and frames already emitted for that context must be
//!   drained, per the `cartesia-cancel-drain` roster fixture. Auth is the
//!   `X-API-Key` header (the doc's browser-side `access_token` query-param
//!   alternative is banned here — `auth-hygiene-no-query-param-creds`);
//!   the free tier is non-commercial → advisory label, never blocking
//!   (§3.2 provider notes).
//!
//! Capability posture (derived per slot, VOICE_AGENT §3.2 slot rules: "the
//! channel's `ChannelCapabilities` is derived from the active slots"): see
//! [`SessionChannel::capabilities`] below for the per-flag reasoning.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

use crate::voice::audio::generation::Generation;
use crate::voice::keys::KeyManager;

use super::{
    ChannelCapabilities, ChannelError, ChannelEvent, ContextUpdate, ErrorKind, LifecycleEvent,
    Role, SessionChannel, SessionContext, ToolResult, VadMode,
};

const EVENT_CAPACITY: usize = 4096;

/// How long `close` waits for the composition loop to drain after the slot
/// shutdowns ended its input stream (the deepgram.rs shutdown-budget shape).
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

/// The STT role of the composition (VOICE_AGENT §3.2 pseudocode: "stream PCM
/// in → transcript deltas + end-of-utterance"). The skeleton stride declared
/// this trait capability-only; this stride adds the streaming surface the
/// §3.2 pseudocode names, kept to exactly the role:
///
/// - `connect` takes **no** `SessionContext`: everything in it (system
///   prompt, excerpt, tool schemas) is prompt-side LLM traffic, and the STT
///   role "never sees a tool result" or any other LLM traffic (§3.2
///   pseudocode role comments) — an implementation over a `SessionChannel`
///   wire synthesizes whatever empty context that wire's connect signature
///   needs.
/// - The output stream reuses [`ChannelEvent`] rather than minting a narrow
///   slot enum — the composed channel forwards slot outputs onto its own
///   uniform event stream, so a shared vocabulary avoids a translation
///   layer — but under a narrowed contract: an `SttSlot` stream carries
///   **only** `TranscriptDelta { role: User }`, `WordTimestamps`,
///   `UtteranceEnd` (exactly one per utterance — the implementation owns the
///   dedupe, event.rs contract), `Lifecycle`, and `Error`. `AudioDelta`,
///   `ToolCall`, and `TeacherAnchor` never appear: nothing LLM- or TTS-ish
///   is reachable on an STT wire.
/// - `WordTimestamps` follows only a **final** delta — interim word lists
///   fluctuate and are never emitted (the launch slot's `deepgram::read_loop`
///   rule, promoted here to the slot contract) — so it doubles as the
///   stream's finality signal: the composed loop commits the delta it
///   follows as settled utterance text, and everything else is a
///   display-only interim. A future timestampless slot (e.g. plain
///   Whisper-compatible HTTP) needs an explicit finality signal instead;
///   that slot's stride decides it.
#[async_trait]
pub trait SttSlot: Send {
    /// Whether this slot's wire has authoritative endpointing and therefore
    /// owns end-of-utterance for the whole session (§3.2 turn-detection
    /// ownership rule — e.g. Deepgram Nova `speech_final`/`UtteranceEnd` →
    /// `true`; a plain Whisper-compatible HTTP slot → `false`, client VAD
    /// owns turns). The mic-ring replay boundary (§3) uses the owner's last
    /// committed end-of-utterance as its acknowledgement watermark; the
    /// harness owns the replay itself.
    fn owns_endpointing(&self) -> bool;

    /// Opens the slot's transport, resolving its key through the VA3
    /// keyring under the slot's own base-URL identity (§3.2/§7 per-slot
    /// per-base-URL rule). Emits `Lifecycle(Connected)` on the slot's event
    /// stream once the provider accepted the stream.
    async fn connect(&mut self) -> Result<(), ChannelError>;

    /// Ends the slot's transport cleanly (e.g. the Deepgram `CloseStream`
    /// flush). Idempotent, like [`SessionChannel::close`].
    async fn close(&mut self) -> Result<(), ChannelError>;

    /// Streams one chunk of 16 kHz mono PCM16 mic audio into the slot — the
    /// "stream PCM in" half of the §3.2 role.
    async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError>;

    /// Hands over the slot's owned event receiver, `Some` exactly once (the
    /// [`SessionChannel::take_events`] shape, same rationale). The stream is
    /// bound to the narrowed vocabulary documented on the trait.
    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>>;
}

/// The LLM role of the composition (VOICE_AGENT §3.2 pseudocode:
/// "OpenAI-compatible chat: text + tool calls"). Contributes no capability
/// flag — `vad_mode` belongs to the STT slot and `word_timestamps` to the
/// TTS slot (§3.1 table, §3.2 slot rules). The skeleton stride declared this
/// trait a bare marker; this stride adds the chat surface the §3.2
/// pseudocode names — the [`SttSlot`] trajectory exactly (capability-only at
/// the skeleton, the role surface when its first implementation lands),
/// forced the same way: a marker has no method an implementation could
/// attach the role to. Kept to exactly the role:
///
/// - No `connect`/`close`: the slot is stateless per-turn HTTP (this
///   channel's `session_renewal` reasoning below) — there is no transport to
///   dial or flush. The prompt-side half of the composed connect is
///   [`LlmSlot::install_context`] instead: everything a [`SessionContext`]
///   carries (system prompt, excerpt, tool schemas) is LLM traffic (§3.2
///   pseudocode role comments), so it lands on this slot and no other.
/// - The output stream reuses [`ChannelEvent`] under a narrowed contract
///   (the [`SttSlot`] rationale): an `LlmSlot` stream carries **only**
///   `TranscriptDelta { role: Assistant }`, `ToolCall`, `TurnComplete` (the
///   reply boundary — exactly one per completed reply, after the finish
///   that leaves no tool answers owed; the composed loop's TTS flush
///   trigger, event.rs contract), `Lifecycle`, and `Error`. `AudioDelta`
///   and `WordTimestamps` are TTS-role, `UtteranceEnd` is STT-role, and
///   `TeacherAnchor` is the harness's normalization of a `teacher_anchor`
///   tool call (event.rs contract) — none of them is reachable on a chat
///   wire.
#[async_trait]
pub trait LlmSlot: Send {
    /// Installs (or wholesale replaces) the prompt-side session context —
    /// this slot's share of the [`SessionChannel::connect`] context.
    /// Synchronous and infallible: the slot is per-turn HTTP, so this is
    /// bookkeeping rendered into the next request's messages, not wire
    /// traffic.
    fn install_context(&mut self, ctx: SessionContext);

    /// Appends one user turn and streams the model's reply onto the slot's
    /// event stream. Both user-input paths land here: the STT slot's
    /// committed utterance and typed text ([`SessionChannel::send_text`],
    /// §3 text-input path).
    async fn send_user_turn(&mut self, text: &str) -> Result<(), ChannelError>;

    /// Appends the `role: "tool"` message answering `call_id` and continues
    /// the chat stream (§5; OpenAI-compat provenance in the module header).
    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: ToolResult,
    ) -> Result<(), ChannelError>;

    /// Mid-session context refresh (§4): `ChapterBoundary` re-renders the
    /// system/context messages; `PlaybackPosition` is lightweight state —
    /// never a blind mid-turn re-prompt (the harness owns when this fires).
    async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError>;

    /// Barge-in (§2 rule 2): aborts the in-flight streaming response and
    /// drops its unread tail.
    async fn interrupt(&mut self) -> Result<(), ChannelError>;

    /// Hands over the slot's owned event receiver, `Some` exactly once (the
    /// [`SessionChannel::take_events`] shape, same rationale), bound to the
    /// narrowed vocabulary documented on the trait.
    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>>;
}

/// The TTS role of the composition (VOICE_AGENT §3.2 pseudocode: "stream
/// text in → PCM deltas (+ word timestamps if supported)"). The skeleton
/// stride declared this trait capability-only; this stride adds the speaking
/// surface the §3.2 pseudocode names — the [`SttSlot`]/[`LlmSlot`] trajectory
/// exactly, forced the same way: a capability-only trait has no method an
/// implementation could attach the role to. Kept to exactly the role:
///
/// - The output stream reuses [`ChannelEvent`] under a narrowed contract
///   (the [`SttSlot`] rationale): a `TtsSlot` stream carries **only**
///   `AudioDelta`, `WordTimestamps`, `Lifecycle`, and `Error`.
///   `TranscriptDelta` is STT/LLM-role (this slot consumes text, it never
///   narrates any), `UtteranceEnd` is STT-role, `ToolCall` is LLM-role, and
///   `TeacherAnchor` is the harness's normalization of a tool call (event.rs
///   contract) — none of them is reachable on a synthesis wire.
///
/// - **Generation tagging is loop-owned, never read-time** (the gemini.rs
///   turn-scoping lesson, m3-fix4): response-scoped payloads must carry the
///   generation of the response that produced them, and the composed loop —
///   not the slot — owns response turns. So `speak` takes the turn's `gen`
///   alongside its `context_id`: the loop snapshots the barge-in generation
///   once when it opens the TTS turn and hands both in; the slot stamps that
///   same snapshot on every `AudioDelta`/`WordTimestamps` of that context.
///   The slot holds no `Generation` handle at all — read-time tagging, which
///   would launder a superseded turn's tail as live, is structurally
///   unreachable. A mid-turn bump therefore leaves the open context's tag
///   stale and its remaining deltas die at the playback gate (§2 rule 2) —
///   and a cancelled context's still-in-flight frames surface the same way,
///   old-tagged, for that same gate to refuse (the [`TtsSlot::cancel`]
///   contract: honest tagging, never local censorship).
#[async_trait]
pub trait TtsSlot: Send {
    /// Whether this slot emits word-aligned timing for [`super::TimedWord`]
    /// (§3.2 slot rules; §11: Cartesia `add_timestamps` → `true`; Fish
    /// `s2.1-pro-free` has no timestamps → `false`, sentence-level degrade).
    fn word_timestamps(&self) -> bool;

    /// Opens the slot's transport, resolving its key through the VA3
    /// keyring under the slot's own base-URL identity (§3.2/§7 per-slot
    /// per-base-URL rule). Emits `Lifecycle(Connected)` on the slot's event
    /// stream once the provider accepted the stream.
    async fn connect(&mut self) -> Result<(), ChannelError>;

    /// Ends the slot's transport cleanly. Idempotent, like
    /// [`SessionChannel::close`].
    async fn close(&mut self) -> Result<(), ChannelError>;

    /// Synthesizes one response turn's text under the loop-chosen
    /// `context_id` — the "stream text in" half of the §3.2 role — tagging
    /// every resulting `AudioDelta`/`WordTimestamps` with `gen`, the turn's
    /// generation snapshot (loop-owned tagging, trait doc above). A `speak`
    /// while an earlier context is still streaming supersedes it: the old
    /// context's remaining frames drain unsurfaced, exactly as if cancelled.
    async fn speak(&mut self, text: &str, context_id: &str, gen: u64) -> Result<(), ChannelError>;

    /// The network half of barge-in for this role (§2 rule 2): asks the
    /// provider to cancel `context_id`, which stops **future** frames only
    /// ("the network round-trip only stops future deltas"). Frames of the
    /// cancelled context still in flight surface under their speak-time
    /// generation tag — the playback gate, not the slot, is the arbiter of
    /// staleness (conformance check 3's mid-air invariant); a slot that
    /// discards them locally censors what the gate is owed.
    async fn cancel(&mut self, context_id: &str) -> Result<(), ChannelError>;

    /// Hands over the slot's owned event receiver, `Some` exactly once (the
    /// [`SessionChannel::take_events`] shape, same rationale), bound to the
    /// narrowed vocabulary documented on the trait.
    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>>;
}

/// STT-slot configuration. Credentials are deliberately absent, as in every
/// VA5 adapter: the slot's key is resolved at connect time through the VA3
/// [`KeyManager`] under the slot's own base-URL identity (per-slot **and**
/// per-base-URL keys, VOICE_AGENT §3.2/§7) — for the launch Deepgram slot
/// that is `Provider::Deepgram.default_base_url()`, the same identity split
/// `super::deepgram` uses.
#[derive(Clone, Debug)]
pub struct SttSlotConfig {
    /// Full WS endpoint replacing the slot implementation's default when set
    /// (proxies and recorded-fixture servers).
    pub endpoint_override: Option<String>,
    /// Streaming model name, e.g. `nova-3` (§3.2: Flux vs Nova is a
    /// query-param change evaluated within the slot).
    pub model: String,
}

/// LLM-slot configuration (credential-free, key via [`KeyManager`]). The
/// base URL is required and IS the key identity: `Provider::OpenaiCompat`
/// has no default base URL (`voice::keys::probe`), so the key is stored and
/// resolved under this exact URL (§3.2 per-base-URL rule). Custom base URLs
/// are HTTPS-only behind the first-use consent interstitial naming every
/// host contacted (§7 — this channel is the only one that accepts them,
/// §3.1 table).
#[derive(Clone, Debug)]
pub struct LlmSlotConfig {
    /// OpenAI-compatible base URL; the slot appends `/chat/completions`
    /// (module header, OpenAI-compat provenance).
    pub base_url: String,
    /// Model name for the request body's `model` field.
    pub model: String,
}

/// TTS-slot configuration (credential-free, key via [`KeyManager`] — launch
/// Cartesia identity is `Provider::Cartesia.default_base_url()`,
/// `https://api.cartesia.ai`, with the dated version pin
/// [`crate::voice::keys::probe::CARTESIA_VERSION`]).
#[derive(Clone, Debug)]
pub struct TtsSlotConfig {
    /// Full WS endpoint replacing the slot implementation's default when set
    /// (proxies and recorded-fixture servers).
    pub endpoint_override: Option<String>,
    /// Model name (Cartesia request `model_id`; wire mapping recorded in the
    /// [`super::cartesia`] module header against the live doc).
    pub model: String,
    /// Voice id (Cartesia request `voice`, the doc's voice-ID-string form —
    /// a required generation-request field, so it is session config exactly
    /// like `model`; provenance in the [`super::cartesia`] module header).
    pub voice: String,
}

/// The composed channel's configuration: one credential-free config per slot
/// (VOICE_AGENT §3.2 — keys are per-slot and per-base-URL; the §7 keyring
/// rule applies to each slot independently).
#[derive(Clone, Debug)]
pub struct CompatComposedConfig {
    pub stt: SttSlotConfig,
    pub llm: LlmSlotConfig,
    pub tts: TtsSlotConfig,
}

/// Session state, the uniform contract the trait documents: connect is valid
/// exactly once from `New`; a closed (or connect-failed) channel never
/// reopens — the slots underneath are themselves single-shot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    New,
    Connected,
    Closed,
}

/// Tags a slot failure with the slot role that produced it — connect is
/// sequential and fail-fast, and which slot failed must be visible to the
/// harness/user (§3.2: the composition is three independent providers, three
/// independent keys). The `kind`/`retryable` classification passes through
/// untouched: the §9 ladder keys on those, never on the message.
fn slot_error(slot: &str, err: ChannelError) -> ChannelError {
    ChannelError::new(
        err.kind,
        err.retryable,
        format!("{slot} slot: {}", err.message),
    )
}

/// The composition loop. Consumes the STT slot's event stream for the life
/// of the session, forwards the user-facing slice onto the composed
/// channel's stream, assembles the pending user turn and hands it to the LLM
/// slot when the STT slot commits the utterance (LLM-turn wiring stride),
/// consumes the LLM slot's own stream alongside — surfacing the assistant's
/// reply text, the assembled tool calls, and slot-tagged errors (w1b) — and,
/// the TTS-feed stride (w2), speaks the reply: assistant deltas accumulate
/// into `reply` as they surface, and the reply boundary (`TurnComplete`,
/// [`LlmSlot`] contract) flushes the buffer into one [`TtsSlot::speak`],
/// whose `AudioDelta`/`WordTimestamps` come back on the TTS slot's stream
/// and forward here onto the composed stream. Ends when the STT stream does
/// — slot shutdown at `close` ends it, and `close` joins it.
///
/// TTS granularity — whole-turn, decided by the slot's own input contract
/// ([`TtsSlot::speak`]): a speak frame carries one response turn's
/// **complete** text (the Cartesia wire's `continue: false`), and a later
/// speak supersedes the previous context, draining its still-streaming
/// frames — so incremental per-delta or sentence-boundary speaks would cut
/// off the audio of every chunk but the last. No splitter, one speak per
/// reply, fired at the boundary.
///
/// Turn assembly (interims fluctuate and are display-only): `last_delta`
/// always holds the newest user delta — within a segment each interim
/// replaces the last — and only the finality signal ([`SttSlot`] contract:
/// the `WordTimestamps` that follows only finals) promotes it into
/// `utterance`, the settled segments of the turn in progress. `UtteranceEnd`
/// commits: a non-empty `utterance` becomes one `LlmSlot::send_user_turn`
/// call and both buffers reset; an empty one (silence the endpointing
/// committed with nothing settled) makes no LLM call at all.
// One spawn site, and each arg is a distinct shared handle — a bundling
// struct would exist only to appease the count.
#[allow(clippy::too_many_arguments)]
async fn composition_loop(
    mut stt_events: mpsc::Receiver<ChannelEvent>,
    mut llm_events: Option<mpsc::Receiver<ChannelEvent>>,
    mut tts_events: Option<mpsc::Receiver<ChannelEvent>>,
    llm: Arc<Mutex<Box<dyn LlmSlot>>>,
    tts: Arc<Mutex<Box<dyn TtsSlot>>>,
    generation: Generation,
    spoken_turns: Arc<AtomicU64>,
    events: mpsc::Sender<ChannelEvent>,
) {
    let mut last_delta = String::new();
    let mut utterance = String::new();
    // The assistant reply in progress, flushed to TTS at the boundary — and
    // the generation it was committed under (w3): a boundary arriving after
    // a barge-in bump is a superseded reply, dropped at the flush check
    // below. `spoken_turns` is the loop-owned context-id sequence the speaks
    // ride under, shared with the channel so `interrupt` can address the
    // live context (struct field doc).
    let mut reply = String::new();
    let mut reply_gen = generation.current();
    loop {
        // `select!` over both slot streams rather than a spawned forwarder:
        // one task keeps close's single join point, and the STT stream's end
        // stays the loop's only terminator.
        tokio::select! {
            stt_event = stt_events.recv() => {
                let Some(event) = stt_event else { return };
                let forwarded = match event {
            // Live captions: the user's own words surface immediately (§11);
            // the same text is the turn-in-progress candidate the next
            // finality signal settles.
            ChannelEvent::TranscriptDelta {
                role: Role::User,
                text,
            } => {
                last_delta.clone_from(&text);
                ChannelEvent::TranscriptDelta {
                    role: Role::User,
                    text,
                }
            }
            // The turn trigger (§3.2 turn-detection ownership: the STT slot
            // owns end-of-utterance for the whole session). Passed through —
            // the harness tracks its mic-ring replay watermark off it — and
            // the accumulated utterance goes to the LLM slot. The direct
            // await is safe: `send_user_turn` streams the reply onto the
            // slot's own event stream from a task it spawns (openai_compat
            // `spawn_turn`), it never blocks on response consumption.
            ChannelEvent::UtteranceEnd => {
                // An unsettled interim tail is display-only; it never
                // crosses the utterance boundary.
                last_delta.clear();
                let turn = std::mem::take(&mut utterance);
                if !turn.is_empty() {
                    // A new turn supersedes any in-flight reply slot-side
                    // (openai_compat spawn_turn aborts the stale task), so a
                    // partial reply buffered without its boundary is dead
                    // text — it must never leak into the next turn's speak.
                    reply.clear();
                    // The commit-time generation: the reply this turn streams
                    // back is live only while no barge-in bumps past it (the
                    // flush check at TurnComplete).
                    reply_gen = generation.current();
                    if let Err(err) = llm.lock().await.send_user_turn(&turn).await {
                        let failure = ChannelEvent::Error(slot_error("llm", err));
                        if events.send(failure).await.is_err() {
                            return;
                        }
                    }
                    // The fired turn's reply streams back on the LLM slot's
                    // own event stream — the select's LLM branch below
                    // surfaces it (w1b-i).
                }
                ChannelEvent::UtteranceEnd
            }
            // User-speech word timing, passed through under the tag the slot
            // stamped (§11 follow-along). Also the finality signal (SttSlot
            // contract): the delta it follows is settled — promote it into
            // the pending turn.
            words @ ChannelEvent::WordTimestamps { .. } => {
                if !last_delta.is_empty() {
                    if !utterance.is_empty() {
                        utterance.push(' ');
                    }
                    utterance.push_str(&std::mem::take(&mut last_delta));
                }
                words
            }
            // The slot's own Connected is slot-internal plumbing: the
            // composed channel emits its single Connected from `connect`,
            // after ALL three slots are ready — forwarding this one would
            // announce a session that is still partially dialing.
            ChannelEvent::Lifecycle(LifecycleEvent::Connected) => continue,
            // VA5 (wiring stride): the composed renewal/expiry policy.
            // Nothing is dropped today — no launch slot wire announces
            // renewal (both adapters declare `session_renewal: false`).
            ChannelEvent::Lifecycle(_) => continue,
            // Slot failure → channel failure with the failing slot visible,
            // classification untouched (see [`slot_error`]).
            ChannelEvent::Error(err) => ChannelEvent::Error(slot_error("stt", err)),
            // Outside the SttSlot narrowed vocabulary (trait doc): nothing
            // LLM- or TTS-ish is reachable on an STT stream.
            _ => continue,
        };
                if events.send(forwarded).await.is_err() {
                    return;
                }
            }
            // The async block defers the borrow: with the branch disabled
            // (`llm_events.is_none()`) the expression is built but never
            // polled, so the expect can't fire.
            llm_event = async { llm_events.as_mut().expect("branch gated on is_some").recv().await },
                if llm_events.is_some() =>
            {
                let forwarded = match llm_event {
                    // The slot dropped its sender mid-session; the STT side
                    // keeps flowing and the branch stays disabled.
                    None => {
                        llm_events = None;
                        continue;
                    }
                    // The assistant's streamed reply text, surfaced as-is
                    // (§11 live text) and accumulated for the boundary's
                    // TTS flush (whole-turn granularity, loop doc).
                    Some(ChannelEvent::TranscriptDelta {
                        role: Role::Assistant,
                        text,
                    }) => {
                        reply.push_str(&text);
                        ChannelEvent::TranscriptDelta {
                            role: Role::Assistant,
                            text,
                        }
                    }
                    // The reply boundary (LlmSlot contract): the buffered
                    // reply becomes ONE speak under a loop-chosen context id
                    // and the generation snapshot taken here — loop-owned
                    // tagging, TtsSlot trait doc. Consumed, never forwarded:
                    // the boundary is composition-internal vocabulary
                    // (event.rs contract). An empty reply (a turn that was
                    // all tool calls, no prose) speaks nothing.
                    Some(ChannelEvent::TurnComplete) => {
                        let text = std::mem::take(&mut reply);
                        // The flush-time staleness check (w3, §2 rule 2): a
                        // boundary already queued when barge-in bumped the
                        // generation belongs to the superseded reply.
                        // `LlmSlot::interrupt` aborts the stream, but it
                        // cannot unsend this event — speaking the buffered
                        // text here would launder dead text under the LIVE
                        // gen, straight past the playback gate. Dropped, and
                        // the take above already cleared the buffer.
                        let gen = generation.current();
                        if gen != reply_gen {
                            continue;
                        }
                        if !text.is_empty() {
                            let turn = spoken_turns.fetch_add(1, Ordering::SeqCst) + 1;
                            let context_id = format!("turn-{turn}");
                            if let Err(err) =
                                tts.lock().await.speak(&text, &context_id, gen).await
                            {
                                let failure = ChannelEvent::Error(slot_error("tts", err));
                                if events.send(failure).await.is_err() {
                                    return;
                                }
                            }
                        }
                        continue;
                    }
                    // Slot failure → channel failure with the failing slot
                    // visible, classification untouched (see [`slot_error`]).
                    // The errored turn's boundary never comes, so its
                    // partial reply is dead text (the UtteranceEnd rule).
                    Some(ChannelEvent::Error(err)) => {
                        reply.clear();
                        ChannelEvent::Error(slot_error("llm", err))
                    }
                    // The assembled tool call, forwarded verbatim (w1b-ii,
                    // §5): the harness executes it and answers through
                    // [`SessionChannel::send_tool_result`]; the slot already
                    // registered the id as pending before emitting
                    // (openai_compat history-first rule).
                    Some(call @ ChannelEvent::ToolCall { .. }) => call,
                    // Outside the LlmSlot narrowed vocabulary (trait doc).
                    Some(_) => continue,
                };
                if events.send(forwarded).await.is_err() {
                    return;
                }
            }
            // The speaking end's return path (w2): the same deferred-borrow
            // shape as the LLM branch.
            tts_event = async { tts_events.as_mut().expect("branch gated on is_some").recv().await },
                if tts_events.is_some() =>
            {
                let forwarded = match tts_event {
                    // The slot dropped its sender mid-session; the rest of
                    // the session keeps flowing and the branch stays disabled.
                    None => {
                        tts_events = None;
                        continue;
                    }
                    // The synthesized reply audio and its word timing,
                    // surfaced as-is: the slot already stamped each with the
                    // speak-time generation snapshot (loop-owned tagging,
                    // TtsSlot trait doc), so the playback gate can key on it.
                    Some(audio @ ChannelEvent::AudioDelta { .. }) => audio,
                    Some(words @ ChannelEvent::WordTimestamps { .. }) => words,
                    // Slot failure → channel failure with the failing slot
                    // visible, classification untouched (see [`slot_error`]).
                    Some(ChannelEvent::Error(err)) => {
                        ChannelEvent::Error(slot_error("tts", err))
                    }
                    // Outside the TtsSlot narrowed vocabulary (trait doc) —
                    // and the slot-internal Connected is not the channel's
                    // (the STT-branch rule).
                    Some(_) => continue,
                };
                if events.send(forwarded).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// The slot-composition session channel (VOICE_AGENT §3.1/§3.2): composed
/// connect/close lifecycle, the composition loop across all three slot
/// streams (mic → STT → committed turn → LLM → buffered reply → TTS →
/// audio), the mic path, and the barge-in cancel (w3: `interrupt` below).
pub struct CompatComposedChannel {
    // Held from construction so the slot-wire strides slot in without
    // reshaping the struct; unused until those strides land.
    #[allow(dead_code)]
    config: CompatComposedConfig,
    stt: Box<dyn SttSlot>,
    /// Shared with the [`composition_loop`] task, which drives the
    /// utterance→turn hand-off; the channel's own methods (connect's
    /// `install_context`, close's `interrupt`, the send_* strides) take the
    /// same lock.
    llm: Arc<Mutex<Box<dyn LlmSlot>>>,
    /// Shared with the [`composition_loop`] task, which drives the reply→
    /// speak flush (w2); the channel's own connect/close (and the w3 cancel)
    /// take the same lock.
    tts: Arc<Mutex<Box<dyn TtsSlot>>>,
    /// [`TtsSlot::word_timestamps`], snapshotted at construction: the flag
    /// is stable for the slot's lifetime (§3.2 slot rules) and the sync
    /// `capabilities` cannot reach through the async slot lock.
    tts_word_timestamps: bool,
    #[allow(dead_code)]
    keys: Arc<KeyManager>,
    /// Cloned into the [`composition_loop`], which snapshots it per TTS turn
    /// (loop-owned generation tagging, [`TtsSlot`] trait doc).
    generation: Generation,
    /// The loop-owned TTS context ordinal (`turn-{n}`), shared with the
    /// [`composition_loop`] (which increments it per flush) so `interrupt`
    /// can address the live context for the network-half cancel (w3). Zero
    /// means no speak has fired yet — nothing to cancel.
    spoken_turns: Arc<AtomicU64>,
    state: SessionState,
    /// The [`composition_loop`] task; joined (budgeted) at `close`.
    composition: Option<JoinHandle<()>>,
    /// The event pair exists from construction: take_events must yield the
    /// receiver pre-connect (conformance check 1, matching MockChannel's
    /// build-time pair); connect and the composition loop only clone this
    /// sender.
    events_tx: Option<mpsc::Sender<ChannelEvent>>,
    events: Option<mpsc::Receiver<ChannelEvent>>,
}

impl CompatComposedChannel {
    pub fn new(
        config: CompatComposedConfig,
        stt: Box<dyn SttSlot>,
        llm: Box<dyn LlmSlot>,
        tts: Box<dyn TtsSlot>,
        keys: Arc<KeyManager>,
        generation: Generation,
    ) -> Self {
        let (events_tx, events) = mpsc::channel(EVENT_CAPACITY);
        let tts_word_timestamps = tts.word_timestamps();
        Self {
            config,
            stt,
            llm: Arc::new(Mutex::new(llm)),
            tts: Arc::new(Mutex::new(tts)),
            tts_word_timestamps,
            keys,
            generation,
            spoken_turns: Arc::new(AtomicU64::new(0)),
            state: SessionState::New,
            composition: None,
            events_tx: Some(events_tx),
            events: Some(events),
        }
    }

    /// Test-only: the shared LLM-slot handle. The composed provider's
    /// scripted side is per-turn HTTP — there is no server-push lever — so
    /// the conformance rig's "the provider emits an assistant transcript"
    /// IS the next scripted turn fired through the same slot the channel
    /// drives; the reply streams back onto the composition loop's LLM
    /// branch exactly like any other turn.
    #[cfg(test)]
    pub(crate) fn llm_slot_handle(&self) -> Arc<Mutex<Box<dyn LlmSlot>>> {
        Arc::clone(&self.llm)
    }

    /// Test-only: the shared TTS-slot handle plus the loop's spoken-turn
    /// counter — the conformance rig's context-opening lever. The suite
    /// treats the provider as a push wire (check 3 admits nothing but
    /// `AudioDelta`s on the stream), while the composed loop only opens
    /// synthesis contexts by speaking a reply — and every reply surfaces
    /// transcript events first. So when no reply is in flight, the rig
    /// opens the context exactly the way the loop's flush does — same
    /// `turn-{n}` naming, same counter, through the same slot — keeping
    /// `interrupt`'s cancel addressing and the slot's tagging
    /// production-true.
    #[cfg(test)]
    pub(crate) fn tts_slot_handle(&self) -> (Arc<Mutex<Box<dyn TtsSlot>>>, Arc<AtomicU64>) {
        (Arc::clone(&self.tts), Arc::clone(&self.spoken_turns))
    }
}

#[async_trait]
impl SessionChannel for CompatComposedChannel {
    async fn connect(&mut self, ctx: SessionContext) -> Result<(), ChannelError> {
        if self.state != SessionState::New {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("connect on a channel in state {:?}", self.state),
            ));
        }
        // Sequential and fail-fast, each failure slot-tagged (§3.2: three
        // independent providers, three independent keys — which one failed
        // must be visible). Each slot resolves its own key through the
        // shared KeyManager under its own base-URL identity (§3.2/§7),
        // inside its connect. A failed composed connect retires the channel:
        // the slots are single-shot, so there is nothing to retry through
        // this instance — already-connected slots are best-effort closed.
        if let Err(err) = self.stt.connect().await {
            self.state = SessionState::Closed;
            return Err(slot_error("stt", err));
        }
        // The prompt-side half of the composed connect: everything the
        // SessionContext carries is LLM traffic ([`LlmSlot`] trait doc), so
        // it lands on this slot and no other. Infallible bookkeeping — the
        // stateless slot renders it into the next request.
        self.llm.lock().await.install_context(ctx);
        if let Err(err) = self.tts.lock().await.connect().await {
            let _ = self.stt.close().await;
            self.state = SessionState::Closed;
            return Err(slot_error("tts", err));
        }

        let stt_events = self
            .stt
            .take_events()
            .expect("a freshly connected STT slot still holds its receiver");
        // The return paths of the composition (w1b-i, w2): the loop consumes
        // the LLM and TTS slots' streams too. `None` (a slot without a
        // stream, e.g. capability-only test doubles) just disables that
        // select branch.
        let llm_events = self.llm.lock().await.take_events();
        let tts_events = self.tts.lock().await.take_events();
        let tx = self
            .events_tx
            .clone()
            .expect("a New channel still holds its construction-time sender");
        // Store-before-emit: the channel is Connected before anyone can
        // observe the Connected event, and the event goes out before the
        // composition loop spawns so nothing the loop forwards can precede
        // it on the composed stream.
        self.state = SessionState::Connected;
        let _ = tx.try_send(ChannelEvent::Lifecycle(LifecycleEvent::Connected));
        self.composition = Some(tokio::spawn(composition_loop(
            stt_events,
            llm_events,
            tts_events,
            Arc::clone(&self.llm),
            Arc::clone(&self.tts),
            self.generation.clone(),
            Arc::clone(&self.spoken_turns),
            tx,
        )));
        Ok(())
    }

    async fn close(&mut self) -> Result<(), ChannelError> {
        if self.state == SessionState::Closed {
            return Ok(());
        }
        self.state = SessionState::Closed;
        self.events_tx = None;
        // Reverse connect order (§3): the speaking end first, then the LLM
        // slot's only shutdown — aborting any in-flight turn (stateless HTTP
        // holds no transport to close) — then the STT slot, whose
        // CloseStream flush ends the composition loop's input stream. Every
        // slot gets its shutdown even if an earlier one failed; the first
        // failure is what surfaces, slot-tagged.
        let mut first_failure = None;
        if let Err(err) = self.tts.lock().await.close().await {
            first_failure.get_or_insert(slot_error("tts", err));
        }
        let _ = self.llm.lock().await.interrupt().await;
        if let Err(err) = self.stt.close().await {
            first_failure.get_or_insert(slot_error("stt", err));
        }
        // The STT shutdown ended the slot's event stream, so the loop drains
        // and exits on its own; the budget only guards a wedged slot.
        if let Some(mut composition) = self.composition.take() {
            if timeout(SHUTDOWN_BUDGET, &mut composition).await.is_err() {
                composition.abort();
            }
        }
        match first_failure {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
        // The uniform state gate first (trait contract), then straight
        // through to the STT slot — the upstream half of the composed data
        // plane (§2; §3.2 SttSlot: "stream PCM in"). The slot's own error
        // classification passes through untouched on this hot path.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_audio on a channel in state {:?}", self.state),
            ));
        }
        self.stt.send_audio(pcm_16k_mono).await
    }

    async fn send_text(&mut self, text: &str) -> Result<(), ChannelError> {
        // The uniform state gate first (trait contract), then straight to
        // the LLM slot as ONE user turn — the §3 text-input path bypasses
        // the STT slot entirely (there is no audio to transcribe; §4 routes
        // text to the LLM slot). The reply's assistant deltas ride the
        // composition loop's already-wired LLM branch, exactly like a tool
        // continuation, so no new plumbing exists here either.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_text on a channel in state {:?}", self.state),
            ));
        }
        self.llm
            .lock()
            .await
            .send_user_turn(text)
            .await
            .map_err(|err| slot_error("llm", err))
    }

    async fn finalize_input(&mut self) -> Result<(), ChannelError> {
        tracing::debug!(
            seam = "compat_composed",
            "finalize_input called on composed channel"
        );
        Ok(())
    }

    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: ToolResult,
    ) -> Result<(), ChannelError> {
        // The uniform state gate first (trait contract, the gemini.rs/
        // deepgram_agent.rs ordering), then straight to the LLM slot — the
        // slot owns the continuation (§5). Its pre-flight rejections
        // (unknown or already-answered call, no usable key) return through
        // this Result slot-tagged; the continuation's wire failures and
        // follow-up assistant deltas both ride the composition loop's
        // already-wired LLM branch, so no new plumbing exists here.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_tool_result on a channel in state {:?}", self.state),
            ));
        }
        self.llm
            .lock()
            .await
            .send_tool_result(call_id, result)
            .await
            .map_err(|err| slot_error("llm", err))
    }

    async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError> {
        // The uniform state gate first (trait contract), then straight to
        // the LLM slot — prompt context is LLM-slot business (§4), and the
        // slot's stateless HTTP makes the refresh a pure history edit the
        // next request carries. Slot rejections return slot-tagged, the
        // send_tool_result shape.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("update_context on a channel in state {:?}", self.state),
            ));
        }
        self.llm
            .lock()
            .await
            .update_context(update)
            .await
            .map_err(|err| slot_error("llm", err))
    }

    async fn interrupt(&mut self) -> Result<(), ChannelError> {
        // VA5 w3: barge-in cancels TTS. Trigger: the harness's explicit
        // `interrupt()` through [`super::barge_in`] — the §2 rule 2 driver.
        // User-speech-triggered barge-in is the app layer's job: it calls
        // `barge_in` off its own speech signal; nothing in this channel
        // fires it. By the helper's bump-before-cancel order ("local stop
        // first, network second") the generation is ALREADY bumped when
        // this runs — no bump here (unlike gemini.rs, whose provider has no
        // cancel frame): the playback gate already refuses the old gen, and
        // every frame of the cancelled context still in flight — before OR
        // after the cancel goes out — surfaces under its speak-time (old)
        // tag ([`TtsSlot::cancel`] contract, check 3's mid-air invariant) —
        // honest tagging is this channel's whole duty; refusing them is the
        // gate's.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("interrupt on a channel in state {:?}", self.state),
            ));
        }
        // Both halves run even if the first fails — a dead LLM abort must
        // not leave the TTS speaking through a barge-in; the first failure
        // is what surfaces, slot-tagged (the close-path rule).
        //
        // LLM half: abort the in-flight SSE turn and drop its unread tail
        // ([`LlmSlot::interrupt`]), so the next committed user turn starts
        // clean. A `TurnComplete` already queued past the abort is dropped
        // by the loop's flush-time staleness check (`reply_gen`), so the
        // buffered partial reply never flushes as a stale speak.
        let llm_result = self
            .llm
            .lock()
            .await
            .interrupt()
            .await
            .map_err(|err| slot_error("llm", err));
        // TTS half: cancel the loop's live context ([`TtsSlot::cancel`],
        // the `cartesia-cancel-drain` semantics — the slot retires it
        // locally, then sends the provider cancel). Zero flushes = no
        // context has ever existed, nothing to cancel; cancelling a context
        // that already finished is the slot's documented no-op-plus-frame.
        let spoken = self.spoken_turns.load(Ordering::SeqCst);
        let tts_result = if spoken > 0 {
            self.tts
                .lock()
                .await
                .cancel(&format!("turn-{spoken}"))
                .await
                .map_err(|err| slot_error("tts", err))
        } else {
            Ok(())
        };
        llm_result?;
        tts_result
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
        self.events.take()
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            // The STT slot decides turn detection for the whole channel
            // (§3.2 turn-detection ownership rule, which "resolves the
            // Double-VAD rule for this channel"; the §3.1 table's `Client`
            // entry is the endpointing-less-slot posture that same rule
            // refines). Launch Deepgram Nova owns endpointing → Server;
            // a plain Whisper-compatible HTTP slot → Client.
            vad_mode: if self.stt.owns_endpointing() {
                VadMode::Server
            } else {
                VadMode::Client
            },
            // Derived per TTS slot — the §3 capability listing's own words
            // ("derived per TTS slot (Cartesia, ElevenLabs)"; §3.1 table
            // column "per TTS slot"; §11): launch Cartesia emits word
            // timestamps via `add_timestamps` → true. The construction-time
            // snapshot (field doc) — the flag is lifetime-stable.
            word_timestamps: self.tts_word_timestamps,
            // The composed session has no single provider session lifetime:
            // the session IS the harness's composition, and each slot's
            // transport reconnects independently beneath it (§3.2). No
            // launch slot wire announces end-of-session — Deepgram listen
            // has nothing Renewing-shaped in its vocabulary
            // (super::deepgram capabilities, accessed 2026-08-21), the
            // Cartesia WS doc carries no renewal vocabulary (module header,
            // accessed 2026-08-21), and the LLM slot is stateless per-turn
            // HTTP with no session at all — so per the types.rs contract
            // ("Channels whose provider ends sessions without any advance
            // announcement declare false") this is false.
            session_renewal: false,
            text_only: false,
            stt_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::voice::channel::fixture::{load_fixture, replay, WireServer};
    use crate::voice::channel::{
        CartesiaTtsSlot, DeepgramSttSlot, DelimitedExcerpt, OpenAiCompatLlmSlot, TimedWord,
    };
    use crate::voice::keys::store::{Backend, KeyStore};
    use crate::voice::keys::ApiKey;

    /// The slot fixtures' keys, echoed in their `absent` needles; one keyring
    /// carries both — per-slot per-base-URL identities (§3.2/§7).
    const DEEPGRAM_FIXTURE_KEY: &str = "fixture-deepgram-key-9c41";
    const CARTESIA_FIXTURE_KEY: &str = "fixture-cartesia-key-7b3d";
    const DEEPGRAM_KEY_BASE_URL: &str = "https://api.deepgram.com";
    const CARTESIA_KEY_BASE_URL: &str = "https://api.cartesia.ai";

    fn keys() -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        Arc::new(KeyManager::with_store(store))
    }

    fn keys_with_slot_keys(llm_base_url: &str) -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(
                DEEPGRAM_KEY_BASE_URL,
                &ApiKey::new(DEEPGRAM_FIXTURE_KEY.into()),
            )
            .unwrap();
        store
            .store(
                CARTESIA_KEY_BASE_URL,
                &ApiKey::new(CARTESIA_FIXTURE_KEY.into()),
            )
            .unwrap();
        store
            .store(llm_base_url, &ApiKey::new(COMPOSED_LLM_FIXTURE_KEY.into()))
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

    fn launch_shaped_config() -> CompatComposedConfig {
        CompatComposedConfig {
            stt: SttSlotConfig {
                endpoint_override: None,
                model: "nova-3".into(),
            },
            llm: LlmSlotConfig {
                base_url: "https://api.groq.com/openai/v1".into(),
                model: "llama-3.3-70b-versatile".into(),
            },
            tts: TtsSlotConfig {
                endpoint_override: None,
                model: "sonic-3".into(),
                voice: "voice-lena".into(),
            },
        }
    }

    /// Launch-shaped slots: an endpointing-owning STT (Deepgram-shaped) and
    /// a word-timestamped TTS (Cartesia-shaped).
    struct EndpointingStt;
    #[async_trait]
    impl SttSlot for EndpointingStt {
        fn owns_endpointing(&self) -> bool {
            true
        }
        async fn connect(&mut self) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn close(&mut self) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn send_audio(&mut self, _pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
            Ok(())
        }
        fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
            None
        }
    }
    struct MarkerLlm;
    #[async_trait]
    impl LlmSlot for MarkerLlm {
        fn install_context(&mut self, _ctx: SessionContext) {}
        async fn send_user_turn(&mut self, _text: &str) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn send_tool_result(
            &mut self,
            _call_id: &str,
            _result: ToolResult,
        ) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn update_context(&mut self, _update: ContextUpdate) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn interrupt(&mut self) -> Result<(), ChannelError> {
            Ok(())
        }
        fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
            None
        }
    }
    struct TimestampedTts;
    #[async_trait]
    impl TtsSlot for TimestampedTts {
        fn word_timestamps(&self) -> bool {
            true
        }
        async fn connect(&mut self) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn close(&mut self) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn speak(
            &mut self,
            _text: &str,
            _context_id: &str,
            _gen: u64,
        ) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn cancel(&mut self, _context_id: &str) -> Result<(), ChannelError> {
            Ok(())
        }
        fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
            None
        }
    }

    /// The composed connect/close + loop skeleton against the three launch
    /// slots, fixture-backed: two recorded WireServer scripts (the STT slot's
    /// full session fixture, a Cartesia connect-close handshake pin) plus an
    /// LLM listener whose only traffic is the committed turn — the stateless
    /// slot's share of connect is `install_context`, pure bookkeeping. Pins:
    /// the channel's single `Connected` arrives only after all three slots
    /// are ready (each slot's key on its own header, per-base-URL
    /// identities), with the LLM wire untouched; the mic path reaches the
    /// STT wire as the slot's pinned binary frame; the user's transcript
    /// deltas, word timing, and one `UtteranceEnd` flow through the
    /// composition loop onto the channel's own stream (the slot's
    /// slot-internal `Connected` does not); the served utterance's committed
    /// turn dials the LLM listener exactly once; close tears down in reverse
    /// order — every fixture sees its close sequence (Deepgram CloseStream;
    /// Cartesia's is the bare WS close handshake, no frame), the loop joins,
    /// the stream ends, and a second close is a no-op.
    #[tokio::test]
    async fn composed_connect_readies_all_three_slots_and_stt_flows_through_to_close() {
        let stt_handle = WireServer::start().await;
        let tts_handle = WireServer::start().await;
        // The LLM slot is per-turn HTTP: nothing may touch this listener
        // during connect/close — only the committed turn dials it.
        let llm_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind llm listener");
        llm_listener.set_nonblocking(true).expect("nonblocking");
        let llm_base_url = format!("http://{}", llm_listener.local_addr().expect("addr"));

        let keys = keys_with_slot_keys(&llm_base_url);
        let generation = Generation::new();
        let config = CompatComposedConfig {
            stt: SttSlotConfig {
                endpoint_override: Some(stt_handle.url("/v1/listen")),
                model: "nova-3".into(),
            },
            llm: LlmSlotConfig {
                base_url: llm_base_url,
                model: "llama-3.3-70b-versatile".into(),
            },
            tts: TtsSlotConfig {
                endpoint_override: Some(tts_handle.url("/tts/websocket")),
                model: "sonic-2".into(),
                voice: "voice-abc".into(),
            },
        };
        let stt = Box::new(DeepgramSttSlot::new(
            config.stt.clone(),
            Arc::clone(&keys),
            generation.clone(),
        ));
        let llm = Box::new(OpenAiCompatLlmSlot::new(
            config.llm.clone(),
            Arc::clone(&keys),
        ));
        let tts = Box::new(CartesiaTtsSlot::new(config.tts.clone(), Arc::clone(&keys)));
        let mut channel = CompatComposedChannel::new(config, stt, llm, tts, keys, generation);

        let stt_fixture = load_fixture("deepgram-stt-slot-session");
        let stt_replay_handle = stt_handle.clone();
        let stt_replayer =
            tokio::spawn(async move { replay(&stt_fixture, &stt_replay_handle).await });
        let tts_fixture = load_fixture("cartesia-connect-close");
        let tts_replay_handle = tts_handle.clone();
        let tts_replayer =
            tokio::spawn(async move { replay(&tts_fixture, &tts_replay_handle).await });

        // Conformance check 1 shape: the receiver exists pre-connect.
        let mut events = channel.take_events().expect("the receiver is handed over");
        assert!(
            channel.take_events().is_none(),
            "take_events yields the receiver exactly once"
        );

        channel.connect(ctx()).await.expect("composed connect");

        // Connect put nothing on the LLM wire: the stateless slot's share is
        // `install_context`, pure bookkeeping (the fixture is parked on the
        // mic frame here, so no utterance can have committed yet).
        match llm_listener.accept() {
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            other => panic!("the LLM wire must stay silent through connect: {other:?}"),
        }

        // Both sockets dialed, each with its own key on its own header —
        // the per-slot per-base-URL identities out of one keyring (§3.2/§7).
        let stt_record = stt_handle.wait_for_connection(1).await;
        let expected_deepgram_auth = format!("Token {DEEPGRAM_FIXTURE_KEY}");
        assert_eq!(
            stt_record.header("authorization"),
            Some(expected_deepgram_auth.as_str()),
            "the Deepgram key rides the Authorization header in the Token scheme"
        );
        let tts_record = tts_handle.wait_for_connection(1).await;
        assert_eq!(
            tts_record.header("x-api-key"),
            Some(CARTESIA_FIXTURE_KEY),
            "the Cartesia key rides the X-API-Key header"
        );

        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "one Connected on the composed stream, emitted after all three \
             slots are ready — the STT slot's slot-internal Connected is not \
             forwarded"
        );

        // The mic path: send_audio → STT slot passthrough; the fixture pins
        // the chunk's exact little-endian bytes in one raw binary frame.
        let mic_chunk: [i16; 7] = [1000, -1000, 257, -2, 0, i16::MAX, i16::MIN];
        channel.send_audio(&mic_chunk).await.expect("mic path");

        // The STT slice of the composition loop: user deltas, the final's
        // word timing, and exactly one UtteranceEnd, on the CHANNEL's stream.
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::User,
                text: "what does".into(),
            }),
            "interim user deltas pass through the composition loop"
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
            "user-speech word timing passes through under the slot's tag"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::UtteranceEnd),
            "the slot's deduped end-of-utterance reaches the channel stream \
             (the wire fired both end-of-speech signals; the slot committed one)"
        );

        // The committed utterance became one LLM turn: the slot's spawned
        // turn task dials the listener (the session's only LLM traffic).
        // Accepted before close, which aborts an in-flight turn; dropping
        // the socket unanswered is fine — the reply's fate is the
        // response-surfacing stride's business, not this pin's.
        timeout(Duration::from_secs(5), async {
            loop {
                match llm_listener.accept() {
                    Ok(_) => break,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(err) => panic!("llm listener accept: {err}"),
                }
            }
        })
        .await
        .expect("the committed turn dials the LLM listener");

        // Reverse-order teardown: every fixture sees its close sequence and
        // the composition loop joins.
        channel.close().await.expect("clean close");
        stt_replayer
            .await
            .expect("the STT fixture replays green: CloseStream, then the termination");
        tts_replayer.await.expect("the TTS fixture replays green");
        channel
            .close()
            .await
            .expect("closing a closed channel is a no-op");
        // The turn this test dropped unanswered may (or may not — close's
        // interrupt races the abort) have surfaced a slot-tagged llm Error
        // before the loop exited (w1b-i); either way the stream ends.
        loop {
            match events.recv().await {
                None => break,
                Some(ChannelEvent::Error(err)) => assert!(
                    err.message.starts_with("llm slot:"),
                    "only the dropped turn's llm-slot failure may trail: {err:?}"
                ),
                Some(other) => panic!(
                    "the composed stream ends at close: the loop exited and \
                     dropped its sender, but got {other:?}"
                ),
            }
        }
        assert!(
            stt_handle.try_next_from_adapter().await.is_none(),
            "the STT wire saw exactly the pinned frames: the binary PCM \
             chunk, CloseStream, nothing else"
        );
        assert!(
            tts_handle.try_next_from_adapter().await.is_none(),
            "the skeleton put nothing on the TTS wire: connect and close are \
             frameless on this slot (the WS close handshake is the close)"
        );
    }

    /// The key the scripted chat server's exchange must carry in its auth
    /// header and nowhere else (URL/body stay credential-free — the
    /// openai_compat tests own that pin; here it only unlocks the slot).
    const COMPOSED_LLM_FIXTURE_KEY: &str = "fixture-composed-llm-key-51aa";

    fn keys_with_llm_key(base_url: &str) -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(base_url, &ApiKey::new(COMPOSED_LLM_FIXTURE_KEY.into()))
            .unwrap();
        Arc::new(KeyManager::with_store(store))
    }

    /// A scripted STT slot: the test holds the sender and serves slot-stream
    /// events directly, pinning the composition loop at the [`SttSlot`]
    /// contract (the wire→event mapping is the deepgram slot's own pin).
    struct ScriptedStt {
        events: Option<mpsc::Receiver<ChannelEvent>>,
    }
    #[async_trait]
    impl SttSlot for ScriptedStt {
        fn owns_endpointing(&self) -> bool {
            true
        }
        async fn connect(&mut self) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn close(&mut self) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn send_audio(&mut self, _pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
            Ok(())
        }
        fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
            self.events.take()
        }
    }

    /// A minimal complete reply so the slot's spawned turn task ends cleanly.
    const SSE_STOP_TURN: &[&str] = &[
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Figure three shows a phase diagram."},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ];

    /// Scripted chat-completions server (the openai_compat test shape): one
    /// connection per script — the shared writer's `Connection: close`
    /// framing means the pooled client redials per request — each serving
    /// its lines as an SSE stream, handing every request's head+body back
    /// for the test to pin.
    fn spawn_chat_server(
        scripts: Vec<&'static [&'static str]>,
    ) -> (String, tokio::task::JoinHandle<Vec<(String, String)>>) {
        spawn_chat_server_recording(scripts, Arc::new(std::sync::Mutex::new(Vec::new())), false)
    }

    /// [`spawn_chat_server`] with the conformance rig's two extras: each
    /// exchange is pushed into `record` the moment its request is fully
    /// read (the rig's readbacks and its continuation synchronization read
    /// it live — the JoinHandle only resolves once every script served,
    /// too late for checks that interleave), and `tolerate_aborted_reader`
    /// forgives SSE writes to a client that died mid-reply — openai_compat
    /// aborts an in-flight continuation on `interrupt` and on turn
    /// supersede, so a reply nobody reads is normal rig traffic, not a
    /// protocol failure (the request itself was already recorded).
    fn spawn_chat_server_recording(
        scripts: Vec<&'static [&'static str]>,
        record: Arc<std::sync::Mutex<Vec<(String, String)>>>,
        tolerate_aborted_reader: bool,
    ) -> (String, tokio::task::JoinHandle<Vec<(String, String)>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let server = tokio::spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
            let mut exchanges = Vec::new();
            for script in scripts {
                let (mut sock, _) = listener.accept().await.expect("accept");
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                let exchange = loop {
                    let n = sock.read(&mut buf).await.expect("read request");
                    assert!(n > 0, "client closed before a full request arrived");
                    raw.extend_from_slice(&buf[..n]);
                    let Some(split) = raw.windows(4).position(|window| window == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&raw[..split]).to_string();
                    let content_length: usize = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    let body_start = split + 4;
                    while raw.len() < body_start + content_length {
                        let n = sock.read(&mut buf).await.expect("read body");
                        assert!(n > 0, "client closed mid-body");
                        raw.extend_from_slice(&buf[..n]);
                    }
                    break (
                        head,
                        String::from_utf8_lossy(&raw[body_start..body_start + content_length])
                            .to_string(),
                    );
                };
                record.lock().unwrap().push(exchange.clone());
                exchanges.push(exchange);
                let served: Result<(), std::io::Error> = async {
                    crate::test_support::mock_http::write_head(
                        &mut sock,
                        "200 OK",
                        &[("Content-Type", "text/event-stream")],
                        None,
                    )
                    .await?;
                    for line in script {
                        sock.write_all(line.as_bytes()).await?;
                        sock.write_all(b"\n\n").await?;
                        sock.flush().await?;
                    }
                    Ok(())
                }
                .await;
                if let Err(err) = served {
                    assert!(
                        tolerate_aborted_reader,
                        "failed serving the SSE reply: {err}"
                    );
                }
            }
            exchanges
        });
        (base_url, server)
    }

    /// The LLM-turn hand-off, pinned at the slot contract: finals — the
    /// deltas the finality signal (`WordTimestamps`, SttSlot contract)
    /// settles — accumulate across segments, and `UtteranceEnd` commits them
    /// as ONE `send_user_turn` whose request body carries exactly the
    /// accumulated text; interims never reach the wire. The empty-utterance
    /// pin rides the same exchange ordering: the first served utterance is
    /// an unsettled interim + `UtteranceEnd`, so the server's single
    /// accepted request being the second utterance's turn IS the proof the
    /// empty commit made no call.
    #[tokio::test]
    async fn utterance_end_commits_accumulated_finals_as_one_llm_turn() {
        let (base_url, server) = spawn_chat_server(vec![SSE_STOP_TURN]);
        let keys = keys_with_llm_key(&base_url);
        let config = CompatComposedConfig {
            stt: SttSlotConfig {
                endpoint_override: None,
                model: "nova-3".into(),
            },
            llm: LlmSlotConfig {
                base_url,
                model: "llama-3.3-70b-versatile".into(),
            },
            tts: TtsSlotConfig {
                endpoint_override: None,
                model: "sonic-3".into(),
                voice: "voice-lena".into(),
            },
        };
        let llm = Box::new(OpenAiCompatLlmSlot::new(
            config.llm.clone(),
            Arc::clone(&keys),
        ));
        let (stt_tx, stt_rx) = mpsc::channel(64);
        let mut channel = CompatComposedChannel::new(
            config,
            Box::new(ScriptedStt {
                events: Some(stt_rx),
            }),
            llm,
            Box::new(TimestampedTts),
            keys,
            Generation::new(),
        );
        let mut events = channel.take_events().expect("the receiver is handed over");
        channel.connect(ctx()).await.expect("composed connect");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        let user = |text: &str| ChannelEvent::TranscriptDelta {
            role: Role::User,
            text: text.into(),
        };
        let word_timing = |w: &str| ChannelEvent::WordTimestamps {
            words: vec![TimedWord {
                w: w.into(),
                start_ms: 0,
                end_ms: 100,
            }],
            gen: 0,
        };
        // Utterance 1 — an interim the endpointing commits with nothing
        // settled: display-only, the buffer stays empty, no LLM call.
        stt_tx.send(user("and then")).await.unwrap();
        stt_tx.send(ChannelEvent::UtteranceEnd).await.unwrap();
        // Utterance 2 — two settled segments, each an interim replaced by
        // its final and committed by the finality signal.
        stt_tx.send(user("what")).await.unwrap();
        stt_tx.send(user("what does")).await.unwrap();
        stt_tx.send(word_timing("does")).await.unwrap();
        stt_tx.send(user("figure three")).await.unwrap();
        stt_tx.send(user("figure three show")).await.unwrap();
        stt_tx.send(word_timing("show")).await.unwrap();
        stt_tx.send(ChannelEvent::UtteranceEnd).await.unwrap();

        // Everything still passes through onto the composed stream, in order.
        for expected in [
            user("and then"),
            ChannelEvent::UtteranceEnd,
            user("what"),
            user("what does"),
            word_timing("does"),
            user("figure three"),
            user("figure three show"),
            word_timing("show"),
            ChannelEvent::UtteranceEnd,
        ] {
            assert_eq!(
                timeout(Duration::from_secs(5), events.recv())
                    .await
                    .expect("the composed stream keeps flowing"),
                Some(expected),
                "the STT slice passes through unchanged around the hand-off"
            );
        }

        // The wire saw exactly one turn — the second utterance's — with the
        // accumulated finals as the user message.
        let exchanges = timeout(Duration::from_secs(5), server)
            .await
            .expect("the turn reaches the chat server")
            .expect("server task");
        let [(_head, body)] = exchanges.try_into().expect("exactly one exchange");
        assert!(
            !body.contains("and then"),
            "interims never reach the LLM wire (finals only): {body}"
        );
        let body: serde_json::Value = serde_json::from_str(&body).expect("JSON body");
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(
            messages.last().unwrap(),
            &serde_json::json!({
                "role": "user",
                "content": "what does figure three show",
            }),
            "UtteranceEnd committed the accumulated settled segments as one turn"
        );

        // w1b-i: the fired turn's reply surfaces on the composed stream —
        // consumed here so the end-of-stream pin below stays exact.
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the reply streams back"),
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "Figure three shows a phase diagram.".into(),
            })
        );

        drop(stt_tx);
        channel.close().await.expect("clean close");
        assert_eq!(
            events.recv().await,
            None,
            "the composed stream ends at close"
        );
    }

    /// The response-surfacing slice (w1b-i), pinned at the slot contract on
    /// the LLM-turn rig: the committed turn's SSE reply surfaces from the
    /// CHANNEL's stream as assistant `TranscriptDelta`s, payload-pinned to
    /// the scripted chunk's exact text. `ToolCall` stays slot-side until
    /// w1b-ii.
    #[tokio::test]
    async fn post_turn_sse_text_deltas_surface_on_the_composed_stream() {
        let (base_url, server) = spawn_chat_server(vec![SSE_STOP_TURN]);
        let keys = keys_with_llm_key(&base_url);
        let config = CompatComposedConfig {
            stt: SttSlotConfig {
                endpoint_override: None,
                model: "nova-3".into(),
            },
            llm: LlmSlotConfig {
                base_url,
                model: "llama-3.3-70b-versatile".into(),
            },
            tts: TtsSlotConfig {
                endpoint_override: None,
                model: "sonic-3".into(),
                voice: "voice-lena".into(),
            },
        };
        let llm = Box::new(OpenAiCompatLlmSlot::new(
            config.llm.clone(),
            Arc::clone(&keys),
        ));
        let (stt_tx, stt_rx) = mpsc::channel(64);
        let mut channel = CompatComposedChannel::new(
            config,
            Box::new(ScriptedStt {
                events: Some(stt_rx),
            }),
            llm,
            Box::new(TimestampedTts),
            keys,
            Generation::new(),
        );
        let mut events = channel.take_events().expect("the receiver is handed over");
        channel.connect(ctx()).await.expect("composed connect");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        // One settled segment, committed: interim → finality signal →
        // UtteranceEnd fires the turn (the w1a pin owns this path).
        let delta = ChannelEvent::TranscriptDelta {
            role: Role::User,
            text: "what does figure three show".into(),
        };
        let word_timing = ChannelEvent::WordTimestamps {
            words: vec![TimedWord {
                w: "show".into(),
                start_ms: 0,
                end_ms: 100,
            }],
            gen: 0,
        };
        stt_tx.send(delta.clone()).await.unwrap();
        stt_tx.send(word_timing.clone()).await.unwrap();
        stt_tx.send(ChannelEvent::UtteranceEnd).await.unwrap();
        for expected in [delta, word_timing, ChannelEvent::UtteranceEnd] {
            assert_eq!(
                timeout(Duration::from_secs(5), events.recv())
                    .await
                    .expect("the STT slice flows"),
                Some(expected)
            );
        }

        // The pin: the reply's SSE text delta surfaces from the CHANNEL's
        // stream, exact payload, role Assistant.
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the post-turn reply surfaces"),
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "Figure three shows a phase diagram.".into(),
            }),
            "the LLM slot's assistant deltas surface on the composed stream"
        );
        server.await.expect("server task");

        drop(stt_tx);
        channel.close().await.expect("clean close");
        assert_eq!(
            events.recv().await,
            None,
            "the composed stream ends at close"
        );
    }

    /// A tool-call turn: one call split across two fragments (id/name on the
    /// first, arguments concatenated), closed by `finish_reason:
    /// "tool_calls"` (module-header provenance shape).
    const SSE_TOOL_CALL_TURN: &[&str] = &[
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-42","type":"function","function":{"name":"get_context_around_position","arguments":"{\"window\":"}}]},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"7}"}}]},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ];

    /// The continuation the tool answer earns: one delta, `stop`, `[DONE]`.
    const SSE_TOOL_FOLLOW_UP: &[&str] = &[
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"The window shows the phase diagram caption."},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ];

    /// Builds the LLM-turn rig every w1b test shares: scripted STT slot, a
    /// real `OpenAiCompatLlmSlot` against `base_url`, launch-shaped TTS.
    fn scripted_channel(base_url: String) -> (CompatComposedChannel, mpsc::Sender<ChannelEvent>) {
        let keys = keys_with_llm_key(&base_url);
        let config = CompatComposedConfig {
            stt: SttSlotConfig {
                endpoint_override: None,
                model: "nova-3".into(),
            },
            llm: LlmSlotConfig {
                base_url,
                model: "llama-3.3-70b-versatile".into(),
            },
            tts: TtsSlotConfig {
                endpoint_override: None,
                model: "sonic-3".into(),
                voice: "voice-lena".into(),
            },
        };
        let llm = Box::new(OpenAiCompatLlmSlot::new(
            config.llm.clone(),
            Arc::clone(&keys),
        ));
        let (stt_tx, stt_rx) = mpsc::channel(64);
        let channel = CompatComposedChannel::new(
            config,
            Box::new(ScriptedStt {
                events: Some(stt_rx),
            }),
            llm,
            Box::new(TimestampedTts),
            keys,
            Generation::new(),
        );
        (channel, stt_tx)
    }

    /// Serves one settled utterance through the scripted STT slot and drains
    /// its slice (delta, word timing, `UtteranceEnd`) off the composed
    /// stream, leaving the next event the fired turn's reply.
    async fn commit_one_turn(
        stt_tx: &mpsc::Sender<ChannelEvent>,
        events: &mut mpsc::Receiver<ChannelEvent>,
    ) {
        let delta = ChannelEvent::TranscriptDelta {
            role: Role::User,
            text: "what does figure three show".into(),
        };
        let word_timing = ChannelEvent::WordTimestamps {
            words: vec![TimedWord {
                w: "show".into(),
                start_ms: 0,
                end_ms: 100,
            }],
            gen: 0,
        };
        stt_tx.send(delta.clone()).await.unwrap();
        stt_tx.send(word_timing.clone()).await.unwrap();
        stt_tx.send(ChannelEvent::UtteranceEnd).await.unwrap();
        for expected in [delta, word_timing, ChannelEvent::UtteranceEnd] {
            assert_eq!(
                timeout(Duration::from_secs(5), events.recv())
                    .await
                    .expect("the STT slice flows"),
                Some(expected)
            );
        }
    }

    /// ToolCall forwarding (w1b-ii, §5), pinned at the slot contract on the
    /// LLM-turn rig: the committed turn's scripted tool call surfaces from
    /// the CHANNEL's stream verbatim — exact `call_id`, exact name, and the
    /// fragment-concatenated arguments parsed to uniform args.
    #[tokio::test]
    async fn a_scripted_tool_call_turn_surfaces_toolcall_on_the_composed_stream() {
        let (base_url, server) = spawn_chat_server(vec![SSE_TOOL_CALL_TURN]);
        let (mut channel, stt_tx) = scripted_channel(base_url);
        let mut events = channel.take_events().expect("the receiver is handed over");
        channel.connect(ctx()).await.expect("composed connect");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        commit_one_turn(&stt_tx, &mut events).await;

        // The pin: the assembled tool call crosses the composed stream
        // unchanged — name and args exactly as scripted.
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the tool call surfaces"),
            Some(ChannelEvent::ToolCall {
                call_id: "call-42".into(),
                name: "get_context_around_position".into(),
                args: serde_json::json!({ "window": 7 }),
            }),
            "the LLM slot's assembled ToolCall forwards verbatim"
        );
        server.await.expect("server task");

        drop(stt_tx);
        channel.close().await.expect("clean close");
        assert_eq!(
            events.recv().await,
            None,
            "the composed stream ends at close"
        );
    }

    /// The §5 continuation (w1b-ii): `send_tool_result` delegates to the LLM
    /// slot, and the follow-up turn's scripted SSE text surfaces as assistant
    /// deltas through the SAME composed stream the reply path already rides —
    /// payload-pinned to the scripted chunk's exact text, with the wire's
    /// second exchange carrying the `role: "tool"` answer (delegation went
    /// through the slot's history, not some parallel path).
    #[tokio::test]
    async fn send_tool_result_surfaces_the_follow_up_deltas_on_the_composed_stream() {
        let (base_url, server) = spawn_chat_server(vec![SSE_TOOL_CALL_TURN, SSE_TOOL_FOLLOW_UP]);
        let (mut channel, stt_tx) = scripted_channel(base_url);
        let mut events = channel.take_events().expect("the receiver is handed over");
        channel.connect(ctx()).await.expect("composed connect");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        commit_one_turn(&stt_tx, &mut events).await;
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the tool call surfaces"),
            Some(ChannelEvent::ToolCall {
                call_id: "call-42".into(),
                name: "get_context_around_position".into(),
                args: serde_json::json!({ "window": 7 }),
            })
        );

        channel
            .send_tool_result(
                "call-42",
                ToolResult::Ok(serde_json::json!({ "excerpt": "the caption text" })),
            )
            .await
            .expect("the answer continues the turn through the slot");

        // The pin: the continuation's text rides the already-wired return
        // path — assistant deltas on the CHANNEL's stream, exact payload.
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the follow-up surfaces"),
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "The window shows the phase diagram caption.".into(),
            }),
            "send_tool_result's follow-up deltas surface on the composed stream"
        );

        let exchanges = server.await.expect("server task");
        assert_eq!(exchanges.len(), 2, "the tool answer re-dialed the wire");
        let body: serde_json::Value =
            serde_json::from_str(&exchanges[1].1).expect("continuation JSON body");
        assert_eq!(
            body["messages"]
                .as_array()
                .expect("messages")
                .last()
                .unwrap(),
            &serde_json::json!({
                "role": "tool",
                "tool_call_id": "call-42",
                "content": "{\"excerpt\":\"the caption text\"}",
            }),
            "the delegation ran through the slot: the continuation carries \
             the role:tool answer under its tool_call_id"
        );

        drop(stt_tx);
        channel.close().await.expect("clean close");
        assert_eq!(
            events.recv().await,
            None,
            "the composed stream ends at close"
        );
    }

    /// The no-live-turn failure path (w1b-ii): `send_tool_result` with no
    /// pending tool call — nothing in flight, nothing to continue — returns
    /// the slot's pre-flight rejection slot-tagged, never panics (the marker
    /// this stride replaced was a `todo!()`), and puts nothing on the
    /// composed stream.
    #[tokio::test]
    async fn send_tool_result_without_a_live_llm_turn_is_a_slot_tagged_error() {
        // No chat server at all: the pending-call gate rejects before any
        // request could be built, so the wire must never be needed.
        let (mut channel, stt_tx) = scripted_channel("http://127.0.0.1:9".into());
        let mut events = channel.take_events().expect("the receiver is handed over");
        channel.connect(ctx()).await.expect("composed connect");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        let rejected = channel
            .send_tool_result("call-42", ToolResult::Ok(serde_json::json!({})))
            .await;
        let err = rejected.expect_err("no live LLM turn: the answer is unanswerable");
        assert_eq!(err.kind, ErrorKind::InvalidState);
        assert!(!err.retryable, "nothing to retry against: {err:?}");
        assert!(
            err.message.starts_with("llm slot:"),
            "the failing slot is visible in the error: {err:?}"
        );

        drop(stt_tx);
        channel.close().await.expect("clean close");
        assert_eq!(
            events.recv().await,
            None,
            "the rejection rode the Result; the composed stream stays clean \
             and ends at close"
        );
    }

    /// The scripted reply split across two deltas, so the TTS pin below can
    /// prove whole-turn buffering: only the concatenation matches the
    /// fixture's pinned `transcript`.
    const SSE_TWO_DELTA_STOP_TURN: &[&str] = &[
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Figure three "},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"shows a phase diagram."},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ];

    /// The TTS leg (w2), payload-pinned end to end on the scripted rig:
    /// scripted STT commits the turn, the scripted SSE reply streams back,
    /// and the reply boundary flushes ONE speak into a real
    /// [`CartesiaTtsSlot`] against the recorded `cartesia-composed-reply`
    /// fixture — whose `expect_text` pins the generation request's
    /// transcript to the CONCATENATED reply under the loop-chosen
    /// `turn-1` context (a per-delta speak would red the fixture), with
    /// `continue: false` and no credential. The served chunks surface from
    /// the CHANNEL's stream as byte-exact `AudioDelta`s and ms-exact
    /// `WordTimestamps`, stamped with the speak-time generation snapshot
    /// (bumped pre-turn to prove it is read, not defaulted), and the
    /// ordering is pinned to this implementation's contract: both assistant
    /// text deltas precede every audio frame (the flush fires only at the
    /// boundary, after the last delta). Rig vacuity is guarded by the
    /// budgeted recv pins themselves: zero frames observed = timeout = red.
    #[tokio::test]
    async fn the_reply_boundary_speaks_the_turn_and_audio_surfaces_on_the_composed_stream() {
        let (base_url, server) = spawn_chat_server(vec![SSE_TWO_DELTA_STOP_TURN]);
        let tts_handle = WireServer::start().await;
        let keys = keys_with_slot_keys(&base_url);
        let config = CompatComposedConfig {
            stt: SttSlotConfig {
                endpoint_override: None,
                model: "nova-3".into(),
            },
            llm: LlmSlotConfig {
                base_url,
                model: "llama-3.3-70b-versatile".into(),
            },
            tts: TtsSlotConfig {
                endpoint_override: Some(tts_handle.url("/tts/websocket")),
                model: "sonic-2".into(),
                voice: "voice-abc".into(),
            },
        };
        let llm = Box::new(OpenAiCompatLlmSlot::new(
            config.llm.clone(),
            Arc::clone(&keys),
        ));
        let tts = Box::new(CartesiaTtsSlot::new(config.tts.clone(), Arc::clone(&keys)));
        // A non-zero live generation: the frames' `gen` below proves the
        // loop snapshotted the counter at speak time (loop-owned tagging).
        let generation = Generation::new();
        generation.bump();
        generation.bump();
        generation.bump();
        let (stt_tx, stt_rx) = mpsc::channel(64);
        let mut channel = CompatComposedChannel::new(
            config,
            Box::new(ScriptedStt {
                events: Some(stt_rx),
            }),
            llm,
            tts,
            keys,
            generation.clone(),
        );

        let fixture = load_fixture("cartesia-composed-reply");
        let replay_handle = tts_handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        let mut events = channel.take_events().expect("the receiver is handed over");
        channel.connect(ctx()).await.expect("composed connect");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        commit_one_turn(&stt_tx, &mut events).await;

        // The ordering pin, first half: the reply's text surfaces delta by
        // delta, before any audio exists (the speak has not fired yet).
        for expected_text in ["Figure three ", "shows a phase diagram."] {
            assert_eq!(
                timeout(Duration::from_secs(5), events.recv())
                    .await
                    .expect("the reply streams back"),
                Some(ChannelEvent::TranscriptDelta {
                    role: Role::Assistant,
                    text: expected_text.into(),
                }),
                "assistant deltas precede the audio on the composed stream"
            );
        }

        // The ordering pin, second half — the boundary fired the flush and
        // the synthesized audio surfaces: byte-exact frames and ms-exact
        // word timing, all under the speak-time generation snapshot. These
        // budgeted recvs are the vacuity guard: no frames, no green.
        const SPEAK_TIME_GEN: u64 = 3;
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the first audio frame surfaces"),
            Some(ChannelEvent::AudioDelta {
                pcm: vec![1000, -1000],
                gen: SPEAK_TIME_GEN,
            }),
            "the TTS slot's decoded chunks surface from the composed stream"
        );
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the second audio frame surfaces"),
            Some(ChannelEvent::AudioDelta {
                pcm: vec![257, -2],
                gen: SPEAK_TIME_GEN,
            })
        );
        let timed = |w: &str, start_ms: u32, end_ms: u32| TimedWord {
            w: w.into(),
            start_ms,
            end_ms,
        };
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the reply's word timing surfaces"),
            Some(ChannelEvent::WordTimestamps {
                words: vec![timed("figure", 520, 900), timed("three", 900, 1280)],
                gen: SPEAK_TIME_GEN,
            }),
            "the TTS slot's word timing rides the same tag"
        );

        // The fixture replayed green: the generation request carried the
        // concatenated transcript under turn-1 (the whole-turn pin).
        replayer.await.expect("the recorded fixture replays green");
        server.await.expect("server task");

        drop(stt_tx);
        channel.close().await.expect("clean close");
        assert_eq!(
            events.recv().await,
            None,
            "the composed stream ends at close: turn-1's done retired the \
             context silently, nothing trails"
        );
    }

    /// The barge-in turn's follow-up reply, distinct from the first turn's
    /// text so the fixture's `turn-2` transcript needle can prove the
    /// post-interrupt speak carried the NEW reply — a stale flush of the
    /// first turn's text would miss the needle and red the replayer.
    const SSE_SECOND_TURN: &[&str] = &[
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"It plots pressure against temperature."},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ];

    /// Barge-in on the composed channel (w3), on the w2 scripted rig against
    /// the recorded `cartesia-composed-barge-in` fixture. Mid-audio — one
    /// `turn-1` frame observed, the rig's vacuity guard — [`barge_in`] bumps
    /// and interrupts, and the pins are:
    ///
    /// 1. the fixture's next expected client frame is the documented cancel
    ///    for `turn-1` (`"cancel":true`) — skip the TTS cancel and the
    ///    replayer reds on the turn-2 request arriving where the cancel
    ///    frame belongs;
    /// 2. every late `turn-1` frame — the one already through the slot
    ///    pre-cancel AND the fixture's post-cancel mid-air chunk — surfaces
    ///    AFTER the interrupt, byte-exact, still tagged with the speak-time
    ///    (old, non-default) generation: honest tagging, the playback
    ///    gate's food (the mid-air invariant, [`TtsSlot::cancel`]
    ///    contract);
    /// 3. no speak fires between the cancel and the next committed turn:
    ///    the fixture's step order pins the cancel's successor to be
    ///    turn-2's generation request carrying exactly the SECOND reply's
    ///    transcript (zero stale generation requests);
    /// 4. the next committed turn speaks under the bumped generation — its
    ///    audio surfaces tagged with [`barge_in`]'s returned live gen.
    #[tokio::test]
    async fn barge_in_cancels_the_live_tts_context_and_the_next_turn_speaks_under_the_bumped_gen() {
        let (base_url, server) = spawn_chat_server(vec![SSE_TWO_DELTA_STOP_TURN, SSE_SECOND_TURN]);
        let tts_handle = WireServer::start().await;
        let keys = keys_with_slot_keys(&base_url);
        let config = CompatComposedConfig {
            stt: SttSlotConfig {
                endpoint_override: None,
                model: "nova-3".into(),
            },
            llm: LlmSlotConfig {
                base_url,
                model: "llama-3.3-70b-versatile".into(),
            },
            tts: TtsSlotConfig {
                endpoint_override: Some(tts_handle.url("/tts/websocket")),
                model: "sonic-2".into(),
                voice: "voice-abc".into(),
            },
        };
        let llm = Box::new(OpenAiCompatLlmSlot::new(
            config.llm.clone(),
            Arc::clone(&keys),
        ));
        let tts = Box::new(CartesiaTtsSlot::new(config.tts.clone(), Arc::clone(&keys)));
        // A non-zero pre-interrupt generation (the w2 rationale): the old
        // tag pinned below is 2, not the u64 default — a frame that lost
        // its tag to a zeroing bug cannot pass as "old".
        let generation = Generation::new();
        generation.bump();
        generation.bump();
        let (stt_tx, stt_rx) = mpsc::channel(64);
        let mut channel = CompatComposedChannel::new(
            config,
            Box::new(ScriptedStt {
                events: Some(stt_rx),
            }),
            llm,
            tts,
            keys,
            generation.clone(),
        );

        let fixture = load_fixture("cartesia-composed-barge-in");
        let replay_handle = tts_handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        let mut events = channel.take_events().expect("the receiver is handed over");
        channel.connect(ctx()).await.expect("composed connect");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        commit_one_turn(&stt_tx, &mut events).await;
        for expected_text in ["Figure three ", "shows a phase diagram."] {
            assert_eq!(
                timeout(Duration::from_secs(5), events.recv())
                    .await
                    .expect("the first reply streams back"),
                Some(ChannelEvent::TranscriptDelta {
                    role: Role::Assistant,
                    text: expected_text.into(),
                })
            );
        }

        // Mid-audio: the first turn-1 frame observed IS the vacuity guard —
        // the speak fired and the synthesized audio is streaming.
        const OLD_GEN: u64 = 2;
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("a pre-interrupt frame surfaces"),
            Some(ChannelEvent::AudioDelta {
                pcm: vec![1000, -1000],
                gen: OLD_GEN,
            }),
            "barge-in below is mid-audio, not vacuous"
        );

        // Park the test off the runtime so the slot's read loop drains the
        // fixture's already-flushed burst before the barge — keeping the
        // "late frame" below a frame that was genuinely through the slot
        // pre-cancel, distinct from the post-cancel mid-air chunk pinned
        // after it. One-sided: too little time reds the budgeted recv
        // below, it can never false-green.
        tokio::time::sleep(Duration::from_millis(250)).await;

        // The spec-ordered barge-in (mod.rs helper, the conformance driver):
        // bump first, provider cancel second.
        let live = crate::voice::channel::barge_in(&generation, &mut channel)
            .await
            .expect("barge-in on a connected composed channel");
        assert_eq!(live, OLD_GEN + 1, "barge_in returns the bumped live gen");

        // Pin 2: the late frame — through the slot's gate before the cancel
        // retired turn-1 — surfaces post-interrupt under the OLD tag. Its
        // payload is the fixture's second turn-1 chunk, byte-exact.
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the late frame surfaces"),
            Some(ChannelEvent::AudioDelta {
                pcm: vec![257, -2],
                gen: OLD_GEN,
            }),
            "a late frame of the superseded context keeps its speak-time \
             tag — the playback gate refuses it, the channel never re-tags"
        );

        // Pin 2b, the mid-air invariant: the fixture's post-cancel turn-1
        // chunk — sent by the server AFTER it received the cancel — still
        // surfaces, old-tagged. The cancel stops future frames; it never
        // censors mid-air ones ([`TtsSlot::cancel`] contract, §2 rule 2).
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the mid-air post-cancel frame surfaces"),
            Some(ChannelEvent::AudioDelta {
                pcm: vec![42],
                gen: OLD_GEN,
            }),
            "stale audio emitted after the bump surfaces tagged old-gen — \
             the playback gate, not the channel, refuses it"
        );

        // The next committed user turn: its reply must speak under the
        // bumped generation, and — the fixture's step order (pin 3) — its
        // generation request must be the cancel's IMMEDIATE successor on
        // the TTS wire.
        commit_one_turn(&stt_tx, &mut events).await;
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the second reply streams back"),
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "It plots pressure against temperature.".into(),
            })
        );
        assert_eq!(
            timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the next turn's audio surfaces"),
            Some(ChannelEvent::AudioDelta {
                pcm: vec![32767, -32768],
                gen: OLD_GEN + 1,
            }),
            "the post-barge-in turn plays under the bumped generation, with \
             the mid-air turn-1 chunk already surfaced old-tagged above"
        );

        // Pins 1 and 3 replayed green: the cancel frame arrived where the
        // fixture demands it, followed by exactly one generation request —
        // turn-2's, carrying the second reply's transcript.
        replayer.await.expect("the recorded fixture replays green");
        server.await.expect("server task");

        drop(stt_tx);
        channel.close().await.expect("clean close");
        assert_eq!(
            events.recv().await,
            None,
            "the composed stream ends at close"
        );
    }

    #[test]
    fn capabilities_derive_from_the_active_slots() {
        let channel = CompatComposedChannel::new(
            launch_shaped_config(),
            Box::new(EndpointingStt),
            Box::new(MarkerLlm),
            Box::new(TimestampedTts),
            keys(),
            Generation::new(),
        );
        assert_eq!(
            channel.capabilities(),
            ChannelCapabilities {
                // §3.2 turn-detection rule: the endpointing-owning STT slot
                // makes the channel Server-VAD.
                vad_mode: VadMode::Server,
                // §3.1 table / §11: word timing is the TTS slot's
                // contribution (Cartesia-shaped → true).
                word_timestamps: true,
                // No slot wire announces renewal; the composed lifetime is
                // harness-managed (types.rs contract).
                session_renewal: false,
                text_only: false,
                stt_only: false,
            }
        );
    }

    /// The composed channel's conformance rig (cc-1f-i): the suite's ONE
    /// scripted-provider handle ([`ProviderSim`]) routed onto the THREE slot
    /// fixtures this module already drives —
    ///
    /// - user-speech emission (`emit_transcript(User, ..)`) → the scripted
    ///   STT injector's sender ([`ScriptedStt`]'s shape, here [`RigStt`],
    ///   which also records accepted mic audio for the epoch readback);
    /// - `emit_audio(gen, pcm)` → a server-side `chunk` frame on the
    ///   Cartesia [`WireServer`], pushed under the context mapped to `gen`
    ///   (the loop names contexts `turn-N`; the sim binds each new `gen` to
    ///   the next unclaimed generation request observed on the TTS wire);
    /// - `received_interrupts()` → the cancel frames the Cartesia wire
    ///   actually received, each mapped to the generation its interrupt
    ///   call observed on entry ([`BargeObservedChannel`], the gemini.rs
    ///   m3-fix3 / deepgram_agent.rs shape — here the entry snapshot IS the
    ///   post-bump value, because composed's `interrupt` never bumps and
    ///   [`barge_in`]'s bump precedes the call);
    /// - scripted replies → the SSE chat-server scripts fixed at rig time
    ///   ([`spawn_chat_server`]) — the LLM slot is per-turn HTTP, so the
    ///   reply script IS the provider's scripted side.
    ///
    /// Assistant-transcript and tool-call emission share one lever (the
    /// LLM leg is per-turn HTTP, no server push): "the provider emits" IS
    /// the next scripted turn firing through the channel's own shared slot
    /// handle, asserted against the script fixed at rig time. When the
    /// suite needs provider audio with no reply in flight (check 3's
    /// push-wire shape), the rig opens the synthesis context exactly as
    /// the loop's flush would — same `turn-{n}` naming, same counter,
    /// same slot. The text/context/tool-result readbacks parse the chat
    /// server's live request record (history is cumulative, so the last
    /// body carries everything). Only the renewal trio (composed declares
    /// `session_renewal: false`) panics — the suite must never drive it.
    mod conformance_rig {
        use std::sync::Mutex;

        use base64::Engine as _;

        use super::*;
        use crate::voice::channel::conformance::{run_all, ConformanceFixture, ProviderSim};
        use crate::voice::channel::fixture::{WireFrame, WireHandle};

        /// The scripted STT injector, plus the mic-audio record the epoch
        /// readback needs: the composed channel routes `send_audio` straight
        /// to this slot, so what lands here IS what the provider accepted.
        struct RigStt {
            events: Option<mpsc::Receiver<ChannelEvent>>,
            received: Arc<Mutex<Vec<Vec<i16>>>>,
        }
        #[async_trait]
        impl SttSlot for RigStt {
            fn owns_endpointing(&self) -> bool {
                true
            }
            async fn connect(&mut self) -> Result<(), ChannelError> {
                Ok(())
            }
            async fn close(&mut self) -> Result<(), ChannelError> {
                Ok(())
            }
            async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
                self.received.lock().unwrap().push(pcm_16k_mono.to_vec());
                Ok(())
            }
            fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
                self.events.take()
            }
        }

        /// The channel under test with the rig's cancel observation wrapped
        /// around `interrupt` (the deepgram_agent.rs shape). Composed's
        /// `interrupt` performs no bump — [`barge_in`] bumps before calling
        /// it — so the counter on entry is exactly the value the wire cancel
        /// goes out under; [`ComposedSim::received_interrupts`] pairs these
        /// entry snapshots, in call order, with the cancel frames the
        /// Cartesia wire actually received.
        struct BargeObservedChannel {
            inner: CompatComposedChannel,
            generation: Generation,
            observed_interrupts: Arc<Mutex<Vec<u64>>>,
            /// The chat server's live exchange record, shared with the sim
            /// and the server task — the wrapper's `send_tool_result`
            /// synchronizes on it (see the impl).
            llm_record: Arc<Mutex<Vec<(String, String)>>>,
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
                // Rig synchronization, not semantics: the slot fires the
                // ack's continuation from a spawned task, and the suite's
                // next step may abort it (`barge_in`) or supersede it (the
                // next scripted turn) before its request ever left —
                // desyncing the serial script server. Hold the suite here
                // until the continuation request lands; every core check
                // answers single-call turns, so an accepted result always
                // fires exactly one.
                let before = self.llm_record.lock().unwrap().len();
                let outcome = self.inner.send_tool_result(call_id, result).await;
                if outcome.is_ok() {
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                    while self.llm_record.lock().unwrap().len() <= before {
                        assert!(
                            tokio::time::Instant::now() < deadline,
                            "conformance rig: the ack's continuation request never \
                             reached the chat server"
                        );
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                }
                outcome
            }
            async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError> {
                self.inner.update_context(update).await
            }
            async fn interrupt(&mut self) -> Result<(), ChannelError> {
                let observed = self.generation.current();
                self.inner.interrupt().await?;
                self.observed_interrupts.lock().unwrap().push(observed);
                Ok(())
            }
            fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
                self.inner.take_events()
            }
            fn capabilities(&self) -> ChannelCapabilities {
                self.inner.capabilities()
            }
        }

        /// The user-turn text the sim's hidden scripted-turn fires carry —
        /// rig vocabulary, so the text readback can tell the sim's own
        /// trigger turns from inputs the CHECK sent through the channel.
        const RIG_SCRIPTED_TURN: &str = "(conformance rig: fire the next scripted reply)";

        /// The reply text a script streams: the concatenated `delta.content`
        /// fragments. The sim asserts a requested assistant emission against
        /// this — the script fixed at rig time IS the provider's scripted
        /// side; the sim never synthesizes reply text.
        fn script_reply_text(script: &[&str]) -> String {
            script
                .iter()
                .filter_map(|line| {
                    let payload = line.strip_prefix("data: ")?;
                    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
                    Some(
                        value
                            .get("choices")?
                            .get(0)?
                            .get("delta")?
                            .get("content")?
                            .as_str()?
                            .to_string(),
                    )
                })
                .collect()
        }

        /// The tool call a script carries: `id`/`name` from the first
        /// fragment, `arguments` concatenated across fragments and parsed —
        /// the sim asserts a requested tool-call emission against this, the
        /// same never-synthesize rule as [`script_reply_text`].
        fn script_tool_call(script: &[&str]) -> (String, String, serde_json::Value) {
            let mut id = String::new();
            let mut name = String::new();
            let mut arguments = String::new();
            for line in script {
                let Some(payload) = line.strip_prefix("data: ") else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
                    continue;
                };
                let Some(calls) = value
                    .pointer("/choices/0/delta/tool_calls")
                    .and_then(|calls| calls.as_array())
                else {
                    continue;
                };
                for call in calls {
                    if let Some(fragment_id) = call.get("id").and_then(|id| id.as_str()) {
                        id = fragment_id.to_string();
                    }
                    if let Some(fragment_name) = call
                        .pointer("/function/name")
                        .and_then(|name| name.as_str())
                    {
                        name = fragment_name.to_string();
                    }
                    if let Some(fragment) = call
                        .pointer("/function/arguments")
                        .and_then(|args| args.as_str())
                    {
                        arguments.push_str(fragment);
                    }
                }
            }
            (
                id,
                name,
                serde_json::from_str(&arguments)
                    .expect("conformance rig: the scripted tool arguments parse"),
            )
        }

        struct ComposedSim {
            stt_tx: mpsc::Sender<ChannelEvent>,
            tts: WireHandle,
            stt_received: Arc<Mutex<Vec<Vec<i16>>>>,
            observed_interrupts: Arc<Mutex<Vec<u64>>>,
            /// gen → the Cartesia context it plays under, in binding order.
            contexts: Vec<(u64, String)>,
            /// The channel's own shared LLM-slot handle
            /// ([`CompatComposedChannel::llm_slot_handle`]): the sim's
            /// assistant-emission lever — per-turn HTTP has no server push,
            /// so "the provider emits" IS the next scripted turn firing.
            llm: Arc<tokio::sync::Mutex<Box<dyn LlmSlot>>>,
            /// The channel's own shared TTS-slot handle and spoken-turn
            /// counter ([`CompatComposedChannel::tts_slot_handle`]): the
            /// sim's context-opening lever for push-wire-shaped checks —
            /// see `emit_audio`.
            tts_slot: Arc<tokio::sync::Mutex<Box<dyn TtsSlot>>>,
            spoken_turns: Arc<AtomicU64>,
            /// This rig's SSE scripts in serve order. The cursor of the
            /// NEXT script to serve is the live exchange record's length —
            /// continuations the suite fires through the channel
            /// (`send_tool_result`, `send_text`) consume script slots too,
            /// and the record counts every request the server accepted.
            scripts: Vec<&'static [&'static str]>,
            /// The chat server's live exchange record (the text/tool-result
            /// readbacks' source), shared with the server task and the
            /// wrapper — see [`ComposedSim::llm_exchanges`].
            llm_record: Arc<Mutex<Vec<(String, String)>>>,
        }

        impl ComposedSim {
            /// TTS frames, snapshot only once two consecutive reads agree
            /// (the deepgram_agent.rs settling rule): a cancel the suite
            /// awaited is flushed by the slot but not necessarily read by
            /// the fixture server yet.
            async fn settled_tts_frames(&self) -> Vec<WireFrame> {
                let mut last = self.tts.received_by_connection();
                loop {
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    let now = self.tts.received_by_connection();
                    if now == last {
                        return now.into_iter().flatten().collect();
                    }
                    last = now;
                }
            }

            /// The chat exchanges recorded so far, snapshot once two
            /// consecutive reads agree (the settling rule — a request the
            /// suite's last send fired may still be mid-read server-side).
            async fn llm_exchanges(&mut self) -> Vec<(String, String)> {
                let mut last = self.llm_record.lock().unwrap().clone();
                loop {
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    let now = self.llm_record.lock().unwrap().clone();
                    if now == last {
                        return now;
                    }
                    last = now;
                }
            }

            /// The generation requests observed on the Cartesia wire so
            /// far: each text frame carrying a `transcript` is one speak,
            /// its `context_id` the loop-chosen turn context, wire order.
            fn contexts_on_wire(&self) -> Vec<String> {
                self.tts
                    .received_by_connection()
                    .iter()
                    .flatten()
                    .filter_map(|frame| {
                        let WireFrame::Text(text) = frame else {
                            return None;
                        };
                        let value: serde_json::Value = serde_json::from_str(text).ok()?;
                        value.get("transcript")?;
                        Some(value.get("context_id")?.as_str()?.to_string())
                    })
                    .collect()
            }
        }

        #[async_trait]
        impl ProviderSim for ComposedSim {
            /// Provider audio for `gen` is a `chunk` frame under the mapped
            /// context: a `gen` already bound reuses its context; a new
            /// `gen` binds to the next UNCLAIMED generation request on the
            /// wire (polled — the loop's speak flush races the suite's
            /// call). If no unclaimed request appears within the grace
            /// window, no reply is in flight — the suite is treating the
            /// provider as a push wire (check 3's shape) — and the rig
            /// opens the context itself, exactly as the loop's flush
            /// would: same `turn-{n}` naming, same counter, through the
            /// channel's own slot ([`CompatComposedChannel::
            /// tts_slot_handle`]). The mic path cannot do it: every commit
            /// and reply surfaces on the app stream, which check 3's
            /// frozen recv discipline (nothing but `AudioDelta`s) does not
            /// admit. The chunk's `data` is the pcm as base64'd
            /// little-endian s16, the wire's documented encoding.
            async fn emit_audio(&mut self, gen: u64, pcm: Vec<i16>) {
                let context = match self.contexts.iter().find(|(bound, _)| *bound == gen) {
                    Some((_, context)) => context.clone(),
                    None => {
                        let grace = tokio::time::Instant::now() + Duration::from_millis(400);
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                        let mut opened = false;
                        let context = loop {
                            let unclaimed = self.contexts_on_wire().into_iter().find(|on_wire| {
                                !self.contexts.iter().any(|(_, claimed)| claimed == on_wire)
                            });
                            if let Some(context) = unclaimed {
                                break context;
                            }
                            if !opened && tokio::time::Instant::now() >= grace {
                                opened = true;
                                let turn = self.spoken_turns.fetch_add(1, Ordering::SeqCst) + 1;
                                self.tts_slot
                                    .lock()
                                    .await
                                    .speak(
                                        "(conformance rig context)",
                                        &format!("turn-{turn}"),
                                        gen,
                                    )
                                    .await
                                    .expect("conformance rig: the context-opening speak fires");
                            }
                            assert!(
                                tokio::time::Instant::now() < deadline,
                                "conformance rig: no unclaimed TTS context appeared for gen {gen}"
                            );
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        };
                        self.contexts.push((gen, context.clone()));
                        context
                    }
                };
                let mut bytes = Vec::with_capacity(pcm.len() * 2);
                for sample in &pcm {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
                self.tts.send(WireFrame::Text(
                    serde_json::json!({
                        "type": "chunk",
                        "context_id": context,
                        "status_code": 206,
                        "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    })
                    .to_string(),
                ));
            }

            async fn emit_transcript(&mut self, role: Role, text: String) {
                match role {
                    Role::User => {
                        self.stt_tx
                            .send(ChannelEvent::TranscriptDelta { role, text })
                            .await
                            .expect("conformance rig: the STT injector outlives the sim");
                        self.stt_tx
                            .send(ChannelEvent::UtteranceEnd)
                            .await
                            .expect("conformance rig: the STT injector outlives the sim");
                    }
                    // Assistant transcripts are SSE reply deltas, and the
                    // LLM leg is per-turn HTTP with no server push: "the
                    // provider emits" IS the next scripted turn firing —
                    // through the channel's own shared slot handle, so the
                    // reply rides the composition loop's LLM branch like
                    // any other turn. The requested text must BE the next
                    // script fixed at rig time (asserted, never
                    // synthesized), one script per emission.
                    Role::Assistant => {
                        let cursor = self.llm_record.lock().unwrap().len();
                        let script = *self.scripts.get(cursor).unwrap_or_else(|| {
                            panic!(
                                "conformance rig: assistant emission at script cursor \
                                 {cursor} has no scripted turn"
                            )
                        });
                        assert_eq!(
                            script_reply_text(script),
                            text,
                            "conformance rig: a requested assistant emission must match \
                             the reply script fixed at rig time"
                        );
                        let before = self.contexts_on_wire().len();
                        self.llm
                            .lock()
                            .await
                            .send_user_turn(RIG_SCRIPTED_TURN)
                            .await
                            .expect("conformance rig: the scripted turn fires");
                        // Return only once the turn's speak flush reaches
                        // the TTS wire: the boundary flush follows every
                        // forwarded delta, so a later emission cannot
                        // overtake this one on the app stream.
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                        while self.contexts_on_wire().len() <= before {
                            assert!(
                                tokio::time::Instant::now() < deadline,
                                "conformance rig: the scripted turn's speak never \
                                 reached the TTS wire"
                            );
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                    }
                }
            }

            async fn emit_tool_call(
                &mut self,
                call_id: String,
                name: String,
                args: serde_json::Value,
            ) {
                // Tool calls originate in the SSE reply script (the
                // assistant-emission rule, same lever): firing the next
                // scripted turn IS the provider emitting one. A tool-call
                // turn carries no prose, so nothing speaks and nothing but
                // the ToolCall surfaces; the check's own recv synchronizes
                // delivery, so no wire wait exists here.
                let cursor = self.llm_record.lock().unwrap().len();
                let script = *self.scripts.get(cursor).unwrap_or_else(|| {
                    panic!(
                        "conformance rig: tool-call emission at script cursor {cursor} \
                         has no scripted turn"
                    )
                });
                assert_eq!(
                    script_tool_call(script),
                    (call_id, name, args),
                    "conformance rig: a requested tool-call emission must match \
                     the script fixed at rig time"
                );
                self.llm
                    .lock()
                    .await
                    .send_user_turn(RIG_SCRIPTED_TURN)
                    .await
                    .expect("conformance rig: the scripted tool-call turn fires");
            }

            // The renewal trio: composed declares `session_renewal: false`,
            // so `run_all`'s capability gate must never drive these against
            // this rig — a call here means the gate broke, and the panic is
            // the alarm.
            async fn announce_session_end(&mut self) {
                panic!("conformance rig: composed declares session_renewal: false")
            }
            async fn complete_renewal(&mut self) {
                panic!("conformance rig: composed declares session_renewal: false")
            }
            async fn expire(&mut self) {
                panic!("conformance rig: composed declares session_renewal: false")
            }

            /// No renewal → exactly one epoch: everything the RigStt slot
            /// accepted, in order.
            async fn received_audio_by_epoch(&mut self) -> Vec<Vec<Vec<i16>>> {
                vec![self.stt_received.lock().unwrap().clone()]
            }

            /// Text inputs the provider accepted: the `role:"user"`
            /// messages of the LAST request body (history is cumulative),
            /// minus the rig's own scripted-turn sentinels. On this
            /// channel a mic-committed utterance is also a user message —
            /// indistinguishable on the wire — but the readback's one
            /// consumer (check 8) drives no mic turns.
            async fn received_texts(&mut self) -> Vec<String> {
                let exchanges = self.llm_exchanges().await;
                let Some((_, body)) = exchanges.last() else {
                    return Vec::new();
                };
                let value: serde_json::Value =
                    serde_json::from_str(body).expect("conformance rig: request body is JSON");
                let Some(messages) = value.get("messages").and_then(|m| m.as_array()) else {
                    return Vec::new();
                };
                messages
                    .iter()
                    .filter(|msg| msg.get("role").and_then(|r| r.as_str()) == Some("user"))
                    .filter_map(|msg| msg.get("content").and_then(|c| c.as_str()))
                    .filter(|content| *content != RIG_SCRIPTED_TURN)
                    .map(str::to_string)
                    .collect()
            }

            /// Context updates the provider accepted, inverted from the
            /// wire truth: the composed LLM leg renders them into the
            /// system message (§4 pure history edit — openai_compat's
            /// `render`: prompt, excerpt, then the chapter/position state
            /// lines). A chapter line proves a ChapterBoundary carried the
            /// current excerpt; a position line proves a PlaybackPosition
            /// applied AFTER any boundary (a boundary clears the position,
            /// so one that survived postdates it) — the order is recovered
            /// from the rendering, never assumed. Cumulative rendering
            /// keeps only the latest state, which covers every sequence
            /// the suite drives.
            async fn received_context_updates(&mut self) -> Vec<ContextUpdate> {
                let exchanges = self.llm_exchanges().await;
                let Some((_, body)) = exchanges.last() else {
                    return Vec::new();
                };
                let value: serde_json::Value =
                    serde_json::from_str(body).expect("conformance rig: request body is JSON");
                let system = value
                    .pointer("/messages/0/content")
                    .and_then(|c| c.as_str())
                    .expect("conformance rig: the system message leads the history");

                let mut chapter_idx = None;
                let mut pos_ms = None;
                let mut body_lines = Vec::new();
                for line in system.lines() {
                    if let Some(idx) = line
                        .strip_prefix("The reader is now at chapter ")
                        .and_then(|rest| rest.strip_suffix('.'))
                        .and_then(|idx| idx.parse::<u32>().ok())
                    {
                        chapter_idx = Some(idx);
                    } else if let Some(pos) = line
                        .strip_prefix("Playback position: ")
                        .and_then(|rest| rest.strip_suffix(" ms."))
                        .and_then(|pos| pos.parse::<u64>().ok())
                    {
                        pos_ms = Some(pos);
                    } else {
                        body_lines.push(line);
                    }
                }

                let mut updates = Vec::new();
                if let Some(chapter_idx) = chapter_idx {
                    let excerpt = body_lines
                        .last()
                        .expect("conformance rig: the excerpt follows the prompt")
                        .to_string();
                    updates.push(ContextUpdate::ChapterBoundary {
                        chapter_idx,
                        excerpt: DelimitedExcerpt {
                            delimited_text: excerpt,
                        },
                    });
                }
                if let Some(pos_ms) = pos_ms {
                    updates.push(ContextUpdate::PlaybackPosition { pos_ms });
                }
                updates
            }

            /// The tool results the provider received: history is
            /// cumulative (openai_compat renders the full history per
            /// request), so the LAST request body carries every
            /// `role:"tool"` message in order; each content string reverses
            /// the slot's envelope rule (`tool_result_content`) back to the
            /// [`ToolResult`] the channel was handed.
            async fn received_tool_results(&mut self) -> Vec<(String, ToolResult)> {
                let exchanges = self.llm_exchanges().await;
                let (_, body) = exchanges
                    .last()
                    .expect("conformance rig: at least one exchange exists");
                let value: serde_json::Value =
                    serde_json::from_str(body).expect("conformance rig: request body is JSON");
                let Some(messages) = value.get("messages").and_then(|m| m.as_array()) else {
                    return Vec::new();
                };
                messages
                    .iter()
                    .filter(|msg| msg.get("role").and_then(|r| r.as_str()) == Some("tool"))
                    .map(|msg| {
                        let call_id = msg
                            .get("tool_call_id")
                            .and_then(|id| id.as_str())
                            .expect("conformance rig: tool messages carry tool_call_id")
                            .to_string();
                        let content: serde_json::Value = serde_json::from_str(
                            msg.get("content")
                                .and_then(|c| c.as_str())
                                .expect("conformance rig: tool content is a string"),
                        )
                        .expect("conformance rig: tool content parses");
                        let single_key = |key: &str| {
                            content
                                .as_object()
                                .is_some_and(|obj| obj.len() == 1 && obj.contains_key(key))
                        };
                        let result = if single_key("error") {
                            ToolResult::Err {
                                code: content
                                    .pointer("/error/code")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                message: content
                                    .pointer("/error/message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                            }
                        } else if single_key("result") {
                            ToolResult::Ok(content.get("result").cloned().expect("checked"))
                        } else {
                            ToolResult::Ok(content)
                        };
                        (call_id, result)
                    })
                    .collect()
            }

            /// The cancels the provider RECEIVED: the wire's `"cancel":true`
            /// frames, in order, each paired with the generation snapshot
            /// its interrupt call observed on entry (call order — every
            /// cancel frame originates in exactly one wrapped `interrupt`).
            async fn received_interrupts(&mut self) -> Vec<Option<u64>> {
                let cancels = self
                    .settled_tts_frames()
                    .await
                    .iter()
                    .filter(|frame| {
                        let WireFrame::Text(text) = frame else {
                            return false;
                        };
                        serde_json::from_str::<serde_json::Value>(text)
                            .ok()
                            .and_then(|value| value.get("cancel").cloned())
                            == Some(serde_json::Value::Bool(true))
                    })
                    .count();
                let observed = self.observed_interrupts.lock().unwrap();
                assert!(
                    cancels <= observed.len(),
                    "conformance rig: a cancel frame the wire received has no \
                     originating interrupt call"
                );
                observed[..cancels].iter().map(|gen| Some(*gen)).collect()
            }
        }

        struct ComposedFixture {
            /// Per-rig script sets, consumed front-first: the core checks rig
            /// once each in fixed order, so each entry is one check's
            /// SSE reply scripts — one script per LLM turn, fixed at rig
            /// time (the LLM provider's scripted side). `run_all`'s trailing
            /// capabilities probe drains nothing (`unwrap_or_default`).
            per_rig: std::collections::VecDeque<Vec<&'static [&'static str]>>,
            rigs: usize,
        }

        #[async_trait]
        impl ConformanceFixture for ComposedFixture {
            type Channel = BargeObservedChannel;
            type Sim = ComposedSim;

            async fn rig(
                &mut self,
                generation: &Generation,
            ) -> (BargeObservedChannel, ComposedSim) {
                self.rigs += 1;
                let scripts = self.per_rig.pop_front().unwrap_or_default();
                let llm_record = Arc::new(Mutex::new(Vec::new()));
                let (base_url, _server) =
                    spawn_chat_server_recording(scripts.clone(), Arc::clone(&llm_record), true);
                let tts_handle = WireServer::start().await;
                let keys = keys_with_slot_keys(&base_url);
                let config = CompatComposedConfig {
                    stt: SttSlotConfig {
                        endpoint_override: None,
                        model: "nova-3".into(),
                    },
                    llm: LlmSlotConfig {
                        base_url,
                        model: "llama-3.3-70b-versatile".into(),
                    },
                    tts: TtsSlotConfig {
                        endpoint_override: Some(tts_handle.url("/tts/websocket")),
                        model: "sonic-2".into(),
                        voice: "voice-abc".into(),
                    },
                };
                let llm = Box::new(OpenAiCompatLlmSlot::new(
                    config.llm.clone(),
                    Arc::clone(&keys),
                ));
                let tts = Box::new(CartesiaTtsSlot::new(config.tts.clone(), Arc::clone(&keys)));
                let (stt_tx, stt_rx) = mpsc::channel(64);
                let stt_received = Arc::new(Mutex::new(Vec::new()));
                let channel = CompatComposedChannel::new(
                    config,
                    Box::new(RigStt {
                        events: Some(stt_rx),
                        received: Arc::clone(&stt_received),
                    }),
                    llm,
                    tts,
                    keys,
                    generation.clone(),
                );
                let observed_interrupts = Arc::new(Mutex::new(Vec::new()));
                let llm_handle = channel.llm_slot_handle();
                let (tts_slot, spoken_turns) = channel.tts_slot_handle();
                (
                    BargeObservedChannel {
                        inner: channel,
                        generation: generation.clone(),
                        observed_interrupts: Arc::clone(&observed_interrupts),
                        llm_record: Arc::clone(&llm_record),
                    },
                    ComposedSim {
                        stt_tx,
                        tts: tts_handle,
                        stt_received,
                        observed_interrupts,
                        contexts: Vec::new(),
                        llm: llm_handle,
                        tts_slot,
                        spoken_turns,
                        scripts,
                        llm_record,
                    },
                )
            }
        }

        /// The rig assembles (cc-1f-i's smoke pin, w2 data throughout): one
        /// committed user utterance flows through all three slot fixtures —
        /// the injector's slice surfaces, the scripted SSE reply streams
        /// back, the boundary's speak opens `turn-1` on the Cartesia wire —
        /// and the sim's `emit_audio` lands under that mapped context as one
        /// byte-exact `AudioDelta` tagged with the speak-time generation.
        /// Wiring `run_all` over this rig is cc-1f-ii.
        #[tokio::test]
        async fn composed_rig_assembles() {
            let mut fixture = ComposedFixture {
                per_rig: [vec![SSE_STOP_TURN]].into(),
                rigs: 0,
            };
            let generation = Generation::new();
            let (mut channel, mut sim) = fixture.rig(&generation).await;
            let mut events = channel.take_events().expect("the receiver is handed over");
            channel
                .connect(fixture.session_context())
                .await
                .expect("composed connect");
            assert_eq!(
                events.recv().await,
                Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
            );

            commit_one_turn(&sim.stt_tx, &mut events).await;
            assert_eq!(
                timeout(Duration::from_secs(5), events.recv())
                    .await
                    .expect("the scripted reply streams back"),
                Some(ChannelEvent::TranscriptDelta {
                    role: Role::Assistant,
                    text: "Figure three shows a phase diagram.".into(),
                })
            );

            // One provider frame through the sim: emit_audio binds gen 0 to
            // the reply's freshly opened context and pushes the chunk.
            sim.emit_audio(0, vec![1000, -1000]).await;
            assert_eq!(
                timeout(Duration::from_secs(5), events.recv())
                    .await
                    .expect("the sim's frame surfaces"),
                Some(ChannelEvent::AudioDelta {
                    pcm: vec![1000, -1000],
                    gen: 0,
                }),
                "the pushed chunk surfaces from the composed stream under the \
                 speak-time generation"
            );

            // The injector's sender lives in the sim: drop it so the loop's
            // STT branch ends and close can join.
            drop(sim);
            channel.close().await.expect("clean close");
            assert_eq!(
                events.recv().await,
                None,
                "the composed stream ends at close"
            );
        }

        /// Check 2's scripted replies: one single-delta turn per assistant
        /// emission the check requests, texts exactly the check's script.
        const SSE_CONF_THE_FIGURE: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"The figure "},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ];
        const SSE_CONF_SHOWS_THE: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"shows the "},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ];
        const SSE_CONF_FIGURE_THREE: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Figure 3, on this page."},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ];

        /// Check 4's scripted tool-call turn — the check's exact call, one
        /// fragmented-arguments split to keep the assembly path honest.
        const SSE_CONF_SEEK_TOOL: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-7","type":"function","function":{"name":"seek_relative","arguments":"{\"seconds\":"}}]},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":" -30}"}}]},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ];

        /// A content-free continuation: the tool answer's follow-up turn
        /// where the check reads back the result and expects no further
        /// events — no prose means no assistant delta and no speak.
        const SSE_CONF_EMPTY_STOP: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ];

        /// Check 5's scripted anchor calls: live figure, superseded section,
        /// out-of-vocabulary page — the check's exact three.
        const SSE_CONF_ANCHOR_LIVE: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"anchor-1","type":"function","function":{"name":"teacher_anchor","arguments":"{\"kind\":\"figure\",\"id\":\"fig-2\"}"}}]},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ];
        const SSE_CONF_ANCHOR_STALE: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"anchor-2","type":"function","function":{"name":"teacher_anchor","arguments":"{\"kind\":\"section\",\"id\":\"sec-9\"}"}}]},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ];
        const SSE_CONF_ANCHOR_MALFORMED: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"anchor-3","type":"function","function":{"name":"teacher_anchor","arguments":"{\"kind\":\"page\",\"id\":\"p-4\"}"}}]},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ];
        const SSE_CONF_ASSISTANT_ANSWER: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"assistant answer"},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ];

        /// The composed channel's conformance RUN (cc-1f-ii): the frozen
        /// core checks (1-5, 8) over the cc-1f-i rig, via `run_all` so the
        /// capability gate itself is under test — `session_renewal: false`
        /// must make run_all skip the renewal trio (the deepgram_agent.rs
        /// precedent for a renewal-less wire), pinned by the exact rig
        /// count. One script set per check, in the core checks' fixed order.
        #[tokio::test]
        async fn composed_passes_session_channel_conformance() {
            let mut fixture = ComposedFixture {
                per_rig: [
                    // 1: open/close — no LLM turn fires.
                    vec![],
                    // 2: transcript ordering — three assistant emissions.
                    vec![
                        SSE_CONF_THE_FIGURE,
                        SSE_CONF_SHOWS_THE,
                        SSE_CONF_FIGURE_THREE,
                    ],
                    // 3: barge-in.
                    vec![],
                    // 4: tool-call round-trip — the scripted call, then the
                    // content-free continuation the tool answer earns.
                    vec![SSE_CONF_SEEK_TOOL, SSE_CONF_EMPTY_STOP],
                    // 5: teacher-anchor normalization — three anchor calls,
                    // each ack earning a content-free continuation.
                    vec![
                        SSE_CONF_ANCHOR_LIVE,
                        SSE_CONF_EMPTY_STOP,
                        SSE_CONF_ANCHOR_STALE,
                        SSE_CONF_EMPTY_STOP,
                        SSE_CONF_ANCHOR_MALFORMED,
                        SSE_CONF_EMPTY_STOP,
                    ],
                    // 8: context updates + text input — the send_text turn's
                    // content-free reply (the updates themselves are pure
                    // history edits; its request body is the readback).
                    vec![SSE_CONF_EMPTY_STOP],
                    // 10: turn boundary — one assistant emission.
                    vec![SSE_CONF_ASSISTANT_ANSWER],
                ]
                .into(),
                rigs: 0,
            };
            run_all(&mut fixture).await;
            assert_eq!(
                fixture.rigs, 8,
                "seven core checks + the capabilities probe — session_renewal: \
                 false means run_all never requested a renewal rig"
            );
        }
    }
}
