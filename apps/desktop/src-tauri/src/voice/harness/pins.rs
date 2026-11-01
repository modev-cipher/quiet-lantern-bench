//! Role B adversarial pin suite for the VA6 session-harness event pump
//! (docs/TASKS.md VA6; VOICE_AGENT §3, §5, §11.1).
//!
//! Written by the test role against the pump as a *suspect black box*: every
//! test pins an externally promised invariant — the at-most-once
//! dispatch/emit shape under send failure (H1 through the pump), the anchor
//! ack's immunity to generation bumps that land between normalize and retry,
//! and the routing gate's §5 identity — rather than the implementation's own
//! structure. Each pin was proven load-bearing by a recorded local sabotage
//! of the production code (red) before the pristine code was restored
//! (green) — see the commit footers.
//!
//! Kept separate from the in-module tests in `harness.rs` on purpose: those
//! were written alongside the implementation; these are the independent
//! adversarial layer, sharing no rig helper with the code under test.

#![cfg(test)]

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::EventPump;
use crate::voice::audio::generation::Generation;
use crate::voice::channel::anchor::{
    OUT_OF_SCHEMA_CODE, STALE_GENERATION_CODE, TEACHER_ANCHOR_TOOL,
};
use crate::voice::channel::mock::{MockChannel, MockDriver};
use crate::voice::channel::types::{
    ChannelError, DelimitedExcerpt, ErrorKind, LifecycleEvent, Role, SessionContext, ToolResult,
};
use crate::voice::channel::{AnchorKind, AnchorRef, ChannelEvent, SessionChannel, TimedWord};
use crate::voice::excerpt::policy::{EgressEvent, TruncationPolicy};
use crate::voice::excerpt::selector::{ExcerptSelector, HARD_EGRESS_BYTE_CAP};
use crate::voice::excerpt::source::SessionSnapshot;
use crate::voice::excerpt::window::WindowPolicy;
use crate::voice::tools::dispatcher::{
    answer_tool_call, resend_pending_answers, CODE_PLAYBACK_UNAVAILABLE, CODE_RATE_LIMITED,
    CODE_UNKNOWN_CHAPTER, CODE_UNKNOWN_PAGE, CODE_UNKNOWN_TOOL, CODE_UNROUTED_TOOL,
};
use crate::voice::tools::playback::InMemoryPlayback;
use crate::voice::tools::registry::{self, ToolRegistry};
use crate::voice::tools::Dispatcher;

/// Five paragraph segments over three printed pages, two chapters — the same
/// small book shape the dispatcher pin suite uses, rebuilt here so this suite
/// shares no helper with the code under test. Timeline ends at 5000 ms.
fn pump_pin_book() -> ExcerptSelector {
    ExcerptSelector::new(
        SessionSnapshot::from_voice_test_parts(
            vec![
                (1, 0, 1000, "pump pin segment zero"),
                (1, 1000, 2000, "pump pin segment one"),
                (2, 2000, 3000, "pump pin segment two"),
                (2, 3000, 4000, "pump pin segment three"),
                (3, 4000, 5000, "pump pin segment four"),
            ],
            vec![(0, 2000), (2000, 5000)],
        )
        .with_test_figures_with_id(vec![
            ("fig-1", 1, "figure", "Figure 1 diagram"),
            ("fig-3", 1, "figure", "Figure 3 chart"),
            ("fig-6", 2, "figure", "Figure 6 illustration"),
            ("fig-7", 2, "figure", "Figure 7 photo"),
            ("fig-9", 3, "figure", "Figure 9 chart"),
            ("fig-q2", 3, "figure", "Figure Q2 plot"),
        ]),
        WindowPolicy {
            before: 1,
            after: 1,
        },
    )
}

/// The full pump seam under one roof: the pump plus everything it borrows per
/// call, and the mock driver's view of the wire.
struct PumpRig {
    pump: EventPump,
    generation: Generation,
    channel: MockChannel,
    driver: MockDriver,
    dispatcher: Dispatcher,
    playback: InMemoryPlayback,
    excerpts: ExcerptSelector,
    forwarded: Vec<ChannelEvent>,
}

async fn pump_rig(anchor_offered: bool) -> PumpRig {
    let generation = Generation::new();
    let (mut channel, driver) = MockChannel::builder().build();
    channel
        .connect(SessionContext {
            book_id: Some("pump-pin-book".to_string()),
            session_id: "pump-pin-session".to_string(),
            system_prompt: "tutor".to_string(),
            excerpt: DelimitedExcerpt {
                delimited_text: "<s>x</s>".to_string(),
            },
            rolling_summary: None,
            playback_pos_ms: 0,
            tool_schemas: ToolRegistry::new(anchor_offered).schemas(),
        })
        .await
        .expect("mock connects");
    PumpRig {
        pump: EventPump::new(generation.clone()),
        generation,
        channel,
        driver,
        dispatcher: Dispatcher::new(ToolRegistry::new(anchor_offered)),
        playback: InMemoryPlayback::at(2500),
        excerpts: pump_pin_book(),
        forwarded: Vec::new(),
    }
}

impl PumpRig {
    async fn handle(&mut self, event: ChannelEvent) -> Result<(), ChannelError> {
        self.pump
            .handle(
                event,
                &mut self.channel,
                &mut self.dispatcher,
                &mut self.playback,
                &mut self.excerpts,
                &mut |event| self.forwarded.push(event),
            )
            .await
    }

    fn arm_transport_failure(&self) {
        self.driver
            .fail_next_send(ChannelError::new(ErrorKind::Transport, true, "ws drop"));
    }
}

fn tool_call(call_id: &str, name: &str, args: Value) -> ChannelEvent {
    ChannelEvent::ToolCall {
        call_id: call_id.to_string(),
        name: name.to_string(),
        args,
    }
}

