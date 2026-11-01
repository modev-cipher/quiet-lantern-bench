//! Tool-call dispatch: `ToolCall` event → validate → execute → answer
//! (task VA6 in docs/TASKS.md — "Tier-0/1 tool set (pause/seek/replay/jump/
//! speed/summarize/get_context)"; VOICE_AGENT §5; ARCHITECTURE §8.1).
//!
//! This slice is the validation core, pure and channel-free: a tool call's
//! `(name, args)` is checked against the session's [`ToolRegistry`] and the
//! §5 strict schemas, producing either a typed [`ValidatedCall`] for the
//! execution layer or a [`ToolOutcome`] rejection. The checks here and the
//! JSON schemas in [`registry`] must agree — both are written against the
//! same named bound constants ([`registry::SEEK_RELATIVE_MIN_SECONDS`] and
//! friends), which is what keeps a provider that ignores schemas from
//! reaching execution with out-of-spec numbers.
//!
//! Two rejection shapes, mirroring §5's failure vocabulary:
//!
//! - [`ToolOutcome::UnknownTool`]: the name is not in this session's registry
//!   — hallucinated names and `teacher_anchor` on a session that never
//!   offered it land here identically ([`ToolRegistry::tier`] returns `None`
//!   for both, on purpose).
//! - [`ToolOutcome::InvalidArgs`]: the name is offered but the arguments
//!   break the schema — wrong JSON type, missing required field, unknown
//!   field (§5: deny-unknown-fields everywhere), or a numeric outside the
//!   spec bounds.
//!
//! `teacher_anchor` is the one offered tool whose argument shape is *not*
//! decided here: [`crate::voice::channel::anchor::normalize_teacher_anchor`]
//! owns that schema check (its `out_of_schema` ack) and the generation
//! gating, so the dispatcher only enforces the session gate and hands the raw
//! args through as [`ValidatedCall::TeacherAnchor`].

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

#[cfg(feature = "book-agent")]
use super::agent::{AgentHighlightPort, AgentRetrievalPort, HighlightStatus, PassageCoord};
use super::learning::LearningProfileSink;
use super::playback::{PlaybackControl, PlaybackUnavailable};
use super::registry::{self, ToolRegistry, ToolTier};
use crate::voice::channel::anchor::TEACHER_ANCHOR_TOOL;
use crate::voice::channel::types::ToolResult;
use crate::voice::channel::{ChannelError, SessionChannel};
use crate::voice::excerpt::coverage::{position_label, PositionUnits};
use crate::voice::excerpt::policy::{EgressEvent, TruncationPolicy};
use crate::voice::excerpt::selector::{ExcerptSelector, RangeFetch, SelectedExcerpt};
use crate::voice::excerpt::source::{SessionSegment, SessionSnapshot};

/// The dispatcher's answer for one tool call, before it is encoded as a
/// [`crate::voice::channel::types::ToolResult`] on the channel (the wiring
/// slice owns that mapping; the variants here are 1:1 with §5's outcome
/// vocabulary so nothing is lost in translation).
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    /// The tool executed; the payload is what the model gets back.
    Ok(Value),
    /// The name is not part of this session's offered vocabulary.
    UnknownTool,
    /// The name is offered but the arguments break its schema.
    InvalidArgs { detail: String },
    /// The call was well-formed but execution could not honor it — playback
    /// surface gone ([`PlaybackUnavailable`]), Tier-1 rate budget spent
    /// (ARCHITECTURE §8.1), a jump target the book does not have. Always
    /// answered, never panicked (VOICE_AGENT §5): the model gets the code and
    /// a message it can act on.
    Failed { code: String, message: String },
    /// Agent citation in flight (feature `book-agent`): the surface will ack
    /// after render and the session actor answers the provider then. The
    /// caller MUST register the pending call — nothing is sent yet, and the
    /// Tier-1 slot is committed only by an Applied ack.
    #[cfg(feature = "book-agent")]
    Deferred,
}

#[derive(Clone)]
pub struct PreparedNavigation {
    payload: Value,
    landing_text: bool,
    position: u64,
}

impl PreparedNavigation {
    pub fn position(&self) -> u64 {
        self.position
    }

    pub fn page(&self) -> Option<i64> {
        self.payload.get("page").and_then(Value::as_i64)
    }
}

struct NavigationPreview(u64);

impl PlaybackControl for NavigationPreview {
    fn position_ms(&self) -> Result<u64, PlaybackUnavailable> {
        Ok(self.0)
    }
    fn seek_to_ms(&mut self, pos: u64) -> Result<(), PlaybackUnavailable> {
        self.0 = pos;
        Ok(())
    }
    fn pause(&mut self) -> Result<(), PlaybackUnavailable> {
        Err(PlaybackUnavailable)
    }
    fn resume(&mut self) -> Result<(), PlaybackUnavailable> {
        Err(PlaybackUnavailable)
    }
    fn set_speed(&mut self, _: f64) -> Result<(), PlaybackUnavailable> {
        Err(PlaybackUnavailable)
    }
}

pub fn is_navigation_tool(name: &str) -> bool {
    matches!(
        name,
        registry::JUMP_TO_CHAPTER
            | registry::JUMP_TO_PAGE
            | registry::TURN_PAGE
            | registry::REPLAY_PREVIOUS_PARAGRAPH
            | registry::REPLAY_PREVIOUS_PAGE
            | registry::REPLAY_LAST_MINUTE
            | registry::SEEK_RELATIVE
    )
}

/// `code` values on the wire ([`ToolResult::Err`]) for the rejection variants.
pub const CODE_UNKNOWN_TOOL: &str = "unknown_tool";
pub const CODE_INVALID_ARGS: &str = "invalid_args";

/// `code` values for [`ToolOutcome::Failed`], fixed strings so the model (and
/// Role B's pin tests) can branch on them.
pub const CODE_PLAYBACK_UNAVAILABLE: &str = "playback_unavailable";
pub const CODE_RATE_LIMITED: &str = "rate_limited";
pub const CODE_UNKNOWN_CHAPTER: &str = "unknown_chapter";
pub const CODE_UNKNOWN_PAGE: &str = "unknown_page";
pub const CODE_NO_CONTEXT: &str = "no_context";
/// F10-D2: a BookDirect chapter jump whose target has no readable text AND no
/// known page span — nothing on screen could turn to it, so the jump refuses
/// instead of "succeeding" onto an offset no reader can show.
pub const CODE_UNRESOLVED_PAGE: &str = "unresolved_page";
/// F7 jobless-grounded: an audio-bound tool (pause/resume/speed/seek/replay
/// minute) refused because the book has no narration audio — honest and
/// terminal for this session, unlike the retryable
/// [`CODE_PLAYBACK_UNAVAILABLE`].
pub const CODE_NO_NARRATION: &str = "no_narration";
/// A tool the wiring layer owns reached the dispatcher's executor — a wiring
/// bug (`teacher_anchor` must be routed to `normalize_teacher_anchor` before
/// dispatch). Answered as an error rather than panicking mid-session.
pub const CODE_UNROUTED_TOOL: &str = "unrouted_tool";
pub const CODE_PROFILE_UNAVAILABLE: &str = "profile_unavailable";
pub const CODE_PROFILE_REFUSED: &str = "profile_refused";
/// Book Agent tool called before the session wiring installed the agent
/// ports (charter B-1) — refused loudly, never silently dropped.
#[cfg(feature = "book-agent")]
pub const CODE_AGENT_UNAVAILABLE: &str = "agent_unavailable";
/// An opaque passage handle that does not resolve in this session/generation.
/// Refused, never best-effort remapped (charter T10).
#[cfg(feature = "book-agent")]
pub const CODE_UNKNOWN_HANDLE: &str = "unknown_handle";
/// The reading surface did not apply the citation (absent surface/geometry,
/// disabled, stale). Honest non-applied status (charter §4.5); costs no
/// Tier-1 budget because nothing mutated.
#[cfg(feature = "book-agent")]
pub const CODE_HIGHLIGHT_NOT_APPLIED: &str = "highlight_not_applied";

impl ToolOutcome {
    fn invalid(detail: impl Into<String>) -> Self {
        ToolOutcome::InvalidArgs {
            detail: detail.into(),
        }
    }

    fn failed(code: &str, message: impl Into<String>) -> Self {
        ToolOutcome::Failed {
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// The wire encoding (VOICE_AGENT §3/§5): every outcome becomes exactly
    /// one [`ToolResult`], so every `ChannelEvent::ToolCall` gets exactly one
    /// answer and the tool protocol can never stall on a rejection.
    pub fn into_tool_result(self) -> ToolResult {
        match self {
            ToolOutcome::Ok(payload) => ToolResult::Ok(payload),
            ToolOutcome::UnknownTool => ToolResult::Err {
                code: CODE_UNKNOWN_TOOL.to_string(),
                message: "this tool is not offered in this session".to_string(),
            },
            ToolOutcome::InvalidArgs { detail } => ToolResult::Err {
                code: CODE_INVALID_ARGS.to_string(),
                message: detail,
            },
            ToolOutcome::Failed { code, message } => ToolResult::Err { code, message },
            #[cfg(feature = "book-agent")]
            ToolOutcome::Deferred => unreachable!(
                "a deferred citation is answered by the ack path, never serialized directly"
            ),
        }
    }
}

/// One `jump_to_chapter`/`jump_to_page` outcome, recorded for the session
/// layer (field dossier 2026-08-31, D3): tool results used to be
/// provider-facing only, so a jump that failed — or was never called — was
/// indistinguishable on screen from one that worked, and the model's
/// narration was the sole witness of navigation. The dispatcher now records
/// every jump outcome in a drainable buffer (the egress-event shape); the
/// session runtime drains it after each tool call and surfaces failures to
/// the user. Refused `teacher_anchor` calls ride the same buffer (F2).
/// 4b (owner ruling 2026-09-28): one ANSWERED tool call, journaled for
/// complete tool-execution observability — the post-hoc evidence the
/// 2026-08-31/09-28 field investigations lacked (did the model call a
/// navigation tool at all, and what did it answer?).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcomeRecord {
    pub tool: String,
    pub call_id: String,
    pub ok: bool,
    pub code: Option<String>,
    pub pos_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NavigationOutcome {
    /// The tool name (`jump_to_chapter` / `jump_to_page`).
    pub tool: String,
    /// `Ok` carries the reached book-timeline position; `Err` the refusal's
    /// `(code, message)` exactly as the model heard it.
    pub result: Result<u64, (String, String)>,
}

/// The session shape the dispatcher words its refusals for (F7: the F6
/// boolean grew a third kind). Wording ONLY — codes, tiers, and outcomes are
/// identical across modes, except the audio-bound refusals BookDirect adds
/// (a jobless book session has no narration timeline to pause or seek).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DispatchMode {
    /// Book + narration job: the pre-F6 wording, byte-for-byte.
    #[default]
    Grounded,
    /// Book attached, NO narration job (F7 jobless-grounded): book text and
    /// navigation are live over the character-offset snapshot; audio-bound
    /// tools refuse with "no narration yet", never "the book has 0 chapters".
    BookDirect,
    /// No book attached (F6): refusals say so.
    Standalone,
}

impl From<PlaybackUnavailable> for ToolOutcome {
    /// §5 "errors answered, never panicked": a torn-down playing surface is a
    /// retryable tool error the model hears about, not a session fault.
    fn from(err: PlaybackUnavailable) -> Self {
        ToolOutcome::failed(
            CODE_PLAYBACK_UNAVAILABLE,
            format!("{err}; retry once a book is open and playing"),
        )
    }
}

/// A tool call that passed name + schema validation: arguments decoded into
/// spec-checked Rust types, ready for the execution layer. No `serde_json`
/// re-inspection happens past this point (except `TeacherAnchor`, whose
/// schema authority is the anchor normalizer — see the module header).
#[derive(Debug, Clone, PartialEq)]
pub enum ValidatedCall {
    Pause,
    Resume,
    /// Signed offset, already within §5's ±600 s.
    SeekRelative {
        seconds: f64,
    },
    /// Multiplier, already within §5's 0.5..=3.0.
    SetSpeed {
        speed: f64,
    },
    ReplayPreviousParagraph,
    /// `replay_previous_page`. "Printed" rather than the obvious noun: the
    /// egress scan (`voice::excerpt::scan`) forbids the page-text surface's
    /// CamelCase type name in any voice module outside the excerpt tree.
    ReplayPreviousPrinted,
    ReplayLastMinute,
    /// 0-based chapter index in reading order.
    JumpToChapter {
        chapter: u64,
    },
    /// Render-cache page numbers, independent of printed footer labels.
    TurnPrinted {
        forward: bool,
    },
    JumpToPrinted {
        page: u64,
    },
    SummarizeSection,
    /// `None` means "around the current playback position".
    GetContextAroundPosition {
        position_ms: Option<u64>,
    },
    /// `get_context_for_range` (F8-D1b): an explicit half-open `[start, end)`
    /// range in the session's position space, or `None` for the played-range
    /// selector (the span actually played/read so far). `end > start` is
    /// already validated.
    GetContextForRange {
        range: Option<(u64, u64)>,
    },
    /// `update_learning_profile` (owner ruling R1): both fields already
    /// within the registry's char bounds.
    UpdateLearningProfile {
        key: String,
        value: String,
    },
    /// Raw args, validated only for session gating here; shape validation
    /// belongs to `normalize_teacher_anchor` (VOICE_AGENT §11.1).
    TeacherAnchor {
        args: Value,
    },
    Unrouted(&'static str),
    /// `search_book` (charter §4.2): 1-4 nonblank capped variants, optional
    /// chapter scope. Deep validation (dedupe, index state) belongs to the
    /// retrieval port.
    #[cfg(feature = "book-agent")]
    SearchBook {
        queries: Vec<String>,
        chapter: Option<u64>,
    },
    /// `open_passage`: one live opaque handle token.
    #[cfg(feature = "book-agent")]
    OpenPassage {
        handle: String,
    },
    /// `go_to_passage`: one live opaque handle token.
    #[cfg(feature = "book-agent")]
    GoToPassage {
        handle: String,
    },
    /// `highlight_passage`: handle plus optional (start_word, word_count).
    #[cfg(feature = "book-agent")]
    HighlightPassage {
        handle: String,
        span: Option<(u64, u64)>,
    },
}

/// Validate one tool call against the session's registry and the §5 schemas.
///
/// Pure: no playback, no channel, no book text. `Err` carries the exact
/// [`ToolOutcome`] to answer with; `Ok` carries the typed call for execution.
pub fn validate(
    registry: &ToolRegistry,
    name: &str,
    args: &Value,
) -> Result<ValidatedCall, ToolOutcome> {
    if registry.tier(name).is_none() {
        return Err(ToolOutcome::UnknownTool);
    }
    match name {
        registry::PAUSE_PLAYBACK => no_args(args, ValidatedCall::Pause),
        registry::RESUME_PLAYBACK => no_args(args, ValidatedCall::Resume),
        registry::REPLAY_PREVIOUS_PARAGRAPH => {
            no_args(args, ValidatedCall::ReplayPreviousParagraph)
        }
        registry::REPLAY_PREVIOUS_PAGE => no_args(args, ValidatedCall::ReplayPreviousPrinted),
        registry::REPLAY_LAST_MINUTE => no_args(args, ValidatedCall::ReplayLastMinute),
        registry::SUMMARIZE_SECTION => no_args(args, ValidatedCall::SummarizeSection),
        registry::SEEK_RELATIVE => {
            let fields = object_with(args, &["seconds"])?;
            let seconds = required_number(fields, "seconds")?;
            if !(registry::SEEK_RELATIVE_MIN_SECONDS..=registry::SEEK_RELATIVE_MAX_SECONDS)
                .contains(&seconds)
            {
                return Err(ToolOutcome::invalid(format!(
                    "`seconds` must be within [{}, {}], got {seconds}",
                    registry::SEEK_RELATIVE_MIN_SECONDS,
                    registry::SEEK_RELATIVE_MAX_SECONDS,
                )));
            }
            Ok(ValidatedCall::SeekRelative { seconds })
        }
        registry::SET_PLAYBACK_SPEED => {
            let fields = object_with(args, &["speed"])?;
            let speed = required_number(fields, "speed")?;
            if !(registry::SPEED_MIN..=registry::SPEED_MAX).contains(&speed) {
                return Err(ToolOutcome::invalid(format!(
                    "`speed` must be within [{}, {}], got {speed}",
                    registry::SPEED_MIN,
                    registry::SPEED_MAX,
                )));
            }
            Ok(ValidatedCall::SetSpeed { speed })
        }
        registry::JUMP_TO_CHAPTER => {
            let fields = object_with(args, &["chapter_index", "chapter"])?;
            let chapter = match optional_unsigned(fields, "chapter_index")? {
                Some(idx) => idx,
                None => required_unsigned(fields, "chapter")?,
            };
            Ok(ValidatedCall::JumpToChapter { chapter })
        }
        registry::JUMP_TO_PAGE => {
            let fields = object_with(args, &["page"])?;
            let page = required_unsigned(fields, "page")?;
            Ok(ValidatedCall::JumpToPrinted { page })
        }
        registry::TURN_PAGE => {
            let fields = object_with(args, &["direction"])?;
            match fields.get("direction").and_then(Value::as_str) {
                Some("next") => Ok(ValidatedCall::TurnPrinted { forward: true }),
                Some("previous") => Ok(ValidatedCall::TurnPrinted { forward: false }),
                _ => Err(ToolOutcome::invalid(
                    "`direction` must be \"next\" or \"previous\"",
                )),
            }
        }
        registry::GET_CONTEXT_AROUND_POSITION => {
            let fields = object_with(args, &["position_ms"])?;
            let position_ms = optional_unsigned(fields, "position_ms")?;
            Ok(ValidatedCall::GetContextAroundPosition { position_ms })
        }
        registry::GET_CONTEXT_FOR_RANGE => {
            let fields = object_with(args, &["start_position", "end_position"])?;
            let start = optional_unsigned(fields, "start_position")?;
            let end = optional_unsigned(fields, "end_position")?;
            let range = match (start, end) {
                (None, None) => None,
                (Some(start), Some(end)) => {
                    if end <= start {
                        return Err(ToolOutcome::invalid(format!(
                            "`end_position` must be greater than `start_position`, \
                             got {start}..{end}"
                        )));
                    }
                    Some((start, end))
                }
                _ => {
                    return Err(ToolOutcome::invalid(
                        "pass BOTH `start_position` and `end_position`, or neither \
                         to fetch the range played/read so far",
                    ))
                }
            };
            Ok(ValidatedCall::GetContextForRange { range })
        }
        registry::UPDATE_LEARNING_PROFILE => {
            let fields = object_with(args, &["key", "value"])?;
            let key = required_bounded_string(fields, "key", registry::PROFILE_KEY_MAX_CHARS)?;
            let value =
                required_bounded_string(fields, "value", registry::PROFILE_VALUE_MAX_CHARS)?;
            Ok(ValidatedCall::UpdateLearningProfile { key, value })
        }
        TEACHER_ANCHOR_TOOL => Ok(ValidatedCall::TeacherAnchor { args: args.clone() }),
        #[cfg(feature = "book-agent")]
        registry::SEARCH_BOOK => {
            let fields = object_with(args, &["queries", "chapter"])?;
            let Some(raw) = fields.get("queries").and_then(Value::as_array) else {
                return Err(ToolOutcome::invalid(
                    "`queries` must be an array of strings",
                ));
            };
            if raw.is_empty() || raw.len() > registry::SEARCH_MAX_VARIANTS {
                return Err(ToolOutcome::invalid(format!(
                    "`queries` must hold 1-{} variants, got {}",
                    registry::SEARCH_MAX_VARIANTS,
                    raw.len()
                )));
            }
            let mut queries = Vec::with_capacity(raw.len());
            for entry in raw {
                let Some(text) = entry.as_str() else {
                    return Err(ToolOutcome::invalid("every query variant must be a string"));
                };
                if text.trim().is_empty() {
                    return Err(ToolOutcome::invalid("query variants must be nonblank"));
                }
                if text.chars().count() > registry::SEARCH_MAX_QUERY_CHARS {
                    return Err(ToolOutcome::invalid(format!(
                        "query variants are capped at {} chars",
                        registry::SEARCH_MAX_QUERY_CHARS
                    )));
                }
                queries.push(text.to_string());
            }
            let chapter = optional_unsigned(fields, "chapter")?;
            if chapter.is_some_and(|c| c > u64::from(u32::MAX)) {
                return Err(ToolOutcome::invalid("`chapter` is out of range"));
            }
            Ok(ValidatedCall::SearchBook { queries, chapter })
        }
        #[cfg(feature = "book-agent")]
        registry::OPEN_PASSAGE => {
            let fields = object_with(args, &["handle"])?;
            let handle = required_bounded_string(fields, "handle", registry::HANDLE_MAX_CHARS)?;
            Ok(ValidatedCall::OpenPassage { handle })
        }
        #[cfg(feature = "book-agent")]
        registry::GO_TO_PASSAGE => {
            let fields = object_with(args, &["handle"])?;
            let handle = required_bounded_string(fields, "handle", registry::HANDLE_MAX_CHARS)?;
            Ok(ValidatedCall::GoToPassage { handle })
        }
        #[cfg(feature = "book-agent")]
        registry::HIGHLIGHT_PASSAGE => {
            let fields = object_with(args, &["handle", "start_word", "word_count"])?;
            let handle = required_bounded_string(fields, "handle", registry::HANDLE_MAX_CHARS)?;
            let span = match (
                optional_unsigned(fields, "start_word")?,
                optional_unsigned(fields, "word_count")?,
            ) {
                (None, None) => None,
                (Some(start), Some(count)) if count >= 1 => {
                    // Strict-input law: unrepresentable values are refused,
                    // never saturated into a different span.
                    if start > u64::from(u32::MAX) || count > u64::from(u32::MAX) {
                        return Err(ToolOutcome::invalid("cited span is out of range"));
                    }
                    Some((start, count))
                }
                _ => {
                    return Err(ToolOutcome::invalid(
                        "a cited span requires both `start_word` and a `word_count` >= 1",
                    ))
                }
            };
            Ok(ValidatedCall::HighlightPassage { handle, span })
        }
        registry::LOOK_AT_PAGE => Ok(ValidatedCall::Unrouted(registry::LOOK_AT_PAGE)),
        other => unreachable!("registry offered a tool the dispatcher does not know: {other}"),
    }
}

/// Tier-1 rate budget (ARCHITECTURE §8.1: "~5 calls / 10 s"): sliding window,
/// counted only against calls whose **mutation succeeded** (the H3 decision,
/// documented at the commit site in [`Dispatcher::dispatch`]) — a rejected
/// name, a schema break, a torn-down surface, and a missing jump target all
/// cost no budget, and Tier-0 reads are never limited.
pub const TIER_ONE_MAX_CALLS: usize = 5;
pub const TIER_ONE_WINDOW: Duration = Duration::from_secs(10);

/// How many unplayed-gap labels the range fetch names verbatim before
/// capping with an "and N more" tail (2026-09-24 context diet).
/// OWNER-TUNABLE. Failure direction if too low: the model must re-fetch to
/// see the middle gaps of a huge range; if too high: a whole-book recap
/// payload carries a long gap list into server-side history again.
const UNPLAYED_GAPS_VERBATIM_MAX: usize = 8;

/// Per-session tool dispatch: validation ([`validate`]) plus execution against
/// the session's playback port and excerpt selector. Owns the only mutable
/// dispatch state, the Tier-1 rate window; playback and excerpts stay with the
/// session harness and are borrowed per call, so the dispatcher imposes no
/// ownership shape on the wiring layer.
///
/// `now` is a parameter rather than an `Instant::now()` inside so the rate
/// window is pinned by deterministic tests; the wiring layer passes the real
/// clock.
pub struct Dispatcher {
    registry: ToolRegistry,
    tier_one_calls: VecDeque<Instant>,
    shown_page: Option<i64>,
    saved_preferences: Vec<(String, String)>,
    /// Computed-but-unacknowledged answers, keyed by `call_id` (H1): dispatch
    /// and send are decoupled so a failed send never loses the outcome — and a
    /// retry re-*sends* instead of re-*executing* (a second `seek_relative`
    /// execution would move the surface twice for one model call). Entries are
    /// settled (removed) on send success, so the cache holds only in-flight
    /// answers and a later provider event that happens to reuse a settled id
    /// still executes fresh (pin3: the harness echoes ids, it does not dedupe
    /// distinct provider events).
    pending_answers: VecDeque<(String, ToolResult)>,
    /// The provider's tokenizer-measured excerpt budget (VOICE_AGENT §4 — H2).
    /// Ships with the channel config; the session wiring installs it via
    /// [`Dispatcher::set_truncation_policy`] before the first tool call. While
    /// absent (test rigs, an unwired session) Tier-0 reads fall back to the
    /// 64 KiB hard byte backstop alone — the backstop is never the working cap
    /// on a correctly wired session.
    truncation: Option<TruncationPolicy>,
    /// The session shape (F6 boolean → F7 three-kind, [`DispatchMode`]).
    /// Installed by the session wiring like the truncation policy. Grounded
    /// and Standalone change only the WORDING of empty-snapshot refusals;
    /// BookDirect additionally refuses the audio-bound tools typed
    /// ([`CODE_NO_NARRATION`]) — a jobless book session has no narration
    /// timeline for them to act on.
    mode: DispatchMode,
    /// Egress loss the budgeted path reported (`ExcerptTruncated`,
    /// `HardByteCapApplied`), buffered for the session event stream to drain
    /// ([`Dispatcher::drain_egress_events`]; surfaced by the Phase 5 UI task).
    /// The same facts also ride the tool payload (`truncation`/`hard_cap`
    /// riders), so this buffer is observability, not the loudness guarantee.
    egress_events: VecDeque<EgressEvent>,
    /// The learning-profile sink (owner ruling R1), installed by the session
    /// wiring like the truncation policy. `None` (test rigs, an unwired
    /// session) answers profile calls with [`CODE_PROFILE_UNAVAILABLE`] —
    /// refused loudly, never silently dropped.
    profile: Option<Box<dyn LearningProfileSink>>,
    /// Book Agent retrieval port (charter B-1), installed like the profile
    /// sink by the (future) Agent session wiring; fixture harnesses install
    /// the retrieval-crate adapter. Absent => agent tools answer
    /// [`CODE_AGENT_UNAVAILABLE`].
    #[cfg(feature = "book-agent")]
    agent_retrieval: Option<Box<dyn AgentRetrievalPort>>,
    /// Book Agent citation port (charter §4.5), same wiring shape.
    #[cfg(feature = "book-agent")]
    agent_highlight: Option<Box<dyn AgentHighlightPort>>,
    /// Navigation-tool outcomes buffered for the session runtime
    /// ([`NavigationOutcome`]; drained by
    /// [`Dispatcher::drain_navigation_outcomes`]).
    nav_outcomes: VecDeque<NavigationOutcome>,
    /// 4b: every answered tool call, buffered for the runtime's journal
    /// write (drained beside the navigation outcomes; the journal is the
    /// sole writer of `voice_tool_outcome` rows).
    tool_outcomes: VecDeque<ToolOutcomeRecord>,
}

/// Upper bound on unacknowledged answers held per session. In-flight answers
/// are one-per-outstanding-tool-call, so anything near this bound means the
/// channel has been failing sends for a long stretch; the oldest entry is
/// evicted first (its retry would re-execute — by then the session is renewal
/// or teardown territory, and an unbounded map keyed by provider-supplied ids
/// would be a memory leak the model could drive).
const PENDING_ANSWER_CAP: usize = 32;

/// Upper bound on buffered egress events. Truncation fires at most twice per
/// Tier-0 read, and every notice also rides its payload, so a full buffer only
/// means nobody is draining — the oldest observability record is the right one
/// to shed.
const EGRESS_EVENT_CAP: usize = 64;

impl Dispatcher {
    pub fn new(registry: ToolRegistry) -> Self {
        Self {
            registry,
            tier_one_calls: VecDeque::new(),
            shown_page: None,
            saved_preferences: Vec::new(),
            pending_answers: VecDeque::new(),
            truncation: None,
            mode: DispatchMode::default(),
            egress_events: VecDeque::new(),
            profile: None,
            #[cfg(feature = "book-agent")]
            agent_retrieval: None,
            #[cfg(feature = "book-agent")]
            agent_highlight: None,
            nav_outcomes: VecDeque::new(),
            tool_outcomes: VecDeque::new(),
        }
    }

