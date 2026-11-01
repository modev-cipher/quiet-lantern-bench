//! The Tier-0/1 tool registry (VOICE_AGENT §5 table, ARCHITECTURE §8.1).
//!
//! One closed list: the twelve core tools (the VOICE_AGENT §5 table plus
//! `update_learning_profile`, owner ruling R1) plus the presentation-sync
//! tool `teacher_anchor` (offered only in live tutor sessions — the
//! constructor flag). Schemas are strict by construction — `additionalProperties: false`
//! everywhere, bounded numerics spelled as `minimum`/`maximum` — and use the
//! OpenAI function-tool shape, the crate's neutral schema convention: the
//! openai_compat slot forwards [`SessionContext::tool_schemas`] verbatim
//! because that wire family *is* this shape, and every other adapter
//! translates from it.
//!
//! The registry is vocabulary only. Validation and execution live in the
//! dispatcher; the schema here and the dispatcher's checks must agree, which
//! is why the bounds are named constants shared by both.
//!
//! [`SessionContext::tool_schemas`]: crate::voice::channel::types::SessionContext

use serde_json::{json, Value};

use crate::voice::channel::anchor::TEACHER_ANCHOR_TOOL;
use crate::voice::tools::dispatcher::DispatchMode;

pub const PAUSE_PLAYBACK: &str = "pause_playback";
pub const RESUME_PLAYBACK: &str = "resume_playback";
pub const SEEK_RELATIVE: &str = "seek_relative";
pub const SET_PLAYBACK_SPEED: &str = "set_playback_speed";
pub const REPLAY_PREVIOUS_PARAGRAPH: &str = "replay_previous_paragraph";
pub const REPLAY_PREVIOUS_PAGE: &str = "replay_previous_page";
pub const REPLAY_LAST_MINUTE: &str = "replay_last_minute";
pub const JUMP_TO_CHAPTER: &str = "jump_to_chapter";
pub const JUMP_TO_PAGE: &str = "jump_to_page";
pub const TURN_PAGE: &str = "turn_page";
pub const SUMMARIZE_SECTION: &str = "summarize_section";
pub const GET_CONTEXT_AROUND_POSITION: &str = "get_context_around_position";
/// F8-D1b (owner-approved charter amendment, field defect D-QA1): fetch the
/// real book text across an explicit bounded range — or the played range —
/// in ONE call, so "key points from minute 0 to 32" never needs dozens of
/// serial window fetches or a clarifying question. Tier 0 — read-only.
pub const GET_CONTEXT_FOR_RANGE: &str = "get_context_for_range";
pub const UPDATE_LEARNING_PROFILE: &str = "update_learning_profile";
/// F7 (owner ruling 2026-08-30, tutor tier: ON-DEMAND pixels): sends the
/// CURRENT page's rendered image into the session so the model can see
/// figures, charts, diagrams, tables, and rendered equations. Tier 0 —
/// read-only, mutates nothing; the egress itself is consented at session
/// level (the voice consent copy names page images).
pub const LOOK_AT_PAGE: &str = "look_at_page";

/// `seek_relative` bounds, spec-fixed (VOICE_AGENT §5: seconds ∈ [−600, 600]).
pub const SEEK_RELATIVE_MIN_SECONDS: f64 = -600.0;
pub const SEEK_RELATIVE_MAX_SECONDS: f64 = 600.0;

/// `set_playback_speed` bounds, spec-fixed (VOICE_AGENT §5: speed ∈ [0.5, 3.0]).
pub const SPEED_MIN: f64 = 0.5;
pub const SPEED_MAX: f64 = 3.0;

/// `update_learning_profile` bounds (owner ruling R1; §5 strict-schema
/// discipline). Mirrored by migration 0019's CHECK constraints as the
/// storage-side backstop.
pub const PROFILE_KEY_MAX_CHARS: usize = 48;
pub const PROFILE_VALUE_MAX_CHARS: usize = 200;