// ---- pump pin 1: at-most-once execution under send failure -----------------
//
// VOICE_AGENT §5 / H1, through the pump: a MUTATING call whose ack send fails
// has already had its effect, exactly once. Whatever the recovery path —
// resend_pending_answers after the channel heals, or the same call_id
// arriving again — the surface never moves a second time and the provider
// records exactly one ack, carrying the outcome computed before the failure.
// The alternative is the double-seek: one model call, two surface moves.

#[tokio::test]
async fn pump_pin1_a_failed_seek_ack_is_resent_after_recovery_without_a_second_seek() {
    let mut rig = pump_rig(false).await;
    rig.arm_transport_failure();
    rig.handle(tool_call(
        "seek-p1",
        registry::SEEK_RELATIVE,
        json!({"seconds": 2}),
    ))
    .await
    .expect_err("the armed transport failure must surface to the pump's caller");

    assert_eq!(
        rig.playback.position_ms, 4500,
        "the seek executed exactly once before the send failed (2500 + 2000)"
    );
    assert!(
        rig.driver.tool_results().is_empty(),
        "a failed send records no ghost ack"
    );

    // The harness's recovery step once the channel is healthy again.
    resend_pending_answers(&mut rig.dispatcher, &mut rig.channel)
        .await
        .expect("the channel is healthy again");
    assert_eq!(
        rig.playback.position_ms, 4500,
        "recovery re-sends the ack; it must not re-dispatch the seek"
    );
    assert_eq!(
        rig.driver.tool_results(),
        vec![(
            "seek-p1".to_string(),
            ToolResult::Ok(json!({ "position_ms": 4500 }))
        )],
        "exactly one ack, carrying the outcome computed before the failure"
    );

    // The recovery step is safely repeatable: a second pass finds the entry
    // settled and must neither double the ack nor touch the surface.
    resend_pending_answers(&mut rig.dispatcher, &mut rig.channel)
        .await
        .expect("an empty resend pass is a no-op");
    assert_eq!(rig.driver.tool_results().len(), 1, "still exactly one ack");
    assert_eq!(rig.playback.position_ms, 4500);
}

#[tokio::test]
async fn pump_pin1_a_redelivered_call_id_resends_the_cached_answer_not_the_effect() {
    let mut rig = pump_rig(false).await;
    rig.arm_transport_failure();
    let event = tool_call("seek-p2", registry::SEEK_RELATIVE, json!({"seconds": 2}));
    rig.handle(event.clone())
        .await
        .expect_err("the armed transport failure must surface");
    assert_eq!(rig.playback.position_ms, 4500, "the seek ran once");

    // The same logical call arrives again (a provider retry after the drop);
    // the channel is healthy now. Re-dispatching here is the double-seek.
    rig.handle(event)
        .await
        .expect("the retry's send lands on the healthy channel");
    assert_eq!(
        rig.playback.position_ms, 4500,
        "the retry re-sent the cached answer; a second dispatch would be 6500"
    );
    assert_eq!(
        rig.driver.tool_results(),
        vec![(
            "seek-p2".to_string(),
            ToolResult::Ok(json!({ "position_ms": 4500 }))
        )],
        "one logical call, one ack — no fork"
    );
}

// ---- pump pin 2: anchor emit-at-most-once across a generation bump ---------
//
// VOICE_AGENT §11.1 under the H1 shape: a gated teacher_anchor is normalized
// exactly once — on first sight of its call_id — and both halves of the
// decision are then frozen: the TeacherAnchor event surfaces at most once,
// and the ack that eventually lands is the CACHED one. A barge-in bump that
// arrives between the first normalize and the retry must not flip the frozen
// success ack into a re-normalized stale_generation: the anchor WAS live when
// the pump judged it, and the presentation already moved on the surfaced
// event, so acking it stale would tell the model its applied anchor failed.

fn live_anchor_event(call_id: &str, id: &str) -> ChannelEvent {
    tool_call(
        call_id,
        TEACHER_ANCHOR_TOOL,
        json!({"kind": "figure", "id": id}),
    )
}

#[tokio::test]
async fn pump_pin2_a_bump_between_normalize_and_recovery_cannot_flip_the_cached_ack() {
    let mut rig = pump_rig(true).await;
    rig.arm_transport_failure();
    rig.handle(live_anchor_event("anchor-p1", "fig-7"))
        .await
        .expect_err("the armed transport failure must surface");
    assert_eq!(
        rig.forwarded,
        vec![ChannelEvent::TeacherAnchor {
            anchor: AnchorRef {
                kind: AnchorKind::Figure,
                id: "fig-7".to_string(),
            },
            gen: 0,
        }],
        "the event surfaced exactly once, on the first (and only) normalize"
    );
    assert!(
        rig.driver.tool_results().is_empty(),
        "a failed send records no ghost ack"
    );

    // A barge-in bumps the live generation while the ack is still unsent.
    rig.generation.bump();

    resend_pending_answers(&mut rig.dispatcher, &mut rig.channel)
        .await
        .expect("the channel is healthy again");
    assert_eq!(
        rig.forwarded.len(),
        1,
        "recovery re-sends acks; it never re-emits events"
    );
    assert_eq!(
        rig.driver.tool_results(),
        vec![(
            "anchor-p1".to_string(),
            ToolResult::Ok(json!({ "applied": true }))
        )],
        "the CACHED success ack lands — a re-normalize after the bump would \
         have flipped it to stale_generation"
    );
}

#[tokio::test]
async fn pump_pin2_a_redelivered_anchor_after_a_bump_reuses_the_cached_ack_and_never_reemits() {
    let mut rig = pump_rig(true).await;
    rig.arm_transport_failure();
    let event = live_anchor_event("anchor-p2", "fig-9");
    rig.handle(event.clone())
        .await
        .expect_err("the armed transport failure must surface");
    assert_eq!(rig.forwarded.len(), 1, "surfaced once before the failure");

    // The bump lands, then the provider redelivers the same logical call.
    rig.generation.bump();
    rig.handle(event)
        .await
        .expect("the retry's send lands on the healthy channel");

    assert_eq!(
        rig.forwarded.len(),
        1,
        "the retry must not surface the anchor a second time"
    );
    assert_eq!(
        rig.driver.tool_results(),
        vec![(
            "anchor-p2".to_string(),
            ToolResult::Ok(json!({ "applied": true }))
        )],
        "one ack, the cached success — neither doubled nor re-judged stale"
    );
}

