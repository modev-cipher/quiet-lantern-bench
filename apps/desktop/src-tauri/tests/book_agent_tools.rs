//! B-1 gate fixtures: Agent act verbs, registry separation, and the
//! playhead-decoupled citation contract (feature `book-agent`).
//! Run: cargo test --features book-agent --test book_agent_tools
#![cfg(feature = "book-agent")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use txtcaster_lib::reading::{GeometryOutcome, ReadingChapter, ReadingSegment, TimelineOrigin};
use txtcaster_lib::retrieval::agent_port::{RetrievalPortAdapter, ScriptedHighlightPort};
use txtcaster_lib::retrieval::contract::HandleTable;
use txtcaster_lib::retrieval::{
    BookFingerprint, Provenance, RetrievalProvider, SegmentRow, TantivyRetrieval,
};
use txtcaster_lib::voice::excerpt::selector::ExcerptSelector;
use txtcaster_lib::voice::excerpt::source::SessionSnapshot;
use txtcaster_lib::voice::excerpt::window::WindowPolicy;
use txtcaster_lib::voice::tools::dispatcher::{
    CODE_AGENT_UNAVAILABLE, CODE_HIGHLIGHT_NOT_APPLIED, CODE_RATE_LIMITED, CODE_UNKNOWN_HANDLE,
};
use txtcaster_lib::voice::tools::playback::{PlaybackControl, PlaybackUnavailable};
use txtcaster_lib::voice::tools::{Dispatcher, ToolOutcome, ToolRegistry};
use txtcaster_lib::words::WordProvenance;

struct FakePlayback {
    position_ms: u64,
    seeks: Vec<u64>,
}

impl FakePlayback {
    fn new() -> Self {
        Self {
            position_ms: 0,
            seeks: Vec::new(),
        }
    }
}

impl PlaybackControl for FakePlayback {
    fn position_ms(&self) -> Result<u64, PlaybackUnavailable> {
        Ok(self.position_ms)
    }
    fn pause(&mut self) -> Result<(), PlaybackUnavailable> {
        Ok(())
    }
    fn resume(&mut self) -> Result<(), PlaybackUnavailable> {
        Ok(())
    }
    fn seek_to_ms(&mut self, pos_ms: u64) -> Result<(), PlaybackUnavailable> {
        self.position_ms = pos_ms;
        self.seeks.push(pos_ms);
        Ok(())
    }
    fn set_speed(&mut self, _speed: f64) -> Result<(), PlaybackUnavailable> {
        Ok(())
    }
}

/// Six-segment book: retrieval rows and the session snapshot agree on the
/// (idx, page) identity the resolver verifies. Segment texts carry unique
/// terms so single-variant searches land deterministically.
const TEXTS: &[(&str, u32)] = &[
    ("the astrolabe fixed the meridian altitude at dawn", 1),
    ("dead reckoning drifted badly around the cape currents", 1),
    ("the marine chronometer settled the longitude question", 1),
    ("portolan charts guided venetian merchant convoys", 2),
    (
        "generated overview velgorath summarizes navigation themes",
        2,
    ),
    ("the kamal measured polaris with a knotted cord", 2),
];

fn segment_rows() -> Vec<SegmentRow> {
    TEXTS
        .iter()
        .enumerate()
        .map(|(i, (text, page))| SegmentRow {
            chapter: 0,
            page: *page,
            idx: i as u32,
            text: (*text).into(),
            provenance: if i == 4 {
                Provenance::Generated
            } else {
                Provenance::SourceFaithful
            },
        })
        .collect()
}

