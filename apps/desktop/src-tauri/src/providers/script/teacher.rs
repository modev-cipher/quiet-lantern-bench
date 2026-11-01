//! Port of `engine/txtcaster_engine/script/teacher.py` — the Teacher script
//! contract for the cloud path (charter §3.4). Prompts, constants, budgets,
//! JSON-array extraction, marker validation, retry ladder, chunk halving,
//! rolling summary, and narrator fallback are verbatim ports; the fixture
//! corpus below pins the two implementations together.
//!
//! One deliberate divergence, required by charter §5.5/A19: the engine's
//! blanket `except Exception → retry → fallback` is split by failure class.
//! Malformed OUTPUT (unparseable JSON, broken markers) walks the ladder —
//! strict repair, chunk halving, narrator fallback — exactly as today,
//! because that is recorded product behavior. Provider/key/network failure
//! (auth, quota, cancellation immediately; transport/timeout/5xx after the
//! bounded attempts) BUBBLES UP so the stage pauses for user action —
//! narrator fallback on an unreachable provider would be silent switching.

use crate::providers::registry::{ScriptError, ScriptErrorKind, ScriptLlm, ScriptRequest};

use super::chunker::{chunk_units, Chunk, SourceUnit, CHARS_PER_TOKEN};
use super::cues::decide_code;

pub const NARRATOR_FALLBACK_FLAG: &str = "fallback_narrator";
pub const MAX_ATTEMPTS: usize = 3;
pub const RETRY_TEMPERATURE: f32 = 0.3;
pub const MIN_OUTPUT_TOKENS: usize = 512;
pub const MIN_CLOUD_OUTPUT_TOKENS: usize = 2048;
pub const CTX_SAFETY_TOKENS: usize = 64;
pub const TEACHER_OUTPUT_RATIO: f64 = 1.5;
pub const TEACHER_PROMPT_OVERHEAD_TOKENS: usize = 800;
pub const MIN_TEACHER_CHUNK_TOKENS: usize = 128;
pub const MAX_TEACHER_CHUNK_CHARS: usize = 6_000;
pub const SUMMARY_MAX_TOKENS: usize = 400;
pub const SUMMARY_MAX_CHARS: usize = SUMMARY_MAX_TOKENS * 4;
pub const MARKER_DETAIL_MAX_CHARS: usize = 500;

pub const TEACHER_PERSONA: &str =
    "You are a patient teacher recording an audio lecture. Re-explain the source \
material below as a clear spoken lecture: simplify the language, explain each \
key concept, and add one short contextual example per key concept. Keep poetry \
and quotations verbatim. Never introduce facts, numbers, names, or claims that \
are not present in the source. For text between [CODE] and [/CODE], briefly \
describe what the code does in one sentence; never read code aloud character \
by character.";
pub const FIGURE_RULE: &str =
    "Each [FIGURE:id] block gives a figure's type, caption and description. Teach that \
figure in your own words as part of the lecture. Preserve every [FIGURE:id] marker \
exactly once, immediately before the sentence where you begin explaining it.";
pub const OUTPUT_RULE: &str = "Output ONLY a JSON array of objects, each with a \"text\" field \
containing one spoken paragraph: [{\"text\": \"...\"}]";
pub const STRICT_SUFFIX: &str =
    "\nYour previous answer was not valid JSON. Respond with NOTHING except a valid \
JSON array of objects with a \"text\" string field. No prose, no code fences.";
pub const MARKER_SUFFIX_PREFIX: &str = "\nYour previous answer misused figure markers (";
pub const MARKER_SUFFIX_TAIL: &str = "). Each marker from the \
source may appear AT MOST once, copied exactly; never invent new markers.";

fn summary_prompt(summary: &str, lecture: &str) -> String {
    format!(
        "Summarize the lecture so far in plain text, at most 300 words, keeping the key \
concepts and terminology so the next part of the lecture stays consistent.\n\n\
Previous summary:\n{summary}\n\nLatest lecture part:\n{lecture}\n\nSummary:"
    )
}

fn marker_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\[FIGURE:([^\]]+)]").expect("marker regex"))
}

fn marker_like_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\[\s*FIGURE\b").expect("marker-like regex"))
}

fn fence_trimmed(raw: &str) -> String {
    let mut cleaned = raw.trim().to_string();
    if cleaned.starts_with("```") {
        if let Some(newline) = cleaned.find('\n') {
            let fence: String = cleaned[3..newline].to_string();
            if fence.chars().all(|c| c.is_ascii_alphabetic()) {
                cleaned = cleaned[newline + 1..].to_string();
            }
        }
    }
    let trimmed = cleaned.trim_end();
    if let Some(stripped) = trimmed.strip_suffix("```") {
        cleaned = stripped.trim_end_matches('\n').to_string();
    }
    cleaned.trim().to_string()
}