// ---- pump pin 3: the routing gate's identity ------------------------------
//
// VOICE_AGENT §5 / §11.1 through the pump, both directions at once:
//
// - UNGATED (the session never offered the tool): teacher_anchor must fall
//   through to the dispatcher and come back byte-identical on the wire to a
//   hallucinated name — code AND message — with nothing surfaced. Anything
//   distinguishable leaks the gated tool's existence to a model that was not
//   offered it.
// - GATED: every anchor outcome (live, stale, malformed) is decided by the
//   normalize path before the dispatcher ever sees the call, so the
//   dispatcher's unrouted_tool backstop must never reach the wire on a
//   correctly wired session.

#[tokio::test]
async fn pump_pin3_an_ungated_anchor_is_wire_identical_to_a_hallucinated_name() {
    let mut rig = pump_rig(false).await;
    // Args that would be perfectly valid on a gated session: the rejection
    // must come from the vocabulary gate, not from arg shape.
    let args = json!({"kind": "figure", "id": "fig-3"});
    rig.handle(tool_call("ungated-a", TEACHER_ANCHOR_TOOL, args.clone()))
        .await
        .expect("send succeeds");
    rig.handle(tool_call("hall-a", "definitely_not_a_tool", args))
        .await
        .expect("send succeeds");

    assert!(
        rig.forwarded.is_empty(),
        "nothing surfaces: an ungated anchor must not move the presentation"
    );
    let acks = rig.driver.tool_results();
    assert_eq!(acks.len(), 2, "both calls answered");
    let (
        ToolResult::Err { code, message },
        ToolResult::Err {
            code: hall_code,
            message: hall_message,
        },
    ) = (&acks[0].1, &acks[1].1)
    else {
        panic!("both must be errors, got {acks:?}");
    };
    assert_eq!(code, CODE_UNKNOWN_TOOL);
    assert_eq!(
        (code, message),
        (hall_code, hall_message),
        "byte-identical on the wire — code AND message — so the answer cannot \
         leak that the gated tool exists"
    );
}

#[tokio::test]
async fn pump_pin3_a_gated_session_never_answers_unrouted_tool() {
    let mut rig = pump_rig(true).await;
    // Every anchor outcome class, plus an ordinary playback tool to prove the
    // routing gate does not over-capture the rest of the vocabulary.
    rig.handle(live_anchor_event("live-a", "fig-1"))
        .await
        .expect("send succeeds");
    rig.handle(tool_call(
        "bad-a",
        TEACHER_ANCHOR_TOOL,
        json!({"kind": "paragraph", "id": "p-1"}),
    ))
    .await
    .expect("send succeeds");
    rig.generation.bump(); // supersede the committed response
    rig.handle(live_anchor_event("stale-a", "fig-2"))
        .await
        .expect("send succeeds");
    rig.handle(tool_call("pause-a", registry::PAUSE_PLAYBACK, json!({})))
        .await
        .expect("send succeeds");

    let acks = rig.driver.tool_results();
    assert_eq!(
        acks.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        vec!["live-a", "bad-a", "stale-a", "pause-a"],
        "every call answered, in order"
    );
    for (id, result) in &acks {
        if let ToolResult::Err { code, .. } = result {
            assert_ne!(
                code, CODE_UNROUTED_TOOL,
                "{id}: the dispatcher's mis-wiring backstop must never fire \
                 on a correctly wired session"
            );
        }
    }
    assert_eq!(acks[0].1, ToolResult::Ok(json!({ "applied": true })));
    assert!(
        matches!(&acks[1].1, ToolResult::Err { code, .. } if code == OUT_OF_SCHEMA_CODE),
        "malformed args are the normalize path's verdict, got {:?}",
        acks[1].1
    );
    assert!(
        matches!(&acks[2].1, ToolResult::Err { code, .. } if code == STALE_GENERATION_CODE),
        "a superseded anchor is the normalize path's verdict, got {:?}",
        acks[2].1
    );
    assert_eq!(acks[3].1, ToolResult::Ok(json!({ "paused": true })));
    assert_eq!(
        rig.forwarded.len(),
        1,
        "exactly the live anchor surfaced — stale and malformed never do"
    );
    assert!(rig.playback.paused, "the ordinary tool still executed");
}

// ---- pump pin 4: response-generation attribution through the queue ---------
//
// VOICE_AGENT §11.1 staleness / §2 rule 2, the pump's attribution discipline:
// ToolCall carries no gen, so an anchor is judged against the generation the
// in-flight response was COMMITTED under — snapshotted at UtteranceEnd, held
// until the next boundary. A barge-in bump between that snapshot and the
// anchor's arrival makes the anchor stale (acked, never surfaced), however
// many bumps land; only the NEXT UtteranceEnd re-arms attribution. Judging
// against the live counter instead would let an anchor from the superseded
// response move the presentation whenever the counters happen to agree —
// including for events that were already buffered in the pump's queue when
// the bump landed.

