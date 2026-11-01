//! Roster fixtures for roadmap slots (VA5, VOICE_AGENT §3.2).
//!
//! The 12-entry conformance-fixture roster (TASKS VA5; §3.2 provider notes)
//! covers providers in two states. Providers with an implemented slot keep
//! their roster entries next to the adapter (cartesia.rs, deepgram.rs). The
//! providers named by the roster that have **no implemented slot yet** —
//! AssemblyAI, ElevenLabs, Fish Audio, OpenAI TTS are roadmap additions in
//! the §3.2 slot table — get their entries here: a **recorded-shape fixture**
//! (cut from the provider's live official docs, provenance in the fixture
//! file) plus a test that pins the contract-level handling §3.2 documents,
//! driven by a reference driver that behaves exactly as the future slot must.
//! When such a slot lands, its module inherits the fixture and the pinned
//! contract — the shape is already decided and CI-guarded (§3.2 slot rule:
//! one recorded conformance fixture per new slot).
//!
//! Replay discipline is unchanged: everything runs on the shared
//! [`fixture`](super::fixture) rig against a loopback [`WireServer`] — CI
//! never opens a non-loopback socket (VOICE_AGENT §10).

#![cfg(test)]

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use std::sync::Arc;

use super::composed::{SttSlot as _, SttSlotConfig, TtsSlot as _, TtsSlotConfig};
use super::event::TimedWord;
use super::fixture::{fixture_dir, load_fixture, replay, WireServer, WireStep};
use super::{CartesiaTtsSlot, DeepgramSttSlot};
use crate::voice::audio::generation::Generation;
use crate::voice::keys::store::{Backend, KeyStore};
use crate::voice::keys::{ApiKey, KeyManager};

/// Deliberately echoed in the fixtures' `absent` needles: no frame a
/// reference driver sends — and no connect URL — may carry a credential.
const FIXTURE_KEY: &str = "fixture-assemblyai-key-4c9e";
const ELEVENLABS_FIXTURE_KEY: &str = "fixture-elevenlabs-key-9d21";
const FISH_FIXTURE_KEY: &str = "fixture-fish-key-2f8a";

type DriverSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_with_headers(url: String, headers: &[(&'static str, &str)]) -> DriverSocket {
    let mut request = url.into_client_request().expect("client request");
    for (header, value) in headers {
        request
            .headers_mut()
            .insert(*header, HeaderValue::from_str(value).expect("header value"));
    }
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("reference driver connect");
    socket
}

async fn connect_with_header(url: String, header: &'static str, value: &str) -> DriverSocket {
    connect_with_headers(url, &[(header, value)]).await
}

/// Next binary frame from the provider side, with the same loud budget as
/// [`next_json`].
async fn next_binary(socket: &mut DriverSocket) -> Vec<u8> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match socket.next().await.expect("stream open").expect("frame") {
                Message::Binary(bytes) => return bytes.to_vec(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("expected a provider binary frame, got {other:?}"),
            }
        }
    })
    .await
    .expect("reference driver: provider sent nothing within the step budget")
}

/// Next text frame from the provider side, JSON-parsed. Bounded by the same
/// posture as the replay rig's step budget: a silent provider fails the test
/// loudly instead of hanging it (a replay step that reds leaves the driver
/// waiting on frames that will never come).
async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match socket.next().await.expect("stream open").expect("frame") {
                Message::Text(text) => return serde_json::from_str(&text).expect("provider JSON"),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("expected a provider text frame, got {other:?}"),
            }
        }
    })
    .await
    .expect("reference driver: provider sent nothing within the step budget")
}

