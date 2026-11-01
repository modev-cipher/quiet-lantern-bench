//! The channel conformance suite (VA2 unit 3) — the mold every adapter must fit.
//!
//! Each `check_*` function is generic over a [`ConformanceFixture`], which wires
//! a fresh channel to a scripted provider: the deterministic
//! [`super::mock::MockChannel`] in CI (the tests at the bottom of this file are
//! that run), and each future adapter's recorded-fixture replay rig when those
//! land (VOICE_AGENT §3, §10 — CI never opens a socket or reads a key). An
//! adapter that passes every check honors the contracts the session harness is
//! built on; one suite, every provider, no provider-shaped exceptions.
//!
//! What the suite pins, by check:
//! 1. open/close lifecycle — `Connected` is the first event, `take_events`
//!    yields exactly once, sends outside a session fail `InvalidState`, close is
//!    idempotent and ends the event stream.
//! 2. transcript ordering — deltas arrive in emission order, payload-pinned.
//! 3. barge-in — the generation bump reaches the playback gate before the
//!    provider cancel goes out, and after the bump at most the one in-flight
//!    device buffer of superseded audio can play (VOICE_AGENT §2 rule 2; the
//!    VA1 gate arithmetic in `voice::audio::generation`).
//! 4. tool-call round-trip — a tool call surfaces intact and its result lands
//!    at the provider under the same `call_id`, exactly once.
//! 5. teacher-anchor normalization — live anchors emit + ack, superseded-
//!    generation anchors are dropped but still acked, malformed args are
//!    rejected as out-of-schema (VOICE_AGENT §11.1, §5).
//! 6. renewal-recycle — every accepted frame lands in exactly one epoch,
//!    payload-pinned; a send rejected during renewal reaches the provider
//!    never, not later (VOICE_AGENT §2 rule 3, §9).
//! 7. session-end announcement mid-response — response audio emitted before the
//!    announcement is still delivered, in order, with nothing lost.

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

use super::anchor::{
    normalize_teacher_anchor, AnchorDecision, OUT_OF_SCHEMA_CODE, STALE_GENERATION_CODE,
    TEACHER_ANCHOR_TOOL,
};
use super::event::{AnchorKind, AnchorRef, ChannelEvent};
use super::types::{
    ContextUpdate, DelimitedExcerpt, ErrorKind, LifecycleEvent, Role, SessionContext, ToolResult,
};
use super::{barge_in, SessionChannel};
use crate::voice::audio::device::PlaybackSource;
use crate::voice::audio::generation::{tagged_playback, Generation};
use crate::voice::audio::{frames_per_chunk, MIC_CHUNK_MS, PROVIDER_OUTPUT_RATE};

/// The scripted provider behind a channel under test: what the suite injects
/// and what it reads back. The mock driver implements it directly; an adapter's
/// replay rig implements it by scripting wire frames and observing its socket.
#[async_trait]
pub trait ProviderSim: Send {
    async fn emit_audio(&mut self, gen: u64, pcm: Vec<i16>);
    async fn emit_transcript(&mut self, role: Role, text: String);
    async fn emit_tool_call(&mut self, call_id: String, name: String, args: serde_json::Value);
    /// Provider announces the session is ending (its session cap — VOICE_AGENT
    /// §3, §9): the channel must surface `Lifecycle(Renewing)`.
    async fn announce_session_end(&mut self);
    /// The recycle succeeded: the channel must surface `Lifecycle(Reconnected)`
    /// on the same receiver.
    async fn complete_renewal(&mut self);
    /// The session ended without recovery (VOICE_AGENT §3, §9): the channel
    /// must surface `Lifecycle(Expired)` on the same receiver.
    async fn expire(&mut self);
    /// Audio the provider accepted, grouped by renewal epoch, in order.
    async fn received_audio_by_epoch(&mut self) -> Vec<Vec<Vec<i16>>>;
    /// Text inputs the provider accepted, in order.
    async fn received_texts(&mut self) -> Vec<String>;
    /// Context updates the provider accepted, in order.
    async fn received_context_updates(&mut self) -> Vec<ContextUpdate>;
    /// Tool results the provider received, in order.
    async fn received_tool_results(&mut self) -> Vec<(String, ToolResult)>;
    /// For each cancel the provider received: the shared generation counter's
    /// value at that instant (the rig observes the suite's `Generation`).
    async fn received_interrupts(&mut self) -> Vec<Option<u64>>;
}

/// Builds one fresh channel + scripted provider per check.
#[async_trait]
pub trait ConformanceFixture {
    type Channel: SessionChannel;
    type Sim: ProviderSim;

    /// A fresh, unconnected channel wired to a fresh provider script. The rig
    /// must observe `generation` in its interrupt records so check 3 can prove
    /// the bump-before-cancel ordering.
    async fn rig(&mut self, generation: &Generation) -> (Self::Channel, Self::Sim);