/// The §5/§8.1 tool-tier vocabulary for the tools this task ships. Tier 2
/// ("anything beyond playback/presentation state — per-invocation user
/// confirmation, forever") has no member here and gets no enum variant until
/// something ships in it: an unrepresentable tier cannot be auto-allowed by
/// accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolTier {
    /// Read-only (VOICE_AGENT §5: `summarize_section`,
    /// `get_context_around_position`).
    Zero,
    /// Reversible playback/presentation-state mutation, auto-allowed but
    /// rate-limited (ARCHITECTURE §8.1: ~5 calls / 10 s).
    One,
}

/// The always-offered tools (VOICE_AGENT §5 table + R1's profile tool +
/// F8-D1b's bounded-range fetch, minus `teacher_anchor`, which is
/// session-kind-gated).
const CORE_TOOLS: &[(&str, ToolTier)] = &[
    (PAUSE_PLAYBACK, ToolTier::One),
    (RESUME_PLAYBACK, ToolTier::One),
    (SEEK_RELATIVE, ToolTier::One),
    (SET_PLAYBACK_SPEED, ToolTier::One),
    (REPLAY_PREVIOUS_PARAGRAPH, ToolTier::One),
    (REPLAY_PREVIOUS_PAGE, ToolTier::One),
    (REPLAY_LAST_MINUTE, ToolTier::One),
    (JUMP_TO_CHAPTER, ToolTier::One),
    (JUMP_TO_PAGE, ToolTier::One),
    (TURN_PAGE, ToolTier::One),
    (SUMMARIZE_SECTION, ToolTier::Zero),
    (GET_CONTEXT_AROUND_POSITION, ToolTier::Zero),
    // Owner-approved capability F8-D1b: bounded-range fetch, read-only.
    (GET_CONTEXT_FOR_RANGE, ToolTier::Zero),
    // Owner ruling R1: local-only, reversible (an entry can be re-stated or
    // deleted), never book text — Tier 1 by the §8.1 definition.
    (UPDATE_LEARNING_PROFILE, ToolTier::One),
];

/// Tools offered in BookDirect mode: navigation and reading tools only, no narration playback tools.
const BOOK_DIRECT_TOOLS: &[(&str, ToolTier)] = &[
    (JUMP_TO_CHAPTER, ToolTier::One),
    (JUMP_TO_PAGE, ToolTier::One),
    (TURN_PAGE, ToolTier::One),
    (SUMMARIZE_SECTION, ToolTier::Zero),
    (GET_CONTEXT_AROUND_POSITION, ToolTier::Zero),
    (GET_CONTEXT_FOR_RANGE, ToolTier::Zero),
    (UPDATE_LEARNING_PROFILE, ToolTier::One),
];

/// Tools offered in Standalone mode: learning profile only (no book or playback tools).
const STANDALONE_TOOLS: &[(&str, ToolTier)] = &[(UPDATE_LEARNING_PROFILE, ToolTier::One)];

/// Book Agent tool names (charter §4.2; feature `book-agent`, offered only by
/// [`ToolRegistry::agent`]). Present unconditionally as constants so the
/// Tutor pins can assert they are NOT offered without cfg gymnastics.
pub const SEARCH_BOOK: &str = "search_book";
pub const OPEN_PASSAGE: &str = "open_passage";
pub const GO_TO_PASSAGE: &str = "go_to_passage";
pub const HIGHLIGHT_PASSAGE: &str = "highlight_passage";

/// `search_book` bounds (charter §4.2): 1-4 variants, each length-capped;
/// mirrored by the retrieval layer's own validation.
pub const SEARCH_MAX_VARIANTS: usize = 4;
pub const SEARCH_MAX_QUERY_CHARS: usize = 256;
/// Opaque handle tokens are 32 hex chars today; the schema caps generously.
pub const HANDLE_MAX_CHARS: usize = 64;

/// The Book Agent registry (charter §4): exactly four reused Tier-1 verbs and
/// the four new Agent tools. No other Tutor tool is implicitly inherited —
/// in particular no `summarize_section`, `get_context_around_position`,
/// `seek_relative`, replay/speed verbs, `update_learning_profile`, or
/// `teacher_anchor`.
#[cfg(feature = "book-agent")]
const AGENT_TOOLS: &[(&str, ToolTier)] = &[
    (PAUSE_PLAYBACK, ToolTier::One),
    (RESUME_PLAYBACK, ToolTier::One),
    (JUMP_TO_CHAPTER, ToolTier::One),
    (JUMP_TO_PAGE, ToolTier::One),
    (SEARCH_BOOK, ToolTier::Zero),
    (OPEN_PASSAGE, ToolTier::Zero),
    (GO_TO_PASSAGE, ToolTier::One),
    (HIGHLIGHT_PASSAGE, ToolTier::One),
];