/// The `assemblyai-socket-billing-close` roster entry (VOICE_AGENT §3.2).
///
/// The official doc (https://www.assemblyai.com/docs/speech-to-text/universal-streaming,
/// accessed 2026-08-22): "Streaming Speech-to-Text is billed on the total
/// duration that your WebSocket connection stays open" — socket-open time,
/// not audio seconds; an unclosed session auto-closes only after 3 hours and
/// bills for all of it. The §3.2 contract therefore is: **the slot must close
/// the socket on pause** — never hold an idle streaming socket open across a
/// playback pause. The documented close procedure is a `{"type": "Terminate"}`
/// text message (which finalizes any open turn), then reading the server's
/// `Termination` message (`audio_duration_seconds`, `session_duration_seconds`)
/// before dropping the socket.
///
/// The fixture pins that wire shape end to end: the `/v3/ws` upgrade with the
/// key on the `Authorization` header (the doc's server scheme — no `Bearer`
/// prefix) and only protocol metadata in the query; binary PCM16 audio
/// upstream; and — the entry's point — the pause path being Terminate →
/// Termination → close, not silence.
#[tokio::test]
async fn assemblyai_pause_terminates_the_billed_socket() {
    let handle = WireServer::start().await;
    let fixture = load_fixture("assemblyai-socket-billing-close");
    let replay_handle = handle.clone();
    let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

    // The reference driver is the future AssemblyAI STT slot's contract:
    // connect exactly as the doc's server scheme prescribes.
    let mut socket = connect_with_header(
        handle.url("/v3/ws?sample_rate=16000&encoding=pcm_s16le"),
        "authorization",
        FIXTURE_KEY,
    )
    .await;

    let begin = next_json(&mut socket).await;
    assert_eq!(begin["type"], "Begin", "the session opens with Begin");

    // One mic chunk upstream: raw binary PCM16, both i16 extremes present so
    // a byte-order slip cannot pass the fixture's pinned payload.
    let pcm: [i16; 4] = [1000, -1000, i16::MAX, i16::MIN];
    let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    socket.send(Message::binary(bytes)).await.expect("audio");

    let turn = next_json(&mut socket).await;
    assert_eq!(turn["type"], "Turn");
    assert_eq!(
        turn["words"][0]["start"], 80,
        "v3 word timings are integer milliseconds — no seconds-float decode"
    );

    // PAUSE. Billing runs on socket-open time, so the pause path is the
    // documented termination, not an idle open socket: Terminate, then wait
    // for Termination (the doc: keep the socket open briefly to receive the
    // last messages), then drop the socket.
    socket
        .send(Message::text("{\"type\":\"Terminate\"}"))
        .await
        .expect("terminate");
    let termination = next_json(&mut socket).await;
    assert_eq!(termination["type"], "Termination");
    let billed = termination["session_duration_seconds"]
        .as_u64()
        .expect("session seconds");
    let audio = termination["audio_duration_seconds"]
        .as_u64()
        .expect("audio seconds");
    assert!(
        billed >= audio,
        "the recorded receipt shows the billing rule itself: the session \
         (socket-open) duration, not the audio duration, is what accrues"
    );
    socket.close(None).await.expect("driver close");

    replayer.await.expect("the recorded fixture replays green");
}

/// The accumulation rule the future ElevenLabs TTS slot must implement
/// (VOICE_AGENT §3.2 ElevenLabs note): `charStartTimesMs` in each
/// `AudioOutput.alignment` is **chunk-relative, not stream-absolute** — the
/// doc states the times "are relative to the returned chunk from the model,
/// and not the full audio response." Absolute timing therefore needs a
/// running offset: each char's absolute start is `offset + start`, and after
/// a chunk the offset advances by that chunk's span (its last char's start +
/// duration). Words split on whitespace chars; punctuation stays attached —
/// the [`TimedWord`] derivation §11 follow-along consumes.
fn accumulate_chunk_relative_alignment(chunks: &[serde_json::Value]) -> Vec<TimedWord> {
    let mut words = Vec::new();
    let mut offset: u32 = 0;
    let mut current: Option<TimedWord> = None;
    for chunk in chunks {
        let alignment = &chunk["alignment"];
        let chars = alignment["chars"].as_array().expect("chars");
        let starts = alignment["charStartTimesMs"].as_array().expect("starts");
        let durations = alignment["charDurationsMs"].as_array().expect("durations");
        assert!(
            chars.len() == starts.len() && chars.len() == durations.len(),
            "the doc requires all three alignment arrays to match in length"
        );
        for ((ch, start), duration) in chars.iter().zip(starts).zip(durations) {
            let ch = ch.as_str().expect("char");
            let abs_start = offset + start.as_u64().expect("start") as u32;
            let abs_end = abs_start + duration.as_u64().expect("duration") as u32;
            if ch.chars().all(char::is_whitespace) {
                words.extend(current.take());
            } else if let Some(word) = current.as_mut() {
                word.w.push_str(ch);
                word.end_ms = abs_end;
            } else {
                current = Some(TimedWord {
                    w: ch.to_string(),
                    start_ms: abs_start,
                    end_ms: abs_end,
                });
            }
        }
        if let (Some(last_start), Some(last_duration)) = (starts.last(), durations.last()) {
            offset += (last_start.as_u64().expect("start")
                + last_duration.as_u64().expect("duration")) as u32;
        }
    }
    words.extend(current.take());
    words
}

