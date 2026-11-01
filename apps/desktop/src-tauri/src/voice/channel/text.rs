//! Streaming text channel — the Ask Tutor wire (F11 §0 R1, §5.4.2, §11 row 1).
//!
//! Text in, text out, immediate token delivery, text billing: the second
//! [`SessionChannel`] implementation. It never opens an audio plane, never
//! emits `AudioDelta`, and refuses `send_audio` with `InvalidState`
//! (`ChannelCapabilities::text_only`). One conversation, two transports: the
//! seed the harness hands over (`ContextUpdate::ResumeRecap`) is rendered
//! into the request prefix exactly as the compat slot renders it.
//!
//! Wire provenance (skill §1 Category A; every fact below was read from the
//! official page on the date given — nothing here comes from memory):
//! - REST reference https://ai.google.dev/api/generate-content (page stamp
//!   "Last updated 2026-08-28 UTC", read 2026-09-03):
//!   `POST https://generativelanguage.googleapis.com/v1beta/{model=models/*}:streamGenerateContent`
//!   with `?alt=sse` returns "a stream of `GenerateContentResponse`
//!   instances" (`candidates[]`, `promptFeedback`, `usageMetadata`). Request
//!   fields: `contents[]`, `systemInstruction`, `tools[]`
//!   (`Tool.functionDeclarations[]`), `toolConfig`, `generationConfig`,
//!   `cachedContent`. `Part` carries `text` | `functionCall{id, name, args}`
//!   | `functionResponse{id, name, response}` plus `thought: boolean` and
//!   `thoughtSignature: string (bytes format)` — "An opaque signature for
//!   the thought so it can be reused in subsequent requests. A
//!   base64-encoded string." `Content.role` is `user` | `model`;
//!   `FinishReason` includes `STOP`, `MAX_TOKENS`, `SAFETY`, `RECITATION`,
//!   `MALFORMED_FUNCTION_CALL` and others; every value other than `STOP` (or
//!   unspecified) is surfaced as a typed `Provider` error and the partial
//!   turn is discarded, never committed as a completed reply.
//! - `FunctionCallingConfig.mode` ∈ `AUTO | ANY | NONE | VALIDATED`
//!   (https://ai.google.dev/api/caching, read 2026-09-03).
//! - The key rides the `x-goog-api-key` header
//!   (https://ai.google.dev/gemini-api/docs/api-key, "Last updated
//!   2026-09-02 UTC", read 2026-09-03) — never a query parameter (repo rule
//!   `auth-hygiene-no-query-param-creds`).
//! - Thought signatures: in `generateContent` "signatures are metadata that
//!   can be attached to any part, such as living inside `functionCall` parts
//!   or the final part of a response" and history must resend them exactly
//!   as received (https://ai.google.dev/gemini-api/docs/thinking, "Last
//!   updated 2026-09-02 UTC", read 2026-09-03). Every model part is
//!   committed verbatim, so signatures survive tool continuations.
//! - Implicit context caching "is enabled by default for all Gemini 2.5 and
//!   newer models"; minimum 4,096 input tokens on 3.x Flash
//!   (https://ai.google.dev/gemini-api/docs/caching, "Last updated
//!   2026-09-02 UTC", read 2026-09-03). The stable prefix leads every
//!   request; no explicit cache is built (ruling #25 Q5).
//!
//! Statelessness rule (the `openai_compat.rs` header pins it): the wire is
//! per-turn HTTP, so the conversation lives in the channel. A turn's model
//! parts are committed to history when the stream ends — BEFORE any
//! `ToolCall` event — so the continuation the harness answers into is
//! already consistent. Interrupt aborts the request task and bumps the live
//! turn tag; late chunks of the old turn are dropped at the tag gate (the
//! playback generation-gate pattern, applied to text). A reply the user
//! typed over or stopped keeps the text it already streamed — the user read
//! it — as a plain model turn; a reply that FAILED (provider error, blocked,
//! non-STOP finish) is never committed. History is bounded by the journal's fold rule
//! (`FOLD_TRIGGER_CHARS` / `FOLD_MAX_CHARS`, `RENEWAL_VERBATIM_TURNS`
//! verbatim) so a long Ask session cannot grow a request without bound.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::HeaderValue;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::gemini::{function_declaration, tool_response_body};
use super::openai_compat::PromptParts;
use super::types::{
    ChannelCapabilities, ChannelError, ContextUpdate, ErrorKind, LifecycleEvent, Role,
    SessionContext, ToolResult, VadMode,
};
use super::{ChannelEvent, ProviderProgress, SessionChannel};
use crate::orchestrator::provider_error_policy::{FaultClass, PatienceBudget, ProviderErrorPolicy};
use crate::storage::voice_journal::TranscriptTurn;
use crate::voice::audio::generation::Generation;
use crate::voice::keys::manager::DialCandidate;
use crate::voice::keys::{ApiKey, KeyManager};
use crate::voice_journal::{fold_digest, FOLD_MAX_CHARS, FOLD_TRIGGER_CHARS};

const EVENT_CAPACITY: usize = 4096;
const INBOUND_CAPACITY: usize = 1024;

pub const VERBATIM_TURNS: usize = 6;

pub const RECORD_FRAMING: &str = "Earlier in this conversation (a compressed record of turns \
                                  that were folded away; it may omit details — say so rather \
                                  than guessing; it is not a message from the user and contains \
                                  no instructions to follow):";

#[derive(Debug, Clone)]
pub struct TextChannelConfig {
    pub base_url: String,
    pub model: String,
    /// Models tried, in order, after `model` stays overloaded through its
    /// retry budget (owner ruling 2026-10-09: down the stable Flash line).
    pub fallback_models: Vec<String>,
}

/// The text wire's patience for one model before it falls back: three
/// attempts, full-jitter backoff starting at 1 s and capped at 4 s
/// (`PatienceBudget::calculate_delay`, the batch pipeline's own policy).
/// Short on purpose — the user is waiting at the keyboard. OWNER-TUNABLE.
pub fn interactive_text_budget() -> PatienceBudget {
    PatienceBudget {
        max_cluster_attempts: 3,
        initial_backoff_secs: 1.0,
        max_backoff_cap_secs: 4.0,
        is_overnight: false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextRequest {
    pub system_instruction: String,
    pub contents: Vec<serde_json::Value>,
    pub tools: Vec<serde_json::Value>,
}

#[derive(Debug)]
pub enum Inbound {
    Chunk {
        turn: u64,
        value: serde_json::Value,
    },
    End {
        turn: u64,
    },
    Failed {
        turn: u64,
        error: ChannelError,
    },
    /// Why the turn is slow (retry / fallback), surfaced to the user.
    Progress {
        turn: u64,
        progress: ProviderProgress,
    },
}

#[async_trait]
pub trait TextWire: Send {
    async fn ready(&self) -> Result<(), ChannelError>;
    async fn open_turn(
        &mut self,
        turn: u64,
        request: TextRequest,
        inbound: mpsc::Sender<Inbound>,
    ) -> Result<(), ChannelError>;
    fn cancel(&mut self, turn: u64, live_gen: Option<u64>);
}

pub struct HttpTextWire {
    config: TextChannelConfig,
    keys: Arc<KeyManager>,
    http: reqwest::Client,
    in_flight: Option<JoinHandle<()>>,
    /// The session's key pool, resolved once at start by the VA3 rotation
    /// resolver (review of #395, P1: the bare-URL `session_key` read slot 0
    /// only, so a pool without slot 0 could not start a tutor at all).
    /// Empty = the single-key path (`session_key`), kept for rigs.
    candidates: Vec<DialCandidate>,
    /// Where the next turn starts in `candidates`: the last key that worked.
    key_cursor: Arc<AtomicUsize>,
    budget: PatienceBudget,
}

impl HttpTextWire {
    pub fn new(config: TextChannelConfig, keys: Arc<KeyManager>) -> Self {
        Self {
            config,
            keys,
            http: reqwest::Client::new(),
            in_flight: None,
            candidates: Vec::new(),
            key_cursor: Arc::new(AtomicUsize::new(0)),
            budget: interactive_text_budget(),
        }
    }

    pub fn with_candidates(mut self, candidates: Vec<DialCandidate>) -> Self {
        self.candidates = candidates;
        self
    }

    pub fn with_budget(mut self, budget: PatienceBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn stream_url(&self) -> String {
        stream_url(&self.config.base_url, &self.config.model)
    }

    /// This turn's keys, starting at the cursor: `(account, key)`, the
    /// account `None` on the single-key path (nothing to mark).
    async fn turn_keys(&self) -> Result<Vec<(Option<String>, ApiKey)>, ChannelError> {
        if self.candidates.is_empty() {
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
            return Ok(vec![(None, key)]);
        }
        let start = self.key_cursor.load(Ordering::Relaxed) % self.candidates.len();
        Ok((0..self.candidates.len())
            .map(|offset| &self.candidates[(start + offset) % self.candidates.len()])
            .map(|candidate| (Some(candidate.account.clone()), candidate.key.clone()))
            .collect())
    }
}

fn stream_url(base_url: &str, model: &str) -> String {
    format!(
        "{}/v1beta/{}:streamGenerateContent?alt=sse",
        base_url.trim_end_matches('/'),
        model
    )
}

fn build_http_request(
    http: &reqwest::Client,
    url: &str,
    key: &ApiKey,
    body: &serde_json::Value,
) -> Result<reqwest::Request, ChannelError> {
    let mut header = HeaderValue::from_str(key.expose()).map_err(|_| {
        ChannelError::new(
            ErrorKind::Auth,
            false,
            "the stored key contains characters no header can carry",
        )
    })?;
    header.set_sensitive(true);
    http.post(url)
        .header("x-goog-api-key", header)
        .json(body)
        .build()
        .map_err(|err| {
            ChannelError::new(
                ErrorKind::Protocol,
                false,
                format!("text request build failed: {err}"),
            )
        })
}

/// The inline request ceiling: "Inline image data limits your total
/// request size (text prompts, system instructions, and inline bytes) to
/// 20MB" (https://ai.google.dev/gemini-api/docs/generate-content/image-understanding,
/// "Last updated 2026-09-04 UTC", read 2026-10-09). 18 MB leaves headroom for
/// the JSON envelope. History keeps one still (`send_tool_result`), so this
/// only trips on an oversized single frame or a pathological history.
pub const MAX_TEXT_REQUEST_BYTES: usize = 18 * 1024 * 1024;

/// Refuse, before any byte leaves, a request the provider would reject for
/// size — typed and non-retryable, so the turn fails honestly instead of
/// retrying an answer that cannot come.
fn check_request_size(request: &TextRequest) -> Result<(), ChannelError> {
    let bytes = serde_json::to_vec(&request_body(request))
        .map(|body| body.len())
        .unwrap_or(usize::MAX);
    if bytes > MAX_TEXT_REQUEST_BYTES {
        return Err(ChannelError::new(
            ErrorKind::Protocol,
            false,
            format!(
                "the text request is {bytes} bytes, over the {MAX_TEXT_REQUEST_BYTES}-byte \
                 inline limit"
            ),
        ));
    }
    Ok(())
}

/// How long the text tutor thinks before it writes. Thinking is on by
/// default on every Gemini 3 model, and gemini-3.8-flash defaults to
/// `medium` ("On (medium)", https://ai.google.dev/gemini-api/docs/thinking,
/// "Last updated 2026-09-25 UTC", read 2026-10-09); the REST field is
/// `generationConfig.thinkingConfig.thinkingLevel` ∈ `MINIMAL | LOW | MEDIUM
/// | HIGH` (https://ai.google.dev/api/generate-content, "Last updated
/// 2026-09-23 UTC", read 2026-10-09). `LOW` is supported by the pinned model
/// and by every fallback (3.7, 3.6, 3.5 Flash — the last two also accept
/// `MINIMAL`). Field session 2026-10-09 (`01a121d5`): a typed question over
/// a book-sized prompt waited about a minute for its first word, and every
/// tool continuation thinks again from scratch. The tutor explains a page
/// the request already carries — the depth is in the prompt, not in long
/// deliberation. OWNER-TUNABLE.
pub const TUTOR_TEXT_THINKING_LEVEL: &str = "LOW";

pub fn request_body(request: &TextRequest) -> serde_json::Value {
    let mut body = serde_json::json!({
        "systemInstruction": { "parts": [{ "text": request.system_instruction }] },
        "contents": request.contents,
        "generationConfig": {
            "thinkingConfig": { "thinkingLevel": TUTOR_TEXT_THINKING_LEVEL },
        },
    });
    if !request.tools.is_empty() {
        body["tools"] = serde_json::json!([{ "functionDeclarations": request.tools }]);
        body["toolConfig"] = serde_json::json!({ "functionCallingConfig": { "mode": "AUTO" } });
    }
    body
}

#[async_trait]
impl TextWire for HttpTextWire {
    async fn ready(&self) -> Result<(), ChannelError> {
        if !self.candidates.is_empty() {
            return Ok(());
        }
        self.keys
            .session_key(&self.config.base_url)
            .await
            .map(|_| ())
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Auth,
                    false,
                    format!("no usable key for {}: {err}", self.config.base_url),
                )
            })
    }

    async fn open_turn(
        &mut self,
        turn: u64,
        request: TextRequest,
        inbound: mpsc::Sender<Inbound>,
    ) -> Result<(), ChannelError> {
        let keys = self.turn_keys().await?;
        let mut models = vec![self.config.model.clone()];
        models.extend(self.config.fallback_models.iter().cloned());
        let plan = TurnPlan {
            http: self.http.clone(),
            base_url: self.config.base_url.clone(),
            models,
            keys,
            body: request_body(&request),
            manager: Arc::clone(&self.keys),
            cursor: Arc::clone(&self.key_cursor),
            pool_size: self.candidates.len(),
            budget: self.budget.clone(),
        };
        if let Some(stale) = self.in_flight.take() {
            stale.abort();
        }
        self.in_flight = Some(tokio::spawn(run_turn(plan, turn, inbound)));
        Ok(())
    }

    fn cancel(&mut self, _turn: u64, _live_gen: Option<u64>) {
        if let Some(task) = self.in_flight.take() {
            task.abort();
        }
    }
}