#[tokio::test]
async fn pump_pin4_a_bump_after_the_committed_boundary_stales_anchors_until_the_next_boundary() {
    let mut rig = pump_rig(true).await;
    rig.handle(ChannelEvent::UtteranceEnd)
        .await
        .expect("forwarding cannot fail");

    // The response is committed under generation 0; two barge-ins land
    // before its anchors arrive.
    rig.generation.bump();
    rig.handle(live_anchor_event("p4-stale-a", "fig-4"))
        .await
        .expect("the stale ack still goes out");
    rig.generation.bump();
    rig.handle(live_anchor_event("p4-stale-b", "fig-5"))
        .await
        .expect("attribution must not creep toward the live counter");

    // The next turn boundary commits the new response under the live
    // generation (2): anchors are live again.
    rig.handle(ChannelEvent::UtteranceEnd)
        .await
        .expect("forwarding cannot fail");
    rig.handle(live_anchor_event("p4-live", "fig-6"))
        .await
        .expect("send succeeds");

    assert_eq!(
        rig.forwarded,
        vec![
            ChannelEvent::UtteranceEnd,
            ChannelEvent::UtteranceEnd,
            ChannelEvent::TeacherAnchor {
                anchor: AnchorRef {
                    kind: AnchorKind::Figure,
                    id: "fig-6".to_string(),
                },
                gen: 2,
            },
        ],
        "no stale anchor surfaces; the re-armed one carries the new generation"
    );
    let acks = rig.driver.tool_results();
    assert_eq!(
        acks.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        vec!["p4-stale-a", "p4-stale-b", "p4-live"],
        "every anchor is acked, in order — staleness is an answer, not a drop"
    );
    for (id, result) in &acks[..2] {
        assert!(
            matches!(result, ToolResult::Err { code, .. } if code == STALE_GENERATION_CODE),
            "{id}: judged against the committed generation, got {result:?}"
        );
    }
    assert_eq!(
        acks[2].1,
        ToolResult::Ok(json!({ "applied": true })),
        "the post-boundary anchor is live again"
    );
}

#[tokio::test]
async fn pump_pin4_events_buffered_behind_a_bump_are_judged_against_the_committed_generation() {
    let mut rig = pump_rig(true).await;
    let (tx, mut events) = tokio::sync::mpsc::channel(16);
    // The whole script is already sitting in the pump's queue when the
    // barge-in bump lands: the buffered anchor belongs to the response
    // committed under generation 0 and must be judged against 0, not
    // against whatever the counter reads when the pump gets to it.
    for event in [
        live_anchor_event("p4-q-stale", "fig-q1"),
        ChannelEvent::UtteranceEnd,
        live_anchor_event("p4-q-live", "fig-q2"),
    ] {
        tx.try_send(event).expect("scripted event fits");
    }
    drop(tx);
    rig.generation.bump();

    rig.pump
        .run(
            &mut events,
            &mut rig.channel,
            &mut rig.dispatcher,
            &mut rig.playback,
            &mut rig.excerpts,
            &mut |event| rig.forwarded.push(event),
        )
        .await
        .expect("no send fails in this script");

    assert_eq!(
        rig.forwarded,
        vec![
            ChannelEvent::UtteranceEnd,
            ChannelEvent::TeacherAnchor {
                anchor: AnchorRef {
                    kind: AnchorKind::Figure,
                    id: "fig-q2".to_string(),
                },
                gen: 1,
            },
        ],
        "the buffered pre-boundary anchor never surfaces; the post-boundary \
         one carries the bumped generation"
    );
    let acks = rig.driver.tool_results();
    assert_eq!(
        acks.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        vec!["p4-q-stale", "p4-q-live"],
        "both anchors answered, in order"
    );
    assert!(
        matches!(&acks[0].1, ToolResult::Err { code, .. } if code == STALE_GENERATION_CODE),
        "the buffered pre-boundary anchor acks stale, got {:?}",
        acks[0].1
    );
    assert_eq!(
        acks[1].1,
        ToolResult::Ok(json!({ "applied": true })),
        "the boundary re-armed attribution for the rest of the queue"
    );
}

// ---- pump pin 5: the unsent-answer cache boundary is exactly 32 ------------
//
// H1's documented trade-off (dispatcher.rs, PENDING_ANSWER_CAP): the cache of
// computed-but-unacknowledged answers holds at most 32 entries; the 33rd
// evicts the oldest, whose retry then RE-EXECUTES — acceptable only because a
// channel that deep in failed sends is renewal/teardown territory. The number
// is load-bearing in both directions: silently widening it grows the memory a
// provider can drive by replaying ids, silently shrinking it converts the
// at-most-once guarantee into double execution while the session is still
// live. Detector: a Tier-0 read cached at one playback position, retried
// after the surface moved — the cached answer carries the OLD window's text,
// a re-executed one the NEW window's.

/// One `get_context_around_position` call (no explicit position: the payload
/// is computed from the live playback position, which is what makes cached
/// vs re-executed answers distinguishable after the surface moves).
fn context_read(call_id: &str) -> ChannelEvent {
    tool_call(call_id, registry::GET_CONTEXT_AROUND_POSITION, json!({}))
}

/// The `book_text` payload out of an Ok ack — panics loudly on any other shape.
fn book_text(result: &ToolResult) -> String {
    match result {
        ToolResult::Ok(payload) => payload["book_text"]
            .as_str()
            .expect("a context read answers with book_text")
            .to_string(),
        other => panic!("expected an Ok context payload, got {other:?}"),
    }
}

/// Fill the pending-answer cache with `count` unsent Tier-0 reads (ids
/// `evict-target`, then `fill-02`, `fill-03`, …), every ack send armed to
/// fail. All are computed at the rig's opening playback position (2500 ms).
async fn fill_unsent_answers(rig: &mut PumpRig, count: usize) {
    rig.arm_transport_failure();
    rig.handle(context_read("evict-target"))
        .await
        .expect_err("the armed transport failure must surface");
    for i in 2..=count {
        rig.arm_transport_failure();
        rig.handle(context_read(&format!("fill-{i:02}")))
            .await
            .expect_err("every fill send is armed to fail");
    }
    assert!(
        rig.driver.tool_results().is_empty(),
        "nothing reached the wire during the fill"
    );
}

#[tokio::test]
async fn pump_pin5_the_32nd_unsent_answer_does_not_evict_the_oldest() {
    let mut rig = pump_rig(false).await;
    fill_unsent_answers(&mut rig, 32).await;

    // The surface moves before the retry: a re-executed read would now see
    // the window around 4500 ms instead of the cached one around 2500 ms.
    rig.playback.position_ms = 4500;
    rig.handle(context_read("evict-target"))
        .await
        .expect("the retry's send lands on the healthy channel");

    let acks = rig.driver.tool_results();
    assert_eq!(acks.len(), 1, "exactly the retried ack reached the wire");
    assert_eq!(acks[0].0, "evict-target");
    let text = book_text(&acks[0].1);
    assert!(
        text.contains("pump pin segment two") && !text.contains("pump pin segment four"),
        "at 32 unsent answers the oldest is still cached: the retry re-sends \
         the answer computed at 2500 ms, it does not re-read at 4500 ms — got {text:?}"
    );
}

