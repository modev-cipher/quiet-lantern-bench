//! Role B adversarial pin suite for the VA6 dispatcher (docs/TASKS.md VA6;
//! VOICE_AGENT §5; ARCHITECTURE §8.1).
//!
//! Written by the test role against the dispatcher as a *suspect black box*:
//! every test pins an externally promised invariant (the §5 outcome
//! vocabulary, the §8.1 tier rules, the §4 egress shape) rather than the
//! implementation's own structure, and each pin was proven load-bearing by a
//! recorded local sabotage of the production code (red) before the pristine
//! code was restored (green) — see the commit footers.
//!
//! Kept separate from the in-module tests in `dispatcher.rs` on purpose: those
//! were written alongside the implementation; these are the independent
//! adversarial layer.

#![cfg(test)]

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::dispatcher::{
    answer_tool_call, validate, Dispatcher, ToolOutcome, CODE_INVALID_ARGS, CODE_RATE_LIMITED,
    CODE_UNKNOWN_CHAPTER, CODE_UNKNOWN_TOOL, TIER_ONE_MAX_CALLS, TIER_ONE_WINDOW,
};
use super::playback::InMemoryPlayback;
use super::registry::{self, ToolRegistry};
use crate::voice::channel::anchor::TEACHER_ANCHOR_TOOL;
use crate::voice::channel::mock::{MockChannel, MockDriver};
use crate::voice::channel::types::ToolResult;
use crate::voice::channel::types::{ChannelError, DelimitedExcerpt, ErrorKind, SessionContext};
use crate::voice::channel::SessionChannel;
use crate::voice::excerpt::selector::ExcerptSelector;
use crate::voice::excerpt::source::SessionSnapshot;
use crate::voice::excerpt::window::WindowPolicy;

/// Two chapters over three printed pages, five paragraph segments — the same
/// small book shape the VA4 fixtures use, rebuilt here so this suite shares no
/// helper with the code under test.
fn small_book() -> ExcerptSelector {
    ExcerptSelector::new(
        SessionSnapshot::from_voice_test_parts(
            vec![
                (1, 0, 1000, "pin book segment zero"),
                (1, 1000, 2000, "pin book segment one"),
                (2, 2000, 3000, "pin book segment two"),
                (2, 3000, 4000, "pin book segment three"),
                (3, 4000, 5000, "pin book segment four"),
            ],
            vec![(0, 2000), (2000, 5000)],
        ),
        WindowPolicy {
            before: 1,
            after: 1,
        },
    )
}

fn rig() -> (Dispatcher, InMemoryPlayback, ExcerptSelector) {
    (
        Dispatcher::new(ToolRegistry::new(false)),
        InMemoryPlayback::at(2500),
        small_book(),
    )
}

fn wire(outcome: ToolOutcome) -> ToolResult {
    outcome.into_tool_result()
}

// ---- pin 1: unknown-tool ------------------------------------------------
//
// §5: a name outside the session's offered vocabulary is answered
// `unknown_tool` — and an *ungated* `teacher_anchor` (a session that never
// offered the live preview) must be indistinguishable from a hallucinated
// name, both as a `ToolOutcome` and on the wire. Anything else leaks the
// existence of the gated tool to a model that was not offered it.

/// Model-plausible hallucinations: near-misses of real names, casing drift,
/// whitespace drift, the empty name, and names from other tool ecosystems.
const HALLUCINATED_NAMES: &[&str] = &[
    "open_shell",
    "pause",                   // prefix of a real tool
    "pause_playback ",         // trailing space
    " pause_playback",         // leading space
    "Pause_Playback",          // casing drift
    "PAUSE_PLAYBACK",          // shouting drift
    "seek_absolute",           // sibling that was never offered
    "jump_to_paragraph",       // plausible blend of two real tools
    "get_context",             // truncation of a real name
    "summarize_chapter",       // plausible widening of a real tool
    "read_file",               // generic agent-tool vocabulary
    "browser.open",            // namespaced foreign vocabulary
    "",                        // empty name
    "pausa_reproducci\u{f3}n", // non-ASCII drift
    "pause_playback\n",        // embedded newline
];

#[test]
fn pin1_hallucinated_names_answer_unknown_tool_and_touch_nothing() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    for name in HALLUCINATED_NAMES {
        let outcome = dispatcher.dispatch(
            name,
            &json!({}),
            Instant::now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(
            outcome,
            ToolOutcome::UnknownTool,
            "hallucinated name {name:?} must be answered unknown_tool"
        );
        let ToolResult::Err { code, message } = wire(outcome) else {
            panic!("unknown_tool must encode as ToolResult::Err");
        };
        assert_eq!(code, CODE_UNKNOWN_TOOL, "{name:?}");
        assert!(!message.is_empty(), "{name:?}: the model gets a message");
    }
    // Nothing executed: the playing surface is bit-identical to the rig start.
    assert_eq!(playback.position_ms, 2500);
    assert!(!playback.paused);
    assert_eq!(playback.speed, 1.0);
}

#[test]
fn pin1_ungated_teacher_anchor_is_wire_identical_to_a_hallucinated_name() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    // Args that would be perfectly valid on a gated session: the rejection
    // must come from the vocabulary gate, not from arg shape.
    let anchor_args = json!({"kind": "figure", "id": "fig-3"});
    let anchor = dispatcher.dispatch(
        TEACHER_ANCHOR_TOOL,
        &anchor_args,
        Instant::now(),
        &mut playback,
        &mut excerpts,
    );
    let hallucinated = dispatcher.dispatch(
        "definitely_not_a_tool",
        &anchor_args,
        Instant::now(),
        &mut playback,
        &mut excerpts,
    );
    assert_eq!(
        anchor, hallucinated,
        "ungated teacher_anchor and a hallucinated name must be the same ToolOutcome"
    );
    assert_eq!(
        wire(anchor),
        wire(hallucinated),
        "and byte-identical on the wire — code AND message — so the answer cannot \
         leak that the gated tool exists"
    );
    // The pure validation layer agrees with the dispatch layer.
    assert_eq!(
        validate(&ToolRegistry::new(false), TEACHER_ANCHOR_TOOL, &anchor_args),
        Err(ToolOutcome::UnknownTool)
    );
}

