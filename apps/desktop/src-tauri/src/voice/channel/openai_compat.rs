//! OpenAI-compatible streaming chat — the baseline [`LlmSlot`] (VA5,
//! VOICE_AGENT §3.1/§3.2; the skeleton stride built the config- and
//! credential-side request plumbing, this stride lands the streaming paths:
//! SSE decode, tool-call assembly, the stateless-HTTP continuation, and
//! barge-in abort).
//!
//! Wire provenance (§3.2 implementation rule — the full schema notes are in
//! the [`super::composed`] module header, recorded at the skeleton stride):
//! canonical reference
//! https://platform.openai.com/docs/api-reference/chat/streaming, verified
//! 2026-08-21 against OpenAI's official machine-readable API spec
//! https://github.com/openai/openai-openapi (raw `master` `openapi.yaml`,
//! accessed 2026-08-21). `POST {base_url}/chat/completions` with
//! `"stream": true` returns data-only server-sent events; each event is a
//! `chat.completion.chunk` whose `choices[].delta` carries `content` text
//! fragments and `tool_calls[]` chunks (required `index`; `id` and
//! `function.name` on a call's first fragment; `function.arguments` as
//! JSON-string fragments the client concatenates); `finish_reason` is `null`
//! mid-stream; the stream terminates with the `data: [DONE]` sentinel. The
//! credential rides the `Authorization: Bearer` header only — never a query
//! param (§3.2 `auth-hygiene-no-query-param-creds`). The baseline shape is
//! what the default [`ProviderProfile::Generic`] emits; real "compat"
//! endpoints deviate, and each deviation the slot handles is verified
//! against the provider's live official docs (§3.2 implementation rule):
//! - Qwen/DashScope ([`ProviderProfile::Qwen`]):
//!   https://help.aliyun.com/en/model-studio/qwen-api-via-openai-chat-completions
//!   (accessed 2026-08-22) — `parallel_tool_calls` carries "Default value:
//!   `false`" on this wire (OpenAI defaults it true), and the non-standard
//!   `tool_stream` field ("Takes effect only when `stream=true`") decides
//!   whether complex (array/object-typed) tool arguments stream as
//!   fragments or land as one block; the profile emits both alongside
//!   `tools` so DashScope behaves like the baseline the streaming decode
//!   assumes.
//! - DeepSeek ([`ProviderProfile::DeepSeek`]):
//!   https://api-docs.deepseek.com/guides/chat_prefix_completion and
//!   https://api-docs.deepseek.com/guides/fim_completion (both accessed
//!   2026-08-22) — both Beta features carry the same gate: "The user needs
//!   to set `base_url="https://api.deepseek.com/beta"` to enable the Beta
//!   feature." Streaming chat itself is GA on the normal base, so the chat
//!   URL never moves; the profile owns the per-feature base-path routing
//!   ([`ProviderProfile::feature_base_url`]).
//! - Groq ([`ProviderProfile::Groq`]): https://console.groq.com/docs/openai
//!   (accessed 2026-08-22) — `logprobs`, `logit_bias`, `top_logprobs`,
//!   `messages[].name`, and `n` (accepted only as 1) "are currently not
//!   supported and will result in a 400 error" if supplied; the profile
//!   strips them wherever a body carries them, so no future request knob
//!   can 400 a Groq session by accident.
//!
//! OpenRouter variance stays banked for a later stride (VA5 bank U4).
//!
//! Statelessness rule: the wire is per-turn HTTP, so the conversation lives
//! in the slot — `history` is the rendered `messages` list, system message at
//! index 0, re-rendered on `update_context` via the same composition rule the
//! Deepgram Agent `ThinkPrompt` pinned (`{system}\n{excerpt}` + optional
//! state lines), so the two prompt-side adapters can never drift apart. A
//! turn's task appends the assembled assistant message (accumulated
//! `content` + assembled `tool_calls`) when the first non-null
//! `finish_reason` closes the turn — BEFORE any `ToolCall` event is emitted,
//! so by the time the harness answers via [`LlmSlot::send_tool_result`] the
//! continuation history is already consistent. `tool_schemas` travel into the
//! request `tools` field verbatim: this wire family IS the OpenAI shape, so
//! no translation layer exists to get wrong.
//!
//! Turn-complete signal: [`ChannelEvent::TurnComplete`], emitted when a
//! finish leaves the reply complete — the event.rs vocabulary growth the
//! earlier strides deferred to the cc-1e TTS wiring, which needed it (the
//! composed loop flushes the buffered reply into `TtsSlot::speak` on this
//! boundary; [`super::composed::composition_loop`] owns that decision's
//! doc). "Complete" is precise: a `finish_reason` that surfaced answerable
//! tool calls emits nothing — the harness owes `send_tool_result`
//! continuations, and the reply keeps streaming after them — so the boundary
//! fires only on a finish with no pending tool calls (`stop`/`length`/
//! `content_filter`, or a tool-shaped finish whose calls were all dropped as
//! unanswerable). The assistant message is still committed to history at
//! every finish, before any event, exactly as before.
//!
//! Interrupt (§2 rule 2): aborting the turn's task drops the in-flight
//! response stream, which aborts the connection. The interrupted turn's
//! partial assistant message is NOT committed to history — the remnants die
//! uncommitted, mirroring gemini.rs where a superseded turn's tail dies at
//! the generation gates; the gate semantics themselves stay harness-side.
//!
//! Error split (§9 degradation matrix): pre-flight failures (key resolution,
//! request build) return through each verb's `Result`; wire failures ride
//! `Error` events on the stream (the read-loop split every adapter uses).
//! HTTP status mapping mirrors the matrix rows: 401/403 → `Auth`
//! non-retryable (key revoked), 429 → `RateLimited` retryable, 5xx →
//! `Provider` retryable, any other non-2xx → `Protocol` non-retryable;
//! transport and mid-stream failures → `Transport` retryable.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::voice::keys::KeyManager;

use super::composed::{LlmSlot, LlmSlotConfig};
use super::{
    ChannelError, ChannelEvent, ContextUpdate, ErrorKind, Role, SessionContext, ToolResult,
};

const EVENT_CAPACITY: usize = 4096;