    pub fn saved_preferences(&self) -> &[(String, String)] {
        &self.saved_preferences
    }

    pub fn set_shown_page(&mut self, page: Option<i64>) {
        self.shown_page = page;
    }

    pub fn shown_page(&self) -> Option<i64> {
        self.shown_page
    }

    pub fn prepare_navigation(
        &mut self,
        name: &str,
        args: &Value,
        now: Instant,
        playback: &dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
    ) -> Result<PreparedNavigation, ToolOutcome> {
        let call = validate(&self.registry, name, args)?;
        if !is_navigation_tool(name) {
            return Err(ToolOutcome::UnknownTool);
        }
        if !self.tier_one_slot_free(now) {
            return Err(ToolOutcome::failed(
                CODE_RATE_LIMITED,
                "navigation budget exhausted; retry shortly",
            ));
        }
        let mut preview = NavigationPreview(playback.position_ms()?);
        let call = match call {
            ValidatedCall::TurnPrinted { forward } => ValidatedCall::JumpToPrinted {
                page: turn_page_target(self.shown_page, forward, playback, excerpts)?,
            },
            other => other,
        };
        let landing_text = matches!(
            call,
            ValidatedCall::JumpToChapter { .. } | ValidatedCall::JumpToPrinted { .. }
        );
        let payload = execute(
            call,
            &mut preview,
            excerpts,
            None,
            None,
            &mut |_| {},
            self.mode,
            false,
        )?;
        Ok(PreparedNavigation {
            position: preview.0,
            payload,
            landing_text,
        })
    }

    pub fn commit_navigation(
        &mut self,
        prepared: PreparedNavigation,
        now: Instant,
        playback: &mut dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
    ) -> ToolOutcome {
        let outcome = self.resume_navigation(prepared, playback, excerpts);
        if matches!(outcome, ToolOutcome::Ok(_)) {
            self.tier_one_calls.push_back(now);
        }
        outcome
    }

    pub fn resume_navigation(
        &mut self,
        prepared: PreparedNavigation,
        playback: &mut dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
    ) -> ToolOutcome {
        if let Err(error) = playback.seek_to_ms(prepared.position) {
            return error.into();
        }
        let mut payload = prepared.payload;
        payload["applied"] = json!(true);
        payload["status"] = json!("The reader confirmed displaying this document page. For PDFs these are physical PDF pages, not printed book labels.");
        if prepared.landing_text {
            attach_landing_text(
                &mut payload,
                prepared.position,
                playback,
                excerpts,
                self.truncation.as_ref(),
                &mut |event| {
                    if self.egress_events.len() == EGRESS_EVENT_CAP {
                        self.egress_events.pop_front();
                    }
                    self.egress_events.push_back(event);
                },
                self.mode,
            );
        }
        ToolOutcome::Ok(payload)
    }