#[test]
fn pin1_unknown_names_cost_no_tier_one_budget() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let start = Instant::now();
    // A storm of rejected names inside one rate window...
    for i in 0..50 {
        let name = format!("hallucination_{i}");
        let outcome = dispatcher.dispatch(&name, &json!({}), start, &mut playback, &mut excerpts);
        assert_eq!(outcome, ToolOutcome::UnknownTool);
    }
    // ...must leave the full Tier-1 budget intact.
    for i in 0..TIER_ONE_MAX_CALLS {
        let outcome = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(
            matches!(outcome, ToolOutcome::Ok(_)),
            "budget slot {i} must have survived the unknown-name storm"
        );
    }
    // Sanity that the budget is real in this rig at all: the next one trips.
    let sixth = dispatcher.dispatch(
        registry::PAUSE_PLAYBACK,
        &json!({}),
        start,
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(&sixth, ToolOutcome::Failed { code, .. } if code == CODE_RATE_LIMITED),
        "got {sixth:?}"
    );
}
// ---- pin 2: bad-args battery ---------------------------------------------
//
// §5: an offered name with schema-breaking arguments answers `invalid_args`
// with an actionable detail, and a rejected call never reaches execution —
// so it costs zero Tier-1 budget and touches no playback state.

/// (tool, args, what-the-detail-must-mention) — one row per §5 failure class:
/// unknown field, wrong JSON type (including non-object args), missing
/// required field, out-of-bounds numerics on both edges, float-for-integer,
/// and negative-for-unsigned.
fn bad_args_battery() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        // unknown field (deny-unknown-fields everywhere)
        (registry::PAUSE_PLAYBACK, json!({"force": true}), "force"),
        (
            registry::SEEK_RELATIVE,
            json!({"seconds": 5, "unit": "s"}),
            "unit",
        ),
        (
            registry::GET_CONTEXT_AROUND_POSITION,
            json!({"pos_ms": 100}),
            "pos_ms",
        ),
        // wrong JSON type for the whole args value
        (registry::RESUME_PLAYBACK, json!([1, 2]), "array"),
        (registry::SEEK_RELATIVE, json!("30"), "string"),
        (registry::SET_PLAYBACK_SPEED, json!(1.5), "number"),
        (registry::JUMP_TO_CHAPTER, json!(true), "boolean"),
        // wrong JSON type for a field
        (registry::SEEK_RELATIVE, json!({"seconds": "30"}), "seconds"),
        (registry::SEEK_RELATIVE, json!({"seconds": null}), "seconds"),
        (
            registry::SET_PLAYBACK_SPEED,
            json!({"speed": [1.0]}),
            "speed",
        ),
        (
            registry::JUMP_TO_CHAPTER,
            json!({"chapter": "3"}),
            "chapter",
        ),
        (
            registry::GET_CONTEXT_AROUND_POSITION,
            json!({"position_ms": {}}),
            "position_ms",
        ),
        // missing required field
        (registry::SEEK_RELATIVE, json!({}), "seconds"),
        (registry::SET_PLAYBACK_SPEED, json!({}), "speed"),
        (registry::JUMP_TO_CHAPTER, json!({}), "chapter"),
        (registry::JUMP_TO_PAGE, json!({}), "page"),
        // right field name on the wrong tool = unknown field + missing required
        (registry::JUMP_TO_CHAPTER, json!({"page": 2}), "page"),
        // out-of-bounds numerics, both edges, epsilon and gross
        (registry::SEEK_RELATIVE, json!({"seconds": 600.001}), "600"),
        (
            registry::SEEK_RELATIVE,
            json!({"seconds": -600.001}),
            "-600",
        ),
        (registry::SEEK_RELATIVE, json!({"seconds": 1e9}), "600"),
        (registry::SET_PLAYBACK_SPEED, json!({"speed": 0.499}), "0.5"),
        (registry::SET_PLAYBACK_SPEED, json!({"speed": 3.001}), "3"),
        (registry::SET_PLAYBACK_SPEED, json!({"speed": 0}), "0.5"),
        // float-for-integer
        (
            registry::JUMP_TO_CHAPTER,
            json!({"chapter": 1.5}),
            "chapter",
        ),
        (
            registry::JUMP_TO_CHAPTER,
            json!({"chapter": 2.0}),
            "chapter",
        ),
        (registry::JUMP_TO_PAGE, json!({"page": 0.5}), "page"),
        (
            registry::GET_CONTEXT_AROUND_POSITION,
            json!({"position_ms": 10.5}),
            "position_ms",
        ),
        // F8-D1b bounded-range fetch: inverted/empty range, one bound only,
        // unknown field, float and negative positions.
        (
            registry::GET_CONTEXT_FOR_RANGE,
            json!({"start_position": 5, "end_position": 5}),
            "end_position",
        ),
        (
            registry::GET_CONTEXT_FOR_RANGE,
            json!({"start_position": 9, "end_position": 3}),
            "end_position",
        ),
        (
            registry::GET_CONTEXT_FOR_RANGE,
            json!({"start_position": 0}),
            "end_position",
        ),
        (
            registry::GET_CONTEXT_FOR_RANGE,
            json!({"end_position": 10}),
            "start_position",
        ),
        (
            registry::GET_CONTEXT_FOR_RANGE,
            json!({"begin": 0, "end_position": 10}),
            "begin",
        ),
        (
            registry::GET_CONTEXT_FOR_RANGE,
            json!({"start_position": 1.5, "end_position": 10}),
            "start_position",
        ),
        (
            registry::GET_CONTEXT_FOR_RANGE,
            json!({"start_position": -1, "end_position": 10}),
            "start_position",
        ),
        // negative-for-unsigned
        (registry::JUMP_TO_CHAPTER, json!({"chapter": -1}), "chapter"),
        (registry::JUMP_TO_PAGE, json!({"page": -3}), "page"),
        (
            registry::GET_CONTEXT_AROUND_POSITION,
            json!({"position_ms": -1}),
            "position_ms",
        ),
    ]
}