/// The `elevenlabs-alignment-accumulation` roster entry (VOICE_AGENT §3.2).
///
/// Official doc (https://elevenlabs.io/docs/api-reference/text-to-speech/v-1-text-to-speech-voice-id-stream-input,
/// accessed 2026-08-22): `stream-input` audio messages carry `alignment`
/// (`chars` / `charStartTimesMs` / `charDurationsMs`), and the timings are
/// per-chunk — "relative to the returned chunk from the model, and not the
/// full audio response." The fixture records a two-chunk turn whose second
/// chunk restarts at 0 ms; the test pins the contract the §3.2 note demands:
/// a running offset accumulated across chunks, so derived [`TimedWord`]s stay
/// stream-absolute and monotonic. A naive absolute read of the recorded data
/// would place the second chunk's words before the first chunk's end — the
/// exact drift this entry exists to catch.
///
/// Connect shape pinned alongside: the key rides the `xi-api-key` header
/// (never `authorization`/`single_use_token` query params — the doc's
/// browser-side alternatives are banned by auth hygiene), the init message is
/// the doc's single blank space, and outgoing text ends with a space.
#[tokio::test]
async fn elevenlabs_alignment_accumulates_across_chunks() {
    let handle = WireServer::start().await;
    let fixture = load_fixture("elevenlabs-alignment-accumulation");
    let replay_handle = handle.clone();
    let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

    let mut socket = connect_with_header(
        handle.url("/v1/text-to-speech/voice-abc/stream-input?model_id=eleven_flash_v2_5&sync_alignment=true"),
        "xi-api-key",
        ELEVENLABS_FIXTURE_KEY,
    )
    .await;

    // The doc's initialization message: a single blank space.
    socket
        .send(Message::text("{\"text\":\" \"}"))
        .await
        .expect("init");
    // One turn's text, space-terminated per the doc, flushed because the
    // turn is complete (the elevenlabs-first-chunk-buffer entry pins why).
    socket
        .send(Message::text(
            "{\"text\":\"Hi there again. \",\"flush\":true}",
        ))
        .await
        .expect("text");

    let first = next_json(&mut socket).await;
    let second = next_json(&mut socket).await;
    let final_message = next_json(&mut socket).await;
    assert_eq!(final_message["isFinal"], true);
    assert!(
        final_message["audio"].is_null(),
        "the doc: audio is null when isFinal is set"
    );

    // The recorded proof of chunk-relativity: the second chunk's raw
    // timeline restarts at zero even though its audio follows the first
    // chunk's 1000 ms span.
    assert_eq!(second["alignment"]["charStartTimesMs"][0], 0);

    let words = accumulate_chunk_relative_alignment(&[first, second]);
    assert_eq!(
        words,
        vec![
            TimedWord {
                w: "Hi".into(),
                start_ms: 0,
                end_ms: 240,
            },
            TimedWord {
                w: "there".into(),
                start_ms: 300,
                end_ms: 900,
            },
            TimedWord {
                w: "again.".into(),
                start_ms: 1000,
                end_ms: 1580,
            },
        ],
        "the running offset carries chunk 2 past chunk 1's span"
    );
    assert!(
        words.windows(2).all(|w| w[0].end_ms <= w[1].start_ms),
        "accumulated words are stream-monotonic — the naive absolute read \
         (again. at 0 ms, before there ends at 900 ms) cannot be"
    );

    socket.close(None).await.expect("driver close");
    replayer.await.expect("the recorded fixture replays green");
}

