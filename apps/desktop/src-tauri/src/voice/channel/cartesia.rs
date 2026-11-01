//! Cartesia streaming-TTS slot (VA5 — BYOK external provider, VOICE_AGENT
//! §3.2 TTS slot; this unit is struct + config wiring + the capability
//! declaration; the wire lands in this stride's impl step).
//!
//! Wire protocol provenance (https://docs.cartesia.ai/api-reference/tts/tts,
//! accessed 2026-08-21, verified live against the WS reference this date):
//!
//! - Endpoint & version: `wss://api.cartesia.ai/tts/websocket`, with the
//!   **required** `cartesia_version` parameter on the upgrade URL — the dated
//!   API version, single allowed value
//!   [`crate::voice::keys::probe::CARTESIA_VERSION`] (`2026-08-14`), the same
//!   const the VA3 probe pins (its provenance lives there; the
//!   `cartesia-version-pin` roster fixture guards the pin). The version is
//!   protocol metadata, not a credential, so riding the query string does not
//!   touch `auth-hygiene-no-query-param-creds`.
//!
//! - Auth: the `X-API-Key` request header on the upgrade — the doc's
//!   trusted-server scheme. The doc's browser-side alternative, a short-lived
//!   `access_token` **query param**, is banned here outright (VOICE_AGENT
//!   §3.2 `auth-hygiene-no-query-param-creds`): connect URLs stay
//!   credential-free. The key resolves at connect through the VA3
//!   [`KeyManager`] under `Provider::Cartesia.default_base_url()`
//!   (`https://api.cartesia.ai`), the §3.2/§7 per-slot per-base-URL identity.
//!
//! - Generation request (client → server, JSON text frame): required fields
//!   `model_id`, `transcript`, `voice`, `output_format`, `context_id`.
//!   `voice` is the plain voice-ID-string form (the doc's oneOf also allows
//!   an `{id}` object; embeddings are not accepted in this API version) —
//!   [`TtsSlotConfig::voice`] carries exactly that string. `output_format`
//!   is `{container: "raw", encoding: "pcm_s16le", sample_rate: 24000}`:
//!   little-endian PCM16 at [`PROVIDER_OUTPUT_RATE`], the exact
//!   `AudioDelta` contract, so the adapter never resamples or transcodes —
//!   only base64-decodes. `add_timestamps: true` (word-level timing, the
//!   `word_timestamps` capability; §11 follow-along). `continue` defaults to
//!   `false` = "no more transcript chunks follow for this context":
//!   [`TtsSlot::speak`] takes one response turn's complete text, so every
//!   speak frame is its context's last chunk and the default is the truth —
//!   sent explicitly so the recorded frame is self-describing.
//!
//! - Server messages (JSON text frames, all carrying `context_id` +
//!   `status_code`): `"type": "chunk"` — `data` is **base64-inside-JSON**
//!   audio (`done` flags the context's final audio chunk; `step_time` is
//!   server-side timing); the §2 jitter-buffer inflation note applies to
//!   this slot exactly as to JSON-audio channels (§3.2 Cartesia note).
//!   `"type": "timestamps"` — `word_timestamps` holds parallel arrays
//!   `words[]`/`start[]`/`end[]` with **seconds**-float timing →
//!   millisecond [`TimedWord`]s. `"type": "done"` — the generation
//!   completion signal. `"type": "error"` — `title`/`message`/`status_code`.
//!   `"type": "flush_done"`/`"type": "phoneme_timestamps"` — responses to
//!   request features this adapter never enables; tolerated and ignored.
//!
//! - Cancellation: the client frame `{"context_id": …, "cancel": true}`
//!   (`cancel` is enum-locked to `true` in the doc) — the barge-in mapping.
//!   The wire cancel stops **future** frames only (§2 rule 2: "the network
//!   round-trip only stops future deltas"); frames of the cancelled context
//!   still in flight surface under their speak-time generation tag — the
//!   playback gate is the ONE arbiter of staleness, and the slot's duty is
//!   the honest tag, never local censorship. (The `cartesia-cancel-drain`
//!   roster fixture pins the wire exchange; the mid-air surfacing is the
//!   conformance suite's check-3 invariant.)
//!
//! Completion signal choice: **no event** — the cc-1c precedent
//! (`openai_compat` turn-complete reasoning) holds identically here: the
//! `TtsSlot` narrowed vocabulary (composed.rs trait doc) is
//! `AudioDelta`/`WordTimestamps`/`Lifecycle`/`Error`, and nothing in it is
//! turn-boundary-shaped (`Lifecycle` is session-lifetime only;
//! `UtteranceEnd` is STT-role). The wire's `done` chunk flag and `"done"`
//! message retire the live context silently — internal state, like gemini's
//! `turnComplete`. If the cc-1e composition loop needs an explicit
//! synthesis-complete boundary, that is an event.rs vocabulary growth
//! decided THERE, not smuggled in here.
//!
//! Capability posture: `add_timestamps` yields real word-aligned timing →
//! `word_timestamps` is `true` (§3.1 table "per TTS slot"; §11 — no
//! sentence-level degrade on this slot). The free tier is non-commercial →
//! advisory label, never blocking (§3.2 provider notes).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::voice::audio::PROVIDER_OUTPUT_RATE;
use crate::voice::keys::probe::CARTESIA_VERSION;
use crate::voice::keys::KeyManager;