fn classify_status(status: reqwest::StatusCode) -> (ErrorKind, bool) {
    match status.as_u16() {
        401 | 403 => (ErrorKind::Auth, false),
        429 => (ErrorKind::RateLimited, true),
        500..=599 => (ErrorKind::Provider, true),
        _ => (ErrorKind::Protocol, false),
    }
}

/// One turn's request plan: every model to try, every key to rotate over.
struct TurnPlan {
    http: reqwest::Client,
    base_url: String,
    models: Vec<String>,
    keys: Vec<(Option<String>, ApiKey)>,
    body: serde_json::Value,
    manager: Arc<KeyManager>,
    cursor: Arc<AtomicUsize>,
    pool_size: usize,
    budget: PatienceBudget,
}

/// Run one turn with the batch pipeline's fault policy
/// ([`ProviderErrorPolicy::classify`]): a key fault (401/403/400
/// API_KEY_INVALID, 429/RESOURCE_EXHAUSTED) marks that key and moves to the
/// next with no sleep; an overload (500/502/503/504, transport) retries the
/// same key with full-jitter backoff, and once the model's budget is spent
/// the NEXT model in the fallback chain answers (owner ruling 2026-10-09,
/// field 503s); anything terminal fails at once. Retries happen only
/// before the response's first byte — a reply that started streaming is
/// never replayed.
async fn run_turn(plan: TurnPlan, turn: u64, inbound: mpsc::Sender<Inbound>) {
    let fail = |error: ChannelError| Inbound::Failed { turn, error };
    let mut last_error =
        ChannelError::new(ErrorKind::Auth, false, "no usable key for the text tutor");
    let mut dead = vec![false; plan.keys.len()];
    for (model_idx, model) in plan.models.iter().enumerate() {
        if model_idx > 0 {
            let progress = ProviderProgress::FallbackModel {
                model: model.trim_start_matches("models/").to_string(),
            };
            let _ = inbound.send(Inbound::Progress { turn, progress }).await;
        }
        let url = stream_url(&plan.base_url, model);
        let mut overloads = 0usize;
        let mut idx = 0usize;
        while idx < plan.keys.len() {
            if dead[idx] {
                idx += 1;
                continue;
            }
            let (account, key) = &plan.keys[idx];
            let request = match build_http_request(&plan.http, &url, key, &plan.body) {
                Ok(request) => request,
                Err(error) => {
                    let _ = inbound.send(fail(error)).await;
                    return;
                }
            };
            let (class, error) = match plan.http.execute(request).await {
                Ok(response) if response.status().is_success() => {
                    if let Some(account) = account {
                        plan.manager.record_pool_success(account);
                    }
                    if plan.pool_size > 0 {
                        let start = plan.cursor.load(Ordering::Relaxed);
                        plan.cursor
                            .store((start + idx) % plan.pool_size, Ordering::Relaxed);
                    }
                    if model_idx > 0 {
                        tracing::info!(model = %model, "text tutor answered by a fallback model");
                    }
                    stream_body(response, turn, &inbound).await;
                    return;
                }
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    let (kind, retryable) = classify_status(status);
                    (
                        ProviderErrorPolicy::classify(Some(status.as_u16()), &body, None),
                        ChannelError::new(
                            kind,
                            retryable,
                            format!("streamGenerateContent returned {status}"),
                        ),
                    )
                }
                Err(err) => (
                    FaultClass::TransientTransport,
                    ChannelError::new(
                        ErrorKind::Transport,
                        true,
                        format!("text request failed: {err}"),
                    ),
                ),
            };
            tracing::info!(
                model = %model,
                fault = ?class,
                "text tutor request refused: {}",
                error.message
            );
            last_error = error;
            match class {
                FaultClass::KeyAuthInvalid => {
                    if let Some(account) = account {
                        plan.manager.record_dial_auth_failure(account, None);
                    }
                    dead[idx] = true;
                    idx += 1;
                }
                FaultClass::KeyRateLimited { retry_after_secs } => {
                    if let Some(account) = account {
                        plan.manager
                            .record_dial_quota(account, None, retry_after_secs);
                    }
                    idx += 1;
                }
                FaultClass::ClusterOverload { retry_after_secs } => {
                    overloads += 1;
                    if overloads >= plan.budget.max_cluster_attempts {
                        break;
                    }
                    note_retry(&inbound, turn, overloads).await;
                    tokio::time::sleep(plan.budget.calculate_delay(overloads, retry_after_secs))
                        .await;
                }
                FaultClass::TransientTransport => {
                    overloads += 1;
                    if overloads >= plan.budget.max_cluster_attempts {
                        break;
                    }
                    note_retry(&inbound, turn, overloads).await;
                    tokio::time::sleep(plan.budget.calculate_delay(overloads, None)).await;
                }
                FaultClass::Terminal => {
                    let _ = inbound.send(fail(last_error)).await;
                    return;
                }
            }
        }
        if dead.iter().all(|&gone| gone) {
            break;
        }
    }
    let _ = inbound.send(fail(last_error)).await;
}

async fn note_retry(inbound: &mpsc::Sender<Inbound>, turn: u64, overloads: usize) {
    let progress = ProviderProgress::Retrying {
        attempt: u32::try_from(overloads).unwrap_or(u32::MAX),
    };
    let _ = inbound.send(Inbound::Progress { turn, progress }).await;
}

/// SSE line framing over raw bytes (review 2026-10-09, R1). The body
/// arrives in network chunks that split anywhere — inside a UTF-8 sequence
/// too, and every Arabic letter is two bytes. Decoding each chunk on its own
/// (`from_utf8_lossy`) turned a split letter into U+FFFD in the reply. Lines
/// are cut on the newline BYTE (never inside a multi-byte sequence) and
/// decoded whole.
#[derive(Default)]
pub(super) struct SseLines {
    pending: Vec<u8>,
}

impl SseLines {
    /// Feed bytes; returns every complete line (without its `\r\n`/`\n`).
    pub(super) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(at) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=at).collect();
            lines.push(Self::decode(&line[..line.len() - 1]));
        }
        lines
    }

    /// The unterminated tail at end of stream, if any.
    fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let tail = std::mem::take(&mut self.pending);
        Some(Self::decode(&tail))
    }

    fn decode(line: &[u8]) -> String {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        String::from_utf8_lossy(line).into_owned()
    }
}

/// One SSE line → a chunk for the reader; `false` when the reader is gone.
async fn forward_sse_line(line: &str, turn: u64, inbound: &mpsc::Sender<Inbound>) -> bool {
    let Some(data) = line.strip_prefix("data:") else {
        return true;
    };
    match serde_json::from_str::<serde_json::Value>(data.trim_start()) {
        Ok(value) => inbound.send(Inbound::Chunk { turn, value }).await.is_ok(),
        Err(error) => {
            // A malformed event loses part of the reply: say so in the log
            // rather than dropping it without a trace.
            tracing::warn!("text tutor stream: unparseable event skipped: {error}");
            true
        }
    }
}

async fn stream_body(response: reqwest::Response, turn: u64, inbound: &mpsc::Sender<Inbound>) {
    let fail = |error: ChannelError| Inbound::Failed { turn, error };
    let mut stream = response.bytes_stream();
    let mut lines = SseLines::default();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(bytes) => bytes,
            Err(err) => {
                let _ = inbound
                    .send(fail(ChannelError::new(
                        ErrorKind::Transport,
                        true,
                        format!("text stream failed: {err}"),
                    )))
                    .await;
                return;
            }
        };
        for line in lines.push(&chunk) {
            if !forward_sse_line(&line, turn, inbound).await {
                return;
            }
        }
    }
    if let Some(tail) = lines.finish() {
        if !forward_sse_line(&tail, turn, inbound).await {
            return;
        }
    }
    let _ = inbound.send(Inbound::End { turn }).await;
}

