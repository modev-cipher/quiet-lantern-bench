//! Behavioral evaluation and assertion harness for the Voice Tutor (Slices S1-S6).
//!
//! Provides automated evaluation routines to verify the three core behavioral invariants:
//! 1. Autonomous Navigation upon topical progression without passive hesitation.
//! 2. Zero-Leakage of internal machinery and tool names.
//! 3. Visual-Verbal Grounding Synchronization with dialect-safe deixis detection and viewport idempotency.
//!
//! Owner ruling 2026-10-01: the harness grades DETERMINISTIC contracts only
//! (tool execution, viewport sync, machinery leaks). Natural-language social
//! behaviors (offers, apologies, register) are NOT regex-graded here —
//! phrase blacklists were brittle, unmaintainable, and overfitted to one
//! dialect. The prompt carries those rules as universal, language-agnostic
//! invariants; the journal and field sessions are the evidence loop.

use regex::Regex;

/// Represents a location in the document reader's viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportPosition {
    pub chapter: Option<usize>,
    pub page: Option<u32>,
}

impl ViewportPosition {
    pub fn new(chapter: Option<usize>, page: Option<u32>) -> Self {
        Self { chapter, page }
    }

    /// Check if this viewport matches a target position.
    /// If both specify a page, pages must match.
    /// If only chapters are specified, chapters must match.
    pub fn matches(&self, other: &Self) -> bool {
        if let (Some(p1), Some(p2)) = (self.page, other.page) {
            if p1 == p2 {
                return true;
            }
        }
        if let (Some(c1), Some(c2)) = (self.chapter, other.chapter) {
            if c1 == c2 && (self.page.is_none() || other.page.is_none()) {
                return true;
            }
        }
        self.chapter == other.chapter && self.page == other.page
    }
}

/// Recorded tool call from a session transcript or mock channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Violations detected during behavioral grading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BehavioralViolation {
    MachineryLeakage {
        pattern: String,
        matched_text: String,
    },
    UnsynchronizedVisualDeixis {
        marker: String,
        matched_text: String,
        current_viewport: ViewportPosition,
        target_viewport: Option<ViewportPosition>,
    },
    MissingAutonomousNavigation {
        target_viewport: ViewportPosition,
        current_viewport: ViewportPosition,
    },
    RogueAdvisoryNavigation {
        query: String,
        tool_call: String,
    },
    RogueCurricularJump {
        start_action: String,
        target: String,
        expected: String,
    },
}

impl std::fmt::Display for BehavioralViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MachineryLeakage {
                pattern,
                matched_text,
            } => {
                write!(
                    f,
                    "ZERO_LEAKAGE_VIOLATION (Machinery): matched pattern '{pattern}' in text '{matched_text}'"
                )
            }
            Self::UnsynchronizedVisualDeixis {
                marker,
                matched_text,
                current_viewport,
                target_viewport,
            } => {
                write!(
                    f,
                    "VISUAL_GROUNDING_VIOLATION: spoken deictic marker '{marker}' in '{matched_text}' while viewport at {:?} is unsynchronized with target {:?}",
                    current_viewport, target_viewport
                )
            }
            Self::MissingAutonomousNavigation {
                target_viewport,
                current_viewport,
            } => {
                write!(
                    f,
                    "AUTONOMOUS_NAVIGATION_VIOLATION: topic shifted to {:?} but viewport remains at {:?} without navigation tool call",
                    target_viewport, current_viewport
                )
            }
            Self::RogueAdvisoryNavigation { query, tool_call } => {
                write!(
                    f,
                    "PHASE_A_VIOLATION (Rogue Navigation): advisory query '{query}' executed navigation tool '{tool_call}' without user direction"
                )
            }
            Self::RogueCurricularJump {
                start_action,
                target,
                expected,
            } => {
                write!(
                    f,
                    "PHASE_B_VIOLATION (Rogue Curricular Jump): start action '{start_action}' jumped to '{target}' instead of expected initial section '{expected}'"
                )
            }
        }
    }
}