/// Which real provider sits behind the configured OpenAI-compatible base
/// URL — the quirk knob for "compat" endpoints that deviate from the
/// baseline request shape (module header, quirk provenance). Chosen at
/// construction ([`OpenAiCompatLlmSlot::with_profile`]) and constant for
/// the slot's life: the profile is wire-shape knowledge private to this
/// adapter, and every [`LlmSlotConfig`] construction site lives in the
/// composed-channel module this stride must not touch, so the knob rides
/// the slot rather than the shared config type — the slot-registry stride
/// is the natural place to lift it into config wiring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderProfile {
    /// A spec-shaped endpoint: the unmodified baseline body. The `new()`
    /// default, so existing construction sites keep their exact shape.
    #[default]
    Generic,
    /// Qwen/DashScope compatible mode (module header provenance): emits
    /// `tool_stream: true` and an explicit `parallel_tool_calls: true`
    /// alongside `tools`.
    Qwen,
    /// DeepSeek (module header provenance): the baseline body verbatim —
    /// its deviation is path-side, gating [`CompatFeature`]s behind the
    /// `/beta` base ([`Self::feature_base_url`]).
    DeepSeek,
    /// Groq (module header provenance): strips the five documented
    /// 400-on-sight fields from the request body.
    Groq,
}

/// OpenAI-compat features some providers gate behind a different base path
/// (today: DeepSeek's Beta surface — module header provenance). A slot
/// wanting one asks [`ProviderProfile::feature_base_url`] where — or
/// whether — the configured provider serves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatFeature {
    /// Constraining how the reply starts via a final `assistant` message
    /// flagged `prefix: true` (DeepSeek "Chat Prefix Completion (Beta)").
    ChatPrefixCompletion,
    /// Fill-in-the-middle `/completions` with `prompt` + `suffix`
    /// (DeepSeek "FIM Completion (Beta)").
    FimCompletion,
}

impl ProviderProfile {
    /// Applies the profile's documented deviations to the baseline request
    /// body — the single place every provider quirk shapes the wire, so a
    /// profile's whole deviation surface is auditable in one match arm.
    /// Where a request using `feature` must be sent, given the configured
    /// base URL — `Some(base)` is the base path the feature's request must
    /// target, `None` means the profile's wire does not document the
    /// feature at all and the caller must not emit its shape. DeepSeek
    /// gates every [`CompatFeature`] behind `/beta` (module header
    /// provenance) while plain streaming chat stays on the configured base,
    /// so [`OpenAiCompatLlmSlot::chat_completions_url`] never consults
    /// this. The configured base normalizes first: DeepSeek documents
    /// `https://api.deepseek.com/v1` as an alias of the bare base, so a
    /// trailing `/v1` is replaced — never suffixed — by `/beta`, and an
    /// already-`/beta` base passes through.
    pub fn feature_base_url(&self, configured: &str, feature: CompatFeature) -> Option<String> {
        match self {
            Self::DeepSeek => {
                // Both known features are Beta-gated; the match stays
                // exhaustive so a future non-Beta feature must decide.
                match feature {
                    CompatFeature::ChatPrefixCompletion | CompatFeature::FimCompletion => {}
                }
                let base = configured.trim_end_matches('/');
                let base = base.strip_suffix("/v1").unwrap_or(base);
                if base.ends_with("/beta") {
                    Some(base.to_string())
                } else {
                    Some(format!("{base}/beta"))
                }
            }
            Self::Generic | Self::Qwen | Self::Groq => None,
        }
    }

    fn shape_request(&self, body: &mut serde_json::Value) {
        match self {
            Self::Generic | Self::DeepSeek => {}
            Self::Groq => {
                // The documented 400-fields (module header provenance) are
                // removed wherever they appear rather than merely never
                // emitted: the builder doesn't produce them today, but this
                // hook is the wire's last exit, so the guarantee survives
                // future body knobs. `n`'s only accepted value is the wire
                // default (1), so dropping it outright is shape-neutral.
                if let Some(fields) = body.as_object_mut() {
                    for rejected in ["logprobs", "logit_bias", "top_logprobs", "n"] {
                        fields.remove(rejected);
                    }
                }
                if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
                    for message in messages {
                        if let Some(message) = message.as_object_mut() {
                            message.remove("name");
                        }
                    }
                }
            }
            Self::Qwen => {
                // Both fields are tool/stream options (module header
                // provenance): DashScope holds complex tool arguments to a
                // single block unless `tool_stream` is set, and flips the
                // `parallel_tool_calls` default to false — so both ride
                // only a request that actually carries tools.
                if body.get("tools").is_some() {
                    body["tool_stream"] = serde_json::Value::Bool(true);
                    body["parallel_tool_calls"] = serde_json::Value::Bool(true);
                }
            }
        }
    }
}

/// The baseline [`LlmSlot`] (VOICE_AGENT §3.2 slot table: "LLM —
/// OpenAI-compatible streaming chat"). Stateless per-turn HTTP: no transport
/// is held between turns, so the struct is the request-derivation state —
/// the credential-free config, the keyring handle the credential is resolved
/// through at call time, and one pooled HTTP client.
pub struct OpenAiCompatLlmSlot {
    config: LlmSlotConfig,
    /// The provider-quirk knob ([`ProviderProfile`]); construction-time,
    /// like everything else request derivation reads.
    profile: ProviderProfile,
    keys: Arc<KeyManager>,
    /// One clonable handle over a shared connection pool, built at
    /// construction so per-turn requests reuse connections.
    http: reqwest::Client,
    /// The parts the system message (history index 0) is re-rendered from on
    /// `update_context` (see [`PromptParts`]); `Some` from `install_context`
    /// on, and the installed-context gate every send checks.
    prompt: Option<PromptParts>,
    /// [`SessionContext::tool_schemas`], passed into the request `tools`
    /// field verbatim (module header: this wire family IS the OpenAI shape).
    tool_schemas: Vec<serde_json::Value>,
    /// The conversation — the rendered `messages` list, system message at
    /// index 0 (module header, statelessness rule). Shared with the turn
    /// task, which appends the assembled assistant message at
    /// `finish_reason`.
    history: Arc<Mutex<Vec<serde_json::Value>>>,
    /// Tool-call ids surfaced as `ToolCall` events but not yet answered (the
    /// gemini.rs/deepgram_agent.rs bookkeeping shape, minus the name — the
    /// OpenAI-compat `role: "tool"` message carries only `tool_call_id`).
    /// The turn task inserts before emitting; `send_tool_result` rejects ids
    /// not in here (`InvalidState`, non-retryable — the dgva-1f rule for a
    /// wire with no cancellation vocabulary).
    pending_tool_calls: Arc<Mutex<HashSet<String>>>,
    /// The one in-flight turn task; aborted by `interrupt` (§2 rule 2) and
    /// superseded by the next send.
    in_flight: Option<JoinHandle<()>>,
    /// The event pair exists from construction: `take_events` must yield the
    /// receiver pre-connect (the every-adapter shape, conformance check 1);
    /// turn tasks only clone this sender.
    events_tx: mpsc::Sender<ChannelEvent>,
    events: Option<mpsc::Receiver<ChannelEvent>>,
}

impl OpenAiCompatLlmSlot {
    /// Builds the slot from the composed channel's credential-free LLM
    /// config. As with the launch STT slot, no slot registry exists yet, so
    /// until a registry stride lands this constructor is the §3.2 "one
    /// registry entry" for the baseline LLM slot.
    pub fn new(config: LlmSlotConfig, keys: Arc<KeyManager>) -> Self {
        Self::with_profile(config, keys, ProviderProfile::default())
    }