    /// Install the Book Agent ports (charter B-1), the same wiring shape as
    /// the truncation policy and profile sink: once, before events flow.
    #[cfg(feature = "book-agent")]
    pub fn set_agent_ports(
        &mut self,
        retrieval: Box<dyn AgentRetrievalPort>,
        highlight: Box<dyn AgentHighlightPort>,
    ) {
        self.agent_retrieval = Some(retrieval);
        self.agent_highlight = Some(highlight);
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Install the session provider's excerpt truncation policy (H2). Called
    /// once by the session wiring, from the channel config, before events
    /// flow; Tier-0 reads from then on egress through the budgeted selector
    /// paths with the 64 KiB byte cap left as the final backstop.
    pub fn set_truncation_policy(&mut self, policy: TruncationPolicy) {
        self.truncation = Some(policy);
    }

    /// Install the session shape (F7: the F6 `set_standalone` boolean grown
    /// to the three-kind [`DispatchMode`]) — once, by the session wiring,
    /// before events flow.
    pub fn set_mode(&mut self, mode: DispatchMode) {
        self.mode = mode;
    }

    /// The installed session shape (F8 D2: the runtime's manual-turn
    /// handler applies BookCursor sync only to BookDirect sessions).
    pub fn mode(&self) -> DispatchMode {
        self.mode
    }

    /// Install the session's learning-profile sink (owner ruling R1), the
    /// same wiring shape as the truncation policy: once, by the session
    /// constructor, before events flow. While absent, a profile call is
    /// answered with a loud refusal — never a silent drop.
    pub fn set_profile_sink(&mut self, sink: Box<dyn LearningProfileSink>) {
        self.profile = Some(sink);
    }

    /// Drain the buffered egress-loss events, oldest first (the session event
    /// stream's read side).
    pub fn drain_egress_events(&mut self) -> Vec<EgressEvent> {
        self.egress_events.drain(..).collect()
    }

    /// Drain the buffered navigation outcomes, oldest first (dossier D3: the
    /// session runtime reads these after every tool call and surfaces the
    /// failures — a silent jump refusal is the fake-navigation defect).
    pub fn drain_navigation_outcomes(&mut self) -> Vec<NavigationOutcome> {
        self.nav_outcomes.drain(..).collect()
    }

    /// 4b: drain the buffered tool-outcome records, oldest first — the
    /// runtime journals one `voice_tool_outcome` row per entry.
    pub fn drain_tool_outcomes(&mut self) -> Vec<ToolOutcomeRecord> {
        self.tool_outcomes.drain(..).collect()
    }

    /// 4b: direct record for the pump-executed calls that do not flow
    /// through [`Dispatcher::dispatch`] — the `teacher_anchor` decision and
    /// the sight still. Same buffer, same cap, same journal drain.
    pub fn journal_record(&mut self, record: ToolOutcomeRecord) {
        if self.tool_outcomes.len() == EGRESS_EVENT_CAP {
            self.tool_outcomes.pop_front();
        }
        self.tool_outcomes.push_back(record);
    }

    /// 4b: record EVERY answered call (unconditional — the navigation
    /// filter above stays UI-only). Unknown tools record too: a
    /// hallucinated name is exactly the evidence a post-hoc investigation
    /// needs.
    fn record_tool_outcome(&mut self, tool: &str, call_id: &str, outcome: &ToolOutcome) {
        let (ok, code, pos_ms) = match outcome {
            ToolOutcome::Ok(payload) => (
                true,
                None,
                payload.get("position_ms").and_then(Value::as_u64),
            ),
            ToolOutcome::UnknownTool => (false, Some(CODE_UNKNOWN_TOOL.to_string()), None),
            ToolOutcome::InvalidArgs { .. } => (false, Some(CODE_INVALID_ARGS.to_string()), None),
            ToolOutcome::Failed { code, .. } => (false, Some(code.clone()), None),
            #[cfg(feature = "book-agent")]
            ToolOutcome::Deferred => (true, None, None),
        };
        if self.tool_outcomes.len() == EGRESS_EVENT_CAP {
            self.tool_outcomes.pop_front();
        }
        self.tool_outcomes.push_back(ToolOutcomeRecord {
            tool: tool.to_string(),
            call_id: call_id.to_string(),
            ok,
            code,
            pos_ms,
        });
    }

    /// Record a refused `teacher_anchor` on the same drainable buffer as the
    /// jump outcomes (dossier 2026-08-31, F2): the anchor is the only tool
    /// that moves the visible page, so a refusal acked to the model alone
    /// recreates the fake-navigation defect through the presentation path.
    pub fn record_anchor_refusal(&mut self, code: &str, message: &str) {
        self.record_navigation(
            crate::voice::channel::anchor::TEACHER_ANCHOR_TOOL,
            &ToolOutcome::failed(code, message),
        );
    }

    fn record_navigation(&mut self, tool: &str, outcome: &ToolOutcome) {
        let result = match outcome {
            ToolOutcome::Ok(payload) => Ok(payload
                .get("position_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default()),
            ToolOutcome::UnknownTool => Err((
                CODE_UNKNOWN_TOOL.to_string(),
                "this tool is not offered in this session".to_string(),
            )),
            ToolOutcome::InvalidArgs { detail } => {
                Err((CODE_INVALID_ARGS.to_string(), detail.clone()))
            }
            ToolOutcome::Failed { code, message } => Err((code.clone(), message.clone())),
            #[cfg(feature = "book-agent")]
            ToolOutcome::Deferred => return,
        };
        if self.nav_outcomes.len() == EGRESS_EVENT_CAP {
            self.nav_outcomes.pop_front();
        }
        self.nav_outcomes.push_back(NavigationOutcome {
            tool: tool.to_string(),
            result,
        });
    }

    /// The cached answer for `call_id`, if its send has not succeeded yet.
    /// pub(crate): the pump's sight path consults it so a RETRIED
    /// `look_at_page` re-sends only its cached ack, never a second still.
    pub(crate) fn pending_answer(&self, call_id: &str) -> Option<ToolResult> {
        self.pending_answers
            .iter()
            .find(|(id, _)| id == call_id)
            .map(|(_, result)| result.clone())
    }

    fn remember_answer(&mut self, call_id: &str, result: ToolResult) {
        if self.pending_answers.len() == PENDING_ANSWER_CAP {
            self.pending_answers.pop_front();
        }
        self.pending_answers
            .push_back((call_id.to_string(), result));
    }

    fn settle_answer(&mut self, call_id: &str) {
        if let Some(at) = self
            .pending_answers
            .iter()
            .position(|(id, _)| id == call_id)
        {
            self.pending_answers.remove(at);
        }
    }

    /// Drop every cached, unacknowledged answer: the channel that owed them
    /// is gone or never renews, so no socket will ever take them. Resending
    /// them to a different channel would name call ids it never issued.
    pub(crate) fn forget_pending_answers(&mut self) {
        self.pending_answers.clear();
    }

    #[cfg(test)]
    fn pending_answer_count(&self) -> usize {
        self.pending_answers.len()
    }

    /// Answer one tool call. Every path returns a [`ToolOutcome`] — nothing
    /// here panics on model-supplied input (VOICE_AGENT §5).
    ///
    /// `teacher_anchor` is deliberately not executable here: the wiring layer
    /// routes it to `normalize_teacher_anchor` (which owns its schema check,
    /// ack shape, and generation gating) *before* calling this. If one slips
    /// through anyway that is a wiring bug, answered as
    /// [`CODE_UNROUTED_TOOL`].
    pub fn dispatch(
        &mut self,
        name: &str,
        args: &Value,
        now: Instant,
        playback: &mut dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
    ) -> ToolOutcome {
        self.dispatch_with_call_id(name, args, "", now, playback, excerpts)
    }

    /// [`Dispatcher::dispatch`] with the provider call id, which the Agent
    /// citation path forwards to the surface so the post-render ack can
    /// answer the right call. Non-agent tools ignore it.
    pub fn dispatch_with_call_id(
        &mut self,
        name: &str,
        args: &Value,
        call_id: &str,
        now: Instant,
        playback: &mut dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
    ) -> ToolOutcome {
        let outcome = self.dispatch_unrecorded(name, args, call_id, now, playback, excerpts);
        // 4b: journal-grade observability — every answered call, before the
        // UI-only navigation filter below.
        self.record_tool_outcome(name, call_id, &outcome);
        // F8 §2.4 D4: every viewport-moving motion is recorded — the jumps
        // (as before, dossier 2026-08-31 D3) plus the replay motions, whose
        // successes now become user-visible Navigation events. An
        // UnknownTool refusal is recorded regardless of name: a hallucinated
        // `open_book` must surface on screen, never stay provider-only.
        if matches!(
            name,
            registry::JUMP_TO_CHAPTER
                | registry::JUMP_TO_PAGE
                | registry::TURN_PAGE
                | registry::REPLAY_PREVIOUS_PARAGRAPH
                | registry::REPLAY_PREVIOUS_PAGE
                | registry::REPLAY_LAST_MINUTE
                | registry::SEEK_RELATIVE
        ) || matches!(outcome, ToolOutcome::UnknownTool)
        {
            self.record_navigation(name, &outcome);
        }
        outcome
    }

    /// [`Dispatcher::dispatch_with_call_id`] minus the navigation-outcome
    /// tap — every early return below stays on the recorded path through the
    /// wrapper above.
    fn dispatch_unrecorded(
        &mut self,
        name: &str,
        args: &Value,
        call_id: &str,
        now: Instant,
        playback: &mut dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
    ) -> ToolOutcome {
        #[cfg(not(feature = "book-agent"))]
        let _ = call_id;
        let call = match validate(&self.registry, name, args) {
            Ok(call) => call,
            Err(outcome) => return outcome,
        };
        if matches!(call, ValidatedCall::TeacherAnchor { .. }) {
            return ToolOutcome::failed(
                CODE_UNROUTED_TOOL,
                "teacher_anchor is handled by the presentation-sync path, not the dispatcher",
            );
        }
        if let ValidatedCall::Unrouted(tool_name) = call {
            return ToolOutcome::failed(
                CODE_UNROUTED_TOOL,
                format!(
                    "{tool_name} is handled by the presentation or sight path, not the dispatcher"
                ),
            );
        }
        let tier_one = self.registry.tier(name) == Some(ToolTier::One);
        if tier_one && !self.tier_one_slot_free(now) {
            return ToolOutcome::failed(
                CODE_RATE_LIMITED,
                format!(
                    "playback tool budget exhausted ({TIER_ONE_MAX_CALLS} calls per \
                     {}s); retry shortly",
                    TIER_ONE_WINDOW.as_secs()
                ),
            );
        }
        let turns_reader = matches!(call, ValidatedCall::TurnPrinted { .. });
        #[cfg(feature = "book-agent")]
        let exec_result = if matches!(
            call,
            ValidatedCall::SearchBook { .. }
                | ValidatedCall::OpenPassage { .. }
                | ValidatedCall::GoToPassage { .. }
                | ValidatedCall::HighlightPassage { .. }
        ) {
            execute_agent(
                self.agent_retrieval.as_deref_mut(),
                self.agent_highlight.as_deref_mut(),
                call,
                call_id,
                playback,
                excerpts,
            )
        } else {
            self.execute_core(call, playback, excerpts)
        };
        #[cfg(not(feature = "book-agent"))]
        let exec_result = self.execute_core(call, playback, excerpts);
        match exec_result {
            Ok(payload) => {
                // H3 decision (docs/TASKS.md VA6; ARCHITECTURE §8.1;
                // VOICE_AGENT §5): the budget slot is committed only when the
                // mutation actually happened. §8.1 rates "reversible playback
                // mutations" — a call answered `playback_unavailable`,
                // `unknown_chapter`, or `unknown_page` mutated nothing, and
                // the unavailable answer *instructs* the model to retry
                // (§5's answered-never-panicked message), so charging those
                // refusals would rate-limit it exactly when the surface comes
                // back. This is pin4's rule ("a refused call must not itself
                // occupy the window") extended from pre-execution refusals to
                // execution refusals.
                if tier_one {
                    self.tier_one_calls.push_back(now);
                }
                if turns_reader {
                    self.shown_page = None;
                }
                ToolOutcome::Ok(payload)
            }
            Err(outcome) => outcome,
        }
    }

    /// The pre-existing execution path: excerpt/profile/playback tools with
    /// the egress-event sink borrowed from `self`.
    fn execute_core(
        &mut self,
        call: ValidatedCall,
        playback: &mut dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
    ) -> Result<Value, ToolOutcome> {
        let call = match call {
            ValidatedCall::TurnPrinted { forward } => ValidatedCall::JumpToPrinted {
                page: turn_page_target(self.shown_page, forward, playback, excerpts)?,
            },
            other => other,
        };
        let call = match call {
            ValidatedCall::GetContextAroundPosition { position_ms: None }
                if self.mode == DispatchMode::BookDirect =>
            {
                let visible = self.shown_page.and_then(|page| {
                    excerpts
                        .snapshot()
                        .segments()
                        .iter()
                        .find(|s| s.page_no == page)
                        .map(|s| s.start_ms)
                });
                if visible.is_none() && self.shown_page.is_some() {
                    return Err(ToolOutcome::failed(CODE_NO_CONTEXT, "The displayed page has no readable text. Use its page image or navigate to a readable page; no neighbouring text was substituted."));
                }
                ValidatedCall::GetContextAroundPosition {
                    position_ms: visible,
                }
            }
            other => other,
        };
        let saving = match &call {
            ValidatedCall::UpdateLearningProfile { key, value } => {
                Some((key.clone(), value.clone()))
            }
            _ => None,
        };
        let Self {
            truncation,
            egress_events,
            profile,
            mode,
            saved_preferences,
            ..
        } = self;
        let mut sink = |event: EgressEvent| {
            if egress_events.len() == EGRESS_EVENT_CAP {
                egress_events.pop_front();
            }
            egress_events.push_back(event);
        };
        let result = execute(
            call,
            playback,
            excerpts,
            truncation.as_ref(),
            profile.as_mut(),
            &mut sink,
            *mode,
            true,
        );
        if let (Ok(_), Some((key, value))) = (&result, saving) {
            saved_preferences.retain(|(saved, _)| *saved != key);
            saved_preferences.push((key, value));
        }
        result
    }

    /// Ack-side Tier-1 commit (feature `book-agent`): a deferred citation
    /// spends its slot only when the surface ack says Applied — an emission
    /// or an honest non-application costs nothing (H3 extended).
    #[cfg(feature = "book-agent")]
    pub fn commit_tier_one(&mut self, now: Instant) {
        self.tier_one_calls.push_back(now);
    }

    /// Prune the window and report whether a Tier-1 slot is open at `now`.
    /// Reservation-free: the slot is committed by `dispatch` only after the
    /// mutation succeeds (the H3 decision above).
    fn tier_one_slot_free(&mut self, now: Instant) -> bool {
        while self
            .tier_one_calls
            .front()
            .is_some_and(|&at| now.duration_since(at) >= TIER_ONE_WINDOW)
        {
            self.tier_one_calls.pop_front();
        }
        self.tier_one_calls.len() < TIER_ONE_MAX_CALLS
    }
}

/// Wire one `ChannelEvent::ToolCall` to its answer: dispatch locally, send
/// the outcome back on the same channel as [`SessionChannel::send_tool_result`]
/// (VOICE_AGENT §5; the composed loop forwards tool calls verbatim and the
/// harness owes each one exactly one answer).
///
/// Rejections, playback failures, and rate-limit refusals all travel the same
/// path — the only `Err` out of here is the channel itself failing to send,
/// which is the harness's degradation-matrix problem (§9), not a tool error.
/// `teacher_anchor` must be routed to `normalize_teacher_anchor` before this;
/// one arriving anyway is still answered (as [`CODE_UNROUTED_TOOL`]) so a
/// mis-wired session degrades to a refused tool instead of a stalled protocol.
///
/// **Dispatch and send are decoupled (H1):** the call executes at most once
/// per `call_id`. On a failed send the computed [`ToolResult`] stays cached in
/// the dispatcher, and the harness's retry (pin3: an ack may be delayed, never
/// dropped or forked) re-sends that cached answer — it never re-runs the
/// effect, so a retried `seek_relative` moves the surface exactly once. A
/// successful send settles the cache entry; a distinct provider event reusing
/// a settled id executes fresh.
#[allow(clippy::too_many_arguments)]
pub async fn answer_tool_call(
    dispatcher: &mut Dispatcher,
    channel: &mut dyn SessionChannel,
    call_id: &str,
    name: &str,
    args: &Value,
    now: Instant,
    playback: &mut dyn PlaybackControl,
    excerpts: &mut ExcerptSelector,
) -> Result<(), ChannelError> {
    answer_once(dispatcher, channel, call_id, |dispatcher| {
        let started = Instant::now();
        // 4b: the REAL provider call id rides the dispatch so the journaled
        // outcome row names the call the wire saw (dispatch() would pass
        // ""). Non-agent tools ignore the id otherwise; the agent path
        // already forwards it.
        let outcome =
            dispatcher.dispatch_with_call_id(name, args, call_id, now, playback, excerpts);
        let code = match &outcome {
            ToolOutcome::Ok(_) => "ok",
            ToolOutcome::UnknownTool => CODE_UNKNOWN_TOOL,
            ToolOutcome::InvalidArgs { .. } => CODE_INVALID_ARGS,
            ToolOutcome::Failed { code, .. } => code.as_str(),
            #[cfg(feature = "book-agent")]
            ToolOutcome::Deferred => {
                unreachable!("agent tools flow through answer_agent_tool_call, never here")
            }
        };
        tracing::info!(
            seam = "tools",
            tool = name,
            call = call_id,
            outcome = code,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "tool call answered"
        );
        outcome.into_tool_result()
    })
    .await
}

/// The H1 outcome-preserving send, generalized over how the answer is
/// computed: `compute` runs **at most once per `call_id`** — on the first
/// sight of the id — and its [`ToolResult`] is cached until a send succeeds.
/// Later calls with the same id (the harness retrying after a failed send)
/// re-send the cached answer and never invoke `compute` again, so any side
/// effect inside it (a playback mutation, a surfaced `TeacherAnchor` event)
/// happens exactly once per logical call.
///
/// [`answer_tool_call`] is this with `compute = dispatch`; the session
/// harness's anchor path is this with `compute = normalize + emit`.
pub async fn answer_once(
    dispatcher: &mut Dispatcher,
    channel: &mut dyn SessionChannel,
    call_id: &str,
    compute: impl FnOnce(&mut Dispatcher) -> ToolResult,
) -> Result<(), ChannelError> {
    let result = match dispatcher.pending_answer(call_id) {
        Some(cached) => cached,
        None => {
            let result = compute(dispatcher);
            dispatcher.remember_answer(call_id, result.clone());
            result
        }
    };
    channel.send_tool_result(call_id, result).await?;
    dispatcher.settle_answer(call_id);
    Ok(())
}

/// Re-send every cached, unacknowledged answer in arrival order — the H1
/// recovery step the harness runs after a renewal completes (pin3: an ack may
/// be delayed across a renewal, never dropped). Each success settles its
/// entry; the first failure returns with everything not yet acknowledged
/// still cached, so the call is safely repeatable.
pub async fn resend_pending_answers(
    dispatcher: &mut Dispatcher,
    channel: &mut dyn SessionChannel,
) -> Result<(), ChannelError> {
    while let Some((call_id, result)) = dispatcher.pending_answers.front().cloned() {
        channel.send_tool_result(&call_id, result).await?;
        dispatcher.settle_answer(&call_id);
    }
    Ok(())
}

/// Execute a validated call. Tier-1 arms resolve to plain [`PlaybackControl`]
/// effects — the dispatcher computes every absolute seek target itself
/// (playback.rs header: target arithmetic stays on this side of the seam).
/// Tier-0 arms return book text **only** as a [`SelectedExcerpt`] out of the
/// [`ExcerptSelector`] — sentinel-delimited and hard-byte-capped — which is
/// the VOICE_AGENT §4 single-egress invariant this module's header promises.
/// With a provider `truncation` policy installed (H2) they take the selector's
/// budgeted paths: tokenizer-measured trimming first, the byte cap as the
/// backstop behind it, every content drop reported into `egress` *and* onto
/// the payload ([`excerpt_payload`]).
#[allow(clippy::too_many_arguments)]
fn execute(
    call: ValidatedCall,
    playback: &mut dyn PlaybackControl,
    excerpts: &mut ExcerptSelector,
    truncation: Option<&TruncationPolicy>,
    profile: Option<&mut Box<dyn LearningProfileSink>>,
    egress: &mut dyn FnMut(EgressEvent),
    mode: DispatchMode,
    include_landing_text: bool,
) -> Result<Value, ToolOutcome> {
    let standalone = mode == DispatchMode::Standalone;
    // F7 jobless-grounded: the audio-bound tools have no narration timeline
    // to act on — an honest typed refusal, never a silent cursor mutation
    // that pretends seconds mean something in character-offset space.
    let no_narration = || {
        ToolOutcome::failed(
            CODE_NO_NARRATION,
            "this book has no narration audio yet — cast the book to get \
             playback; chapter and page navigation still work",
        )
    };
    if mode == DispatchMode::BookDirect
        && matches!(
            call,
            ValidatedCall::Pause
                | ValidatedCall::Resume
                | ValidatedCall::SetSpeed { .. }
                | ValidatedCall::SeekRelative { .. }
                | ValidatedCall::ReplayLastMinute
        )
    {
        return Err(no_narration());
    }
    match call {
        ValidatedCall::UpdateLearningProfile { key, value } => {
            let sink = profile.ok_or_else(|| {
                ToolOutcome::failed(
                    CODE_PROFILE_UNAVAILABLE,
                    "the learning profile is not available in this session",
                )
            })?;
            match sink.update(&key, &value) {
                Ok(()) => {
                    tracing::debug!(
                        "voice tool: update_learning_profile accepted key {key:?} \
                         ({} value chars)",
                        value.chars().count()
                    );
                    Ok(json!({
                        "recorded": true,
                        "key": key,
                        "apply": format!("Follow this from now on: {key}: {value}"),
                    }))
                }
                Err(refused) => {
                    tracing::debug!(
                        "voice tool: update_learning_profile refused key {key:?}: {}",
                        refused.reason
                    );
                    Err(ToolOutcome::failed(CODE_PROFILE_REFUSED, refused.reason))
                }
            }
        }
        ValidatedCall::Pause => {
            playback.pause()?;
            Ok(json!({ "paused": true }))
        }
        ValidatedCall::Resume => {
            playback.resume()?;
            Ok(json!({ "paused": false }))
        }
        ValidatedCall::SetSpeed { speed } => {
            playback.set_speed(speed)?;
            Ok(json!({ "speed": speed }))
        }
        ValidatedCall::SeekRelative { seconds } => {
            // K10 fix (F7 spec §5): an empty snapshot used to clamp every
            // target to 0 and answer success — a silent seek-to-start over
            // nothing. Refuse typed instead.
            if excerpts.snapshot().is_empty() {
                return Err(ToolOutcome::failed(
                    CODE_NO_CONTEXT,
                    if standalone {
                        "there is no timeline to seek: no book is attached to this session"
                    } else {
                        "there is no timeline to seek: the book has no narrated segments"
                    },
                ));
            }
            let from = playback.position_ms()?;
            let offset_ms = (seconds * 1000.0).round() as i64;
            let target = clamp_to_timeline(from as i64 + offset_ms, excerpts.snapshot());
            seek(playback, target)
        }
        ValidatedCall::ReplayLastMinute => {
            let from = playback.position_ms()?;
            seek(playback, from.saturating_sub(60_000))
        }
        ValidatedCall::ReplayPreviousParagraph => {
            let from = playback.position_ms()?;
            let segments = excerpts.snapshot().segments();
            let target = current_segment(segments, from)
                .and_then(|idx| segments.get(idx.saturating_sub(1)))
                .map_or(0, |segment| segment.start_ms);
            seek(playback, target)
        }
        ValidatedCall::ReplayPreviousPrinted => {
            let from = playback.position_ms()?;
            let segments = excerpts.snapshot().segments();
            seek(playback, previous_page_start(segments, from))
        }
        ValidatedCall::JumpToChapter { chapter } => {
            let chapters = excerpts.snapshot().chapters();
            let span = match usize::try_from(chapter)
                .ok()
                .and_then(|idx| chapters.get(idx))
            {
                Some(s) => s,
                None => {
                    return Err(ToolOutcome::failed(
                        CODE_UNKNOWN_CHAPTER,
                        match mode {
                            DispatchMode::Standalone => format!(
                                "chapter index {chapter} does not exist; no book is attached \
                                 to this session"
                            ),
                            _ => format!(
                                "chapter index {chapter} does not exist; the book has {} chapters \
                                 (0-based)",
                                chapters.len()
                            ),
                        },
                    ));
                }
            };
            // F10-D2: a segment-less chapter (its pages carry no extracted
            // text) has a degenerate span; without the book's own page span
            // either, no reader can turn to it — refuse typed rather than
            // answer success for a move nothing on screen can show.
            if mode == DispatchMode::BookDirect
                && span.start_ms == span.end_ms
                && span.page_start <= 0
            {
                return Err(ToolOutcome::failed(
                    CODE_UNRESOLVED_PAGE,
                    format!(
                        "chapter {chapter} could not be opened: it has no readable text and \
                         no page this book can show, so the reader did not move — tell the \
                         user plainly"
                    ),
                ));
            }
            seek(playback, span.start_ms)?;
            let chapter_title = if span.title.trim().is_empty() {
                format!("Chapter {}", span.idx)
            } else {
                span.title.clone()
            };
            let page_no = if span.page_start > 0 {
                span.page_start
            } else {
                excerpts
                    .snapshot()
                    .segments()
                    .iter()
                    .take_while(|s| s.start_ms <= span.start_ms)
                    .last()
                    .map(|s| s.page_no)
                    .filter(|p| *p > 0)
                    .unwrap_or(1)
            };
            let has_text = span.start_ms < span.end_ms;
            let target = span.start_ms;
            let mut payload = json!({
                "position_ms": target,
                "chapter": span.idx,
                "chapter_title": chapter_title,
                "page": page_no,
                "status": "The reader's screen now displays this chapter."
            });
            if has_text && include_landing_text {
                attach_landing_text(
                    &mut payload,
                    target,
                    playback,
                    excerpts,
                    truncation,
                    egress,
                    mode,
                );
            }
            Ok(payload)
        }
        ValidatedCall::JumpToPrinted { page } => {
            let segments = excerpts.snapshot().segments();
            let first_on_page = i64::try_from(page)
                .ok()
                .and_then(|page_no| segments.iter().find(|s| s.page_no == page_no))
                .ok_or_else(|| {
                    ToolOutcome::failed(
                        CODE_UNKNOWN_PAGE,
                        match mode {
                            DispatchMode::Standalone => format!(
                                "no document page {page} — no book is attached to this session"
                            ),
                            // No "narrated" claim without a narration job.
                            DispatchMode::BookDirect => {
                                format!(
                                    "document page {page} carries no readable text in this book"
                                )
                            }
                            DispatchMode::Grounded => {
                                format!("no narrated passage carries document page number {page}")
                            }
                        },
                    )
                })?;
            seek(playback, first_on_page.start_ms)?;
            let chapter_title = excerpts
                .snapshot()
                .chapters()
                .iter()
                .find(|c| {
                    first_on_page.start_ms >= c.start_ms
                        && (c.start_ms == c.end_ms || first_on_page.start_ms < c.end_ms)
                })
                .or_else(|| {
                    excerpts.snapshot().chapters().iter().find(|c| {
                        first_on_page.page_no >= c.page_start && first_on_page.page_no <= c.page_end
                    })
                })
                .map(|c| {
                    if c.title.trim().is_empty() {
                        format!("Chapter {}", c.idx)
                    } else {
                        c.title.clone()
                    }
                })
                .unwrap_or_else(|| "Book Content".to_string());

            let target = first_on_page.start_ms;
            let mut payload = json!({
                "position_ms": target,
                "page": page,
                "chapter_title": chapter_title,
                "status": "The reader was sent to this page."
            });
            if include_landing_text {
                attach_landing_text(
                    &mut payload,
                    target,
                    playback,
                    excerpts,
                    truncation,
                    egress,
                    mode,
                );
            }
            Ok(payload)
        }
        ValidatedCall::SummarizeSection => {
            let no_window = || {
                ToolOutcome::failed(
                    CODE_NO_CONTEXT,
                    match mode {
                        DispatchMode::Standalone => {
                            "no book is attached to this session, so there is no book text"
                        }
                        DispatchMode::BookDirect => "this book has no readable text extracted yet",
                        DispatchMode::Grounded => {
                            "no book text is in context yet; wait for playback to inject a window"
                        }
                    },
                )
            };
            let selected = match truncation {
                Some(policy) => excerpts
                    .budgeted_excerpt_for_window(policy, egress)
                    .ok_or_else(no_window)?,
                None => {
                    let window = excerpts.current_window().ok_or_else(no_window)?;
                    excerpts.excerpt_for_range(window.first_idx, window.last_idx)
                }
            };
            let mut payload = excerpt_payload(selected);
            attach_coverage(&mut payload, playback, excerpts, mode, None);
            Ok(payload)
        }
        ValidatedCall::GetContextAroundPosition { position_ms } => {
            let pos = match position_ms {
                Some(pos) => pos,
                None => playback.position_ms()?,
            };
            let selected = match truncation {
                Some(policy) => excerpts.budgeted_excerpt_at(pos, policy, egress),
                None => excerpts.excerpt_at(pos),
            }
            .ok_or_else(|| {
                ToolOutcome::failed(
                    CODE_NO_CONTEXT,
                    match mode {
                        DispatchMode::Standalone => {
                            "no book is attached to this session, so there is no book text"
                        }
                        DispatchMode::BookDirect => "this book has no readable text extracted yet",
                        DispatchMode::Grounded => "the book has no narrated segments",
                    },
                )
            })?;
            let mut payload = excerpt_payload(selected);
            attach_coverage(&mut payload, playback, excerpts, mode, Some(pos));
            Ok(payload)
        }
        ValidatedCall::GetContextForRange { range } => {
            // F8-D1b: one-round-trip bounded fetch (owner defect D-QA1). The
            // shape is HTTP-range / partial-content semantics over the
            // played-ranges coverage model — real snapshot text only, gap
            // honesty on the payload, honest first/last chunks over budget.
            let no_book = || {
                ToolOutcome::failed(
                    CODE_NO_CONTEXT,
                    match mode {
                        DispatchMode::Standalone => {
                            "no book is attached to this session, so there is no book text"
                        }
                        DispatchMode::BookDirect => "this book has no readable text extracted yet",
                        DispatchMode::Grounded => "the book has no narrated segments",
                    },
                )
            };
            if standalone || excerpts.snapshot().is_empty() {
                return Err(no_book());
            }
            let units = match mode {
                DispatchMode::BookDirect => PositionUnits::Offset,
                _ => PositionUnits::Millis,
            };
            let (start, end, range_source) =
                match range {
                    Some((start, end)) => (start, end, None),
                    // The omitted-bounds selector. F9-D1 / F10-D1 (owner
                    // semantics law): "covered so far" is the USER's mental model
                    // — everything up to where they stand — in EVERY mode:
                    // [0, narration position) in a grounded session,
                    // [0, reading position) in BookDirect. NOT the span consumed
                    // since this voice session started. Session honesty still
                    // rides the payload: `range_fully_played`/`unplayed_gaps`
                    // below are measured against THIS session's accumulator, so
                    // quiz bounding stays intact. Only without a live position
                    // (surface gone, or standing at the book's start) does the
                    // selector fall back to the session's outer span, then to a
                    // typed refusal.
                    None => {
                        let so_far = playback.position_ms().ok().filter(|&pos| pos > 0).map(
                            |pos| match mode {
                                DispatchMode::BookDirect => (pos, "reading_so_far"),
                                _ => (pos, "narration_so_far"),
                            },
                        );
                        match so_far {
                            Some((pos, source)) => (0, pos, Some(source)),
                            None => {
                                match excerpts.played_ranges().outer_span() {
                                    Some((start, end)) => (start, end, Some("played_range")),
                                    None => return Err(ToolOutcome::failed(
                                        CODE_NO_CONTEXT,
                                        "nothing has been played or read in this session yet, so \
                                     there is no played range to fetch; pass explicit \
                                     `start_position` and `end_position` instead",
                                    )),
                                }
                            }
                        }
                    }
                };
            let Some((first, last)) = excerpts.segment_span_for_positions(start, end) else {
                return Err(ToolOutcome::failed(
                    CODE_NO_CONTEXT,
                    format!(
                        "no book text lies within {}–{}; the book's text spans {}–{}",
                        position_label(units, start),
                        position_label(units, end),
                        position_label(
                            units,
                            excerpts
                                .snapshot()
                                .segments()
                                .first()
                                .map_or(0, |s| s.start_ms)
                        ),
                        position_label(
                            units,
                            excerpts
                                .snapshot()
                                .segments()
                                .last()
                                .map_or(0, |s| s.end_ms)
                        ),
                    ),
                ));
            };
            // Gap honesty is measured BEFORE this fetch mutates any coverage:
            // "covered so far" means covered before the model asked.
            let gaps = excerpts.played_ranges().gaps_within(start, end);
            // F10-D1 round 3: every returned piece names its positional
            // bounds in the session's units, so a Split reply is machine-
            // usable — the model fetches `omitted_range` next, recursing
            // while replies keep splitting, until the pieces cover the
            // requested range. Additive fields; nothing renamed.
            let fetch = excerpts.range_fetch_within(first, last, start, end, truncation, egress);
            let clip = |a: u64, b: u64| json!({ "start": a.max(start), "end": b.min(end) });
            let mut payload = match fetch {
                RangeFetch::Full(selected) => {
                    // Only text actually returned in full may count as
                    // visited (BookDirect): a hard-capped payload noted whole
                    // would over-claim — the forbidden direction.
                    let returned: Vec<(usize, usize)> = selected
                        .hard_cap
                        .is_none()
                        .then_some(selected.segment_range)
                        .into_iter()
                        .collect();
                    let complete = selected.hard_cap.is_none();
                    let all = excerpts.snapshot().segments();
                    let range = clip(
                        all[selected.segment_range.0].start_ms,
                        all[selected.segment_range.1].end_ms,
                    );
                    let mut payload = excerpt_payload(selected);
                    payload["complete"] = json!(complete);
                    payload["range"] = range;
                    note_range_visited(excerpts, mode, &returned);
                    payload
                }
                RangeFetch::Split {
                    head,
                    head_partially_cut,
                    tail,
                    tail_partially_cut,
                    omitted_segments,
                    total_segments,
                    head_end,
                    tail_start,
                } => {
                    // Same under-claim rule per chunk: a cut or capped chunk
                    // is never noted as visited.
                    let returned: Vec<(usize, usize)> = [
                        (!head_partially_cut && head.hard_cap.is_none())
                            .then_some(head.segment_range),
                        (!tail_partially_cut && tail.hard_cap.is_none())
                            .then_some(tail.segment_range),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    // The interior not returned, in position units — whole
                    // segments between the chunks, or the uncovered inside
                    // of a cut segment. Emitted only when it is a strict
                    // sub-range of the request, so recursion always shrinks.
                    let omitted_range = (head_end < tail_start
                        && (head_end > start || tail_start < end))
                        .then(|| json!({ "start": head_end, "end": tail_start }));
                    let all = excerpts.snapshot().segments();
                    let head_range = clip(all[head.segment_range.0].start_ms, head_end);
                    let tail_range = clip(tail_start, all[tail.segment_range.1].end_ms);
                    let mut first_chunk = excerpt_payload(head);
                    first_chunk["range"] = head_range;
                    let mut last_chunk = excerpt_payload(tail);
                    last_chunk["range"] = tail_range;
                    let mut payload = json!({
                        "complete": false,
                        "note": "the requested range exceeds the reply budget: this \
                                 payload carries the FIRST and LAST chunks only; the \
                                 middle you do not have yet is `omitted_range` — call \
                                 get_context_for_range on it next, and again on any \
                                 further `omitted_range`, until every piece is complete",
                        "first_chunk": first_chunk,
                        "last_chunk": last_chunk,
                        "omitted_middle_segments": omitted_segments,
                        "total_segments_in_range": total_segments,
                    });
                    if let Some(omitted_range) = omitted_range {
                        payload["omitted_range"] = omitted_range;
                    }
                    if head_partially_cut {
                        payload["first_chunk_partially_cut"] = json!(true);
                    }
                    if tail_partially_cut {
                        payload["last_chunk_partially_cut"] = json!(true);
                    }
                    note_range_visited(excerpts, mode, &returned);
                    payload
                }
            };
            payload["requested_range"] = json!(format!(
                "{}–{}",
                position_label(units, start),
                position_label(units, end)
            ));
            if let Some(source) = range_source {
                payload["range_source"] = json!(source);
            }
            // The D1 quiz-bounding law: whether the request reaches beyond
            // what actually played, with every unplayed gap named.
            payload["range_fully_played"] = json!(gaps.is_empty());
            if !gaps.is_empty() {
                let labels: Vec<String> = gaps
                    .iter()
                    .map(|&(a, b)| {
                        format!("{}–{}", position_label(units, a), position_label(units, b))
                    })
                    .collect();
                // Context diet (2026-09-24): a whole-book fetch can name
                // more gaps than any turn needs — cap the verbatim list at
                // 8, count the rest. The honesty is the count + the fetch
                // hint, not the enumeration.
                let bounded = if labels.len() > UNPLAYED_GAPS_VERBATIM_MAX {
                    let overflow = labels.len() - UNPLAYED_GAPS_VERBATIM_MAX;
                    let mut capped = labels[..UNPLAYED_GAPS_VERBATIM_MAX].to_vec();
                    capped.push(format!("and {overflow} more"));
                    capped
                } else {
                    labels
                };
                payload["unplayed_gaps"] = json!(bounded);
            }
            let pos = playback.position_ms().ok().unwrap_or(start);
            payload["coverage"] = json!(excerpts.coverage_line(pos, units));
            Ok(payload)
        }
        ValidatedCall::TeacherAnchor { .. } => {
            unreachable!("dispatch() answers TeacherAnchor before execute()")
        }
        ValidatedCall::Unrouted(_) => {
            unreachable!("dispatch() answers Unrouted before execute()")
        }
        ValidatedCall::TurnPrinted { .. } => {
            unreachable!("execute_core() resolves TurnPrinted to a printed jump")
        }
        #[cfg(feature = "book-agent")]
        ValidatedCall::SearchBook { .. }
        | ValidatedCall::OpenPassage { .. }
        | ValidatedCall::GoToPassage { .. }
        | ValidatedCall::HighlightPassage { .. } => {
            unreachable!("dispatch() routes agent tools to execute_agent()")
        }
    }
}

/// F8 D1: every Tier-0 window payload carries the coverage header line —
/// the model's per-turn view of what actually played (BookGrounded) or was
/// visited (BookDirect). BookDirect additionally records the just-rendered
/// window as visited here: rendering IS the cursor-visit coverage feed (the
/// grounded feed is the runtime's playback sampling instead — a fetched
/// window is not "played" audio and must not claim coverage there).
/// `pos` is the position the line reports; `None` falls back to the current
/// playback/cursor position, then to the rendered window's anchor.
fn attach_coverage(
    payload: &mut Value,
    playback: &mut dyn PlaybackControl,
    excerpts: &mut ExcerptSelector,
    mode: DispatchMode,
    pos: Option<u64>,
) {
    if mode == DispatchMode::Standalone || excerpts.snapshot().is_empty() {
        return;
    }
    if mode == DispatchMode::BookDirect {
        excerpts.note_window_visited();
    }
    let units = match mode {
        DispatchMode::BookDirect => PositionUnits::Offset,
        _ => PositionUnits::Millis,
    };
    let pos = pos
        .or_else(|| playback.position_ms().ok())
        .or_else(|| {
            let window = excerpts.current_window()?;
            excerpts
                .snapshot()
                .segments()
                .get(window.anchor_idx)
                .map(|segment| segment.start_ms)
        })
        .unwrap_or(0);
    payload["coverage"] = json!(excerpts.coverage_header(pos, units));
    // Reading order (field session 2026-10-09): where teaching continues
    // once this window is explained — the tutor otherwise re-read the same
    // window or jumped a chapter ahead with pages still uncovered.
    if mode == DispatchMode::BookDirect {
        if let Some(next) = excerpts.continue_after_window() {
            payload["continue_at_page"] = json!(next.page);
            // Review 2026-10-09, T1: a window that ended mid-page continues
            // on the rest of that page — a page jump would land on the
            // page's top and the model would treat it as read.
            payload["reading_next"] = json!(if next.rest_of_page {
                format!(
                    "Explain this text to the user first. Only when they want to go on, the \
                     rest of page {} comes next: call get_context_around_position with \
                     position_ms {}.",
                    next.page, next.position
                )
            } else {
                format!(
                    "Explain this text to the user first. Only when they want to go on, next \
                     comes page {}: call jump_to_page with page {}.",
                    next.page, next.page
                )
            });
        }
    }
}

/// A successful jump answers with the book text at the landing point — the
/// same budgeted window `get_context_around_position` would return there
/// (investigation 2026-10-09, §3.4/§3.5). The tool answer is the one channel
/// that reaches the model inside its current turn without interrupting it:
/// before this, a jump returned only a page number, so the model explained a
/// page it had never read (book-only sessions), or the 1 s chapter sampler
/// pushed the window afterwards as a `clientContent` frame that interrupts
/// the reply explaining the jump (cast sessions). A landing with no
/// selectable text answers without `book_text`, never with a neighbour's.
fn attach_landing_text(
    payload: &mut Value,
    target: u64,
    playback: &mut dyn PlaybackControl,
    excerpts: &mut ExcerptSelector,
    truncation: Option<&TruncationPolicy>,
    egress: &mut dyn FnMut(EgressEvent),
    mode: DispatchMode,
) {
    if mode == DispatchMode::Standalone {
        return;
    }
    // Read BEFORE the landing window marks itself visited.
    if let Some((first, last)) = excerpts.skipped_pages_before(target) {
        payload["reading_order"] = json!(skipped_pages_note(first, last));
    }
    let selected = match truncation {
        Some(policy) => excerpts.budgeted_excerpt_at(target, policy, egress),
        None => excerpts.excerpt_at(target),
    };
    let Some(selected) = selected else {
        return;
    };
    if let (Value::Object(fields), Value::Object(text)) = (payload, excerpt_payload(selected)) {
        fields.extend(text);
        let mut merged = Value::Object(std::mem::take(fields));
        attach_coverage(&mut merged, playback, excerpts, mode, Some(target));
        if let Value::Object(done) = merged {
            *fields = done;
        }
    }
}

/// The jump answer's `reading_order` line: the pages this jump skipped
/// and the way back to teaching in order.
fn skipped_pages_note(first: i64, last: i64) -> String {
    let pages = if first == last {
        format!("page {first}")
    } else {
        format!("pages {first}\u{2013}{last}")
    };
    format!(
        "This jump skipped {pages}, which were not covered yet. If the jump \
         was only to show the user something they asked to see, explain it, then call \
         jump_to_page with page {first} and continue teaching in order from there — unless \
         the user asked to skip ahead."
    )
}

/// F8-D1b coverage feed for the bounded-range fetch: in BookDirect, text this
/// fetch actually RETURNED counts as visited (rendering is the cursor-visit
/// coverage feed there, the `attach_coverage` rule); a grounded fetch is not
/// playback and must claim nothing. Only returned chunks are noted — an
/// omitted split middle stays uncovered, honestly.
fn note_range_visited(
    excerpts: &mut ExcerptSelector,
    mode: DispatchMode,
    returned: &[(usize, usize)],
) {
    if mode != DispatchMode::BookDirect {
        return;
    }
    let spans: Vec<(u64, u64)> = returned
        .iter()
        .filter_map(|&(first, last)| {
            let segments = excerpts.snapshot().segments();
            Some((segments.get(first)?.start_ms, segments.get(last)?.end_ms))
        })
        .collect();
    for (start, end) in spans {
        excerpts.note_played(start, end);
    }
}

fn turn_page_target(
    shown_page: Option<i64>,
    forward: bool,
    playback: &dyn PlaybackControl,
    excerpts: &ExcerptSelector,
) -> Result<u64, ToolOutcome> {
    let segments = excerpts.snapshot().segments();
    let base = match shown_page {
        Some(page) => page,
        None => {
            let pos = playback.position_ms()?;
            segments
                .iter()
                .rev()
                .find(|segment| segment.start_ms <= pos && segment.page_no > 0)
                .or_else(|| segments.iter().find(|segment| segment.page_no > 0))
                .map(|segment| segment.page_no)
                .ok_or_else(|| {
                    ToolOutcome::failed(CODE_UNKNOWN_PAGE, "this book has no pages with text")
                })?
        }
    };
    let target = if forward {
        segments
            .iter()
            .find(|segment| segment.page_no > base)
            .map(|segment| segment.page_no)
    } else {
        segments
            .iter()
            .rev()
            .find(|segment| segment.page_no > 0 && segment.page_no < base)
            .map(|segment| segment.page_no)
    };
    target
        .and_then(|page| u64::try_from(page).ok())
        .ok_or_else(|| {
            ToolOutcome::failed(
                CODE_UNKNOWN_PAGE,
                format!(
                    "page {base} is the {} page with text",
                    if forward { "last" } else { "first" }
                ),
            )
        })
}

fn seek(playback: &mut dyn PlaybackControl, target: u64) -> Result<Value, ToolOutcome> {
    playback.seek_to_ms(target)?;
    Ok(json!({ "position_ms": target }))
}

/// Book Agent tool execution (charter B-1). Runs against the two agent ports;
/// unwired ports answer [`CODE_AGENT_UNAVAILABLE`]. The retrieval port owns
/// handles and payload caps; this layer owns navigation resolution against
/// the session snapshot and the applied-effect honesty of `highlight_passage`.
#[cfg(feature = "book-agent")]
fn execute_agent(
    retrieval: Option<&mut (dyn AgentRetrievalPort + 'static)>,
    highlight: Option<&mut (dyn AgentHighlightPort + 'static)>,
    call: ValidatedCall,
    call_id: &str,
    playback: &mut dyn PlaybackControl,
    excerpts: &mut ExcerptSelector,
) -> Result<Value, ToolOutcome> {
    let unwired = || {
        ToolOutcome::failed(
            CODE_AGENT_UNAVAILABLE,
            "book agent retrieval is not wired in this session",
        )
    };
    let retrieval = retrieval.ok_or_else(unwired)?;
    match call {
        ValidatedCall::SearchBook { queries, chapter } => {
            // validate() bounded the chapter to u32 range already.
            let scope = chapter.map(|c| c as u32);
            Ok(retrieval.search_book(&queries, scope))
        }
        ValidatedCall::OpenPassage { handle } => Ok(retrieval.open_passage(&handle)),
        ValidatedCall::GoToPassage { handle } => {
            let coord = retrieval
                .resolve(&handle)
                .ok_or_else(|| unknown_handle(&handle))?;
            let segment = resolve_session_segment(excerpts.snapshot(), coord)
                .ok_or_else(|| unknown_handle(&handle))?;
            let (start_ms, page) = (segment.start_ms, segment.page_no);
            playback.seek_to_ms(start_ms)?;
            // Applied position/page facts only (charter §4.2) — no text.
            Ok(json!({
                "position_ms": start_ms,
                "page": page,
                "chapter": coord.chapter,
            }))
        }
        ValidatedCall::HighlightPassage { handle, span } => {
            if retrieval.resolve(&handle).is_none() {
                return Err(unknown_handle(&handle));
            }
            let coord = retrieval.resolve_citable(&handle).ok_or_else(|| {
                // Live handle, generated prose: honest refusal, never a
                // citation over non-evidence (charter T9).
                ToolOutcome::failed(
                    CODE_HIGHLIGHT_NOT_APPLIED,
                    "this passage is generated narration, not citable source text",
                )
            })?;
            // Validate the coordinate against the session snapshot before any
            // presentation effect (wrong-render handles refuse here too).
            resolve_session_segment(excerpts.snapshot(), coord)
                .ok_or_else(|| unknown_handle(&handle))?;
            // §4.2 "validated word span": the span must lie inside the cited
            // passage. Out-of-range spans are refused, never clamped into a
            // different citation.
            let span = match span {
                None => None,
                Some((s, c)) => {
                    let words = excerpts
                        .snapshot()
                        .segment_word_count(coord.idx as usize)
                        .unwrap_or(0) as u64;
                    if s.saturating_add(c) > words {
                        return Err(ToolOutcome::invalid(format!(
                            "cited span exceeds the passage ({words} words)"
                        )));
                    }
                    // validate() bounded both to u32 range already.
                    Some((s as u32, c as u32))
                }
            };
            let highlight = highlight.ok_or_else(unwired)?;
            match highlight.apply(call_id, coord, span) {
                // Ack/status only, zero book text (charter §4.2).
                HighlightStatus::Applied => Ok(json!({ "applied": true })),
                HighlightStatus::NotApplied { reason } => {
                    Err(ToolOutcome::failed(CODE_HIGHLIGHT_NOT_APPLIED, reason))
                }
                // The surface answers after render; the caller registers the
                // pending call and the ack path completes it.
                HighlightStatus::Pending => Err(ToolOutcome::Deferred),
            }
        }
        // Static message on purpose: Debug-formatting the call would echo
        // model-supplied argument contents into a provider-visible error.
        _ => Err(ToolOutcome::failed(CODE_UNROUTED_TOOL, "not an agent tool")),
    }
}

#[cfg(feature = "book-agent")]
fn unknown_handle(handle: &str) -> ToolOutcome {
    // The token itself is not echoed at full length: bounded, no book text.
    let shown: String = handle.chars().take(8).collect();
    ToolOutcome::failed(
        CODE_UNKNOWN_HANDLE,
        format!("handle {shown}… is not live in this session; search again"),
    )
}

/// Resolve a passage coordinate against the session snapshot. The retrieval
/// `idx` space and the snapshot's dense segment index coincide for the active
/// render (both 0-based and gap-free per render); the printed-page check
/// verifies that identity per call, and any mismatch is refused as an
/// unknown handle rather than best-effort remapped (charter T10).
#[cfg(feature = "book-agent")]
fn resolve_session_segment(
    snapshot: &SessionSnapshot,
    coord: PassageCoord,
) -> Option<&SessionSegment> {
    let segment = snapshot.segments().get(coord.idx as usize)?;
    if segment.page_no != i64::from(coord.page) {
        return None;
    }
    Some(segment)
}

/// Clamp a computed seek target to `[0, end-of-book]` — §5's "the result stays
/// within the book". The end is the last segment's end; an empty snapshot
/// clamps everything to 0.
fn clamp_to_timeline(target_ms: i64, snapshot: &SessionSnapshot) -> u64 {
    let end = snapshot.segments().last().map_or(0, |s| s.end_ms);
    target_ms.clamp(0, end as i64) as u64
}

/// The segment "currently playing" at `pos`: the last one whose start is at or
/// before `pos` (gaps between segments attribute to the segment just heard).
/// `None` before the first segment starts or on an empty snapshot.
fn current_segment(segments: &[SessionSegment], pos_ms: u64) -> Option<usize> {
    segments
        .partition_point(|s| s.start_ms <= pos_ms)
        .checked_sub(1)
}

/// Start of the printed page before the one playing at `pos`; the start of the
/// book when already on its first page (a clamp, mirroring seek), relying on
/// segments being in playback order.
fn previous_page_start(segments: &[SessionSegment], pos_ms: u64) -> u64 {
    let Some(current) = current_segment(segments, pos_ms) else {
        return 0;
    };
    let page = segments[current].page_no;
    let first_of_page = segments[..=current]
        .iter()
        .rposition(|s| s.page_no != page)
        .map_or(0, |before| before + 1);
    let Some(previous_page_last) = first_of_page.checked_sub(1) else {
        return segments.first().map_or(0, |s| s.start_ms);
    };
    let prev_page = segments[previous_page_last].page_no;
    let first_of_prev = segments[..=previous_page_last]
        .iter()
        .rposition(|s| s.page_no != prev_page)
        .map_or(0, |before| before + 1);
    segments[first_of_prev].start_ms
}

/// The one shape book text leaves a tool outcome in: the selector's delimited,
/// capped excerpt. Fired caps ride along — content loss is never silent
/// (selector.rs / policy.rs contract), not even toward the model: a token-
/// budget trim carries its `truncation` rider (H2) and the byte backstop its
/// `hard_cap` rider (pin6), each present exactly when it fired.
fn excerpt_payload(selected: SelectedExcerpt) -> Value {
    let mut payload = json!({ "book_text": selected.excerpt.delimited_text });
    if let Some(cap) = selected.hard_cap {
        payload["hard_cap"] = json!({
            "kept_bytes": cap.kept_bytes,
            "dropped_bytes": cap.dropped_bytes,
        });
    }
    if let Some(notice) = selected.truncation {
        payload["truncation"] = json!({
            "budget_tokens": notice.budget_tokens,
            "window_tokens": notice.window_tokens,
            "dropped_leading_segments": notice.dropped_leading_segments,
            "dropped_trailing_segments": notice.dropped_trailing_segments,
            "anchor_partially_cut": notice.anchor_partially_cut,
        });
    }
    payload
}

static NO_FIELDS: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);

/// Args as an object with only `allowed` keys. `null` is accepted as the
/// empty object: several wire protocols omit the arguments member entirely
/// for parameterless calls, and adapters surface that as `Value::Null`.
fn object_with<'a>(
    args: &'a Value,
    allowed: &[&str],
) -> Result<&'a Map<String, Value>, ToolOutcome> {
    let fields = match args {
        Value::Null => &NO_FIELDS,
        Value::Object(fields) => fields,
        other => {
            return Err(ToolOutcome::invalid(format!(
                "arguments must be a JSON object, got {}",
                json_type(other)
            )))
        }
    };
    if let Some(unknown) = fields.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ToolOutcome::invalid(format!(
            "unknown field `{unknown}` (schema forbids additional properties)"
        )));
    }
    Ok(fields)
}

fn no_args(args: &Value, call: ValidatedCall) -> Result<ValidatedCall, ToolOutcome> {
    object_with(args, &[])?;
    Ok(call)
}

fn required_number(fields: &Map<String, Value>, key: &str) -> Result<f64, ToolOutcome> {
    match fields.get(key) {
        None => Err(ToolOutcome::invalid(format!(
            "missing required field `{key}`"
        ))),
        Some(value) => value.as_f64().ok_or_else(|| {
            ToolOutcome::invalid(format!(
                "`{key}` must be a number, got {}",
                json_type(value)
            ))
        }),
    }
}

fn required_unsigned(fields: &Map<String, Value>, key: &str) -> Result<u64, ToolOutcome> {
    match fields.get(key) {
        None => Err(ToolOutcome::invalid(format!(
            "missing required field `{key}`"
        ))),
        Some(value) => decode_unsigned(value, key),
    }
}

fn required_bounded_string(
    fields: &Map<String, Value>,
    key: &str,
    max_chars: usize,
) -> Result<String, ToolOutcome> {
    let value = match fields.get(key) {
        None => {
            return Err(ToolOutcome::invalid(format!(
                "missing required field `{key}`"
            )))
        }
        Some(value) => value.as_str().ok_or_else(|| {
            ToolOutcome::invalid(format!(
                "`{key}` must be a string, got {}",
                json_type(value)
            ))
        })?,
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ToolOutcome::invalid(format!("`{key}` must not be empty")));
    }
    if trimmed.chars().count() > max_chars {
        return Err(ToolOutcome::invalid(format!(
            "`{key}` must be at most {max_chars} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_unsigned(fields: &Map<String, Value>, key: &str) -> Result<Option<u64>, ToolOutcome> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => decode_unsigned(value, key).map(Some),
    }
}

fn decode_unsigned(value: &Value, key: &str) -> Result<u64, ToolOutcome> {
    value.as_u64().ok_or_else(|| {
        ToolOutcome::invalid(format!(
            "`{key}` must be a non-negative integer, got {}",
            if value.is_number() {
                value.to_string()
            } else {
                json_type(value).to_string()
            }
        ))
    })
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn registry() -> ToolRegistry {
        ToolRegistry::new(false)
    }

    #[test]
    fn unknown_names_and_ungated_teacher_anchor_are_unknown_tool() {
        assert_eq!(
            validate(&registry(), "open_shell", &json!({})),
            Err(ToolOutcome::UnknownTool)
        );
        assert_eq!(
            validate(
                &registry(),
                TEACHER_ANCHOR_TOOL,
                &json!({"kind": "section", "id": "s1"})
            ),
            Err(ToolOutcome::UnknownTool),
            "a session without the live preview rejects teacher_anchor like any hallucinated name"
        );
        assert!(matches!(
            validate(
                &ToolRegistry::new(true),
                TEACHER_ANCHOR_TOOL,
                &json!({"kind": "section", "id": "s1"})
            ),
            Ok(ValidatedCall::TeacherAnchor { .. })
        ));
    }

    #[test]
    fn every_registered_name_validates_or_answers() {
        let registry = ToolRegistry::new(true);
        for name in registry.names() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                validate(&registry, name, &json!({}))
            }));
            assert!(
                result.is_ok(),
                "validate panicked on registered tool: {name}"
            );
        }
    }