#[test]
fn pin2_every_schema_break_answers_invalid_args_with_an_actionable_detail() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    for (name, args, must_mention) in bad_args_battery() {
        let outcome =
            dispatcher.dispatch(name, &args, Instant::now(), &mut playback, &mut excerpts);
        let ToolOutcome::InvalidArgs { detail } = &outcome else {
            panic!("{name} {args}: expected invalid_args, got {outcome:?}");
        };
        assert!(
            detail.contains(must_mention),
            "{name} {args}: detail {detail:?} must mention {must_mention:?} so the \
             model can repair the call"
        );
        let ToolResult::Err { code, message } = wire(outcome.clone()) else {
            panic!("invalid_args must encode as ToolResult::Err");
        };
        assert_eq!(code, CODE_INVALID_ARGS, "{name} {args}");
        assert_eq!(message, *detail, "the detail must survive to the wire");
    }
    // No rejected call reached execution: playback is untouched.
    assert_eq!(playback.position_ms, 2500);
    assert!(!playback.paused);
    assert_eq!(playback.speed, 1.0);
}

#[test]
fn pin2_schema_breaks_cost_zero_tier_one_budget() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let start = Instant::now();
    // The full battery, all inside one rate window...
    for (name, args, _) in bad_args_battery() {
        let outcome = dispatcher.dispatch(name, &args, start, &mut playback, &mut excerpts);
        assert!(matches!(outcome, ToolOutcome::InvalidArgs { .. }));
    }
    // ...and the whole Tier-1 budget is still there.
    for _ in 0..TIER_ONE_MAX_CALLS {
        let outcome = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(outcome, ToolOutcome::Ok(_)));
    }
    let sixth = dispatcher.dispatch(
        registry::PAUSE_PLAYBACK,
        &json!({}),
        start,
        &mut playback,
        &mut excerpts,
    );
    assert!(matches!(&sixth, ToolOutcome::Failed { code, .. } if code == CODE_RATE_LIMITED));
}

#[test]
fn pin2_boundary_values_inside_the_spec_are_not_rejected() {
    // The battery above pins rejection; this pins the complement so a
    // sabotage cannot pass by rejecting everything.
    let registry_ = ToolRegistry::new(false);
    for (name, args) in [
        (registry::SEEK_RELATIVE, json!({"seconds": 600})),
        (registry::SEEK_RELATIVE, json!({"seconds": -600})),
        (registry::SEEK_RELATIVE, json!({"seconds": 600.0})),
        (registry::SET_PLAYBACK_SPEED, json!({"speed": 0.5})),
        (registry::SET_PLAYBACK_SPEED, json!({"speed": 3.0})),
        (registry::JUMP_TO_CHAPTER, json!({"chapter": 0})),
        (registry::JUMP_TO_PAGE, json!({"page": 0})),
        (
            registry::GET_CONTEXT_AROUND_POSITION,
            json!({"position_ms": 0}),
        ),
        (
            registry::GET_CONTEXT_AROUND_POSITION,
            json!({"position_ms": null}),
        ),
    ] {
        assert!(
            validate(&registry_, name, &args).is_ok(),
            "{name} {args}: an exact-boundary or spec-legal value must validate"
        );
    }
}

// ---- pin 3: round trip ----------------------------------------------------
//
// §5/§3: the harness owes every `ToolCall` event exactly one
// `send_tool_result` — same call_id, emission order — regardless of outcome
// class. Across a session renewal or a transport failure the ack may be
// *delayed* (the send errors and the harness retries), but never dropped,
// duplicated, or re-ordered, and a failed send must reach the provider
// record not at all.

async fn connected_mock() -> (MockChannel, MockDriver) {
    let (mut channel, driver) = MockChannel::builder().build();
    channel
        .connect(SessionContext {
            book_id: Some("pin-book".to_string()),
            session_id: "pin-session".to_string(),
            system_prompt: "tutor".to_string(),
            excerpt: DelimitedExcerpt {
                delimited_text: "<s>x</s>".to_string(),
            },
            rolling_summary: None,
            playback_pos_ms: 0,
            tool_schemas: ToolRegistry::new(false).schemas(),
        })
        .await
        .expect("mock connects");
    (channel, driver)
}