    fn session_context(&self) -> SessionContext {
        SessionContext {
            book_id: Some("conformance-book".to_string()),
            session_id: "conformance-session".to_string(),
            system_prompt: "conformance tutor prompt".to_string(),
            excerpt: DelimitedExcerpt {
                delimited_text: "<sentinel>conformance excerpt</sentinel>".to_string(),
            },
            rolling_summary: None,
            playback_pos_ms: 0,
            tool_schemas: Vec::new(),
        }
    }
}

async fn recv(events: &mut mpsc::Receiver<ChannelEvent>) -> ChannelEvent {
    timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("conformance: no event within 5s")
        .expect("conformance: event stream ended unexpectedly")
}

/// [`recv`], skipping `UtteranceEnd`. The checks below pin *content* order
/// (audio, transcripts, tool calls); where a Server-VAD channel places its
/// utterance boundary relative to that content is a per-adapter mapping the
/// suite must not pin — Deepgram commits on the wire's own utterance markers,
/// Gemini Live at each fresh response-turn open (issue #201) — and each
/// adapter's placement is pinned by its own fixture tests instead.
async fn recv_content(events: &mut mpsc::Receiver<ChannelEvent>) -> ChannelEvent {
    // Bounded skip: tolerating the boundary must not tolerate a boundary
    // STORM. One utterance yields one `UtteranceEnd` (event.rs §3.2 rule);
    // between two content events at most a handful of boundaries can
    // legitimately accumulate, so an adapter re-emitting the signal per
    // frame (a #201-fix regression) fails the suite loudly here instead of
    // being skipped into a false green.
    const STORM_BOUND: usize = 4;
    for _ in 0..STORM_BOUND {
        let event = recv(events).await;
        // `TurnComplete` is the reply-side boundary (a Live `turnComplete`
        // or `interrupted` closing an opened reply, review 2026-10-09 J2):
        // tolerated exactly like the utterance boundary, under the same
        // storm bound.
        if !matches!(
            event,
            ChannelEvent::UtteranceEnd | ChannelEvent::TurnComplete
        ) {
            return event;
        }
    }
    panic!(
        "conformance: {STORM_BOUND} consecutive UtteranceEnd events — the \
         adapter is storming the turn boundary instead of committing once \
         per utterance"
    );
}

async fn expect_lifecycle(events: &mut mpsc::Receiver<ChannelEvent>, want: LifecycleEvent) {
    let event = recv(events).await;
    assert_eq!(
        event,
        ChannelEvent::Lifecycle(want),
        "conformance: expected Lifecycle({want:?})"
    );
}

/// Check 1: open/close lifecycle and event-stream ownership.
pub async fn check_open_close<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, _sim) = fixture.rig(&generation).await;

    let before = channel
        .send_audio(&[0i16; 16])
        .await
        .expect_err("conformance: send_audio before connect must fail");
    assert_eq!(before.kind, ErrorKind::InvalidState);
    assert!(!before.retryable, "pre-connect sends are not retryable");

    let mut events = channel
        .take_events()
        .expect("conformance: first take_events must yield the receiver");
    assert!(
        channel.take_events().is_none(),
        "conformance: take_events yields exactly once per session"
    );

    channel
        .connect(fixture.session_context())
        .await
        .expect("conformance: connect on a fresh channel succeeds");
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    let caps = channel.capabilities();
    if caps.text_only {
        let refused = channel
            .send_audio(&[0i16; 16])
            .await
            .expect_err("conformance: send_audio on a connected text_only channel must fail");
        assert_eq!(refused.kind, ErrorKind::InvalidState);
        assert!(!refused.retryable);
        channel
            .finalize_input()
            .await
            .expect("conformance: finalize_input is a harmless no-op on a text wire");
    }

    channel.close().await.expect("conformance: close succeeds");
    channel
        .close()
        .await
        .expect("conformance: close is idempotent");
    assert_eq!(
        timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("conformance: stream end within 5s"),
        None,
        "conformance: the event stream ends at close"
    );
    let after = channel
        .send_text("late")
        .await
        .expect_err("conformance: sends after close must fail");
    assert_eq!(after.kind, ErrorKind::InvalidState);
}

/// Check 2: transcript deltas arrive in emission order, payload-pinned.
pub async fn check_transcript_ordering<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, mut sim) = fixture.rig(&generation).await;
    if channel.capabilities().text_only {
        return check_transcript_ordering_text_only(
            channel,
            sim,
            generation,
            fixture.session_context(),
        )
        .await;
    }
    let mut events = channel.take_events().unwrap();
    channel.connect(fixture.session_context()).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    let script = [
        (Role::Assistant, "The figure "),
        (Role::Assistant, "shows the "),
        (Role::User, "wait — which figure?"),
        (Role::Assistant, "Figure 3, on this page."),
    ];
    for (role, text) in &script {
        sim.emit_transcript(*role, text.to_string()).await;
    }
    for (role, text) in &script {
        assert_eq!(
            recv_content(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: *role,
                text: text.to_string()
            },
            "conformance: transcript deltas must arrive in emission order, intact"
        );
    }
}