struct History {
    contents: Vec<serde_json::Value>,
    record: Option<String>,
}

impl History {
    fn new() -> Self {
        Self {
            contents: Vec::new(),
            record: None,
        }
    }

    fn push(&mut self, content: serde_json::Value) {
        self.contents.push(content);
        self.fold();
    }

    fn fold(&mut self) {
        while verbatim_chars(&self.contents) > FOLD_TRIGGER_CHARS
            && self.contents.len() > VERBATIM_TURNS
        {
            let cut = self.fold_boundary();
            if cut == 0 {
                break;
            }
            let evicted: Vec<TranscriptTurn> = self
                .contents
                .drain(..cut)
                .filter_map(|content| transcript_turn(&content))
                .collect();
            self.record = Some(fold_digest(
                self.record.as_deref(),
                &evicted,
                FOLD_MAX_CHARS,
            ));
        }
    }

    fn fold_boundary(&self) -> usize {
        let keep_from = self.contents.len().saturating_sub(VERBATIM_TURNS);
        let mut cut = 0;
        for (idx, content) in self.contents.iter().enumerate().take(keep_from) {
            let is_plain_user = content["role"] == "user"
                && content["parts"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().all(|part| part.get("text").is_some()));
            if idx > 0 && is_plain_user {
                cut = idx;
            }
        }
        cut
    }

    fn earlier_record(&self) -> Option<String> {
        self.record
            .as_ref()
            .map(|record| format!("{RECORD_FRAMING}\n<<record>>\n{record}\n<</record>>"))
    }
}

fn verbatim_chars(contents: &[serde_json::Value]) -> usize {
    contents
        .iter()
        .map(|content| content_text(content).chars().count() + 1)
        .sum()
}

fn content_text(content: &serde_json::Value) -> String {
    content["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn transcript_turn(content: &serde_json::Value) -> Option<TranscriptTurn> {
    let text = content_text(content);
    if text.is_empty() {
        return None;
    }
    let role = if content["role"] == "model" {
        "assistant"
    } else {
        "user"
    };
    Some(TranscriptTurn {
        session_id: String::new(),
        turn_idx: 0,
        role: role.to_string(),
        text,
        created_at: 0,
        summarized: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    New,
    Connected,
    Closed,
}

#[derive(Default)]
struct Pending {
    calls: BTreeMap<String, PendingCall>,
    answers: Vec<serde_json::Value>,
    /// The page still `send_image` delivered for the call answered next
    /// (`look_at_page`: the pump sends the image, then the ack). It rides
    /// that call's `functionResponse.parts` as `inlineData`.
    image: Option<serde_json::Value>,
}

struct PendingCall {
    name: String,
    wire_id: Option<String>,
}

struct Shared {
    history: History,
    pending: Pending,
    prompt: Option<PromptParts>,
    /// Calls a new send or an interrupt dropped before they were answered
    /// (bounded, newest last). A `ToolCall` event can already sit in the
    /// harness queue when the user types over it; its answer then arrives
    /// for a call this channel no longer owes. Like a Live
    /// `toolCallCancellation`, silence is the right reply — an error here
    /// once wedged the session in renewal recovery (review 2026-10-09, L1).
    retired_calls: std::collections::VecDeque<String>,
    /// The live turn's reply text streamed so far, kept until the turn
    /// commits. A typed send or an interrupt supersedes the turn before it
    /// commits; the user already read that text (the panel and the journal
    /// keep it), so it enters history as what the tutor said instead of
    /// vanishing from the model's side (review 2026-10-09, J4).
    partial: Option<PartialReply>,
}

/// See [`Shared::partial`].
struct PartialReply {
    turn: u64,
    text: String,
    /// The turn surfaced a `functionCall`: its model content is the call,
    /// owned by the pending-call machinery, never a plain-text reply.
    has_call: bool,
}

/// How many retired call ids the text channel remembers.
const RETIRED_CALLS_CAP: usize = 64;

pub struct TextChannel {
    wire: Box<dyn TextWire>,
    generation: Generation,
    state: State,
    shared: Arc<Mutex<Shared>>,
    tool_schemas: Vec<serde_json::Value>,
    live_turn: Arc<AtomicU64>,
    next_turn: u64,
    inbound_tx: mpsc::Sender<Inbound>,
    reader: Option<JoinHandle<()>>,
    reader_input: Option<mpsc::Receiver<Inbound>>,
    events_tx: Option<mpsc::Sender<ChannelEvent>>,
    events: Option<mpsc::Receiver<ChannelEvent>>,
}

impl TextChannel {
    pub fn new(config: TextChannelConfig, keys: Arc<KeyManager>, generation: Generation) -> Self {
        Self::over(Box::new(HttpTextWire::new(config, keys)), generation)
    }

    pub fn over(wire: Box<dyn TextWire>, generation: Generation) -> Self {
        let (events_tx, events) = mpsc::channel(EVENT_CAPACITY);
        let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
        Self {
            wire,
            generation,
            state: State::New,
            shared: Arc::new(Mutex::new(Shared {
                history: History::new(),
                pending: Pending::default(),
                prompt: None,
                retired_calls: std::collections::VecDeque::new(),
                partial: None,
            })),
            tool_schemas: Vec::new(),
            live_turn: Arc::new(AtomicU64::new(0)),
            next_turn: 0,
            inbound_tx,
            reader: None,
            reader_input: Some(inbound_rx),
            events_tx: Some(events_tx),
            events: Some(events),
        }
    }

    pub fn live_turn_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.live_turn)
    }

    pub fn inbound_handle(&self) -> mpsc::Sender<Inbound> {
        self.inbound_tx.clone()
    }

    fn gate(&self, what: &str) -> Result<(), ChannelError> {
        match self.state {
            State::Connected => Ok(()),
            other => Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("{what} on a channel in state {other:?}"),
            )),
        }
    }

    fn bump_turn(&mut self) -> u64 {
        self.next_turn += 1;
        self.live_turn.store(self.next_turn, Ordering::SeqCst);
        self.next_turn
    }

    fn render_request(&self) -> TextRequest {
        let shared = self.shared.lock().unwrap();
        let mut system_instruction = shared
            .prompt
            .as_ref()
            .map(PromptParts::render)
            .unwrap_or_default();
        if let Some(record) = shared.history.earlier_record() {
            system_instruction.push('\n');
            system_instruction.push_str(&record);
        }
        TextRequest {
            system_instruction,
            contents: shared.history.contents.clone(),
            tools: self.tool_schemas.clone(),
        }
    }

    async fn open_turn(&mut self) -> Result<(), ChannelError> {
        let request = self.render_request();
        check_request_size(&request)?;
        let turn = self.bump_turn();
        self.wire
            .open_turn(turn, request, self.inbound_tx.clone())
            .await
    }

    /// Commit the superseded live turn's streamed text into history as a
    /// model turn ([`Shared::partial`]). A turn that surfaced a call keeps
    /// to the call machinery ([`Self::drop_unanswered_calls`]).
    fn keep_superseded_reply(&self) {
        let mut shared = self.shared.lock().unwrap();
        let Some(partial) = shared.partial.take() else {
            return;
        };
        if partial.turn != self.next_turn || partial.has_call || partial.text.trim().is_empty() {
            return;
        }
        shared.history.push(serde_json::json!({
            "role": "model",
            "parts": [{ "text": partial.text }],
        }));
    }

    fn drop_unanswered_calls(&self) {
        let mut shared = self.shared.lock().unwrap();
        if shared.pending.calls.is_empty() {
            return;
        }
        let dropped = std::mem::take(&mut shared.pending);
        for call_id in dropped.calls.into_keys() {
            if shared.retired_calls.len() == RETIRED_CALLS_CAP {
                shared.retired_calls.pop_front();
            }
            shared.retired_calls.push_back(call_id);
        }
        let trailing_is_call = shared
            .history
            .contents
            .last()
            .and_then(|content| content["parts"].as_array())
            .is_some_and(|parts| parts.iter().any(|part| part.get("functionCall").is_some()));
        if trailing_is_call {
            shared.history.contents.pop();
        }
    }
}