#[tokio::test]
async fn pin3_n_calls_in_exactly_n_acks_out_same_ids_same_order() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    excerpts.advance_to(2500);
    let (mut channel, driver) = connected_mock().await;
    // Every outcome class §5 names, plus awkward-but-legal call ids: a
    // duplicate id (two distinct provider events may collide on id — the
    // harness echoes, it does not dedupe), the empty id, and a non-ASCII id.
    let calls: Vec<(&str, &str, Value)> = vec![
        ("ok-1", registry::PAUSE_PLAYBACK, json!({})),
        ("unknown-2", "made_up_tool", json!({})),
        ("bad-3", registry::SET_PLAYBACK_SPEED, json!({"speed": 99})),
        ("dup", registry::RESUME_PLAYBACK, json!({})),
        ("dup", registry::RESUME_PLAYBACK, json!({})),
        ("", registry::SUMMARIZE_SECTION, json!({})),
        (
            "id-\u{7ba1}",
            registry::JUMP_TO_CHAPTER,
            json!({"chapter": 99}),
        ),
        ("ok-8", registry::GET_CONTEXT_AROUND_POSITION, json!({})),
    ];
    for (call_id, name, args) in &calls {
        answer_tool_call(
            &mut dispatcher,
            &mut channel,
            call_id,
            name,
            args,
            Instant::now(),
            &mut playback,
            &mut excerpts,
        )
        .await
        .expect("mock send succeeds");
    }
    let answered = driver.tool_results();
    assert_eq!(
        answered
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        calls.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(),
        "exactly one ack per call, ids echoed verbatim, emission order kept"
    );
    // Outcome classes land where §5 says they land.
    let codes: Vec<Option<&str>> = answered
        .iter()
        .map(|(_, result)| match result {
            ToolResult::Ok(_) => None,
            ToolResult::Err { code, .. } => Some(code.as_str()),
        })
        .collect();
    assert_eq!(
        codes,
        vec![
            None,
            Some(CODE_UNKNOWN_TOOL),
            Some(CODE_INVALID_ARGS),
            None,
            None,
            None,
            Some(CODE_UNKNOWN_CHAPTER),
            None,
        ]
    );
}

#[tokio::test]
async fn pin3_a_renewal_gap_delays_the_ack_but_never_drops_or_forks_it() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let (mut channel, driver) = connected_mock().await;
    let t0 = Instant::now();
    answer_tool_call(
        &mut dispatcher,
        &mut channel,
        "c1",
        registry::PAUSE_PLAYBACK,
        &json!({}),
        t0,
        &mut playback,
        &mut excerpts,
    )
    .await
    .expect("pre-renewal send");

    driver.announce_session_end();
    let err = answer_tool_call(
        &mut dispatcher,
        &mut channel,
        "c2",
        registry::RESUME_PLAYBACK,
        &json!({}),
        t0,
        &mut playback,
        &mut excerpts,
    )
    .await
    .expect_err("a send during renewal must surface, not vanish");
    assert_eq!(err.kind, ErrorKind::SessionExpired);
    assert!(err.retryable, "renewal is a wait-and-retry condition");
    assert_eq!(
        driver.tool_results().len(),
        1,
        "the failed ack must not be half-recorded"
    );

    driver.complete_renewal();
    // The harness retries the same call after the renewal completes.
    answer_tool_call(
        &mut dispatcher,
        &mut channel,
        "c2",
        registry::RESUME_PLAYBACK,
        &json!({}),
        t0,
        &mut playback,
        &mut excerpts,
    )
    .await
    .expect("post-renewal retry");
    answer_tool_call(
        &mut dispatcher,
        &mut channel,
        "c3",
        registry::SET_PLAYBACK_SPEED,
        &json!({"speed": 1.5}),
        t0,
        &mut playback,
        &mut excerpts,
    )
    .await
    .expect("post-renewal fresh call");

    let answered = driver.tool_results();
    assert_eq!(
        answered
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["c1", "c2", "c3"],
        "one ack per logical call across the renewal — none lost, none doubled"
    );
    assert!(answered.iter().all(|(_, r)| matches!(r, ToolResult::Ok(_))));
}

#[tokio::test]
async fn pin3_an_armed_transport_failure_surfaces_and_records_nothing() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let (mut channel, driver) = connected_mock().await;
    driver.fail_next_send(ChannelError::new(ErrorKind::RateLimited, true, "429"));
    let err = answer_tool_call(
        &mut dispatcher,
        &mut channel,
        "f1",
        registry::PAUSE_PLAYBACK,
        &json!({}),
        Instant::now(),
        &mut playback,
        &mut excerpts,
    )
    .await
    .expect_err("the transport failure must reach the caller");
    assert_eq!(err.kind, ErrorKind::RateLimited);
    assert!(driver.tool_results().is_empty(), "no ghost ack");

    answer_tool_call(
        &mut dispatcher,
        &mut channel,
        "f1",
        registry::PAUSE_PLAYBACK,
        &json!({}),
        Instant::now(),
        &mut playback,
        &mut excerpts,
    )
    .await
    .expect("the retry lands");
    assert_eq!(
        driver
            .tool_results()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["f1"],
        "exactly one ack after the retry"
    );
}

// ---- pin 4: the Tier-1 rate window ----------------------------------------
//
// ARCHITECTURE §8.1: ~5 calls / 10 s across *all* Tier-1 mutations as one
// shared budget; Tier-0 reads are never limited; a refused call must not
// itself occupy the window (or a chatty model gets rate-limited forever).

#[test]
fn pin4_all_tier_one_names_share_one_budget_and_tier_zero_is_immune() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    excerpts.advance_to(2500);
    let start = Instant::now();
    // Five different Tier-1 mutations, one shared window.
    let mixed: [(&str, Value); 5] = [
        (registry::PAUSE_PLAYBACK, json!({})),
        (registry::SEEK_RELATIVE, json!({"seconds": 1})),
        (registry::SET_PLAYBACK_SPEED, json!({"speed": 1.25})),
        (registry::JUMP_TO_CHAPTER, json!({"chapter": 0})),
        (registry::REPLAY_LAST_MINUTE, json!({})),
    ];
    for (name, args) in &mixed {
        let outcome = dispatcher.dispatch(name, args, start, &mut playback, &mut excerpts);
        assert!(matches!(outcome, ToolOutcome::Ok(_)), "{name} in budget");
    }
    // The sixth Tier-1 call is a *different* name again: still refused.
    let sixth = dispatcher.dispatch(
        registry::RESUME_PLAYBACK,
        &json!({}),
        start,
        &mut playback,
        &mut excerpts,
    );
    let ToolOutcome::Failed { code, message } = &sixth else {
        panic!("expected rate_limited, got {sixth:?}");
    };
    assert_eq!(code, CODE_RATE_LIMITED);
    assert!(
        message.contains('5') && message.contains("10"),
        "the refusal names the budget so the model can pace itself: {message:?}"
    );
    // A Tier-0 storm inside the exhausted window: never limited.
    for _ in 0..100 {
        let read = dispatcher.dispatch(
            registry::SUMMARIZE_SECTION,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(read, ToolOutcome::Ok(_)));
        let read = dispatcher.dispatch(
            registry::GET_CONTEXT_AROUND_POSITION,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(read, ToolOutcome::Ok(_)));
    }
}