fn snapshot() -> SessionSnapshot {
    let segments: Vec<ReadingSegment> = TEXTS
        .iter()
        .enumerate()
        .map(|(i, (text, page))| ReadingSegment {
            segment_id: format!("seg-{i}"),
            idx: i as i64,
            page_no: *page as i64,
            kind: "text".into(),
            figure_id: None,
            text: (*text).into(),
            start_ms: i as i64 * 1_000,
            end_ms: i as i64 * 1_000 + 900,
            provenance: WordProvenance::Missing,
            words: Vec::new(),
            anchor: txtcaster_lib::reading::anchor::Rung::Page,
            rects: Vec::new(),
        })
        .collect();
    let chapter = ReadingChapter {
        anchor_geometry: GeometryOutcome::Unavailable,
        chapter_id: "ch-0".into(),
        start_ms: 0,
        end_ms: 5_900,
        origin: TimelineOrigin::Measured,
        segments,
        mode: None,
    };
    SessionSnapshot::from_reading(&[chapter], Vec::new(), |_| None)
}

struct Rig {
    dispatcher: Dispatcher,
    playback: FakePlayback,
    excerpts: ExcerptSelector,
    now: Instant,
}

fn rig_with_highlight(dir: &std::path::Path, highlight: ScriptedHighlightPort) -> Rig {
    let provider = Arc::new(TantivyRetrieval::new(dir));
    let fingerprint = BookFingerprint::new("b1-book", "job-1", "hash-1", "blocks-1");
    provider
        .index_book(&fingerprint, &segment_rows())
        .expect("index");
    let adapter = RetrievalPortAdapter::new(provider, HandleTable::new(fingerprint));
    let mut dispatcher = Dispatcher::new(ToolRegistry::agent());
    dispatcher.set_agent_ports(Box::new(adapter), Box::new(highlight));
    Rig {
        dispatcher,
        playback: FakePlayback::new(),
        excerpts: ExcerptSelector::new(
            snapshot(),
            WindowPolicy {
                before: 2,
                after: 2,
            },
        ),
        now: Instant::now(),
    }
}

fn rig(dir: &std::path::Path) -> Rig {
    rig_with_highlight(dir, ScriptedHighlightPort::applying())
}

impl Rig {
    fn call(&mut self, name: &str, args: Value) -> ToolOutcome {
        self.dispatcher.dispatch(
            name,
            &args,
            self.now,
            &mut self.playback,
            &mut self.excerpts,
        )
    }

    /// One live handle for the segment whose text matches `term`.
    fn handle_for(&mut self, term: &str) -> String {
        let outcome = self.call("search_book", json!({ "queries": [term] }));
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("search failed for {term}: {outcome:?}")
        };
        payload["hits"][0]["handle"]
            .as_str()
            .expect("handle")
            .to_string()
    }
}

