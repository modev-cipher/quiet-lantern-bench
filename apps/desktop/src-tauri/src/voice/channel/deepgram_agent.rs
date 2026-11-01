//! VOICE_AGENT §3.1 `DeepgramAgentChannel`): the single WS that orchestrates
//! STT+LLM+TTS server-side, with **BYO-LLM** in the think slot — a SEPARATE
//! full-duplex wire from the `/v1/listen` STT slot (deepgram.rs), sharing
//! only the Deepgram account credential. This unit is struct + config +
//! capabilities + the connect/close lifecycle (the Settings first-message
//! handshake, `Welcome` → `SettingsApplied` awaited before `Connected`),
//! the read loop (turn-scoped TTS audio-out → `AudioDelta`, transcripts →
//! `TranscriptDelta`, `Error` mapped; the rest marked for its later
//! stride — see [`read_loop`]), the binary audio-in hot path
//! (`send_audio`), the idle KeepAlive ticker, barge-in (`interrupt` +
//! the `UserStartedSpeaking` decode — the ruling below), and the LLM-facing
//! session verbs (the dgva-1f stride): the client-side function-call bridge
//! (`FunctionCallRequest` → `ToolCall` → `send_tool_result` →
//! `FunctionCallResponse`), the `InjectUserMessage` text turn, and the
//! `UpdatePrompt` context refresh — every trait method is implemented.
//!
//! Wire protocol provenance (all accessed 2026-08-21):
//!
//! - Endpoint: `wss://agent.deepgram.com/v1/agent/converse`
//!   (https://developers.deepgram.com/reference/voice-agent/voice-agent — the
//!   AsyncAPI protocol reference; the docs landing page
//!   https://developers.deepgram.com/docs/voice-agent names it the "complete
//!   WebSocket protocol reference for the Agent API").
//!
//! - Auth: the reference declares an `Authorization` header on the WS
//!   upgrade (it does not restate the token scheme); the scheme is the
//!   account-wide `Authorization: Token <key>` that `voice::keys::probe`
//!   pins for Deepgram and deepgram.rs uses on the listen wire — one
//!   Deepgram account key drives both hosts, so the credential identity
//!   stays `Provider::Deepgram.default_base_url()`
//!   (`https://api.deepgram.com`) even though the socket dials
//!   `agent.deepgram.com`. Connect URLs stay credential-free (VOICE_AGENT
//!   §3.2 `auth-hygiene-no-query-param-creds`).
//!
//! - Settings handshake (same reference): unlike the listen wire (query
//!   params, no setup message), the agent session is configured by a
//!   **first JSON text message** `{"type": "Settings", "audio": {...},
//!   "agent": {...}}` — `audio.input`/`audio.output` carry
//!   `encoding`/`sample_rate` (default `linear16` @ 24000; this adapter will
//!   pin input to the VA1 16 kHz seam and output to the 24 kHz playback
//!   seam), and `agent` carries the three provider slots:
//!   - `agent.listen.provider`: `type: "deepgram"` with `version` `"v1"`
//!     (`model` e.g. `nova-3`, `smart_format`, `keyterms[]`) or `"v2"`
//!     (Flux: `eot_threshold`, `eager_eot_threshold`, `eot_timeout_ms`);
//!   - `agent.think`: `provider` one of `open_ai` / `anthropic` /
//!     `aws_bedrock` / `google` / `groq`, plus `prompt`, `functions[]`, and
//!     — the **BYO-LLM** hook — an optional `endpoint`: "Optional for
//!     non-Deepgram LLM providers. When present, must include url field and
//!     headers object", i.e. `{"url": ..., "headers": {...}}` pointing at
//!     any OpenAI-compatible endpoint (VOICE_AGENT §3.1 roster note;
//!     `context_length` is only configurable with a custom think endpoint);
//!   - `agent.speak`: `provider` `deepgram` (Aura v1 / Flux TTS v2 models)
//!     or external TTS providers, each with their own `endpoint` rules.
//!
//!   The server acks with `{"type": "SettingsApplied"}` (preceded by a
//!   `{"type": "Welcome", "request_id": ...}` on connect) — that ack, not
//!   the socket open, is this channel's `Lifecycle(Connected)` moment.
//!
//! - Framing (same reference): client→server audio is **raw binary WS
//!   frames** per `audio.input` (no envelope — same as the listen wire,
//!   opposite of Gemini's base64-in-JSON, VOICE_AGENT §1 wire-format
//!   reality); server→client TTS audio is **raw binary frames** per
//!   `audio.output`, with the end of each response's audio burst signaled by
//!   the JSON event `{"type": "AgentAudioDone"}`. Control/events are JSON
//!   text frames in both directions.
//!
//! - Client→server JSON vocabulary (exact names, same reference):
//!   `Settings`, `UpdateListen`, `UpdateThink`, `UpdateSpeak`,
//!   `UpdatePrompt`, `InjectUserMessage`, `InjectAgentMessage` (behavior:
//!   default/queue/interrupt), `FunctionCallResponse` (answers a
//!   client-side `FunctionCallRequest` by matching `id`), `KeepAlive` —
//!   plus the binary audio frames.
//!
//! - Server→client JSON vocabulary (exact names, same reference):
//!   `Welcome`, `SettingsApplied`, `ListenUpdated`, `ThinkUpdated`,
//!   `SpeakUpdated`, `PromptUpdated`, `ConversationText` (role
//!   user/assistant — the transcript stream for BOTH sides of the
//!   conversation), `UserStartedSpeaking` (the barge-in trigger),
//!   `AgentThinking`, `AgentStartedSpeaking` (with
//!   total/tts/ttt latencies), `AgentAudioDone`, `LatencyReport`,
//!   `FunctionCallRequest` (`functions[]` with `id`, `name`, `arguments`,
//!   `client_side`), `FunctionCallResponse` (server-side results),
//!   `InjectionRefused`, `History`, `Error` (`description`, `code`),
//!   `Warning` — plus the binary audio frames.
//!
//! - Barge-in (this unit — the dgva-1a flag, resolved 2026-08-21 against the
//!   AsyncAPI reference above plus
//!   https://developers.deepgram.com/docs/voice-agent-user-started-speaking):
//!   the client vocabulary has **no cancel frame** — none of the ten client
//!   message types stops the agent's current speech. `InjectAgentMessage`
//!   `behavior: "interrupt"` exists but is a speak verb — "The agent
//!   immediately speaks" the injected `message`, replacing current speech —
//!   not a cancel; the only wire evidence the server cuts its own generation
//!   on user speech is that same field's doc text ("the user's continued
//!   speech triggers `UserStartedSpeaking`, which quickly interrupts the
//!   agent"). What the docs DO mandate is client-side: on
//!   `UserStartedSpeaking` the client must "stop any ongoing agent audio
//!   playback and discard any buffered audio" — which is exactly the §2
//!   rule 2 generation-gate drop. So, mirroring gemini.rs: `interrupt` sends
//!   nothing and bumps the shared barge-in generation (the gen-tag drop IS
//!   the cancel mechanism), and — because on this Server-VAD wire only the
//!   provider can detect user speech — the read loop performs the same bump
//!   when it decodes `UserStartedSpeaking`, leaving the open turn's snapshot
//!   stale so its in-flight remnants die at the playback gate
//!   (`TaggedPlaybackWriter::push_block`).
//!
//! - KeepAlive (https://developers.deepgram.com/docs/agent-keep-alive):
//!   "Most agent conversations do not need `KeepAlive`" — continuous mic
//!   streaming keeps the wire alive — but the server closes connections
//!   that go silent, so while idle the doc prescribes one
//!   `{"type": "KeepAlive"}` text frame **every 8 seconds** (never
//!   acknowledged; does not extend the 2-hour maximum session length —
//!   that hard cap is a `Lifecycle` concern for the session-lifecycle
//!   stride).
//!
//! - **BYO-LLM double-disclosure (owner directive).** Deepgram BYO-LLM is
//!   relay plumbing: the custom LLM endpoint URL + its key are sent **to
//!   Deepgram** inside `Settings.agent.think.endpoint`, and Deepgram calls
//!   the endpoint server-side (VOICE_AGENT §3.2 custom-endpoint routing
//!   rule). When the think slot is user-provided, retention disclosures
//!   must therefore cover **both** Deepgram AND the BYO-LLM provider. The
//!   composed consent copy sources this from docs/VOICE_AGENT.md: the §3.2
//!   custom-endpoint routing rule (the first-use interstitial "names
//!   **every** host involved (for BYO-LLM: Deepgram *and* the LLM endpoint,
//!   stating that the LLM key transits Deepgram)") and the §7 privacy
//!   defaults ("Settings link each provider's data-retention policy; the
//!   retention disclosures list every provider-bound data category
//!   uniformly — audio, transcripts, book excerpts …"). The consent-stride
//!   implementation composes those two clauses per configured think
//!   endpoint; nothing here may connect before that interstitial exists.
//!
//! Capability posture (flags justified at [`SessionChannel::capabilities`]):
//! full-duplex single socket, server-owned turn detection, no word-aligned
//! timing on the agent wire.

use std::collections::HashMap;
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
use crate::voice::audio::{PROVIDER_INPUT_RATE, PROVIDER_OUTPUT_RATE};
use crate::voice::keys::{ApiKey, KeyManager};

use super::{
    ChannelCapabilities, ChannelError, ChannelEvent, ContextUpdate, ErrorKind, LifecycleEvent,
    Role, SessionChannel, SessionContext, ToolResult, VadMode,
};

/// Default agent endpoint (module header provenance);
/// [`DeepgramAgentConfig::endpoint_override`] replaces it for proxies and
/// recorded-fixture servers, exactly like deepgram.rs.
pub const DEFAULT_AGENT_ENDPOINT: &str = "wss://agent.deepgram.com/v1/agent/converse";

/// The canonical base URL the user's Deepgram key is stored under
/// (`Provider::Deepgram.default_base_url()`): one account key drives both
/// `api.deepgram.com` (listen) and `agent.deepgram.com` (this wire), so the
/// key lookup is by this identity even though the socket dials the agent
/// host — the same endpoint/credential split deepgram.rs documents.
const DEEPGRAM_AGENT_KEY_BASE_URL: &str = "https://api.deepgram.com";

const EVENT_CAPACITY: usize = 4096;

/// How long connect waits for the server's `Welcome` and `SettingsApplied`
/// acks, and close for the read loop to drain, before giving up — the same
/// budget gemini.rs applies to its `setupComplete` wait.
const HANDSHAKE_BUDGET: Duration = Duration::from_secs(10);

/// The idle heartbeat (module header, KeepAlive provenance): a JSON **text**
/// frame — control never rides binary on a Deepgram wire — that the server
/// never acknowledges.
const KEEPALIVE_FRAME: &str = r#"{"type":"KeepAlive"}"#;

/// Idle gap after this side's last frame before the KeepAlive ticker fires:
/// the agent wire's own prescribed cadence — "While idle, send one KeepAlive
/// every 8 seconds" (https://developers.deepgram.com/docs/agent-keep-alive,
/// accessed 2026-08-21) — unlike the listen wire's 3-5 s against its
/// 10-second NET-0001 window. The same doc pins the 2-hour maximum session
/// length KeepAlive does NOT extend, and defines no expiry warning or
/// renewal signal before that close (checked 2026-08-21: nothing like
/// Gemini's GoAway exists in the vocabulary) — so the cap surfaces as a
/// plain server-side close, a `Lifecycle` concern for the session-lifecycle
/// stride, and the simple New/Connected/Closed gate stays correct here.
const KEEPALIVE_IDLE_INTERVAL: Duration = Duration::from_secs(8);