    #[test]
    fn parameterless_tools_accept_empty_or_null_args_and_nothing_else() {
        for name in [
            registry::PAUSE_PLAYBACK,
            registry::RESUME_PLAYBACK,
            registry::REPLAY_PREVIOUS_PARAGRAPH,
            registry::REPLAY_PREVIOUS_PAGE,
            registry::REPLAY_LAST_MINUTE,
            registry::SUMMARIZE_SECTION,
        ] {
            assert!(
                validate(&registry(), name, &json!({})).is_ok(),
                "{name}: empty object"
            );
            assert!(
                validate(&registry(), name, &Value::Null).is_ok(),
                "{name}: null args"
            );
            assert!(
                matches!(
                    validate(&registry(), name, &json!({"stray": 1})),
                    Err(ToolOutcome::InvalidArgs { .. })
                ),
                "{name}: unknown field must be rejected"
            );
        }
    }

    #[test]
    fn seek_and_speed_enforce_the_spec_bounds() {
        let ok = validate(
            &registry(),
            registry::SEEK_RELATIVE,
            &json!({"seconds": -30}),
        );
        assert_eq!(ok, Ok(ValidatedCall::SeekRelative { seconds: -30.0 }));
        for bad in [
            json!({"seconds": 601}),
            json!({"seconds": -600.5}),
            json!({}),
        ] {
            assert!(matches!(
                validate(&registry(), registry::SEEK_RELATIVE, &bad),
                Err(ToolOutcome::InvalidArgs { .. })
            ));
        }
        assert_eq!(
            validate(
                &registry(),
                registry::SET_PLAYBACK_SPEED,
                &json!({"speed": 3.0})
            ),
            Ok(ValidatedCall::SetSpeed { speed: 3.0 })
        );
        for bad in [json!({"speed": 0.25}), json!({"speed": "fast"})] {
            assert!(matches!(
                validate(&registry(), registry::SET_PLAYBACK_SPEED, &bad),
                Err(ToolOutcome::InvalidArgs { .. })
            ));
        }
    }