impl std::error::Error for BehavioralViolation {}

/// Prohibited internal machinery leakage (tool names and manual turn-by-hand instructions).
const MACHINERY_LEAKAGE_PATTERNS: &[&str] = &[
    r"\bjump_to_page\b",
    r"\bjump_to_chapter\b",
    r"\bsearch_book\b",
    r"\blook_at_page\b",
    r"\bteacher_anchor\b",
    r"\bsummarize_section\b",
    r"\bget_context_around_position\b",
    r"\bget_context_for_range\b",
    r"(?i)turn to .* by hand",
    r"(?i)navigate to .* by hand",
    r"(?i)flip to .* manually",
    r"(?i)scroll to .* by yourself",
    r"أستطيع استخدام أداة",
    r"أداة الـ",
    r"انتقل بنفسك",
    r"اقلب الصفحة بنفسك",
];

/// Explicit visual and document deictic markers (Safe against colloquial dialect idioms).
///
/// NOTE on Egyptian Arabic & Colloquial Dialects:
/// Conversational idioms like "زي ما إنت شايف" or "شايف الفكرة" mean "as is evident"
/// or "you know", and are NOT ocular deixis. Only explicit visual artifact citations
/// ("بص في الرسمة", "في الشكل ده", "في الصفحة دي") are flagged here.
const EXPLICIT_DEIXIS_PATTERNS: &[&str] = &[
    r"(?i)\blook at the figure\b",
    r"(?i)\bin this diagram\b",
    r"(?i)\bin the diagram (below|above)\b",
    r"(?i)\bon this page we see\b",
    r"(?i)\bon page \d+ you can see\b",
    r"(?i)\bas shown in the figure\b",
    r"(?i)\bas seen in the chart\b",
    r"(?i)\bnotice the figure\b",
    r"بص في الرسمة",
    r"بص في الشكل",
    r"في الشكل ده",
    r"في الرسمة دي",
    r"في الصفحة دي زي ما باين",
    r"زي ما هو موضح في الشكل",
    r"زي ما موضح في الرسمة",
    r"على الشاشة دلوقتي",
    r"شوف على الشاشة",
];

/// Suite A: Evaluates autonomous navigation on topical shift.
///
/// Enforces Viewport Idempotency: if `current_viewport` already matches `target_viewport`,
/// no tool call is required and the check succeeds.
pub fn evaluate_autonomous_navigation(
    current_viewport: &ViewportPosition,
    target_viewport: &ViewportPosition,
    tool_calls: &[RecordedToolCall],
) -> Result<(), BehavioralViolation> {
    if current_viewport.matches(target_viewport) {
        return Ok(());
    }

    let navigated = tool_calls.iter().any(|call| match call.name.as_str() {
        "jump_to_chapter" => {
            if let Some(target_ch) = target_viewport.chapter {
                let ch_val = call
                    .arguments
                    .get("chapter_index")
                    .or_else(|| call.arguments.get("chapter"))
                    .and_then(|v| v.as_u64());
                if let Some(ch_val) = ch_val {
                    return ch_val as usize == target_ch;
                }
            }
            false
        }
        "jump_to_page" => {
            if let Some(target_pg) = target_viewport.page {
                if let Some(pg_val) = call.arguments.get("page").and_then(|v| v.as_u64()) {
                    return pg_val as u32 == target_pg;
                }
            }
            false
        }
        _ => false,
    });

    if navigated {
        Ok(())
    } else {
        Err(BehavioralViolation::MissingAutonomousNavigation {
            target_viewport: target_viewport.clone(),
            current_viewport: current_viewport.clone(),
        })
    }
}

/// Suite B: Evaluates zero machinery leakage.
///
/// Owner ruling 2026-10-01: this suite grades ONLY deterministic
/// machinery leaks (our own tool names, manual-operation instructions).
/// Natural-language social phrasing is out of regex scope by design.
pub fn evaluate_zero_leakage(text: &str) -> Result<(), BehavioralViolation> {
    for pattern in MACHINERY_LEAKAGE_PATTERNS {
        let re = Regex::new(pattern).unwrap();
        if let Some(mat) = re.find(text) {
            return Err(BehavioralViolation::MachineryLeakage {
                pattern: pattern.to_string(),
                matched_text: mat.as_str().to_string(),
            });
        }
    }

    Ok(())
}