/// Check 3: barge-in — bump-before-cancel, and after the bump at most the one
/// in-flight device buffer of superseded audio plays (VOICE_AGENT §2 rule 2).
pub async fn check_barge_in<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, mut sim) = fixture.rig(&generation).await;
    if channel.capabilities().text_only {
        return check_barge_in_text_only(channel, sim, generation, fixture.session_context()).await;
    }
    let mut events = channel.take_events().unwrap();
    channel.connect(fixture.session_context()).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    let frame = frames_per_chunk(PROVIDER_OUTPUT_RATE, MIC_CHUNK_MS);
    let mut pcm = super::mock::SeededPcm::new(0x5ba2_0003);
    let stale_a = pcm.block(frame);
    let stale_b = pcm.block(frame);
    let stale_late = pcm.block(frame);
    let live_block = pcm.block(frame);

    let (mut writer, mut source) = tagged_playback(generation.clone(), frame * 8, 8);
    fn push_delta(
        writer: &mut crate::voice::audio::generation::TaggedPlaybackWriter,
        got: ChannelEvent,
    ) -> usize {
        match got {
            ChannelEvent::AudioDelta { pcm, gen } => writer.push_block(gen, &pcm),
            other => panic!("conformance: expected AudioDelta, got {other:?}"),
        }
    }

    sim.emit_audio(0, stale_a.clone()).await;
    sim.emit_audio(0, stale_b.clone()).await;
    let got = recv_content(&mut events).await;
    assert_eq!(push_delta(&mut writer, got), frame);
    let got = recv_content(&mut events).await;
    assert_eq!(push_delta(&mut writer, got), frame);

    let mut buffer = vec![0i16; frame];
    assert_eq!(source.fill_playback(&mut buffer), frame);
    assert_eq!(
        buffer, stale_a,
        "conformance: the pre-bump buffer is the response being played — this is \
         the one frame the barge-in bound charges"
    );

    let live = barge_in(&generation, &mut channel)
        .await
        .expect("conformance: barge_in succeeds on a connected channel");
    assert_eq!(live, 1);
    assert_eq!(
        sim.received_interrupts().await,
        vec![Some(1)],
        "conformance: the provider cancel must observe the already-bumped \
         generation — local stop first, network second"
    );

    sim.emit_audio(0, stale_late.clone()).await;
    let got = recv_content(&mut events).await;
    assert_eq!(
        push_delta(&mut writer, got),
        0,
        "conformance: superseded-generation audio still in flight is refused at \
         the playback gate"
    );

    sim.emit_audio(1, live_block.clone()).await;
    let got = recv_content(&mut events).await;
    assert_eq!(push_delta(&mut writer, got), frame);

    let mut after = vec![0i16; frame];
    assert_eq!(source.fill_playback(&mut after), frame);
    assert_eq!(
        after, live_block,
        "conformance: the first buffer after the bump is the new response — zero \
         samples of the superseded one"
    );
    assert_eq!(
        source.skipped_frames() as usize,
        frame,
        "conformance: the queued-but-unplayed superseded block was discarded whole"
    );
    assert_eq!(
        writer.refused_stale_frames() as usize,
        frame,
        "conformance: the post-bump stale delta never entered the ring"
    );
}

/// Check 4: a tool call surfaces intact and its result reaches the provider
/// under the same `call_id`, exactly once.
pub async fn check_tool_call_round_trip<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, mut sim) = fixture.rig(&generation).await;
    let mut events = channel.take_events().unwrap();
    channel.connect(fixture.session_context()).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    if channel.capabilities().stt_only {
        let err = channel
            .send_tool_result("call-7", ToolResult::Ok(json!({"position_ms": 41_000})))
            .await
            .expect_err("conformance: STT-only channel must refuse send_tool_result");
        assert_eq!(err.kind, ErrorKind::InvalidState);
        assert!(!err.retryable);
        assert!(
            sim.received_tool_results().await.is_empty(),
            "conformance: refused tool result must not reach provider"
        );
        return;
    }

    let args = json!({"seconds": -30});
    sim.emit_tool_call(
        "call-7".to_string(),
        "seek_relative".to_string(),
        args.clone(),
    )
    .await;
    let got = recv_content(&mut events).await;
    assert_eq!(
        got,
        ChannelEvent::ToolCall {
            call_id: "call-7".to_string(),
            name: "seek_relative".to_string(),
            args,
        },
        "conformance: the tool call must surface with id, name and args intact"
    );

    let result = ToolResult::Ok(json!({"position_ms": 41_000}));
    channel
        .send_tool_result("call-7", result.clone())
        .await
        .expect("conformance: sending a tool result on a live session succeeds");
    assert_eq!(
        sim.received_tool_results().await,
        vec![("call-7".to_string(), result)],
        "conformance: exactly one result, under the calling id"
    );
}