/// The think-slot (LLM) configuration carried into
/// `Settings.agent.think` (module header, Settings provenance). Credentials
/// are deliberately absent, as everywhere in VA5: when
/// [`byo_endpoint_url`](Self::byo_endpoint_url) is set, the endpoint's key is
/// resolved at connect time through the VA3 [`KeyManager`] under that base
/// URL (per-base-URL key isolation, VOICE_AGENT §3.2) and placed into the
/// wire message's `think.endpoint.headers` — never into this struct.
#[derive(Clone, Debug)]
pub struct DeepgramAgentThinkConfig {
    /// Wire provider label for `think.provider.type` — `open_ai` is the
    /// label the OpenAI-compatible clause rides (VOICE_AGENT §3.2 LLM-slot
    /// notes: Kimi/Qwen/DeepSeek/Groq/aggregators are protocol-shape
    /// examples, not an allowlist).
    pub provider_type: String,
    /// Model name passed through to the think provider.
    pub model: String,
    /// BYO-LLM: the custom OpenAI-compatible endpoint for
    /// `think.endpoint.url` ("Optional for non-Deepgram LLM providers" —
    /// module header, Settings provenance). `None` means a Deepgram-hosted
    /// think provider. When `Some`, the BYO-LLM double-disclosure applies
    /// (module header, owner directive) — this URL and its key transit
    /// Deepgram.
    pub byo_endpoint_url: Option<String>,
}

/// Adapter configuration for the agent wire. Mirrors [`super::DeepgramConfig`]
/// where the wires overlap (endpoint override, credential-free), and adds the
/// three Settings slots the agent wire configures in its first message.
#[derive(Clone, Debug)]
pub struct DeepgramAgentConfig {
    /// Full WS endpoint replacing [`DEFAULT_AGENT_ENDPOINT`] when set.
    pub endpoint_override: Option<String>,
    /// `agent.listen.provider.model` (v1, e.g. `nova-3`; Flux is a
    /// version/model change inside the same slot — module header, Settings
    /// provenance).
    pub listen_model: String,
    /// `agent.speak.provider.model` (Deepgram-hosted, e.g. an `aura-2-*`
    /// voice; external TTS providers are out of this channel's scope — that
    /// composition belongs to `CompatComposedChannel`, VOICE_AGENT §3.2).
    pub speak_model: String,
    /// The think slot, including the BYO-LLM endpoint hook.
    pub think: DeepgramAgentThinkConfig,
}

/// Session state, the uniform contract the trait documents: connect is valid
/// exactly once from `New`; a closed session never reopens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    New,
    Connected,
    Closed,
}

/// What `think.prompt` is rendered from — the single composition rule shared
/// by the Settings first message and every `UpdatePrompt` refresh. On this
/// wire the ONLY mid-session context mechanism is `UpdatePrompt`, and it
/// REPLACES the whole system prompt (module header, client vocabulary:
/// "The new system prompt to be used by the agent"), so each refresh must
/// re-render the complete prompt — unlike Gemini's appended `clientContent`
/// note. The channel therefore keeps these parts across the session:
/// `ContextUpdate::ChapterBoundary` swaps the excerpt (and clears the
/// playback position — a position from the old chapter is stale against the
/// new excerpt), `ContextUpdate::PlaybackPosition` updates the position line.
/// The delimited excerpt travels inside verbatim — the adapter never parses
/// it (VOICE_AGENT §4); refreshes happen only at the harness's chosen
/// boundaries (§4: never a blind mid-turn re-prompt — the harness owns when
/// `update_context` fires, this type only renders).
#[derive(Debug, Clone)]
struct ThinkPrompt {
    system_prompt: String,
    /// The current delimited excerpt (`DelimitedExcerpt::delimited_text`).
    excerpt: String,
    chapter_idx: Option<u32>,
    playback_pos_ms: Option<u64>,
    /// The renewal recap (`ContextUpdate::RenewalRecap`) — delimited
    /// transcript text, rendered behind the shared framing (§4).
    renewal_recap: Option<String>,
    /// The resume recap (`ContextUpdate::ResumeRecap`): the composed framing
    /// and the delimited body, rendered behind the same `Note:` seam.
    resume_recap: Option<(String, String)>,
    /// The JIT code-switching vocabulary (`ContextUpdate::CodeSwitchFragment`)
    /// — present only while the fragment is active; `None` renders nothing.
    code_switch_terms: Option<Vec<String>>,
}

impl ThinkPrompt {
    fn new(ctx: &SessionContext) -> Self {
        Self {
            system_prompt: ctx.system_prompt.clone(),
            excerpt: ctx.excerpt.delimited_text.clone(),
            chapter_idx: None,
            playback_pos_ms: None,
            renewal_recap: None,
            resume_recap: None,
            code_switch_terms: None,
        }
    }

    /// The composition rule: system prompt, then the delimited excerpt, then
    /// the lightweight-state lines when set. With no state set this is exactly
    /// the dgva-1b Settings composition (`{system}\n{excerpt}`), so connect
    /// and refresh can never drift apart.
    fn render(&self) -> String {
        let mut prompt = format!("{}\n{}", self.system_prompt, self.excerpt);
        if let Some(chapter_idx) = self.chapter_idx {
            prompt.push_str(&format!("\nThe reader is now at chapter {chapter_idx}."));
        }
        if let Some(pos_ms) = self.playback_pos_ms {
            prompt.push_str(&format!("\nPlayback position: {pos_ms} ms."));
        }
        if let Some(recap) = &self.renewal_recap {
            prompt.push_str(&format!("\nNote: {recap}"));
        }
        if let Some((framing, body)) = &self.resume_recap {
            prompt.push_str(&format!("\nNote: {framing}\n{body}"));
        }
        if let Some(terms) = &self.code_switch_terms {
            prompt.push_str(&format!(
                "\nThe user speaks Arabic mixed with English technical terms; \
                 the transcription may render them in Arabic script. Technical \
                 vocabulary near the current reading position: {}. Treat a \
                 close Arabic-script rendering of any of these as that \
                 English term.",
                terms.join(", ")
            ));
        }
        prompt
    }

    /// One context update folded into the stored parts (the harness owns
    /// when this fires, never mid-turn).
    fn apply(&mut self, update: &ContextUpdate) {
        match update {
            ContextUpdate::ChapterBoundary {
                chapter_idx,
                excerpt,
            } => {
                self.excerpt = excerpt.delimited_text.clone();
                self.chapter_idx = Some(*chapter_idx);
                // A position from the old chapter is stale against the new
                // excerpt; it returns with the next PlaybackPosition update.
                self.playback_pos_ms = None;
            }
            ContextUpdate::PlaybackPosition { pos_ms } => {
                self.playback_pos_ms = Some(*pos_ms);
            }
            ContextUpdate::RenewalRecap { recap, switched_to } => {
                self.renewal_recap = Some(format!(
                    "{}\n{}",
                    crate::voice::channel::types::renewal_recap_framing(*switched_to),
                    recap.delimited_text
                ));
            }
            ContextUpdate::ResumeRecap {
                recap,
                origin,
                location,
                topic,
                lang,
            } => {
                self.resume_recap = Some((
                    crate::voice::channel::types::resume_recap_framing(
                        origin,
                        location.as_ref(),
                        topic.as_deref(),
                        *lang,
                    ),
                    recap.delimited_text.clone(),
                ));
            }
            ContextUpdate::CodeSwitchFragment { terms, active } => {
                self.code_switch_terms = active.then(|| terms.clone());
            }
        }
    }
}

type AgentSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type AgentWriter = SplitSink<AgentSocket, Message>;

/// The Deepgram Voice Agent session channel — connect/close lifecycle (the
/// Settings first-message handshake), the read loop (turn-scoped TTS
/// audio-out, transcripts, and the client-side function-call bridge), the
/// binary audio-in hot path, the idle KeepAlive ticker, and the full set of
/// session verbs: `send_text` (`InjectUserMessage`), `send_tool_result`
/// (`FunctionCallResponse`), and `update_context` (`UpdatePrompt`).
pub struct DeepgramAgentChannel {
    config: DeepgramAgentConfig,
    keys: Arc<KeyManager>,
    /// Tags outbound `AudioDelta`s and is bumped by barge-in — the agent
    /// wire DOES emit assistant audio (unlike the listen wire), so the
    /// generation gate is load-bearing here (VOICE_AGENT §2). The read loop
    /// snapshots it per response turn ([`read_loop`]'s turn scoping).
    generation: Generation,
    state: SessionState,
    /// Write half of the split socket, live while `Connected`. The same
    /// shared-slot shape as deepgram.rs; the KeepAlive ticker writes off the
    /// session task through this slot, and finding it empty is the ticker's
    /// stop signal.
    writer: Arc<tokio::sync::Mutex<Option<AgentWriter>>>,
    /// The inbound decode loop; joined (budgeted) at `close`.
    reader: Option<JoinHandle<()>>,
    /// When this side last put a frame on the wire (audio or KeepAlive); the
    /// KeepAlive ticker sleeps against this, so every `send_audio` resets the
    /// idle countdown for free — the dg-1d ticker shape.
    last_send: Arc<std::sync::Mutex<tokio::time::Instant>>,
    /// The idle-heartbeat ticker (module header, KeepAlive provenance),
    /// spawned at connect and aborted at `close`.
    keepalive: Option<JoinHandle<()>>,
    /// [`KEEPALIVE_IDLE_INTERVAL`] in production; tests shrink it so the idle
    /// threshold is crossable inside a test budget.
    keepalive_interval: Duration,
    /// The event pair exists from construction: `take_events` must yield the
    /// receiver pre-connect (conformance check 1, matching MockChannel's
    /// build-time pair); connect and the read loop only clone this sender.
    events_tx: Option<mpsc::Sender<ChannelEvent>>,
    events: Option<mpsc::Receiver<ChannelEvent>>,
    /// Names of client-side tool calls surfaced but not yet answered, by call
    /// id (the gemini.rs bookkeeping shape): the read loop records each
    /// bridged `FunctionCallRequest.functions[]` entry, and `send_tool_result`
    /// needs the name because the wire `FunctionCallResponse` carries it
    /// alongside the matching `id` (module header, client vocabulary). Only
    /// `client_side: true` entries ever enter this map — server-side functions
    /// are Deepgram's own to execute, so a result for one is as unknown here
    /// as a made-up id. Unlike Gemini there is no cancellation frame in the
    /// vocabulary (no `toolCallCancellation` equivalent, checked 2026-08-21),
    /// so no cancelled-set exists: unknown, already-answered, and server-side
    /// ids all reject the same way.
    pending_tool_calls: Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// The parts `think.prompt` is re-rendered from on `update_context`
    /// (see [`ThinkPrompt`]); `Some` from connect on.
    think_prompt: Option<ThinkPrompt>,
}