fn failure_code(outcome: &ToolOutcome) -> &str {
    match outcome {
        ToolOutcome::Failed { code, .. } => code,
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn the_two_registries_are_disjoint_where_the_charter_says_so() {
    let agent = ToolRegistry::agent();
    assert_eq!(
        agent.names(),
        vec![
            "pause_playback",
            "resume_playback",
            "jump_to_chapter",
            "jump_to_page",
            "search_book",
            "open_passage",
            "go_to_passage",
            "highlight_passage",
        ],
        "the Agent registry is exactly the charter §4 set"
    );
    // No Tutor-only vocabulary leaks into Agent mode.
    for tutor_only in [
        "seek_relative",
        "set_playback_speed",
        "replay_previous_paragraph",
        "replay_previous_page",
        "replay_last_minute",
        "summarize_section",
        "get_context_around_position",
        "update_learning_profile",
        "teacher_anchor",
    ] {
        assert!(
            agent.tier(tutor_only).is_none(),
            "{tutor_only} leaked into Agent mode"
        );
    }
    // No Agent vocabulary leaks into Tutor mode (either anchor flag state).
    for tutor in [ToolRegistry::new(false), ToolRegistry::new(true)] {
        for agent_tool in [
            "search_book",
            "open_passage",
            "go_to_passage",
            "highlight_passage",
        ] {
            assert!(
                tutor.tier(agent_tool).is_none(),
                "{agent_tool} leaked into Tutor mode"
            );
        }
    }
}

#[test]
fn tutor_dispatcher_answers_agent_tools_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    // A Tutor-registry dispatcher — even with agent ports somehow wired —
    // refuses the Agent names as unknown tools.
    r.dispatcher = {
        let mut d = Dispatcher::new(ToolRegistry::new(false));
        let provider = Arc::new(TantivyRetrieval::new(dir.path()));
        let fingerprint = BookFingerprint::new("b1-book", "job-1", "hash-1", "blocks-1");
        let adapter = RetrievalPortAdapter::new(provider, HandleTable::new(fingerprint));
        d.set_agent_ports(
            Box::new(adapter),
            Box::new(ScriptedHighlightPort::applying()),
        );
        d
    };
    for name in [
        "search_book",
        "open_passage",
        "go_to_passage",
        "highlight_passage",
    ] {
        assert!(matches!(
            r.call(name, json!({ "queries": ["x"], "handle": "h" })),
            ToolOutcome::UnknownTool
        ));
    }
}

#[test]
fn agent_dispatcher_answers_tutor_only_tools_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    for (name, args) in [
        ("summarize_section", json!({})),
        ("seek_relative", json!({ "seconds": 30 })),
        ("teacher_anchor", json!({ "kind": "figure", "id": "f1" })),
        (
            "update_learning_profile",
            json!({ "key": "pace", "value": "slow" }),
        ),
    ] {
        assert!(
            matches!(r.call(name, args), ToolOutcome::UnknownTool),
            "{name} must be refused in Agent mode"
        );
    }
}

#[test]
fn search_go_to_passage_seeks_the_exact_segment_start_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    let handle = r.handle_for("chronometer longitude");
    let outcome = r.call("go_to_passage", json!({ "handle": handle }));
    let ToolOutcome::Ok(payload) = outcome else {
        panic!("go_to_passage failed: {outcome:?}")
    };
    // Segment idx 2: start_ms 2000, page 1. Applied facts only, zero text.
    assert_eq!(
        payload,
        json!({ "position_ms": 2000, "page": 1, "chapter": 0 })
    );
    assert_eq!(r.playback.seeks, vec![2000], "exactly one absolute seek");
}

#[test]
fn search_payload_is_bounded_and_snippet_free_of_raw_coordinates() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    let outcome = r.call(
        "search_book",
        json!({ "queries": ["astrolabe meridian", "meridian altitude dawn"] }),
    );
    let ToolOutcome::Ok(payload) = outcome else {
        panic!("search failed: {outcome:?}")
    };
    let serialized = serde_json::to_string(&payload).unwrap();
    assert!(
        serialized.len() <= 900,
        "payload {}B over ceiling",
        serialized.len()
    );
    for hit in payload["hits"].as_array().unwrap() {
        assert!(hit.get("score").is_none());
        assert!(hit.get("idx").is_none());
        assert_eq!(hit["handle"].as_str().unwrap().len(), 32);
    }
}

#[test]
fn highlight_applied_ack_carries_status_only_and_the_port_gets_the_coord() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    let handle = r.handle_for("kamal polaris");
    let outcome = r.call(
        "highlight_passage",
        json!({ "handle": handle, "start_word": 1, "word_count": 3 }),
    );
    let ToolOutcome::Ok(payload) = outcome else {
        panic!("highlight failed: {outcome:?}")
    };
    assert_eq!(
        payload,
        json!({ "applied": true }),
        "ack/status only, zero book text"
    );
    assert_eq!(
        r.playback.seeks,
        Vec::<u64>::new(),
        "citation never moves playback"
    );
}