/// Check 5: teacher-anchor normalization — live emits + acks, superseded drops
/// but acks, malformed rejects (VOICE_AGENT §11.1 staleness rule, §5 schema
/// policy).
pub async fn check_teacher_anchor<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, mut sim) = fixture.rig(&generation).await;
    if channel.capabilities().text_only {
        return check_teacher_anchor_text_only(channel, sim, generation, fixture.session_context())
            .await;
    }
    let mut events = channel.take_events().unwrap();
    channel.connect(fixture.session_context()).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    let surface = |call: ChannelEvent, response_gen: u64| -> (String, AnchorDecision) {
        match call {
            ChannelEvent::ToolCall {
                call_id,
                name,
                args,
            } => {
                assert_eq!(name, TEACHER_ANCHOR_TOOL);
                (
                    call_id,
                    normalize_teacher_anchor(&args, response_gen, &generation),
                )
            }
            other => panic!("conformance: expected the anchor tool call, got {other:?}"),
        }
    };

    sim.emit_tool_call(
        "anchor-1".to_string(),
        TEACHER_ANCHOR_TOOL.to_string(),
        json!({"kind": "figure", "id": "fig-2"}),
    )
    .await;
    let got = recv_content(&mut events).await;
    let (call_id, decision) = surface(got, 0);
    let AnchorDecision::Emit { event, ack } = decision else {
        panic!("conformance: a live valid anchor must Emit, got {decision:?}");
    };
    assert_eq!(
        event,
        ChannelEvent::TeacherAnchor {
            anchor: AnchorRef {
                kind: AnchorKind::Figure,
                id: "fig-2".to_string()
            },
            gen: 0,
        }
    );
    channel.send_tool_result(&call_id, ack).await.unwrap();

    barge_in(&generation, &mut channel).await.unwrap();

    sim.emit_tool_call(
        "anchor-2".to_string(),
        TEACHER_ANCHOR_TOOL.to_string(),
        json!({"kind": "section", "id": "sec-9"}),
    )
    .await;
    let got = recv_content(&mut events).await;
    let (call_id, decision) = surface(got, 0);
    let AnchorDecision::Stale { ack } = decision else {
        panic!(
            "conformance: an anchor from the superseded generation must be \
             dropped (VOICE_AGENT §11.1), got {decision:?}"
        );
    };
    channel.send_tool_result(&call_id, ack).await.unwrap();

    sim.emit_tool_call(
        "anchor-3".to_string(),
        TEACHER_ANCHOR_TOOL.to_string(),
        json!({"kind": "page", "id": "p-4"}),
    )
    .await;
    let got = recv_content(&mut events).await;
    let (call_id, decision) = surface(got, 1);
    let AnchorDecision::Invalid { ack } = decision else {
        panic!("conformance: an out-of-vocabulary kind must be Invalid, got {decision:?}");
    };
    channel.send_tool_result(&call_id, ack).await.unwrap();

    let acks = sim.received_tool_results().await;
    assert_eq!(acks.len(), 3, "every anchor call is acked, dropped or not");
    assert_eq!(acks[0].0, "anchor-1");
    assert!(
        matches!(&acks[0].1, ToolResult::Ok(_)),
        "live anchor acks success"
    );
    assert!(
        matches!(&acks[1].1, ToolResult::Err { code, .. } if code == STALE_GENERATION_CODE),
        "superseded anchor acks {STALE_GENERATION_CODE}"
    );
    assert!(
        matches!(&acks[2].1, ToolResult::Err { code, .. } if code == OUT_OF_SCHEMA_CODE),
        "malformed anchor acks {OUT_OF_SCHEMA_CODE}"
    );
}

/// Check 6: renewal-recycle without frame loss — every accepted payload lands
/// in exactly one epoch; a renewal-rejected payload reaches the provider never.
pub async fn check_renewal_recycle<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, mut sim) = fixture.rig(&generation).await;
    let mut events = channel.take_events().unwrap();
    channel.connect(fixture.session_context()).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    let mut pcm = super::mock::SeededPcm::new(0x5ba2_0006);
    let mic = frames_per_chunk(crate::voice::audio::PROVIDER_INPUT_RATE, MIC_CHUNK_MS);
    let payload_a = pcm.block(mic);
    let payload_rejected = pcm.block(mic);
    let payload_b = pcm.block(mic);

    channel.send_audio(&payload_a).await.unwrap();

    sim.announce_session_end().await;
    expect_lifecycle(&mut events, LifecycleEvent::Renewing { time_left: None }).await;

    let rejected = channel
        .send_audio(&payload_rejected)
        .await
        .expect_err("conformance: sends during renewal must fail");
    assert!(
        rejected.retryable,
        "conformance: a renewal rejection is retryable — the session is coming back"
    );

    sim.complete_renewal().await;
    expect_lifecycle(&mut events, LifecycleEvent::Reconnected { resumed: false }).await;

    channel.send_audio(&payload_b).await.unwrap();

    let by_epoch = sim.received_audio_by_epoch().await;
    assert_eq!(
        by_epoch.len(),
        2,
        "conformance: one renewal produces exactly two epochs"
    );
    assert_eq!(
        by_epoch[0],
        vec![payload_a],
        "conformance: the pre-renewal payload lands in epoch 0, exactly once"
    );
    assert_eq!(
        by_epoch[1],
        vec![payload_b],
        "conformance: the post-renewal payload lands in epoch 1, exactly once"
    );
    for epoch in &by_epoch {
        assert!(
            !epoch.contains(&payload_rejected),
            "conformance: a rejected payload must never surface later — the \
             harness owns replay/discard, not the channel (VOICE_AGENT §2 rule 3)"
        );
    }
}