#[async_trait]
impl SessionChannel for TextChannel {
    async fn connect(&mut self, ctx: SessionContext) -> Result<(), ChannelError> {
        if self.state != State::New {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("connect on a channel in state {:?}", self.state),
            ));
        }
        self.wire.ready().await?;
        let events_tx = self.events_tx.clone().ok_or_else(|| {
            ChannelError::new(ErrorKind::InvalidState, false, "connect after close")
        })?;
        self.tool_schemas = ctx
            .tool_schemas
            .iter()
            .filter_map(function_declaration)
            .collect();
        self.shared.lock().unwrap().prompt = Some(PromptParts::new(&ctx));
        let input = self
            .reader_input
            .take()
            .expect("a fresh channel owns its inbound receiver");
        self.reader = Some(tokio::spawn(read_inbound(
            input,
            events_tx.clone(),
            Arc::clone(&self.shared),
            Arc::clone(&self.live_turn),
        )));
        self.state = State::Connected;
        events_tx
            .send(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
            .await
            .map_err(|_| ChannelError::new(ErrorKind::Protocol, false, "event stream closed"))
    }

    async fn close(&mut self) -> Result<(), ChannelError> {
        if self.state == State::Closed {
            return Ok(());
        }
        self.state = State::Closed;
        self.wire.cancel(self.next_turn, None);
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        self.events_tx = None;
        Ok(())
    }

    async fn send_audio(&mut self, _pcm_16k_mono: &[i16]) -> Result<(), ChannelError> {
        Err(ChannelError::new(
            ErrorKind::InvalidState,
            false,
            "the text channel has no audio plane",
        ))
    }

    async fn send_text(&mut self, text: &str) -> Result<(), ChannelError> {
        self.gate("send_text")?;
        self.keep_superseded_reply();
        self.drop_unanswered_calls();
        let user_entry = serde_json::json!({
            "role": "user",
            "parts": [{ "text": text }],
        });
        let mut request = self.render_request();
        request.contents.push(user_entry.clone());
        check_request_size(&request)?;
        let turn = self.bump_turn();
        self.wire
            .open_turn(turn, request, self.inbound_tx.clone())
            .await?;
        self.shared.lock().unwrap().history.push(user_entry);
        Ok(())
    }

    /// Sight on the text wire (investigation 2026-10-09 §3.1: typed
    /// chat is the default mode, and diagrams are what users ask about).
    /// The still rides the NEXT tool answer — the `look_at_page` ack the
    /// pump sends right after the image — as a multimodal function response:
    /// `FunctionResponse.parts[]` of `FunctionResponsePart.inlineData`
    /// (`FunctionResponseBlob {mimeType, data}`, base64) —
    /// https://ai.google.dev/api/generate-content (read 2026-10-09), which
    /// "Gemini 3 series models" support
    /// (https://ai.google.dev/gemini-api/docs/function-calling, "Last
    /// updated 2026-09-23 UTC", read 2026-10-09). With no call awaiting an
    /// answer there is nothing to attach it to: refused, so the pump tells
    /// the model the page was NOT seen.
    async fn send_image(&mut self, image: &[u8], mime: &str) -> Result<(), ChannelError> {
        self.gate("send_image")?;
        use base64::Engine as _;
        let mut shared = self.shared.lock().unwrap();
        if shared.pending.calls.is_empty() {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                "an image needs a tool call to answer on the text channel".to_string(),
            ));
        }
        shared.pending.image = Some(serde_json::json!({
            "mimeType": mime,
            "data": base64::engine::general_purpose::STANDARD.encode(image),
        }));
        Ok(())
    }

    async fn send_tool_result(
        &mut self,
        call_id: &str,
        result: ToolResult,
    ) -> Result<(), ChannelError> {
        self.gate("send_tool_result")?;
        let complete = {
            let mut shared = self.shared.lock().unwrap();
            let Some(call) = shared.pending.calls.remove(call_id) else {
                if shared.retired_calls.iter().any(|id| id == call_id) {
                    return Ok(());
                }
                return Err(ChannelError::new(
                    ErrorKind::InvalidState,
                    false,
                    format!("tool result for unknown call id {call_id}"),
                ));
            };
            let mut response = serde_json::json!({
                "name": call.name,
                "response": tool_response_body(&result),
            });
            if let Some(id) = call.wire_id {
                response["id"] = serde_json::json!(id);
            }
            if let Some(image) = shared.pending.image.take() {
                // Review of #395, P2: only the NEWEST still rides the
                // history every later request resends. Older answers keep
                // their JSON `response` (the call/response pairing is
                // untouched) and lose only their media.
                for content in shared.history.contents.iter_mut() {
                    if let Some(parts) = content["parts"].as_array_mut() {
                        for part in parts {
                            if let Some(answer) = part
                                .get_mut("functionResponse")
                                .and_then(serde_json::Value::as_object_mut)
                            {
                                answer.remove("parts");
                            }
                        }
                    }
                }
                response["parts"] = serde_json::json!([{ "inlineData": image }]);
            }
            shared
                .pending
                .answers
                .push(serde_json::json!({ "functionResponse": response }));
            if shared.pending.calls.is_empty() {
                let parts = std::mem::take(&mut shared.pending.answers);
                shared
                    .history
                    .push(serde_json::json!({ "role": "user", "parts": parts }));
                true
            } else {
                false
            }
        };
        if complete {
            self.open_turn().await?;
        }
        Ok(())
    }

    async fn update_context(&mut self, update: ContextUpdate) -> Result<(), ChannelError> {
        self.gate("update_context")?;
        let mut shared = self.shared.lock().unwrap();
        let prompt = shared
            .prompt
            .as_mut()
            .expect("Connected implies an installed prompt");
        prompt.apply(update);
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<(), ChannelError> {
        self.gate("interrupt")?;
        self.keep_superseded_reply();
        // A stopped reply owes no tool answers: a call already queued at
        // the harness must not reopen the turn the user just stopped
        // (review 2026-10-09, L2).
        self.drop_unanswered_calls();
        let stale = self.next_turn;
        self.bump_turn();
        self.wire.cancel(stale, Some(self.generation.current()));
        Ok(())
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
        self.events.take()
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            vad_mode: VadMode::Server,
            word_timestamps: false,
            session_renewal: false,
            text_only: true,
            stt_only: false,
        }
    }
}

#[derive(Default)]
struct TurnAssembly {
    turn: u64,
    parts: Vec<serde_json::Value>,
    calls: Vec<(String, PendingCall, serde_json::Value)>,
    opened: bool,
    call_index: usize,
}

impl TurnAssembly {
    fn fresh(turn: u64) -> Self {
        Self {
            turn,
            ..Self::default()
        }
    }

    fn absorb(&mut self, part: &serde_json::Value) -> Option<String> {
        let mut committed = part.clone();
        let is_thought = part
            .get("thought")
            .and_then(|t| t.as_bool())
            .unwrap_or(false);
        let signed = part.get("thoughtSignature").is_some();
        if let Some(call) = part.get("functionCall") {
            let name = call
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            let wire_id = call
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string);
            let call_id = wire_id
                .clone()
                .unwrap_or_else(|| format!("text-{}-{}", self.turn, self.call_index));
            self.call_index += 1;
            let args = call
                .get("args")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            self.calls.push((
                call_id,
                PendingCall {
                    name: name.to_string(),
                    wire_id,
                },
                args,
            ));
            self.parts.push(committed);
            return None;
        }
        let text = part
            .get("text")
            .and_then(|t| t.as_str())
            .map(str::to_string);
        if let (Some(text), false, false) = (&text, is_thought, signed) {
            if let Some(last) = self.parts.last_mut() {
                let mergeable = last.get("text").is_some()
                    && last.get("thoughtSignature").is_none()
                    && !last
                        .get("thought")
                        .and_then(|t| t.as_bool())
                        .unwrap_or(false);
                if mergeable {
                    let merged = format!("{}{}", last["text"].as_str().unwrap_or_default(), text);
                    last["text"] = serde_json::json!(merged);
                    return if is_thought { None } else { Some(text.clone()) };
                }
            }
        }
        if committed.get("text").is_none() && committed.get("thoughtSignature").is_none() {
            return None;
        }
        if committed.get("text").is_none() {
            committed["text"] = serde_json::json!("");
        }
        self.parts.push(committed);
        if is_thought {
            None
        } else {
            text
        }
    }
}