impl DeepgramAgentChannel {
    pub fn new(config: DeepgramAgentConfig, keys: Arc<KeyManager>, generation: Generation) -> Self {
        let (events_tx, events) = mpsc::channel(EVENT_CAPACITY);
        Self {
            config,
            keys,
            generation,
            state: SessionState::New,
            writer: Arc::new(tokio::sync::Mutex::new(None)),
            reader: None,
            last_send: Arc::new(std::sync::Mutex::new(tokio::time::Instant::now())),
            keepalive: None,
            keepalive_interval: KEEPALIVE_IDLE_INTERVAL,
            events_tx: Some(events_tx),
            events: Some(events),
            pending_tool_calls: Arc::new(std::sync::Mutex::new(HashMap::new())),
            think_prompt: None,
        }
    }

    /// Shrinks the KeepAlive idle threshold so a test can cross it in real
    /// time; must be called before `connect` (the ticker snapshots it there).
    #[cfg(test)]
    fn set_keepalive_interval(&mut self, interval: Duration) {
        self.keepalive_interval = interval;
    }

    /// The endpoint the session will dial: the override when present, else
    /// [`DEFAULT_AGENT_ENDPOINT`]. No query params — unlike the listen wire,
    /// the whole session config rides the Settings first message.
    fn endpoint(&self) -> &str {
        self.config
            .endpoint_override
            .as_deref()
            .unwrap_or(DEFAULT_AGENT_ENDPOINT)
    }

    /// The `Settings` first message (module header, Settings provenance):
    /// `audio.input` pinned to the VA1 16 kHz mic seam and `audio.output` to
    /// the 24 kHz playback seam, `agent.listen`/`agent.speak` the
    /// Deepgram-hosted models from config, and `agent.think` the LLM slot —
    /// the [`ThinkPrompt`] rendering (system prompt + delimited excerpt; the
    /// adapter never parses the excerpt, VOICE_AGENT §4) as `think.prompt`,
    /// the same composition `update_context`'s `UpdatePrompt` replaces it
    /// with, plus, when the
    /// think slot is BYO, the `endpoint {url, headers}` hook: the BYO key
    /// rides `headers.authorization` as `Bearer <key>` (the OpenAI-compatible
    /// scheme) **inside a message sent to Deepgram** — the §3.2
    /// custom-endpoint routing rule's double-disclosure implication (module
    /// header, owner directive): the LLM key transits Deepgram, so retention
    /// disclosures must name both hosts. The Deepgram account key never
    /// appears here — it rides the upgrade header only.
    fn settings_message(&self, think_prompt: &str, byo_key: Option<&ApiKey>) -> String {
        let mut think = serde_json::json!({
            "provider": {
                "type": self.config.think.provider_type,
                "model": self.config.think.model,
            },
            "prompt": think_prompt,
        });
        if let (Some(url), Some(key)) = (self.config.think.byo_endpoint_url.as_deref(), byo_key) {
            think["endpoint"] = serde_json::json!({
                "url": url,
                "headers": {
                    "authorization": format!("Bearer {}", key.expose()),
                },
            });
        }
        serde_json::json!({
            "type": "Settings",
            "audio": {
                "input": { "encoding": "linear16", "sample_rate": PROVIDER_INPUT_RATE },
                "output": { "encoding": "linear16", "sample_rate": PROVIDER_OUTPUT_RATE },
            },
            "agent": {
                "listen": {
                    "provider": {
                        "type": "deepgram",
                        "version": "v1",
                        "model": self.config.listen_model,
                    }
                },
                "think": think,
                "speak": {
                    "provider": {
                        "type": "deepgram",
                        "model": self.config.speak_model,
                    }
                },
            },
        })
        .to_string()
    }

    /// Reads frames until the server has acked the Settings handshake, in the
    /// documented order: `Welcome` (sent on connect — it may already be
    /// queued before our Settings is even processed, which is why this runs
    /// only after the Settings send), then `SettingsApplied` — the ack that
    /// IS the ready signal, mirroring Gemini's `setupComplete`. A server
    /// `Error` frame anywhere in the handshake is a clean non-retryable
    /// connect failure carrying the frame's `description`/`code`; any other
    /// unexpected frame is a protocol violation.
    async fn await_settings_applied(socket: &mut AgentSocket) -> Result<(), ChannelError> {
        let wait = async {
            let mut welcomed = false;
            while let Some(frame) = socket.next().await {
                let message = frame.map_err(|err| {
                    ChannelError::new(
                        ErrorKind::Transport,
                        true,
                        format!("agent ws dropped during the Settings handshake: {err}"),
                    )
                })?;
                let Message::Text(text) = message else {
                    continue;
                };
                let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|err| {
                    ChannelError::new(
                        ErrorKind::Protocol,
                        false,
                        format!("non-JSON frame during the Settings handshake: {err}"),
                    )
                })?;
                match parsed.get("type").and_then(|kind| kind.as_str()) {
                    Some("Welcome") if !welcomed => welcomed = true,
                    Some("SettingsApplied") if welcomed => return Ok(()),
                    Some("Error") => return Err(handshake_rejection(&parsed)),
                    other => {
                        return Err(ChannelError::new(
                            ErrorKind::Protocol,
                            false,
                            format!(
                                "expected {} during the Settings handshake, got {other:?}",
                                if welcomed {
                                    "SettingsApplied"
                                } else {
                                    "Welcome"
                                }
                            ),
                        ))
                    }
                }
            }
            Err(ChannelError::new(
                ErrorKind::Transport,
                true,
                "agent ws closed before SettingsApplied",
            ))
        };
        timeout(HANDSHAKE_BUDGET, wait).await.map_err(|_| {
            ChannelError::new(
                ErrorKind::Provider,
                true,
                "the provider never acknowledged the Settings handshake",
            )
        })?
    }
}

/// A server `Error` frame during the handshake, as the connect failure: the
/// provider rejected the session (bad Settings, rejected BYO endpoint, …), so
/// retrying the same connect can only fail the same way — non-retryable,
/// carrying the frame's own `description` and `code` (module header, server
/// vocabulary).
fn handshake_rejection(frame: &serde_json::Value) -> ChannelError {
    let description = frame
        .get("description")
        .and_then(|text| text.as_str())
        .unwrap_or("(no description)");
    let code = frame
        .get("code")
        .and_then(|text| text.as_str())
        .unwrap_or("(no code)");
    ChannelError::new(
        ErrorKind::Provider,
        false,
        format!("the agent rejected the Settings handshake: {description} (code {code})"),
    )
}

/// One tool-call answer as the wire message: a `FunctionCallResponse` whose
/// `id` matches the inbound `FunctionCallRequest` and which carries the
/// call's `name` (module header, client vocabulary — fields verified against
/// the AsyncAPI reference, accessed 2026-08-21: `type`/`id`/`name`/`content`,
/// with `id` "Required for client responses"). The wire `content` field is a
/// **string** ("The content or result of the function call"), so the uniform
/// `ToolResult` is serialized into it as JSON text under the same envelope
/// rule as gemini.rs's `tool_response_message`: an `Ok` object passes
/// through, a non-object `Ok` is wrapped as `{"result": …}`, and an `Err`
/// becomes `{"error": {code, message}}` — the harness-side ack vocabulary
/// (`super::anchor` codes included) travels identically on every wire.
fn function_call_response_message(call_id: &str, name: &str, result: &ToolResult) -> String {
    let payload = match result {
        ToolResult::Ok(value) if value.is_object() => value.clone(),
        ToolResult::Ok(value) => serde_json::json!({ "result": value }),
        ToolResult::Err { code, message } => serde_json::json!({
            "error": { "code": code, "message": message }
        }),
    };
    serde_json::json!({
        "type": "FunctionCallResponse",
        "id": call_id,
        "name": name,
        "content": payload.to_string(),
    })
    .to_string()
}

/// One user-typed turn as the wire message (module header, client
/// vocabulary; the §3 no-mic fallback works natively on this wire):
/// `{"type": "InjectUserMessage", "content"}` — `content` is "The specific
/// phrase or statement the agent should respond to" (AsyncAPI reference,
/// accessed 2026-08-21). Deliberately NOT `InjectAgentMessage`, whose
/// `message` the agent would SPEAK rather than respond to.
fn inject_user_message(text: &str) -> String {
    serde_json::json!({ "type": "InjectUserMessage", "content": text }).to_string()
}

/// One context refresh as the wire message: `{"type": "UpdatePrompt",
/// "prompt"}` — `prompt` is "The new system prompt to be used by the agent"
/// (AsyncAPI reference, accessed 2026-08-21), i.e. a full REPLACEMENT of the
/// Settings `think.prompt`, so the caller passes a complete
/// [`ThinkPrompt::render`] and never a bare delta.
fn update_prompt_message(prompt: &str) -> String {
    serde_json::json!({ "type": "UpdatePrompt", "prompt": prompt }).to_string()
}