/// Check 7: a session-end announcement arriving mid-response loses none of the
/// audio the provider had already emitted, and the stream continues after the
/// recycle on the same receiver.
pub async fn check_session_end_mid_response<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, mut sim) = fixture.rig(&generation).await;
    let mut events = channel.take_events().unwrap();
    channel.connect(fixture.session_context()).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    let frame = frames_per_chunk(PROVIDER_OUTPUT_RATE, MIC_CHUNK_MS);
    let mut pcm = super::mock::SeededPcm::new(0x5ba2_0007);
    let delta_a = pcm.block(frame);
    let delta_b = pcm.block(frame);
    let delta_after = pcm.block(frame);

    sim.emit_audio(0, delta_a.clone()).await;
    sim.emit_audio(0, delta_b.clone()).await;
    sim.announce_session_end().await;

    assert_eq!(
        recv_content(&mut events).await,
        ChannelEvent::AudioDelta {
            pcm: delta_a,
            gen: 0
        },
        "conformance: audio emitted before the announcement is not lost"
    );
    assert_eq!(
        recv_content(&mut events).await,
        ChannelEvent::AudioDelta {
            pcm: delta_b,
            gen: 0
        },
        "conformance: ... and arrives in order"
    );
    expect_lifecycle(&mut events, LifecycleEvent::Renewing { time_left: None }).await;

    sim.complete_renewal().await;
    expect_lifecycle(&mut events, LifecycleEvent::Reconnected { resumed: false }).await;

    sim.emit_audio(0, delta_after.clone()).await;
    assert_eq!(
        recv_content(&mut events).await,
        ChannelEvent::AudioDelta {
            pcm: delta_after,
            gen: 0
        },
        "conformance: the same receiver keeps delivering after the recycle"
    );
}

/// Check 8 (validator-added): the control-plane send surface beyond tool
/// results — `update_context` in both §4 variants and `send_text`, the no-mic
/// fallback (VOICE_AGENT §3) — reaches the provider intact and in order on a
/// live session. Without this check an adapter with a broken `update_context`
/// or `send_text` passes the suite untested.
pub async fn check_context_update_and_text<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, mut sim) = fixture.rig(&generation).await;
    let mut events = channel.take_events().unwrap();
    channel.connect(fixture.session_context()).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    let boundary = ContextUpdate::ChapterBoundary {
        chapter_idx: 3,
        excerpt: DelimitedExcerpt {
            delimited_text: "<sentinel>chapter three excerpt</sentinel>".to_string(),
        },
    };
    let position = ContextUpdate::PlaybackPosition { pos_ms: 90_500 };
    channel
        .update_context(boundary.clone())
        .await
        .expect("conformance: a chapter-boundary refresh on a live session succeeds");
    channel
        .update_context(position.clone())
        .await
        .expect("conformance: a playback-position update on a live session succeeds");
    channel
        .send_text("what does the third equation mean?")
        .await
        .expect("conformance: the no-mic text fallback works on a live session");

    assert_eq!(
        sim.received_context_updates().await,
        vec![boundary, position],
        "conformance: context updates reach the provider intact, in order"
    );
    assert_eq!(
        sim.received_texts().await,
        vec!["what does the third equation mean?".to_string()],
        "conformance: the text input reaches the provider intact"
    );
}

/// Check 9 (validator-added): unrecovered session end (VOICE_AGENT §3 uniform
/// lifecycle, §9): a renewal that never recycles surfaces `Lifecycle(Expired)`
/// on the same receiver; sends during the renewal fail retryably, sends after
/// expiry fail non-retryably — the session is not coming back.
pub async fn check_session_expiry<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, mut sim) = fixture.rig(&generation).await;
    let mut events = channel.take_events().unwrap();
    channel.connect(fixture.session_context()).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    sim.announce_session_end().await;
    expect_lifecycle(&mut events, LifecycleEvent::Renewing { time_left: None }).await;

    let during = channel
        .update_context(ContextUpdate::PlaybackPosition { pos_ms: 1 })
        .await
        .expect_err("conformance: control-plane sends during renewal must fail too");
    assert!(
        during.retryable,
        "conformance: a renewal-window rejection is retryable — recovery is still possible"
    );

    sim.expire().await;
    expect_lifecycle(&mut events, LifecycleEvent::Expired).await;

    let audio = channel
        .send_audio(&[0i16; 16])
        .await
        .expect_err("conformance: sends after expiry must fail");
    assert!(
        !audio.retryable,
        "conformance: an expired session is not coming back — the failure must \
         not be retryable"
    );
    let text = channel
        .send_text("late")
        .await
        .expect_err("conformance: text after expiry must fail");
    assert!(!text.retryable);
}