/// The `elevenlabs-first-chunk-buffer` roster entry (VOICE_AGENT §3.2).
///
/// Official doc (https://elevenlabs.io/docs/api-reference/text-to-speech/v-1-text-to-speech-voice-id-stream-input,
/// accessed 2026-08-22): input text accumulates in a server-side buffer
/// governed by `chunk_length_schedule`, default `[120, 160, 250, 290]` — the
/// first audio chunk does not generate until at least **120 characters** are
/// buffered. `flush: true` on a text message "forces the generation of audio"
/// regardless of buffer fill; the empty-string message is connection close,
/// not a mid-stream flush. The §3.2 contract ("first audio is buffered until
/// ~120 characters unless explicitly flushed — schedule the first chunk
/// accordingly"): a voice turn's text is typically far under 120 characters,
/// so the slot must send its turn-final text **with an explicit flush** —
/// waiting out the schedule would stall the first audio chunk of nearly
/// every reply.
///
/// The fixture pins exactly that: a 7-character turn whose text message
/// carries `flush: true`, answered by audio despite the schedule's 120-char
/// floor — plus the same credential-free connect shape as the alignment
/// entry.
#[tokio::test]
async fn elevenlabs_short_turn_flushes_the_first_chunk_buffer() {
    let handle = WireServer::start().await;
    let fixture = load_fixture("elevenlabs-first-chunk-buffer");
    let replay_handle = handle.clone();
    let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

    let mut socket = connect_with_header(
        handle.url("/v1/text-to-speech/voice-abc/stream-input?model_id=eleven_flash_v2_5"),
        "xi-api-key",
        ELEVENLABS_FIXTURE_KEY,
    )
    .await;

    socket
        .send(Message::text("{\"text\":\" \"}"))
        .await
        .expect("init");

    // The whole turn is 7 characters — under every rung of the default
    // chunk_length_schedule. The contract move: flush rides the turn-final
    // text message instead of trusting the 120-char floor.
    let turn = "Go on. ";
    assert!(
        turn.len() < 120,
        "the entry is about a turn under the schedule's first rung"
    );
    socket
        .send(Message::text(
            "{\"text\":\"Go on. \",\"flush\":true,\"try_trigger_generation\":false}",
        ))
        .await
        .expect("turn text");

    let audio = next_json(&mut socket).await;
    assert!(
        audio["audio"].is_string(),
        "the flush forced first audio despite the 120-char schedule floor"
    );
    let final_message = next_json(&mut socket).await;
    assert_eq!(final_message["isFinal"], true);

    socket.close(None).await.expect("driver close");
    replayer.await.expect("the recorded fixture replays green");
}

/// Fish Audio client events (doc: msgpack-serialized maps keyed by `event`).
/// Serialized with `rmp_serde::to_vec_named` so the wire carries msgpack maps
/// with string keys — the exact framing the fixtures pin byte-for-byte.
#[derive(serde::Serialize)]
struct FishStart<'a> {
    event: &'a str,
    request: FishRequest<'a>,
}

#[derive(serde::Serialize)]
struct FishRequest<'a> {
    text: &'a str,
    format: &'a str,
    chunk_length: u32,
    latency: &'a str,
    reference_id: &'a str,
}

#[derive(serde::Serialize)]
struct FishText<'a> {
    event: &'a str,
    text: &'a str,
}

#[derive(serde::Serialize)]
struct FishSignal<'a> {
    event: &'a str,
}

fn fish_frame(event: &impl serde::Serialize) -> Message {
    Message::binary(rmp_serde::to_vec_named(event).expect("msgpack encode"))
}

/// Walks one fixstr at `at`, returning it and advancing the cursor — the
/// framing entry reads server events marker-by-marker instead of through a
/// decoder, so a wrong wire format cannot hide behind a lenient library.
fn read_fixstr(frame: &[u8], at: &mut usize) -> String {
    let marker = frame[*at];
    assert_eq!(marker & 0xe0, 0xa0, "expected a msgpack fixstr marker");
    let len = (marker & 0x1f) as usize;
    *at += 1;
    let text = std::str::from_utf8(&frame[*at..*at + len])
        .expect("fixstr utf-8")
        .to_string();
    *at += len;
    text
}