pub fn teacher_chunk_budget_chars(ctx: usize) -> usize {
    let usable = ctx.saturating_sub(TEACHER_PROMPT_OVERHEAD_TOKENS + CTX_SAFETY_TOKENS);
    let source_tokens = (usable as f64 / (1.0 + TEACHER_OUTPUT_RATIO)) as usize;
    (source_tokens.max(MIN_TEACHER_CHUNK_TOKENS) * CHARS_PER_TOKEN).min(MAX_TEACHER_CHUNK_CHARS)
}

pub fn output_budget_tokens(source_chars: usize, prompt_chars: usize, ctx: usize) -> usize {
    let source_tokens = source_chars / CHARS_PER_TOKEN;
    let min_output = if ctx >= 16_384 {
        MIN_CLOUD_OUTPUT_TOKENS
    } else {
        MIN_OUTPUT_TOKENS
    };
    let desired = min_output.max((source_tokens as f64 * TEACHER_OUTPUT_RATIO) as usize);
    let prompt_tokens = prompt_chars / CHARS_PER_TOKEN;
    let remaining = ctx.saturating_sub(prompt_tokens + CTX_SAFETY_TOKENS);
    desired.min(remaining).max(1)
}

pub fn build_prompt(
    summary: &str,
    source: &str,
    strict: bool,
    marker_detail: &str,
    has_figures: bool,
) -> String {
    let mut rules = vec![TEACHER_PERSONA];
    if has_figures {
        rules.push(FIGURE_RULE);
    }
    rules.push(OUTPUT_RULE);
    let mut parts = vec![rules.join(" ")];
    if strict {
        if marker_detail.is_empty() {
            parts.push(STRICT_SUFFIX.to_string());
        } else {
            parts.push(format!(
                "{MARKER_SUFFIX_PREFIX}{marker_detail}{MARKER_SUFFIX_TAIL}"
            ));
        }
    }
    if !summary.is_empty() {
        parts.push(format!("\nCONTEXT SUMMARY (previous material):\n{summary}"));
    }
    parts.push(format!("\nSOURCE:\n{source}\n\nJSON:"));
    parts.join("\n")
}

pub fn parse_lecture(raw: &str) -> Option<Vec<String>> {
    let parse_items = |s: &str| -> Option<Vec<String>> {
        let data: serde_json::Value = serde_json::from_str(s).ok()?;
        let items = data.as_array()?;
        if items.is_empty() {
            return None;
        }
        let mut texts = Vec::with_capacity(items.len());
        for item in items {
            let text = item.as_object()?.get("text")?.as_str()?;
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            texts.push(text.to_string());
        }
        Some(texts)
    };

    let trimmed = raw.trim();
    if let Some(texts) = parse_items(trimmed) {
        return Some(texts);
    }

    let cleaned = fence_trimmed(trimmed);
    let start = cleaned.find('[')?;
    let end = cleaned.rfind(']')?;
    if end <= start {
        return None;
    }
    parse_items(&cleaned[start..=end])
}

pub fn figure_prompt_block(unit: &SourceUnit) -> String {
    let mut lines = Vec::new();
    if let Some(region_type) = &unit.region_type {
        lines.push(format!("type: {region_type}"));
    }
    if let Some(caption) = &unit.caption {
        lines.push(format!("caption: {}", caption.trim()));
    }
    if let Some(explanation) = &unit.explanation {
        lines.push(format!("description: {}", explanation.trim()));
    }
    lines.join("\n")
}