/// Check 10: turn boundary on Server-VAD channels.
///
/// On a `VadMode::Server` channel, an utterance boundary must surface
/// between user speech and assistant response: exactly one `UtteranceEnd`
/// event after user input, before or when the assistant begins.
pub async fn check_turn_boundary<F: ConformanceFixture>(fixture: &mut F) {
    let generation = Generation::new();
    let (mut channel, mut sim) = fixture.rig(&generation).await;
    let caps = channel.capabilities();
    if caps.text_only || caps.vad_mode != crate::voice::channel::types::VadMode::Server {
        return;
    }
    let mut events = channel.take_events().unwrap();
    channel.connect(fixture.session_context()).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    sim.emit_transcript(Role::User, "user speech".to_string())
        .await;
    sim.emit_transcript(Role::Assistant, "assistant answer".to_string())
        .await;

    let mut user_seen = false;
    let mut boundary_seen = false;
    let mut assistant_seen = false;

    for _ in 0..5 {
        let ev = recv(&mut events).await;
        match ev {
            ChannelEvent::TranscriptDelta {
                role: Role::User,
                text,
            } => {
                assert!(!user_seen, "conformance: duplicate user transcript");
                assert_eq!(text, "user speech");
                user_seen = true;
            }
            ChannelEvent::UtteranceEnd => {
                assert!(
                    user_seen,
                    "conformance: UtteranceEnd must follow user speech"
                );
                assert!(
                    !boundary_seen,
                    "conformance: exactly one UtteranceEnd per utterance"
                );
                boundary_seen = true;
            }
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text,
            } => {
                assert!(
                    boundary_seen,
                    "conformance: UtteranceEnd must precede assistant response"
                );
                assert_eq!(text, "assistant answer");
                assistant_seen = true;
                break;
            }
            other => panic!("conformance: unexpected event in check_turn_boundary: {other:?}"),
        }
    }

    assert!(user_seen, "conformance: user speech was not received");
    assert!(boundary_seen, "conformance: UtteranceEnd was not received");
    assert!(
        assistant_seen,
        "conformance: assistant response was not received"
    );
}

/// The renewal-free checks (1–5, 8, 10): every adapter runs these, whatever its
/// wire can do.
pub async fn run_core<F: ConformanceFixture>(fixture: &mut F) {
    check_open_close(fixture).await;
    check_transcript_ordering(fixture).await;
    check_barge_in(fixture).await;
    check_tool_call_round_trip(fixture).await;
    check_teacher_anchor(fixture).await;
    check_context_update_and_text(fixture).await;
    check_turn_boundary(fixture).await;
}

/// The conformance checks applicable to STT-only channels (Deepgram listen).
pub async fn run_stt<F: ConformanceFixture>(fixture: &mut F) {
    check_open_close(fixture).await;
    check_tool_call_round_trip(fixture).await;
}

/// The renewal checks (6, 7, 9): each drives the uniform renewal contract
/// (`Renewing` → `Reconnected`/`Expired`), which only wires declaring
/// `session_renewal` can produce — a renewal-less wire (a hard cap ending in
/// a plain close) has no frame that could surface `Renewing`.
pub async fn run_renewal<F: ConformanceFixture>(fixture: &mut F) {
    check_renewal_recycle(fixture).await;
    check_session_end_mid_response(fixture).await;
    check_session_expiry(fixture).await;
}

/// Runs every check the channel's declared capabilities make meaningful,
/// against fresh rigs: the core checks always, the renewal checks iff the
/// channel declares `session_renewal`. Adapters get their whole conformance
/// run from this one call.
pub async fn run_all<F: ConformanceFixture>(fixture: &mut F) {
    run_core(fixture).await;
    let generation = Generation::new();
    let (channel, _sim) = fixture.rig(&generation).await;
    if channel.capabilities().session_renewal {
        run_renewal(fixture).await;
    }
}

async fn expect_silence(events: &mut mpsc::Receiver<ChannelEvent>, why: &str) {
    match timeout(Duration::from_millis(300), events.recv()).await {
        Err(_) => {}
        Ok(event) => panic!("conformance: {why}, got {event:?}"),
    }
}

fn refuse_audio(event: &ChannelEvent) {
    assert!(
        !matches!(event, ChannelEvent::AudioDelta { .. }),
        "conformance: a text_only channel emitted AudioDelta"
    );
}

async fn check_transcript_ordering_text_only<C: SessionChannel, S: ProviderSim>(
    mut channel: C,
    mut sim: S,
    _generation: Generation,
    ctx: SessionContext,
) {
    let mut events = channel.take_events().unwrap();
    channel.connect(ctx).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    channel.send_text("which figure?").await.unwrap();
    sim.emit_transcript(Role::Assistant, "The figure ".to_string())
        .await;
    sim.emit_transcript(Role::Assistant, "shows the ".to_string())
        .await;
    for want in ["The figure ", "shows the "] {
        let got = recv_content(&mut events).await;
        refuse_audio(&got);
        assert_eq!(
            got,
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text: want.to_string()
            },
            "conformance: text deltas arrive in emission order, intact"
        );
    }
    channel.send_text("wait — which figure?").await.unwrap();
    sim.emit_transcript(Role::Assistant, "Figure 3, on this page.".to_string())
        .await;
    let got = recv_content(&mut events).await;
    assert_eq!(
        got,
        ChannelEvent::TranscriptDelta {
            role: Role::Assistant,
            text: "Figure 3, on this page.".to_string()
        }
    );
    assert_eq!(
        sim.received_texts().await,
        vec![
            "which figure?".to_string(),
            "wait — which figure?".to_string()
        ],
        "conformance: the reader's lines reach the provider intact, in order"
    );
}