#[tokio::test]
async fn pump_pin5_the_33rd_unsent_answer_evicts_exactly_the_oldest_whose_retry_reexecutes() {
    let mut rig = pump_rig(false).await;
    fill_unsent_answers(&mut rig, 33).await;

    rig.playback.position_ms = 4500;
    // The second-oldest survived the 33rd insert: its retry still re-sends
    // the cached answer. Retried first, before anything can push it out.
    rig.handle(context_read("fill-02"))
        .await
        .expect("send lands on the healthy channel");
    // The oldest was evicted by the 33rd insert: its retry finds no cache
    // entry and re-executes — the documented trade-off, pinned here so the
    // boundary cannot silently move in either direction.
    rig.handle(context_read("evict-target"))
        .await
        .expect("send lands on the healthy channel");

    let acks = rig.driver.tool_results();
    assert_eq!(
        acks.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        vec!["fill-02", "evict-target"],
        "both retries answered, in retry order"
    );
    let survivor = book_text(&acks[0].1);
    assert!(
        survivor.contains("pump pin segment two") && !survivor.contains("pump pin segment four"),
        "the 33rd insert evicts exactly one entry — the second-oldest still \
         answers from cache, got {survivor:?}"
    );
    let evicted = book_text(&acks[1].1);
    assert!(
        evicted.contains("pump pin segment four") && !evicted.contains("pump pin segment two"),
        "the evicted call's retry re-executed at the moved position — the \
         trade-off this pin documents, got {evicted:?}"
    );
}

// ---- pump pin 6: H2 — an installed policy budgets every Tier-0 egress ------
//
// VOICE_AGENT §4 through the pump: once the session wiring installs the
// provider's TruncationPolicy, no Tier-0 payload leaves unbudgeted — both
// reads (get_context_around_position AND summarize_section) take the
// selector's budgeted paths, every content drop rides the payload as an EXACT
// `truncation` rider, the 64 KiB byte backstop still fires BENEATH a
// permissive budget (its `hard_cap` rider intact), and the same facts land in
// the dispatcher's egress buffer: drained oldest-first, capped at 64 with the
// oldest record shed.

/// A whitespace-word tokenizer policy: every pin-book segment is 4 words and
/// the "\n\n" join is whitespace to it, so a k-segment window counts 4k.
fn word_policy(budget: usize) -> TruncationPolicy {
    TruncationPolicy::new(budget, |text: &str| text.split_whitespace().count())
}

#[tokio::test]
async fn pump_pin6_an_installed_policy_budgets_every_tier0_payload_with_an_exact_rider() {
    let mut rig = pump_rig(false).await;
    // Window at 2500 ms is segments 1..=3 (12 word-tokens); budget 8 forces
    // one leading segment out (the edge tie sheds the summarized side).
    rig.dispatcher.set_truncation_policy(word_policy(8));

    // Ordering is load-bearing: the context read resolves the window that
    // summarize_section then budgets without re-anchoring; summarize alone
    // on a fresh session answers no_context.
    rig.handle(context_read("h2-context"))
        .await
        .expect("send succeeds");
    rig.handle(tool_call(
        "h2-summarize",
        registry::SUMMARIZE_SECTION,
        json!({}),
    ))
    .await
    .expect("send succeeds");

    let acks = rig.driver.tool_results();
    assert_eq!(
        acks.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        vec!["h2-context", "h2-summarize"],
        "both Tier-0 reads answered"
    );
    let expected_rider = json!({
        "budget_tokens": 8,
        "window_tokens": 12,
        "dropped_leading_segments": 1,
        "dropped_trailing_segments": 0,
        "anchor_partially_cut": false,
    });
    for (id, result) in &acks {
        let ToolResult::Ok(payload) = result else {
            panic!("{id}: expected an Ok payload, got {result:?}");
        };
        let text = payload["book_text"].as_str().expect("book_text present");
        assert!(
            text.contains("pump pin segment two") && text.contains("pump pin segment three"),
            "{id}: the anchor-side content survives the trim, got {text:?}"
        );
        assert!(
            !text.contains("pump pin segment one"),
            "{id}: an unbudgeted payload left the pump — the leading segment \
             the budget dropped is on the wire"
        );
        assert_eq!(
            payload["truncation"], expected_rider,
            "{id}: the truncation rider is exact"
        );
        assert!(
            payload["hard_cap"].is_null(),
            "{id}: the backstop never fired, so its rider must be absent"
        );
    }
    assert_eq!(
        rig.dispatcher.drain_egress_events(),
        vec![
            EgressEvent::ExcerptTruncated {
                budget_tokens: 8,
                window_tokens: 12,
                dropped_leading_segments: 1,
                dropped_trailing_segments: 0,
                anchor_partially_cut: false,
            };
            2
        ],
        "the same loss facts reach the session event stream, one per read"
    );

    // The exact-fit boundary: a budget equal to the window's 12 tokens drops
    // nothing — the whole window ships, BOTH riders absent, nothing buffered.
    rig.dispatcher.set_truncation_policy(word_policy(12));
    rig.handle(context_read("h2-fits")).await.expect("send ok");
    let acks = rig.driver.tool_results();
    let ToolResult::Ok(payload) = &acks[2].1 else {
        panic!("expected an Ok payload, got {:?}", acks[2].1);
    };
    let text = payload["book_text"].as_str().expect("book_text present");
    assert!(
        text.contains("pump pin segment one")
            && text.contains("pump pin segment two")
            && text.contains("pump pin segment three"),
        "an exactly-fitting window ships whole, got {text:?}"
    );
    assert!(
        payload["truncation"].is_null() && payload["hard_cap"].is_null(),
        "no loss, no riders"
    );
    assert!(
        rig.dispatcher.drain_egress_events().is_empty(),
        "no loss, no event — a raised event always means something"
    );
}