/// The idle-heartbeat ticker (module header, KeepAlive provenance; the
/// dg-1d ticker shape): whenever `interval` passes with nothing sent, put
/// the [`KEEPALIVE_FRAME`] text frame on the wire. Sleeps against the shared
/// `last_send` mark — every `send_audio` pushes the deadline out, so the
/// frame only rides an idle wire ("Most agent conversations do not need
/// KeepAlive" — continuous mic streaming keeps it alive). Stops when the
/// writer slot is empty (`close` took it) or the send fails (the socket
/// ended); `close` also aborts it outright, before draining the slot.
async fn keepalive_loop(
    writer: Arc<tokio::sync::Mutex<Option<AgentWriter>>>,
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

/// Inbound loop for the life of the session, decoding server frames into the
/// uniform event stream:
///
/// - Binary frames are the server's TTS audio — raw little-endian PCM16 per
///   the pinned `audio.output` (`linear16` @ 24 kHz, module header, framing
///   provenance) — and become `AudioDelta` events **tagged by response turn,
///   not by read time** (the gemini.rs turn-scoping shape): the turn's opener
///   snapshots the barge-in generation ONCE, every delta of the turn carries
///   that snapshot, and `AgentAudioDone` retires it. A turn superseded
///   mid-flight (generation bumped after its snapshot) keeps the STALE tag,
///   so its late frames die at the playback gate
///   (`TaggedPlaybackWriter::push_block`) while the next turn snapshots the
///   bumped generation afresh.
/// - `AgentStartedSpeaking` is the documented turn opener; `AgentAudioDone`
///   ("the agent has finished sending audio") closes the turn. But the
///   AsyncAPI reference (module header provenance, accessed 2026-08-21)
///   guarantees NO ordering between the JSON events and the binary frames —
///   nothing says audio cannot precede its `AgentStartedSpeaking`. So a
///   binary frame arriving with no open turn **opens one implicitly,
///   tagging with the current generation**: dropping would eat the head of
///   agent speech, and buffering waits on an opener the wire never promised;
///   the one delimiter the vocabulary does pin is `AgentAudioDone`, which
///   retires implicit turns exactly like explicit ones. An
///   `AgentStartedSpeaking` landing while a turn is already open keeps the
///   existing snapshot — the whole response is live or stale as a unit.
/// - `ConversationText {role, content}` — the transcript stream for BOTH
///   sides — becomes `TranscriptDelta` in arrival order, role mapped
///   user/assistant. No word timing exists on this wire (capabilities:
///   `word_timestamps: false`, the dgva-1a sentence-level degrade), so the
///   deltas are plain text.
/// - `UserStartedSpeaking` — the wire's barge-in signal on this Server-VAD
///   channel — bumps the shared barge-in generation right here at decode
///   (module header, barge-in provenance): the docs mandate the client stop
///   playback and discard buffered agent audio on receipt, the generation
///   gate is that mechanism, and no one upstream ever sees the frame. The
///   open turn is deliberately NOT retired: its remnants keep the dead tag.
/// - `FunctionCallRequest` is the function-call bridge (module header, server
///   vocabulary): each `functions[]` entry with `client_side: true` surfaces
///   as one `ToolCall`, id/name/args passed through uninterpreted — the
///   harness owns the §11.1 `teacher_anchor` normalization (`super::anchor`)
///   and answers via `send_tool_result`, exactly the gemini.rs split — with
///   the entry's name remembered in `pending_tool_calls` so the answer's
///   `FunctionCallResponse` can carry it. Entries that are not client-side
///   are Deepgram's own to execute server-side: never bridged, never pending.
///
/// The loop ends when the socket does — after `close`'s WS close handshake
/// that is the clean exit.
async fn read_loop(
    mut reader: SplitStream<AgentSocket>,
    events: mpsc::Sender<ChannelEvent>,
    generation: Generation,
    pending_tool_calls: Arc<std::sync::Mutex<HashMap<String, String>>>,
) {
    // The open response turn's generation snapshot, `None` between turns:
    // taken once at the turn's opener (`AgentStartedSpeaking`, or the first
    // orphan binary frame — see the doc comment), carried by every audio
    // delta of the turn, retired by `AgentAudioDone`.
    let mut turn_gen: Option<u64> = None;
    while let Some(Ok(message)) = reader.next().await {
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(bytes) => {
                // TTS audio (doc comment above): whole samples only —
                // `linear16` frames carry whole PCM16 samples, so a dangling
                // odd byte is unrepresentable and `chunks_exact` drops it.
                let pcm: Vec<i16> = bytes
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                if pcm.is_empty() {
                    // Nothing to play, so nothing to open a turn over.
                    continue;
                }
                // No open turn: implicit open, tag-with-current (the
                // documented choice — doc comment above). An open turn keeps
                // its snapshot however late this frame was read.
                let gen = *turn_gen.get_or_insert_with(|| generation.current());
                if events
                    .send(ChannelEvent::AudioDelta { pcm, gen })
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
            _ => continue,
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        match value.get("type").and_then(|kind| kind.as_str()) {
            // A mid-session `Error` frame maps to the uniform error event —
            // the §9 ladder reacts to kind/retryable, never the provider
            // shape. Retryable: the session may still be serviceable (the
            // wire stays open unless the server also closes it).
            Some("Error") => {
                let description = value
                    .get("description")
                    .and_then(|text| text.as_str())
                    .unwrap_or("(no description)");
                let code = value
                    .get("code")
                    .and_then(|text| text.as_str())
                    .unwrap_or("(no code)");
                if events
                    .send(ChannelEvent::Error(ChannelError::new(
                        ErrorKind::Provider,
                        true,
                        format!("agent error frame: {description} (code {code})"),
                    )))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // The transcript stream for BOTH sides of the conversation
            // (module header, server vocabulary): role user/assistant maps to
            // the uniform `Role`; anything else the vocabulary might grow is
            // skipped, not an error. Missing/non-string `content` is skipped
            // the same way — an empty delta carries nothing downstream.
            Some("ConversationText") => {
                let role = match value.get("role").and_then(|role| role.as_str()) {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    _ => continue,
                };
                let Some(content) = value.get("content").and_then(|text| text.as_str()) else {
                    continue;
                };
                if events
                    .send(ChannelEvent::TranscriptDelta {
                        role,
                        text: content.to_string(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                if role == Role::User && events.send(ChannelEvent::UtteranceEnd).await.is_err() {
                    return;
                }
            }
            // The wire's barge-in signal (dgva-1e ruling, module header
            // barge-in provenance): on this Server-VAD channel only the
            // provider detects user speech, so this frame is the ONLY place
            // the barge-in can start — the harness has no client-side turn
            // signal to drive `barge_in` from (VOICE_AGENT §3.1 double-VAD
            // rule). The doc's mandate on receipt — stop playback, discard
            // buffered agent audio — IS the §2 rule 2 local half, so the
            // bump lands here, at decode: the open turn keeps its now-stale
            // snapshot (deliberately NOT retired — its in-flight remnants
            // must keep the dead tag and die at the playback gate; retiring
            // would let them implicit-open a fresh-tagged turn and play),
            // and the next `AgentStartedSpeaking` snapshots the bumped
            // generation afresh after `AgentAudioDone` retires the cut-off
            // turn. No network half follows: the server already cut its own
            // generation (the ruling's citation trail, module header), and
            // the client vocabulary has no cancel frame to send anyway.
            Some("UserStartedSpeaking") => {
                generation.bump();
            }
            // The documented turn opener: snapshot the barge-in generation
            // ONCE. `if none`: an implicit-opened turn (orphan audio, doc
            // comment above) keeps its snapshot — never re-read mid-turn.
            Some("AgentStartedSpeaking") => {
                if turn_gen.is_none() {
                    turn_gen = Some(generation.current());
                }
            }
            // The turn's end delimiter: retire the tag. The next response
            // snapshots afresh, so a barge-in bump between turns takes effect
            // exactly at the turn boundary.
            Some("AgentAudioDone") => turn_gen = None,
            // `AgentThinking` is a response-progress marker; nothing
            // downstream consumes it.
            Some("AgentThinking") => continue,
            // The function-call bridge (doc comment above). `arguments`
            // arrives as a JSON *string* ("JSON string containing the
            // function arguments" — AsyncAPI reference, accessed 2026-08-21)
            // and is parsed into the uniform `args` value; an unparseable
            // string degrades to `{}` so the call still surfaces answerable
            // (the harness's schema validation owns rejecting bad args, and a
            // skipped call would strand the id server-side forever). Entries
            // missing `id` or `name` ARE skipped: unanswerable calls must not
            // surface.
            Some("FunctionCallRequest") => {
                let Some(calls) = value.get("functions").and_then(|calls| calls.as_array()) else {
                    continue;
                };
                for call in calls {
                    if call.get("client_side").and_then(|flag| flag.as_bool()) != Some(true) {
                        // Server-side: Deepgram executes it itself (module
                        // header) — not the harness's call to answer.
                        continue;
                    }
                    let (Some(id), Some(name)) = (
                        call.get("id").and_then(|id| id.as_str()),
                        call.get("name").and_then(|name| name.as_str()),
                    ) else {
                        continue;
                    };
                    let args = call
                        .get("arguments")
                        .and_then(|raw| raw.as_str())
                        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                        .unwrap_or_else(|| serde_json::json!({}));
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
            }
            // A server→client `FunctionCallResponse` narrates a server-side
            // function Deepgram executed itself (the frame shape is
            // bidirectional — module header): nothing to bridge, nothing
            // pending.
            Some("FunctionCallResponse") => continue,
            // Update acks, diagnostics, and history — decoded and ignored:
            // nothing downstream consumes them. `PromptUpdated` is the ack of
            // `update_context`'s fire-and-forget `UpdatePrompt` (the method
            // doc rules on not awaiting it).
            Some("ListenUpdated")
            | Some("ThinkUpdated")
            | Some("SpeakUpdated")
            | Some("PromptUpdated")
            | Some("InjectionRefused")
            | Some("LatencyReport")
            | Some("History")
            | Some("Warning") => continue,
            // The event surface must tolerate vocabulary growth (module
            // header discipline, matching gemini.rs/deepgram.rs).
            _ => continue,
        }
    }
}

#[async_trait]
impl SessionChannel for DeepgramAgentChannel {
    async fn connect(&mut self, ctx: SessionContext) -> Result<(), ChannelError> {
        if self.state != SessionState::New {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("connect on a channel in state {:?}", self.state),
            ));
        }

        // The account key, by its canonical identity: one Deepgram key drives
        // both hosts, so the lookup is under `api.deepgram.com` even though
        // the socket dials `agent.deepgram.com` (the dgva-1a split, see
        // [`DEEPGRAM_AGENT_KEY_BASE_URL`]).
        let key = self
            .keys
            .session_key(DEEPGRAM_AGENT_KEY_BASE_URL)
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Auth,
                    false,
                    format!("no usable Deepgram key: {err}"),
                )
            })?;
        // The BYO think key, resolved fresh at connect under the endpoint's
        // own base URL (per-base-URL key isolation, VOICE_AGENT §3.2) — never
        // stored in config; its only destination is the Settings message's
        // `think.endpoint.headers` (the double-disclosure path, module
        // header).
        let byo_key = match self.config.think.byo_endpoint_url.as_deref() {
            Some(url) => Some(self.keys.session_key(url).await.map_err(|err| {
                ChannelError::new(
                    ErrorKind::Auth,
                    false,
                    format!("no usable key for the BYO think endpoint: {err}"),
                )
            })?),
            None => None,
        };
        // The think-prompt parts persist for `update_context`'s re-renders;
        // the Settings first message and every later `UpdatePrompt` share the
        // one composition rule ([`ThinkPrompt::render`]).
        let think_prompt = ThinkPrompt::new(&ctx);
        let settings = self.settings_message(&think_prompt.render(), byo_key.as_ref());

        let mut request = self.endpoint().into_client_request().map_err(|err| {
            ChannelError::new(ErrorKind::Protocol, false, format!("bad endpoint: {err}"))
        })?;
        // Header auth only, the account-wide `Authorization: Token <key>`
        // scheme (module header, auth provenance): the connect URL stays
        // credential-free (VOICE_AGENT §3.2
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

        let (mut socket, _response) = connect_async(request).await.map_err(|err| {
            ChannelError::new(
                ErrorKind::Transport,
                true,
                format!("agent ws connect failed: {err}"),
            )
        })?;

        // The Settings first-message handshake (module header, Settings
        // provenance): Settings goes out first, then the acks are awaited —
        // `Welcome` (queued by the server on connect) then `SettingsApplied`,
        // the ack that is this channel's ready signal.
        socket.send(Message::text(settings)).await.map_err(|err| {
            ChannelError::new(
                ErrorKind::Transport,
                true,
                format!("agent ws dropped sending Settings: {err}"),
            )
        })?;
        Self::await_settings_applied(&mut socket).await?;

        let tx = self
            .events_tx
            .clone()
            .expect("a New channel still holds its construction-time sender");
        let _ = tx.try_send(ChannelEvent::Lifecycle(LifecycleEvent::Connected));
        let (writer, reader) = socket.split();
        self.reader = Some(tokio::spawn(read_loop(
            reader,
            tx,
            self.generation.clone(),
            Arc::clone(&self.pending_tool_calls),
        )));
        *self.writer.lock().await = Some(writer);
        self.think_prompt = Some(think_prompt);
        // The idle window opens with the session (the Settings frame is the
        // last thing this side sent), so the mark and the ticker start now —
        // not at the first audio frame.
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
        self.events_tx = None;
        // The ticker dies before the writer leaves the slot so no KeepAlive
        // can race onto a closing wire.
        if let Some(keepalive) = self.keepalive.take() {
            keepalive.abort();
        }
        // The doc's shutdown guidance, checked: the client vocabulary has no
        // CloseStream equivalent (module header) — so a plain WS close
        // handshake is the whole shutdown. Best-effort: the sink close sends
        // the close frame; the read loop drains until the peer acknowledges
        // and exits. A peer that already vanished is not a close failure.
        if let Some(mut writer) = self.writer.lock().await.take() {
            let _ = writer.close().await;
        }
        if let Some(mut reader) = self.reader.take() {
            if timeout(HANDSHAKE_BUDGET, &mut reader).await.is_err() {
                reader.abort();
            }
        }
        Ok(())
    }

    async fn send_audio(&mut self, pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
        // The whole gate, same as deepgram.rs: New and Closed reject with the
        // trait's InvalidState (MockChannel's send-on-closed semantics), and
        // nothing is ever retryable-buffered. Checked against the agent
        // wire's 2-hour session cap: the doc defines no expiry warning or
        // renewal signal before that close ([`KEEPALIVE_IDLE_INTERVAL`]
        // provenance), so there is no Gemini-style phase between Connected
        // and gone to gate on — the cap lands as a server close, the
        // session-lifecycle stride's business.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_audio on a channel in state {:?}", self.state),
            ));
        }
        // The hot path (module header, framing provenance): one raw binary WS
        // frame per chunk — `Settings.audio.input` pinned `linear16` @ 16 kHz,
        // i.e. little-endian signed PCM16 bytes, no envelope; control frames
        // are text, and the two kinds never mix on a Deepgram wire.
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
                    format!("agent ws dropped sending audio: {err}"),
                )
            })?;
        *self.last_send.lock().unwrap() = tokio::time::Instant::now();
        Ok(())
    }

    async fn send_text(&mut self, text: &str) -> Result<(), ChannelError> {
        // Unlike the STT-only listen wire (which rejects), this wire HAS a
        // text turn — `InjectUserMessage` (module header, client vocabulary)
        // — so the §3 no-mic fallback works natively here. The state gate is
        // the whole gate: this wire has no renewal phase to gate on
        // ([`KEEPALIVE_IDLE_INTERVAL`] provenance — no Gemini-style
        // renewing window exists between Connected and gone).
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_text on a channel in state {:?}", self.state),
            ));
        }
        let mut writer = self.writer.lock().await;
        writer
            .as_mut()
            .expect("Connected implies a live write half")
            .send(Message::text(inject_user_message(text)))
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("agent ws dropped sending text: {err}"),
                )
            })?;
        *self.last_send.lock().unwrap() = tokio::time::Instant::now();
        Ok(())
    }

    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: ToolResult,
    ) -> Result<(), ChannelError> {
        // The state gate first (the gemini.rs ordering, minus the renewal
        // phase this wire does not have), then the pending-call gate: only a
        // call the read loop bridged — a client-side `FunctionCallRequest`
        // entry — can be answered. An unknown, already-answered, or
        // server-side id is `InvalidState`, non-retryable: unlike Gemini
        // there is no cancellation frame in this vocabulary, so no
        // late-result-after-retraction case exists to soften it.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("send_tool_result on a channel in state {:?}", self.state),
            ));
        }
        let pending_name = self
            .pending_tool_calls
            .lock()
            .unwrap()
            .get(call_id)
            .cloned();
        let Some(name) = pending_name else {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!(
                    "tool result for unknown, already-answered, or server-side call {call_id:?}"
                ),
            ));
        };
        let mut writer = self.writer.lock().await;
        writer
            .as_mut()
            .expect("Connected implies a live write half")
            .send(Message::text(function_call_response_message(
                call_id, &name, &result,
            )))
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("agent ws dropped sending tool response: {err}"),
                )
            })?;
        // Answered exactly once: only a delivered response retires the call —
        // a transport failure above leaves it pending for the retry (the
        // gemini.rs rule).
        self.pending_tool_calls.lock().unwrap().remove(call_id);
        *self.last_send.lock().unwrap() = tokio::time::Instant::now();
        Ok(())
    }

    async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError> {
        // Chapter-boundary refresh and playback position both map to
        // `UpdatePrompt` — the only mid-session context mechanism in the
        // client vocabulary (module header) — which REPLACES the Settings
        // `think.prompt`, so the stored [`ThinkPrompt`] parts are updated
        // first and the complete prompt re-rendered (the same composition
        // rule Settings used; the delimited excerpt travels verbatim,
        // VOICE_AGENT §4). Fire-and-forget, mirroring gemini.rs's
        // update_context and the trait contract (which requires the update to
        // reach the provider, not an ack round-trip): the server's
        // `PromptUpdated` ack is decoded and ignored by the read loop —
        // awaiting it here would serialize the control plane on provider
        // latency for no consumer.
        if self.state != SessionState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("update_context on a channel in state {:?}", self.state),
            ));
        }
        let think_prompt = self
            .think_prompt
            .as_mut()
            .expect("Connected implies the stored think prompt");
        think_prompt.apply(&update);
        let prompt = think_prompt.render();
        let mut writer = self.writer.lock().await;
        writer
            .as_mut()
            .expect("Connected implies a live write half")
            .send(Message::text(update_prompt_message(&prompt)))
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("agent ws dropped sending context update: {err}"),
                )
            })?;
        *self.last_send.lock().unwrap() = tokio::time::Instant::now();
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
        // The dgva-1a flag, resolved (module header, barge-in provenance):
        // the client vocabulary has NO cancel frame — the AsyncAPI reference
        // defines exactly Settings, UpdateListen/UpdateThink/UpdateSpeak/
        // UpdatePrompt, InjectUserMessage, InjectAgentMessage,
        // FunctionCallResponse, KeepAlive plus binary audio, and none of them
        // cancels. `InjectAgentMessage` behavior "interrupt" is real but it
        // is a SPEAK verb ("The agent immediately speaks" the injected
        // `message`, replacing current speech) — sending it here would make
        // the agent say something, not go quiet, so it is not the cancel. So,
        // exactly like gemini.rs: the network half is empty and the
        // generation bump IS the cancel — every remaining delta of the
        // superseded turn carries the dead gen and dies at the playback gate.
        // Through [`super::barge_in`] this is a second bump, which is
        // harmless — the gate is a staleness comparison, not a step count.
        self.generation.bump();
        Ok(())
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
        self.events.take()
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            // The agent wire owns turn detection server-side: it emits
            // `UserStartedSpeaking` and takes end-of-turn tuning only as
            // listen-slot Settings (Flux `eot_threshold`/`eot_timeout_ms` —
            // module header, Settings provenance); no client turn signal
            // exists in the vocabulary. VOICE_AGENT §3.1 roster: Server —
            // client VAD is at most a cost optimizer (§3.1 double-VAD rule).
            vad_mode: VadMode::Server,
            // No word-aligned timing anywhere on the agent wire:
            // `ConversationText` carries only role/content (module header,
            // server vocabulary — accessed 2026-08-21), unlike the listen
            // wire's `words[]`. VOICE_AGENT §3.1 roster: ❌ → the §11
            // sentence-level follow-along degrade applies to this channel.
            word_timestamps: false,
            // The agent wire has no end-of-session announcement: the 2h cap
            // ends with a plain close (module header, accessed 2026-08-21),
            // so nothing on this wire can surface `Renewing`.
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
    use crate::voice::channel::fixture::{load_fixture, replay, WireFrame, WireServer};
    use crate::voice::channel::DelimitedExcerpt;
    use crate::voice::keys::store::{Backend, KeyStore};

    /// Deliberately echoed in the fixture's `absent` needles: the account key
    /// rides the upgrade header only — never the connect URL, never any frame
    /// (the Settings message carries the BYO key, not this one).
    const FIXTURE_KEY: &str = "fixture-deepgram-agent-key-5b12";
    /// The BYO think endpoint and its key. The key's REQUIRED destination is
    /// `think.endpoint.headers` inside the Settings frame — the §3.2 routing
    /// rule's double-disclosure path (the LLM key transits Deepgram) — and
    /// its forbidden one is the connect URL (fixture `target_absent`).
    const FIXTURE_BYO_ENDPOINT: &str = "https://byo-llm.example.com/v1";
    const FIXTURE_BYO_KEY: &str = "fixture-byo-llm-key-3e77";

    /// The exact Settings first message the fixture session must open with
    /// (serde_json serializes map keys in alphabetical order, so the
    /// rendering is deterministic), byte-for-byte: audio pinned to the
    /// 16 kHz-in / 24 kHz-out seams, the three provider slots from config,
    /// ctx system prompt + delimited excerpt as think.prompt, and the BYO
    /// endpoint with the resolved key in its headers.
    const SETTINGS_FRAME: &str = "{\"agent\":{\"listen\":{\"provider\":{\"model\":\"nova-3\",\"type\":\"deepgram\",\"version\":\"v1\"}},\"speak\":{\"provider\":{\"model\":\"aura-2-thalia-en\",\"type\":\"deepgram\"}},\"think\":{\"endpoint\":{\"headers\":{\"authorization\":\"Bearer fixture-byo-llm-key-3e77\"},\"url\":\"https://byo-llm.example.com/v1\"},\"prompt\":\"You are the reading tutor.\\n<<x>>excerpt<</x>>\",\"provider\":{\"model\":\"kimi-k2\",\"type\":\"open_ai\"}}},\"audio\":{\"input\":{\"encoding\":\"linear16\",\"sample_rate\":16000},\"output\":{\"encoding\":\"linear16\",\"sample_rate\":24000}},\"type\":\"Settings\"}";

    fn keys() -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        Arc::new(KeyManager::with_store(store))
    }

    fn keys_with_agent_and_byo_keys() -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(
                DEEPGRAM_AGENT_KEY_BASE_URL,
                &crate::voice::keys::ApiKey::new(FIXTURE_KEY.into()),
            )
            .unwrap();
        store
            .store(
                FIXTURE_BYO_ENDPOINT,
                &crate::voice::keys::ApiKey::new(FIXTURE_BYO_KEY.into()),
            )
            .unwrap();
        Arc::new(KeyManager::with_store(store))
    }

    fn byok_config(endpoint_override: Option<String>) -> DeepgramAgentConfig {
        DeepgramAgentConfig {
            endpoint_override,
            listen_model: "nova-3".into(),
            speak_model: "aura-2-thalia-en".into(),
            think: DeepgramAgentThinkConfig {
                provider_type: "open_ai".into(),
                model: "kimi-k2".into(),
                byo_endpoint_url: Some(FIXTURE_BYO_ENDPOINT.into()),
            },
        }
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
    async fn deepgram_agent_connect_handshake_fixture_replays_green() {
        let handle = WireServer::start().await;
        let mut channel = DeepgramAgentChannel::new(
            byok_config(Some(handle.url("/v1/agent/converse"))),
            keys_with_agent_and_byo_keys(),
            Generation::new(),
        );

        let fixture = load_fixture("deepgram-agent-connect");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        replayer.await.expect("the recorded fixture replays green");

        // The account key rides the upgrade header in the account-wide Token
        // scheme (module header, auth provenance), resolved under the
        // api.deepgram.com identity even though the socket dialed the agent
        // host (the dgva-1a split).
        let record = handle.wait_for_connection(1).await;
        let expected_auth = format!("Token {FIXTURE_KEY}");
        assert_eq!(
            record.header("authorization"),
            Some(expected_auth.as_str()),
            "the Deepgram key rides the Authorization header, Token scheme"
        );

        // The fixture pinned the protocol needles; this pins the whole first
        // message byte-for-byte: the exact Settings JSON, BYO endpoint and
        // its key in think.endpoint (the §3.2 double-disclosure path)
        // included.
        let by_connection = handle.received_by_connection();
        assert_eq!(
            by_connection[0].first(),
            Some(&WireFrame::Text(SETTINGS_FRAME.to_string())),
            "the session opens with exactly the pinned Settings first message"
        );

        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "Connected is reported only after Welcome then SettingsApplied — \
             the ack IS the ready signal, mirroring Gemini's setupComplete"
        );
        assert!(
            channel.take_events().is_none(),
            "take_events yields the receiver exactly once"
        );

        channel.close().await.expect("clean close");
        channel
            .close()
            .await
            .expect("closing a closed channel is a no-op");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned: \
             Settings, then nothing — no client close frame exists in the \
             vocabulary, so the shutdown is the WS close handshake alone"
        );
    }

    /// The audio-in hot path plus the agent wire's idle KeepAlive, one
    /// recorded script (the dg-1d test shape on the agent wire): the mic
    /// chunk must reach the wire as exactly its little-endian PCM16 bytes in
    /// one raw binary frame (payload-pinned — no JSON wrapper, no base64),
    /// once the wire idles past the keepalive threshold the
    /// `{"type":"KeepAlive"}` text frame must follow, and send_audio outside
    /// Connected — before connect and after close — is the trait's
    /// non-retryable InvalidState.
    #[tokio::test]
    async fn deepgram_agent_audio_rides_raw_binary_frames_and_idle_sends_keepalive() {
        let handle = WireServer::start().await;
        let mut channel = DeepgramAgentChannel::new(
            byok_config(Some(handle.url("/v1/agent/converse"))),
            keys_with_agent_and_byo_keys(),
            Generation::new(),
        );
        // Real-time test: 200 ms crosses the idle threshold quickly while
        // staying far above scheduler jitter (the production 8 s would sail
        // past the fixture step budget).
        channel.set_keepalive_interval(Duration::from_millis(200));

        // The fixture's pinned payload: these samples, little-endian —
        // e8 03 | 18 fc | 01 01 | fe ff | 00 00 | ff 7f | 00 80 — including
        // both i16 extremes so a byte-order slip cannot pass.
        let mic_chunk: [i16; 7] = [1000, -1000, 257, -2, 0, i16::MAX, i16::MIN];

        let premature = channel.send_audio(&mic_chunk).await.unwrap_err();
        assert_eq!(
            (premature.kind, premature.retryable),
            (ErrorKind::InvalidState, false),
            "send-on-New is InvalidState: the Connected gate holds before \
             connect, not just after close"
        );

        let fixture = load_fixture("deepgram-agent-audio-keepalive");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
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
            "the session sent exactly the pinned frames: Settings, audio, \
             KeepAlive, nothing else — no client close frame exists in the \
             vocabulary, so the shutdown is the WS close handshake alone"
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

    /// The recorded turn's TTS payloads (base64 in the fixture file), decoded
    /// sample-for-sample: the first burst includes both i16 extremes so a
    /// byte-order slip cannot pass.
    const TTS_TURN_PART_1: [i16; 5] = [1111, -1111, i16::MAX, i16::MIN, 3333];
    const TTS_TURN_PART_2: [i16; 3] = [4444, -4444, 5555];
    /// The orphan burst: binary audio after `AgentAudioDone` with no new
    /// `AgentStartedSpeaking` — the no-open-turn case.
    const TTS_ORPHAN: [i16; 4] = [7777, -7777, 257, -2];
    /// Mic chunks whose only job is sequencing: the fixture's expect steps
    /// gate each server burst on one of these, so the test's generation bumps
    /// land at provable points in the stream.
    const MIC_SYNC_A: [i16; 2] = [10, -10];
    const MIC_SYNC_B: [i16; 2] = [20, -20];
    const MIC_SYNC_C: [i16; 2] = [30, -30];

    /// Server→client audio + transcripts (the dgva-1d stride), one recorded
    /// script: TTS binary frames become `AudioDelta`s **tagged by response
    /// turn, not read time** (the gemini.rs turn-scoping lesson, m3-fix4) —
    /// the snapshot is taken once at `AgentStartedSpeaking` (here after two
    /// barge-ins, so gen 2), a mid-turn bump leaves the open turn's tag
    /// STALE (part 2 still gen 2 while the live counter is 3), and
    /// `AgentAudioDone` retires it, so the orphan burst that follows with no
    /// new `AgentStartedSpeaking` opens an implicit turn tagged with the
    /// CURRENT generation (3 — the documented tag-with-current choice at the
    /// decode site: the wire guarantees no JSON/binary ordering, so dropping
    /// would eat the head of agent speech). `ConversationText` streams both
    /// roles as plain `TranscriptDelta`s in arrival order.
    #[tokio::test]
    async fn deepgram_agent_tts_audio_is_turn_scoped_and_transcripts_stream() {
        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = DeepgramAgentChannel::new(
            byok_config(Some(handle.url("/v1/agent/converse"))),
            keys_with_agent_and_byo_keys(),
            generation.clone(),
        );

        let fixture = load_fixture("deepgram-agent-audio-out-transcripts");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        // Two prior barge-ins left the counter at 2 before the turn opens:
        // the turn's deltas must carry that snapshot, not a constant. The
        // fixture holds AgentStartedSpeaking until MIC_SYNC_A arrives, so
        // these bumps provably precede the snapshot.
        generation.bump();
        generation.bump();
        channel.send_audio(&MIC_SYNC_A).await.expect("sync A");

        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: TTS_TURN_PART_1.to_vec(),
                gen: 2,
            }),
            "the raw binary TTS frame decodes as little-endian PCM16, \
             byte-for-byte, tagged with the generation snapshotted at \
             AgentStartedSpeaking"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::User,
                text: "wait — which figure?".into(),
            }),
            "ConversationText role user maps to Role::User, content intact"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::UtteranceEnd),
            "ConversationText role user commits the utterance with UtteranceEnd"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "Figure 3, on this page.".into(),
            }),
            "ConversationText role assistant maps to Role::Assistant"
        );

        // Mid-turn barge-in: the bump lands after the turn's snapshot
        // (guaranteed — the deltas above prove the snapshot is taken) and
        // before part 2 (guaranteed — the fixture holds part 2 until
        // MIC_SYNC_B arrives, sent only now).
        generation.bump();
        channel.send_audio(&MIC_SYNC_B).await.expect("sync B");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: TTS_TURN_PART_2.to_vec(),
                gen: 2,
            }),
            "a turn superseded mid-flight keeps its STALE tag — the playback \
             gate drops it; the tag is never re-read from the live counter"
        );

        // The orphan burst rides after AgentAudioDone with no new
        // AgentStartedSpeaking (MIC_SYNC_C only sequences it after the
        // retire): implicit turn, tagged with the current generation.
        channel.send_audio(&MIC_SYNC_C).await.expect("sync C");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: TTS_ORPHAN.to_vec(),
                gen: 3,
            }),
            "AgentAudioDone retired the turn's tag, so orphan audio opens an \
             implicit turn tagged with the CURRENT generation (the documented \
             tag-with-current choice at the decode site), not the dead 2"
        );

        replayer.await.expect("the recorded fixture replays green");
        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned: \
             Settings and the three sync mic chunks"
        );
    }

    /// The barged-in turn's first burst (base64 in the fixture file), with
    /// both i16 extremes so a byte-order slip cannot pass.
    const BARGE_PART_1: [i16; 4] = [2222, -2222, i16::MAX, i16::MIN];
    /// The in-flight remnant: binary audio the server put on the wire AFTER
    /// `UserStartedSpeaking` — the frames the doc mandates the client discard.
    const BARGE_PART_2: [i16; 2] = [6666, -6666];
    /// The next response's audio, after `AgentAudioDone` retired the cut-off
    /// turn and a fresh `AgentStartedSpeaking` opened this one.
    const BARGE_PART_3: [i16; 3] = [8888, -8888, 123];

    /// Barge-in end to end (the dgva-1e stride, one recorded script): the
    /// dgva-1a ruling is that this wire has NO client cancel frame
    /// (`InjectAgentMessage` behavior "interrupt" is a speak verb, not a
    /// cancel — module header, barge-in provenance), so both barge-in entry
    /// points are pure generation bumps. `UserStartedSpeaking` — the only
    /// place user speech is detectable on a Server-VAD channel — bumps at
    /// decode: the open turn keeps its now-stale snapshot, so the remnant
    /// audio the server sends after it arrives STALE-tagged (gen 0 while the
    /// live counter is 1 — the playback gate drops it, the doc's mandated
    /// discard), and the next `AgentStartedSpeaking` turn snapshots the
    /// bumped generation and plays. `interrupt()` is the same bump behind
    /// the trait's state gate (InvalidState non-retryable off Connected,
    /// MockChannel/gemini.rs semantics) and puts NOTHING on the wire.
    #[tokio::test]
    async fn deepgram_agent_barge_in_stales_the_open_turn_and_the_next_turn_plays() {
        let handle = WireServer::start().await;
        let generation = Generation::new();
        let mut channel = DeepgramAgentChannel::new(
            byok_config(Some(handle.url("/v1/agent/converse"))),
            keys_with_agent_and_byo_keys(),
            generation.clone(),
        );

        let premature = channel.interrupt().await.unwrap_err();
        assert_eq!(
            (premature.kind, premature.retryable),
            (ErrorKind::InvalidState, false),
            "interrupt-on-New is the trait's non-retryable InvalidState \
             (MockChannel send_gate semantics)"
        );
        assert_eq!(
            generation.current(),
            0,
            "a state-rejected interrupt must not bump"
        );

        let fixture = load_fixture("deepgram-agent-barge-in");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        channel.connect(ctx()).await.expect("connect");
        let mut events = channel.take_events().expect("the receiver is handed over");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        // Turn open + audio flowing: the fixture holds AgentStartedSpeaking
        // and the first burst until MIC_SYNC_A arrives.
        channel.send_audio(&MIC_SYNC_A).await.expect("sync A");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: BARGE_PART_1.to_vec(),
                gen: 0,
            }),
            "the turn opened on the pre-barge-in generation"
        );

        // The barge-in: the fixture holds UserStartedSpeaking and the
        // remnant burst until MIC_SYNC_B arrives, so the bump provably lands
        // between the turn's snapshot and the remnant's decode.
        channel.send_audio(&MIC_SYNC_B).await.expect("sync B");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: BARGE_PART_2.to_vec(),
                gen: 0,
            }),
            "the in-flight remnant keeps the turn's snapshot — the tag is \
             never re-read from the live counter"
        );
        assert_eq!(
            generation.current(),
            1,
            "UserStartedSpeaking bumped the live generation at decode (the \
             read loop processed it before the remnant), so the remnant's \
             tag (0) is STALE and the playback gate drops it — the doc's \
             mandated stop-playback-and-discard"
        );

        // The next response: AgentAudioDone retires the cut-off turn, the
        // fresh AgentStartedSpeaking snapshots the bumped generation.
        channel.send_audio(&MIC_SYNC_C).await.expect("sync C");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: BARGE_PART_3.to_vec(),
                gen: 1,
            }),
            "the next AgentStartedSpeaking turn tags with the bumped \
             generation — live at the playback gate, so it plays"
        );

        replayer.await.expect("the recorded fixture replays green");

        // The harness-driven half: interrupt() while Connected is the same
        // bump — and nothing goes on the wire (the final exactly-the-pinned-
        // frames assertion is the proof).
        channel
            .interrupt()
            .await
            .expect("interrupt is valid while connected");
        assert_eq!(
            generation.current(),
            2,
            "interrupt bumps the shared barge-in generation"
        );

        channel.close().await.expect("clean close");
        let after_close = channel.interrupt().await.unwrap_err();
        assert_eq!(
            (after_close.kind, after_close.retryable),
            (ErrorKind::InvalidState, false),
            "interrupt-on-Closed is InvalidState: a closed session is not \
             coming back"
        );
        assert_eq!(generation.current(), 2, "no bump after close");

        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned — \
             Settings and the three sync mic chunks: neither interrupt() nor \
             the UserStartedSpeaking decode put ANYTHING on the wire, because \
             no cancel frame exists in the client vocabulary (dgva-1a ruling)"
        );
    }

    /// The four LLM-facing verbs (the dgva-1f stride — the last unimplemented
    /// methods), one recorded script: a client-side `FunctionCallRequest`
    /// entry bridges
    /// to `ToolCall` (its JSON-string `arguments` parsed into the uniform
    /// args) while the server-side entry in the SAME frame is never bridged;
    /// `send_tool_result` answers with the pinned `FunctionCallResponse`
    /// (id/name matched, `content` a JSON string under the gemini envelope
    /// rule) and unknown / server-side / already-answered ids reject
    /// `InvalidState` without a frame; `send_text` is the pinned
    /// `InjectUserMessage` (§3 no-mic fallback); both `ContextUpdate`
    /// variants are pinned `UpdatePrompt` full-prompt replacements rendered
    /// by the one composition rule Settings used (old excerpt gone, chapter
    /// then position lines appended), fire-and-forget — the served
    /// `PromptUpdated` ack is decoded and ignored, nothing blocks on it.
    #[tokio::test]
    async fn deepgram_agent_tool_bridge_text_turn_and_prompt_refresh_ride_the_documented_frames() {
        let handle = WireServer::start().await;
        let mut channel = DeepgramAgentChannel::new(
            byok_config(Some(handle.url("/v1/agent/converse"))),
            keys_with_agent_and_byo_keys(),
            Generation::new(),
        );

        // The trait's uniform pre-connect gate holds for all three verbs.
        for premature in [
            channel.send_text("early").await.unwrap_err(),
            channel
                .send_tool_result("fc-call-1", ToolResult::Ok(serde_json::json!({})))
                .await
                .unwrap_err(),
            channel
                .update_context(ContextUpdate::PlaybackPosition { pos_ms: 1 })
                .await
                .unwrap_err(),
        ] {
            assert_eq!(
                (premature.kind, premature.retryable),
                (ErrorKind::InvalidState, false),
                "LLM-facing verbs on a New channel are the trait's \
                 non-retryable InvalidState"
            );
        }

        let fixture = load_fixture("deepgram-agent-tools-text-context");
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
            Some(ChannelEvent::ToolCall {
                call_id: "fc-call-1".into(),
                name: "get_context_around_position".into(),
                args: serde_json::json!({ "position_ms": 42000 }),
            }),
            "the client_side:true FunctionCallRequest entry bridges to \
             ToolCall, its JSON-string arguments parsed into the uniform args"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "marker: nothing else was bridged".into(),
            }),
            "the client_side:false entry is Deepgram's own to execute \
             server-side — the next event is the fixture's marker transcript, \
             not a second ToolCall"
        );

        // Negative gates: a server-side id was never pending and an invented
        // id never existed — both reject non-retryably, and neither puts a
        // frame on the wire (the final pinned-frames assertion is the proof).
        for bogus in ["fc-srv-1", "fc-made-up"] {
            let rejected = channel
                .send_tool_result(bogus, ToolResult::Ok(serde_json::json!({})))
                .await
                .unwrap_err();
            assert_eq!(
                (rejected.kind, rejected.retryable),
                (ErrorKind::InvalidState, false),
                "call id {bogus:?} (server-side / unknown) is rejected \
                 without a frame"
            );
        }

        channel
            .send_tool_result(
                "fc-call-1",
                ToolResult::Ok(serde_json::json!({ "excerpt": "figure three is a phase diagram" })),
            )
            .await
            .expect("the bridged call is answerable");
        let retired = channel
            .send_tool_result("fc-call-1", ToolResult::Ok(serde_json::json!({})))
            .await
            .unwrap_err();
        assert_eq!(
            (retired.kind, retired.retryable),
            (ErrorKind::InvalidState, false),
            "a delivered response retires the call: answering twice is \
             InvalidState (the gemini.rs answered-exactly-once rule)"
        );

        channel
            .send_text("what does the third equation mean?")
            .await
            .expect("the §3 no-mic text turn sends");

        channel
            .update_context(ContextUpdate::ChapterBoundary {
                chapter_idx: 7,
                excerpt: DelimitedExcerpt {
                    delimited_text: "<<x>>chapter seven excerpt<</x>>".into(),
                },
            })
            .await
            .expect("chapter-boundary refresh sends");
        channel
            .update_context(ContextUpdate::PlaybackPosition { pos_ms: 90500 })
            .await
            .expect("playback-position refresh sends");

        replayer.await.expect("the recorded fixture replays green");
        channel.close().await.expect("clean close");
        assert!(
            handle.try_next_from_adapter().await.is_none(),
            "the whole session sent exactly the frames the fixture pinned: \
             the rejected tool results put NOTHING on the wire"
        );

        // Byte-for-byte pins on everything after the Settings first message
        // (whose exact rendering the connect test already pins): serde_json's
        // alphabetical key order makes each rendering deterministic.
        let frames = handle.received_by_connection()[0].clone();
        assert_eq!(
            frames.len(),
            5,
            "Settings plus exactly the four pinned verb frames"
        );
        let expected = [
            "{\"content\":\"{\\\"excerpt\\\":\\\"figure three is a phase diagram\\\"}\",\"id\":\"fc-call-1\",\"name\":\"get_context_around_position\",\"type\":\"FunctionCallResponse\"}",
            "{\"content\":\"what does the third equation mean?\",\"type\":\"InjectUserMessage\"}",
            "{\"prompt\":\"You are the reading tutor.\\n<<x>>chapter seven excerpt<</x>>\\nThe reader is now at chapter 7.\",\"type\":\"UpdatePrompt\"}",
            "{\"prompt\":\"You are the reading tutor.\\n<<x>>chapter seven excerpt<</x>>\\nThe reader is now at chapter 7.\\nPlayback position: 90500 ms.\",\"type\":\"UpdatePrompt\"}",
        ];
        for (frame, want) in frames[1..].iter().zip(expected) {
            assert_eq!(
                frame,
                &WireFrame::Text(want.to_string()),
                "each LLM-facing verb renders its documented frame \
                 byte-for-byte"
            );
        }
    }

    #[test]
    fn capabilities_declare_server_vad_and_no_word_timestamps() {
        let channel = DeepgramAgentChannel::new(
            DeepgramAgentConfig {
                endpoint_override: None,
                listen_model: "nova-3".into(),
                speak_model: "aura-2-thalia-en".into(),
                think: DeepgramAgentThinkConfig {
                    provider_type: "open_ai".into(),
                    model: "gpt-4.1".into(),
                    byo_endpoint_url: None,
                },
            },
            keys(),
            Generation::new(),
        );
        assert_eq!(
            channel.capabilities(),
            ChannelCapabilities {
                vad_mode: VadMode::Server,
                word_timestamps: false,
                session_renewal: false,
                text_only: false,
                stt_only: false,
            },
            "the agent wire owns turns server-side (UserStartedSpeaking, \
             VOICE_AGENT §3.1 roster: Server), ConversationText carries \
             no word timing (roster: ❌ → §11 sentence-level degrade), and \
             the 2h cap ends with a plain close (no session_renewal)"
        );
    }

    /// The VA2 conformance suite against a fixture-backed
    /// [`DeepgramAgentChannel`] (the dgva-1g gate): the same
    /// [`crate::voice::channel::conformance`] checks the MockChannel passes,
    /// driven over a real socket — the gemini.rs `conformance_rig` shape, on
    /// the agent wire. The rig yields a fresh channel plus a [`ProviderSim`]
    /// that plays the provider by scripting wire frames into the loopback
    /// [`WireServer`] and observing what the adapter's socket carried. The
    /// per-connection session opening (handshake hygiene, Settings, Welcome →
    /// SettingsApplied) is the recorded `deepgram-agent-conformance-session`
    /// fixture, replayed verbatim — once per rig, because this wire declares
    /// `session_renewal: false` (the 2h cap ends in a plain close, no
    /// renewal signal exists in the vocabulary), so `run_all` runs exactly
    /// the core checks and never dials a second connection.
    mod conformance_rig {
        use std::sync::Mutex;

        use super::*;
        use crate::voice::channel::conformance::{run_all, ConformanceFixture, ProviderSim};
        use crate::voice::channel::fixture::{load_fixture, replay, WireHandle, WireServer};

        fn frame_json(frame: &WireFrame) -> Option<serde_json::Value> {
            let WireFrame::Text(text) = frame else {
                return None;
            };
            serde_json::from_str(text).ok()
        }

        /// Binary in IS user audio on this wire (module header, framing
        /// provenance): raw little-endian PCM16, no envelope — so every
        /// binary frame the socket carried decodes as one accepted mic chunk.
        fn decode_audio(frame: &WireFrame) -> Option<Vec<i16>> {
            let WireFrame::Binary(bytes) = frame else {
                return None;
            };
            Some(
                bytes
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect(),
            )
        }

        /// The text turn as the wire carried it: an `InjectUserMessage`
        /// frame's `content` (the §3 no-mic fallback, check 8's proof).
        fn inject_user_text(frame: &WireFrame) -> Option<String> {
            let value = frame_json(frame)?;
            if value.get("type")?.as_str()? != "InjectUserMessage" {
                return None;
            }
            Some(value.get("content")?.as_str()?.to_string())
        }

        /// A rendered `think.prompt` split back into its [`ThinkPrompt`]
        /// parts: (excerpt, chapter_idx, playback_pos_ms). Reverses
        /// [`ThinkPrompt::render`]'s composition — first line the system
        /// prompt, then the excerpt, then the optional state lines in render
        /// order (position last, so it strips first).
        fn prompt_parts(prompt: &str) -> Option<(String, Option<u32>, Option<u64>)> {
            let (_system, rest) = prompt.split_once('\n')?;
            let mut rest = rest;
            let mut pos_ms = None;
            if let Some((head, tail)) = rest.rsplit_once("\nPlayback position: ") {
                pos_ms = Some(tail.strip_suffix(" ms.")?.parse().ok()?);
                rest = head;
            }
            let mut chapter_idx = None;
            if let Some((head, tail)) = rest.rsplit_once("\nThe reader is now at chapter ") {
                chapter_idx = Some(tail.strip_suffix('.')?.parse().ok()?);
                rest = head;
            }
            Some((rest.to_string(), chapter_idx, pos_ms))
        }

        /// Reverses [`function_call_response_message`]'s envelope rule: the
        /// wire `content` is a JSON string; `{"error": {code, message}}`
        /// decodes back to `ToolResult::Err`, anything else to the `Ok`
        /// payload as the wire carried it.
        fn decode_tool_result(frame: &WireFrame) -> Option<(String, ToolResult)> {
            let value = frame_json(frame)?;
            if value.get("type")?.as_str()? != "FunctionCallResponse" {
                return None;
            }
            let id = value.get("id")?.as_str()?.to_string();
            let payload: serde_json::Value =
                serde_json::from_str(value.get("content")?.as_str()?).ok()?;
            let result = match payload.get("error") {
                Some(err) => ToolResult::Err {
                    code: err.get("code")?.as_str()?.to_string(),
                    message: err.get("message")?.as_str()?.to_string(),
                },
                None => ToolResult::Ok(payload.clone()),
            };
            Some((id, result))
        }

        struct DgvaSim {
            handle: WireHandle,
            /// The cancel record shared with [`BargeObservedChannel`]: this
            /// wire has no client cancel frame (dgva-1a ruling), so the
            /// socket cannot carry it — see the wrapper's doc for where the
            /// observation actually happens.
            interrupts: Arc<Mutex<Vec<Option<u64>>>>,
            /// The `gen` of the last scripted response audio, so
            /// [`ProviderSim::emit_audio`] can tell a continuation of the
            /// open turn from the start of a new one — see its doc for the
            /// delimiter frames that difference scripts.
            last_audio_gen: Option<u64>,
        }

        impl DgvaSim {
            fn send_json(&self, frame: serde_json::Value) {
                self.handle.send(WireFrame::Text(frame.to_string()));
            }

            /// Adapter frames, snapshot only once two consecutive reads
            /// agree: a send the suite awaited is flushed to the socket but
            /// not necessarily read by the server yet.
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
        impl ProviderSim for DgvaSim {
            /// The agent wire carries no generation id — `gen` itself never
            /// reaches a frame. What the wire does carry is the turn
            /// boundary: audio only ever starts under a different generation
            /// because the previous response ended or was superseded, and the
            /// server delimits that with `AgentAudioDone` (closing the open
            /// turn) then `AgentStartedSpeaking` (opening the next — the
            /// gemini sim's `serverContent.interrupted` analog, module header
            /// framing provenance). So a `gen` change scripts that
            /// close-then-open pair; the first audio of the session opens
            /// with a bare `AgentStartedSpeaking`; same-`gen` audio is a
            /// continuation of the open turn and scripts nothing extra. The
            /// adapter's turn-scoped tagging reconstructs the suite's
            /// staleness from those delimiters. Deliberately NEVER
            /// `UserStartedSpeaking`: that frame bumps the shared generation
            /// at decode (the read loop's barge-in half), which would stack
            /// bumps beyond what the suite's drive sequences expect.
            async fn emit_audio(&mut self, gen: u64, pcm: Vec<i16>) {
                match self.last_audio_gen {
                    Some(last) if last == gen => {}
                    Some(_) => {
                        self.send_json(serde_json::json!({"type": "AgentAudioDone"}));
                        self.send_json(serde_json::json!({"type": "AgentStartedSpeaking"}));
                    }
                    None => self.send_json(serde_json::json!({"type": "AgentStartedSpeaking"})),
                }
                self.last_audio_gen = Some(gen);
                let mut bytes = Vec::with_capacity(pcm.len() * 2);
                for sample in &pcm {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
                self.handle.send(WireFrame::Binary(bytes));
            }

            /// `ConversationText` narrates BOTH sides of the conversation
            /// (module header, server vocabulary), role user/assistant.
            async fn emit_transcript(&mut self, role: Role, text: String) {
                let role = match role {
                    Role::Assistant => "assistant",
                    Role::User => "user",
                };
                self.send_json(serde_json::json!({
                    "type": "ConversationText", "role": role, "content": text
                }));
            }

            /// One client-side `FunctionCallRequest` entry — `arguments` a
            /// JSON *string* per the wire shape (module header, server
            /// vocabulary), `client_side: true` so the read loop bridges it.
            async fn emit_tool_call(
                &mut self,
                call_id: String,
                name: String,
                args: serde_json::Value,
            ) {
                self.send_json(serde_json::json!({
                    "type": "FunctionCallRequest",
                    "functions": [{
                        "id": call_id,
                        "name": name,
                        "arguments": args.to_string(),
                        "client_side": true,
                    }]
                }));
            }

            async fn announce_session_end(&mut self) {
                unreachable!(
                    "the agent wire declares session_renewal: false — run_all \
                     must never drive a renewal check against this rig"
                );
            }

            async fn complete_renewal(&mut self) {
                unreachable!(
                    "the agent wire declares session_renewal: false — run_all \
                     must never drive a renewal check against this rig"
                );
            }

            async fn expire(&mut self) {
                unreachable!(
                    "the agent wire declares session_renewal: false — run_all \
                     must never drive a renewal check against this rig"
                );
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
                    .filter_map(inject_user_text)
                    .collect()
            }

            /// `UpdatePrompt` REPLACES the whole `think.prompt` (module
            /// header, client vocabulary), so each frame carries the full
            /// re-render, not the update that caused it. The updates are
            /// recovered by diffing consecutive prompt states — seeded from
            /// the Settings first message's own `think.prompt`, so the rig
            /// never hardcodes the session context: an excerpt/chapter change
            /// is the `ChapterBoundary` that scripted it (which also clears
            /// the position — [`ThinkPrompt`]'s staleness rule), anything
            /// else is the `PlaybackPosition` update.
            async fn received_context_updates(&mut self) -> Vec<ContextUpdate> {
                let mut prev: Option<(String, Option<u32>, Option<u64>)> = None;
                let mut updates = Vec::new();
                for frame in self.settled_frames().await.iter().flatten() {
                    let Some(value) = frame_json(frame) else {
                        continue;
                    };
                    match value.get("type").and_then(|kind| kind.as_str()) {
                        Some("Settings") => {
                            prev = value
                                .get("agent")
                                .and_then(|agent| agent.get("think"))
                                .and_then(|think| think.get("prompt"))
                                .and_then(|prompt| prompt.as_str())
                                .and_then(prompt_parts);
                        }
                        Some("UpdatePrompt") => {
                            let parts = value
                                .get("prompt")
                                .and_then(|prompt| prompt.as_str())
                                .and_then(prompt_parts)
                                .expect("every UpdatePrompt renders the ThinkPrompt composition");
                            let before = prev
                                .replace(parts.clone())
                                .expect("the Settings first message precedes every UpdatePrompt");
                            let (excerpt, chapter_idx, pos_ms) = parts;
                            if excerpt != before.0 || chapter_idx != before.1 {
                                updates.push(ContextUpdate::ChapterBoundary {
                                    chapter_idx: chapter_idx
                                        .expect("a chapter boundary renders its chapter line"),
                                    excerpt: DelimitedExcerpt {
                                        delimited_text: excerpt,
                                    },
                                });
                            } else {
                                updates.push(ContextUpdate::PlaybackPosition {
                                    pos_ms: pos_ms
                                        .expect("a position update renders its position line"),
                                });
                            }
                        }
                        _ => {}
                    }
                }
                updates
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
        /// around it (the gemini.rs m3-fix3 shape). On this adapter
        /// `interrupt` sends no frame — the client vocabulary has no cancel
        /// message (dgva-1a ruling), so the generation-tag drop IS the cancel
        /// — which means the socket cannot carry the suite's cancel record
        /// ([`ProviderSim::received_interrupts`]). The channel's real
        /// interrupt behavior is still observable: it advances the shared
        /// barge-in counter, the same counter its read loop tags turns with.
        /// So the rig records a cancel only when the delegated call
        /// demonstrably performed that advance — an interrupt that left the
        /// counter untouched records nothing and the suite reds; the record
        /// is tied to what the channel did, not to the call — and the
        /// recorded value is the counter the cancel observed on entry, the
        /// same snapshot-at-cancel-time the mock records when its cancel op
        /// reaches the provider.
        struct BargeObservedChannel {
            inner: DeepgramAgentChannel,
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

        /// Counts rigs so the entry test can assert the capability gate: on a
        /// `session_renewal: false` wire, `run_all` builds exactly one rig
        /// per core check plus the capabilities probe — never a renewal rig
        /// (the conformance suite's own NoRenewalFixture arithmetic).
        struct DgvaFixture {
            rigs: usize,
        }

        #[async_trait]
        impl ConformanceFixture for DgvaFixture {
            type Channel = BargeObservedChannel;
            type Sim = DgvaSim;

            async fn rig(&mut self, generation: &Generation) -> (BargeObservedChannel, DgvaSim) {
                self.rigs += 1;
                let handle = WireServer::start().await;
                let concierge = handle.clone();
                tokio::spawn(async move {
                    let script = load_fixture("deepgram-agent-conformance-session");
                    replay(&script, &concierge).await;
                });
                let channel = DeepgramAgentChannel::new(
                    byok_config(Some(handle.url("/v1/agent/converse"))),
                    keys_with_agent_and_byo_keys(),
                    generation.clone(),
                );
                let interrupts = Arc::new(Mutex::new(Vec::new()));
                (
                    BargeObservedChannel {
                        inner: channel,
                        generation: generation.clone(),
                        interrupts: interrupts.clone(),
                    },
                    DgvaSim {
                        handle,
                        interrupts,
                        last_audio_gen: None,
                    },
                )
            }
        }

        #[tokio::test]
        async fn deepgram_agent_passes_session_channel_conformance() {
            let mut fixture = DgvaFixture { rigs: 0 };
            run_all(&mut fixture).await;
            assert_eq!(
                fixture.rigs, 8,
                "seven core checks + the capabilities probe — session_renewal: \
                 false means run_all never requested a renewal rig"
            );
        }
    }

    /// F11-U3: the think prompt carries the resume recap behind the composed
    /// framing, after the system prompt and excerpt — never inside them.
    #[test]
    fn resume_recap_rides_the_think_prompt_behind_the_composed_framing() {
        use crate::voice::channel::types::{Location, RecapOrigin, UiLang};
        let ctx = SessionContext {
            book_id: Some("book-1".into()),
            session_id: "session-1".into(),
            system_prompt: "You are the reading tutor.".into(),
            excerpt: DelimitedExcerpt {
                delimited_text: "<<x>>excerpt<</x>>".into(),
            },
            rolling_summary: None,
            playback_pos_ms: 0,
            tool_schemas: vec![],
        };
        let origin = RecapOrigin::Continue;
        let location = Location {
            chapter: 1,
            page: None,
        };
        let mut prompt = ThinkPrompt::new(&ctx);
        prompt.apply(&ContextUpdate::ResumeRecap {
            recap: DelimitedExcerpt {
                delimited_text: "<<s>>user: hi<</s>>".into(),
            },
            origin: origin.clone(),
            location: Some(location.clone()),
            topic: None,
            lang: UiLang::En,
        });
        let framing = crate::voice::channel::types::resume_recap_framing(
            &origin,
            Some(&location),
            None,
            UiLang::En,
        );
        assert_eq!(
            prompt.render(),
            format!(
                "You are the reading tutor.\n<<x>>excerpt<</x>>\nNote: {framing}\n<<s>>user: hi<</s>>"
            )
        );
    }
}