async fn check_barge_in_text_only<C: SessionChannel, S: ProviderSim>(
    mut channel: C,
    mut sim: S,
    generation: Generation,
    ctx: SessionContext,
) {
    let mut events = channel.take_events().unwrap();
    channel.connect(ctx).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    channel.send_text("explain the diagram").await.unwrap();
    sim.emit_transcript(Role::Assistant, "The diagram ".to_string())
        .await;
    let got = recv_content(&mut events).await;
    refuse_audio(&got);
    assert!(matches!(got, ChannelEvent::TranscriptDelta { .. }));

    let live = barge_in(&generation, &mut channel).await.unwrap();
    assert_eq!(live, 1);
    assert_eq!(
        sim.received_interrupts().await,
        vec![Some(1)],
        "conformance: the cancel observes the already-bumped generation"
    );
    sim.emit_transcript(Role::Assistant, "plots pressure".to_string())
        .await;
    expect_silence(&mut events, "a superseded reply's tail must not surface").await;

    channel.send_text("shorter, please").await.unwrap();
    sim.emit_transcript(Role::Assistant, "Pressure vs temperature.".to_string())
        .await;
    let got = recv_content(&mut events).await;
    refuse_audio(&got);
    assert_eq!(
        got,
        ChannelEvent::TranscriptDelta {
            role: Role::Assistant,
            text: "Pressure vs temperature.".to_string()
        },
        "conformance: the next request's reply streams after the barge-in"
    );
}

async fn check_teacher_anchor_text_only<C: SessionChannel, S: ProviderSim>(
    mut channel: C,
    mut sim: S,
    generation: Generation,
    ctx: SessionContext,
) {
    let mut events = channel.take_events().unwrap();
    channel.connect(ctx).await.unwrap();
    expect_lifecycle(&mut events, LifecycleEvent::Connected).await;

    let surface = |call: ChannelEvent, response_gen: u64| -> (String, AnchorDecision) {
        match call {
            ChannelEvent::ToolCall {
                call_id,
                name,
                args,
            } => {
                assert_eq!(name, TEACHER_ANCHOR_TOOL);
                (
                    call_id,
                    normalize_teacher_anchor(&args, response_gen, &generation),
                )
            }
            other => panic!("conformance: expected the anchor tool call, got {other:?}"),
        }
    };

    channel.send_text("where is that figure?").await.unwrap();
    sim.emit_tool_call(
        "anchor-1".to_string(),
        TEACHER_ANCHOR_TOOL.to_string(),
        json!({"kind": "figure", "id": "fig-2"}),
    )
    .await;
    let (call_id, decision) = surface(recv_content(&mut events).await, 0);
    let AnchorDecision::Emit { ack, .. } = decision else {
        panic!("conformance: a live valid anchor must Emit, got {decision:?}");
    };
    channel.send_tool_result(&call_id, ack).await.unwrap();

    barge_in(&generation, &mut channel).await.unwrap();

    channel.send_text("and the section?").await.unwrap();
    sim.emit_tool_call(
        "anchor-2".to_string(),
        TEACHER_ANCHOR_TOOL.to_string(),
        json!({"kind": "section", "id": "sec-9"}),
    )
    .await;
    let (call_id, decision) = surface(recv_content(&mut events).await, 0);
    let AnchorDecision::Stale { ack } = decision else {
        panic!("conformance: an anchor from the superseded generation must be dropped, got {decision:?}");
    };
    channel.send_tool_result(&call_id, ack).await.unwrap();

    sim.emit_tool_call(
        "anchor-3".to_string(),
        TEACHER_ANCHOR_TOOL.to_string(),
        json!({"kind": "page", "id": "p-4"}),
    )
    .await;
    let (call_id, decision) = surface(recv_content(&mut events).await, 1);
    let AnchorDecision::Invalid { ack } = decision else {
        panic!("conformance: an out-of-vocabulary kind must be Invalid, got {decision:?}");
    };
    channel.send_tool_result(&call_id, ack).await.unwrap();

    let acks = sim.received_tool_results().await;
    assert_eq!(acks.len(), 3, "every anchor call is acked, dropped or not");
    assert_eq!(acks[0].0, "anchor-1");
    assert!(matches!(&acks[0].1, ToolResult::Ok(_)));
    assert!(matches!(&acks[1].1, ToolResult::Err { code, .. } if code == STALE_GENERATION_CODE));
    assert!(matches!(&acks[2].1, ToolResult::Err { code, .. } if code == OUT_OF_SCHEMA_CODE));
}

#[cfg(test)]
mod tests {
    use super::super::mock::{MockChannel, MockDriver};
    use super::*;

    struct MockSim {
        driver: MockDriver,
    }