/// Suite C: Evaluates visual-verbal grounding synchronization.
///
/// If explicit visual deixis is spoken:
/// 1. If viewport is already at target, succeeds (idempotent).
/// 2. If a navigation tool call was executed in this turn, succeeds.
/// 3. Otherwise, fails with `UnsynchronizedVisualDeixis`.
pub fn evaluate_visual_verbal_grounding(
    text: &str,
    current_viewport: &ViewportPosition,
    target_viewport: Option<&ViewportPosition>,
    navigation_executed: bool,
) -> Result<(), BehavioralViolation> {
    let mut detected_marker = None;
    for pattern in EXPLICIT_DEIXIS_PATTERNS {
        let re = Regex::new(pattern).unwrap();
        if let Some(mat) = re.find(text) {
            detected_marker = Some((pattern.to_string(), mat.as_str().to_string()));
            break;
        }
    }

    let (pattern, matched_text) = match detected_marker {
        Some(m) => m,
        None => return Ok(()),
    };

    let already_at_target = match target_viewport {
        Some(target) => current_viewport.matches(target),
        None => false,
    };

    if already_at_target || navigation_executed {
        Ok(())
    } else {
        Err(BehavioralViolation::UnsynchronizedVisualDeixis {
            marker: pattern,
            matched_text,
            current_viewport: current_viewport.clone(),
            target_viewport: target_viewport.cloned(),
        })
    }
}

/// Prohibited navigation tools during Phase A advisory / meta queries.
const NAVIGATION_TOOL_NAMES: &[&str] = &["jump_to_chapter", "jump_to_page"];

/// Patterns identifying Phase A advisory queries (suitability, opinion, overview, recommendations).
const ADVISORY_QUERY_PATTERNS: &[&str] = &[
    r"(?i)is (this|the) book (suitable|good|useful|right|worth)",
    r"(?i)what (do you think of|is your opinion on) (this|the) book",
    r"(?i)(overview|summary|outline) of (the|this) book",
    r"(?i)what are the (prerequisites|topics)",
    r"(?i)where should (we|i) (begin|start)",
    r"(?i)who is this book for",
    r"هل الكتاب ده? (مناسب|مفيد|كويس)",
    r"ما رأيك في الكتاب",
    r"إيه رأيك في الكتاب",
    r"اديني نبذة (عامة )?عن",
    r"إيه محتويات الكتاب",
    r"من فين نبدأ",
    r"من أين نبدأ",
];

/// Determines if a user query is a Phase A advisory / orientation query.
pub fn is_advisory_query(query: &str) -> bool {
    ADVISORY_QUERY_PATTERNS.iter().any(|pattern| {
        Regex::new(pattern)
            .map(|re| re.is_match(query))
            .unwrap_or(false)
    })
}

/// Suite D: Evaluates Phase A advisory queries to assert STRICTLY ZERO navigation tool calls.
///
/// In Phase A, the tutor must answer verbally as a mentor and never invoke navigation tools.
pub fn evaluate_advisory_navigation(
    query: &str,
    tool_calls: &[RecordedToolCall],
) -> Result<(), BehavioralViolation> {
    if !is_advisory_query(query) {
        return Ok(());
    }

    for call in tool_calls {
        if NAVIGATION_TOOL_NAMES.contains(&call.name.as_str()) {
            return Err(BehavioralViolation::RogueAdvisoryNavigation {
                query: query.to_string(),
                tool_call: call.name.clone(),
            });
        }
    }

    Ok(())
}