use super::composed::{TtsSlot, TtsSlotConfig};
use super::{ChannelError, ChannelEvent, ErrorKind, LifecycleEvent, TimedWord};

/// Default TTS WS endpoint (module header provenance);
/// [`TtsSlotConfig::endpoint_override`] replaces it for proxies and
/// recorded-fixture servers.
pub const DEFAULT_TTS_ENDPOINT: &str = "wss://api.cartesia.ai/tts/websocket";

/// The canonical base URL the user's Cartesia key is stored under
/// (`Provider::Cartesia.default_base_url()`); the key lookup is by this
/// identity even when the endpoint override points the socket elsewhere —
/// the same split deepgram.rs and gemini.rs use.
const CARTESIA_KEY_BASE_URL: &str = "https://api.cartesia.ai";

const EVENT_CAPACITY: usize = 4096;

type TtsSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type TtsWriter = SplitSink<TtsSocket, Message>;

/// The one synthesis context currently allowed to surface frames, with the
/// generation snapshot the composed loop handed [`TtsSlot::speak`]
/// (loop-owned tagging — the slot only stamps the snapshot, composed.rs
/// trait doc). Shared between the slot methods and the read loop; every
/// inbound frame whose `context_id` is not (or no longer) this one is
/// discarded — superseded and already-done contexts drain through that one
/// gate. A CANCELLED context is different: it stays live for tagging, so
/// its mid-air frames keep surfacing under the speak-time `gen` (§2 rule 2:
/// the cancel stops future frames; the playback gate — not the slot — is
/// the arbiter of staleness).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveContext {
    context_id: String,
    gen: u64,
    /// The provider cancel already went out for this context: its mid-air
    /// frames still surface (honest tagging, doc above), but the socket
    /// ending under it is no longer a mid-synthesis transport failure —
    /// this response was stopped deliberately.
    cancelled: bool,
}

/// Retires `context_id` if it is the live context: its remaining in-flight
/// frames then die at the read loop's drain gate. A context that is not live
/// is already draining — nothing to do.
fn retire(live: &std::sync::Mutex<Option<LiveContext>>, context_id: &str) {
    let mut guard = live.lock().unwrap();
    if guard
        .as_ref()
        .is_some_and(|ctx| ctx.context_id == context_id)
    {
        *guard = None;
    }
}

/// Slot state, the uniform contract the slot traits inherit from
/// [`super::SessionChannel`]: connect is valid exactly once from `New`; a
/// closed slot never reopens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    New,
    Connected,
    Closed,
}