/// Which closed vocabulary one session offers. Mode is immutable for the
/// session's lifetime (charter §2.1) exactly like the registry itself.
#[derive(Debug, Clone, Copy)]
enum RegistryKind {
    /// The existing Tutor surface, `teacher_anchor` and `look_at_page` gated by
    /// whether the live teacher surface exists.
    Tutor { has_teacher_surface: bool },
    /// The Book Agent surface (charter §4).
    #[cfg(feature = "book-agent")]
    Agent,
}

/// The tool list one session offers. Immutable for the session's lifetime —
/// the schemas travel in the session-setup message, so a mid-session change
/// would desynchronize the provider's view from the dispatcher's.
#[derive(Debug, Clone, Copy)]
pub struct ToolRegistry {
    kind: RegistryKind,
}

impl ToolRegistry {
    /// The Tutor registry. `teacher_anchor` and `look_at_page` are live-tutor-session
    /// vocabulary only (VOICE_AGENT §11.1); a session without the live preview must not
    /// offer them, or the model can call tools whose effect surfaces do not exist.
    pub fn new(has_teacher_surface: bool) -> Self {
        Self {
            kind: RegistryKind::Tutor {
                has_teacher_surface,
            },
        }
    }

    /// The Book Agent registry (charter §4). Fixture-harness vocabulary until
    /// the Agent session mode ships; no production session constructs it yet.
    #[cfg(feature = "book-agent")]
    pub fn agent() -> Self {
        Self {
            kind: RegistryKind::Agent,
        }
    }

    pub fn has_teacher_surface(&self) -> bool {
        matches!(
            self.kind,
            RegistryKind::Tutor {
                has_teacher_surface: true
            }
        )
    }

    pub fn teacher_anchor_enabled(&self) -> bool {
        self.has_teacher_surface()
    }

    /// The tier of an offered tool; `None` for anything this session did not
    /// offer — including `teacher_anchor` and `look_at_page` when disabled (or in Agent mode),
    /// so the dispatcher rejects it exactly like a hallucinated name.
    pub fn tier(&self, name: &str) -> Option<ToolTier> {
        match self.kind {
            RegistryKind::Tutor {
                has_teacher_surface,
            } => {
                if has_teacher_surface {
                    if name == TEACHER_ANCHOR_TOOL {
                        return Some(ToolTier::One);
                    }
                    if name == LOOK_AT_PAGE {
                        return Some(ToolTier::Zero);
                    }
                }
                CORE_TOOLS
                    .iter()
                    .find(|(tool, _)| *tool == name)
                    .map(|(_, tier)| *tier)
            }
            #[cfg(feature = "book-agent")]
            RegistryKind::Agent => AGENT_TOOLS
                .iter()
                .find(|(tool, _)| *tool == name)
                .map(|(_, tier)| *tier),
        }
    }