    /// [`Self::new`] with an explicit [`ProviderProfile`] — the quirk knob's
    /// construction-time entry point (its doc explains why it rides the slot
    /// and not [`LlmSlotConfig`]).
    pub fn with_profile(
        config: LlmSlotConfig,
        keys: Arc<KeyManager>,
        profile: ProviderProfile,
    ) -> Self {
        let (events_tx, events) = mpsc::channel(EVENT_CAPACITY);
        Self {
            config,
            profile,
            keys,
            http: reqwest::Client::new(),
            prompt: None,
            tool_schemas: Vec::new(),
            history: Arc::new(Mutex::new(Vec::new())),
            pending_tool_calls: Arc::new(Mutex::new(HashSet::new())),
            in_flight: None,
            events_tx,
            events: Some(events),
        }
    }

    /// `{base_url}/chat/completions` (module header provenance), tolerant of
    /// a trailing slash on the configured base URL.
    fn chat_completions_url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    /// The streaming request body: config `model`, the rendered `messages`,
    /// `stream: true`, and — when the installed context carries any — the
    /// `tools` the model may call, passed through verbatim (module header,
    /// statelessness rule). The plain OpenAI-compatible baseline shape,
    /// then the profile's documented deviations
    /// ([`ProviderProfile::shape_request`]).
    fn request_body(&self, messages: &[serde_json::Value]) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "stream": true,
        });
        if !self.tool_schemas.is_empty() {
            body["tools"] = serde_json::json!(self.tool_schemas);
        }
        self.profile.shape_request(&mut body);
        body
    }

    /// Builds one ready-to-send streaming chat request. The key is resolved
    /// here — at call time, per request — through the VA3 keyring under the
    /// configured base URL: `Provider::OpenaiCompat` has no default base URL
    /// (`voice::keys::probe`), so the base URL IS the key identity
    /// (VOICE_AGENT §3.2/§7 per-slot per-base-URL rule). The credential
    /// rides only the `Authorization: Bearer` header, marked sensitive to
    /// stay out of Debug renderings; URL and body stay credential-free
    /// (§3.2 `auth-hygiene-no-query-param-creds`).
    async fn build_chat_request(
        &self,
        messages: &[serde_json::Value],
    ) -> Result<reqwest::Request, ChannelError> {
        let key = self
            .keys
            .session_key(&self.config.base_url)
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Auth,
                    false,
                    format!("no usable key for {}: {err}", self.config.base_url),
                )
            })?;
        let mut auth =
            HeaderValue::from_str(&format!("Bearer {}", key.expose())).map_err(|_| {
                ChannelError::new(
                    ErrorKind::Auth,
                    false,
                    "the stored key contains characters no header can carry",
                )
            })?;
        auth.set_sensitive(true);
        self.http
            .post(self.chat_completions_url())
            .header(AUTHORIZATION, auth)
            .json(&self.request_body(messages))
            .build()
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Protocol,
                    false,
                    format!("chat request build failed: {err}"),
                )
            })
    }

    /// The installed-context gate every send checks — the [`LlmSlot`]
    /// analogue of "send before connect" (`ErrorKind::InvalidState`'s own
    /// example): the slot has no transport, so [`LlmSlot::install_context`]
    /// is what "connected" means here.
    fn require_context(&self, verb: &str) -> Result<(), ChannelError> {
        if self.prompt.is_some() {
            return Ok(());
        }
        Err(ChannelError::new(
            ErrorKind::InvalidState,
            false,
            format!("{verb} before install_context"),
        ))
    }

    /// Snapshot of the conversation plus the not-yet-committed next message —
    /// what the turn's request carries. The message is committed to history
    /// only after the request builds, so a pre-flight failure leaves the
    /// conversation exactly as it was.
    fn messages_with(&self, next: &serde_json::Value) -> Vec<serde_json::Value> {
        let history = self.history.lock().unwrap();
        history.iter().cloned().chain([next.clone()]).collect()
    }

    /// Spawns the turn's request/decode task ([`stream_turn`]). One turn is
    /// in flight at a time: a still-running predecessor is aborted the same
    /// way `interrupt` aborts it — a new send supersedes the old turn.
    fn spawn_turn(&mut self, request: reqwest::Request) {
        if let Some(stale) = self.in_flight.take() {
            stale.abort();
        }
        self.in_flight = Some(tokio::spawn(stream_turn(
            self.http.clone(),
            request,
            self.events_tx.clone(),
            Arc::clone(&self.history),
            Arc::clone(&self.pending_tool_calls),
        )));
    }
}