/// The launch [`TtsSlot`] (VOICE_AGENT §3.2 slot table: "TTS — Cartesia
/// (WS streaming)") — one Cartesia `/tts/websocket` connection driven
/// through the slot role: `speak` opens a context, the read loop decodes
/// that context's chunks/timestamps onto the event stream, `cancel` kills
/// and drains it.
pub struct CartesiaTtsSlot {
    config: TtsSlotConfig,
    keys: Arc<KeyManager>,
    state: SlotState,
    /// Write half of the split socket, live while `Connected` — the same
    /// shared-slot shape as deepgram.rs/gemini.rs; `speak` and `cancel`
    /// frames go out through it.
    writer: Arc<tokio::sync::Mutex<Option<TtsWriter>>>,
    /// The inbound decode loop; aborted at `close` — this wire has no
    /// CloseStream-like flush to drain (module header).
    reader: Option<JoinHandle<()>>,
    /// The live context / drain gate shared with the read loop (see
    /// [`LiveContext`]).
    live: Arc<std::sync::Mutex<Option<LiveContext>>>,
    /// The event pair exists from construction: take_events must yield the
    /// receiver pre-connect (the [`super::SessionChannel::take_events`]
    /// shape the slot traits mirror); connect only clones this sender.
    events_tx: Option<mpsc::Sender<ChannelEvent>>,
    events: Option<mpsc::Receiver<ChannelEvent>>,
}

impl CartesiaTtsSlot {
    /// Builds the slot from the composed channel's credential-free TTS
    /// config (the key is resolved at connect through the [`KeyManager`]
    /// under the Cartesia base-URL identity, VOICE_AGENT §3.2/§7). No
    /// [`crate::voice::audio::generation::Generation`] parameter, by
    /// design: generation tagging is loop-owned — the composed loop hands
    /// each turn's snapshot into [`TtsSlot::speak`] (composed.rs trait
    /// doc, the m3-fix4 lesson). Like [`super::DeepgramSttSlot::new`],
    /// this constructor is the §3.2 "one registry entry" for the launch
    /// TTS slot until a registry stride lands.
    pub fn new(config: TtsSlotConfig, keys: Arc<KeyManager>) -> Self {
        let (events_tx, events) = mpsc::channel(EVENT_CAPACITY);
        Self {
            config,
            keys,
            state: SlotState::New,
            writer: Arc::new(tokio::sync::Mutex::new(None)),
            reader: None,
            live: Arc::new(std::sync::Mutex::new(None)),
            events_tx: Some(events_tx),
            events: Some(events),
        }
    }

    /// The URL the slot dials: the override when present (else
    /// [`DEFAULT_TTS_ENDPOINT`]) plus the **required** `cartesia_version`
    /// parameter pinned to [`CARTESIA_VERSION`] (module header: protocol
    /// metadata, not a credential — the query string stays inside
    /// `auth-hygiene-no-query-param-creds`). The key never rides here.
    fn tts_url(&self) -> String {
        let endpoint = self
            .config
            .endpoint_override
            .as_deref()
            .unwrap_or(DEFAULT_TTS_ENDPOINT);
        let separator = if endpoint.contains('?') { '&' } else { '?' };
        format!("{endpoint}{separator}cartesia_version={CARTESIA_VERSION}")
    }
}

/// One response turn's text as the wire generation request (module header,
/// request provenance): the required fields plus `add_timestamps: true` (the
/// `word_timestamps` capability) and the explicit `continue: false` — every
/// speak frame is its context's complete transcript, so the recorded frame
/// is self-describing. `output_format` is the exact `AudioDelta` contract —
/// raw little-endian PCM16 at [`PROVIDER_OUTPUT_RATE`] — so the read loop
/// only base64-decodes, never resamples or transcodes.
fn generation_request(config: &TtsSlotConfig, text: &str, context_id: &str) -> String {
    serde_json::json!({
        "model_id": config.model,
        "transcript": text,
        "voice": config.voice,
        "output_format": {
            "container": "raw",
            "encoding": "pcm_s16le",
            "sample_rate": PROVIDER_OUTPUT_RATE,
        },
        "context_id": context_id,
        "add_timestamps": true,
        "continue": false,
    })
    .to_string()
}

/// Decodes a chunk message's base64-inside-JSON `data` (module header,
/// server-message provenance) into 24 kHz PCM16 samples — little-endian per
/// the requested `pcm_s16le`. Malformed base64 yields `None` and the chunk
/// is skipped, matching the gemini.rs posture for the same JSON-audio shape.
fn decode_chunk_audio(data: &str) -> Option<Vec<i16>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some(
        bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect(),
    )
}