    #[test]
    fn jump_targets_must_be_non_negative_integers() {
        assert_eq!(
            validate(
                &registry(),
                registry::JUMP_TO_CHAPTER,
                &json!({"chapter_index": 4})
            ),
            Ok(ValidatedCall::JumpToChapter { chapter: 4 })
        );
        assert_eq!(
            validate(
                &registry(),
                registry::JUMP_TO_CHAPTER,
                &json!({"chapter": 4})
            ),
            Ok(ValidatedCall::JumpToChapter { chapter: 4 })
        );
        for bad in [
            json!({"chapter_index": -1}),
            json!({"chapter_index": 1.5}),
            json!({"chapter_index": "4"}),
            json!({"chapter": -1}),
            json!({"chapter": 1.5}),
            json!({"chapter": "4"}),
            json!({"page": 4}),
        ] {
            assert!(matches!(
                validate(&registry(), registry::JUMP_TO_CHAPTER, &bad),
                Err(ToolOutcome::InvalidArgs { .. })
            ));
        }
        assert_eq!(
            validate(&registry(), registry::JUMP_TO_PAGE, &json!({"page": 0})),
            Ok(ValidatedCall::JumpToPrinted { page: 0 })
        );
    }

    #[test]
    fn get_context_position_is_optional_but_typed() {
        assert_eq!(
            validate(
                &registry(),
                registry::GET_CONTEXT_AROUND_POSITION,
                &json!({})
            ),
            Ok(ValidatedCall::GetContextAroundPosition { position_ms: None })
        );
        assert_eq!(
            validate(
                &registry(),
                registry::GET_CONTEXT_AROUND_POSITION,
                &json!({"position_ms": 90000})
            ),
            Ok(ValidatedCall::GetContextAroundPosition {
                position_ms: Some(90000)
            })
        );
        assert!(matches!(
            validate(
                &registry(),
                registry::GET_CONTEXT_AROUND_POSITION,
                &json!({"position_ms": -5})
            ),
            Err(ToolOutcome::InvalidArgs { .. })
        ));
    }

    // ---- execution rig: dispatcher against the in-memory playing surface ----

    use crate::voice::excerpt::window::WindowPolicy;
    use crate::voice::tools::playback::InMemoryPlayback;