    #[async_trait]
    impl ProviderSim for MockSim {
        async fn emit_audio(&mut self, gen: u64, pcm: Vec<i16>) {
            self.driver.emit_audio(gen, pcm);
        }
        async fn emit_transcript(&mut self, role: Role, text: String) {
            self.driver
                .emit(ChannelEvent::TranscriptDelta { role, text });
            if role == Role::User {
                self.driver.emit(ChannelEvent::UtteranceEnd);
            }
        }
        async fn emit_tool_call(&mut self, call_id: String, name: String, args: serde_json::Value) {
            self.driver.emit(ChannelEvent::ToolCall {
                call_id,
                name,
                args,
            });
        }
        async fn announce_session_end(&mut self) {
            self.driver.announce_session_end();
        }
        async fn complete_renewal(&mut self) {
            self.driver.complete_renewal();
        }
        async fn expire(&mut self) {
            self.driver.expire();
        }
        async fn received_audio_by_epoch(&mut self) -> Vec<Vec<Vec<i16>>> {
            self.driver.audio_by_epoch()
        }
        async fn received_texts(&mut self) -> Vec<String> {
            self.driver
                .ops()
                .into_iter()
                .filter_map(|op| match op {
                    super::super::mock::MockOp::Text { text, .. } => Some(text),
                    _ => None,
                })
                .collect()
        }
        async fn received_context_updates(&mut self) -> Vec<ContextUpdate> {
            self.driver
                .ops()
                .into_iter()
                .filter_map(|op| match op {
                    super::super::mock::MockOp::ContextUpdate(update) => Some(update),
                    _ => None,
                })
                .collect()
        }
        async fn received_tool_results(&mut self) -> Vec<(String, ToolResult)> {
            self.driver.tool_results()
        }
        async fn received_interrupts(&mut self) -> Vec<Option<u64>> {
            self.driver.interrupts()
        }
    }

    struct MockFixture;

    #[async_trait]
    impl ConformanceFixture for MockFixture {
        type Channel = MockChannel;
        type Sim = MockSim;

        async fn rig(&mut self, generation: &Generation) -> (MockChannel, MockSim) {
            let (channel, driver) = MockChannel::builder()
                .observe_generation(generation.clone())
                .build();
            (channel, MockSim { driver })
        }
    }

    #[tokio::test]
    async fn mock_channel_conforms_open_close() {
        check_open_close(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_conforms_transcript_ordering() {
        check_transcript_ordering(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_conforms_barge_in() {
        check_barge_in(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_conforms_tool_call_round_trip() {
        check_tool_call_round_trip(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_conforms_teacher_anchor() {
        check_teacher_anchor(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_conforms_renewal_recycle() {
        check_renewal_recycle(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_conforms_session_end_mid_response() {
        check_session_end_mid_response(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_conforms_context_update_and_text() {
        check_context_update_and_text(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_conforms_turn_boundary() {
        check_turn_boundary(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_conforms_session_expiry() {
        check_session_expiry(&mut MockFixture).await;
    }

    #[tokio::test]
    async fn mock_channel_passes_the_whole_suite_through_run_all() {
        run_all(&mut MockFixture).await;
    }

    /// A renewal-less channel (capabilities say no) gets exactly the core
    /// checks plus the one capabilities-probe rig — no renewal rigs.
    struct NoRenewalFixture {
        rigs: usize,
    }

    #[async_trait]
    impl ConformanceFixture for NoRenewalFixture {
        type Channel = MockChannel;
        type Sim = MockSim;

        async fn rig(&mut self, generation: &Generation) -> (MockChannel, MockSim) {
            self.rigs += 1;
            let (channel, driver) = MockChannel::builder()
                .capabilities(super::super::types::ChannelCapabilities {
                    vad_mode: super::super::types::VadMode::Server,
                    word_timestamps: false,
                    session_renewal: false,
                    text_only: false,
                    stt_only: false,
                })
                .observe_generation(generation.clone())
                .build();
            (channel, MockSim { driver })
        }
    }

    #[tokio::test]
    async fn run_all_skips_the_renewal_checks_when_capabilities_say_no() {
        let mut fixture = NoRenewalFixture { rigs: 0 };
        run_all(&mut fixture).await;
        assert_eq!(
            fixture.rigs, 8,
            "seven core checks + the capabilities probe — never a renewal rig"
        );
    }
    #[tokio::test]
    async fn recv_content_skips_stray_boundaries_and_returns_the_next_content_event() {
        let (tx, mut events) = mpsc::channel(8);
        for _ in 0..3 {
            tx.try_send(ChannelEvent::UtteranceEnd).unwrap();
        }
        tx.try_send(ChannelEvent::TranscriptDelta {
            role: Role::User,
            text: "content".to_string(),
        })
        .unwrap();
        assert_eq!(
            recv_content(&mut events).await,
            ChannelEvent::TranscriptDelta {
                role: Role::User,
                text: "content".to_string(),
            }
        );
    }

    #[tokio::test]
    #[should_panic(expected = "storming the turn boundary")]
    async fn recv_content_refuses_an_utterance_end_storm() {
        let (tx, mut events) = mpsc::channel(8);
        for _ in 0..5 {
            tx.try_send(ChannelEvent::UtteranceEnd).unwrap();
        }
        recv_content(&mut events).await;
    }
}