/// Affirmations that trigger starting Phase B instruction.
const START_AFFIRMATION_PATTERNS: &[&str] = &[
    r"(?i)\b(let's start|let's dive in|let's go|start reading|start with chapter 1)\b",
    r"يلا بينا",
    r"يلا نبدأ",
    r"يلا نبدا",
    r"نبدأ بقى",
    r"ابدأ",
    r"توكل على الله",
];

/// Determines if an utterance is an affirmation to start instruction.
pub fn is_start_affirmation(text: &str) -> bool {
    START_AFFIRMATION_PATTERNS.iter().any(|pattern| {
        Regex::new(pattern)
            .map(|re| re.is_match(text))
            .unwrap_or(false)
    })
}

/// Suite E: Evaluates Phase B start confirmation from cover to assert curricular sequence.
///
/// When the user confirms starting from cover / outline, navigation must lead to the
/// expected initial section (e.g. Chapter 1 / Section 1.1), NOT a random mid-book chapter.
pub fn evaluate_curricular_start_sequence(
    start_text: &str,
    current_viewport: &ViewportPosition,
    expected_start: &ViewportPosition,
    tool_calls: &[RecordedToolCall],
) -> Result<(), BehavioralViolation> {
    if !is_start_affirmation(start_text) {
        return Ok(());
    }

    for call in tool_calls {
        match call.name.as_str() {
            "jump_to_chapter" => {
                let ch_val = call
                    .arguments
                    .get("chapter_index")
                    .or_else(|| call.arguments.get("chapter"))
                    .and_then(|v| v.as_u64());
                if let Some(ch) = ch_val {
                    let ch = ch as usize;
                    if let Some(exp_ch) = expected_start.chapter {
                        if ch != exp_ch {
                            return Err(BehavioralViolation::RogueCurricularJump {
                                start_action: start_text.to_string(),
                                target: format!("chapter {ch}"),
                                expected: format!("chapter {exp_ch}"),
                            });
                        }
                    }
                }
            }
            "jump_to_page" => {
                if let Some(pg) = call.arguments.get("page").and_then(|v| v.as_u64()) {
                    let pg = pg as u32;
                    if let Some(exp_pg) = expected_start.page {
                        if pg != exp_pg {
                            return Err(BehavioralViolation::RogueCurricularJump {
                                start_action: start_text.to_string(),
                                target: format!("page {pg}"),
                                expected: format!("page {exp_pg}"),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !current_viewport.matches(expected_start) {
        let has_nav = tool_calls
            .iter()
            .any(|c| NAVIGATION_TOOL_NAMES.contains(&c.name.as_str()));
        if !has_nav {
            return Err(BehavioralViolation::MissingAutonomousNavigation {
                target_viewport: expected_start.clone(),
                current_viewport: current_viewport.clone(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn suite_a_passes_when_autonomous_jump_is_called() {
        let current = ViewportPosition::new(Some(0), Some(1));
        let target = ViewportPosition::new(Some(1), Some(14));
        let tools = vec![RecordedToolCall {
            name: "jump_to_page".to_string(),
            arguments: json!({ "page": 14 }),
        }];

        assert!(evaluate_autonomous_navigation(&current, &target, &tools).is_ok());
    }

    #[test]
    fn suite_a_fails_when_navigation_is_missing() {
        let current = ViewportPosition::new(Some(0), Some(1));
        let target = ViewportPosition::new(Some(1), Some(14));
        let tools = vec![];

        let res = evaluate_autonomous_navigation(&current, &target, &tools);
        assert!(matches!(
            res,
            Err(BehavioralViolation::MissingAutonomousNavigation { .. })
        ));
    }

    #[test]
    fn suite_a_satisfies_viewport_idempotency_without_tools() {
        let current = ViewportPosition::new(Some(1), Some(14));
        let target = ViewportPosition::new(Some(1), Some(14));
        let tools = vec![];

        assert!(evaluate_autonomous_navigation(&current, &target, &tools).is_ok());
    }

    #[test]
    fn suite_a_continuous_inter_section_navigation() {
        let current = ViewportPosition::new(Some(15), Some(73));
        let target = ViewportPosition::new(Some(16), Some(76));

        let res = evaluate_autonomous_navigation(&current, &target, &[]);
        assert!(matches!(
            res,
            Err(BehavioralViolation::MissingAutonomousNavigation { .. })
        ));

        let tools_ch = vec![RecordedToolCall {
            name: "jump_to_chapter".to_string(),
            arguments: json!({ "chapter": 16 }),
        }];
        assert!(evaluate_autonomous_navigation(&current, &target, &tools_ch).is_ok());

        let tools_ch_idx = vec![RecordedToolCall {
            name: "jump_to_chapter".to_string(),
            arguments: json!({ "chapter_index": 16 }),
        }];
        assert!(evaluate_autonomous_navigation(&current, &target, &tools_ch_idx).is_ok());

        let tools_pg = vec![RecordedToolCall {
            name: "jump_to_page".to_string(),
            arguments: json!({ "page": 76 }),
        }];
        assert!(evaluate_autonomous_navigation(&current, &target, &tools_pg).is_ok());

        assert!(evaluate_autonomous_navigation(&current, &current, &[]).is_ok());
    }

    #[test]
    fn suite_b_detects_machinery_leakage() {
        let bad_tool = "I will invoke jump_to_page to get you there.";
        assert!(matches!(
            evaluate_zero_leakage(bad_tool),
            Err(BehavioralViolation::MachineryLeakage { .. })
        ));

        let bad_manual = "Please turn to page 14 by hand so you can see.";
        assert!(matches!(
            evaluate_zero_leakage(bad_manual),
            Err(BehavioralViolation::MachineryLeakage { .. })
        ));
    }

    #[test]
    fn suite_b_allows_natural_conversational_teaching() {
        let natural_en = "Let's dive into Section 1.1: Information is Bits + Context. Notice how every character is represented.";
        assert!(evaluate_zero_leakage(natural_en).is_ok());

        let natural_ar = "يلا بينا نبدأ في الفصل الأول: المعلومات عبارة عن بتات وسياق.";
        assert!(evaluate_zero_leakage(natural_ar).is_ok());
    }

    #[test]
    fn suite_c_prevents_colloquial_arabic_false_positives() {
        // Colloquial idioms like "زي ما إنت شايف" or "شايف الفكرة" are figurative and must NOT trigger deixis violations
        let colloquial_text =
            "زي ما إنت شايف، الفكرة كلها بتعتمد على السياق، وشايف إزاي النظام بيشتغل.";
        let current = ViewportPosition::new(Some(0), Some(1));
        let target = ViewportPosition::new(Some(1), Some(14));

        assert!(
            evaluate_visual_verbal_grounding(colloquial_text, &current, Some(&target), false)
                .is_ok()
        );
    }

    #[test]
    fn suite_c_flags_explicit_visual_deixis_when_viewport_unsynchronized() {
        let off_target_en = "As you can see, look at the figure to understand the byte layout.";
        let current = ViewportPosition::new(Some(0), Some(1));
        let target = ViewportPosition::new(Some(1), Some(14));

        assert!(matches!(
            evaluate_visual_verbal_grounding(off_target_en, &current, Some(&target), false),
            Err(BehavioralViolation::UnsynchronizedVisualDeixis { .. })
        ));

        let off_target_ar = "بص في الرسمة دي علشان تفهم توزيع البايتات.";
        assert!(matches!(
            evaluate_visual_verbal_grounding(off_target_ar, &current, Some(&target), false),
            Err(BehavioralViolation::UnsynchronizedVisualDeixis { .. })
        ));
    }

    #[test]
    fn suite_c_passes_explicit_visual_deixis_when_synchronized() {
        let text = "Look at the figure to understand the byte layout.";
        let current = ViewportPosition::new(Some(0), Some(1));
        let target = ViewportPosition::new(Some(1), Some(14));

        // Synchronized via navigation execution in this turn
        assert!(evaluate_visual_verbal_grounding(text, &current, Some(&target), true).is_ok());

        // Or already at target (idempotency)
        let at_target = ViewportPosition::new(Some(1), Some(14));
        assert!(evaluate_visual_verbal_grounding(text, &at_target, Some(&target), false).is_ok());
    }

    #[test]
    fn suite_d_advisory_queries_forbid_navigation_tools() {
        let query_ar = "هل الكتاب ده مناسب ليا كـ software engineer مهتم بالـ hardware؟";
        assert!(is_advisory_query(query_ar));
        // Verbal answer without tool calls -> passes
        assert!(evaluate_advisory_navigation(query_ar, &[]).is_ok());

        let query_opinion = "ما رأيك في الكتاب؟";
        assert!(is_advisory_query(query_opinion));
        assert!(evaluate_advisory_navigation(query_opinion, &[]).is_ok());

        let query_en = "Is this book suitable for me as a backend engineer?";
        assert!(is_advisory_query(query_en));
        assert!(evaluate_advisory_navigation(query_en, &[]).is_ok());

        // Rogue tool call during advisory query -> fails
        let rogue_tools = vec![RecordedToolCall {
            name: "jump_to_chapter".to_string(),
            arguments: json!({ "chapter": 8 }),
        }];
        let res = evaluate_advisory_navigation(query_ar, &rogue_tools);
        assert!(matches!(
            res,
            Err(BehavioralViolation::RogueAdvisoryNavigation { .. })
        ));

        let rogue_page = vec![RecordedToolCall {
            name: "jump_to_page".to_string(),
            arguments: json!({ "page": 150 }),
        }];
        let res_pg = evaluate_advisory_navigation(query_en, &rogue_page);
        assert!(matches!(
            res_pg,
            Err(BehavioralViolation::RogueAdvisoryNavigation { .. })
        ));
    }

    #[test]
    fn suite_e_start_affirmation_from_cover_enforces_curricular_sequence() {
        let cover_viewport = ViewportPosition::new(Some(0), Some(1));
        let expected_start = ViewportPosition::new(Some(15), Some(73)); // Section 1.1

        // Valid start: jumping to Section 1.1 (chapter 15 or page 73)
        let valid_tools_ch = vec![RecordedToolCall {
            name: "jump_to_chapter".to_string(),
            arguments: json!({ "chapter": 15 }),
        }];
        assert!(evaluate_curricular_start_sequence(
            "يلا بينا",
            &cover_viewport,
            &expected_start,
            &valid_tools_ch,
        )
        .is_ok());

        let valid_tools_pg = vec![RecordedToolCall {
            name: "jump_to_page".to_string(),
            arguments: json!({ "page": 73 }),
        }];
        assert!(evaluate_curricular_start_sequence(
            "let's dive in",
            &cover_viewport,
            &expected_start,
            &valid_tools_pg,
        )
        .is_ok());

        // Rogue jump: jumping to arbitrary mid-book Chapter 8
        let rogue_ch8 = vec![RecordedToolCall {
            name: "jump_to_chapter".to_string(),
            arguments: json!({ "chapter": 8 }),
        }];
        let res = evaluate_curricular_start_sequence(
            "يلا بينا",
            &cover_viewport,
            &expected_start,
            &rogue_ch8,
        );
        assert!(matches!(
            res,
            Err(BehavioralViolation::RogueCurricularJump { .. })
        ));

        // Rogue jump: jumping to arbitrary page 500
        let rogue_p500 = vec![RecordedToolCall {
            name: "jump_to_page".to_string(),
            arguments: json!({ "page": 500 }),
        }];
        let res_pg = evaluate_curricular_start_sequence(
            "يلا بينا",
            &cover_viewport,
            &expected_start,
            &rogue_p500,
        );
        assert!(matches!(
            res_pg,
            Err(BehavioralViolation::RogueCurricularJump { .. })
        ));

        // Missing navigation when starting from cover
        let res_missing =
            evaluate_curricular_start_sequence("يلا بينا", &cover_viewport, &expected_start, &[]);
        assert!(matches!(
            res_missing,
            Err(BehavioralViolation::MissingAutonomousNavigation { .. })
        ));
    }
}