    /// Two chapters, three printed pages, five paragraph segments:
    /// pages 1: [0,2000), 2: [2000,4000), 3: [4000,5000);
    /// chapters 0: [0,2000), 1: [2000,5000).
    fn selector() -> ExcerptSelector {
        ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 1000, "page one opening"),
                    (1, 1000, 2000, "page one close"),
                    (2, 2000, 3000, "page two opening"),
                    (2, 3000, 4000, "page two close"),
                    (3, 4000, 5000, "page three all"),
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
            selector(),
        )
    }

    fn now() -> Instant {
        Instant::now()
    }

    fn code_of(outcome: &ToolOutcome) -> &str {
        match outcome {
            ToolOutcome::Failed { code, .. } => code,
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// F6 P6 (bookless-tool-honesty): in a standalone session — the
    /// dispatcher flagged standalone over an EMPTY snapshot — the book tools
    /// fail with their usual typed codes and a wording that says no book is
    /// attached, instead of describing a zero-chapter book that does not
    /// exist. Codes are unchanged (navigation honesty operates on them).
    #[test]
    fn standalone_book_tools_fail_typed_with_no_book_wording() {
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::Standalone);
        let mut playback = InMemoryPlayback::at(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(vec![], vec![]),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let message_of = |outcome: &ToolOutcome| match outcome {
            ToolOutcome::Failed { message, .. } => message.clone(),
            other => panic!("expected Failed, got {other:?}"),
        };

        let jump = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 0}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&jump), CODE_UNKNOWN_CHAPTER);
        assert!(
            message_of(&jump).contains("no book is attached"),
            "standalone wording, not a zero-chapter book: {jump:?}"
        );

        let context = dispatcher.dispatch(
            registry::GET_CONTEXT_AROUND_POSITION,
            &json!({"position_ms": 0}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&context), CODE_NO_CONTEXT);
        assert!(
            message_of(&context).contains("no book is attached"),
            "standalone wording: {context:?}"
        );

        let page = dispatcher.dispatch(
            registry::JUMP_TO_PAGE,
            &json!({"page": 3}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&page), CODE_UNKNOWN_PAGE);
        assert!(
            message_of(&page).contains("no book is attached"),
            "standalone wording: {page:?}"
        );
    }

    /// A successful jump answers its navigation fields exactly AND carries
    /// the landing window's book text (investigation 2026-10-09: the tool
    /// answer is the in-turn channel; a jump that returned no text sent the
    /// model to explain a page it never read).
    fn assert_jump_lands(outcome: &ToolOutcome, nav: Value, landing_text: &str) {
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("expected a successful jump, got {outcome:?}");
        };
        for (key, value) in nav.as_object().unwrap() {
            assert_eq!(&payload[key], value, "jump field {key}: {payload}");
        }
        let text = payload["book_text"]
            .as_str()
            .unwrap_or_else(|| panic!("the jump carries the landing text: {payload}"));
        assert!(
            text.contains(landing_text),
            "the landing window holds the target's text {landing_text:?}: {text}"
        );
    }

    #[test]
    fn a_book_direct_read_answers_the_whole_page_and_defers_the_next_read() {
        use crate::voice::tools::playback::BookCursor;
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::BookDirect);
        let mut playback = BookCursor::new(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                (0..20u64)
                    .map(|i| (12, i * 10, i * 10 + 10, "block of page twelve"))
                    .chain([(13, 200, 210, "first block of page thirteen")])
                    .collect(),
                vec![(0, 210)],
            ),
            WindowPolicy {
                before: 2,
                after: 2,
            },
        );
        excerpts.set_page_windows(true);
        let read = dispatcher.dispatch(
            registry::GET_CONTEXT_AROUND_POSITION,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = read else {
            panic!("{read:?}")
        };
        let text = payload["book_text"].as_str().expect("book text");
        assert_eq!(text.matches("block of page twelve").count(), 20, "{text}");
        assert!(!text.contains("page thirteen"), "{text}");
        assert_eq!(payload["continue_at_page"], 13, "{payload}");
        let next = payload["reading_next"].as_str().expect("reading_next");
        assert!(
            next.starts_with("Explain this text to the user first.")
                && next.contains("jump_to_page with page 13"),
            "{next}"
        );
    }

    fn paged_book() -> (
        Dispatcher,
        crate::voice::tools::playback::BookCursor,
        ExcerptSelector,
    ) {
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::BookDirect);
        let excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 100, "page one text"),
                    (2, 100, 200, "page two text"),
                    (4, 200, 300, "page four text"),
                ],
                vec![(0, 300)],
            ),
            WindowPolicy {
                before: 0,
                after: 0,
            },
        );
        (
            dispatcher,
            crate::voice::tools::playback::BookCursor::new(150),
            excerpts,
        )
    }

    fn turn(
        dispatcher: &mut Dispatcher,
        playback: &mut crate::voice::tools::playback::BookCursor,
        excerpts: &mut ExcerptSelector,
        direction: &str,
    ) -> ToolOutcome {
        dispatcher.dispatch(
            registry::TURN_PAGE,
            &json!({ "direction": direction }),
            now(),
            playback,
            excerpts,
        )
    }

    #[test]
    fn turn_page_steps_from_the_current_page_and_skips_pages_without_text() {
        let (mut dispatcher, mut playback, mut excerpts) = paged_book();
        let next = turn(&mut dispatcher, &mut playback, &mut excerpts, "next");
        assert_jump_lands(&next, json!({ "page": 4 }), "page four text");
        assert_eq!(playback.position_ms().unwrap(), 200);
        let back = turn(&mut dispatcher, &mut playback, &mut excerpts, "previous");
        assert_jump_lands(&back, json!({ "page": 2 }), "page two text");
    }

    #[test]
    fn navigation_waits_for_the_surface_before_moving_or_marking_coverage() {
        let (mut dispatcher, mut playback, mut excerpts) = paged_book();
        let before = playback.position_ms().unwrap();
        let coverage_before = excerpts.played_ranges().to_json("offset");
        let prepared = dispatcher
            .prepare_navigation(
                registry::JUMP_TO_PAGE,
                &json!({"page": 4}),
                now(),
                &playback,
                &mut excerpts,
            )
            .unwrap();
        assert_eq!(prepared.position(), 200);
        assert_eq!(playback.position_ms().unwrap(), before);
        assert!(dispatcher.drain_navigation_outcomes().is_empty());
        assert!(dispatcher.drain_tool_outcomes().is_empty());
        assert!(prepared.payload.get("book_text").is_none());
        assert_eq!(excerpts.played_ranges().to_json("offset"), coverage_before);
        let applied = dispatcher.commit_navigation(prepared, now(), &mut playback, &mut excerpts);
        assert_jump_lands(&applied, json!({ "page": 4 }), "page four text");
        assert_eq!(playback.position_ms().unwrap(), 200);
    }

    #[test]
    fn book_direct_context_reads_the_visible_page_without_seeking_the_cursor() {
        let (mut dispatcher, mut playback, mut excerpts) = paged_book();
        dispatcher.set_shown_page(Some(4));
        let result = dispatcher.dispatch(
            registry::GET_CONTEXT_AROUND_POSITION,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert!(
            matches!(result, ToolOutcome::Ok(payload) if payload["book_text"].as_str().is_some_and(|text| text.contains("page four text")))
        );
        assert_eq!(playback.position_ms().unwrap(), 150);
    }

    #[test]
    fn turn_page_steps_from_the_page_the_user_flipped_to() {
        let (mut dispatcher, mut playback, mut excerpts) = paged_book();
        dispatcher.set_shown_page(Some(4));
        let back = turn(&mut dispatcher, &mut playback, &mut excerpts, "previous");
        assert_jump_lands(&back, json!({ "page": 2 }), "page two text");
        let again = turn(&mut dispatcher, &mut playback, &mut excerpts, "previous");
        assert_jump_lands(&again, json!({ "page": 1 }), "page one text");
    }

    #[test]
    fn a_jump_the_reader_did_not_show_keeps_the_shown_page_for_turn_page() {
        let (mut dispatcher, mut playback, mut excerpts) = paged_book();
        dispatcher.set_mode(DispatchMode::Grounded);
        dispatcher.set_shown_page(Some(2));
        let jumped = dispatcher.dispatch(
            registry::JUMP_TO_PAGE,
            &json!({ "page": 4 }),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(jumped, ToolOutcome::Ok(_)), "{jumped:?}");
        let next = turn(&mut dispatcher, &mut playback, &mut excerpts, "next");
        assert_jump_lands(&next, json!({ "page": 4 }), "page four text");
    }

    #[test]
    fn turn_page_past_the_last_page_with_text_is_refused_honestly() {
        let (mut dispatcher, mut playback, mut excerpts) = paged_book();
        dispatcher.set_shown_page(Some(4));
        let over = turn(&mut dispatcher, &mut playback, &mut excerpts, "next");
        assert_eq!(code_of(&over), CODE_UNKNOWN_PAGE);
        assert!(
            matches!(&over, ToolOutcome::Failed { message, .. } if message.contains("page 4 is the last")),
            "{over:?}"
        );
        let invalid = turn(&mut dispatcher, &mut playback, &mut excerpts, "sideways");
        assert!(
            matches!(invalid, ToolOutcome::InvalidArgs { .. }),
            "{invalid:?}"
        );
    }

    /// Field session 2026-10-09 (`01a121e0`): "show me the figure" jumped
    /// past pages the tutor had not taught, and it carried on from the
    /// figure. The jump answer names the skipped pages and the way back;
    /// window answers name the page teaching continues on.
    #[test]
    fn a_detour_jump_names_the_skipped_pages_and_windows_name_the_next_page() {
        use crate::voice::tools::playback::BookCursor;
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::BookDirect);
        let mut playback: BookCursor = BookCursor::new(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (10, 0, 100, "chapter one text"),
                    (
                        20,
                        100,
                        200,
                        "chapter two opens with the architectural styles",
                    ),
                    (21, 200, 300, "page twenty-one explains components"),
                    (22, 300, 400, "page twenty-two explains connectors"),
                    (23, 400, 500, "Figure 2.1 shows the layered style"),
                ],
                vec![(0, 100), (100, 500)],
            ),
            WindowPolicy {
                before: 0,
                after: 0,
            },
        );
        let opening = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(opening) = opening else {
            panic!("{opening:?}")
        };
        assert!(opening.get("reading_order").is_none(), "{opening}");
        assert_eq!(opening["continue_at_page"], 21, "{opening}");
        assert!(
            opening["reading_next"]
                .as_str()
                .is_some_and(|next| next.contains("jump_to_page with page 21")),
            "the answer says how to continue: {opening}"
        );

        let detour = dispatcher.dispatch(
            registry::JUMP_TO_PAGE,
            &json!({"page": 23}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(detour) = detour else {
            panic!("{detour:?}")
        };
        let note = detour["reading_order"]
            .as_str()
            .unwrap_or_else(|| panic!("{detour}"));
        assert!(note.contains("pages 21\u{2013}22"), "{note}");
        assert!(note.contains("jump_to_page with page 21"), "{note}");

        let back = dispatcher.dispatch(
            registry::JUMP_TO_PAGE,
            &json!({"page": 21}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(back) = back else {
            panic!("{back:?}")
        };
        assert!(
            back.get("reading_order").is_none(),
            "going back skips nothing: {back}"
        );
        assert_eq!(back["continue_at_page"], 22);
    }

    /// F7 P7 half + §5 table: in a BOOK-DIRECT session (jobless, offset
    /// snapshot, real [`BookCursor`]) the audio-bound tools refuse typed
    /// with the no-narration wording, while chapter/page navigation and
    /// context windows keep working over the cursor.
    #[test]
    fn book_direct_audio_tools_refuse_typed_while_navigation_works() {
        use crate::voice::tools::playback::BookCursor;
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::BookDirect);
        let mut playback: BookCursor = BookCursor::new(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 20, "alpha offset text"),
                    (2, 20, 45, "bravo offset text"),
                ],
                vec![(0, 20), (20, 45)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let message_of = |outcome: &ToolOutcome| match outcome {
            ToolOutcome::Failed { message, .. } => message.clone(),
            other => panic!("expected Failed, got {other:?}"),
        };

        for (name, args) in [
            (registry::PAUSE_PLAYBACK, json!({})),
            (registry::RESUME_PLAYBACK, json!({})),
            (registry::SET_PLAYBACK_SPEED, json!({"speed": 1.5})),
            (registry::SEEK_RELATIVE, json!({"seconds": 30.0})),
            (registry::REPLAY_LAST_MINUTE, json!({})),
        ] {
            let outcome = dispatcher.dispatch(name, &args, now(), &mut playback, &mut excerpts);
            assert_eq!(code_of(&outcome), CODE_NO_NARRATION, "{name}");
            assert!(
                message_of(&outcome).contains("no narration audio yet"),
                "{name} refusal names the missing narration: {outcome:?}"
            );
        }
        assert_eq!(
            playback.position_ms().unwrap(),
            0,
            "refused audio tools never move the cursor"
        );

        let jump = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_jump_lands(
            &jump,
            json!({
                "position_ms": 20,
                "chapter": 1,
                "chapter_title": "chapter 1",
                "page": 2,
                "status": "The reader's screen now displays this chapter."
            }),
            "bravo offset text",
        );
        assert_eq!(playback.position_ms().unwrap(), 20, "the cursor moved");

        let context = dispatcher.dispatch(
            registry::GET_CONTEXT_AROUND_POSITION,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        match context {
            ToolOutcome::Ok(payload) => assert!(
                payload["book_text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("bravo offset text"),
                "the window follows the cursor: {payload}"
            ),
            other => panic!("context must ground at the cursor: {other:?}"),
        }

        let page = dispatcher.dispatch(
            registry::JUMP_TO_PAGE,
            &json!({"page": 99}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&page), CODE_UNKNOWN_PAGE);
        assert!(
            !message_of(&page).contains("narrated"),
            "book-direct page refusals never claim narration: {page:?}"
        );
        assert!(
            !matches!(&page, ToolOutcome::Ok(payload) if payload.get("book_text").is_some()),
            "a refused page jump carries no text"
        );

        let back = dispatcher.dispatch(
            registry::JUMP_TO_PAGE,
            &json!({"page": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_jump_lands(
            &back,
            json!({
                "position_ms": 0,
                "page": 1,
                "status": "The reader was sent to this page."
            }),
            "alpha offset text",
        );
        let ToolOutcome::Ok(landing) = &back else {
            unreachable!("asserted above")
        };
        assert!(
            landing["book_text"]
                .as_str()
                .is_some_and(|text| text.contains("[page 1]\nalpha offset text")),
            "the landing text names its page, so the tutor knows where it is: {landing}"
        );
    }

    /// F7 P8 (K10 fix): `seek_relative` over an EMPTY snapshot refuses typed
    /// instead of silently clamping to 0 and answering success — in both the
    /// grounded and standalone wordings.
    #[test]
    fn seek_relative_on_an_empty_snapshot_refuses_instead_of_seeking_to_zero() {
        let empty_selector = || {
            ExcerptSelector::new(
                SessionSnapshot::from_voice_test_parts(vec![], vec![]),
                WindowPolicy {
                    before: 1,
                    after: 1,
                },
            )
        };
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        let mut playback = InMemoryPlayback::at(2500);
        let mut excerpts = empty_selector();
        let outcome = dispatcher.dispatch(
            registry::SEEK_RELATIVE,
            &json!({"seconds": -30.0}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&outcome), CODE_NO_CONTEXT);
        assert_eq!(
            playback.position_ms, 2500,
            "the surface never moved — no silent seek-to-0"
        );

        let mut standalone = Dispatcher::new(ToolRegistry::new(false));
        standalone.set_mode(DispatchMode::Standalone);
        let mut excerpts = empty_selector();
        let outcome = standalone.dispatch(
            registry::SEEK_RELATIVE,
            &json!({"seconds": 30.0}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&outcome), CODE_NO_CONTEXT);
        match &outcome {
            ToolOutcome::Failed { message, .. } => assert!(
                message.contains("no book is attached"),
                "standalone wording: {message}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// F6 P5 guard for the wording fork: a GROUNDED dispatcher over the same
    /// empty snapshot keeps its pre-F6 messages byte-for-byte.
    #[test]
    fn grounded_empty_snapshot_messages_are_unchanged() {
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        let mut playback = InMemoryPlayback::at(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(vec![], vec![]),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_AROUND_POSITION,
            &json!({"position_ms": 0}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        match &outcome {
            ToolOutcome::Failed { code, message } => {
                assert_eq!(code, CODE_NO_CONTEXT);
                assert_eq!(message, "the book has no narrated segments");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn pause_and_resume_drive_the_playback_port() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        let outcome = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(outcome, ToolOutcome::Ok(json!({ "paused": true })));
        assert!(playback.paused);
        dispatcher.dispatch(
            registry::RESUME_PLAYBACK,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert!(!playback.paused);
    }

    #[test]
    fn seek_relative_clamps_to_the_book_timeline_on_both_ends() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        // 2500 − 600000 ms clamps to 0; 2500 + 600000 clamps to end (5000).
        let outcome = dispatcher.dispatch(
            registry::SEEK_RELATIVE,
            &json!({"seconds": -600}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(outcome, ToolOutcome::Ok(json!({ "position_ms": 0 })));
        assert_eq!(playback.position_ms, 0);
        playback.position_ms = 2500;
        dispatcher.dispatch(
            registry::SEEK_RELATIVE,
            &json!({"seconds": 600}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, 5000);
    }

    #[test]
    fn the_replay_family_computes_targets_from_the_snapshot() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        // At 2500 (segment 2): previous paragraph starts at 1000.
        dispatcher.dispatch(
            registry::REPLAY_PREVIOUS_PARAGRAPH,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, 1000);
        // At 3500 (page 2): the previous printed page starts at 0.
        playback.position_ms = 3500;
        dispatcher.dispatch(
            registry::REPLAY_PREVIOUS_PAGE,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, 0);
        // On the first page already: clamp to the start of the book.
        playback.position_ms = 1500;
        dispatcher.dispatch(
            registry::REPLAY_PREVIOUS_PAGE,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, 0);
        // Replay-last-minute saturates at the top of the book.
        playback.position_ms = 2500;
        dispatcher.dispatch(
            registry::REPLAY_LAST_MINUTE,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, 0);
    }

    #[test]
    fn jumps_resolve_against_the_snapshot_and_miss_loudly() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, 2000);
        let missing = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 9}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&missing), CODE_UNKNOWN_CHAPTER);
        dispatcher.dispatch(
            registry::JUMP_TO_PAGE,
            &json!({"page": 3}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, 4000);
        let missing = dispatcher.dispatch(
            registry::JUMP_TO_PAGE,
            &json!({"page": 7}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&missing), CODE_UNKNOWN_PAGE);
    }

    #[test]
    fn jump_to_chapter_deterministic_index_execution() {
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        let mut playback = InMemoryPlayback::at(0);
        let snapshot = SessionSnapshot::from_voice_test_parts(
            vec![
                (1, 0, 1000, "cover text"),
                (2, 1000, 2000, "table of contents"),
                (15, 2000, 4000, "chapter one content"),
            ],
            vec![(0, 1000), (1000, 2000), (2000, 4000)],
        )
        .with_test_chapter_titles(vec![
            "Cover".to_string(),
            "Contents".to_string(),
            "Chapter 1 A Tour of Systems".to_string(),
        ]);
        let mut excerpts = ExcerptSelector::new(
            snapshot,
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );

        // chapter_index: 2 passed -> deterministically lands on index 2 ("Chapter 1 A Tour of Systems")
        let outcome = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter_index": 2}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, 2000);
        let ToolOutcome::Ok(val) = outcome else {
            panic!("expected Ok, got {outcome:?}");
        };
        assert_eq!(val["chapter"], 2);
        assert_eq!(val["chapter_title"], "Chapter 1 A Tour of Systems");

        // Backward compatibility: legacy "chapter": 1 passed -> deterministically lands on index 1 ("Contents")
        let outcome_legacy = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(playback.position_ms, 1000);
        let ToolOutcome::Ok(val_legacy) = outcome_legacy else {
            panic!("expected Ok, got {outcome_legacy:?}");
        };
        assert_eq!(val_legacy["chapter"], 1);
        assert_eq!(val_legacy["chapter_title"], "Contents");

        // chapter_index 10 passed: index 10 is out of bounds (only 3 chapters) -> returns unknown chapter.
        let missing = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter_index": 10}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&missing), CODE_UNKNOWN_CHAPTER);
    }

    /// Dossier 2026-08-31 D3: every jump outcome — applied or refused — is
    /// recorded for the session layer, in call order, and drained exactly
    /// once. Non-navigation tools leave no record.
    #[test]
    fn jump_outcomes_are_recorded_for_the_session_layer() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        dispatcher.dispatch(
            registry::JUMP_TO_PAGE,
            &json!({"page": 7}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let outcomes = dispatcher.drain_navigation_outcomes();
        assert_eq!(outcomes.len(), 2, "jumps only, in call order: {outcomes:?}");
        assert_eq!(outcomes[0].tool, registry::JUMP_TO_CHAPTER);
        assert_eq!(outcomes[0].result, Ok(2000));
        assert_eq!(outcomes[1].tool, registry::JUMP_TO_PAGE);
        let Err((code, message)) = &outcomes[1].result else {
            panic!("the missing page records as a refusal: {outcomes:?}");
        };
        assert_eq!(code, CODE_UNKNOWN_PAGE);
        assert!(message.contains('7'), "the refusal names the target");
        assert!(
            dispatcher.drain_navigation_outcomes().is_empty(),
            "drained exactly once"
        );
    }

    #[test]
    fn seek_relative_records_navigation_outcome() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        dispatcher.dispatch(
            registry::SEEK_RELATIVE,
            &json!({"seconds": 1.0}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let outcomes = dispatcher.drain_navigation_outcomes();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].tool, registry::SEEK_RELATIVE);
        assert_eq!(outcomes[0].result, Ok(3500));
    }

    #[test]
    fn tier_zero_reads_egress_only_delimited_capped_excerpts() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        // Before any window is injected, summarize has nothing to scope to.
        let early = dispatcher.dispatch(
            registry::SUMMARIZE_SECTION,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&early), CODE_NO_CONTEXT);

        excerpts.advance_to(2500); // harness injects the window around playback
        let ToolOutcome::Ok(payload) = dispatcher.dispatch(
            registry::SUMMARIZE_SECTION,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        ) else {
            panic!("summarize with a window must succeed");
        };
        let text = payload["book_text"].as_str().expect("delimited text");
        assert!(
            text.contains("page two opening"),
            "the window's anchor text"
        );
        assert!(
            text.lines().next().unwrap() != "page one close",
            "payload is sentinel-delimited, not raw book text"
        );

        // get_context defaults to the playback position; explicit position
        // resolves its own window through the same egress path.
        let ToolOutcome::Ok(payload) = dispatcher.dispatch(
            registry::GET_CONTEXT_AROUND_POSITION,
            &json!({"position_ms": 4500}),
            now(),
            &mut playback,
            &mut excerpts,
        ) else {
            panic!("in-book position must resolve");
        };
        assert!(payload["book_text"]
            .as_str()
            .unwrap()
            .contains("page three all"));
    }

    #[test]
    fn a_torn_down_playing_surface_is_answered_not_panicked() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        playback.available = false;
        for (name, args) in [
            (registry::PAUSE_PLAYBACK, json!({})),
            (registry::SEEK_RELATIVE, json!({"seconds": 5})),
            (registry::GET_CONTEXT_AROUND_POSITION, json!({})),
        ] {
            let outcome = dispatcher.dispatch(name, &args, now(), &mut playback, &mut excerpts);
            assert_eq!(code_of(&outcome), CODE_PLAYBACK_UNAVAILABLE, "{name}");
        }
    }

    #[test]
    fn the_tier_one_budget_is_five_per_ten_seconds_and_spares_tier_zero() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        excerpts.advance_to(2500);
        let start = Instant::now();
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
                "call {i} within budget"
            );
        }
        let sixth = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&sixth), CODE_RATE_LIMITED);
        // Tier-0 reads are not limited (§8.1 rates Tier-1 mutations only).
        let read = dispatcher.dispatch(
            registry::SUMMARIZE_SECTION,
            &json!({}),
            start,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(read, ToolOutcome::Ok(_)));
        // The window slides: the same call is in budget again 10 s later.
        let later = dispatcher.dispatch(
            registry::PAUSE_PLAYBACK,
            &json!({}),
            start + TIER_ONE_WINDOW,
            &mut playback,
            &mut excerpts,
        );
        assert!(matches!(later, ToolOutcome::Ok(_)));
    }

    // ---- channel wiring: ToolCall event → send_tool_result round trip ----

    use crate::voice::channel::mock::{MockChannel, MockDriver};
    use crate::voice::channel::types::{DelimitedExcerpt, SessionContext};

    async fn connected_mock() -> (MockChannel, MockDriver) {
        let (mut channel, driver) = MockChannel::builder().build();
        channel
            .connect(SessionContext {
                book_id: Some("wiring-book".to_string()),
                session_id: "wiring-session".to_string(),
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
    async fn every_tool_call_gets_exactly_one_answer_on_the_channel() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        excerpts.advance_to(2500);
        let (mut channel, driver) = connected_mock().await;
        // (call_id, name, args) straight off ChannelEvent::ToolCall's fields:
        // a good call, a hallucinated name, a schema break, and a Tier-0 read
        // — four calls in, four answers out, in order.
        let calls = [
            ("call-1", registry::PAUSE_PLAYBACK, json!({})),
            ("call-2", "open_shell", json!({})),
            ("call-3", registry::SEEK_RELATIVE, json!({"seconds": 9000})),
            ("call-4", registry::SUMMARIZE_SECTION, json!({})),
        ];
        for (call_id, name, args) in &calls {
            answer_tool_call(
                &mut dispatcher,
                &mut channel,
                call_id,
                name,
                args,
                now(),
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
            vec!["call-1", "call-2", "call-3", "call-4"],
            "one answer per call, same ids, same order"
        );
        assert_eq!(answered[0].1, ToolResult::Ok(json!({ "paused": true })));
        assert!(matches!(
            &answered[1].1,
            ToolResult::Err { code, .. } if code == CODE_UNKNOWN_TOOL
        ));
        assert!(matches!(
            &answered[2].1,
            ToolResult::Err { code, .. } if code == CODE_INVALID_ARGS
        ));
        let ToolResult::Ok(payload) = &answered[3].1 else {
            panic!("the Tier-0 read must succeed with a window in place");
        };
        assert!(payload["book_text"]
            .as_str()
            .expect("delimited excerpt")
            .contains("page two opening"));
    }

    #[tokio::test]
    async fn a_playback_failure_is_answered_on_the_channel_not_swallowed() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        playback.available = false;
        let (mut channel, driver) = connected_mock().await;
        answer_tool_call(
            &mut dispatcher,
            &mut channel,
            "call-9",
            registry::RESUME_PLAYBACK,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        )
        .await
        .expect("the channel send itself succeeds");
        let answered = driver.tool_results();
        assert!(matches!(
            &answered[0].1,
            ToolResult::Err { code, .. } if code == CODE_PLAYBACK_UNAVAILABLE
        ));
    }

    // ---- U3 (H2): Tier-0 egress under the per-provider token budget ----

    fn word_policy(budget: usize) -> TruncationPolicy {
        TruncationPolicy::new(budget, |text: &str| text.split_whitespace().count())
    }

    /// Every segment in [`selector`] is 3 words, so the (1,1) window at
    /// 2500 ms (segments 1..=3) counts 9 tokens; budget 5 trims one segment
    /// off each edge, keeping the 3-token anchor.
    #[test]
    fn tier_zero_reads_respect_the_installed_token_budget_and_say_so_twice() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        dispatcher.set_truncation_policy(word_policy(5));
        excerpts.advance_to(2500);
        for (name, args) in [
            (registry::SUMMARIZE_SECTION, json!({})),
            (
                registry::GET_CONTEXT_AROUND_POSITION,
                json!({"position_ms": 2500}),
            ),
        ] {
            let ToolOutcome::Ok(payload) =
                dispatcher.dispatch(name, &args, now(), &mut playback, &mut excerpts)
            else {
                panic!("{name}: the budgeted read still answers Ok");
            };
            let keys: Vec<&str> = payload
                .as_object()
                .expect("object payload")
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                vec!["book_text", "coverage", "truncation"],
                "{name}: a fired token budget rides the payload — silent trimming is H2"
            );
            assert_eq!(
                payload["truncation"],
                json!({
                    "budget_tokens": 5,
                    "window_tokens": 9,
                    "dropped_leading_segments": 1,
                    "dropped_trailing_segments": 1,
                    "anchor_partially_cut": false,
                }),
                "{name}: exact loud arithmetic"
            );
            let text = payload["book_text"].as_str().expect("delimited text");
            assert!(text.contains("page two opening"), "{name}: anchor kept");
            assert!(
                !text.contains("page one close") && !text.contains("page two close"),
                "{name}: both edges trimmed to the budget"
            );
            assert_eq!(
                dispatcher.drain_egress_events(),
                vec![EgressEvent::ExcerptTruncated {
                    budget_tokens: 5,
                    window_tokens: 9,
                    dropped_leading_segments: 1,
                    dropped_trailing_segments: 1,
                    anchor_partially_cut: false,
                }],
                "{name}: the same fact reaches the session event stream"
            );
            assert!(
                dispatcher.drain_egress_events().is_empty(),
                "{name}: draining consumes"
            );
        }
    }

    #[test]
    fn a_window_within_the_budget_is_untouched_and_reports_nothing() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        dispatcher.set_truncation_policy(word_policy(15));
        excerpts.advance_to(2500);
        let ToolOutcome::Ok(payload) = dispatcher.dispatch(
            registry::SUMMARIZE_SECTION,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        ) else {
            panic!("summarize with a window must succeed");
        };
        let keys: Vec<&str> = payload
            .as_object()
            .expect("object payload")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["book_text", "coverage"],
            "under the budget the payload is the delimited text plus the F8 D1 \
             coverage line and NOTHING else (pin6's exact-keys shape holds on \
             the budgeted path)"
        );
        assert!(dispatcher.drain_egress_events().is_empty());
    }

    #[test]
    fn the_hard_byte_backstop_stays_loud_underneath_the_budgeted_path() {
        // A token budget too permissive to trim (one flat token for any text)
        // over a segment past 64 KiB: the pin6 backstop must fire with its
        // rider intact even though the read went through the budgeted path.
        const CAP: usize = 64 * 1024;
        let oversized = "a".repeat(CAP + 10);
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_truncation_policy(TruncationPolicy::new(1, |_: &str| 1));
        let mut playback = InMemoryPlayback::at(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![(1, 0, 1000, oversized.as_str())],
                vec![(0, 1000)],
            ),
            WindowPolicy {
                before: 0,
                after: 0,
            },
        );
        excerpts.advance_to(0);
        let ToolOutcome::Ok(payload) = dispatcher.dispatch(
            registry::SUMMARIZE_SECTION,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        ) else {
            panic!("the capped read still answers Ok");
        };
        assert_eq!(
            payload["hard_cap"],
            json!({"kept_bytes": CAP, "dropped_bytes": 10}),
            "the backstop rider survives on the budgeted path"
        );
        assert!(
            payload.get("truncation").is_none(),
            "the token budget saw one token; only the backstop fired"
        );
        assert_eq!(
            dispatcher.drain_egress_events(),
            vec![EgressEvent::HardByteCapApplied {
                kept_bytes: CAP,
                dropped_bytes: 10,
            }]
        );
    }

    #[tokio::test]
    async fn a_failed_send_preserves_the_outcome_and_the_retry_never_reexecutes() {
        use crate::voice::channel::types::ErrorKind;

        let (mut dispatcher, mut playback, mut excerpts) = rig();
        let (mut channel, driver) = connected_mock().await;
        // seek_relative is the H1 poster child: re-executing it on retry
        // would apply the relative offset twice for one model call.
        driver.fail_next_send(ChannelError::new(ErrorKind::Transport, true, "ws drop"));
        answer_tool_call(
            &mut dispatcher,
            &mut channel,
            "seek-1",
            registry::SEEK_RELATIVE,
            &json!({"seconds": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        )
        .await
        .expect_err("the armed transport failure surfaces");
        assert_eq!(playback.position_ms, 3500, "the effect ran exactly once");
        assert_eq!(dispatcher.pending_answer_count(), 1, "the answer is kept");

        answer_tool_call(
            &mut dispatcher,
            &mut channel,
            "seek-1",
            registry::SEEK_RELATIVE,
            &json!({"seconds": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        )
        .await
        .expect("the retry lands");
        assert_eq!(
            playback.position_ms, 3500,
            "the retry re-sent the cached answer; it did not seek again"
        );
        let answered = driver.tool_results();
        assert_eq!(answered.len(), 1);
        assert_eq!(
            answered[0],
            (
                "seek-1".to_string(),
                ToolResult::Ok(json!({ "position_ms": 3500 }))
            ),
            "the ack carries the outcome computed before the failed send"
        );
        assert_eq!(dispatcher.pending_answer_count(), 0, "settled on success");
    }

    #[tokio::test]
    async fn a_settled_call_id_reused_by_a_new_provider_event_executes_fresh() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        let (mut channel, driver) = connected_mock().await;
        // Two distinct provider events colliding on id (pin3's `dup` case):
        // the first settles on its successful send, so the second is a new
        // call that must execute, not a replay of the cached answer.
        for expected_paused in [true, false] {
            let name = if expected_paused {
                registry::PAUSE_PLAYBACK
            } else {
                registry::RESUME_PLAYBACK
            };
            answer_tool_call(
                &mut dispatcher,
                &mut channel,
                "dup",
                name,
                &json!({}),
                now(),
                &mut playback,
                &mut excerpts,
            )
            .await
            .expect("send succeeds");
            assert_eq!(playback.paused, expected_paused);
        }
        let answered = driver.tool_results();
        assert_eq!(
            answered,
            vec![
                ("dup".to_string(), ToolResult::Ok(json!({ "paused": true }))),
                (
                    "dup".to_string(),
                    ToolResult::Ok(json!({ "paused": false }))
                ),
            ],
            "both events answered with their own outcomes — no dedupe, no replay"
        );
    }

    #[tokio::test]
    async fn the_pending_answer_cache_is_bounded_to_the_session_cap() {
        use crate::voice::channel::types::ErrorKind;

        let (mut dispatcher, mut playback, mut excerpts) = rig();
        let (mut channel, driver) = connected_mock().await;
        for i in 0..(PENDING_ANSWER_CAP + 8) {
            driver.fail_next_send(ChannelError::new(ErrorKind::Transport, true, "down"));
            let call_id = format!("lost-{i}");
            answer_tool_call(
                &mut dispatcher,
                &mut channel,
                &call_id,
                registry::GET_CONTEXT_AROUND_POSITION,
                &json!({"position_ms": 2500}),
                now(),
                &mut playback,
                &mut excerpts,
            )
            .await
            .expect_err("every send is armed to fail");
        }
        assert_eq!(
            dispatcher.pending_answer_count(),
            PENDING_ANSWER_CAP,
            "a dead channel must not grow the cache without bound"
        );
    }

    #[test]
    fn teacher_anchor_reaching_the_dispatcher_is_a_wiring_bug_answered_as_error() {
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(true));
        let (_, mut playback, mut excerpts) = rig();
        let outcome = dispatcher.dispatch(
            TEACHER_ANCHOR_TOOL,
            &json!({"kind": "section", "id": "s1"}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&outcome), CODE_UNROUTED_TOOL);
    }

    // ---- update_learning_profile (owner ruling R1) ----

    /// A recording fake for the sink seam; `refuse` exercises the sink-side
    /// refusal path (e.g. the entry cap).
    struct RecordingProfile {
        updates: Vec<(String, String)>,
        refuse: bool,
    }

    impl super::super::learning::LearningProfileSink for RecordingProfile {
        fn update(&mut self, key: &str, value: &str) -> Result<(), ProfileRefused> {
            if self.refuse {
                return Err(ProfileRefused::new("the learning profile is full"));
            }
            self.updates.push((key.to_string(), value.to_string()));
            Ok(())
        }
    }

    use super::super::learning::ProfileRefused;

    #[test]
    fn a_profile_update_validates_strictly_and_reaches_the_sink() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        dispatcher.set_profile_sink(Box::new(RecordingProfile {
            updates: Vec::new(),
            refuse: false,
        }));

        let ok = dispatcher.dispatch(
            registry::UPDATE_LEARNING_PROFILE,
            &json!({"key": "equations", "value": "goal first"}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(
            ok,
            ToolOutcome::Ok(json!({
                "recorded": true,
                "key": "equations",
                "apply": "Follow this from now on: equations: goal first",
            })),
            "an accepted update answers recorded and tells the tutor to apply it now"
        );
        assert_eq!(
            dispatcher.saved_preferences(),
            &[("equations".to_string(), "goal first".to_string())]
        );

        for bad in [
            json!({"key": "", "value": "v"}),
            json!({"key": "k".repeat(49), "value": "v"}),
            json!({"key": "k", "value": "v".repeat(201)}),
            json!({"key": "k", "value": "v", "extra": true}),
            json!({"key": 7, "value": "v"}),
        ] {
            let outcome = dispatcher.dispatch(
                registry::UPDATE_LEARNING_PROFILE,
                &bad,
                now(),
                &mut playback,
                &mut excerpts,
            );
            assert!(
                matches!(outcome, ToolOutcome::InvalidArgs { .. }),
                "§5 strict schema on {bad}, got {outcome:?}"
            );
        }
    }

    #[test]
    fn a_profile_call_without_a_sink_or_refused_by_it_fails_loudly_and_free() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        let args = json!({"key": "pace", "value": "slower"});

        let unwired = dispatcher.dispatch(
            registry::UPDATE_LEARNING_PROFILE,
            &args,
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(
            code_of(&unwired),
            CODE_PROFILE_UNAVAILABLE,
            "no sink installed = loud refusal, never a silent drop"
        );

        dispatcher.set_profile_sink(Box::new(RecordingProfile {
            updates: Vec::new(),
            refuse: true,
        }));
        let refused = dispatcher.dispatch(
            registry::UPDATE_LEARNING_PROFILE,
            &args,
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&refused), CODE_PROFILE_REFUSED);

        // H3: refusals never charge the Tier-1 window — five successful
        // seeks still fit afterwards.
        dispatcher.set_profile_sink(Box::new(RecordingProfile {
            updates: Vec::new(),
            refuse: false,
        }));
        let base = now();
        for at in 0..TIER_ONE_MAX_CALLS {
            let outcome = dispatcher.dispatch(
                registry::UPDATE_LEARNING_PROFILE,
                &json!({"key": format!("k{at}"), "value": "v"}),
                base,
                &mut playback,
                &mut excerpts,
            );
            assert!(
                matches!(outcome, ToolOutcome::Ok(_)),
                "refusals above cost no budget; success {at} fits"
            );
        }
        let over = dispatcher.dispatch(
            registry::UPDATE_LEARNING_PROFILE,
            &json!({"key": "k5", "value": "v"}),
            base,
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(
            code_of(&over),
            CODE_RATE_LIMITED,
            "the profile tool is Tier-1 and shares the §8.1 window"
        );
    }

    /// F8 D1: a grounded window FETCH is not played audio — the payload's
    /// coverage line must say so, and fetching must never mark coverage
    /// (only the runtime's playback sampler feeds the grounded accumulator).
    #[test]
    fn a_grounded_context_fetch_claims_no_coverage_and_marks_none() {
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::Grounded);
        let mut playback = InMemoryPlayback::at(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 20_000, "alpha narrated text"),
                    (2, 20_000, 45_000, "bravo narrated text"),
                ],
                vec![(0, 20_000), (20_000, 45_000)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        for _ in 0..2 {
            let outcome = dispatcher.dispatch(
                registry::GET_CONTEXT_AROUND_POSITION,
                &json!({"position_ms": 25_000}),
                now(),
                &mut playback,
                &mut excerpts,
            );
            let ToolOutcome::Ok(payload) = outcome else {
                panic!("context fetch answers Ok, got {outcome:?}");
            };
            let coverage = payload["coverage"].as_str().expect("coverage line rides");
            assert!(
                coverage.contains("Played DURING THIS SESSION: none"),
                "a fetch is not playback and must not claim coverage: {coverage}"
            );
        }
        assert!(
            excerpts.played_ranges().is_empty(),
            "grounded fetches must never feed the accumulator"
        );
    }

    /// F8 D1 BookDirect coverage feed: rendering a window at the cursor IS
    /// the visit — the payload's coverage line reports it in offsets, and
    /// the accumulator holds exactly the rendered span.
    #[test]
    fn a_book_direct_context_fetch_records_the_rendered_window_as_visited() {
        use crate::voice::tools::playback::BookCursor;
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::BookDirect);
        let mut playback: BookCursor = BookCursor::new(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 20, "alpha offset text"),
                    (2, 20, 45, "bravo offset text"),
                ],
                vec![(0, 20), (20, 45)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_AROUND_POSITION,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("context fetch answers Ok, got {outcome:?}");
        };
        let coverage = payload["coverage"].as_str().expect("coverage line rides");
        assert!(
            coverage.contains("offset"),
            "book-direct positions render as offsets: {coverage}"
        );
        assert!(
            coverage.contains("Played ranges: offset 0\u{2013}offset 45"),
            "the rendered window (both segments around the cursor) is the \
             visited span: {coverage}"
        );
        assert_eq!(
            excerpts.played_ranges().ranges(),
            &[(0, 45)],
            "the accumulator holds exactly the rendered span"
        );
    }

    // ---- F8-D1b pins: the bounded-range fetch (`get_context_for_range`) ----
    //
    // Owner defect D-QA1: "key points from minute 0 to 32" must be ONE tool
    // round-trip over real snapshot text, with played-coverage honesty.

    /// D1b-P1 (grounded half): an in-played explicit range returns the exact
    /// snapshot text across it, complete, with `range_fully_played` true.
    #[test]
    fn d1b_a_bounded_in_played_range_returns_exact_snapshot_text_grounded() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        excerpts.note_played(0, 5000);
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": 3000}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("bounded fetch answers Ok, got {outcome:?}");
        };
        let text = payload["book_text"].as_str().expect("book text rides");
        assert!(
            text.contains("page one opening\n\npage one close\n\npage two opening"),
            "the exact snapshot text across [0, 3000) rides verbatim: {text}"
        );
        assert!(
            !text.contains("page two close") && !text.contains("page three"),
            "nothing beyond the requested range leaks: {text}"
        );
        assert_eq!(payload["complete"], json!(true));
        assert_eq!(payload["range_fully_played"], json!(true));
        assert_eq!(payload["requested_range"], json!("0:00–0:03"));
        assert!(
            payload.get("unplayed_gaps").is_none(),
            "a fully played range names no gaps"
        );
        assert!(
            payload["coverage"].as_str().is_some(),
            "the D1 coverage line rides every Tier-0 payload"
        );
        assert_eq!(
            excerpts.played_ranges().ranges(),
            &[(0, 5000)],
            "a grounded fetch is not playback: the accumulator is untouched"
        );
    }

    /// F10-D1 pin (owner device: narration at 22:23 of a 55:28 timeline,
    /// NOTHING played since this voice session began): "summarize from
    /// minute zero to where we are" is ONE `get_context_for_range` call that
    /// answers Ok with real text — never a refusal, never a silent cut — with
    /// both explicit bounds and with the bounds omitted. Gap honesty rides
    /// (the whole span is unplayed THIS session) and the coverage line never
    /// carries the session-accounting sentence the device relayed verbatim.
    #[test]
    fn f10_d1_zero_to_narration_position_on_a_55_minute_timeline_returns_text_grounded() {
        const POS: u64 = 1_343_000; // 22:23
        const END: u64 = 3_328_000; // 55:28
                                    // A long grounded book: 13 segments across two chapters, spanning
                                    // the whole 55:28 timeline, so [0, 22:23) covers several segments.
        let mut segments = Vec::new();
        let mut start = 0;
        let mut idx: i64 = 0;
        while start < END {
            let end = (start + 256_000).min(END);
            segments.push((idx / 4 + 1, start, end, format!("segment {idx} text")));
            start = end;
            idx += 1;
        }
        let segments: Vec<(i64, u64, u64, &str)> = segments
            .iter()
            .map(|(p, s, e, t)| (*p, *s, *e, t.as_str()))
            .collect();
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(segments, vec![(0, 60_000), (60_000, END)]),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        let mut playback = InMemoryPlayback::at(POS);

        let text_of = |payload: &Value| -> String {
            payload["book_text"]
                .as_str()
                .or_else(|| payload["first_chunk"]["book_text"].as_str())
                .expect("real book text rides, whole or as first chunk")
                .to_string()
        };
        for args in [json!({"start_position": 0, "end_position": POS}), json!({})] {
            let outcome = dispatcher.dispatch(
                registry::GET_CONTEXT_FOR_RANGE,
                &args,
                now(),
                &mut playback,
                &mut excerpts,
            );
            let ToolOutcome::Ok(payload) = outcome else {
                panic!("the device-shaped recap fetch answers Ok for {args}, got {outcome:?}");
            };
            let text = text_of(&payload);
            assert!(
                text.contains("segment 0 text"),
                "the fetch starts at minute zero: {text}"
            );
            assert!(
                !text.contains("segment 12 text"),
                "nothing past the narration position leaks: {text}"
            );
            assert_eq!(payload["requested_range"], json!("0:00–22:23"), "{args}");
            assert_eq!(
                payload["range_fully_played"],
                json!(false),
                "nothing played this session — honesty rides the payload"
            );
            assert_eq!(payload["unplayed_gaps"], json!(["0:00–22:23"]));
            let coverage = payload["coverage"].as_str().expect("coverage line rides");
            assert!(
                coverage.contains("narration position 22:23 of 55:28"),
                "{coverage}"
            );
            assert!(
                !coverage.contains("may have listened before")
                    && !coverage.contains("only tracks playback since it started"),
                "the accounting sentence the device relayed is gone from the model-facing line: {coverage}"
            );
        }
        let omitted = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = omitted else {
            panic!("{omitted:?}")
        };
        assert_eq!(payload["range_source"], json!("narration_so_far"));
    }

    /// F10-D1 round-2 pin (realistic payload pressure): the same device
    /// shape — narration at 22:23 of 55:28, nothing played this session —
    /// but with real text density (hardware/eta.rs WORDS_PER_AUDIO_SECOND
    /// = 2.6 → 22 min ≈ 3.5k words) and the PRODUCTION Gemini truncation
    /// policy installed. The bounded fetch must answer Ok with the LOUD
    /// Split shape (complete=false, first_chunk, last_chunk, omitted count),
    /// and the follow-up strategy the prompt law prescribes — fetch the
    /// omitted middle with narrower interior ranges — must succeed on the
    /// same rig. Never a refusal, never silent truncation.
    #[test]
    fn f10_d1_realistic_density_recap_splits_loudly_and_interior_fetches_complete_it() {
        use crate::voice_session::{truncation_policy, SessionProvider};
        const POS: u64 = 1_343_000; // 22:23
        const END: u64 = 3_328_000; // 55:28
        const SEGMENT_MS: u64 = 10_000; // ten seconds of narration per segment
                                        // 25 words + the seg marker = 26 words per 10 s segment = 2.6
                                        // words/audio-second.
        const SENTENCE: &str = "the replica applies each log record in order so that every \
                                follower converges on the same state as the leader once the \
                                backlog has drained";
        let texts: Vec<String> = (0..END.div_ceil(SEGMENT_MS))
            .map(|idx| format!("seg{idx} {SENTENCE}"))
            .collect();
        let parts: Vec<(i64, u64, u64, &str)> = texts
            .iter()
            .enumerate()
            .map(|(idx, text)| {
                let start = idx as u64 * SEGMENT_MS;
                (
                    idx as i64 / 30 + 1,
                    start,
                    (start + SEGMENT_MS).min(END),
                    text.as_str(),
                )
            })
            .collect();
        let total_segments = parts.len();
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(parts, vec![(0, 60_000), (60_000, END)]),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_truncation_policy(truncation_policy(SessionProvider::GeminiLive));
        let mut playback = InMemoryPlayback::at(POS);

        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": POS}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!(
                "a 22-minute recap fetch under the production budget answers Ok, got {outcome:?}"
            );
        };
        assert_eq!(payload["complete"], json!(false), "{payload}");
        let head = payload["first_chunk"]["book_text"]
            .as_str()
            .expect("first chunk rides");
        let tail = payload["last_chunk"]["book_text"]
            .as_str()
            .expect("last chunk rides");
        assert!(
            head.contains("seg0 ") && !head.contains("seg134 "),
            "the head opens at minute zero and is not the whole range: {head}"
        );
        assert!(
            tail.contains("seg134 ") && !tail.contains("seg135 "),
            "the tail ends at the segment holding 22:23, nothing past it: {tail}"
        );
        let omitted = payload["omitted_middle_segments"]
            .as_u64()
            .expect("omitted count rides");
        let in_range = payload["total_segments_in_range"]
            .as_u64()
            .expect("total rides");
        assert!(omitted > 0 && omitted < in_range, "{payload}");
        assert_eq!(in_range, 135, "segments 0..=134 cover [0, 22:23)");
        assert!(in_range < total_segments as u64);
        assert!(
            payload["note"]
                .as_str()
                .unwrap_or("")
                .contains("`omitted_range`"),
            "the split is announced, never silent: {payload}"
        );
        assert_eq!(payload["range_fully_played"], json!(false));
        assert_eq!(payload["unplayed_gaps"], json!(["0:00–22:23"]));

        // The law's follow-up: an interior stretch of the omitted middle,
        // narrow enough for the budget, arrives whole.
        let interior = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 600_000, "end_position": 660_000}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(interior) = interior else {
            panic!("{interior:?}")
        };
        assert_eq!(interior["complete"], json!(true), "{interior}");
        let text = interior["book_text"].as_str().expect("whole interior text");
        assert!(
            text.contains("seg60 ") && text.contains("seg65 ") && !text.contains("seg59 "),
            "{text}"
        );
    }

    /// F10-D1 round-3 pin (payload-driven completion): from the owner-shaped
    /// 0 → 22:23 Split under the production Gemini budget, follow ONLY the
    /// fields the model receives — `range` on every complete piece,
    /// `omitted_range` on every Split — until the union of complete pieces
    /// covers the requested range. No fixture-known intervals. The loop is
    /// bounded: the call count is asserted and recorded here.
    #[test]
    fn f10_d1_split_payloads_are_completed_by_following_omitted_range_only() {
        use crate::voice_session::{truncation_policy, SessionProvider};
        const POS: u64 = 1_343_000;
        const END: u64 = 3_328_000;
        const SEGMENT_MS: u64 = 10_000;
        const SENTENCE: &str = "the replica applies each log record in order so that every \
                                follower converges on the same state as the leader once the \
                                backlog has drained";
        let texts: Vec<String> = (0..END.div_ceil(SEGMENT_MS))
            .map(|idx| format!("seg{idx} {SENTENCE}"))
            .collect();
        let parts: Vec<(i64, u64, u64, &str)> = texts
            .iter()
            .enumerate()
            .map(|(idx, text)| {
                let start = idx as u64 * SEGMENT_MS;
                (
                    idx as i64 / 30 + 1,
                    start,
                    (start + SEGMENT_MS).min(END),
                    text.as_str(),
                )
            })
            .collect();
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(parts, vec![(0, 60_000), (60_000, END)]),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_truncation_policy(truncation_policy(SessionProvider::GeminiLive));
        let mut playback = InMemoryPlayback::at(POS);

        let bounds = |v: &Value| -> (u64, u64) {
            (
                v["start"].as_u64().expect("range.start"),
                v["end"].as_u64().expect("range.end"),
            )
        };
        // A chunk's `range` is exactly the positions its text represents —
        // a cut chunk covers its (narrower) range in full; only a hard-cap
        // rider means bytes were dropped inside the declared range.
        let piece_is_whole = |chunk: &Value| chunk.get("hard_cap").is_none();
        // Worklist of ranges still to fetch; seeded with the user's request
        // and afterwards fed ONLY by `omitted_range` from replies.
        let mut todo = vec![(0u64, POS)];
        let mut covered: Vec<(u64, u64)> = Vec::new();
        let mut calls = 0usize;
        const CALL_CAP: usize = 12;
        while let Some((start, end)) = todo.pop() {
            calls += 1;
            assert!(
                calls <= CALL_CAP,
                "completion did not converge within {CALL_CAP} calls"
            );
            let outcome = dispatcher.dispatch(
                registry::GET_CONTEXT_FOR_RANGE,
                &json!({"start_position": start, "end_position": end}),
                now(),
                &mut playback,
                &mut excerpts,
            );
            let ToolOutcome::Ok(payload) = outcome else {
                panic!("call {calls} over {start}–{end} answers Ok, got {outcome:?}");
            };
            if payload["complete"] == json!(true) {
                covered.push(bounds(&payload["range"]));
                continue;
            }
            if piece_is_whole(&payload["first_chunk"]) {
                covered.push(bounds(&payload["first_chunk"]["range"]));
            }
            if piece_is_whole(&payload["last_chunk"]) {
                covered.push(bounds(&payload["last_chunk"]["range"]));
            }
            if let Some(omitted) = payload.get("omitted_range") {
                todo.push(bounds(omitted));
            }
        }
        // The union of complete pieces is contiguous from 0 through 22:23.
        covered.sort_unstable();
        let mut reach = 0u64;
        for (start, end) in &covered {
            assert!(*start <= reach, "hole before {start}: covered={covered:?}");
            reach = reach.max(*end);
        }
        assert!(reach >= POS, "coverage stops at {reach}, before 22:23");
        assert!(calls > 1, "the seed request must have split");
        // Recorded convergence for this rig (production Gemini budget,
        // 2.6 words/s): the first Split plus its recursive completions.
        assert!(calls <= 6, "recorded bound regressed: {calls} calls");
    }

    /// F10-D1 round-4 helper: drive `get_context_for_range` from the payload
    /// alone — the worklist is seeded with the request and afterwards fed
    /// ONLY by returned `omitted_range`; every returned piece's `range` is
    /// recorded. Returns the union pieces and the number of calls; panics on
    /// a non-Ok reply or when `cap` calls do not converge.
    fn complete_from_payload(
        dispatcher: &mut Dispatcher,
        playback: &mut dyn PlaybackControl,
        excerpts: &mut ExcerptSelector,
        (start, end): (u64, u64),
        cap: usize,
    ) -> (Vec<(u64, u64)>, usize) {
        let bounds = |v: &Value| -> (u64, u64) {
            (
                v["start"].as_u64().expect("range.start"),
                v["end"].as_u64().expect("range.end"),
            )
        };
        let mut todo = vec![(start, end)];
        let mut covered = Vec::new();
        let mut calls = 0usize;
        while let Some((a, b)) = todo.pop() {
            calls += 1;
            assert!(
                calls <= cap,
                "completion did not converge within {cap} calls"
            );
            let outcome = dispatcher.dispatch(
                registry::GET_CONTEXT_FOR_RANGE,
                &json!({"start_position": a, "end_position": b}),
                now(),
                playback,
                excerpts,
            );
            let ToolOutcome::Ok(payload) = outcome else {
                panic!("call {calls} over {a}–{b} answers Ok, got {outcome:?}");
            };
            if payload["complete"] == json!(true) {
                covered.push(bounds(&payload["range"]));
                continue;
            }
            for chunk in ["first_chunk", "last_chunk"] {
                if payload[chunk].get("hard_cap").is_none() {
                    covered.push(bounds(&payload[chunk]["range"]));
                }
            }
            if let Some(omitted) = payload.get("omitted_range") {
                todo.push(bounds(omitted));
            }
        }
        (covered, calls)
    }

    /// The union of `pieces` is contiguous from `start` through at least `end`.
    fn assert_contiguous(mut pieces: Vec<(u64, u64)>, start: u64, end: u64) {
        pieces.sort_unstable();
        let mut reach = start;
        for (a, b) in &pieces {
            assert!(*a <= reach, "hole before {a}: pieces={pieces:?}");
            reach = reach.max(*b);
        }
        assert!(
            reach >= end,
            "coverage stops at {reach}, before {end}: {pieces:?}"
        );
    }

    /// One segment of 40 words under a 6-word budget: eleven words of text
    /// per 10 s. Round-4 pin: the request over that single segment must
    /// answer a usable Split (no debug underflow, no invalid indices), and
    /// following `omitted_range` alone must complete the segment's text —
    /// sub-segment slices by position, `partially_cut` riders kept.
    #[test]
    fn f10_d1_one_oversized_segment_split_completes_from_payload() {
        let words: Vec<String> = (0..40).map(|i| format!("w{i:02}")).collect();
        let text = words.join(" ");
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![(1, 0, 10_000, text.as_str())],
                vec![(0, 10_000)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_truncation_policy(word_policy(6));
        let mut playback = InMemoryPlayback::at(10_000);

        // The first reply is the honest split of ONE segment.
        let first = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": 10_000}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = first else {
            panic!("{first:?}")
        };
        assert_eq!(payload["complete"], json!(false));
        assert_eq!(payload["omitted_middle_segments"], json!(0));
        assert_eq!(payload["total_segments_in_range"], json!(1));
        assert_eq!(payload["first_chunk_partially_cut"], json!(true));
        assert_eq!(payload["last_chunk_partially_cut"], json!(true));
        let omitted = &payload["omitted_range"];
        assert!(
            omitted["start"].as_u64().unwrap() > 0 && omitted["end"].as_u64().unwrap() < 10_000,
            "the interior of the cut segment is exposed by position: {payload}"
        );

        let (pieces, calls) = complete_from_payload(
            &mut dispatcher,
            &mut playback,
            &mut excerpts,
            (0, 10_000),
            12,
        );
        assert_contiguous(pieces, 0, 10_000);
        assert!(calls > 1, "the seed request must have split");
    }

    /// Two adjacent oversized segments (each over half the budget): the head
    /// is a cut prefix of the first, the tail a cut suffix of the second,
    /// `omitted_segments == 0` — yet the continuation must expose the
    /// uncovered interior spanning the two cut segments and complete.
    #[test]
    fn f10_d1_two_oversized_segments_split_completes_from_payload() {
        let words = |prefix: &str| -> String {
            (0..30)
                .map(|i| format!("{prefix}{i:02}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let (alpha, bravo) = (words("a"), words("b"));
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 10_000, alpha.as_str()),
                    (1, 10_000, 20_000, bravo.as_str()),
                ],
                vec![(0, 20_000)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_truncation_policy(word_policy(8));
        let mut playback = InMemoryPlayback::at(20_000);

        let first = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": 20_000}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = first else {
            panic!("{first:?}")
        };
        assert_eq!(payload["omitted_middle_segments"], json!(0));
        assert_eq!(payload["first_chunk_partially_cut"], json!(true));
        assert_eq!(payload["last_chunk_partially_cut"], json!(true));
        let omitted = &payload["omitted_range"];
        let (a, b) = (
            omitted["start"].as_u64().unwrap(),
            omitted["end"].as_u64().unwrap(),
        );
        assert!(a > 0 && a < 10_000 && b > 10_000 && b < 20_000, "{payload}");

        let (pieces, calls) = complete_from_payload(
            &mut dispatcher,
            &mut playback,
            &mut excerpts,
            (0, 20_000),
            12,
        );
        assert_contiguous(pieces, 0, 20_000);
        assert!(calls > 1);
    }

    /// D1b-P1 (book-direct half): same capability over character offsets,
    /// and the returned text counts as visited (rendering IS the feed there).
    #[test]
    fn d1b_a_bounded_range_works_in_offsets_and_counts_as_visited_book_direct() {
        use crate::voice::tools::playback::BookCursor;
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::BookDirect);
        let mut playback: BookCursor = BookCursor::new(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 20, "alpha offset text"),
                    (2, 20, 45, "bravo offset text"),
                    (3, 45, 70, "charlie offset text"),
                ],
                vec![(0, 20), (20, 70)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": 45}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("book-direct bounded fetch answers Ok, got {outcome:?}");
        };
        let text = payload["book_text"].as_str().expect("book text rides");
        assert!(
            text.contains("alpha offset text\n\nbravo offset text"),
            "the exact snapshot text across the offset range rides: {text}"
        );
        assert!(!text.contains("charlie"), "range end is respected: {text}");
        assert_eq!(
            payload["requested_range"],
            json!("offset 0–offset 45"),
            "book-direct positions label as offsets"
        );
        assert_eq!(
            excerpts.played_ranges().ranges(),
            &[(0, 45)],
            "returned book-direct text counts as visited, nothing more"
        );
    }

    /// D1b-P2 (gap honesty, the quiz-bounding law): a range spanning unplayed
    /// stretches answers with every gap NAMED and `range_fully_played` false.
    #[test]
    fn d1b_a_range_spanning_unplayed_content_names_the_gaps() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        excerpts.note_played(1000, 2000);
        excerpts.note_played(4000, 5000);
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": 5000}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("gap-spanning fetch still answers Ok, got {outcome:?}");
        };
        assert_eq!(payload["range_fully_played"], json!(false));
        assert_eq!(
            payload["unplayed_gaps"],
            json!(["0:00–0:01", "0:02–0:04"]),
            "every unplayed gap inside the request is named"
        );
        assert!(
            payload["book_text"].as_str().is_some(),
            "the text still rides — honesty is disclosure, not refusal"
        );
    }

    /// D1b-P3, F9-D1 semantics: in a GROUNDED session with a live narration
    /// position, omitted bounds resolve to [0, narration position) — the
    /// user's mental model of "covered so far" — while session-played honesty
    /// (gaps, `range_fully_played`) is still measured against THIS session's
    /// accumulator for quiz bounding.
    #[test]
    fn f9_grounded_omitted_bounds_default_to_the_narration_position() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        // Narration stands at 2500; NOTHING played during this session (the
        // owner-device shape: the user listened before the session started).
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("grounded omitted bounds must resolve, not refuse: {outcome:?}");
        };
        assert_eq!(
            payload["requested_range"],
            json!("0:00–0:02"),
            "the resolved range is [0, narration position)"
        );
        assert_eq!(payload["range_source"], json!("narration_so_far"));
        assert_eq!(
            payload["range_fully_played"],
            json!(false),
            "session-played honesty still rides: this session played nothing"
        );
        assert_eq!(
            payload["unplayed_gaps"],
            json!(["0:00–0:02"]),
            "the whole range is named as unplayed THIS session"
        );
        assert!(
            payload["book_text"].as_str().is_some(),
            "the real text rides — the recap has substance"
        );
    }

    /// D1b-P3 fallback half (F9-D1): grounded WITHOUT a live narration
    /// position falls back to the session-played span, and with nothing
    /// played it refuses typed instead of inventing.
    #[test]
    fn d1b_the_played_range_selector_resolves_or_refuses_honestly() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        playback.available = false; // no narration clock to default from
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Failed { code, message } = &outcome else {
            panic!("nothing played: expected a typed refusal, got {outcome:?}");
        };
        assert_eq!(code, CODE_NO_CONTEXT);
        assert!(
            message.contains("nothing has been played"),
            "the refusal says why: {message}"
        );

        excerpts.note_played(0, 2500);
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("played selector answers Ok once something played: {outcome:?}");
        };
        assert_eq!(payload["range_source"], json!("played_range"));
        assert_eq!(payload["requested_range"], json!("0:00–0:02"));
        assert_eq!(payload["range_fully_played"], json!(true));
    }

    /// F9-D1 → F10-D1 re-pin (deliberate, owner semantics law). KEPT: with
    /// nothing read this session AND the reading position at the book's
    /// start, omitted bounds answer an honest typed refusal — never invented
    /// text. RE-HOMED: a nonzero reading position no longer yields the
    /// session-read outer span; "so far" is [0, reading position), exactly
    /// as the grounded selector resolves [0, narration position), so the
    /// tool description, the law, and the dispatcher state ONE rule.
    #[test]
    fn f10_d1_book_direct_omitted_bounds_resolve_to_the_reading_position() {
        use crate::voice::tools::playback::BookCursor;
        let rig = || {
            let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
            dispatcher.set_mode(DispatchMode::BookDirect);
            let excerpts = ExcerptSelector::new(
                SessionSnapshot::from_voice_test_parts(
                    vec![
                        (1, 0, 20, "alpha offset text"),
                        (2, 20, 45, "bravo offset text"),
                        (3, 45, 70, "charlie offset text"),
                    ],
                    vec![(0, 20), (20, 70)],
                ),
                WindowPolicy {
                    before: 1,
                    after: 1,
                },
            );
            (dispatcher, excerpts)
        };
        // KEPT guarantee: book start, nothing read → typed refusal.
        let (mut dispatcher, mut excerpts) = rig();
        let mut at_start: BookCursor = BookCursor::new(0);
        let refused = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({}),
            now(),
            &mut at_start,
            &mut excerpts,
        );
        assert_eq!(code_of(&refused), CODE_NO_CONTEXT);

        // RE-HOMED: reading position 60, only [20,45) read this session →
        // [0, 60), labelled as the reading-position selector, with the
        // session-read honesty still riding as gaps.
        let (mut dispatcher, mut excerpts) = rig();
        let mut playback: BookCursor = BookCursor::new(60);
        excerpts.note_played(20, 45);
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("book-direct so-far selector answers Ok: {outcome:?}");
        };
        assert_eq!(payload["range_source"], json!("reading_so_far"));
        assert_eq!(payload["requested_range"], json!("offset 0–offset 60"));
        assert_eq!(payload["range_fully_played"], json!(false));
        assert_eq!(
            payload["unplayed_gaps"],
            json!(["offset 0–offset 20", "offset 45–offset 60"])
        );
        let text = payload["book_text"].as_str().expect("text rides");
        // The last segment [45,70) is clipped at the reading position 60:
        // its text arrives as the proportional prefix, never the whole.
        assert!(
            text.contains("alpha offset text")
                && text.contains("bravo offset text")
                && text.contains("charlie")
                && !text.contains("charlie offset text"),
            "{text}"
        );
        assert_eq!(payload["range"], json!({"start": 0, "end": 60}));
    }

    /// D1b-P4 (cap honesty): a range over the reply budget answers the honest
    /// split — first/last chunks, explicit omission counts, never a silently
    /// tail-capped blob.
    #[test]
    fn d1b_an_oversized_range_returns_the_honest_split_never_silent_truncation() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        // 15 words across five 3-word segments; budget 6 → half-budget 3.
        dispatcher.set_truncation_policy(TruncationPolicy::new(6, |text: &str| {
            text.split_whitespace().count()
        }));
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": 5000}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("oversized range still answers Ok, got {outcome:?}");
        };
        assert_eq!(payload["complete"], json!(false));
        assert!(
            payload["note"]
                .as_str()
                .expect("the split explains itself")
                .contains("FIRST and LAST"),
            "the split is announced in words: {payload}"
        );
        let head = payload["first_chunk"]["book_text"]
            .as_str()
            .expect("head chunk rides");
        let tail = payload["last_chunk"]["book_text"]
            .as_str()
            .expect("tail chunk rides");
        assert!(head.contains("page one opening"), "{head}");
        assert!(tail.contains("page three all"), "{tail}");
        assert_eq!(payload["omitted_middle_segments"], json!(3));
        assert_eq!(payload["total_segments_in_range"], json!(5));
        assert!(
            payload.get("book_text").is_none(),
            "no flat blob pretending to be the whole range"
        );
        assert!(
            payload.get("first_chunk_partially_cut").is_none()
                && payload.get("last_chunk_partially_cut").is_none(),
            "whole-segment chunks carry no cut flags"
        );
    }

    /// D1b-P4b (review finding): chunks cut INSIDE a segment disclose the cut
    /// on the payload, the tail chunk carries the range's true END, and — in
    /// BookDirect — cut chunks are never noted as visited (under-claim, the
    /// honest direction).
    #[test]
    fn d1b_intra_segment_cuts_are_disclosed_and_never_over_claim_coverage() {
        use crate::voice::tools::playback::BookCursor;
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::BookDirect);
        // 6 + 6 words in two oversized segments; budget 4 → half 2.
        dispatcher.set_truncation_policy(TruncationPolicy::new(4, |text: &str| {
            text.split_whitespace().count()
        }));
        let mut playback: BookCursor = BookCursor::new(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 60, "alpha beta gamma delta epsilon zeta"),
                    (2, 60, 120, "eta theta iota kappa lambda mu"),
                ],
                vec![(0, 120)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": 120}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Ok(payload) = outcome else {
            panic!("cut chunks still answer Ok, got {outcome:?}");
        };
        assert_eq!(payload["first_chunk_partially_cut"], json!(true));
        assert_eq!(payload["last_chunk_partially_cut"], json!(true));
        let tail = payload["last_chunk"]["book_text"]
            .as_str()
            .expect("tail chunk rides");
        assert!(
            tail.contains("lambda mu"),
            "the tail chunk carries the range's true end: {tail}"
        );
        assert!(
            excerpts.played_ranges().is_empty(),
            "cut chunks are never noted as visited — coverage under-claims, \
             never over-claims"
        );
    }

    /// D1b-P5 (standalone honesty): the fourteenth tool fails typed with the
    /// no-book wording exactly like its thirteen siblings.
    #[test]
    fn d1b_standalone_refuses_typed_with_the_no_book_wording() {
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::Standalone);
        let mut playback = InMemoryPlayback::at(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(vec![], vec![]),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": 1000}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Failed { code, message } = &outcome else {
            panic!("standalone must refuse typed, got {outcome:?}");
        };
        assert_eq!(code, CODE_NO_CONTEXT);
        assert!(
            message.contains("no book is attached"),
            "the F6 wording law rides the new tool: {message}"
        );
    }

    /// D1b-P6: a range beyond the book's text answers a typed refusal that
    /// names where the book ends — never an empty success.
    #[test]
    fn d1b_a_range_outside_the_timeline_refuses_and_names_the_end() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 9_000, "end_position": 10_000}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        let ToolOutcome::Failed { code, message } = &outcome else {
            panic!("an out-of-book range must refuse typed, got {outcome:?}");
        };
        assert_eq!(code, CODE_NO_CONTEXT);
        assert!(
            message.contains("the book's text spans 0:00–0:05"),
            "the refusal names the book's span: {message}"
        );
    }

    /// D1b-P7: the fetch is a Tier-0 read — it answers with the playing
    /// surface gone and costs no Tier-1 budget.
    #[test]
    fn d1b_the_bounded_fetch_is_a_read_that_survives_a_dead_surface() {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        playback.available = false;
        let outcome = dispatcher.dispatch(
            registry::GET_CONTEXT_FOR_RANGE,
            &json!({"start_position": 0, "end_position": 2000}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert!(
            matches!(outcome, ToolOutcome::Ok(_)),
            "a bounded read needs no playback clock, got {outcome:?}"
        );
    }

    /// F10-D2: a BookDirect chapter with no readable text and no page span
    /// cannot be shown — the jump refuses typed and the cursor stays; the
    /// same chapter WITH a book page span is a real move.
    #[test]
    fn book_direct_jump_to_an_unshowable_chapter_refuses_typed() {
        use crate::voice::tools::playback::BookCursor;
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::BookDirect);
        let mut playback = BookCursor::new(0);
        let bare = SessionSnapshot::from_voice_test_parts(
            vec![(1, 0, 20, "cover blurb")],
            vec![(0, 20), (20, 20)],
        );
        let mut excerpts = ExcerptSelector::new(
            bare,
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let refused = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(code_of(&refused), CODE_UNRESOLVED_PAGE, "{refused:?}");
        assert_eq!(
            playback.position_ms().unwrap(),
            0,
            "a refused jump never moves the cursor"
        );
        let paged = SessionSnapshot::from_voice_test_parts(
            vec![(1, 0, 20, "cover blurb")],
            vec![(0, 20), (20, 20)],
        )
        .with_test_chapter_pages(vec![(1, 1), (17, 30)]);
        let mut excerpts = ExcerptSelector::new(
            paged,
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let moved = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(
            moved,
            ToolOutcome::Ok(json!({
                "position_ms": 20,
                "chapter": 1,
                "chapter_title": "chapter 1",
                "page": 17,
                "status": "The reader's screen now displays this chapter."
            }))
        );
    }

    /// Ruling #35.3: in BookDirect `replay_previous_paragraph` MOVES the
    /// reader; only the audio-bound tools refuse typed.
    #[test]
    fn book_direct_replay_previous_paragraph_moves_the_reader() {
        use crate::voice::tools::playback::BookCursor;
        let mut dispatcher = Dispatcher::new(ToolRegistry::new(false));
        dispatcher.set_mode(DispatchMode::BookDirect);
        let mut playback: BookCursor = BookCursor::new(0);
        let mut excerpts = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 20, "alpha offset text"),
                    (2, 20, 45, "bravo offset text"),
                ],
                vec![(0, 20), (20, 45)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let jump = dispatcher.dispatch(
            registry::JUMP_TO_CHAPTER,
            &json!({"chapter": 1}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_jump_lands(
            &jump,
            json!({
                "position_ms": 20,
                "chapter": 1,
                "chapter_title": "chapter 1",
                "page": 2,
                "status": "The reader's screen now displays this chapter."
            }),
            "bravo offset text",
        );

        let replay = dispatcher.dispatch(
            registry::REPLAY_PREVIOUS_PARAGRAPH,
            &json!({}),
            now(),
            &mut playback,
            &mut excerpts,
        );
        assert_eq!(
            replay,
            ToolOutcome::Ok(json!({"position_ms": 0})),
            "replay_previous_paragraph is a move, not the no-narration refusal"
        );
        assert_eq!(playback.position_ms().unwrap(), 0, "the reader moved");
    }

    fn matrix_calls() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            (registry::PAUSE_PLAYBACK, json!({})),
            (registry::RESUME_PLAYBACK, json!({})),
            (registry::SEEK_RELATIVE, json!({"seconds": -30})),
            (registry::SET_PLAYBACK_SPEED, json!({"speed": 1.25})),
            (registry::REPLAY_PREVIOUS_PARAGRAPH, json!({})),
            (registry::REPLAY_PREVIOUS_PAGE, json!({})),
            (registry::REPLAY_LAST_MINUTE, json!({})),
            (registry::JUMP_TO_CHAPTER, json!({"chapter": 1})),
            (registry::JUMP_TO_PAGE, json!({"page": 1})),
            (registry::SUMMARIZE_SECTION, json!({})),
            (
                registry::GET_CONTEXT_AROUND_POSITION,
                json!({"position_ms": 2500}),
            ),
            (
                registry::GET_CONTEXT_FOR_RANGE,
                json!({"start_position": 0, "end_position": 1000}),
            ),
            (
                registry::UPDATE_LEARNING_PROFILE,
                json!({"key": "pace", "value": "slow"}),
            ),
        ]
    }

    fn unhurried(idx: usize) -> Instant {
        Instant::now() + std::time::Duration::from_secs(11 * idx as u64)
    }

    fn normalize_sentinels(results: Vec<(String, ToolResult)>) -> Vec<(String, String)> {
        let nonce = regex::Regex::new("untrusted-book-excerpt [0-9a-f]{32}").unwrap();
        results
            .into_iter()
            .map(|(id, result)| {
                let rendered = format!("{result:?}");
                (
                    id,
                    nonce
                        .replace_all(&rendered, "untrusted-book-excerpt <nonce>")
                        .into_owned(),
                )
            })
            .collect()
    }

    async fn matrix_on_mock(mode: DispatchMode) -> Vec<(String, ToolResult)> {
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        dispatcher.set_mode(mode);
        let (mut channel, driver) = connected_mock().await;
        for (idx, (name, args)) in matrix_calls().into_iter().enumerate() {
            let call_id = format!("call-{idx}");
            answer_tool_call(
                &mut dispatcher,
                &mut channel,
                &call_id,
                name,
                &args,
                unhurried(idx),
                &mut playback,
                &mut excerpts,
            )
            .await
            .expect("mock send succeeds");
        }
        driver.tool_results()
    }

    async fn matrix_on_text(mode: DispatchMode) -> Vec<(String, ToolResult)> {
        use crate::voice::audio::generation::Generation;
        use crate::voice::channel::text::scripted::{scripted_channel, tool_results};
        use crate::voice::channel::{ChannelEvent, LifecycleEvent};
        let (mut dispatcher, mut playback, mut excerpts) = rig();
        dispatcher.set_mode(mode);
        let (mut channel, handle) = scripted_channel(Generation::new());
        let mut events = channel.take_events().expect("fresh channel");
        channel
            .connect(SessionContext {
                book_id: Some("wiring-book".to_string()),
                session_id: "wiring-session".to_string(),
                system_prompt: "tutor".to_string(),
                excerpt: DelimitedExcerpt {
                    delimited_text: "<s>x</s>".to_string(),
                },
                rolling_summary: None,
                playback_pos_ms: 0,
                tool_schemas: ToolRegistry::new(false).schemas(),
            })
            .await
            .expect("text channel connects");
        assert!(matches!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected))
        ));
        for (idx, (name, args)) in matrix_calls().into_iter().enumerate() {
            let call_id = format!("call-{idx}");
            handle.push_call(&call_id, name, args.clone()).await;
            handle.end_turn().await;
            loop {
                match events.recv().await.expect("stream open") {
                    ChannelEvent::ToolCall { call_id: got, .. } => {
                        assert_eq!(got, call_id);
                        break;
                    }
                    ChannelEvent::UtteranceEnd => {}
                    other => panic!("unexpected event before the tool call: {other:?}"),
                }
            }
            answer_tool_call(
                &mut dispatcher,
                &mut channel,
                &call_id,
                name,
                &args,
                unhurried(idx),
                &mut playback,
                &mut excerpts,
            )
            .await
            .expect("text send succeeds");
        }
        let (_, last) = handle
            .requests()
            .pop()
            .expect("a continuation request exists");
        tool_results(&last)
    }

    #[tokio::test]
    async fn f11_c13_matrix_outcomes_are_byte_identical_on_text_and_mock_rigs() {
        for mode in [
            DispatchMode::Grounded,
            DispatchMode::BookDirect,
            DispatchMode::Standalone,
        ] {
            let mock = normalize_sentinels(matrix_on_mock(mode).await);
            let text = normalize_sentinels(matrix_on_text(mode).await);
            assert_eq!(
                mock.len(),
                matrix_calls().len(),
                "{mode:?}: one answer per call"
            );
            assert!(
                !mock.iter().any(|(_, r)| r.contains("rate_limited")),
                "{mode:?}: the matrix must not be masked by the Tier-1 budget: {mock:?}"
            );
            assert_eq!(
                text, mock,
                "{mode:?}: the text rig answers exactly as the mock rig"
            );
        }
    }
}