#[test]
fn pin4_a_refused_call_does_not_occupy_the_window() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let start = Instant::now();
    for _ in 0..TIER_ONE_MAX_CALLS {
        let outcome = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(outcome, ToolOutcome::Ok(_)));
    }
    // Five refusals halfway through the window...
    let halfway = start + TIER_ONE_WINDOW / 2;
    for _ in 0..TIER_ONE_MAX_CALLS {
        let refused = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            halfway,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(&refused, ToolOutcome::Failed { code, .. } if code == CODE_RATE_LIMITED));
    }
    // ...must leave no trace: once the original five expire, the budget is
    // whole again. If refusals occupied slots, this call would be refused.
    let after = dispatcher.dispatch(
        registry::PAUSE_PLAYBACK,
        &json!({}),
        start + TIER_ONE_WINDOW,
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(after, ToolOutcome::Ok(_)),
        "refused calls must cost nothing, got {after:?}"
    );
}

#[test]
fn pin4_the_window_edge_is_exact() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let start = Instant::now();
    for _ in 0..TIER_ONE_MAX_CALLS {
        dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
    }
    // One millisecond before the window closes: still refused.
    let just_before = start + (TIER_ONE_WINDOW - Duration::from_millis(1));
    let refused = dispatcher.dispatch(
        registry::PAUSE_PLAYBACK,
        &json!({}),
        just_before,
        &mut playback,
        &mut excerpts,
    );
    assert!(matches!(&refused, ToolOutcome::Failed { code, .. } if code == CODE_RATE_LIMITED));
    // At exactly the window length the oldest entries age out.
    let at_edge = dispatcher.dispatch(
        registry::PAUSE_PLAYBACK,
        &json!({}),
        start + TIER_ONE_WINDOW,
        &mut playback,
        &mut excerpts,
    );
    assert!(matches!(at_edge, ToolOutcome::Ok(_)));
}

#[test]
fn pin4_the_window_slides_per_call_not_in_batches() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let start = Instant::now();
    // Five calls spread over 8 s: t+0, t+2, t+4, t+6, t+8.
    for i in 0..TIER_ONE_MAX_CALLS {
        let at = start + Duration::from_secs(2 * i as u64);
        let outcome = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            at,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(outcome, ToolOutcome::Ok(_)), "call {i}");
    }
    // At t+10 only the t+0 entry has aged out: exactly one slot free.
    let t10 = start + TIER_ONE_WINDOW;
    let freed = dispatcher.dispatch(
        registry::PAUSE_PLAYBACK,
        &json!({}),
        t10,
        &mut playback,
        &mut excerpts,
    );
    assert!(matches!(freed, ToolOutcome::Ok(_)), "the freed slot");
    let refused = dispatcher.dispatch(
        registry::PAUSE_PLAYBACK,
        &json!({}),
        t10,
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(&refused, ToolOutcome::Failed { code, .. } if code == CODE_RATE_LIMITED),
        "one aged-out entry frees exactly one slot, got {refused:?}"
    );
}

#[test]
fn pin4_execution_refusals_cost_no_budget_only_performed_mutations_do() {
    // The H3 decision (docs/TASKS.md VA6; ARCHITECTURE §8.1; VOICE_AGENT §5):
    // §8.1 rates *mutations*, and playback_unavailable's own message tells
    // the model to retry — so a call that mutated nothing (surface gone,
    // jump target missing) must leave the window untouched, or the model is
    // rate-limited at the exact moment the surface returns.
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let start = Instant::now();
    playback.available = false;
    for _ in 0..50 {
        let refused = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(
            matches!(&refused, ToolOutcome::Failed { code, .. } if code == "playback_unavailable")
        );
    }
    playback.available = true;
    for _ in 0..50 {
        let miss = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 99}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(&miss, ToolOutcome::Failed { code, .. } if code == CODE_UNKNOWN_CHAPTER));
    }
    // A hundred refusals inside one window: the full budget must survive.
    for i in 0..TIER_ONE_MAX_CALLS {
        let outcome = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(
            matches!(outcome, ToolOutcome::Ok(_)),
            "budget slot {i} must have survived the refusal storm"
        );
    }
    // And the budget is still real: performed mutations occupy it.
    let sixth = dispatcher.dispatch(
        registry::PAUSE_PLAYBACK,
        &json!({}),
        start,
        &mut playback,
        &mut excerpts,
    );
    assert!(matches!(&sixth, ToolOutcome::Failed { code, .. } if code == CODE_RATE_LIMITED));
}

#[test]
fn pin4_an_unrouted_teacher_anchor_consumes_no_budget() {
    // On a gated session teacher_anchor is Tier-1 vocabulary, but the
    // dispatcher answers it unrouted_tool before the budget check — a
    // mis-wired session must not eat the playback budget as a side effect.
    let mut dispatcher = Dispatcher::new(ToolRegistry::new(true));
    let (_, mut playback, mut excerpts) = rig();
    let start = Instant::now();
    for _ in 0..50 {
        let outcome = dispatcher.dispatch(
            TEACHER_ANCHOR_TOOL,
            &json!({"kind": "section", "id": "s1"}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(&outcome, ToolOutcome::Failed { code, .. } if code == "unrouted_tool"));
    }
    for _ in 0..TIER_ONE_MAX_CALLS {
        let outcome = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(outcome, ToolOutcome::Ok(_)));
    }
}