pub fn chunk_source(chunk: &Chunk) -> String {
    chunk
        .units
        .iter()
        .map(|unit| {
            if unit.kind == "text" && decide_code(unit.label.as_deref(), &unit.text) {
                format!("[CODE]\n{}\n[/CODE]", unit.text)
            } else if unit.kind == "figure" && unit.figure_id.is_some() {
                format!(
                    "[FIGURE:{}]\n{}",
                    unit.figure_id.as_deref().expect("checked"),
                    figure_prompt_block(unit)
                )
            } else {
                unit.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn marker_problems(
    markers: &[String],
    figure_ids: &std::collections::BTreeSet<String>,
    mangled: usize,
) -> String {
    let mut problems: Vec<String> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut unknown = 0usize;
    for marker in markers {
        if figure_ids.contains(marker) {
            if seen.contains(marker) {
                problems.push(format!("[FIGURE:{marker}] appears more than once"));
            }
            seen.insert(marker.clone());
        } else {
            unknown += 1;
        }
    }
    if unknown > 0 {
        problems.push(format!(
            "{unknown} marker(s) are not markers from the source"
        ));
    }
    if mangled > 0 {
        problems.push(format!(
            "{mangled} marker(s) have broken [FIGURE:id] syntax"
        ));
    }
    let mut deduped: Vec<String> = Vec::new();
    for problem in problems {
        if !deduped.contains(&problem) {
            deduped.push(problem);
        }
    }
    deduped
        .join("; ")
        .chars()
        .take(MARKER_DETAIL_MAX_CHARS)
        .collect()
}

/// One chunk's contract outcome: a valid lecture with its marker tallies, or
/// the recorded narrator fallback. Segmentation happens engine-side from
/// these facts.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ChunkOutcome {
    Lecture {
        units: Vec<SourceUnit>,
        texts: Vec<String>,
        markers_dropped: u64,
        markers_omitted: u64,
    },
    NarratorFallback {
        units: Vec<SourceUnit>,
    },
}

fn figure_ids_of(units: &[SourceUnit]) -> std::collections::BTreeSet<String> {
    units
        .iter()
        .filter(|unit| unit.kind == "figure")
        .filter_map(|unit| unit.figure_id.clone())
        .collect()
}

fn split_chunk(chunk: &Chunk) -> Vec<Chunk> {
    let middle = chunk.units.len() / 2;
    vec![
        Chunk {
            idx: chunk.idx,
            chapter_idx: chunk.chapter_idx,
            page_no: chunk.page_no,
            units: chunk.units[..middle].to_vec(),
        },
        Chunk {
            idx: chunk.idx,
            chapter_idx: chunk.chapter_idx,
            page_no: chunk.page_no,
            units: chunk.units[middle..].to_vec(),
        },
    ]
}

/// Charter §5.5 failure split: which adapter errors end the stage now
/// (pause for user action), and which are ladder-retryable within the
fn stage_ending(error: &ScriptError) -> bool {
    let lower = error.message.to_ascii_lowercase();
    matches!(
        error.kind,
        ScriptErrorKind::Auth | ScriptErrorKind::Quota | ScriptErrorKind::Cancelled
    ) || matches!(error.status, Some(429 | 503 | 529))
        || lower.contains("unavailable")
        || lower.contains("overloaded")
        || lower.contains("high demand")
}

async fn generate_lecture(
    llm: &mut dyn ScriptLlm,
    summary: &str,
    source: &str,
    ctx: usize,
    figure_ids: &std::collections::BTreeSet<String>,
) -> Result<Option<(Vec<String>, u64, u64)>, ScriptError> {
    let mut marker_detail = String::new();
    let mut last_provider_error: Option<ScriptError> = None;
    for attempt in 0..MAX_ATTEMPTS {
        let prompt = build_prompt(
            summary,
            source,
            attempt > 0,
            &marker_detail,
            !figure_ids.is_empty(),
        );
        let budget = output_budget_tokens(source.chars().count(), prompt.chars().count(), ctx);
        let temperature = if attempt == 0 { 0.0 } else { RETRY_TEMPERATURE };
        let raw = match llm
            .generate(ScriptRequest {
                system: String::new(),
                user: prompt,
                max_output_tokens: budget as u32,
                temperature,
                effort: Default::default(),
            })
            .await
        {
            Ok(response) => response.text,
            Err(error) if stage_ending(&error) => return Err(error),
            Err(error) => {
                tracing::warn!(
                    seam = "providers",
                    reason = ?error.kind,
                    attempt = attempt + 1,
                    "teacher generation failed"
                );
                last_provider_error = Some(error);
                continue;
            }
        };
        last_provider_error = None;
        let Some(texts) = parse_lecture(&raw) else {
            marker_detail.clear();
            tracing::warn!(
                attempt = attempt + 1,
                raw_chars = raw.chars().count(),
                budget,
                "teacher output malformed"
            );
            continue;
        };
        let markers: Vec<String> = texts
            .iter()
            .flat_map(|text| {
                marker_re()
                    .captures_iter(text)
                    .map(|c| c[1].to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        let known: Vec<&String> = markers
            .iter()
            .filter(|marker| figure_ids.contains(*marker))
            .collect();
        let marker_like: usize = texts
            .iter()
            .map(|text| marker_like_re().find_iter(text).count())
            .sum();
        let mangled = marker_like as i64 - markers.len() as i64;
        if mangled <= 0 {
            let unique_known: std::collections::BTreeSet<&String> = known.iter().copied().collect();
            let dropped = (markers.len() - unique_known.len()) as u64;
            let omitted = figure_ids
                .iter()
                .filter(|id| !unique_known.contains(id))
                .count() as u64;
            return Ok(Some((texts, dropped, omitted)));
        }
        marker_detail = marker_problems(&markers, figure_ids, mangled.max(0) as usize);
        tracing::warn!(
            mangled = mangled.max(0),
            attempt = attempt + 1,
            "teacher output has broken marker syntax"
        );
    }
    if let Some(error) = last_provider_error {
        return Err(error);
    }
    Ok(None)
}

async fn update_summary(
    llm: &mut dyn ScriptLlm,
    summary: &str,
    lecture: &str,
    ctx: usize,
) -> Result<String, ScriptError> {
    let prompt = summary_prompt(
        if summary.is_empty() {
            "(none)"
        } else {
            summary
        },
        lecture,
    );
    let remaining =
        ctx.saturating_sub(prompt.chars().count() / CHARS_PER_TOKEN + CTX_SAFETY_TOKENS);
    let budget = SUMMARY_MAX_TOKENS.min(remaining).max(1);
    match llm
        .generate(ScriptRequest {
            system: String::new(),
            user: prompt,
            max_output_tokens: budget as u32,
            temperature: 0.0,
            effort: Default::default(),
        })
        .await
    {
        Ok(response) => {
            let updated = response.text.trim().to_string();
            if updated.is_empty() {
                Ok(summary.to_string())
            } else {
                Ok(updated.chars().take(SUMMARY_MAX_CHARS).collect())
            }
        }
        Err(error)
            if matches!(
                error.kind,
                ScriptErrorKind::Auth | ScriptErrorKind::Cancelled
            ) =>
        {
            Err(error)
        }
        Err(error) => {
            tracing::warn!(
                seam = "providers",
                reason = ?error.kind,
                "teacher summary update failed, keeping previous summary"
            );
            Ok(summary.to_string())
        }
    }
}

fn cancelled_error() -> ScriptError {
    ScriptError {
        kind: ScriptErrorKind::Cancelled,
        retryable: false,
        message: "cancelled".to_string(),
        retry_after_secs: None,
        status: None,
    }
}

/// Race one provider await against the cancellation token (S-3b, Hermes
/// MF11): winning the race DROPS the provider future, which aborts the
/// in-flight HTTP request — a stalled generation no longer holds a cancel
/// hostage for the client timeout.
async fn raced<T>(
    cancel: &mut crate::providers::registry::CancelToken,
    future: impl std::future::Future<Output = Result<T, ScriptError>>,
) -> Result<T, ScriptError> {
    tokio::select! {
        result = future => result,
        _ = cancel.cancelled() => Err(cancelled_error()),
    }
}

/// A completed source chunk's resumable facts: its outcomes (one, or two
/// after halving) and the rolling summary AFTER it.
#[derive(Debug, Clone)]
pub struct CompletedChunk {
    pub chunk_idx: usize,
    pub outcomes: Vec<ChunkOutcome>,
    pub summary: String,
}

/// The `teach_units` orchestration (teacher.py:297) over the phase trait:
/// chunk, generate with the ladder, halve on total malformed failure,
/// narrator-fallback the irrecoverable halves, and roll the summary forward.
///
/// Resume (MF11's never-re-pay guarantee): `resume` carries the completed
/// chunks a previous run persisted; those source chunks are skipped
/// entirely — no provider request is issued for them — and generation
/// continues from the recorded rolling summary. `on_chunk_complete` fires
/// at each safe boundary (a source chunk fully settled) so the caller can
/// persist durably before the next chunk spends anything.
pub async fn teach_chunks(
    units: &[SourceUnit],
    ctx: usize,
    llm: &mut dyn ScriptLlm,
    resume: Vec<CompletedChunk>,
    mut on_chunk: impl FnMut(usize, usize),
    cancel: &mut crate::providers::registry::CancelToken,
    mut on_chunk_complete: impl FnMut(&CompletedChunk),
) -> Result<Vec<ChunkOutcome>, ScriptError> {
    let chunks = chunk_units(units, teacher_chunk_budget_chars(ctx));
    let total = chunks.len();
    // A valid resume is a contiguous prefix; anything else is not resumable
    // and the caller must not have passed it.
    let resumed = resume.len().min(total);
    let mut outcomes: Vec<ChunkOutcome> = Vec::new();
    let mut summary = String::new();
    for completed in resume.iter().take(resumed) {
        outcomes.extend(completed.outcomes.iter().cloned());
        summary = completed.summary.clone();
    }
    on_chunk(resumed, total);
    for (done, chunk) in chunks.iter().enumerate().skip(resumed) {
        let chunk_idx = done;
        let done = done + 1;
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        let outcomes_before = outcomes.len();
        let figure_ids = figure_ids_of(&chunk.units);
        let lecture = raced(
            cancel,
            generate_lecture(llm, &summary, &chunk_source(chunk), ctx, &figure_ids),
        )
        .await?;
        match lecture {
            Some((texts, dropped, omitted)) => {
                if done < total {
                    summary = raced(
                        cancel,
                        update_summary(llm, &summary, &texts.join("\n\n"), ctx),
                    )
                    .await?;
                }
                outcomes.push(ChunkOutcome::Lecture {
                    units: chunk.units.clone(),
                    texts,
                    markers_dropped: dropped,
                    markers_omitted: omitted,
                });
            }
            None if chunk.units.len() > 1 => {
                let halves = split_chunk(chunk);
                tracing::info!(
                    chunk = done,
                    total,
                    halves = halves.len(),
                    "teacher retrying chunk as smaller chunks before giving up"
                );
                let mut recovered_texts: Vec<String> = Vec::new();
                for half in &halves {
                    let half_ids = figure_ids_of(&half.units);
                    let part = raced(
                        cancel,
                        generate_lecture(llm, &summary, &chunk_source(half), ctx, &half_ids),
                    )
                    .await?;
                    match part {
                        None => outcomes.push(ChunkOutcome::NarratorFallback {
                            units: half.units.clone(),
                        }),
                        Some((texts, dropped, omitted)) => {
                            recovered_texts.extend(texts.clone());
                            outcomes.push(ChunkOutcome::Lecture {
                                units: half.units.clone(),
                                texts,
                                markers_dropped: dropped,
                                markers_omitted: omitted,
                            });
                        }
                    }
                }
                if !recovered_texts.is_empty() && done < total {
                    summary = raced(
                        cancel,
                        update_summary(llm, &summary, &recovered_texts.join("\n\n"), ctx),
                    )
                    .await?;
                }
            }
            None => outcomes.push(ChunkOutcome::NarratorFallback {
                units: chunk.units.clone(),
            }),
        }
        on_chunk_complete(&CompletedChunk {
            chunk_idx,
            outcomes: outcomes[outcomes_before..].to_vec(),
            summary: summary.clone(),
        });
        on_chunk(done, total);
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::registry::{ScriptResponse, ScriptUsage};
    use async_trait::async_trait;

    fn corpus() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../../fixtures/script_contract/teacher_parity.json"
        ))
        .expect("parity corpus parses")
    }

    #[test]
    fn budgets_match_the_python_contract() {
        for case in corpus()["budgets"].as_array().unwrap() {
            let ctx = case["ctx"].as_u64().unwrap() as usize;
            assert_eq!(
                teacher_chunk_budget_chars(ctx) as u64,
                case["chunk_budget_chars"].as_u64().unwrap(),
                "chunk budget diverged at ctx={ctx}"
            );
        }
        for case in corpus()["output_budgets"].as_array().unwrap() {
            assert_eq!(
                output_budget_tokens(
                    case["source_chars"].as_u64().unwrap() as usize,
                    case["prompt_chars"].as_u64().unwrap() as usize,
                    case["ctx"].as_u64().unwrap() as usize,
                ) as u64,
                case["tokens"].as_u64().unwrap()
            );
        }
    }

    #[test]
    fn prompts_match_the_python_contract_byte_for_byte() {
        for case in corpus()["prompts"].as_array().unwrap() {
            assert_eq!(
                build_prompt(
                    case["summary"].as_str().unwrap(),
                    case["source"].as_str().unwrap(),
                    case["strict"].as_bool().unwrap(),
                    case["marker_detail"].as_str().unwrap(),
                    case["has_figures"].as_bool().unwrap(),
                ),
                case["prompt"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn lecture_parsing_matches_the_python_contract() {
        for case in corpus()["parse"].as_array().unwrap() {
            let expected: Option<Vec<String>> = case["texts"].as_array().map(|texts| {
                texts
                    .iter()
                    .map(|t| t.as_str().unwrap().to_string())
                    .collect()
            });
            assert_eq!(
                parse_lecture(case["raw"].as_str().unwrap()),
                expected,
                "parse diverged on {:?}",
                case["raw"]
            );
        }
    }

    #[test]
    fn chunk_source_matches_the_python_contract() {
        let units = vec![
            SourceUnit {
                chapter_idx: 0,
                page_no: 3,
                kind: "text".to_string(),
                text: "Plain prose paragraph about systems.".to_string(),
                figure_id: None,
                region_type: None,
                caption: None,
                explanation: None,
                block_index: None,
                label: None,
            },
            SourceUnit {
                chapter_idx: 0,
                page_no: 3,
                kind: "text".to_string(),
                text: "let x = f(y); return x;".to_string(),
                figure_id: None,
                region_type: None,
                caption: None,
                explanation: None,
                block_index: None,
                label: None,
            },
            SourceUnit {
                chapter_idx: 0,
                page_no: 3,
                kind: "figure".to_string(),
                text: "A figure appears here. Cap. Desc.".to_string(),
                figure_id: Some("fig-1".to_string()),
                region_type: Some("figure".to_string()),
                caption: Some("Cap".to_string()),
                explanation: Some("Desc".to_string()),
                block_index: None,
                label: None,
            },
            SourceUnit {
                chapter_idx: 0,
                page_no: 3,
                kind: "text".to_string(),
                text: "code labelled".to_string(),
                figure_id: None,
                region_type: None,
                caption: None,
                explanation: None,
                block_index: None,
                label: Some("code".to_string()),
            },
        ];
        let chunk = Chunk {
            idx: 0,
            chapter_idx: 0,
            page_no: 3,
            units,
        };
        assert_eq!(
            chunk_source(&chunk),
            corpus()["chunk_source"]["source"].as_str().unwrap()
        );
    }

    struct ScriptedLlm {
        responses: std::collections::VecDeque<Result<String, ScriptError>>,
        prompts: Vec<ScriptRequest>,
    }

    impl ScriptedLlm {
        fn new(responses: Vec<Result<String, ScriptError>>) -> Self {
            Self {
                responses: responses.into(),
                prompts: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl ScriptLlm for ScriptedLlm {
        async fn generate(
            &mut self,
            request: ScriptRequest,
        ) -> Result<ScriptResponse, ScriptError> {
            self.prompts.push(request);
            match self.responses.pop_front().expect("scripted response left") {
                Ok(text) => Ok(ScriptResponse {
                    text,
                    usage: ScriptUsage::default(),
                }),
                Err(error) => Err(error),
            }
        }
    }

    fn prose_unit(text: &str) -> SourceUnit {
        SourceUnit {
            chapter_idx: 0,
            page_no: 1,
            kind: "text".to_string(),
            text: text.to_string(),
            figure_id: None,
            region_type: None,
            caption: None,
            explanation: None,
            block_index: None,
            label: None,
        }
    }

    fn provider_error(kind: ScriptErrorKind) -> ScriptError {
        ScriptError {
            kind,
            retryable: true,
            message: "scripted".to_string(),
            retry_after_secs: None,
            status: None,
        }
    }

    #[tokio::test]
    async fn a_valid_first_attempt_is_one_lecture_at_temperature_zero() {
        let mut llm = ScriptedLlm::new(vec![Ok("[{\"text\": \"Lecture.\"}]".to_string())]);
        let outcomes = teach_chunks(
            &[prose_unit("Some source.")],
            4096,
            &mut llm,
            Vec::new(),
            |_, _| {},
            &mut crate::providers::registry::CancelToken::never(),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            outcomes,
            vec![ChunkOutcome::Lecture {
                units: vec![prose_unit("Some source.")],
                texts: vec!["Lecture.".to_string()],
                markers_dropped: 0,
                markers_omitted: 0,
            }]
        );
        assert_eq!(llm.prompts.len(), 1, "single chunk, no summary call");
        assert_eq!(llm.prompts[0].temperature, 0.0);
    }

    #[tokio::test]
    async fn malformed_output_walks_the_strict_ladder_then_narrator_falls_back() {
        let mut llm = ScriptedLlm::new(vec![
            Ok("not json".to_string()),
            Ok("still not json".to_string()),
            Ok("nope".to_string()),
        ]);
        let outcomes = teach_chunks(
            &[prose_unit("Some source.")],
            4096,
            &mut llm,
            Vec::new(),
            |_, _| {},
            &mut crate::providers::registry::CancelToken::never(),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            outcomes,
            vec![ChunkOutcome::NarratorFallback {
                units: vec![prose_unit("Some source.")],
            }]
        );
        assert_eq!(llm.prompts.len(), MAX_ATTEMPTS);
        assert!(llm.prompts[1].user.contains("was not valid JSON"));
        assert_eq!(llm.prompts[1].temperature, RETRY_TEMPERATURE);
    }

    #[tokio::test]
    async fn auth_failure_pauses_the_stage_and_never_narrator_falls_back() {
        let mut llm = ScriptedLlm::new(vec![Err(provider_error(ScriptErrorKind::Auth))]);
        let error = teach_chunks(
            &[prose_unit("Some source.")],
            4096,
            &mut llm,
            Vec::new(),
            |_, _| {},
            &mut crate::providers::registry::CancelToken::never(),
            |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Auth);
    }

    #[tokio::test]
    async fn exhausted_transport_retries_bubble_up_instead_of_falling_back() {
        let mut llm = ScriptedLlm::new(vec![
            Err(provider_error(ScriptErrorKind::Transport)),
            Err(provider_error(ScriptErrorKind::Transport)),
            Err(provider_error(ScriptErrorKind::Transport)),
        ]);
        let error = teach_chunks(
            &[prose_unit("Some source.")],
            4096,
            &mut llm,
            Vec::new(),
            |_, _| {},
            &mut crate::providers::registry::CancelToken::never(),
            |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Transport);
    }

    #[tokio::test]
    async fn marker_misuse_gets_the_marker_repair_prompt() {
        let figure = SourceUnit {
            chapter_idx: 0,
            page_no: 1,
            kind: "figure".to_string(),
            text: "cue".to_string(),
            figure_id: Some("f1".to_string()),
            region_type: Some("figure".to_string()),
            caption: None,
            explanation: None,
            block_index: None,
            label: None,
        };
        let mut llm = ScriptedLlm::new(vec![
            Ok("[{\"text\": \"Bad [figure marker here.\"}]".to_string()),
            Ok("[{\"text\": \"Good [FIGURE:f1] explained.\"}]".to_string()),
        ]);
        let outcomes = teach_chunks(
            std::slice::from_ref(&figure),
            4096,
            &mut llm,
            Vec::new(),
            |_, _| {},
            &mut crate::providers::registry::CancelToken::never(),
            |_| {},
        )
        .await
        .unwrap();
        assert!(matches!(&outcomes[0], ChunkOutcome::Lecture { texts, .. }
            if texts[0].contains("[FIGURE:f1]")));
        assert!(llm.prompts[1].user.contains("misused figure markers"));
    }

    /// A fake whose generation hangs forever once its scripted responses run
    /// out — models an in-flight provider request only cancellation can end.
    struct HangingLlm {
        scripted: ScriptedLlm,
        started: std::sync::Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl ScriptLlm for HangingLlm {
        async fn generate(
            &mut self,
            request: ScriptRequest,
        ) -> Result<ScriptResponse, ScriptError> {
            if self.scripted.responses.is_empty() {
                self.started.notify_one();
                std::future::pending::<()>().await;
                unreachable!("a dropped in-flight generation never resumes");
            }
            self.scripted.generate(request).await
        }
    }

    #[tokio::test]
    async fn a_resume_never_re_requests_completed_chunks() {
        // Two chunks at this ctx. The resume seed covers chunk 0, so the
        // provider must see ONLY chunk 1's prompt — the never-re-pay
        // guarantee, provable from the fake's prompt log.
        let long = "A sentence of source text. ".repeat(45);
        let units = vec![prose_unit(&long), prose_unit(&long)];
        let chunks = chunk_units(&units, teacher_chunk_budget_chars(2048));
        assert!(chunks.len() >= 2, "premise: a multi-chunk source");

        let mut llm = ScriptedLlm::new(vec![Ok("[{\"text\": \"Second lecture.\"}]".to_string())]);
        let seed = CompletedChunk {
            chunk_idx: 0,
            outcomes: vec![ChunkOutcome::Lecture {
                units: chunks[0].units.clone(),
                texts: vec!["First lecture, previously paid for.".to_string()],
                markers_dropped: 0,
                markers_omitted: 0,
            }],
            summary: "recorded rolling summary".to_string(),
        };
        let mut boundaries: Vec<usize> = Vec::new();
        let outcomes = teach_chunks(
            &units,
            2048,
            &mut llm,
            vec![seed],
            |_, _| {},
            &mut crate::providers::registry::CancelToken::never(),
            |completed| boundaries.push(completed.chunk_idx),
        )
        .await
        .expect("resumes");
        assert_eq!(outcomes.len(), 2, "seeded outcome + fresh outcome");
        assert!(matches!(
            &outcomes[0],
            ChunkOutcome::Lecture { texts, .. } if texts[0].contains("previously paid")
        ));
        assert_eq!(
            boundaries,
            vec![1],
            "only the fresh chunk crosses the boundary hook"
        );
        assert_eq!(
            llm.prompts.len(),
            1,
            "exactly one provider request: chunk 0 was never re-paid"
        );
        assert!(
            llm.prompts[0].user.contains("recorded rolling summary"),
            "generation continued from the RECORDED summary, not a recomputed one"
        );
    }

    #[tokio::test]
    async fn a_cancel_mid_generation_aborts_the_in_flight_request_after_durable_boundaries() {
        // Chunk 0 completes (and crosses the boundary hook = persistable);
        // chunk 1's generation hangs in flight; the cancel must end it
        // promptly, with chunk 0's completion already delivered.
        let long = "A sentence of source text. ".repeat(45);
        let units = vec![prose_unit(&long), prose_unit(&long)];
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut llm = HangingLlm {
            scripted: ScriptedLlm::new(vec![
                Ok("[{\"text\": \"First lecture.\"}]".to_string()),
                Ok("updated summary".to_string()),
            ]),
            started: started.clone(),
        };
        let (handle, mut token) = crate::providers::registry::cancel_channel();
        let canceller = tokio::spawn({
            let started = started.clone();
            async move {
                started.notified().await;
                handle.cancel();
            }
        });
        let mut completed: Vec<usize> = Vec::new();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            teach_chunks(
                &units,
                2048,
                &mut llm,
                Vec::new(),
                |_, _| {},
                &mut token,
                |chunk| completed.push(chunk.chunk_idx),
            ),
        )
        .await
        .expect("the cancel ends the in-flight generation promptly")
        .unwrap_err();
        canceller.await.unwrap();
        assert_eq!(error.kind, ScriptErrorKind::Cancelled);
        assert_eq!(
            completed,
            vec![0],
            "the completed chunk crossed its durable boundary"
        );
    }

    #[tokio::test]
    async fn cancellation_surfaces_as_cancelled() {
        let mut llm = ScriptedLlm::new(vec![]);
        let (handle, mut token) = crate::providers::registry::cancel_channel();
        handle.cancel();
        let error = teach_chunks(
            &[prose_unit("Some source.")],
            4096,
            &mut llm,
            Vec::new(),
            |_, _| {},
            &mut token,
            |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn the_rolling_summary_rides_between_chunks() {
        let long = "A sentence of source text. ".repeat(45);
        let units = vec![prose_unit(&long), prose_unit(&long)];
        let mut llm = ScriptedLlm::new(vec![
            Ok("[{\"text\": \"Part one.\"}]".to_string()),
            Ok("summary of part one".to_string()),
            Ok("[{\"text\": \"Part two.\"}]".to_string()),
        ]);
        let outcomes = teach_chunks(
            &units,
            2048,
            &mut llm,
            Vec::new(),
            |_, _| {},
            &mut crate::providers::registry::CancelToken::never(),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(llm.prompts[1].user.contains("Summarize the lecture so far"));
        assert!(
            llm.prompts[2].user.contains("CONTEXT SUMMARY"),
            "chunk two carries the rolled summary"
        );
        assert!(llm.prompts[2].user.contains("summary of part one"));
    }

    #[test]
    fn parse_lecture_supports_direct_structured_json() {
        let raw = "[{\"text\": \"Direct JSON lecture without markdown fences.\"}]";
        let parsed = parse_lecture(raw).expect("parsed");
        assert_eq!(parsed, vec!["Direct JSON lecture without markdown fences."]);
    }

    #[test]
    fn output_budget_tokens_scales_for_cloud_context() {
        let local_budget = output_budget_tokens(100, 500, 4096);
        assert_eq!(local_budget, 512);

        let cloud_budget = output_budget_tokens(100, 500, 32768);
        assert_eq!(cloud_budget, 2048);
    }

    #[tokio::test]
    async fn test_overload_503_fast_fails_on_attempt_1() {
        let mut llm = ScriptedLlm::new(vec![
            Err(ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: true,
                message: "503 UNAVAILABLE: The model is overloaded".to_string(),
                retry_after_secs: None,
                status: Some(503),
            }),
            Ok("[{\"text\": \"Lecture.\"}]".to_string()),
        ]);

        let figure_ids = std::collections::BTreeSet::new();
        let res = generate_lecture(&mut llm, "", "source text", 4096, &figure_ids).await;
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert_eq!(err.status, Some(503));
        assert_eq!(llm.prompts.len(), 1);
    }

    #[tokio::test]
    async fn test_update_summary_preserves_summary_on_503() {
        let mut llm = ScriptedLlm::new(vec![Err(ScriptError {
            kind: ScriptErrorKind::Provider,
            retryable: true,
            message: "503 UNAVAILABLE: The model is overloaded".to_string(),
            retry_after_secs: None,
            status: Some(503),
        })]);

        let summary = update_summary(&mut llm, "previous summary", "new lecture", 4096)
            .await
            .expect("summary update must not fail the chunk on 503");
        assert_eq!(summary, "previous summary");
    }
}