/// Structural parse of the server's audio event
/// (`{"event": "audio", "audio": <bin>}`): fixmap-2, fixstr keys, and the
/// audio payload as msgpack **bin** (raw bytes), never an int array or
/// base64-in-JSON. Returns the PCM16 LE samples.
fn parse_fish_audio_event(frame: &[u8]) -> Vec<i16> {
    assert_eq!(frame[0], 0x82, "audio event is a msgpack fixmap-2");
    let mut at = 1;
    assert_eq!(read_fixstr(frame, &mut at), "event");
    assert_eq!(read_fixstr(frame, &mut at), "audio");
    assert_eq!(read_fixstr(frame, &mut at), "audio");
    assert_eq!(
        frame[at], 0xc4,
        "the audio payload is msgpack bin8 — raw bytes on the wire"
    );
    let len = frame[at + 1] as usize;
    at += 2;
    assert_eq!(at + len, frame.len(), "nothing trails the audio bytes");
    frame[at..at + len]
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

/// Structural parse of `{"event": "finish", "reason": …}` → the reason.
fn parse_fish_finish_event(frame: &[u8]) -> String {
    assert_eq!(frame[0], 0x82, "finish event is a msgpack fixmap-2");
    let mut at = 1;
    assert_eq!(read_fixstr(frame, &mut at), "event");
    assert_eq!(read_fixstr(frame, &mut at), "finish");
    assert_eq!(read_fixstr(frame, &mut at), "reason");
    read_fixstr(frame, &mut at)
}

/// The `fish-msgpack-framing` roster entry (VOICE_AGENT §3.2).
///
/// Official doc (https://docs.fish.audio/api-reference/endpoint/websocket/tts-live,
/// accessed 2026-08-22): every message on `wss://api.fish.audio/v1/tts/live`
/// is `application/msgpack` — msgpack maps in **binary** WebSocket frames,
/// never JSON text. Client events are `start` (carrying the HTTP-shaped
/// `request`), `text`, `flush`, `stop`; server events are `audio` (the
/// `audio` value is raw bytes — msgpack bin) and `finish`
/// (`reason: "stop" | "error"`). Auth is `Authorization: Bearer …` on the
/// upgrade, model selection is the optional `model` header (`s2.1-pro-free`
/// is the §3.2 pin) — nothing credential-shaped in the URL.
///
/// The fixture pins the framing byte-for-byte: the client's start/text/flush/
/// stop frames are exact msgpack payloads, and the test walks the server's
/// audio event marker-by-marker (fixmap → fixstr keys → bin8 payload), so a
/// slot that slipped into JSON text framing — or base64-wrapped audio — reds
/// on the first frame.
#[tokio::test]
async fn fish_wire_is_msgpack_binary_frames() {
    let handle = WireServer::start().await;
    let fixture = load_fixture("fish-msgpack-framing");
    let replay_handle = handle.clone();
    let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

    let bearer = format!("Bearer {FISH_FIXTURE_KEY}");
    let mut socket = connect_with_headers(
        handle.url("/v1/tts/live"),
        &[
            ("authorization", bearer.as_str()),
            ("model", "s2.1-pro-free"),
        ],
    )
    .await;

    socket
        .send(fish_frame(&FishStart {
            event: "start",
            request: FishRequest {
                text: "",
                format: "pcm",
                chunk_length: 300,
                latency: "normal",
                reference_id: "ref-fixture",
            },
        }))
        .await
        .expect("start");
    socket
        .send(fish_frame(&FishText {
            event: "text",
            text: "Cover the axes. ",
        }))
        .await
        .expect("text");
    socket
        .send(fish_frame(&FishSignal { event: "flush" }))
        .await
        .expect("flush");

    let audio = next_binary(&mut socket).await;
    assert_eq!(
        parse_fish_audio_event(&audio),
        vec![1000, -1000, i16::MAX, i16::MIN],
        "bin8 audio bytes decode as PCM16 LE — both i16 extremes present so \
         a byte-order slip cannot pass"
    );

    socket
        .send(fish_frame(&FishSignal { event: "stop" }))
        .await
        .expect("stop");
    let finish = next_binary(&mut socket).await;
    assert_eq!(parse_fish_finish_event(&finish), "stop");

    replayer.await.expect("the recorded fixture replays green");
}

/// The `fish-stop-flush` roster entry (VOICE_AGENT §3.2).
///
/// Official doc (https://docs.fish.audio/api-reference/endpoint/websocket/tts-live,
/// accessed 2026-08-22): the `stop` event "signals the end of the text
/// stream" — after receiving it "the server will finish synthesizing any
/// remaining buffered text and send a FinishEvent before closing the
/// connection." Stop is therefore an implicit flush, and the §3.2 contract
/// for the future slot is the drain side of that: after sending `stop`, keep
/// reading — the buffered tail's audio arrives **after** the stop went out,
/// and the session is over only at `finish` (`reason: "stop"` = clean;
/// `"error"` = failed), never at the slot's own last send.
///
/// The fixture records exactly that ordering: text buffered with no flush
/// event, `stop`, and only then the buffered tail's audio followed by
/// `finish` and the server's close.
#[tokio::test]
async fn fish_stop_flushes_the_buffered_tail_before_finish() {
    let handle = WireServer::start().await;
    let fixture = load_fixture("fish-stop-flush");
    let replay_handle = handle.clone();
    let replayer = tokio::spawn(async move { replay(&fixture, &replay_handle).await });

    let bearer = format!("Bearer {FISH_FIXTURE_KEY}");
    let mut socket = connect_with_headers(
        handle.url("/v1/tts/live"),
        &[
            ("authorization", bearer.as_str()),
            ("model", "s2.1-pro-free"),
        ],
    )
    .await;

    socket
        .send(fish_frame(&FishStart {
            event: "start",
            request: FishRequest {
                text: "",
                format: "pcm",
                chunk_length: 300,
                latency: "normal",
                reference_id: "ref-fixture",
            },
        }))
        .await
        .expect("start");

    // The turn's tail goes into the server buffer with NO flush event — the
    // stop itself is what flushes it.
    socket
        .send(fish_frame(&FishText {
            event: "text",
            text: "And the summary. ",
        }))
        .await
        .expect("text");
    socket
        .send(fish_frame(&FishSignal { event: "stop" }))
        .await
        .expect("stop");

    // The drain contract: the buffered tail's audio lands after our stop.
    let audio = next_binary(&mut socket).await;
    assert_eq!(
        parse_fish_audio_event(&audio),
        vec![1000, -1000, i16::MAX, i16::MIN],
        "audio synthesized from the buffered tail arrives after stop — \
         dropping the read on stop would lose the turn's ending"
    );
    let finish = next_binary(&mut socket).await;
    assert_eq!(
        parse_fish_finish_event(&finish),
        "stop",
        "the clean-completion reason; the session ends here, not at our send"
    );

    replayer.await.expect("the recorded fixture replays green");
}

/// The `openai-headerless-pcm` roster entry (VOICE_AGENT §3.2).
///
/// Official doc (https://developers.openai.com/api/docs/guides/text-to-speech,
/// accessed 2026-08-22; endpoint reference:
/// https://developers.openai.com/api/reference/resources/audio/subresources/speech/methods/create):
/// `POST /v1/audio/speech` with `response_format: "pcm"` streams "the raw
/// samples in 24kHz (16-bit signed, low-endian), without the header" —
/// headerless PCM, unlike `wav` which prepends RIFF. Auth is the
/// `Authorization: Bearer` header on the HTTP request; nothing rides the URL.
///
/// This provider's wire is a streamed **HTTP response body**, not a
/// WebSocket, so there is no upgrade to replay through [`WireServer`]; the
/// recorded shape is the body's chunk sequence, carried in the shared
/// [`WireStep::SendBinary`] fixture format (same loader, same provenance
/// rules — no second harness). The test pins the §3.2 contract:
///
/// - no container magic leads the stream — a slot that parses for a RIFF/WAV
///   header would wait forever or mangle the first samples;
/// - 24 kHz is [`PROVIDER_OUTPUT_RATE`] exactly — the bytes are the
///   `AudioDelta` contract as-is, no resample and no transcode;
/// - HTTP chunk boundaries owe nothing to sample boundaries — the recorded
///   first chunk is odd-length, so a per-chunk PCM16 decode misparses every
///   later sample; the slot must carry the dangling byte across chunks.
#[test]
fn openai_tts_pcm_stream_is_headerless_and_boundary_blind() {
    let fixture = load_fixture("openai-headerless-pcm");
    let chunks: Vec<Vec<u8>> = fixture
        .steps
        .iter()
        .map(|step| match step {
            WireStep::SendBinary { base64 } => base64::engine::general_purpose::STANDARD
                .decode(base64)
                .expect("recorded chunk decodes"),
            other => panic!("a recorded body stream is send-only, found {other:?}"),
        })
        .collect();

    // Headerless: none of the sibling formats' magic leads the stream.
    for magic in [&b"RIFF"[..], b"ID3", b"OggS", b"fLaC"] {
        assert!(
            !chunks[0].starts_with(magic),
            "the doc's pcm format carries raw samples without the header"
        );
    }

    // The recorded chunk boundary splits a sample: byte-carry is mandatory.
    assert_eq!(chunks[0].len() % 2, 1, "the recorded first chunk is odd");
    let stream: Vec<u8> = chunks.concat();
    assert_eq!(
        stream.len() % 2,
        0,
        "the full stream is whole PCM16 samples"
    );
    let samples: Vec<i16> = stream
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    assert_eq!(
        samples,
        vec![1000, -1000, i16::MAX, i16::MIN, 2, -3],
        "16-bit signed little-endian, decoded across the chunk seam — both \
         i16 extremes present so a byte-order slip cannot pass"
    );

    // A boundary-naive decode of chunk 2 alone shifts every sample by one
    // byte — the recorded proof that the carry is not optional.
    let naive_second: Vec<i16> = chunks[1]
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    assert_ne!(
        naive_second[0], samples[2],
        "dropping the dangling byte misparses the stream from there on"
    );

    // The doc's 24 kHz is the AudioDelta contract rate exactly.
    assert_eq!(crate::voice::audio::PROVIDER_OUTPUT_RATE, 24_000);
}

/// A query-param name that could plausibly carry a credential. The scan bans
/// by name shape rather than blanket-banning query strings: legitimate
/// protocol-metadata params exist all over this corpus (`cartesia_version`,
/// `model`, `sample_rate`, `encoding`, `model_id`, `sync_alignment`) and stay
/// legal by construction — none of them is key/token/auth-shaped.
fn credential_like_param_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "key",
        "token",
        "secret",
        "auth",
        "credential",
        "password",
        "signature",
        "bearer",
    ]
    .iter()
    .any(|marker| name.contains(marker))
}