// ---- pin 5: the surface can vanish; targets always stay in the book -------
//
// §5: "errors answered, never panicked" — a torn-down playing surface
// (closed book, reloading webview) answers playback_unavailable on every
// tool that touches it, while Tier-0 reads that don't need the surface keep
// answering. Seek arithmetic clamps to [0, end-of-book] everywhere, replay
// tools saturate at the top, and a zero-segment session degrades to answers,
// not panics.

/// Every offered tool that must touch the playing surface to do its job,
/// with valid args. get_context appears in its no-position form (it reads
/// the playback clock); its explicit-position form belongs to the Tier-0
/// independence pin below.
fn playback_touching_calls() -> Vec<(&'static str, Value)> {
    vec![
        (registry::PAUSE_PLAYBACK, json!({})),
        (registry::RESUME_PLAYBACK, json!({})),
        (registry::SEEK_RELATIVE, json!({"seconds": 30})),
        (registry::SET_PLAYBACK_SPEED, json!({"speed": 2.0})),
        (registry::REPLAY_PREVIOUS_PARAGRAPH, json!({})),
        (registry::REPLAY_PREVIOUS_PAGE, json!({})),
        (registry::REPLAY_LAST_MINUTE, json!({})),
        (registry::JUMP_TO_CHAPTER, json!({"chapter": 0})),
        (registry::JUMP_TO_PAGE, json!({"page": 1})),
        (registry::GET_CONTEXT_AROUND_POSITION, json!({})),
    ]
}

#[test]
fn pin5_every_playback_touching_tool_answers_unavailable_never_panics() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    playback.available = false;
    let start = Instant::now();
    for (i, (name, args)) in playback_touching_calls().into_iter().enumerate() {
        // Spread the calls one window apart. Since the H3 decision an
        // unavailable surface costs no budget (see the pin4 refusal-storm
        // pin), so the spread is belt-and-braces isolation, not a necessity.
        let at = start + TIER_ONE_WINDOW * (i as u32 + 1);
        let outcome = dispatcher.dispatch(name, &args, at, &mut playback, &mut excerpts);
        let ToolOutcome::Failed { code, message } = &outcome else {
            panic!("{name}: expected playback_unavailable, got {outcome:?}");
        };
        assert_eq!(code, "playback_unavailable", "{name}");
        assert!(
            message.contains("retry"),
            "{name}: the model is told this is retryable: {message:?}"
        );
    }
}

#[test]
fn pin5_tier_zero_reads_answer_even_with_the_surface_gone() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    excerpts.advance_to(2500);
    playback.available = false;
    let summarize = dispatcher.dispatch(
        registry::SUMMARIZE_SECTION,
        &json!({}),
        Instant::now(),
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(summarize, ToolOutcome::Ok(_)),
        "summarize needs no playing surface, got {summarize:?}"
    );
    let explicit = dispatcher.dispatch(
        registry::GET_CONTEXT_AROUND_POSITION,
        &json!({"position_ms": 4500}),
        Instant::now(),
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(explicit, ToolOutcome::Ok(_)),
        "an explicit position needs no playback clock, got {explicit:?}"
    );
}

#[test]
fn pin5_jump_misses_resolve_against_the_book_before_the_surface() {
    // A bogus target is a better answer than "surface gone": the model can
    // repair the target while playback is still coming back.
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    playback.available = false;
    let chapter_miss = dispatcher.dispatch(
        registry::JUMP_TO_CHAPTER,
        &json!({"chapter": 9}),
        Instant::now(),
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(&chapter_miss, ToolOutcome::Failed { code, .. } if code == CODE_UNKNOWN_CHAPTER),
        "got {chapter_miss:?}"
    );
    let printed_miss = dispatcher.dispatch(
        registry::JUMP_TO_PAGE,
        &json!({"page": 7}),
        Instant::now() + TIER_ONE_WINDOW,
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(&printed_miss, ToolOutcome::Failed { code, .. } if code == "unknown_page"),
        "got {printed_miss:?}"
    );
}

#[test]
fn pin5_seek_clamps_to_the_timeline_from_every_position() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let start = Instant::now();
    // (start position, seconds, expected clamped target)
    let cases: [(u64, f64, u64); 5] = [
        (0, -0.5, 0),         // tiny underflow at the very top
        (2500, -600.0, 0),    // gross underflow mid-book
        (4900, 0.2, 5000),    // small overflow near the end
        (5000, 600.0, 5000),  // gross overflow at the exact end
        (2500, 0.0004, 2500), // sub-millisecond offset rounds to staying put
    ];
    for (i, (from, seconds, expected)) in cases.into_iter().enumerate() {
        playback.position_ms = from;
        let at = start + TIER_ONE_WINDOW * (i as u32 + 1);
        let outcome = dispatcher.dispatch(
            registry::SEEK_RELATIVE,
            &json!({"seconds": seconds}),
            at,
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(
            outcome,
            ToolOutcome::Ok(json!({"position_ms": expected})),
            "from {from} by {seconds}s"
        );
        assert_eq!(playback.position_ms, expected, "the surface moved with it");
    }
}