    /// Names offered by this registry for a specific dispatch mode, in table order.
    pub fn names_for_mode(&self, mode: DispatchMode) -> Vec<&'static str> {
        match self.kind {
            RegistryKind::Tutor {
                has_teacher_surface,
            } => {
                let tool_list = match mode {
                    DispatchMode::Grounded => CORE_TOOLS,
                    DispatchMode::BookDirect => BOOK_DIRECT_TOOLS,
                    DispatchMode::Standalone => STANDALONE_TOOLS,
                };
                let mut names: Vec<&'static str> =
                    tool_list.iter().map(|(name, _)| *name).collect();
                if has_teacher_surface && mode != DispatchMode::Standalone {
                    names.push(LOOK_AT_PAGE);
                    names.push(TEACHER_ANCHOR_TOOL);
                }
                names
            }
            #[cfg(feature = "book-agent")]
            RegistryKind::Agent => AGENT_TOOLS.iter().map(|(name, _)| *name).collect(),
        }
    }

    /// Names offered by this registry, in table order (defaults to Grounded mode).
    pub fn names(&self) -> Vec<&'static str> {
        self.names_for_mode(DispatchMode::Grounded)
    }

    /// Schemas offered by this registry for a specific dispatch mode.
    pub fn schemas_for_mode(&self, mode: DispatchMode) -> Vec<Value> {
        let schemas = self.names_for_mode(mode).into_iter().map(schema_for);
        match self.kind {
            RegistryKind::Tutor { .. } => schemas.collect(),
            #[cfg(feature = "book-agent")]
            RegistryKind::Agent => schemas
                .map(|mut schema| {
                    schema["function"]["behavior"] = json!("NON_BLOCKING");
                    schema
                })
                .collect(),
        }
    }

    /// The session's tool schemas, ready for
    /// [`SessionContext::tool_schemas`](crate::voice::channel::types::SessionContext).
    pub fn schemas(&self) -> Vec<Value> {
        self.schemas_for_mode(DispatchMode::Grounded)
    }
}

fn function_schema(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    })
}

fn no_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    })
}