/// A query-param value that looks like a credential: a fixture sentinel key,
/// or a long unbroken base62/`-`/`_` blob (real API keys are exactly that;
/// real protocol metadata in this corpus never is).
fn key_shaped_param_value(value: &str) -> bool {
    value.contains("fixture-")
        || (value.len() >= 20
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// Scans one connect target's query string under the auth-hygiene law.
fn scan_connect_target(target: &str, source: &str) {
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        assert!(
            !credential_like_param_name(name),
            "{source}: query param `{name}` is credential-shaped — creds ride \
             headers, never the URL"
        );
        assert!(
            !key_shaped_param_value(value),
            "{source}: query param `{name}` carries a key-shaped value: {value}"
        );
    }
}

/// The `auth-hygiene-no-query-param-creds` roster entry (VOICE_AGENT §3.2).
///
/// The hygiene rule (module headers of every adapter; §3.2/§7): credentials
/// travel in request headers on the upgrade — never in the connect URL, where
/// they leak into logs, proxies, and browser history. Several providers
/// officially offer a query-param scheme (Cartesia's browser `access_token`,
/// ElevenLabs' `authorization`/`single_use_token`) — those schemes are banned
/// here outright; each fixture's `target_absent` needles pin the ban
/// per-adapter.
///
/// This is a **scan-style** entry, not a session recording — two sweeps:
///
/// 1. **Fixture corpus**: every recorded fixture's handshake expectations
///    obey the law by construction — any query param a fixture *pins*
///    (`target_contains`) must be non-credential by name and value, and every
///    handshake pin must actively ban at least one credential-shaped needle
///    (`target_absent`), so no future fixture can quietly stop checking.
/// 2. **Request builders**: the implemented slots build their connect URLs
///    live against the wire rig with a sentinel key installed; the recorded
///    upgrade target is scanned generically and the sentinel must appear in
///    a header, never the target. (Each full channel's own fixtures pin the
///    same absence adapter-specifically on every recorded session; the
///    openai_compat LLM slot pins its HTTP variant in its own module.)
#[tokio::test]
async fn auth_hygiene_no_query_param_creds() {
    // Sweep 1: the recorded corpus.
    let mut fixtures_scanned = 0;
    let mut handshakes_scanned = 0;
    for entry in std::fs::read_dir(fixture_dir()).expect("fixture dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("fixture name");
        let fixture = load_fixture(name);
        fixtures_scanned += 1;
        for step in &fixture.steps {
            let WireStep::ExpectHandshake {
                target_contains,
                target_absent,
                ..
            } = step
            else {
                continue;
            };
            handshakes_scanned += 1;
            for needle in target_contains {
                if let Some((param_name, value)) = needle.split_once('=') {
                    let param_name = param_name.rsplit(['?', '&']).next().unwrap_or(param_name);
                    assert!(
                        !credential_like_param_name(param_name),
                        "fixture {name} pins credential-shaped query param `{param_name}`"
                    );
                    assert!(
                        !key_shaped_param_value(value),
                        "fixture {name} pins a key-shaped query value in `{needle}`"
                    );
                }
            }
            assert!(
                target_absent.iter().any(|needle| {
                    credential_like_param_name(needle) || needle.contains("fixture-")
                }),
                "fixture {name}: every handshake pin must actively ban a \
                 credential-shaped needle in target_absent"
            );
        }
    }
    assert!(
        fixtures_scanned >= 12 && handshakes_scanned >= 12,
        "the roster corpus is present ({fixtures_scanned} fixtures, \
         {handshakes_scanned} handshake pins scanned)"
    );

    // Sweep 2: the live request builders, sentinel key installed.
    const SENTINEL: &str = "fixture-scan-sentinel-key-a1b2c3d4e5f6";
    let keys_with = |base_url: &str| {
        let store = KeyStore::with_forced_backend(
            std::path::PathBuf::from("/unused/keys.vault"),
            Backend::Session(Default::default()),
        );
        store
            .store(base_url, &ApiKey::new(SENTINEL.into()))
            .unwrap();
        Arc::new(KeyManager::with_store(store))
    };

    // CartesiaTtsSlot — the §7 key identity is its default base URL.
    let handle = WireServer::start().await;
    let mut tts = CartesiaTtsSlot::new(
        TtsSlotConfig {
            endpoint_override: Some(handle.url("/tts/websocket")),
            model: "sonic-2".into(),
            voice: "voice-abc".into(),
        },
        keys_with("https://api.cartesia.ai"),
    );
    tts.connect().await.expect("cartesia slot connect");
    let record = handle.wait_for_connection(1).await;
    assert!(
        !record.target.contains(SENTINEL),
        "CartesiaTtsSlot: the key must never ride the connect URL"
    );
    scan_connect_target(&record.target, "CartesiaTtsSlot");
    assert_eq!(
        record.header("x-api-key"),
        Some(SENTINEL),
        "the key rides the X-API-Key header instead"
    );
    drop(tts);

    // DeepgramSttSlot — same law over the listen wire.
    let handle = WireServer::start().await;
    let mut stt = DeepgramSttSlot::new(
        SttSlotConfig {
            endpoint_override: Some(handle.url("/v1/listen")),
            model: "nova-3".into(),
        },
        keys_with("https://api.deepgram.com"),
        Generation::new(),
    );
    stt.connect().await.expect("deepgram slot connect");
    let record = handle.wait_for_connection(1).await;
    assert!(
        !record.target.contains(SENTINEL),
        "DeepgramSttSlot: the key must never ride the connect URL"
    );
    scan_connect_target(&record.target, "DeepgramSttSlot");
    let authorization = record
        .header("authorization")
        .expect("the key rides the Authorization header instead");
    assert!(authorization.contains(SENTINEL));
    drop(stt);
}