#[test]
fn pin5_replay_tools_saturate_at_the_top_of_the_book() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    let start = Instant::now();
    // replay_last_minute: exact saturation arithmetic on both sides of 60 s.
    for (i, (from, expected)) in [(0u64, 0u64), (59_999, 0), (61_000, 1_000)]
        .into_iter()
        .enumerate()
    {
        playback.position_ms = from;
        let at = start + TIER_ONE_WINDOW * (i as u32 + 1);
        dispatcher.dispatch(
            registry::REPLAY_LAST_MINUTE,
            &json!({}),
            at,
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, expected, "last-minute from {from}");
    }
    // replay_previous_page from the third printed page lands on the second's
    // start; from the first it clamps to the start of the book.
    playback.position_ms = 4500;
    dispatcher.dispatch(
        registry::REPLAY_PREVIOUS_PAGE,
        &json!({}),
        start + TIER_ONE_WINDOW * 4,
        &mut playback,
        &mut excerpts,
    );
    assert_eq!(playback.position_ms, 2000);
    playback.position_ms = 500;
    dispatcher.dispatch(
        registry::REPLAY_PREVIOUS_PAGE,
        &json!({}),
        start + TIER_ONE_WINDOW * 5,
        &mut playback,
        &mut excerpts,
    );
    assert_eq!(playback.position_ms, 0);
    // replay_previous_paragraph inside the first paragraph clamps to its own
    // start rather than under-flowing.
    playback.position_ms = 700;
    dispatcher.dispatch(
        registry::REPLAY_PREVIOUS_PARAGRAPH,
        &json!({}),
        start + TIER_ONE_WINDOW * 6,
        &mut playback,
        &mut excerpts,
    );
    assert_eq!(playback.position_ms, 0);
}

#[test]
fn pin5_a_zero_segment_session_answers_everything_and_panics_never() {
    let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
    let mut playback = InMemoryPlayback::at(2500);
    let mut excerpts = ExcerptSelector::new(
        SessionSnapshot::from_voice_test_parts(vec![], vec![]),
        WindowPolicy {
            before: 1,
            after: 1,
        },
    );
    let start = Instant::now();
    // F7 K10 fix: a relative seek over an empty timeline refuses typed —
    // the pre-F7 behavior silently clamped to 0 and answered success.
    let seek = dispatcher.dispatch(
        registry::SEEK_RELATIVE,
        &json!({"seconds": 600}),
        start,
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(&seek, ToolOutcome::Failed { code, .. } if code == "no_context"),
        "got {seek:?}"
    );
    assert_eq!(playback.position_ms, 2500, "the refused seek moved nothing");
    dispatcher.dispatch(
        registry::REPLAY_PREVIOUS_PAGE,
        &json!({}),
        start + TIER_ONE_WINDOW,
        &mut playback,
        &mut excerpts,
    );
    assert_eq!(playback.position_ms, 0);
    // Structure-dependent tools miss loudly instead of panicking.
    let chapter = dispatcher.dispatch(
        registry::JUMP_TO_CHAPTER,
        &json!({"chapter": 0}),
        start + TIER_ONE_WINDOW * 2,
        &mut playback,
        &mut excerpts,
    );
    assert!(matches!(&chapter, ToolOutcome::Failed { code, .. } if code == CODE_UNKNOWN_CHAPTER));
    let context = dispatcher.dispatch(
        registry::GET_CONTEXT_AROUND_POSITION,
        &json!({"position_ms": 0}),
        start + TIER_ONE_WINDOW * 3,
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(&context, ToolOutcome::Failed { code, .. } if code == "no_context"),
        "got {context:?}"
    );
    let summarize = dispatcher.dispatch(
        registry::SUMMARIZE_SECTION,
        &json!({}),
        start + TIER_ONE_WINDOW * 4,
        &mut playback,
        &mut excerpts,
    );
    assert!(
        matches!(&summarize, ToolOutcome::Failed { code, .. } if code == "no_context"),
        "got {summarize:?}"
    );
}

// ---- pin 6 (crown jewel): the egress cap ----------------------------------
//
// VOICE_AGENT §4: book text leaves a tool outcome ONLY as the selector's
// sentinel-delimited excerpt, hard-capped at 64 KiB of book bytes — and a
// fired cap is never silent: the payload carries an explicit
// hard_cap{kept_bytes, dropped_bytes} rider all the way to the wire.

/// Split a delimited excerpt into (open marker, inner payload, close marker).
fn split_delimited(book_text: &str) -> (&str, &str, &str) {
    let open_end = book_text.find('\n').expect("open marker line");
    let close_start = book_text.rfind('\n').expect("close marker line");
    (
        &book_text[..open_end],
        &book_text[open_end + 1..close_start],
        &book_text[close_start + 1..],
    )
}

/// Assert the §4 delimiter shape and return (token, inner payload).
fn assert_sentinel_shape(book_text: &str) -> (String, &str) {
    let (open, inner, close) = split_delimited(book_text);
    let token = open
        .strip_prefix("<<untrusted-book-excerpt ")
        .and_then(|rest| rest.strip_suffix(">>"))
        .unwrap_or_else(|| panic!("malformed open marker: {open:?}"));
    assert_eq!(token.len(), 32, "128-bit hex token");
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(
        close,
        format!("<<end-untrusted-book-excerpt {token}>>"),
        "close marker carries the same session token"
    );
    assert_eq!(
        book_text.matches(token).count(),
        2,
        "the token appears exactly twice — collision-scanned serialization"
    );
    (token.to_string(), inner)
}