/// The parts the system message is re-rendered from — the same composition
/// state the Deepgram Agent adapter keeps as `ThinkPrompt`, kept shape-equal
/// deliberately (module header, statelessness rule): `install_context` seeds
/// system prompt + excerpt, `ContextUpdate::ChapterBoundary` swaps the
/// excerpt and clears the playback position (a position from the old chapter
/// is stale against the new excerpt), `ContextUpdate::PlaybackPosition`
/// updates the position line. The delimited excerpt travels inside verbatim —
/// the adapter never parses it (VOICE_AGENT §4); refreshes happen only at the
/// harness's chosen boundaries (§4: never a blind mid-turn re-prompt).
#[derive(Debug, Clone)]
pub(super) struct PromptParts {
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

impl PromptParts {
    pub(super) fn new(ctx: &SessionContext) -> Self {
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

    /// The composition rule shared with the Deepgram Agent `ThinkPrompt`:
    /// system prompt, then the delimited excerpt, then the lightweight-state
    /// lines when set — so the prompt-side adapters can never drift apart.
    pub(super) fn render(&self) -> String {
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

    fn as_system_message(&self) -> serde_json::Value {
        serde_json::json!({ "role": "system", "content": self.render() })
    }

    /// One context update folded into the stored parts (the harness owns
    /// when this fires, never mid-turn).
    pub(super) fn apply(&mut self, update: ContextUpdate) {
        match update {
            ContextUpdate::ChapterBoundary {
                chapter_idx,
                excerpt,
            } => {
                self.excerpt = excerpt.delimited_text;
                self.chapter_idx = Some(chapter_idx);
                // A position from the old chapter is stale against the new
                // excerpt; it returns with the next PlaybackPosition update.
                self.playback_pos_ms = None;
            }
            ContextUpdate::PlaybackPosition { pos_ms } => {
                self.playback_pos_ms = Some(pos_ms);
            }
            ContextUpdate::RenewalRecap { recap, switched_to } => {
                self.renewal_recap = Some(format!(
                    "{}\n{}",
                    crate::voice::channel::types::renewal_recap_framing(switched_to),
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
                        &origin,
                        location.as_ref(),
                        topic.as_deref(),
                        lang,
                    ),
                    recap.delimited_text,
                ));
            }
            ContextUpdate::CodeSwitchFragment { terms, active } => {
                self.code_switch_terms = active.then_some(terms);
            }
        }
    }
}

/// HTTP status → the trait's error classes, mirroring the §9 degradation
/// matrix rows (module header, error split): 401/403 key revoked, 429 slow
/// down, 5xx provider-side, anything else a protocol surprise.
fn classify_status(status: reqwest::StatusCode) -> (ErrorKind, bool) {
    match status.as_u16() {
        401 | 403 => (ErrorKind::Auth, false),
        429 => (ErrorKind::RateLimited, true),
        code if code >= 500 => (ErrorKind::Provider, true),
        _ => (ErrorKind::Protocol, false),
    }
}

/// The `role: "tool"` message's `content` string. The envelope rule is the
/// one gemini.rs and deepgram_agent.rs already pinned (an object passes
/// through, anything else wraps as `{"result": …}`, an error becomes
/// `{"error": {code, message}}`), serialized because the OpenAI-compat tool
/// message carries a string (module header provenance).
fn tool_result_content(result: &ToolResult) -> String {
    let payload = match result {
        ToolResult::Ok(value) if value.is_object() => value.clone(),
        ToolResult::Ok(value) => serde_json::json!({ "result": value }),
        ToolResult::Err { code, message } => serde_json::json!({
            "error": { "code": code, "message": message }
        }),
    };
    payload.to_string()
}

/// One in-progress tool call, accumulated across `delta.tool_calls[]`
/// fragments keyed by their required `index` (module header provenance:
/// `id`/`function.name` on the first fragment, `function.arguments` as
/// JSON-string pieces the client concatenates).
#[derive(Default)]
struct ToolCallAssembly {
    id: String,
    name: String,
    arguments: String,
}

/// One turn's request/decode task (module header: streaming decode, error
/// split). Sends the built request, then consumes the data-only SSE stream
/// line-by-line: `choices[0].delta.content` fragments surface immediately as
/// assistant `TranscriptDelta`s; tool-call fragments accumulate by `index`;
/// the first non-null `finish_reason` closes the turn — the assembled
/// assistant message is committed to history first, then each complete tool
/// call (id and name both present; entries missing either are unanswerable
/// and are dropped from events AND history, the gemini.rs rule) registers as
/// pending and surfaces as a `ToolCall` event with its concatenated
/// `arguments` parsed to uniform args (unparseable → `{}` so the call stays
/// answerable, the dgva-1f rule); a finish that registered no pending calls
/// closes the reply with one `TurnComplete` (module header, turn-complete
/// signal). Chunks after the finish (e.g. usage frames) are read and ignored
/// until `data: [DONE]` or EOF. A stream that
/// ends without a finish commits nothing: EOF is a retryable `Transport`
/// error, a `[DONE]` without a finish a non-retryable `Protocol` one.
async fn stream_turn(
    http: reqwest::Client,
    request: reqwest::Request,
    events: mpsc::Sender<ChannelEvent>,
    history: Arc<Mutex<Vec<serde_json::Value>>>,
    pending_tool_calls: Arc<Mutex<HashSet<String>>>,
) {
    let response = match http.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            let _ = events
                .send(ChannelEvent::Error(ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("chat request failed: {err}"),
                )))
                .await;
            return;
        }
    };
    let status = response.status();
    if !status.is_success() {
        let (kind, retryable) = classify_status(status);
        let _ = events
            .send(ChannelEvent::Error(ChannelError::new(
                kind,
                retryable,
                format!("chat completions returned {status}"),
            )))
            .await;
        return;
    }

    let mut stream = response.bytes_stream();
    let mut lines = super::text::SseLines::default();
    let mut content = String::new();
    let mut tool_calls: Vec<ToolCallAssembly> = Vec::new();
    let mut finished = false;
    let mut saw_done = false;
    'read: while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(bytes) => bytes,
            Err(err) => {
                let _ = events
                    .send(ChannelEvent::Error(ChannelError::new(
                        ErrorKind::Transport,
                        true,
                        format!("chat stream failed: {err}"),
                    )))
                    .await;
                return;
            }
        };
        // Byte-level line framing: a chunk may split a UTF-8 sequence
        // (review 2026-10-09, R1 — Arabic letters are two bytes).
        for line in lines.push(&chunk) {
            // Data-only SSE: blank separator lines, comments, and any other
            // field are skipped; every payload rides a `data:` line.
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim_start();
            if data == "[DONE]" {
                saw_done = true;
                break 'read;
            }
            if finished {
                // Post-finish frames (e.g. a usage chunk) carry nothing this
                // role consumes; read on to the terminator.
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            // `n` defaults to 1 on this wire; choices[0] is the response.
            let Some(choice) = value.get("choices").and_then(|choices| choices.get(0)) else {
                continue;
            };
            if let Some(delta) = choice.get("delta") {
                if let Some(text) = delta.get("content").and_then(|text| text.as_str()) {
                    if !text.is_empty() {
                        content.push_str(text);
                        if events
                            .send(ChannelEvent::TranscriptDelta {
                                role: Role::Assistant,
                                text: text.to_string(),
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                if let Some(fragments) = delta.get("tool_calls").and_then(|calls| calls.as_array())
                {
                    for fragment in fragments {
                        let Some(index) = fragment.get("index").and_then(|index| index.as_u64())
                        else {
                            continue;
                        };
                        const MAX_TOOL_CALLS: usize = 64;
                        const MAX_INDEX_LOOKAHEAD: usize = 4;

                        let Ok(index) = usize::try_from(index) else {
                            continue;
                        };
                        if index >= MAX_TOOL_CALLS {
                            continue;
                        }
                        if index > tool_calls.len() + MAX_INDEX_LOOKAHEAD {
                            tracing::debug!(
                                "openai_compat: tool_calls index {index} exceeds lookahead \
                                 (current len {}), skipping fragment",
                                tool_calls.len()
                            );
                            continue;
                        }
                        while tool_calls.len() <= index {
                            tool_calls.push(ToolCallAssembly::default());
                        }
                        let call = &mut tool_calls[index];
                        if let Some(id) = fragment.get("id").and_then(|id| id.as_str()) {
                            call.id = id.to_string();
                        }
                        if let Some(name) = fragment
                            .pointer("/function/name")
                            .and_then(|name| name.as_str())
                        {
                            call.name = name.to_string();
                        }
                        if let Some(arguments) = fragment
                            .pointer("/function/arguments")
                            .and_then(|arguments| arguments.as_str())
                        {
                            call.arguments.push_str(arguments);
                        }
                    }
                }
            }
            if choice
                .get("finish_reason")
                .and_then(|reason| reason.as_str())
                .is_some()
            {
                finished = true;
                let complete: Vec<&ToolCallAssembly> = tool_calls
                    .iter()
                    .filter(|call| !call.id.is_empty() && !call.name.is_empty())
                    .collect();
                let mut message = serde_json::json!({
                    "role": "assistant",
                    "content": if content.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(content.clone())
                    },
                });
                if !complete.is_empty() {
                    message["tool_calls"] = complete
                        .iter()
                        .map(|call| {
                            serde_json::json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": call.arguments,
                                },
                            })
                        })
                        .collect();
                }
                // History first, events second: whoever observes a ToolCall
                // is guaranteed the continuation history is already
                // consistent (module header, statelessness rule).
                history.lock().unwrap().push(message);
                let awaiting_answers = !complete.is_empty();
                for call in complete {
                    pending_tool_calls.lock().unwrap().insert(call.id.clone());
                    let args = serde_json::from_str(&call.arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    if events
                        .send(ChannelEvent::ToolCall {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            args,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                // The reply boundary (module header, turn-complete signal):
                // only a finish that leaves no tool answers owed ends the
                // reply — with calls pending, the continuation's own finish
                // carries the boundary instead.
                if !awaiting_answers && events.send(ChannelEvent::TurnComplete).await.is_err() {
                    return;
                }
            }
        }
    }
    if !finished {
        let (kind, retryable, message) = if saw_done {
            (
                ErrorKind::Protocol,
                false,
                "chat stream terminated without a finish_reason",
            )
        } else {
            (
                ErrorKind::Transport,
                true,
                "chat stream ended before the turn finished",
            )
        };
        let _ = events
            .send(ChannelEvent::Error(ChannelError::new(
                kind, retryable, message,
            )))
            .await;
    }
}

#[async_trait]
impl LlmSlot for OpenAiCompatLlmSlot {
    fn install_context(&mut self, ctx: SessionContext) {
        // Wholesale replacement (trait contract): a fresh conversation whose
        // only message is the rendered system message; stale unanswered tool
        // calls from a replaced conversation are unanswerable.
        let parts = PromptParts::new(&ctx);
        *self.history.lock().unwrap() = vec![parts.as_system_message()];
        self.pending_tool_calls.lock().unwrap().clear();
        self.tool_schemas = ctx.tool_schemas;
        self.prompt = Some(parts);
    }

    async fn send_user_turn(&mut self, text: &str) -> Result<(), ChannelError> {
        self.require_context("send_user_turn")?;
        let user = serde_json::json!({ "role": "user", "content": text });
        let messages = self.messages_with(&user);
        let request = self.build_chat_request(&messages).await?;
        self.history.lock().unwrap().push(user);
        self.spawn_turn(request);
        Ok(())
    }

    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: ToolResult,
    ) -> Result<(), ChannelError> {
        self.require_context("send_tool_result")?;
        let outstanding = {
            let pending = self.pending_tool_calls.lock().unwrap();
            if !pending.contains(call_id) {
                // Unknown, already-answered, or from a replaced conversation:
                // the dgva-1f rule for a wire with no cancellation vocabulary.
                return Err(ChannelError::new(
                    ErrorKind::InvalidState,
                    false,
                    format!("no pending tool call {call_id}"),
                ));
            }
            pending.len()
        };
        let tool_message = serde_json::json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": tool_result_content(&result),
        });
        // A multi-call turn must answer every call before the wire accepts
        // the next assistant turn, so the continuation fires only with the
        // last outstanding answer; earlier ones just join the history.
        if outstanding > 1 {
            self.pending_tool_calls.lock().unwrap().remove(call_id);
            self.history.lock().unwrap().push(tool_message);
            return Ok(());
        }
        let messages = self.messages_with(&tool_message);
        let request = self.build_chat_request(&messages).await?;
        self.pending_tool_calls.lock().unwrap().remove(call_id);
        self.history.lock().unwrap().push(tool_message);
        self.spawn_turn(request);
        Ok(())
    }

    async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError> {
        self.require_context("update_context")?;
        let parts = self.prompt.as_mut().expect("gated above");
        parts.apply(update);
        // Stateless HTTP makes the refresh a pure history edit: the next
        // request simply carries the re-rendered system message (§4 — the
        // harness owns when this fires, never mid-turn).
        self.history.lock().unwrap()[0] = parts.as_system_message();
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<(), ChannelError> {
        // Aborting the task drops the in-flight response stream, which
        // aborts the connection; the partial turn stays uncommitted (module
        // header, interrupt). Generation-gate semantics stay harness-side
        // (§2 rule 2).
        if let Some(turn) = self.in_flight.take() {
            turn.abort();
        }
        Ok(())
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
        self.events.take()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::timeout;

    use super::*;
    use crate::voice::channel::DelimitedExcerpt;
    use crate::voice::keys::store::{Backend, KeyStore};
    use crate::voice::keys::ApiKey;

    /// Reaches the request in the auth header and nowhere else: the URL and
    /// body must stay credential-free.
    const FIXTURE_KEY: &str = "fixture-openai-compat-key-7d20";
    const BASE_URL: &str = "https://api.groq.com/openai/v1";

    fn keys_with_key_for(base_url: &str) -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(base_url, &ApiKey::new(FIXTURE_KEY.into()))
            .unwrap();
        Arc::new(KeyManager::with_store(store))
    }

    fn keys_with_compat_key() -> Arc<KeyManager> {
        keys_with_key_for(BASE_URL)
    }

    /// Construction/config: the request builder derives everything from the
    /// slot's config and the keyring — POST to the credential-free
    /// `{base}/chat/completions` URL (trailing slash tolerated), a
    /// `stream: true` JSON body carrying model + messages, and the key
    /// resolved at call time into a sensitive `Authorization: Bearer` header
    /// under the base-URL identity.
    #[tokio::test]
    async fn request_builder_derives_from_config_and_keyring() {
        let slot = OpenAiCompatLlmSlot::new(
            LlmSlotConfig {
                base_url: BASE_URL.into(),
                model: "llama-3.3-70b-versatile".into(),
            },
            keys_with_compat_key(),
        );

        let messages =
            vec![serde_json::json!({"role": "user", "content": "what does figure three show"})];
        let request = slot
            .build_chat_request(&messages)
            .await
            .expect("a configured slot with a stored key builds the request");

        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(
            request.url().as_str(),
            "https://api.groq.com/openai/v1/chat/completions",
            "credential-free URL: base + /chat/completions, nothing else"
        );

        let auth = request
            .headers()
            .get(AUTHORIZATION)
            .expect("the credential rides the Authorization header");
        assert!(auth.is_sensitive(), "the key stays out of Debug renderings");
        assert_eq!(auth.to_str().unwrap(), format!("Bearer {FIXTURE_KEY}"));

        let body: serde_json::Value = serde_json::from_slice(
            request
                .body()
                .and_then(|b| b.as_bytes())
                .expect("a buffered JSON body"),
        )
        .unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "model": "llama-3.3-70b-versatile",
                "messages": [{"role": "user", "content": "what does figure three show"}],
                "stream": true,
            }),
            "the baseline streaming body: model + messages + stream:true"
        );

        // Config normalization: a trailing slash on the configured base URL
        // must not double the separator.
        let slashed = OpenAiCompatLlmSlot::new(
            LlmSlotConfig {
                base_url: format!("{BASE_URL}/"),
                model: "llama-3.3-70b-versatile".into(),
            },
            keys_with_compat_key(),
        );
        assert_eq!(
            slashed.chat_completions_url(),
            "https://api.groq.com/openai/v1/chat/completions"
        );
    }

    /// The Qwen quirk fixture (module header, quirk provenance): the qwen
    /// profile's serialized request pins the DashScope-safe tool/stream
    /// shape — the non-standard `tool_stream: true` plus an explicit
    /// `parallel_tool_calls: true` (DashScope defaults it false where
    /// OpenAI defaults it true) riding alongside `tools` — while the
    /// default profile's request for the identical conversation stays the
    /// unmodified baseline, and a toolless qwen request carries neither
    /// field (both are tool options; without tools there is nothing to
    /// shape). Payload literals on all three.
    #[tokio::test]
    async fn qwen_profile_pins_the_dashscope_safe_tool_stream_shape() {
        const QWEN_BASE: &str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1";
        let messages =
            vec![serde_json::json!({"role": "user", "content": "what does figure three show"})];
        let baseline = serde_json::json!({
            "model": "qwen-plus",
            "messages": messages,
            "stream": true,
            "tools": [tool_schema()],
        });

        let config = LlmSlotConfig {
            base_url: QWEN_BASE.into(),
            model: "qwen-plus".into(),
        };
        let mut qwen = OpenAiCompatLlmSlot::with_profile(
            config.clone(),
            keys_with_key_for(QWEN_BASE),
            ProviderProfile::Qwen,
        );
        qwen.install_context(ctx());
        let mut expected = baseline.clone();
        expected["tool_stream"] = serde_json::Value::Bool(true);
        expected["parallel_tool_calls"] = serde_json::Value::Bool(true);
        assert_eq!(
            qwen.request_body(&messages),
            expected,
            "the qwen profile emits the Qwen-safe shape: baseline plus \
             tool_stream:true and parallel_tool_calls:true beside tools"
        );

        let mut default = OpenAiCompatLlmSlot::new(config, keys_with_key_for(QWEN_BASE));
        default.install_context(ctx());
        assert_eq!(
            default.request_body(&messages),
            baseline,
            "the default profile stays the unmodified baseline for the \
             identical conversation — the quirk fields are qwen-only"
        );

        let mut toolless = ctx();
        toolless.tool_schemas.clear();
        qwen.install_context(toolless);
        assert_eq!(
            qwen.request_body(&messages),
            serde_json::json!({
                "model": "qwen-plus",
                "messages": messages,
                "stream": true,
            }),
            "without tools the qwen profile shapes nothing: both quirk \
             fields are tool options"
        );
    }

    /// The DeepSeek quirk fixture (module header, quirk provenance): the
    /// deviation is path-side, so the pins are (a) the chat request itself
    /// never moves — full URL and payload literal on the configured base —
    /// and (b) every [`CompatFeature`] routes to the `/beta` base through
    /// the profile: bare base gets `/beta` appended, the documented `/v1`
    /// alias is replaced (never `/v1/beta`), an already-`/beta` base passes
    /// through, and the profiles without a documented Beta surface offer
    /// the features nowhere (`None`).
    #[tokio::test]
    async fn deepseek_profile_routes_beta_features_without_moving_the_chat_path() {
        const DEEPSEEK_BASE: &str = "https://api.deepseek.com";
        let mut slot = OpenAiCompatLlmSlot::with_profile(
            LlmSlotConfig {
                base_url: DEEPSEEK_BASE.into(),
                model: "deepseek-chat".into(),
            },
            keys_with_key_for(DEEPSEEK_BASE),
            ProviderProfile::DeepSeek,
        );
        slot.install_context(ctx());

        let messages =
            vec![serde_json::json!({"role": "user", "content": "what does figure three show"})];
        let request = slot
            .build_chat_request(&messages)
            .await
            .expect("the deepseek profile builds the chat request");
        assert_eq!(
            request.url().as_str(),
            "https://api.deepseek.com/chat/completions",
            "streaming chat is GA on the normal base: the chat path never \
             routes through /beta"
        );
        assert_eq!(
            slot.request_body(&messages),
            serde_json::json!({
                "model": "deepseek-chat",
                "messages": messages,
                "stream": true,
                "tools": [tool_schema()],
            }),
            "the deepseek profile's body is the unmodified baseline — its \
             whole deviation is path-side"
        );

        let beta = Some("https://api.deepseek.com/beta".to_string());
        for feature in [
            CompatFeature::ChatPrefixCompletion,
            CompatFeature::FimCompletion,
        ] {
            assert_eq!(
                ProviderProfile::DeepSeek.feature_base_url(DEEPSEEK_BASE, feature),
                beta,
                "every Beta feature routes to the /beta base ({feature:?})"
            );
            assert_eq!(
                ProviderProfile::DeepSeek.feature_base_url("https://api.deepseek.com/v1", feature),
                beta,
                "the documented /v1 alias is replaced by /beta, never \
                 suffixed to /v1/beta ({feature:?})"
            );
            assert_eq!(
                ProviderProfile::DeepSeek
                    .feature_base_url("https://api.deepseek.com/beta", feature),
                beta,
                "an already-/beta base passes through ({feature:?})"
            );
            assert_eq!(
                ProviderProfile::Generic.feature_base_url(DEEPSEEK_BASE, feature),
                None,
                "no other profile documents the Beta surface ({feature:?})"
            );
            assert_eq!(
                ProviderProfile::Qwen.feature_base_url(DEEPSEEK_BASE, feature),
                None,
                "no other profile documents the Beta surface ({feature:?})"
            );
        }
    }

    /// The Groq quirk fixture (module header, quirk provenance): the five
    /// documented 400-on-sight fields never reach the wire under the groq
    /// profile — `messages[].name` is stripped on the builder path and a
    /// body carrying `logprobs`/`logit_bias`/`top_logprobs`/`n` loses
    /// exactly those keys through the shaping hook, both pinned as payload
    /// literals — while the default profile's request still includes every
    /// one of them (the negative pin: stripping is groq-only, the baseline
    /// passes bodies through untouched).
    #[tokio::test]
    async fn groq_profile_strips_the_documented_400_fields() {
        let named_messages = vec![serde_json::json!({
            "role": "user",
            "content": "what does figure three show",
            "name": "reader-7",
        })];
        let config = LlmSlotConfig {
            base_url: BASE_URL.into(),
            model: "llama-3.3-70b-versatile".into(),
        };

        let mut groq = OpenAiCompatLlmSlot::with_profile(
            config.clone(),
            keys_with_compat_key(),
            ProviderProfile::Groq,
        );
        groq.install_context(ctx());
        assert_eq!(
            groq.request_body(&named_messages),
            serde_json::json!({
                "model": "llama-3.3-70b-versatile",
                "messages": [{"role": "user", "content": "what does figure three show"}],
                "stream": true,
                "tools": [tool_schema()],
            }),
            "the groq profile strips messages[].name — a documented \
             400-field — from the serialized request"
        );

        let mut default = OpenAiCompatLlmSlot::new(config, keys_with_compat_key());
        default.install_context(ctx());
        assert_eq!(
            default.request_body(&named_messages),
            serde_json::json!({
                "model": "llama-3.3-70b-versatile",
                "messages": named_messages,
                "stream": true,
                "tools": [tool_schema()],
            }),
            "negative pin: the default profile still carries messages[].name"
        );

        // The top-level 400-fields, through the shaping hook directly: the
        // builder never emits them today, so the pin drives the last-exit
        // guarantee a future body knob would rely on.
        let loaded = serde_json::json!({
            "model": "llama-3.3-70b-versatile",
            "messages": [{"role": "user", "content": "hi", "name": "reader-7"}],
            "stream": true,
            "logprobs": true,
            "logit_bias": {"50256": -100},
            "top_logprobs": 3,
            "n": 2,
        });
        let mut shaped = loaded.clone();
        ProviderProfile::Groq.shape_request(&mut shaped);
        assert_eq!(
            shaped,
            serde_json::json!({
                "model": "llama-3.3-70b-versatile",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true,
            }),
            "the groq profile removes exactly the documented 400-fields, \
             top-level and per-message, and nothing else"
        );
        let mut untouched = loaded.clone();
        ProviderProfile::Generic.shape_request(&mut untouched);
        assert_eq!(
            untouched, loaded,
            "negative pin: the default profile still includes every \
             rejected field"
        );
    }

    /// Turn one, scripted per the module header's provenance: two text
    /// deltas, one tool call split across three fragments (id/name on the
    /// first, arguments concatenated across all three), the closing
    /// `finish_reason`, and the `[DONE]` sentinel.
    const SSE_TURN_ONE: &[&str] = &[
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"Figure three "},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"shows a phase diagram."},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-7","type":"function","function":{"name":"get_context_around_position","arguments":"{\"win"}}]},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"dow\":"}}]},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"42}"}}]},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ];

    /// The continuation after the tool answer: one delta, `stop`, `[DONE]`.
    const SSE_TURN_TWO: &[&str] = &[
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"It plots temperature against pressure."},"finish_reason":null}]}"#,
        r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ];

    /// Reads one full HTTP request (head, then `Content-Length` body bytes)
    /// off the socket.
    async fn read_request(sock: &mut TcpStream) -> (String, String) {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = sock.read(&mut buf).await.expect("read request");
            assert!(n > 0, "client closed before a full request arrived");
            raw.extend_from_slice(&buf[..n]);
            let Some(split) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
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
            let body =
                String::from_utf8_lossy(&raw[body_start..body_start + content_length]).to_string();
            return (head, body);
        }
    }

    /// A scripted chat-completions server: one connection per script (the
    /// shared writer's `Connection: close` framing means the pooled client
    /// redials per request), each serving its lines as an SSE stream and
    /// returning the request it saw for the test to assert on.
    fn spawn_chat_server(
        scripts: Vec<&'static [&'static str]>,
    ) -> (String, tokio::task::JoinHandle<Vec<(String, String)>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let server = tokio::spawn(async move {
            let listener = TcpListener::from_std(listener).expect("tokio listener");
            let mut exchanges = Vec::new();
            for script in scripts {
                let (mut sock, _) = listener.accept().await.expect("accept");
                exchanges.push(read_request(&mut sock).await);
                crate::test_support::mock_http::write_head(
                    &mut sock,
                    "200 OK",
                    &[("Content-Type", "text/event-stream")],
                    None,
                )
                .await
                .expect("write head");
                for line in script {
                    sock.write_all(line.as_bytes()).await.expect("write event");
                    sock.write_all(b"\n\n").await.expect("write separator");
                    sock.flush().await.expect("flush");
                }
            }
            exchanges
        });
        (base_url, server)
    }

    fn header_value(head: &str, name: &str) -> Option<String> {
        head.lines().skip(1).find_map(|line| {
            let (field, value) = line.split_once(':')?;
            field
                .trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }

    fn tool_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_context_around_position",
                "description": "Window of book text around the playback position.",
                "parameters": {
                    "type": "object",
                    "properties": { "window": { "type": "integer" } },
                    "required": ["window"],
                    "additionalProperties": false
                }
            }
        })
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
            tool_schemas: vec![tool_schema()],
        }
    }

    async fn recv(events: &mut mpsc::Receiver<ChannelEvent>) -> ChannelEvent {
        timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("an event within the budget")
            .expect("the stream is open")
    }

    /// The full streaming contract against a scripted server: the request is
    /// exactly the rendered history (Bearer header, credential-free URL,
    /// pinned body), the SSE decode surfaces deltas as they stream and the
    /// tool call only once assembled, the tool answer re-POSTs the
    /// stateless-HTTP continuation carrying the committed assistant message
    /// (the module header's turn-complete contract, pinned payload-level),
    /// and turn completion emits no event (the documented choice).
    #[tokio::test]
    async fn a_streamed_turn_surfaces_deltas_assembles_the_tool_call_and_continues_after_the_answer(
    ) {
        let (base_url, server) = spawn_chat_server(vec![SSE_TURN_ONE, SSE_TURN_TWO]);
        let mut slot = OpenAiCompatLlmSlot::new(
            LlmSlotConfig {
                base_url: base_url.clone(),
                model: "llama-3.3-70b-versatile".into(),
            },
            keys_with_key_for(&base_url),
        );

        // The installed-context gate: the slot's "send before connect".
        let early = slot.send_user_turn("too early").await;
        assert!(
            matches!(early, Err(ref err) if err.kind == ErrorKind::InvalidState),
            "send_user_turn before install_context is invalid, got {early:?}"
        );

        slot.install_context(ctx());
        let mut events = slot.take_events().expect("the receiver is handed over");
        assert!(
            slot.take_events().is_none(),
            "take_events yields the receiver exactly once"
        );

        slot.send_user_turn("what does figure three show")
            .await
            .expect("a configured slot sends the turn");

        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "Figure three ".into()
            },
            "content fragments surface as assistant deltas in arrival order"
        );
        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "shows a phase diagram.".into()
            }
        );
        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::ToolCall {
                call_id: "call-7".into(),
                name: "get_context_around_position".into(),
                args: serde_json::json!({ "window": 42 }),
            },
            "the tool call surfaces once, assembled: id/name from the first \
             fragment, arguments concatenated across all three and parsed"
        );

        let bogus = slot
            .send_tool_result("call-999", ToolResult::Ok(serde_json::json!({})))
            .await;
        assert!(
            matches!(bogus, Err(ref err) if err.kind == ErrorKind::InvalidState && !err.retryable),
            "an unsurfaced tool-call id is unanswerable, got {bogus:?}"
        );

        slot.send_tool_result(
            "call-7",
            ToolResult::Ok(serde_json::json!({ "excerpt": "figure three is a phase diagram" })),
        )
        .await
        .expect("the answer continues the turn");
        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "It plots temperature against pressure.".into()
            },
            "the continuation streams on the same event stream"
        );

        // The reply boundary (module header, turn-complete signal): turn
        // one's finish surfaced a pending tool call, so no boundary rode
        // between the ToolCall and its answer (the recv order above pins
        // that); the answered continuation's `stop` finish closes the reply
        // with exactly one TurnComplete, then the stream is silent.
        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::TurnComplete,
            "the tool-free finish closes the reply with the boundary event"
        );
        let exchanges = server.await.expect("the scripted server saw both turns");
        assert!(
            timeout(Duration::from_millis(250), events.recv())
                .await
                .is_err(),
            "one boundary per completed reply, nothing after it"
        );

        let (head_one, body_one) = &exchanges[0];
        let request_line = head_one.lines().next().expect("request line");
        assert_eq!(
            request_line, "POST /chat/completions HTTP/1.1",
            "credential-free URL: base + /chat/completions, nothing else"
        );
        assert_eq!(
            header_value(head_one, "authorization").as_deref(),
            Some(format!("Bearer {FIXTURE_KEY}").as_str()),
            "the credential rides the Authorization header"
        );
        assert!(
            !request_line.contains(FIXTURE_KEY),
            "auth-hygiene-no-query-param-creds: the key never rides the URL"
        );

        let system = serde_json::json!({
            "role": "system",
            "content": "You are the reading tutor.\n<<x>>excerpt<</x>>"
        });
        let user = serde_json::json!({
            "role": "user",
            "content": "what does figure three show"
        });
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body_one).unwrap(),
            serde_json::json!({
                "model": "llama-3.3-70b-versatile",
                "messages": [system, user],
                "stream": true,
                "tools": [tool_schema()],
            }),
            "turn one carries the rendered system message (the shared \
             composition rule), the user turn, stream:true, and the tool \
             schemas verbatim"
        );

        let (_, body_two) = &exchanges[1];
        let assistant = serde_json::json!({
            "role": "assistant",
            "content": "Figure three shows a phase diagram.",
            "tool_calls": [{
                "id": "call-7",
                "type": "function",
                "function": {
                    "name": "get_context_around_position",
                    "arguments": "{\"window\":42}",
                },
            }],
        });
        let tool = serde_json::json!({
            "role": "tool",
            "tool_call_id": "call-7",
            "content": "{\"excerpt\":\"figure three is a phase diagram\"}",
        });
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body_two).unwrap(),
            serde_json::json!({
                "model": "llama-3.3-70b-versatile",
                "messages": [system, user, assistant, tool],
                "stream": true,
                "tools": [tool_schema()],
            }),
            "the continuation re-POSTs the whole conversation: the committed \
             assistant message (accumulated content + assembled tool_calls) \
             and the role:tool answer under its tool_call_id"
        );
    }

    /// F11-U3: the composed LLM slot's system message carries the resume
    /// recap behind the composed framing, after the system prompt and the
    /// excerpt — the same composition rule as the Deepgram think prompt.
    #[test]
    fn resume_recap_rides_the_system_message_behind_the_composed_framing() {
        use crate::voice::channel::types::{RecapOrigin, UiLang};
        let origin = RecapOrigin::Continue;
        let mut parts = PromptParts::new(&ctx());
        parts.apply(ContextUpdate::ResumeRecap {
            recap: DelimitedExcerpt {
                delimited_text: "<<s>>user: hi<</s>>".into(),
            },
            origin: origin.clone(),
            location: None,
            topic: Some("inertia".to_string()),
            lang: UiLang::En,
        });
        let framing = crate::voice::channel::types::resume_recap_framing(
            &origin,
            None,
            Some("inertia"),
            UiLang::En,
        );
        assert_eq!(
            parts.as_system_message()["content"].as_str().unwrap(),
            format!(
                "You are the reading tutor.\n<<x>>excerpt<</x>>\nNote: {framing}\n<<s>>user: hi<</s>>"
            )
        );
    }

    #[tokio::test]
    async fn huge_tool_call_index_is_ignored_without_aborting() {
        const SCRIPT_HUGE_INDEX: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":100000000000,"id":"call-huge","type":"function","function":{"name":"pause_playback","arguments":"{}"}}]},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Safe answer."},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ];
        let (base_url, _server) = spawn_chat_server(vec![SCRIPT_HUGE_INDEX]);
        let mut slot = OpenAiCompatLlmSlot::new(
            LlmSlotConfig {
                base_url: base_url.clone(),
                model: "llama-3.3-70b-versatile".into(),
            },
            keys_with_key_for(&base_url),
        );
        slot.install_context(ctx());
        let mut events = slot.take_events().expect("the receiver is handed over");
        slot.send_user_turn("hello").await.expect("turn sends");
        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "Safe answer.".into()
            }
        );
    }

    #[tokio::test]
    async fn lookahead_exceeded_tool_call_index_is_ignored() {
        const SCRIPT_LOOKAHEAD_EXCEEDED: &[&str] = &[
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":63,"id":"call-63","type":"function","function":{"name":"pause_playback","arguments":"{}"}}]},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Lookahead safe."},"finish_reason":null}]}"#,
            r#"data: {"object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ];
        let (base_url, _server) = spawn_chat_server(vec![SCRIPT_LOOKAHEAD_EXCEEDED]);
        let mut slot = OpenAiCompatLlmSlot::new(
            LlmSlotConfig {
                base_url: base_url.clone(),
                model: "llama-3.3-70b-versatile".into(),
            },
            keys_with_key_for(&base_url),
        );
        slot.install_context(ctx());
        let mut events = slot.take_events().expect("the receiver is handed over");
        slot.send_user_turn("hello").await.expect("turn sends");
        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "Lookahead safe.".into()
            }
        );
    }
}