#[tokio::test]
async fn pump_pin6_the_hard_byte_cap_survives_beneath_a_permissive_budget() {
    let mut rig = pump_rig(false).await;
    // A tokenizer that undercounts everything (one flat token) never trims —
    // the byte backstop must still cap the payload and say so, loudly, on the
    // budgeted path.
    let oversized = "x".repeat(HARD_EGRESS_BYTE_CAP + 24);
    rig.excerpts = ExcerptSelector::new(
        SessionSnapshot::from_voice_test_parts(
            vec![(1, 0, 1000, oversized.as_str())],
            vec![(0, 1000)],
        ),
        WindowPolicy {
            before: 1,
            after: 1,
        },
    );
    rig.dispatcher
        .set_truncation_policy(TruncationPolicy::new(1, |_: &str| 1));

    rig.handle(context_read("h2-backstop"))
        .await
        .expect("send succeeds");

    let acks = rig.driver.tool_results();
    let ToolResult::Ok(payload) = &acks[0].1 else {
        panic!("expected an Ok payload, got {:?}", acks[0].1);
    };
    assert_eq!(
        payload["hard_cap"],
        json!({
            "kept_bytes": HARD_EGRESS_BYTE_CAP,
            "dropped_bytes": 24,
        }),
        "the backstop fired beneath the budget and its rider is exact"
    );
    assert!(
        payload["truncation"].is_null(),
        "the token budget saw one token; only the backstop's rider rides"
    );
    assert_eq!(
        rig.dispatcher.drain_egress_events(),
        vec![EgressEvent::HardByteCapApplied {
            kept_bytes: HARD_EGRESS_BYTE_CAP,
            dropped_bytes: 24,
        }],
        "the backstop is loud in the event stream too"
    );
}

#[tokio::test]
async fn pump_pin6_egress_events_drain_oldest_first_and_cap_at_64() {
    let mut rig = pump_rig(false).await;

    // Order: two reads under distinguishable budgets drain oldest-first,
    // and draining empties the buffer.
    rig.dispatcher.set_truncation_policy(word_policy(1));
    rig.handle(context_read("order-1")).await.expect("send ok");
    rig.dispatcher.set_truncation_policy(word_policy(2));
    rig.handle(context_read("order-2")).await.expect("send ok");
    let drained = rig.dispatcher.drain_egress_events();
    assert!(
        matches!(
            drained.as_slice(),
            [
                EgressEvent::ExcerptTruncated {
                    budget_tokens: 1,
                    ..
                },
                EgressEvent::ExcerptTruncated {
                    budget_tokens: 2,
                    ..
                },
            ]
        ),
        "oldest first, exactly the two raised events — got {drained:?}"
    );
    assert!(
        rig.dispatcher.drain_egress_events().is_empty(),
        "draining empties the buffer"
    );

    // Cap: 65 loss events — a uniquely-budgeted first, then 64 more. The
    // buffer holds 64, shedding exactly the oldest record.
    rig.dispatcher.set_truncation_policy(word_policy(3));
    rig.handle(context_read("cap-oldest"))
        .await
        .expect("send ok");
    rig.dispatcher.set_truncation_policy(word_policy(2));
    for i in 1..=64 {
        rig.handle(context_read(&format!("cap-{i:02}")))
            .await
            .expect("send ok");
    }
    let drained = rig.dispatcher.drain_egress_events();
    assert_eq!(
        drained.len(),
        64,
        "the buffer caps at 64 buffered egress events"
    );
    assert!(
        drained.iter().all(|event| matches!(
            event,
            EgressEvent::ExcerptTruncated {
                budget_tokens: 2,
                ..
            }
        )),
        "the 65th insert shed exactly the oldest (budget-3) record — got {drained:?}"
    );
}

// ---- pump pin 7: H3 — the Tier-1 window counts performed mutations only ----
//
// ARCHITECTURE §8.1 under the H3 decision (commit-after-success): the 5-in-10s
// budget slot is committed only when the mutation actually happened. A refusal
// storm — playback_unavailable, unknown_chapter, unknown_page — costs nothing,
// because those answers *instruct* the model to retry and charging them would
// rate-limit it exactly when the surface comes back. Successful mutations
// still cost exactly 5 per sliding 10 s window, edge-exact: window−1 ms after
// a commit is refused, the exact window boundary is allowed, and the edge
// slides per success commit time — never re-arms wholesale, never keys off a
// refusal's timestamp. Driven at the answer_tool_call seam with a pinned
// clock: the same call the pump wraps, whose `now` is the real clock in
// production (harness.rs passes Instant::now()).

/// One Tier-1/Tier-0 call at a pinned instant, returning the ack that hit the
/// wire: `None` for a success payload, `Some(code)` for a refusal.
async fn answered_at(
    rig: &mut PumpRig,
    call_id: &str,
    name: &str,
    args: Value,
    at: Instant,
) -> Option<String> {
    answer_tool_call(
        &mut rig.dispatcher,
        &mut rig.channel,
        call_id,
        name,
        &args,
        at,
        &mut rig.playback,
        &mut rig.excerpts,
    )
    .await
    .expect("no send failures in the pin 7 scripts");
    match &rig.driver.tool_results().last().expect("acked").1 {
        ToolResult::Ok(_) => None,
        ToolResult::Err { code, .. } => Some(code.clone()),
    }
}