#[test]
fn a_refusing_surface_is_an_honest_non_applied_answer_and_costs_no_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig_with_highlight(dir.path(), ScriptedHighlightPort::refusing("no_geometry"));
    let handle = r.handle_for("kamal polaris");
    for _ in 0..8 {
        let outcome = r.call("highlight_passage", json!({ "handle": handle }));
        assert_eq!(failure_code(&outcome), CODE_HIGHLIGHT_NOT_APPLIED);
    }
    // Refusals spent no Tier-1 slots: a real mutation still goes through.
    let outcome = r.call("go_to_passage", json!({ "handle": handle }));
    assert!(
        matches!(outcome, ToolOutcome::Ok(_)),
        "budget wrongly consumed: {outcome:?}"
    );
}

#[test]
fn unknown_and_stale_handles_are_refused_never_remapped() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    for name in ["go_to_passage", "highlight_passage"] {
        let outcome = r.call(
            name,
            json!({ "handle": "deadbeefdeadbeefdeadbeefdeadbeef" }),
        );
        assert_eq!(failure_code(&outcome), CODE_UNKNOWN_HANDLE);
    }
    assert!(r.playback.seeks.is_empty());
}

#[test]
fn generated_prose_navigates_but_never_cites() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    let handle = r.handle_for("velgorath");
    // Navigation to a related (generated) passage is a plain seek…
    let outcome = r.call("go_to_passage", json!({ "handle": handle }));
    assert!(matches!(outcome, ToolOutcome::Ok(_)));
    // …but the citation law refuses it as evidence (T9).
    let outcome = r.call("highlight_passage", json!({ "handle": handle }));
    assert_eq!(failure_code(&outcome), CODE_HIGHLIGHT_NOT_APPLIED);
}

#[test]
fn unwired_agent_ports_answer_agent_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    r.dispatcher = Dispatcher::new(ToolRegistry::agent());
    let outcome = r.call("search_book", json!({ "queries": ["astrolabe"] }));
    assert_eq!(failure_code(&outcome), CODE_AGENT_UNAVAILABLE);
}

#[test]
fn tier_one_budget_is_shared_and_tier_zero_reads_are_free() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    let handle = r.handle_for("astrolabe meridian");
    // Tier-0 reads never touch the window.
    for _ in 0..12 {
        assert!(matches!(
            r.call("search_book", json!({ "queries": ["astrolabe"] })),
            ToolOutcome::Ok(_)
        ));
    }
    // Five successful Tier-1 mutations across reused AND new verbs fill it…
    for name_args in [
        ("pause_playback", json!({})),
        ("resume_playback", json!({})),
        ("jump_to_chapter", json!({ "chapter": 0 })),
        ("go_to_passage", json!({ "handle": handle })),
        ("highlight_passage", json!({ "handle": handle })),
    ] {
        let outcome = r.call(name_args.0, name_args.1);
        assert!(
            matches!(outcome, ToolOutcome::Ok(_)),
            "{}: {outcome:?}",
            name_args.0
        );
    }
    // …and the sixth is refused within the same window.
    let outcome = r.call("go_to_passage", json!({ "handle": handle }));
    assert_eq!(failure_code(&outcome), CODE_RATE_LIMITED);
    // The window slides: past it, mutations flow again.
    r.now += Duration::from_secs(11);
    let outcome = r.call("go_to_passage", json!({ "handle": handle }));
    assert!(matches!(outcome, ToolOutcome::Ok(_)));
}

#[test]
fn schema_violations_are_invalid_args_not_panics() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    for (name, args) in [
        ("search_book", json!({ "queries": [] })),
        (
            "search_book",
            json!({ "queries": ["a", "b", "c", "d", "e"] }),
        ),
        ("search_book", json!({ "queries": ["ok"], "extra": 1 })),
        ("search_book", json!({ "queries": [" "] })),
        ("open_passage", json!({})),
        ("go_to_passage", json!({ "handle": "" })),
        (
            "highlight_passage",
            json!({ "handle": "h", "start_word": 2 }),
        ),
        (
            "highlight_passage",
            json!({ "handle": "h", "word_count": 0 }),
        ),
        (
            "highlight_passage",
            json!({ "handle": "h", "start_word": 4_294_967_296u64, "word_count": 3 }),
        ),
        (
            "search_book",
            json!({ "queries": ["ok"], "chapter": 4_294_967_296u64 }),
        ),
    ] {
        assert!(
            matches!(r.call(name, args.clone()), ToolOutcome::InvalidArgs { .. }),
            "{name} {args} must be invalid_args"
        );
    }
}