/// Decodes a timestamps message's `word_timestamps` — parallel arrays
/// `words[]`/`start[]`/`end[]` with seconds-float timing (module header) —
/// into the event vocabulary's millisecond [`TimedWord`]s. Entries missing a
/// field are skipped, not errors (the deepgram.rs `decode_words` rationale:
/// a word the UI cannot place is worth less than the words it can).
fn decode_word_timestamps(word_timestamps: Option<&serde_json::Value>) -> Vec<TimedWord> {
    let (Some(words), Some(starts), Some(ends)) = (
        word_timestamps
            .and_then(|wt| wt.get("words"))
            .and_then(|v| v.as_array()),
        word_timestamps
            .and_then(|wt| wt.get("start"))
            .and_then(|v| v.as_array()),
        word_timestamps
            .and_then(|wt| wt.get("end"))
            .and_then(|v| v.as_array()),
    ) else {
        return Vec::new();
    };
    words
        .iter()
        .zip(starts)
        .zip(ends)
        .filter_map(|((word, start), end)| {
            Some(TimedWord {
                w: word.as_str()?.to_string(),
                start_ms: (start.as_f64()? * 1000.0).round() as u32,
                end_ms: (end.as_f64()? * 1000.0).round() as u32,
            })
        })
        .collect()
}

/// Maps a wire error message's `status_code` into the §9 error vocabulary —
/// the banked VA5 status map: 401/403 credential rejection → `Auth`
/// non-retryable, 429 → `RateLimited` retryable, 5xx → `Provider` retryable,
/// anything else on an in-protocol error frame → `Protocol` non-retryable.
fn wire_error(status_code: Option<u64>, detail: String) -> ChannelError {
    match status_code {
        Some(401) | Some(403) => ChannelError::new(ErrorKind::Auth, false, detail),
        Some(429) => ChannelError::new(ErrorKind::RateLimited, true, detail),
        Some(code) if (500..600).contains(&code) => {
            ChannelError::new(ErrorKind::Provider, true, detail)
        }
        _ => ChannelError::new(ErrorKind::Protocol, false, detail),
    }
}

/// Inbound loop for the life of the slot, decoding server messages — JSON
/// text frames, all carrying `context_id` (module header) — into the slot's
/// narrowed event vocabulary (`AudioDelta`/`WordTimestamps`/`Lifecycle`/
/// `Error`, composed.rs trait doc).
///
/// The drain gate sits in one place, ahead of every decode: a frame whose
/// context is not the live one is discarded — that single check implements
/// the speak-supersede contract and the silencing of a done context's
/// stragglers. A cancelled context is still the live one (cancel never
/// retires locally — §2 rule 2, cancellation provenance in the module
/// header), so its mid-air frames pass the gate and surface under their
/// speak-time tag; the playback gate downstream refuses them.
///
/// - `chunk`: `data` base64-decodes to 24 kHz PCM16 → `AudioDelta` stamped
///   with the live context's speak-time `gen` (loop-owned tagging —
///   generation-agnostic here, the slot never reads a live counter);
///   `done: true` flags the context's final audio chunk and retires the
///   context silently — the completion-signal decision in the module header:
///   internal state, no event.
/// - `timestamps`: `word_timestamps` → `WordTimestamps` under the same tag.
/// - `done`: the generation completion signal — retires the live context
///   silently (same decision).
/// - `error`: `title`/`message`/`status_code` → one classified
///   `ChannelEvent::Error` ([`wire_error`]); the errored context is retired
///   first so its stragglers drain.
/// - `flush_done`/`phoneme_timestamps`/anything unknown: responses to
///   request features this adapter never enables — tolerated and ignored.
///
/// The loop ends when the socket does. Mid-synthesis — a context still live —
/// that end is a transport failure (the banked VA5 mapping: transport /
/// mid-stream → `Transport`, retryable); with nothing live it is just the
/// connection ending, and `close` aborts the loop before closing the socket
/// so a clean local shutdown never reaches this path.
async fn read_loop(
    mut reader: SplitStream<TtsSocket>,
    events: mpsc::Sender<ChannelEvent>,
    live: Arc<std::sync::Mutex<Option<LiveContext>>>,
) {
    while let Some(Ok(message)) = reader.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let context_id = value
            .get("context_id")
            .and_then(|id| id.as_str())
            .unwrap_or("");
        // The drain gate (doc above): only the live context surfaces, and
        // its frames carry the speak-time generation snapshot.
        let gen = {
            let guard = live.lock().unwrap();
            match guard.as_ref() {
                Some(ctx) if ctx.context_id == context_id => ctx.gen,
                _ => continue,
            }
        };
        match value.get("type").and_then(|kind| kind.as_str()) {
            Some("chunk") => {
                if let Some(pcm) = value
                    .get("data")
                    .and_then(|data| data.as_str())
                    .and_then(decode_chunk_audio)
                {
                    if !pcm.is_empty()
                        && events
                            .send(ChannelEvent::AudioDelta { pcm, gen })
                            .await
                            .is_err()
                    {
                        return;
                    }
                }
                if value
                    .get("done")
                    .and_then(|flag| flag.as_bool())
                    .unwrap_or(false)
                {
                    retire(&live, context_id);
                }
            }
            Some("timestamps") => {
                let words = decode_word_timestamps(value.get("word_timestamps"));
                if !words.is_empty()
                    && events
                        .send(ChannelEvent::WordTimestamps { words, gen })
                        .await
                        .is_err()
                {
                    return;
                }
            }
            Some("done") => retire(&live, context_id),
            Some("error") => {
                let title = value.get("title").and_then(|t| t.as_str()).unwrap_or("");
                let message = value.get("message").and_then(|m| m.as_str()).unwrap_or("");
                let status_code = value.get("status_code").and_then(|s| s.as_u64());
                retire(&live, context_id);
                let err = wire_error(
                    status_code,
                    format!("cartesia error for context {context_id:?}: {title}: {message}"),
                );
                if events.send(ChannelEvent::Error(err)).await.is_err() {
                    return;
                }
            }
            _ => continue,
        }
    }
    // The socket ended on the peer's initiative. Mid-synthesis that is a
    // dropped transport under a live response (banked mapping: Transport,
    // retryable); with no context live there is nothing in flight to mourn,
    // and a CANCELLED context still draining was stopped deliberately — the
    // socket ending under it is not a mid-response failure.
    if live
        .lock()
        .unwrap()
        .take()
        .is_some_and(|ctx| !ctx.cancelled)
    {
        let _ = events
            .send(ChannelEvent::Error(ChannelError::new(
                ErrorKind::Transport,
                true,
                "tts ws ended mid-synthesis",
            )))
            .await;
    }
}