fn schema_for(name: &str) -> Value {
    match name {
        PAUSE_PLAYBACK => function_schema(name, "Pause audiobook playback.", no_parameters()),
        RESUME_PLAYBACK => function_schema(name, "Resume audiobook playback.", no_parameters()),
        SEEK_RELATIVE => function_schema(
            name,
            "Move the playback position by a signed number of seconds \
             (negative rewinds). The result stays within the book.",
            json!({
                "type": "object",
                "properties": {
                    "seconds": {
                        "type": "number",
                        "minimum": SEEK_RELATIVE_MIN_SECONDS,
                        "maximum": SEEK_RELATIVE_MAX_SECONDS,
                        "description": "Offset in seconds; negative rewinds.",
                    }
                },
                "required": ["seconds"],
                "additionalProperties": false,
            }),
        ),
        SET_PLAYBACK_SPEED => function_schema(
            name,
            "Set the playback speed multiplier.",
            json!({
                "type": "object",
                "properties": {
                    "speed": {
                        "type": "number",
                        "minimum": SPEED_MIN,
                        "maximum": SPEED_MAX,
                        "description": "1.0 is normal speed.",
                    }
                },
                "required": ["speed"],
                "additionalProperties": false,
            }),
        ),
        REPLAY_PREVIOUS_PARAGRAPH => function_schema(
            name,
            "Rewind to the start of the paragraph before the one currently \
             playing.",
            no_parameters(),
        ),
        REPLAY_PREVIOUS_PAGE => function_schema(
            name,
            "Rewind to the start of the previous document page.",
            no_parameters(),
        ),
        REPLAY_LAST_MINUTE => {
            function_schema(name, "Rewind playback by one minute.", no_parameters())
        }
        JUMP_TO_CHAPTER => function_schema(
            name,
            "Turn the reader's screen to the start of a chapter or section by its exact 0-based Index in \
             the Book chapter map, and move playback there if audio is active. The answer \
             carries the book text at the landing point (`book_text`) — read it before \
             explaining what is there. Call this autonomously whenever discussing content \
             located in another chapter or section.",
            json!({
                "type": "object",
                "properties": {
                    "chapter_index": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "The exact 0-based index from the Book chapter map (e.g. 0, 1, 14, 15). \
                                        Look up the chapter or section in the map and pass its exact Index number. \
                                        Notice: If 'Chapter 1' is listed at '- Index 2', you must pass chapter_index: 2, NOT 1.",
                    }
                },
                "required": ["chapter_index"],
                "additionalProperties": false,
            }),
        ),
        JUMP_TO_PAGE => function_schema(
            name,
            "Turn the reader's screen to the given physical page number from the Book chapter map, \
             and move playback there if audio is active. The answer carries the book text at the \
             landing point (`book_text`) — read it before explaining what is there. Call this \
             autonomously whenever discussing content located on another page.",
            json!({
                "type": "object",
                "properties": {
                    "page": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-based physical page number from the Book chapter map (e.g. pages 73–75 -> pass 73). \
                                        Turn the reader's screen to this page.",
                    }
                },
                "required": ["page"],
                "additionalProperties": false,
            }),
        ),
        TURN_PAGE => function_schema(
            name,
            "Turn the reader's screen one page forward or back from the page the user is \
             looking at, skipping pages that carry no text. Use this for \"next page\" / \
             \"previous page\" requests in any language (الصفحة الجاية / اقلب الصفحة). The answer \
             carries the landing page's book text (`book_text`).",
            json!({
                "type": "object",
                "properties": {
                    "direction": {
                        "type": "string",
                        "enum": ["next", "previous"],
                    }
                },
                "required": ["direction"],
                "additionalProperties": false,
            }),
        ),
        SUMMARIZE_SECTION => function_schema(
            name,
            "Fetch the exact book text already injected in your context so you \
             can summarize it. The scope is the injected window only and never \
             widens.",
            no_parameters(),
        ),
        UPDATE_LEARNING_PROFILE => function_schema(
            name,
            "Record a lasting teaching preference the user EXPLICITLY stated \
             (how to present material, pace, tone). Only when the user states \
             one — never infer. Local to this device.",
            json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": PROFILE_KEY_MAX_CHARS,
                        "description": "Short topic name, e.g. \"equations\", \
                                        \"pace\", \"tone\".",
                    },
                    "value": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": PROFILE_VALUE_MAX_CHARS,
                        "description": "The preference, in the user's words.",
                    }
                },
                "required": ["key", "value"],
                "additionalProperties": false,
            }),
        ),
        GET_CONTEXT_FOR_RANGE => function_schema(
            name,
            "Call this — do not ask — whenever the user names a range or asks \
             what was covered so far, until now, or up to here: it fetches the \
             real book text across a bounded position range in ONE call — \
             e.g. a recap or quiz over minutes 0 to 32. Positions use \
             this session's position space (milliseconds on the narration \
             timeline, or character offsets when the book has no narration). \
             Omit both bounds for \"covered so far\": ONE rule in every mode \
             — it fetches from the start of the book to the current position \
             (the narration position, or the reading position when the book \
             has no narration). The reply names any unplayed gaps inside the \
             range so covered-so-far answers stay honest. The payload is \
             capped at the session excerpt budget; a range over the cap \
             returns the FIRST and LAST chunks, each with its `range`, plus \
             `omitted_range` — the middle you do not have yet: call this \
             tool on it next, and again on any further `omitted_range`, \
             until every piece is complete — never silent truncation.",
            json!({
                "type": "object",
                "properties": {
                    "start_position": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Range start (inclusive), in the \
                                        session's position space.",
                    },
                    "end_position": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Range end (exclusive); must exceed \
                                        start_position. Pass both bounds or \
                                        neither.",
                    }
                },
                "additionalProperties": false,
            }),
        ),
        GET_CONTEXT_AROUND_POSITION => function_schema(
            name,
            "Fetch the window of book text around a playback position \
             (defaults to the current position). The window size is fixed by \
             the app.",
            json!({
                "type": "object",
                "properties": {
                    "position_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Position on the book timeline, in \
                                        milliseconds. Omit for the current \
                                        playback position.",
                    }
                },
                "additionalProperties": false,
            }),
        ),
        LOOK_AT_PAGE => function_schema(
            name,
            "Look at the reader's current page as an image. Use this whenever \
             the user asks about anything visible on the page — a figure, \
             chart, diagram, illustration, table, or equation — and the text \
             context is not enough. The page image arrives as visual input \
             right after this call returns.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        ),
        TEACHER_ANCHOR_TOOL => function_schema(
            name,
            "Point the live preview at the figure, equation, or table you are \
             explaining. Affects presentation only; never moves audio \
             playback.",
            json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        // `section` is §11.1 vocabulary but descoped from the
                        // live tool offer until #205's upstream fixes land
                        // (PR #206 verifier ruling); a model that emits it
                        // anyway is refused with `unsupported_anchor_kind`
                        // (channel::anchor), never acked `applied`.
                        "enum": ["figure", "equation", "table"],
                    },
                    "id": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The anchor id as given in your context.",
                    }
                },
                "required": ["kind", "id"],
                "additionalProperties": false,
            }),
        ),
        #[cfg(feature = "book-agent")]
        SEARCH_BOOK => function_schema(
            name,
            "Search the whole active book. Pass 1-4 short lexical variants of \
             ONE question (for Arabic, include singular AND plural, and \
             verbal-noun AND verb forms as separate variants). Returns at \
             most 5 ranked snippets with opaque passage handles.",
            json!({
                "type": "object",
                "properties": {
                    "queries": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": SEARCH_MAX_VARIANTS,
                        "items": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": SEARCH_MAX_QUERY_CHARS,
                        },
                        "description": "Lexical variants of one question.",
                    },
                    "chapter": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Optional 0-based chapter scope.",
                    }
                },
                "required": ["queries"],
                "additionalProperties": false,
            }),
        ),
        #[cfg(feature = "book-agent")]
        OPEN_PASSAGE => function_schema(
            name,
            "Open the bounded source passage behind one search handle. The \
             only tool that returns passage text; never a chapter.",
            json!({
                "type": "object",
                "properties": {
                    "handle": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": HANDLE_MAX_CHARS,
                        "description": "A live handle from search_book.",
                    }
                },
                "required": ["handle"],
                "additionalProperties": false,
            }),
        ),
        #[cfg(feature = "book-agent")]
        GO_TO_PASSAGE => function_schema(
            name,
            "Move playback and the reading view to the passage behind one \
             search handle.",
            json!({
                "type": "object",
                "properties": {
                    "handle": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": HANDLE_MAX_CHARS,
                        "description": "A live handle from search_book.",
                    }
                },
                "required": ["handle"],
                "additionalProperties": false,
            }),
        ),
        #[cfg(feature = "book-agent")]
        HIGHLIGHT_PASSAGE => function_schema(
            name,
            "Visibly cite the passage behind one search handle on the reading \
             surface. Presentation only; never moves audio playback.",
            json!({
                "type": "object",
                "properties": {
                    "handle": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": HANDLE_MAX_CHARS,
                        "description": "A live handle from search_book.",
                    },
                    "start_word": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Optional 0-based first word of the \
                                        cited span within the passage.",
                    },
                    "word_count": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional cited span length in words.",
                    }
                },
                "required": ["handle"],
                "additionalProperties": false,
            }),
        ),
        other => unreachable!("schema requested for unregistered tool {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_offers_the_spec_tool_set() {
        // Owner-approved capability F8-D1b: the core roster grew from 13 to
        // 14 with `get_context_for_range`.
        let without_preview = ToolRegistry::new(false);
        assert_eq!(without_preview.names().len(), 14);
        assert_eq!(without_preview.tier(TEACHER_ANCHOR_TOOL), None);
        assert_eq!(without_preview.tier(LOOK_AT_PAGE), None);

        let with_preview = ToolRegistry::new(true);
        assert_eq!(with_preview.names().len(), 16);
        assert_eq!(with_preview.tier(TEACHER_ANCHOR_TOOL), Some(ToolTier::One));
        assert_eq!(with_preview.tier(LOOK_AT_PAGE), Some(ToolTier::Zero));
        assert_eq!(
            with_preview.tier(UPDATE_LEARNING_PROFILE),
            Some(ToolTier::One),
            "R1: the profile tool is Tier-1 local state"
        );
        assert_eq!(
            with_preview.tier(SUMMARIZE_SECTION),
            Some(ToolTier::Zero),
            "summarize is read-only per the §5 table"
        );
        assert_eq!(
            with_preview.tier(GET_CONTEXT_FOR_RANGE),
            Some(ToolTier::Zero),
            "F8-D1b: the bounded-range fetch is a read"
        );
        assert_eq!(with_preview.tier("open_shell"), None);
    }

    #[test]
    fn every_schema_is_strict_and_names_its_tool() {
        let registry = ToolRegistry::new(true);
        let names = registry.names();
        for (schema, name) in registry.schemas().iter().zip(names) {
            assert_eq!(schema["type"], "function");
            assert_eq!(schema["function"]["name"], name);
            let parameters = &schema["function"]["parameters"];
            assert_eq!(
                parameters["additionalProperties"], false,
                "{name}: §5 mandates deny-unknown-fields schemas"
            );
            assert_eq!(parameters["type"], "object");
        }
    }

    /// The #205 descope at the vocabulary source (PR #206 verifier ruling):
    /// the `kind` enum the provider receives is exactly the three supported
    /// kinds, and the whole `teacher_anchor` schema — description included —
    /// never utters "section", so nothing in the setup message invites the
    /// model to call the descoped kind. If the enum ever regains `section`
    /// this REDs before any live model can be lied to.
    #[test]
    fn the_live_anchor_schema_offers_no_section_vocabulary() {
        let schemas = ToolRegistry::new(true).schemas();
        let anchor = schemas
            .iter()
            .find(|schema| schema["function"]["name"] == TEACHER_ANCHOR_TOOL)
            .expect("live sessions offer teacher_anchor");
        assert_eq!(
            anchor["function"]["parameters"]["properties"]["kind"]["enum"],
            json!(["figure", "equation", "table"]),
            "the offered kind vocabulary is exactly the descoped set"
        );
        assert!(
            !anchor.to_string().contains("section"),
            "no corner of the teacher_anchor schema may name the descoped kind"
        );
    }

    #[test]
    fn numeric_bounds_in_the_schemas_are_the_spec_bounds() {
        let registry = ToolRegistry::new(false);
        let schemas = registry.schemas();
        let params_of = |name: &str| -> Value {
            schemas
                .iter()
                .find(|schema| schema["function"]["name"] == name)
                .expect("registered")["function"]["parameters"]
                .clone()
        };
        let seek = params_of(SEEK_RELATIVE);
        assert_eq!(seek["properties"]["seconds"]["minimum"], -600.0);
        assert_eq!(seek["properties"]["seconds"]["maximum"], 600.0);
        let speed = params_of(SET_PLAYBACK_SPEED);
        assert_eq!(speed["properties"]["speed"]["minimum"], 0.5);
        assert_eq!(speed["properties"]["speed"]["maximum"], 3.0);
    }

    #[test]
    fn the_registry_filters_tools_by_dispatch_mode() {
        let registry = ToolRegistry::new(true);

        // Grounded offers all 15 tools (including playback, preview, and teacher_anchor)
        let grounded = registry.names_for_mode(DispatchMode::Grounded);
        assert_eq!(grounded.len(), 16);
        assert!(grounded.contains(&TURN_PAGE));
        assert!(grounded.contains(&PAUSE_PLAYBACK));
        assert!(grounded.contains(&JUMP_TO_PAGE));
        assert!(grounded.contains(&LOOK_AT_PAGE));
        assert!(grounded.contains(&TEACHER_ANCHOR_TOOL));

        // BookDirect offers reading and navigation tools (8 tools: 6 core + 2 teacher)
        let book_direct = registry.names_for_mode(DispatchMode::BookDirect);
        assert_eq!(book_direct.len(), 9);
        assert!(book_direct.contains(&TURN_PAGE));
        assert!(!book_direct.contains(&PAUSE_PLAYBACK));
        assert!(!book_direct.contains(&RESUME_PLAYBACK));
        assert!(!book_direct.contains(&SEEK_RELATIVE));
        assert!(!book_direct.contains(&SET_PLAYBACK_SPEED));
        assert!(!book_direct.contains(&REPLAY_LAST_MINUTE));
        assert!(!book_direct.contains(&REPLAY_PREVIOUS_PARAGRAPH));
        assert!(!book_direct.contains(&REPLAY_PREVIOUS_PAGE));
        assert!(book_direct.contains(&JUMP_TO_CHAPTER));
        assert!(book_direct.contains(&JUMP_TO_PAGE));
        assert!(book_direct.contains(&LOOK_AT_PAGE));
        assert!(book_direct.contains(&TEACHER_ANCHOR_TOOL));
        assert!(book_direct.contains(&UPDATE_LEARNING_PROFILE));

        // Standalone offers only learning profile (1 tool)
        let standalone = registry.names_for_mode(DispatchMode::Standalone);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0], UPDATE_LEARNING_PROFILE);
    }
}