#[tokio::test]
async fn pump_pin7_refusal_storms_cost_no_budget_and_successes_cost_exactly_five() {
    let mut rig = pump_rig(false).await;
    let base = Instant::now();

    // A storm of every execution-refusal class, all inside one instant: a
    // torn-down surface, a chapter the book lacks, a printed number no
    // narrated passage carries.
    rig.playback.available = false;
    for (i, (name, args)) in [
        (registry::PAUSE_PLAYBACK, json!({})),
        (registry::SEEK_RELATIVE, json!({"seconds": 5})),
        (registry::RESUME_PLAYBACK, json!({})),
    ]
    .into_iter()
    .enumerate()
    {
        let code = answered_at(&mut rig, &format!("gone-{i}"), name, args, base).await;
        assert_eq!(
            code.as_deref(),
            Some(CODE_PLAYBACK_UNAVAILABLE),
            "the surface is down: answered, never charged"
        );
    }
    rig.playback.available = true;
    for i in 0..3 {
        let code = answered_at(
            &mut rig,
            &format!("chap-{i}"),
            registry::JUMP_TO_CHAPTER,
            json!({"chapter": 99}),
            base,
        )
        .await;
        assert_eq!(code.as_deref(), Some(CODE_UNKNOWN_CHAPTER));
        let code = answered_at(
            &mut rig,
            &format!("pg-{i}"),
            registry::JUMP_TO_PAGE,
            json!({"page": 77}),
            base,
        )
        .await;
        assert_eq!(code.as_deref(), Some(CODE_UNKNOWN_PAGE));
    }

    // Nine refusals down, the full budget must still be there: five distinct
    // mutations succeed at the same instant.
    for (i, (name, args)) in [
        (registry::PAUSE_PLAYBACK, json!({})),
        (registry::RESUME_PLAYBACK, json!({})),
        (registry::SET_PLAYBACK_SPEED, json!({"speed": 1.5})),
        (registry::SEEK_RELATIVE, json!({"seconds": 1})),
        (registry::JUMP_TO_CHAPTER, json!({"chapter": 0})),
    ]
    .into_iter()
    .enumerate()
    {
        let code = answered_at(&mut rig, &format!("mut-{i}"), name, args, base).await;
        assert_eq!(
            code, None,
            "mutation {i} succeeds: the storm held no budget slot"
        );
    }

    // The sixth performed mutation in the window is the first one refused —
    // and that refusal is loud, typed, and leaves the surface untouched.
    let before = rig.playback.position_ms;
    let code = answered_at(
        &mut rig,
        "mut-6",
        registry::SEEK_RELATIVE,
        json!({"seconds": 1}),
        base,
    )
    .await;
    assert_eq!(
        code.as_deref(),
        Some(CODE_RATE_LIMITED),
        "exactly five performed mutations per window"
    );
    assert_eq!(
        rig.playback.position_ms, before,
        "a rate-limited call mutates nothing"
    );

    // Tier-0 reads are never limited, spent budget or not.
    let code = answered_at(
        &mut rig,
        "read-1",
        registry::GET_CONTEXT_AROUND_POSITION,
        json!({}),
        base,
    )
    .await;
    assert_eq!(code, None, "reads are outside the Tier-1 budget entirely");
}

#[tokio::test]
async fn pump_pin7_the_window_slides_on_success_commit_times_only() {
    let mut rig = pump_rig(false).await;
    let base = Instant::now();
    let at = |ms: u64| base + Duration::from_millis(ms);
    let pause_resume = |i: usize| {
        if i % 2 == 0 {
            registry::PAUSE_PLAYBACK
        } else {
            registry::RESUME_PLAYBACK
        }
    };

    // Five commits at base, +1s, +2s, +3s, +4s — with a refusal at +500 ms
    // wedged in. If the refusal occupied a slot, every edge below shifts.
    let code = answered_at(&mut rig, "commit-0", pause_resume(0), json!({}), at(0)).await;
    assert_eq!(code, None);
    let code = answered_at(
        &mut rig,
        "wedge",
        registry::JUMP_TO_CHAPTER,
        json!({"chapter": 99}),
        at(500),
    )
    .await;
    assert_eq!(code.as_deref(), Some(CODE_UNKNOWN_CHAPTER));
    for i in 1..5 {
        let code = answered_at(
            &mut rig,
            &format!("commit-{i}"),
            pause_resume(i),
            json!({}),
            at(i as u64 * 1000),
        )
        .await;
        assert_eq!(code, None, "commit {i} fits the budget");
    }

    // window − 1 ms after the first commit: still five in the window.
    let code = answered_at(
        &mut rig,
        "edge-early",
        pause_resume(5),
        json!({}),
        at(9_999),
    )
    .await;
    assert_eq!(
        code.as_deref(),
        Some(CODE_RATE_LIMITED),
        "one millisecond inside the window is refused"
    );
    // The exact window boundary: the base commit has aged out — and the
    // +500 ms refusal left nothing behind to take its place.
    let code = answered_at(
        &mut rig,
        "edge-exact",
        pause_resume(6),
        json!({}),
        at(10_000),
    )
    .await;
    assert_eq!(
        code, None,
        "at exactly the window the oldest slot is free again"
    );
    // The edge SLIDES: the +1s commit is still live at +10s, so the next
    // call is refused until it too ages out at +11s.
    let code = answered_at(
        &mut rig,
        "slide-early",
        pause_resume(7),
        json!({}),
        at(10_000),
    )
    .await;
    assert_eq!(
        code.as_deref(),
        Some(CODE_RATE_LIMITED),
        "the window slid by one commit, it did not re-arm wholesale"
    );
    let code = answered_at(
        &mut rig,
        "slide-exact",
        pause_resume(8),
        json!({}),
        at(11_000),
    )
    .await;
    assert_eq!(code, None, "the +1s commit ages out exactly on its edge");
}

// ---- pump pin 8: passthrough totality ---------------------------------------
//
// The pump's third promise (harness.rs header; VOICE_AGENT §3): it consumes
// exactly one event kind — ToolCall — and forwards EVERYTHING else to the
// caller's sink verbatim, in arrival order. Both directions are load-bearing:
// a swallowed variant silently starves the seam that owns it (the audio
// writer, the transcript view, the lifecycle policy), and a leaked ToolCall
// hands raw model tool syntax to seams that must never see it. Pinned over
// the full ChannelEvent vocabulary, including TurnComplete (composition-
// internal, but the pump must not special-case it) and an already-normalized
// TeacherAnchor arriving as an event (forwarded untouched, never re-judged).