#[async_trait]
impl TtsSlot for CartesiaTtsSlot {
    fn word_timestamps(&self) -> bool {
        // `add_timestamps: true` on every generation request yields real
        // word-aligned timing (module header provenance; §11) — the flag
        // this slot contributes to the composed capability derivation.
        true
    }

    async fn connect(&mut self) -> Result<(), ChannelError> {
        if self.state != SlotState::New {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("connect on a slot in state {:?}", self.state),
            ));
        }

        let key = self
            .keys
            .session_key(CARTESIA_KEY_BASE_URL)
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Auth,
                    false,
                    format!("no usable Cartesia key: {err}"),
                )
            })?;
        let mut request = self.tts_url().into_client_request().map_err(|err| {
            ChannelError::new(ErrorKind::Protocol, false, format!("bad endpoint: {err}"))
        })?;
        // Header auth only, in the doc's trusted-server scheme — `X-API-Key`
        // (module header provenance): the connect URL stays credential-free
        // (VOICE_AGENT §3.2 `auth-hygiene-no-query-param-creds`) and the
        // sensitive flag keeps the value out of any Debug rendering.
        let mut auth = HeaderValue::from_str(key.expose()).map_err(|_| {
            ChannelError::new(
                ErrorKind::Auth,
                false,
                "the stored key contains characters no header can carry",
            )
        })?;
        auth.set_sensitive(true);
        request.headers_mut().insert("x-api-key", auth);

        let (socket, _response) = connect_async(request).await.map_err(|err| {
            ChannelError::new(
                ErrorKind::Transport,
                true,
                format!("tts ws connect failed: {err}"),
            )
        })?;

        // No setup/setupComplete exchange on this wire: the session is
        // configured per generation request, so the open connection IS ready.
        let tx = self
            .events_tx
            .clone()
            .expect("a New slot still holds its construction-time sender");
        let _ = tx.try_send(ChannelEvent::Lifecycle(LifecycleEvent::Connected));
        let (writer, reader) = socket.split();
        self.reader = Some(tokio::spawn(read_loop(reader, tx, Arc::clone(&self.live))));
        *self.writer.lock().await = Some(writer);
        self.state = SlotState::Connected;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), ChannelError> {
        if self.state == SlotState::Closed {
            return Ok(());
        }
        self.state = SlotState::Closed;
        self.events_tx = None;
        // No CloseStream-like flush exists in this wire's vocabulary (module
        // header): the clean exit is the WS close handshake itself. The read
        // loop dies first so the socket ending under a still-live context is
        // never mistaken for a mid-synthesis transport drop; a peer that
        // already vanished is not a close failure.
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        if let Some(mut writer) = self.writer.lock().await.take() {
            let _ = writer.close().await;
        }
        Ok(())
    }

    async fn speak(&mut self, text: &str, context_id: &str, gen: u64) -> Result<(), ChannelError> {
        if self.state != SlotState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("speak on a slot in state {:?}", self.state),
            ));
        }
        // The context goes live BEFORE its frame goes out: the read loop runs
        // concurrently, and a first chunk racing the send must already find
        // its context live under the loop's snapshot. Replacing the previous
        // entry is the supersede contract (trait doc): the old context's
        // remaining frames drain unsurfaced, exactly as if cancelled.
        *self.live.lock().unwrap() = Some(LiveContext {
            context_id: context_id.to_string(),
            gen,
            cancelled: false,
        });
        let frame = generation_request(&self.config, text, context_id);
        let mut writer = self.writer.lock().await;
        writer
            .as_mut()
            .expect("Connected implies a live write half")
            .send(Message::text(frame))
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("tts ws dropped sending the generation request: {err}"),
                )
            })?;
        Ok(())
    }

    async fn cancel(&mut self, context_id: &str) -> Result<(), ChannelError> {
        if self.state != SlotState::Connected {
            return Err(ChannelError::new(
                ErrorKind::InvalidState,
                false,
                format!("cancel on a slot in state {:?}", self.state),
            ));
        }
        // No local drain (§2 rule 2: "the network round-trip only stops
        // future deltas"): the context stays live for tagging, so every
        // mid-air frame of it still surfaces under its speak-time gen and
        // dies at the PLAYBACK gate — the one arbiter of staleness; local
        // discard here would censor what the gate is owed. The context is
        // only marked cancelled (socket-end accounting, LiveContext doc);
        // its `done`/`error` or the next speak retires it. Cancelling a
        // context that is not live (already done or superseded) still sends
        // the frame — the provider may hold state — and that context is
        // already draining.
        {
            let mut guard = self.live.lock().unwrap();
            if let Some(ctx) = guard.as_mut().filter(|ctx| ctx.context_id == context_id) {
                ctx.cancelled = true;
            }
        }
        // The doc's cancellation frame, `cancel` enum-locked to `true`
        // (module header, cancellation provenance).
        let frame = serde_json::json!({ "context_id": context_id, "cancel": true }).to_string();
        let mut writer = self.writer.lock().await;
        writer
            .as_mut()
            .expect("Connected implies a live write half")
            .send(Message::text(frame))
            .await
            .map_err(|err| {
                ChannelError::new(
                    ErrorKind::Transport,
                    true,
                    format!("tts ws dropped sending the cancel: {err}"),
                )
            })?;
        Ok(())
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<ChannelEvent>> {
        self.events.take()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::voice::channel::fixture::{load_fixture, replay, WireServer};
    use crate::voice::keys::store::{Backend, KeyStore};
    use crate::voice::keys::ApiKey;

    /// Deliberately echoed in the fixture's `absent` needles: no frame the
    /// adapter sends — and no connect URL — may carry the key.
    const FIXTURE_KEY: &str = "fixture-cartesia-key-7b3d";

    fn keys_with_cartesia_key() -> Arc<KeyManager> {
        let store = KeyStore::with_forced_backend(
            PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(CARTESIA_KEY_BASE_URL, &ApiKey::new(FIXTURE_KEY.into()))
            .unwrap();
        Arc::new(KeyManager::with_store(store))
    }

    fn config(endpoint: String) -> TtsSlotConfig {
        TtsSlotConfig {
            endpoint_override: Some(endpoint),
            model: "sonic-2".into(),
            voice: "voice-abc".into(),
        }
    }

    /// One recorded Cartesia `/tts/websocket` session driven through the slot,
    /// pinning what the launch impl actually does. Connect: the version-pinned
    /// (`cartesia_version=2026-08-14`), credential-free upgrade with the key on
    /// the `X-API-Key` header. Speak: the generation request — required fields
    /// plus the raw-PCM16 `output_format`, `add_timestamps`, and the explicit
    /// `continue: false` — payload-pinned. A context's two chunks and its
    /// word-timing message decode to byte-pinned `AudioDelta` and ms-pinned
    /// `WordTimestamps` stamped with the speak-time generation; the `done`
    /// message retires the context silently (proven by the next context's
    /// delta arriving with nothing between). A second speak opens a new
    /// context, then `cancel` stops it: the cancel frame is pinned, and the
    /// post-cancel straggler of the cancelled context surfaces under its
    /// speak-time tag (the mid-air invariant — the playback gate, not the
    /// slot, refuses stale audio).
    #[tokio::test]
    async fn cartesia_tts_slot_runs_a_session_with_cancel_drain() {
        let handle = WireServer::start().await;
        let mut slot = CartesiaTtsSlot::new(
            config(handle.url("/tts/websocket")),
            keys_with_cartesia_key(),
        );

        let fixture = load_fixture("cartesia-tts-session");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        slot.connect().await.expect("slot connect");

        // The fixture pins the version's presence and the key's absence from
        // the URL; this pins the exact scheme the doc names (module header):
        // the key rides the X-API-Key header on the credential-free upgrade.
        let record = handle.wait_for_connection(1).await;
        assert_eq!(
            record.header("x-api-key"),
            Some(FIXTURE_KEY),
            "the key rides the X-API-Key header, the doc's trusted-server scheme"
        );

        let mut events = slot
            .take_events()
            .expect("the slot hands over the receiver");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "no setup exchange on this wire: the open connection is ready"
        );
        assert!(
            slot.take_events().is_none(),
            "take_events yields the receiver exactly once"
        );

        // First speak opens ctx-1 under generation 7; the generation-request
        // frame is pinned by the fixture's next expect_text step.
        const GEN_ONE: u64 = 7;
        slot.speak("Figure three shows the axes.", "ctx-1", GEN_ONE)
            .await
            .expect("first speak");

        // The two chunks base64-decode to little-endian PCM16 (both i16
        // extremes appear across the run so a byte-order slip cannot pass),
        // each surfaced as an AudioDelta stamped with the speak-time gen.
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: vec![1000, -1000],
                gen: GEN_ONE,
            }),
            "chunk data base64-decodes to little-endian PCM16, stamped with the \
             speak-time generation"
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: vec![257, -2],
                gen: GEN_ONE,
            })
        );
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::WordTimestamps {
                words: vec![
                    TimedWord {
                        w: "figure".into(),
                        start_ms: 520,
                        end_ms: 900,
                    },
                    TimedWord {
                        w: "three".into(),
                        start_ms: 900,
                        end_ms: 1280,
                    },
                ],
                gen: GEN_ONE,
            }),
            "the seconds-float word_timestamps decode to millisecond TimedWords"
        );

        // Second speak opens ctx-2 under a new generation. ctx-1's `done`
        // message must have produced no event: this ctx-2 delta arriving next
        // (the mpsc is FIFO) is the proof of that silent completion.
        const GEN_TWO: u64 = 8;
        slot.speak("Second turn.", "ctx-2", GEN_TWO)
            .await
            .expect("second speak");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: vec![i16::MAX, i16::MIN],
                gen: GEN_TWO,
            }),
            "ctx-1's done retired it silently: the next surfaced event is ctx-2's"
        );

        // Cancel ctx-2: the cancel frame is pinned by the fixture, and the
        // post-cancel straggler chunk of ctx-2 (served immediately after)
        // still surfaces under the speak-time tag — the cancel stops future
        // frames only; the playback gate downstream, not this slot, refuses
        // the stale ones (§2 rule 2, conformance check 3's mid-air
        // invariant).
        slot.cancel("ctx-2").await.expect("cancel ctx-2");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: vec![42],
                gen: GEN_TWO,
            }),
            "the cancelled context's mid-air straggler surfaces old-tagged — \
             honest tagging, never local censorship"
        );

        // The replayer serves the straggler and then closes the socket from
        // the server side; awaiting it before close keeps the straggler's send
        // on a live connection (this wire's close puts no frame on the wire, so
        // there is no adapter-frame step to synchronize against). The socket
        // ending under the cancelled-but-undrained ctx-2 is NOT mourned as a
        // mid-synthesis transport failure: that response was stopped
        // deliberately, so the stream ends clean.
        replayer.await.expect("the recorded fixture replays green");
        slot.close().await.expect("slot close");
        assert_eq!(
            events.recv().await,
            None,
            "the stream ends clean: no mid-synthesis mourning for a \
             deliberately cancelled context"
        );
    }

    /// The `cartesia-buffer-delay` roster entry (VOICE_AGENT §3.2). The doc's
    /// input-buffering machinery (https://docs.cartesia.ai/api-reference/tts/tts,
    /// accessed 2026-08-22): `max_buffer_delay_ms` bounds how long the server
    /// buffers streamed transcript chunks before generating — [0, 5000] ms,
    /// default 3000 — and `flush` forces the buffer out, acknowledged by
    /// `flush_done`. This slot never streams partial transcripts:
    /// [`TtsSlot::speak`] sends one response turn whole, so the fixture pins
    /// the sidestep — `continue: false` (the doc's final-chunk form, "to
    /// minimize latency") with no `max_buffer_delay_ms` and no `flush` on the
    /// generation request — and pins that a buffering-machinery `flush_done`
    /// arriving anyway is ignored without disturbing the live context: the
    /// next surfaced event is the context's own audio (the mpsc is FIFO).
    #[tokio::test]
    async fn cartesia_buffer_delay_stays_unset_and_flush_done_is_ignored() {
        let handle = WireServer::start().await;
        let mut slot = CartesiaTtsSlot::new(
            config(handle.url("/tts/websocket")),
            keys_with_cartesia_key(),
        );

        let fixture = load_fixture("cartesia-buffer-delay");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        slot.connect().await.expect("slot connect");
        let mut events = slot
            .take_events()
            .expect("the slot hands over the receiver");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );

        const GEN: u64 = 11;
        slot.speak("One complete turn, sent whole.", "ctx-bd", GEN)
            .await
            .expect("speak");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::AudioDelta {
                pcm: vec![2, -3],
                gen: GEN,
            }),
            "flush_done produced no event: the context's own audio is the \
             next thing surfaced"
        );

        // The fixture's `done` retired the context before the server close,
        // so the socket ending is not mourned as a mid-synthesis failure.
        replayer.await.expect("the recorded fixture replays green");
        slot.close().await.expect("slot close");
        assert_eq!(
            events.recv().await,
            None,
            "a retired context ends the stream clean"
        );
    }

    /// The `cartesia-version-pin` roster entry (VOICE_AGENT §3.2). The doc
    /// (https://docs.cartesia.ai/api-reference/tts/tts, accessed 2026-08-22)
    /// marks `cartesia_version` **required** on `/tts/websocket` — a dated API
    /// version, e.g. `2026-08-14` — with the key on the `X-API-Key` header
    /// (the trusted-server scheme). The fixture pins the upgrade itself: the
    /// dated version rides the connect target of every session this slot
    /// opens, and no credential does. Beyond the fixture's needle, the test
    /// pins the exact value to [`CARTESIA_VERSION`], the single pin shared
    /// with the key-probe path (module header) — one dated version per
    /// integration, not one per call site.
    #[tokio::test]
    async fn cartesia_version_pin_rides_the_upgrade_target() {
        let handle = WireServer::start().await;
        let mut slot = CartesiaTtsSlot::new(
            config(handle.url("/tts/websocket")),
            keys_with_cartesia_key(),
        );

        let fixture = load_fixture("cartesia-version-pin");
        let replay_handle = handle.clone();
        let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

        slot.connect().await.expect("slot connect");
        let mut events = slot
            .take_events()
            .expect("the slot hands over the receiver");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        );
        replayer.await.expect("the recorded fixture replays green");

        let record = handle.wait_for_connection(1).await;
        assert!(
            record
                .target
                .contains(&format!("cartesia_version={CARTESIA_VERSION}")),
            "the pinned dated version is the one the module shares with the \
             key probe, verbatim: {}",
            record.target
        );

        // No context was ever live, so the provider-side close is clean.
        slot.close().await.expect("slot close");
        assert_eq!(events.recv().await, None, "no live context: a clean end");
    }
}