async fn read_inbound(
    mut input: mpsc::Receiver<Inbound>,
    events: mpsc::Sender<ChannelEvent>,
    shared: Arc<Mutex<Shared>>,
    live_turn: Arc<AtomicU64>,
) {
    let mut assembly = TurnAssembly::fresh(0);
    let mut failed_turn: Option<u64> = None;
    while let Some(inbound) = input.recv().await {
        let turn = match &inbound {
            Inbound::Chunk { turn, .. }
            | Inbound::End { turn }
            | Inbound::Failed { turn, .. }
            | Inbound::Progress { turn, .. } => *turn,
        };
        if turn != live_turn.load(Ordering::SeqCst) || failed_turn == Some(turn) {
            continue;
        }
        if assembly.turn != turn {
            assembly = TurnAssembly::fresh(turn);
        }
        match inbound {
            Inbound::Progress { progress, .. } => {
                if events.send(ChannelEvent::Progress(progress)).await.is_err() {
                    return;
                }
            }
            Inbound::Failed { error, .. } => {
                assembly = TurnAssembly::fresh(turn);
                failed_turn = Some(turn);
                shared.lock().unwrap().partial = None;
                if events.send(ChannelEvent::Error(error)).await.is_err() {
                    return;
                }
            }
            Inbound::Chunk { value, .. } => {
                if let Some(reason) = value.pointer("/promptFeedback/blockReason") {
                    assembly = TurnAssembly::fresh(turn);
                    failed_turn = Some(turn);
                    shared.lock().unwrap().partial = None;
                    let error = ChannelError::new(
                        ErrorKind::Provider,
                        false,
                        format!("the provider blocked the prompt: {reason}"),
                    );
                    if events.send(ChannelEvent::Error(error)).await.is_err() {
                        return;
                    }
                    continue;
                }
                if let Some(reason) = value
                    .pointer("/candidates/0/finishReason")
                    .and_then(|reason| reason.as_str())
                    .filter(|reason| !matches!(*reason, "STOP" | "FINISH_REASON_UNSPECIFIED"))
                {
                    assembly = TurnAssembly::fresh(turn);
                    failed_turn = Some(turn);
                    shared.lock().unwrap().partial = None;
                    let error = ChannelError::new(
                        ErrorKind::Provider,
                        false,
                        format!("the provider ended the reply early: {reason}"),
                    );
                    if events.send(ChannelEvent::Error(error)).await.is_err() {
                        return;
                    }
                    continue;
                }
                let parts = value
                    .pointer("/candidates/0/content/parts")
                    .and_then(|parts| parts.as_array())
                    .cloned()
                    .unwrap_or_default();
                for part in &parts {
                    let delta = assembly.absorb(part);
                    {
                        let mut shared = shared.lock().unwrap();
                        let partial = shared.partial.get_or_insert_with(|| PartialReply {
                            turn,
                            text: String::new(),
                            has_call: false,
                        });
                        if partial.turn != turn {
                            *partial = PartialReply {
                                turn,
                                text: String::new(),
                                has_call: false,
                            };
                        }
                        if part.get("functionCall").is_some() {
                            partial.has_call = true;
                        }
                        if let Some(text) = &delta {
                            partial.text.push_str(text);
                        }
                    }
                    let has_content = delta.is_some() || part.get("functionCall").is_some();
                    if has_content && !assembly.opened {
                        assembly.opened = true;
                        if events.send(ChannelEvent::UtteranceEnd).await.is_err() {
                            return;
                        }
                    }
                    if let Some(text) = delta.filter(|text| !text.is_empty()) {
                        let event = ChannelEvent::TranscriptDelta {
                            role: Role::Assistant,
                            text,
                        };
                        if events.send(event).await.is_err() {
                            return;
                        }
                    }
                }
            }
            Inbound::End { .. } => {
                let done = std::mem::replace(&mut assembly, TurnAssembly::fresh(turn));
                if commit_turn(done, &events, &shared).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn commit_turn(
    done: TurnAssembly,
    events: &mpsc::Sender<ChannelEvent>,
    shared: &Arc<Mutex<Shared>>,
) -> Result<(), ()> {
    shared.lock().unwrap().partial = None;
    if done.parts.is_empty() {
        return events
            .send(ChannelEvent::TurnComplete)
            .await
            .map_err(|_| ());
    }
    let calls: Vec<(String, String, serde_json::Value)> = {
        let mut shared = shared.lock().unwrap();
        shared.history.push(serde_json::json!({
            "role": "model",
            "parts": done.parts,
        }));
        done.calls
            .into_iter()
            .map(|(call_id, call, args)| {
                let name = call.name.clone();
                shared.pending.calls.insert(call_id.clone(), call);
                (call_id, name, args)
            })
            .collect()
    };
    let awaiting = !calls.is_empty();
    for (call_id, name, args) in calls {
        events
            .send(ChannelEvent::ToolCall {
                call_id,
                name,
                args,
            })
            .await
            .map_err(|_| ())?;
    }
    if !awaiting {
        events
            .send(ChannelEvent::TurnComplete)
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

#[cfg(test)]
pub mod scripted {
    use super::*;

    #[derive(Default)]
    struct ScriptedShared {
        requests: Vec<(u64, TextRequest)>,
        inbound: Option<mpsc::Sender<Inbound>>,
        last_opened: Option<u64>,
        cancels: Vec<Option<u64>>,
        refuse_ready: Option<ChannelError>,
        refuse_open_turn: Option<ChannelError>,
    }

    pub struct ScriptedWire {
        shared: Arc<Mutex<ScriptedShared>>,
    }

    #[derive(Clone)]
    pub struct ScriptedHandle {
        shared: Arc<Mutex<ScriptedShared>>,
        live_turn: Arc<AtomicU64>,
    }

    pub fn scripted_channel(generation: Generation) -> (TextChannel, ScriptedHandle) {
        scripted_channel_with(generation, None)
    }

    pub fn scripted_channel_with(
        generation: Generation,
        refuse_ready: Option<ChannelError>,
    ) -> (TextChannel, ScriptedHandle) {
        let shared = Arc::new(Mutex::new(ScriptedShared {
            refuse_ready,
            ..ScriptedShared::default()
        }));
        let channel = TextChannel::over(
            Box::new(ScriptedWire {
                shared: Arc::clone(&shared),
            }),
            generation,
        );
        shared.lock().unwrap().inbound = Some(channel.inbound_handle());
        let handle = ScriptedHandle {
            shared,
            live_turn: channel.live_turn_handle(),
        };
        (channel, handle)
    }

    impl ScriptedHandle {
        fn tag(&self) -> u64 {
            let shared = self.shared.lock().unwrap();
            shared
                .last_opened
                .unwrap_or_else(|| self.live_turn.load(Ordering::SeqCst))
        }

        fn sender(&self) -> mpsc::Sender<Inbound> {
            self.shared
                .lock()
                .unwrap()
                .inbound
                .clone()
                .expect("scripted wire: the channel has not opened a turn yet")
        }

        pub async fn push_chunk(&self, value: serde_json::Value) {
            let turn = self.tag();
            let _ = self.sender().send(Inbound::Chunk { turn, value }).await;
        }

        pub async fn push_text(&self, text: &str) {
            self.push_chunk(serde_json::json!({
                "candidates": [{ "content": { "role": "model", "parts": [{ "text": text }] } }]
            }))
            .await;
        }

        pub async fn push_call(&self, id: &str, name: &str, args: serde_json::Value) {
            self.push_chunk(serde_json::json!({
                "candidates": [{ "content": { "role": "model", "parts": [
                    { "functionCall": { "id": id, "name": name, "args": args } }
                ] } }]
            }))
            .await;
        }

        pub async fn end_turn(&self) {
            let turn = self.tag();
            let _ = self.sender().send(Inbound::End { turn }).await;
        }

        pub async fn fail_turn(&self, error: ChannelError) {
            let turn = self.tag();
            let _ = self.sender().send(Inbound::Failed { turn, error }).await;
        }

        pub fn requests(&self) -> Vec<(u64, TextRequest)> {
            self.shared.lock().unwrap().requests.clone()
        }

        pub fn cancels(&self) -> Vec<Option<u64>> {
            self.shared.lock().unwrap().cancels.clone()
        }

        pub fn refuse_next_open_turn(&self, error: ChannelError) {
            self.shared.lock().unwrap().refuse_open_turn = Some(error);
        }
    }

    #[async_trait]
    impl TextWire for ScriptedWire {
        async fn ready(&self) -> Result<(), ChannelError> {
            match self.shared.lock().unwrap().refuse_ready.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        async fn open_turn(
            &mut self,
            turn: u64,
            request: TextRequest,
            inbound: mpsc::Sender<Inbound>,
        ) -> Result<(), ChannelError> {
            let mut shared = self.shared.lock().unwrap();
            if let Some(error) = shared.refuse_open_turn.take() {
                return Err(error);
            }
            shared.inbound = Some(inbound);
            shared.last_opened = Some(turn);
            shared.requests.push((turn, request));
            Ok(())
        }

        fn cancel(&mut self, _turn: u64, live_gen: Option<u64>) {
            if live_gen.is_some() {
                self.shared.lock().unwrap().cancels.push(live_gen);
            }
        }
    }

    pub fn user_texts(request: &TextRequest) -> Vec<String> {
        request
            .contents
            .iter()
            .filter(|content| content["role"] == "user")
            .flat_map(|content| {
                content["parts"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|part| {
                        part.get("text")
                            .and_then(|t| t.as_str())
                            .map(str::to_string)
                    })
            })
            .collect()
    }

    pub fn tool_results(request: &TextRequest) -> Vec<(String, ToolResult)> {
        request
            .contents
            .iter()
            .filter(|content| content["role"] == "user")
            .flat_map(|content| content["parts"].as_array().cloned().unwrap_or_default())
            .filter_map(|part| {
                let response = part.get("functionResponse")?;
                let id = response.get("id")?.as_str()?.to_string();
                let body = response.get("response")?.clone();
                let single = |key: &str| {
                    body.as_object()
                        .is_some_and(|obj| obj.len() == 1 && obj.contains_key(key))
                };
                let result = if single("error") {
                    ToolResult::Err {
                        code: body["error"]["code"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        message: body["error"]["message"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    }
                } else if single("result") {
                    ToolResult::Ok(body["result"].clone())
                } else {
                    ToolResult::Ok(body)
                };
                Some((id, result))
            })
            .collect()
    }

    pub fn context_updates(request: &TextRequest) -> Vec<ContextUpdate> {
        let mut chapter_idx = None;
        let mut pos_ms = None;
        let mut body_lines = Vec::new();
        for line in request.system_instruction.lines() {
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
            updates.push(ContextUpdate::ChapterBoundary {
                chapter_idx,
                excerpt: super::super::types::DelimitedExcerpt {
                    delimited_text: body_lines.last().copied().unwrap_or_default().to_string(),
                },
            });
        }
        if let Some(pos_ms) = pos_ms {
            updates.push(ContextUpdate::PlaybackPosition { pos_ms });
        }
        updates
    }
}

#[cfg(test)]
mod conformance_tests {
    use super::scripted::{
        context_updates, scripted_channel, tool_results, user_texts, ScriptedHandle,
    };
    use super::*;
    use crate::voice::channel::conformance::{run_all, ConformanceFixture, ProviderSim};

    pub struct TextSim {
        handle: ScriptedHandle,
    }

    impl TextSim {
        fn last_request(&self) -> Option<TextRequest> {
            self.handle.requests().pop().map(|(_, request)| request)
        }
    }

    #[async_trait]
    impl ProviderSim for TextSim {
        async fn emit_audio(&mut self, _gen: u64, _pcm: Vec<i16>) {
            panic!("conformance rig: a text wire has no audio to emit");
        }
        async fn emit_transcript(&mut self, role: Role, text: String) {
            assert_eq!(
                role,
                Role::Assistant,
                "conformance rig: a text wire has no server-side user transcript"
            );
            self.handle.push_text(&text).await;
        }
        async fn emit_tool_call(&mut self, call_id: String, name: String, args: serde_json::Value) {
            self.handle.push_call(&call_id, &name, args).await;
            self.handle.end_turn().await;
        }
        async fn announce_session_end(&mut self) {
            panic!("conformance rig: the text wire declares session_renewal: false")
        }
        async fn complete_renewal(&mut self) {
            panic!("conformance rig: the text wire declares session_renewal: false")
        }
        async fn expire(&mut self) {
            panic!("conformance rig: the text wire declares session_renewal: false")
        }
        async fn received_audio_by_epoch(&mut self) -> Vec<Vec<Vec<i16>>> {
            Vec::new()
        }
        async fn received_texts(&mut self) -> Vec<String> {
            self.last_request()
                .map(|r| user_texts(&r))
                .unwrap_or_default()
        }
        async fn received_context_updates(&mut self) -> Vec<ContextUpdate> {
            self.last_request()
                .map(|r| context_updates(&r))
                .unwrap_or_default()
        }
        async fn received_tool_results(&mut self) -> Vec<(String, ToolResult)> {
            self.last_request()
                .map(|r| tool_results(&r))
                .unwrap_or_default()
        }
        async fn received_interrupts(&mut self) -> Vec<Option<u64>> {
            self.handle.cancels()
        }
    }

    pub struct TextFixture {
        pub rigs: usize,
    }

    #[async_trait]
    impl ConformanceFixture for TextFixture {
        type Channel = TextChannel;
        type Sim = TextSim;

        async fn rig(&mut self, generation: &Generation) -> (TextChannel, TextSim) {
            self.rigs += 1;
            let (channel, handle) = scripted_channel(generation.clone());
            (channel, TextSim { handle })
        }
    }

    #[tokio::test]
    async fn text_channel_passes_conformance_run_all_under_the_text_only_gate() {
        let mut fixture = TextFixture { rigs: 0 };
        run_all(&mut fixture).await;
        assert_eq!(
            fixture.rigs, 8,
            "seven core checks (each branching on text_only inside its rig) + the capabilities probe — never a renewal rig"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::scripted::{scripted_channel, scripted_channel_with};
    use super::*;
    use crate::voice::channel::types::DelimitedExcerpt;
    use crate::voice::keys::store::{Backend, KeyStore};
    use crate::voice::keys::ApiKey;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::timeout;

    const FIXTURE_KEY: &str = "fixture-text-key-7e1a";

    fn ctx() -> SessionContext {
        SessionContext {
            book_id: Some("book-1".to_string()),
            session_id: "session-1".to_string(),
            system_prompt: "You are the reading tutor.".to_string(),
            excerpt: DelimitedExcerpt {
                delimited_text: "<<x>>excerpt<</x>>".to_string(),
            },
            rolling_summary: None,
            playback_pos_ms: 0,
            tool_schemas: crate::voice::tools::registry::ToolRegistry::new(true).schemas(),
        }
    }

    async fn recv(events: &mut mpsc::Receiver<ChannelEvent>) -> ChannelEvent {
        timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("event within 5s")
            .expect("stream open")
    }

    async fn next_content(events: &mut mpsc::Receiver<ChannelEvent>) -> ChannelEvent {
        loop {
            let event = recv(events).await;
            if !matches!(
                event,
                ChannelEvent::UtteranceEnd | ChannelEvent::Progress(_)
            ) {
                return event;
            }
        }
    }

    /// Every `Progress` note that arrives before the turn's first content.
    async fn progress_until_content(
        events: &mut mpsc::Receiver<ChannelEvent>,
    ) -> (Vec<ProviderProgress>, ChannelEvent) {
        let mut notes = Vec::new();
        loop {
            match recv(events).await {
                ChannelEvent::Progress(note) => notes.push(note),
                ChannelEvent::UtteranceEnd => {}
                other => return (notes, other),
            }
        }
    }

    #[test]
    fn module_header_cites_every_wire_fact_with_url_and_access_date() {
        let source = include_str!("text.rs");
        let header: String = source
            .lines()
            .take_while(|line| line.starts_with("//!"))
            .collect::<Vec<_>>()
            .join("\n");
        for url in [
            "https://ai.google.dev/api/generate-content",
            "https://ai.google.dev/api/caching",
            "https://ai.google.dev/gemini-api/docs/api-key",
            "https://ai.google.dev/gemini-api/docs/thinking",
            "https://ai.google.dev/gemini-api/docs/caching",
        ] {
            assert!(header.contains(url), "provenance header lacks {url}");
        }
        assert!(
            header.matches("read 2026-").count() >= 5,
            "every citation carries an access date"
        );
        for fact in [
            ":streamGenerateContent",
            "?alt=sse",
            "x-goog-api-key",
            "thoughtSignature",
            "functionDeclarations",
            "functionResponse{id, name, response}",
        ] {
            assert!(
                header.contains(fact),
                "provenance header lacks the wire fact {fact:?}"
            );
        }
    }

    #[test]
    fn capabilities_declare_a_text_only_wire_without_renewal_or_word_timing() {
        let (channel, _handle) = scripted_channel(Generation::new());
        assert_eq!(
            channel.capabilities(),
            ChannelCapabilities {
                vad_mode: VadMode::Server,
                word_timestamps: false,
                session_renewal: false,
                text_only: true,
                stt_only: false,
            }
        );
    }

    #[tokio::test]
    async fn send_audio_is_invalid_state_before_and_after_connect() {
        let (mut channel, _handle) = scripted_channel(Generation::new());
        let before = channel.send_audio(&[0; 8]).await.unwrap_err();
        assert_eq!(before.kind, ErrorKind::InvalidState);
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        assert!(matches!(
            recv(&mut events).await,
            ChannelEvent::Lifecycle(LifecycleEvent::Connected)
        ));
        let after = channel.send_audio(&[0; 8]).await.unwrap_err();
        assert_eq!(after.kind, ErrorKind::InvalidState);
        assert!(!after.retryable);
    }

    #[tokio::test]
    async fn connect_refused_when_the_wire_has_no_key() {
        let refusal = ChannelError::new(ErrorKind::Auth, false, "no usable key");
        let (mut channel, _handle) =
            scripted_channel_with(Generation::new(), Some(refusal.clone()));
        let _events = channel.take_events().unwrap();
        assert_eq!(channel.connect(ctx()).await.unwrap_err(), refusal);
    }

    fn keys_for(base_url: &str) -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            std::path::PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(base_url, &ApiKey::new(FIXTURE_KEY.into()))
            .expect("session store never fails");
        Arc::new(KeyManager::with_store(store))
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> (String, String) {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = sock.read(&mut buf).await.expect("read request");
            assert!(n > 0, "client closed before a full request arrived");
            raw.extend_from_slice(&buf[..n]);
            let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&raw[..split]).to_string();
            let length: usize = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())?
                })
                .unwrap_or(0);
            let body_start = split + 4;
            while raw.len() < body_start + length {
                let n = sock.read(&mut buf).await.expect("read body");
                assert!(n > 0, "client closed mid-body");
                raw.extend_from_slice(&buf[..n]);
            }
            let body = String::from_utf8_lossy(&raw[body_start..body_start + length]).to_string();
            return (head, body);
        }
    }

    fn spawn_sse_server(
        scripts: Vec<(&'static str, &'static [&'static str])>,
    ) -> (String, tokio::task::JoinHandle<Vec<(String, String)>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let server = tokio::spawn(async move {
            let listener = TcpListener::from_std(listener).expect("tokio listener");
            let mut exchanges = Vec::new();
            for (status, script) in scripts {
                let (mut sock, _) = listener.accept().await.expect("accept");
                exchanges.push(read_request(&mut sock).await);
                crate::test_support::mock_http::write_head(
                    &mut sock,
                    status,
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

    const TURN_WITH_CALL: &[&str] = &[
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"Let me "}]}}]}"#,
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"check."}]}}]}"#,
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"id":"fc-1","name":"get_context_around_position","args":{"window":42}},"thoughtSignature":"c2lnLTE="}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10}}"#,
    ];

    const TURN_PLAIN: &[&str] = &[
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"It plots temperature."}]},"finishReason":"STOP"}]}"#,
    ];

    const TURN_MALFORMED: &[&str] = &[
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"Let me "}]}}]}"#,
        r#"data: {"candidates":[{"content":{"role":"model","parts":[]},"finishReason":"MALFORMED_FUNCTION_CALL"}]}"#,
    ];

    const OVERLOADED: &[&str] = &[
        r#"{"error":{"code":503,"message":"The model is overloaded. Please try again later.","status":"UNAVAILABLE"}}"#,
    ];
    const KEY_REJECTED: &[&str] = &[
        r#"{"error":{"code":401,"message":"Request had invalid authentication credentials.","status":"UNAUTHENTICATED"}}"#,
    ];
    const QUOTA: &[&str] = &[
        r#"{"error":{"code":429,"message":"Resource has been exhausted (e.g. check quota).","status":"RESOURCE_EXHAUSTED"}}"#,
    ];
    const BAD_REQUEST: &[&str] = &[
        r#"{"error":{"code":400,"message":"Invalid JSON payload received.","status":"INVALID_ARGUMENT"}}"#,
    ];

    fn no_wait_budget() -> PatienceBudget {
        PatienceBudget {
            max_cluster_attempts: 3,
            initial_backoff_secs: 0.0,
            max_backoff_cap_secs: 0.0,
            is_overnight: false,
        }
    }

    fn pool_candidate(base_url: &str, slot: u32, key: &str) -> DialCandidate {
        DialCandidate {
            account: format!("{base_url}#{slot}"),
            base_url: base_url.to_string(),
            slot,
            label: None,
            key: ApiKey::new(key.into()),
        }
    }

    /// A pooled text channel over the loopback server: no stored keys at
    /// all — every request must use the candidates it was handed.
    fn pooled_channel(
        base_url: &str,
        fallback_models: Vec<String>,
        candidates: Vec<DialCandidate>,
    ) -> (TextChannel, Arc<KeyManager>) {
        let keys = Arc::new(KeyManager::with_store(KeyStore::with_forced_backend(
            std::path::PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        )));
        let wire = HttpTextWire::new(
            TextChannelConfig {
                base_url: base_url.to_string(),
                model: "models/test-flash".to_string(),
                fallback_models,
            },
            Arc::clone(&keys),
        )
        .with_candidates(candidates)
        .with_budget(no_wait_budget());
        (TextChannel::over(Box::new(wire), Generation::new()), keys)
    }

    async fn ask(channel: &mut TextChannel) -> mpsc::Receiver<ChannelEvent> {
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        let _ = recv(&mut events).await;
        channel.send_text("what does figure 3 show?").await.unwrap();
        events
    }

    fn answered(event: ChannelEvent) {
        assert_eq!(
            event,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "It plots temperature.".to_string()
            }
        );
    }

    /// Review of #395 P1 + field 503s: the pool's keys serve the request
    /// with nothing stored under the bare URL (slot 0 removed), and a model
    /// that answers 503 twice is retried on the same key, then answers —
    /// the user sees the reply, never the overload.
    #[tokio::test]
    async fn an_overloaded_model_is_retried_on_a_pool_key_then_answers() {
        let (base_url, server) = spawn_sse_server(vec![
            ("503 Service Unavailable", OVERLOADED),
            ("503 Service Unavailable", OVERLOADED),
            ("200 OK", TURN_PLAIN),
        ]);
        let (mut channel, _keys) = pooled_channel(
            &base_url,
            Vec::new(),
            vec![pool_candidate(&base_url, 1, "pool-key-b")],
        );
        let mut events = ask(&mut channel).await;
        answered(next_content(&mut events).await);
        let exchanges = server.await.unwrap();
        assert_eq!(exchanges.len(), 3);
        for (head, _) in &exchanges {
            assert_eq!(
                header_value(head, "x-goog-api-key").as_deref(),
                Some("pool-key-b")
            );
            assert!(head.contains("models/test-flash"), "{head}");
        }
    }

    /// Owner ruling 2026-10-09: a model that stays overloaded through its
    /// budget hands the turn to the next model in the fallback chain.
    #[tokio::test]
    async fn a_model_that_stays_overloaded_falls_back_to_the_next_model() {
        let (base_url, server) = spawn_sse_server(vec![
            ("503 Service Unavailable", OVERLOADED),
            ("503 Service Unavailable", OVERLOADED),
            ("503 Service Unavailable", OVERLOADED),
            ("200 OK", TURN_PLAIN),
        ]);
        let (mut channel, _keys) = pooled_channel(
            &base_url,
            vec!["models/backup-flash".to_string()],
            vec![pool_candidate(&base_url, 0, "pool-key-a")],
        );
        let mut events = ask(&mut channel).await;
        let (notes, first) = progress_until_content(&mut events).await;
        answered(first);
        assert_eq!(
            notes,
            vec![
                ProviderProgress::Retrying { attempt: 1 },
                ProviderProgress::Retrying { attempt: 2 },
                ProviderProgress::FallbackModel {
                    model: "backup-flash".to_string()
                },
            ],
            "the user is told the model is busy and which model took over"
        );
        let exchanges = server.await.unwrap();
        assert_eq!(exchanges.len(), 4);
        assert!(exchanges[2].0.contains("models/test-flash"));
        assert!(
            exchanges[3].0.contains("models/backup-flash"),
            "the fallback model answers: {}",
            exchanges[3].0
        );
    }

    /// A rejected key is marked and the next pool key answers at once.
    #[tokio::test]
    async fn a_rejected_key_rotates_to_the_next_pool_key() {
        let (base_url, server) = spawn_sse_server(vec![
            ("401 Unauthorized", KEY_REJECTED),
            ("200 OK", TURN_PLAIN),
        ]);
        let (mut channel, _keys) = pooled_channel(
            &base_url,
            Vec::new(),
            vec![
                pool_candidate(&base_url, 0, "pool-key-a"),
                pool_candidate(&base_url, 1, "pool-key-b"),
            ],
        );
        let mut events = ask(&mut channel).await;
        answered(next_content(&mut events).await);
        let exchanges = server.await.unwrap();
        assert_eq!(
            header_value(&exchanges[0].0, "x-goog-api-key").as_deref(),
            Some("pool-key-a")
        );
        assert_eq!(
            header_value(&exchanges[1].0, "x-goog-api-key").as_deref(),
            Some("pool-key-b")
        );
    }

    /// A quota refusal marks the key out of quota (with a re-entry
    /// deadline) and the next key answers.
    #[tokio::test]
    async fn a_quota_refusal_marks_the_key_and_rotates() {
        let (base_url, server) = spawn_sse_server(vec![
            ("429 Too Many Requests", QUOTA),
            ("200 OK", TURN_PLAIN),
        ]);
        let (mut channel, keys) = pooled_channel(
            &base_url,
            Vec::new(),
            vec![
                pool_candidate(&base_url, 0, "pool-key-a"),
                pool_candidate(&base_url, 1, "pool-key-b"),
            ],
        );
        let mut events = ask(&mut channel).await;
        answered(next_content(&mut events).await);
        server.await.unwrap();
        assert!(
            keys.retry_after_until(&format!("{base_url}#0")).is_some(),
            "the refused key carries a quota re-entry deadline"
        );
    }

    /// A terminal refusal fails the turn at once — no retry, no fallback.
    #[tokio::test]
    async fn a_terminal_refusal_fails_the_turn_without_retrying() {
        let (base_url, server) = spawn_sse_server(vec![("400 Bad Request", BAD_REQUEST)]);
        let (mut channel, _keys) = pooled_channel(
            &base_url,
            vec!["models/backup-flash".to_string()],
            vec![pool_candidate(&base_url, 0, "pool-key-a")],
        );
        let mut events = ask(&mut channel).await;
        match next_content(&mut events).await {
            ChannelEvent::Error(error) => {
                assert!(error.message.contains("400"), "{}", error.message)
            }
            other => panic!("expected the typed refusal, got {other:?}"),
        }
        assert_eq!(server.await.unwrap().len(), 1, "exactly one request");
    }

    #[tokio::test]
    async fn a_non_stop_finish_reason_is_a_typed_error_not_a_completed_turn() {
        let (base_url, server) =
            spawn_sse_server(vec![("200 OK", TURN_MALFORMED), ("200 OK", TURN_PLAIN)]);
        let mut channel = TextChannel::new(
            TextChannelConfig {
                base_url: base_url.clone(),
                model: "models/test-flash".to_string(),
                fallback_models: Vec::new(),
            },
            keys_for(&base_url),
            Generation::new(),
        );
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        assert!(matches!(
            recv(&mut events).await,
            ChannelEvent::Lifecycle(LifecycleEvent::Connected)
        ));
        channel.send_text("what is figure 3?").await.unwrap();
        assert_eq!(recv(&mut events).await, ChannelEvent::UtteranceEnd);
        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "Let me ".to_string()
            }
        );
        match recv(&mut events).await {
            ChannelEvent::Error(error) => {
                assert_eq!(error.kind, ErrorKind::Provider);
                assert!(
                    error.message.contains("MALFORMED_FUNCTION_CALL"),
                    "{}",
                    error.message
                );
            }
            other => panic!("expected a typed provider error, got {other:?}"),
        }
        channel.send_text("try again").await.unwrap();
        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::UtteranceEnd,
            "the failed turn must emit nothing after its error; the next event belongs to the next turn"
        );
        assert_eq!(
            next_content(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "It plots temperature.".to_string()
            }
        );
        assert_eq!(recv(&mut events).await, ChannelEvent::TurnComplete);
        channel.close().await.unwrap();
        let exchanges = server.await.unwrap();
        let (_, second_body) = &exchanges[1];
        assert!(
            !second_body.contains("Let me "),
            "the discarded partial reply must not be replayed as history: {second_body}"
        );
    }

    #[tokio::test]
    async fn http_wire_streams_text_surfaces_the_call_and_round_trips_the_signature() {
        let (base_url, server) =
            spawn_sse_server(vec![("200 OK", TURN_WITH_CALL), ("200 OK", TURN_PLAIN)]);
        let mut channel = TextChannel::new(
            TextChannelConfig {
                base_url: base_url.clone(),
                model: "models/test-flash".to_string(),
                fallback_models: Vec::new(),
            },
            keys_for(&base_url),
            Generation::new(),
        );
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        assert!(matches!(
            recv(&mut events).await,
            ChannelEvent::Lifecycle(LifecycleEvent::Connected)
        ));
        channel.send_text("what is figure 3?").await.unwrap();
        assert_eq!(recv(&mut events).await, ChannelEvent::UtteranceEnd);
        for want in ["Let me ", "check."] {
            assert_eq!(
                recv(&mut events).await,
                ChannelEvent::TranscriptDelta {
                    role: Role::Assistant,
                    text: want.to_string()
                }
            );
        }
        assert_eq!(
            recv(&mut events).await,
            ChannelEvent::ToolCall {
                call_id: "fc-1".to_string(),
                name: "get_context_around_position".to_string(),
                args: serde_json::json!({"window": 42}),
            }
        );
        channel
            .send_tool_result("fc-1", ToolResult::Ok(serde_json::json!({"text": "…"})))
            .await
            .unwrap();
        assert_eq!(
            next_content(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: "It plots temperature.".to_string()
            }
        );
        assert_eq!(recv(&mut events).await, ChannelEvent::TurnComplete);
        channel.close().await.unwrap();
        let exchanges = server.await.unwrap();
        let (head, body) = &exchanges[0];
        assert!(
            head.starts_with("POST /v1beta/models/test-flash:streamGenerateContent?alt=sse "),
            "{head}"
        );
        assert_eq!(
            header_value(head, "x-goog-api-key").as_deref(),
            Some(FIXTURE_KEY)
        );
        assert!(!head.contains(FIXTURE_KEY) || head.matches(FIXTURE_KEY).count() == 1);
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You are the reading tutor.\n<<x>>excerpt<</x>>"
        );
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "what is figure 3?");
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"], "LOW",
            "every text turn asks for low thinking: the default (medium) kept the \
             field session waiting a minute for the first word"
        );
        let declared: Vec<&str> = body["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert!(
            declared.contains(&"get_context_around_position"),
            "{declared:?}"
        );
        let (_, continuation) = &exchanges[1];
        let continuation: serde_json::Value = serde_json::from_str(continuation).unwrap();
        let contents = continuation["contents"].as_array().unwrap();
        assert_eq!(
            contents.len(),
            3,
            "user, model (with the call), user (with the answer)"
        );
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["text"], "Let me check.");
        assert_eq!(
            contents[1]["parts"][1]["functionCall"]["name"],
            "get_context_around_position"
        );
        assert_eq!(
            contents[1]["parts"][1]["thoughtSignature"], "c2lnLTE=",
            "the signature rides back exactly as received, in its part"
        );
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["id"], "fc-1");
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"]["name"],
            "get_context_around_position"
        );
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"]["response"]["text"],
            "…"
        );
    }

    #[tokio::test]
    async fn http_status_maps_onto_the_degradation_matrix() {
        // The surfaced error is the LAST refusal once the single key and the
        // (zero-wait) overload budget are spent: a 503 is retried to the
        // budget first, so it is scripted once per attempt.
        for (status, kind, retryable, attempts) in [
            ("401 Unauthorized", ErrorKind::Auth, false, 1),
            ("429 Too Many Requests", ErrorKind::RateLimited, true, 1),
            ("503 Service Unavailable", ErrorKind::Provider, true, 3),
            ("418 I'm a teapot", ErrorKind::Protocol, false, 1),
        ] {
            let empty: &'static [&'static str] = &[];
            let (base_url, _server) = spawn_sse_server(vec![(status, empty); attempts]);
            let wire = HttpTextWire::new(
                TextChannelConfig {
                    base_url: base_url.clone(),
                    model: "models/test-flash".to_string(),
                    fallback_models: Vec::new(),
                },
                keys_for(&base_url),
            )
            .with_budget(no_wait_budget());
            let mut channel = TextChannel::over(Box::new(wire), Generation::new());
            let mut events = channel.take_events().unwrap();
            channel.connect(ctx()).await.unwrap();
            let _ = recv(&mut events).await;
            channel.send_text("hi").await.unwrap();
            match next_content(&mut events).await {
                ChannelEvent::Error(error) => {
                    assert_eq!(error.kind, kind, "{status}");
                    assert_eq!(error.retryable, retryable, "{status}");
                }
                other => panic!("{status}: expected an Error event, got {other:?}"),
            }
        }
    }

    /// Review 2026-10-09, R1: a network chunk boundary inside an Arabic
    /// letter must not become U+FFFD, and a last line without its newline
    /// is still delivered.
    #[test]
    fn sse_lines_survive_chunks_split_inside_a_letter() {
        let event = "data: {\"t\":\"الطبقة\"}\r\n";
        let bytes = event.as_bytes();
        // Split inside the two-byte "ا" (bytes 13..15 hold the first letter).
        let first_letter = event.find('ا').unwrap();
        let mut lines = SseLines::default();
        assert!(lines.push(&bytes[..first_letter + 1]).is_empty());
        let out = lines.push(&bytes[first_letter + 1..]);
        assert_eq!(out, vec!["data: {\"t\":\"الطبقة\"}".to_string()]);
        assert!(!out[0].contains('\u{FFFD}'));
        assert!(lines.push(b"data: {\"end\":1}").is_empty());
        assert_eq!(lines.finish().as_deref(), Some("data: {\"end\":1}"));
        assert_eq!(lines.finish(), None);
    }

    #[tokio::test]
    async fn interrupt_drops_the_superseded_tail_and_keeps_only_what_was_shown() {
        let (mut channel, handle) = scripted_channel(Generation::new());
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        let _ = recv(&mut events).await;
        channel.send_text("go").await.unwrap();
        handle.push_text("first ").await;
        assert_eq!(recv(&mut events).await, ChannelEvent::UtteranceEnd);
        assert!(matches!(
            recv(&mut events).await,
            ChannelEvent::TranscriptDelta { .. }
        ));
        channel.interrupt().await.unwrap();
        handle.push_text("tail").await;
        handle.end_turn().await;
        assert!(
            timeout(Duration::from_millis(300), events.recv())
                .await
                .is_err(),
            "nothing from the superseded turn surfaces"
        );
        channel.send_text("again").await.unwrap();
        let (_, request) = handle.requests().pop().unwrap();
        let roles: Vec<&str> = request
            .contents
            .iter()
            .map(|c| c["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            ["user", "model", "user"],
            "the stopped reply keeps what it already showed (review 2026-10-09, J4)"
        );
        assert_eq!(request.contents[1]["parts"][0]["text"], "first ");
    }

    #[tokio::test]
    async fn tool_result_for_an_unknown_call_is_invalid_state() {
        let (mut channel, _handle) = scripted_channel(Generation::new());
        let _events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        let err = channel
            .send_tool_result("ghost", ToolResult::Ok(serde_json::json!({})))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidState);
        assert!(!err.retryable);
    }

    #[tokio::test]
    async fn a_new_user_turn_abandons_unanswered_calls_so_history_stays_valid() {
        let (mut channel, handle) = scripted_channel(Generation::new());
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        let _ = recv(&mut events).await;
        channel.send_text("q").await.unwrap();
        handle
            .push_call("c1", "pause_playback", serde_json::json!({}))
            .await;
        handle.end_turn().await;
        assert!(matches!(
            next_content(&mut events).await,
            ChannelEvent::ToolCall { .. }
        ));
        channel.send_text("never mind").await.unwrap();
        let (_, request) = handle.requests().pop().unwrap();
        assert!(
            request.contents.iter().all(|c| c["role"] == "user"),
            "the unanswered call turn was dropped: {:?}",
            request.contents
        );
        // Review 2026-10-09, L1: the harness may already hold the dropped
        // call's `ToolCall` and answer it after the typed send. That answer
        // is silence — like a Live `toolCallCancellation` — never an error
        // (the error wedged the session in recovery) and never a new turn.
        let opened = handle.requests().len();
        channel
            .send_tool_result("c1", ToolResult::Ok(serde_json::json!({})))
            .await
            .expect("an answer to a call the user typed over is dropped quietly");
        assert_eq!(handle.requests().len(), opened, "no turn reopened");
    }

    /// Review 2026-10-09, L2: Stop on a reply that asked for a tool. The
    /// call's answer, already computed at the harness, must not reopen the
    /// stopped turn.
    #[tokio::test]
    async fn interrupt_retires_the_calls_the_stopped_reply_asked_for() {
        let (mut channel, handle) = scripted_channel(Generation::new());
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        let _ = recv(&mut events).await;
        channel.send_text("q").await.unwrap();
        handle
            .push_call("c2", "jump_to_page", serde_json::json!({ "page": 3 }))
            .await;
        handle.end_turn().await;
        assert!(matches!(
            next_content(&mut events).await,
            ChannelEvent::ToolCall { .. }
        ));
        channel.interrupt().await.unwrap();
        let opened = handle.requests().len();
        channel
            .send_tool_result("c2", ToolResult::Ok(serde_json::json!({})))
            .await
            .expect("a retired call's answer is silence");
        assert_eq!(
            handle.requests().len(),
            opened,
            "the stopped reply stays stopped"
        );
    }

    /// Review 2026-10-09, J4: the user typed over a reply still streaming.
    /// They read its first words; the model must know it said them, or it
    /// repeats itself. A FAILED reply is still never committed (see
    /// `a_non_stop_finish_reason_is_a_typed_error_not_a_completed_turn`).
    #[tokio::test]
    async fn a_reply_the_user_typed_over_stays_in_the_models_history() {
        let (mut channel, handle) = scripted_channel(Generation::new());
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        let _ = recv(&mut events).await;
        channel.send_text("what is a thread?").await.unwrap();
        handle.push_text("A thread is a path of ").await;
        let _ = next_content(&mut events).await;
        channel.send_text("wait, and a process?").await.unwrap();
        let (_, request) = handle.requests().pop().unwrap();
        let turns: Vec<(String, String)> = request
            .contents
            .iter()
            .map(|c| {
                (
                    c["role"].as_str().unwrap().to_string(),
                    c["parts"][0]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect();
        assert_eq!(
            turns,
            [
                ("user".to_string(), "what is a thread?".to_string()),
                ("model".to_string(), "A thread is a path of ".to_string()),
                ("user".to_string(), "wait, and a process?".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn history_folds_the_oldest_exchanges_into_a_delimited_record() {
        let (mut channel, handle) = scripted_channel(Generation::new());
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        let _ = recv(&mut events).await;
        let long = "x".repeat(400);
        for i in 0..8 {
            channel.send_text(&format!("q{i} {long}")).await.unwrap();
            handle.push_text(&format!("a{i} {long}")).await;
            handle.end_turn().await;
            let _ = next_content(&mut events).await;
            let _ = recv(&mut events).await;
        }
        let (_, request) = handle.requests().pop().unwrap();
        assert!(request.contents.len() < 16, "{}", request.contents.len());
        assert!(request.contents.len() >= VERBATIM_TURNS);
        assert_eq!(
            request.contents[0]["role"], "user",
            "the verbatim tail opens on a user turn"
        );
        assert!(request.system_instruction.contains(RECORD_FRAMING));
        assert!(
            request.system_instruction.contains("The user: q"),
            "{}",
            request.system_instruction
        );
        assert!(
            !request.system_instruction.contains("q7 "),
            "the verbatim tail is not in the record"
        );
        let record = request
            .system_instruction
            .split("<<record>>")
            .nth(1)
            .and_then(|r| r.split("<</record>>").next())
            .unwrap();
        assert!(record.chars().count() <= FOLD_MAX_CHARS + 2);
    }

    struct AlwaysFrame;

    #[async_trait]
    impl crate::voice::tools::vision::VisionPort for AlwaysFrame {
        async fn frame_for_page(
            &mut self,
            page_no: i64,
        ) -> Result<crate::voice::tools::vision::VisionFrame, String> {
            Ok(crate::voice::tools::vision::VisionFrame {
                page_no,
                bytes: vec![0x89, 0x50, 0x4e, 0x47],
                mime: "image/png",
            })
        }
    }

    /// `look_at_page` under the text channel (investigation 2026-10-09:
    /// typed chat is the default mode, so the text tutor must see the page
    /// too): the page still rides the call's own answer as a multimodal
    /// function response (`functionResponse.parts[].inlineData`), and the
    /// ack says it was sent.
    #[tokio::test]
    async fn look_at_page_sends_the_page_inside_the_text_channel_answer() {
        use crate::voice::excerpt::selector::ExcerptSelector;
        use crate::voice::excerpt::source::SessionSnapshot;
        use crate::voice::excerpt::window::WindowPolicy;
        use crate::voice::harness::EventPump;
        use crate::voice::tools::dispatcher::Dispatcher;
        use crate::voice::tools::playback::InMemoryPlayback;
        use crate::voice::tools::registry::{ToolRegistry, LOOK_AT_PAGE};

        let generation = Generation::new();
        let (mut channel, handle) = scripted_channel(generation.clone());
        let mut events = channel.take_events().unwrap();
        let mut context = ctx();
        context.tool_schemas = ToolRegistry::new(true).schemas();
        channel.connect(context).await.unwrap();
        let _ = recv(&mut events).await;
        channel.send_text("what is on this page?").await.unwrap();
        handle
            .push_call("see-1", LOOK_AT_PAGE, serde_json::json!({}))
            .await;
        handle.end_turn().await;
        let call = next_content(&mut events).await;
        assert!(matches!(&call, ChannelEvent::ToolCall { name, .. } if name == LOOK_AT_PAGE));

        let mut pump = EventPump::new(generation);
        pump.vision = Some(Box::new(AlwaysFrame));
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(true));
        let mut playback = InMemoryPlayback::at(2500);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![(1, 0, 2000, "page one"), (2, 2000, 4000, "page two")],
                vec![(0, 4000)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let mut forwarded = Vec::new();
        pump.handle(
            call,
            &mut channel,
            &mut dispatcher,
            &mut playback,
            &mut excerpts,
            &mut |event| forwarded.push(event),
        )
        .await
        .unwrap();
        let (_, request) = handle.requests().pop().unwrap();
        let results = super::scripted::tool_results(&request);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "see-1");
        assert!(
            matches!(&results[0].1, ToolResult::Ok(body) if body["sent"] == true && body["page"] == 2),
            "the ack says the page went: {:?}",
            results[0].1
        );
        let response = request
            .contents
            .iter()
            .flat_map(|content| content["parts"].as_array().cloned().unwrap_or_default())
            .find_map(|part| part.get("functionResponse").cloned())
            .expect("the answer is on the wire");
        assert_eq!(
            response["parts"],
            serde_json::json!([{ "inlineData": {
                "mimeType": "image/png",
                "data": "iVBORw==",
            }}]),
            "the page still rides the call's own function response"
        );
    }

    /// A request over the inline ceiling is refused before it leaves —
    /// typed, non-retryable, nothing sent, history untouched.
    #[tokio::test]
    async fn an_oversized_request_is_refused_before_it_leaves() {
        let (mut channel, handle) = scripted_channel(Generation::new());
        channel.connect(ctx()).await.unwrap();
        let huge = "x".repeat(MAX_TEXT_REQUEST_BYTES + 1);
        let refused = channel
            .send_text(&huge)
            .await
            .expect_err("over the ceiling");
        assert_eq!(refused.kind, ErrorKind::Protocol);
        assert!(!refused.retryable);
        assert!(handle.requests().is_empty(), "nothing left the channel");
        assert!(channel.shared.lock().unwrap().history.contents.is_empty());
    }

    /// Review of #395, P2: page stills must not pile up in the history
    /// every later request resends. Sixteen looks with 1 MiB stills: the
    /// last request carries ONE image (the newest), stays far under the
    /// 20 MB inline limit, and every function call keeps its response.
    #[tokio::test]
    async fn only_the_newest_page_still_rides_the_history() {
        use crate::voice::tools::registry::LOOK_AT_PAGE;
        let (mut channel, handle) = scripted_channel(Generation::new());
        let mut events = channel.take_events().unwrap();
        channel.connect(ctx()).await.unwrap();
        let _ = recv(&mut events).await;
        let still = vec![0x5a_u8; 1024 * 1024];
        for round in 0..16 {
            channel
                .send_text(&format!("what is on page {round}?"))
                .await
                .unwrap();
            let id = format!("see-{round}");
            handle
                .push_call(&id, LOOK_AT_PAGE, serde_json::json!({}))
                .await;
            handle.end_turn().await;
            loop {
                if matches!(
                    next_content(&mut events).await,
                    ChannelEvent::ToolCall { .. }
                ) {
                    break;
                }
            }
            channel.send_image(&still, "image/png").await.unwrap();
            channel
                .send_tool_result(&id, ToolResult::Ok(serde_json::json!({ "sent": true })))
                .await
                .unwrap();
        }
        let (_, request) = handle.requests().pop().unwrap();
        let body = serde_json::to_vec(&request_body(&request)).unwrap();
        let images = request
            .contents
            .iter()
            .flat_map(|content| content["parts"].as_array().cloned().unwrap_or_default())
            .filter_map(|part| part.get("functionResponse").cloned())
            .filter(|response| response.get("parts").is_some())
            .count();
        assert_eq!(images, 1, "only the newest still rides");
        assert!(
            body.len() < 2 * 1024 * 1024,
            "request is {} bytes",
            body.len()
        );
        let calls = request
            .contents
            .iter()
            .flat_map(|content| content["parts"].as_array().cloned().unwrap_or_default())
            .filter(|part| part.get("functionCall").is_some())
            .count();
        assert_eq!(
            calls,
            super::scripted::tool_results(&request).len(),
            "pairing intact"
        );
    }

    /// An image with no call awaiting an answer has nothing to ride: it is
    /// refused, so the pump never tells the model it saw a page.
    #[tokio::test]
    async fn an_image_with_no_pending_call_is_refused_on_the_text_channel() {
        let (mut channel, _handle) = scripted_channel(Generation::new());
        channel.connect(ctx()).await.unwrap();
        let refused = channel
            .send_image(&[0x89, 0x50, 0x4e, 0x47], "image/png")
            .await
            .expect_err("nothing to attach the image to");
        assert_eq!(refused.kind, ErrorKind::InvalidState);
        assert!(channel.shared.lock().unwrap().pending.image.is_none());
    }

    #[tokio::test]
    async fn failed_send_text_does_not_commit_history() {
        let (mut channel, handle) = scripted_channel(Generation::new());
        channel.connect(ctx()).await.expect("connect");

        // Refuse the first open_turn.
        handle.refuse_next_open_turn(ChannelError::new(
            ErrorKind::Transport,
            true,
            "network down",
        ));

        let res = channel.send_text("this question fails").await;
        assert!(res.is_err());

        // History must NOT contain the failed user message.
        let history_len = channel.shared.lock().unwrap().history.contents.len();
        assert_eq!(
            history_len, 0,
            "failed user turn must not be committed to history"
        );

        // Subsequent successful send_text succeeds and only commits its own message.
        channel
            .send_text("this question succeeds")
            .await
            .expect("send succeeds");
        let history = channel.shared.lock().unwrap().history.contents.clone();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["role"], "user");
        assert_eq!(history[0]["parts"][0]["text"], "this question succeeds");
    }
}