fn ok_payload(outcome: ToolOutcome) -> Value {
    match outcome {
        ToolOutcome::Ok(payload) => payload,
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[test]
fn pin6_tier_zero_payloads_are_sentinel_delimited_selector_output_only() {
    let (mut dispatcher, mut playback, mut excerpts) = rig();
    excerpts.advance_to(2500);
    let start = Instant::now();
    for args in [json!({}), json!({"position_ms": 2500})] {
        let name = if args == json!({}) {
            registry::SUMMARIZE_SECTION
        } else {
            registry::GET_CONTEXT_AROUND_POSITION
        };
        let payload =
            ok_payload(dispatcher.dispatch(name, &args, start, &mut playback, &mut excerpts));
        let keys: Vec<&str> = payload
            .as_object()
            .expect("object payload")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["book_text", "coverage"],
            "{name}: under the cap the payload is the delimited text plus the \
             F8 D1 coverage line and NOTHING else"
        );
        let book_text = payload["book_text"].as_str().expect("string");
        let (_, inner) = assert_sentinel_shape(book_text);
        assert_eq!(
            inner,
            "[page 1]\npin book segment one\n\n[page 2]\npin book segment two\n\npin book segment three",
            "{name}: the inner payload is exactly the selector's window join, page-marked"
        );
    }
    // Tier-1 acks and error messages must carry zero book text.
    let paused = ok_payload(dispatcher.dispatch(
        registry::PAUSE_PLAYBACK,
        &json!({}),
        start,
        &mut playback,
        &mut excerpts,
    ));
    assert_eq!(
        paused,
        json!({"paused": true}),
        "no book text rides a mutation ack"
    );
    let miss = dispatcher.dispatch(
        registry::JUMP_TO_CHAPTER,
        &json!({"chapter": 42}),
        start,
        &mut playback,
        &mut excerpts,
    );
    let ToolOutcome::Failed { message, .. } = &miss else {
        panic!("expected a miss");
    };
    assert!(
        !message.contains("pin book segment"),
        "error messages must not leak book text: {message:?}"
    );
}

/// Three 30 000-byte segments; the (1,1) window joins all three = 90 004
/// bytes of book text, 24 468 over the 64 KiB backstop.
fn oversized_book() -> ExcerptSelector {
    let seg = "a".repeat(30_000);
    ExcerptSelector::new(
        SessionSnapshot::from_voice_test_parts(
            vec![
                (1, 0, 1000, seg.as_str()),
                (1, 1000, 2000, seg.as_str()),
                (1, 2000, 3000, seg.as_str()),
            ],
            vec![(0, 3000)],
        ),
        WindowPolicy {
            before: 1,
            after: 1,
        },
    )
}

const CAP: usize = 64 * 1024;

#[test]
fn pin6_an_oversized_window_is_hard_capped_with_an_explicit_notice() {
    let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
    let mut playback = InMemoryPlayback::at(1500);
    let mut excerpts = oversized_book();
    excerpts.advance_to(1500);
    let start = Instant::now();
    for (name, args) in [
        (registry::SUMMARIZE_SECTION, json!({})),
        (
            registry::GET_CONTEXT_AROUND_POSITION,
            json!({"position_ms": 1500}),
        ),
    ] {
        let payload =
            ok_payload(dispatcher.dispatch(name, &args, start, &mut playback, &mut excerpts));
        let keys: Vec<&str> = payload
            .as_object()
            .expect("object payload")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["book_text", "coverage", "hard_cap"],
            "{name}: a fired cap MUST ride the payload — silent truncation is the defect \
             this pin exists to catch"
        );
        assert_eq!(
            payload["hard_cap"],
            json!({"kept_bytes": CAP, "dropped_bytes": 90_004 - CAP}),
            "{name}: exact loud arithmetic"
        );
        let book_text = payload["book_text"].as_str().expect("string");
        let (_, inner) = assert_sentinel_shape(book_text);
        assert_eq!(
            crate::voice::excerpt::selector::strip_page_markers(inner).len(),
            CAP,
            "{name}: exactly the cap of book bytes (page markers ride on top)"
        );
        assert!(
            book_text.len() > CAP,
            "{name}: the cap counts book bytes; the markers ride on top"
        );
    }
}

#[test]
fn pin6_the_cap_cuts_at_a_char_boundary_through_the_dispatcher() {
    // 65 534 ASCII bytes + one 3-byte char straddling the cap: the whole
    // char drops, through the full dispatch path.
    let mut text = "z".repeat(CAP - 2);
    text.push('\u{8a9e}');
    let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
    let mut playback = InMemoryPlayback::at(0);
    let mut excerpts = ExcerptSelector::new(
        SessionSnapshot::from_voice_test_parts(vec![(1, 0, 1000, text.as_str())], vec![(0, 1000)]),
        WindowPolicy {
            before: 0,
            after: 0,
        },
    );
    excerpts.advance_to(0);
    let payload = ok_payload(dispatcher.dispatch(
        registry::SUMMARIZE_SECTION,
        &json!({}),
        Instant::now(),
        &mut playback,
        &mut excerpts,
    ));
    assert_eq!(
        payload["hard_cap"],
        json!({"kept_bytes": CAP - 2, "dropped_bytes": 3})
    );
    let (_, inner) = assert_sentinel_shape(payload["book_text"].as_str().expect("string"));
    let inner = crate::voice::excerpt::selector::strip_page_markers(inner);
    assert_eq!(inner.len(), CAP - 2);
    assert!(inner.is_char_boundary(inner.len()));
}

#[tokio::test]
async fn pin6_the_cap_notice_survives_to_the_wire() {
    let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
    let mut playback = InMemoryPlayback::at(1500);
    let mut excerpts = oversized_book();
    excerpts.advance_to(1500);
    let (mut channel, driver) = connected_mock().await;
    answer_tool_call(
        &mut dispatcher,
        &mut channel,
        "cap-1",
        registry::SUMMARIZE_SECTION,
        &json!({}),
        Instant::now(),
        &mut playback,
        &mut excerpts,
    )
    .await
    .expect("send succeeds");
    let answered = driver.tool_results();
    let (id, ToolResult::Ok(payload)) = &answered[0] else {
        panic!("the capped read still answers Ok");
    };
    assert_eq!(id, "cap-1");
    assert_eq!(
        payload["hard_cap"],
        json!({"kept_bytes": CAP, "dropped_bytes": 90_004 - CAP}),
        "the provider hears about the drop, verbatim"
    );
    assert_sentinel_shape(payload["book_text"].as_str().expect("string"));
}