/// Compile-time totality guard for the script below: `ChannelEvent` and
/// `LifecycleEvent` are `#[non_exhaustive]`, but that attribute does not bind
/// in-crate, so this wildcard-free match stops compiling the moment either
/// vocabulary grows — forcing the passthrough script to grow with it instead
/// of leaving this pin silently green on a variant it never exercised.
fn covered_by_the_passthrough_script(event: &ChannelEvent) {
    match event {
        ChannelEvent::AudioDelta { .. }
        | ChannelEvent::TranscriptDelta { .. }
        | ChannelEvent::WordTimestamps { .. }
        | ChannelEvent::UtteranceEnd
        | ChannelEvent::ToolCall { .. }
        | ChannelEvent::TurnComplete
        | ChannelEvent::TeacherAnchor { .. }
        // TranscriptActivity is a transcribe-stream existence signal; the
        // dialog passthrough never carries it (voice_session's transcribe
        // arm consumes it for the fan-out ledger).
        | ChannelEvent::TranscriptActivity { .. }
        // Progress is display-only provider status (retry / fallback); the
        // runtime renders it and nothing in the passthrough reacts to it.
        | ChannelEvent::Progress(_)
        | ChannelEvent::Error(_) => {}
        ChannelEvent::Lifecycle(lifecycle) => match lifecycle {
            LifecycleEvent::Connected
            | LifecycleEvent::Renewing { .. }
            | LifecycleEvent::Reconnected { .. }
            | LifecycleEvent::Expired => {}
        },
    }
}

#[tokio::test]
async fn pump_pin8_every_non_toolcall_event_forwards_verbatim_in_order() {
    let mut rig = pump_rig(true).await;
    // One of every non-ToolCall variant, deliberately interleaved: all four
    // lifecycle states, both transcript roles, response-scoped payloads with
    // their gen tags, and a classified error.
    let script = vec![
        ChannelEvent::Lifecycle(LifecycleEvent::Connected),
        ChannelEvent::TranscriptDelta {
            role: Role::User,
            text: "where were we".to_string(),
        },
        ChannelEvent::AudioDelta {
            pcm: vec![1, -2, 3],
            gen: 0,
        },
        ChannelEvent::WordTimestamps {
            words: vec![
                TimedWord {
                    w: "where".to_string(),
                    start_ms: 0,
                    end_ms: 180,
                },
                TimedWord {
                    w: "were".to_string(),
                    start_ms: 180,
                    end_ms: 340,
                },
            ],
            gen: 0,
        },
        ChannelEvent::UtteranceEnd,
        ChannelEvent::TranscriptDelta {
            role: Role::Assistant,
            text: "right here".to_string(),
        },
        ChannelEvent::TurnComplete,
        ChannelEvent::TeacherAnchor {
            anchor: AnchorRef {
                kind: AnchorKind::Section,
                id: "sec-p8".to_string(),
            },
            gen: 0,
        },
        ChannelEvent::Lifecycle(LifecycleEvent::Renewing {
            time_left: Some(Duration::from_secs(5)),
        }),
        ChannelEvent::Lifecycle(LifecycleEvent::Reconnected { resumed: false }),
        ChannelEvent::Error(ChannelError::new(ErrorKind::Provider, true, "upstream 503")),
        ChannelEvent::Lifecycle(LifecycleEvent::Expired),
    ];

    for event in script.clone() {
        covered_by_the_passthrough_script(&event);
        rig.handle(event).await.expect("forwarding cannot fail");
    }

    assert_eq!(
        rig.forwarded, script,
        "every non-ToolCall variant passes through verbatim, in arrival order"
    );
    assert!(
        rig.driver.tool_results().is_empty(),
        "nothing here is a ToolCall, so nothing is answered"
    );
}

#[tokio::test]
async fn pump_pin8_toolcalls_of_every_outcome_class_are_consumed_never_forwarded() {
    let mut rig = pump_rig(false).await;
    let (tx, mut events) = tokio::sync::mpsc::channel(16);
    let passthrough = [
        ChannelEvent::TranscriptDelta {
            role: Role::Assistant,
            text: "pausing now".to_string(),
        },
        ChannelEvent::AudioDelta {
            pcm: vec![7, 7],
            gen: 0,
        },
        ChannelEvent::UtteranceEnd,
        ChannelEvent::Lifecycle(LifecycleEvent::Expired),
    ];
    // Every dispatcher outcome class rides the same stream: a success, a
    // hallucinated name, an execution refusal, and a Tier-0 read. None of
    // them may reach the sink, whatever their answer was.
    let script = vec![
        passthrough[0].clone(),
        tool_call("ok-1", registry::PAUSE_PLAYBACK, json!({})),
        passthrough[1].clone(),
        tool_call("unknown-1", "definitely_not_a_tool", json!({})),
        tool_call(
            "refused-1",
            registry::JUMP_TO_CHAPTER,
            json!({"chapter": 99}),
        ),
        passthrough[2].clone(),
        tool_call("read-1", registry::GET_CONTEXT_AROUND_POSITION, json!({})),
        passthrough[3].clone(),
    ];
    for event in script {
        tx.try_send(event).expect("scripted event fits");
    }
    drop(tx);

    rig.pump
        .run(
            &mut events,
            &mut rig.channel,
            &mut rig.dispatcher,
            &mut rig.playback,
            &mut rig.excerpts,
            &mut |event| rig.forwarded.push(event),
        )
        .await
        .expect("no send fails in this script");

    assert_eq!(
        rig.forwarded,
        passthrough.to_vec(),
        "the stream flows around the consumed ToolCalls: exactly the \
         non-ToolCall events, still in order"
    );
    assert_eq!(
        rig.driver
            .tool_results()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["ok-1", "unknown-1", "refused-1", "read-1"],
        "every consumed ToolCall was answered on the wire instead, in order"
    );
    assert!(
        rig.playback.paused,
        "the successful mutation still executed"
    );
}