#[test]
fn a_span_outside_the_passage_is_refused_never_clamped() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rig(dir.path());
    // "the kamal measured polaris with a knotted cord" = 8 words.
    let handle = r.handle_for("kamal polaris");
    let outcome = r.call(
        "highlight_passage",
        json!({ "handle": handle, "start_word": 6, "word_count": 5 }),
    );
    assert!(
        matches!(outcome, ToolOutcome::InvalidArgs { .. }),
        "out-of-passage span must be invalid_args, got {outcome:?}"
    );
    // An in-range span still applies.
    let outcome = r.call(
        "highlight_passage",
        json!({ "handle": handle, "start_word": 1, "word_count": 3 }),
    );
    assert!(matches!(outcome, ToolOutcome::Ok(_)));
}

#[test]
fn the_port_exposes_its_job_binding_for_the_wiring_invariant() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(TantivyRetrieval::new(dir.path()));
    let fingerprint = BookFingerprint::new("b1-book", "job-1", "hash-1", "blocks-1");
    let adapter = RetrievalPortAdapter::new(provider, HandleTable::new(fingerprint));
    use txtcaster_lib::voice::tools::agent::AgentRetrievalPort;
    assert_eq!(adapter.job_id(), "job-1");
}

#[test]
fn a_handle_that_disagrees_with_the_session_snapshot_is_refused() {
    // Index rows whose page disagrees with the snapshot's page for the same
    // idx: the resolver must refuse (wrong render) rather than best-effort
    // seek (T10).
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(TantivyRetrieval::new(dir.path()));
    let fingerprint = BookFingerprint::new("b1-book", "job-1", "hash-1", "blocks-1");
    let mut rows = segment_rows();
    for r in &mut rows {
        r.page += 40; // a different render's pagination
    }
    provider.index_book(&fingerprint, &rows).expect("index");
    let adapter = RetrievalPortAdapter::new(provider, HandleTable::new(fingerprint));
    let mut dispatcher = Dispatcher::new(ToolRegistry::agent());
    dispatcher.set_agent_ports(
        Box::new(adapter),
        Box::new(ScriptedHighlightPort::applying()),
    );
    let mut r = Rig {
        dispatcher,
        playback: FakePlayback::new(),
        excerpts: ExcerptSelector::new(
            snapshot(),
            WindowPolicy {
                before: 2,
                after: 2,
            },
        ),
        now: Instant::now(),
    };
    let handle = r.handle_for("astrolabe meridian");
    let outcome = r.call("go_to_passage", json!({ "handle": handle }));
    assert_eq!(failure_code(&outcome), CODE_UNKNOWN_HANDLE);
    assert!(
        r.playback.seeks.is_empty(),
        "no best-effort seek on a mismatched handle"
    );
}

#[test]
fn every_agent_schema_declares_non_blocking_and_no_tutor_schema_does() {
    for schema in ToolRegistry::agent().schemas() {
        assert_eq!(
            schema["function"]["behavior"].as_str(),
            Some("NON_BLOCKING"),
            "{} must declare NON_BLOCKING in Agent mode",
            schema["function"]["name"]
        );
    }
    for tutor in [ToolRegistry::new(false), ToolRegistry::new(true)] {
        for schema in tutor.schemas() {
            assert!(
                schema["function"].get("behavior").is_none(),
                "{} must stay blocking in Tutor mode",
                schema["function"]["name"]
            );
        }
    }
}
