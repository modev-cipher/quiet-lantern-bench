pub mod anchor;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use sqlx::SqlitePool;

use crate::render::mp3::{build_timeline, playable_segments, SegmentSpan, CHAPTER_GAP_MS};
use crate::storage::{book, chapter_render, job};
use crate::words::{self, WordProvenance};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineOrigin {
    Measured,
    Dynamic,
    Unverified,
    Legacy,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingWord {
    pub w: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub rect: Option<anchor::Rect>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingSegment {
    pub segment_id: String,
    pub idx: i64,
    pub page_no: i64,
    pub kind: String,
    pub figure_id: Option<String>,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub provenance: WordProvenance,
    pub words: Vec<ReadingWord>,
    pub anchor: anchor::Rung,
    pub rects: Vec<anchor::Rect>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingChapterSummary {
    pub chapter_id: String,
    pub idx: i64,
    pub title: String,
    pub page_start: i64,
    pub page_end: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub segment_count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingIndex {
    pub job_id: String,
    pub book_id: String,
    pub origin: TimelineOrigin,
    pub total_ms: i64,
    pub chapters: Vec<ReadingChapterSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingChapter {
    pub anchor_geometry: GeometryOutcome,
    pub chapter_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub origin: TimelineOrigin,
    pub segments: Vec<ReadingSegment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingPage {
    pub page_no: i64,
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    pub image_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingFigure {
    pub figure_id: String,
    pub page_no: i64,
    pub bbox: Option<[f64; 4]>,
    pub crop_hash: Option<String>,
}

#[derive(Debug)]
pub enum ReadingError {
    Database(sqlx::Error),
    JobNotFound(String),
    ChapterNotInJob(String),
    ChapterOrderMismatch(String),
    InterleavedChapter(String),
}

impl std::fmt::Display for ReadingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(f, "{error}"),
            Self::JobNotFound(job_id) => write!(f, "job '{job_id}' not found"),
            Self::ChapterNotInJob(chapter_id) => {
                write!(f, "chapter '{chapter_id}' has no audio in this job")
            }
            Self::ChapterOrderMismatch(chapter_id) => write!(
                f,
                "chapter '{chapter_id}' is ordered differently in the render rows than in the \
                 audio, so its measured start cannot be trusted"
            ),
            Self::InterleavedChapter(reason) => write!(f, "{reason}"),
        }
    }
}

impl From<sqlx::Error> for ReadingError {
    fn from(error: sqlx::Error) -> Self {
        ReadingError::Database(error)
    }
}

struct Layout {
    origin: TimelineOrigin,
    spans: Vec<SegmentSpan>,
    total_ms: i64,
    order: Vec<String>,
}

enum MeasuredStarts {
    Ok(Vec<i64>),
    NoneMeasured,
    PartiallyMeasured,
}

fn measured_starts(rows: &[chapter_render::ChapterRender]) -> MeasuredStarts {
    let measured = rows.iter().filter(|row| row.encoded_ms.is_some()).count();
    if measured == 0 {
        return MeasuredStarts::NoneMeasured;
    }
    if measured < rows.len() {
        return MeasuredStarts::PartiallyMeasured;
    }
    let mut cursor = 0i64;
    let mut out = Vec::with_capacity(rows.len());
    for (position, row) in rows.iter().enumerate() {
        let Some(encoded) = row.encoded_ms else {
            return MeasuredStarts::PartiallyMeasured;
        };
        let leading = if position == 0 { 0 } else { CHAPTER_GAP_MS };
        out.push(cursor + leading);
        cursor += encoded;
    }
    MeasuredStarts::Ok(out)
}

fn chapter_order(
    playable: &[crate::render::mp3::RenderSegment],
) -> Result<Vec<String>, ReadingError> {
    let groups = crate::orchestrator::render::chapter_groups(playable).map_err(|error| {
        let reason = match error {
            crate::orchestrator::state::OrchestratorError::Render(message) => message,
            other => other.to_string(),
        };
        ReadingError::InterleavedChapter(reason)
    })?;
    Ok(groups
        .into_iter()
        .map(|(chapter_id, _)| chapter_id)
        .collect())
}

async fn layout(
    pool: &SqlitePool,
    job_id: &str,
    cache_dir: &Path,
) -> Result<(Layout, Vec<job::ScriptSegment>), ReadingError> {
    let segments = job::list_segments(pool, job_id).await?;
    let playable = playable_segments(&segments);
    let timeline = build_timeline(&playable, cache_dir, |id| id.to_string());
    let order = chapter_order(&playable)?;
    let rows = chapter_render::list(pool, job_id).await?;

    if rows.is_empty() {
        return Ok((
            Layout {
                origin: TimelineOrigin::Legacy,
                spans: timeline.segments,
                total_ms: timeline.total_ms,
                order,
            },
            segments,
        ));
    }

    let mut ordered = rows.clone();
    ordered.sort_by_key(|row| row.idx);
    let row_order: Vec<String> = ordered.iter().map(|row| row.chapter_id.clone()).collect();
    if row_order.len() != order.len() {
        return Ok((
            Layout {
                origin: TimelineOrigin::Unverified,
                spans: timeline.segments,
                total_ms: timeline.total_ms,
                order,
            },
            segments,
        ));
    }
    if row_order != order {
        let mismatch = row_order
            .iter()
            .zip(order.iter())
            .find(|(row, audio)| row != audio)
            .map(|(row, _)| row.clone())
            .unwrap_or_default();
        return Err(ReadingError::ChapterOrderMismatch(mismatch));
    }

    let starts = match measured_starts(&ordered) {
        MeasuredStarts::Ok(starts) => starts,
        MeasuredStarts::NoneMeasured => {
            return Ok((
                Layout {
                    origin: TimelineOrigin::Dynamic,
                    spans: timeline.segments,
                    total_ms: timeline.total_ms,
                    order,
                },
                segments,
            ));
        }
        MeasuredStarts::PartiallyMeasured => {
            return Ok((
                Layout {
                    origin: TimelineOrigin::Unverified,
                    spans: timeline.segments,
                    total_ms: timeline.total_ms,
                    order,
                },
                segments,
            ));
        }
    };

    let mut shift: HashMap<String, i64> = HashMap::new();
    for (position, row) in ordered.iter().enumerate() {
        let nominal_start = timeline
            .segments
            .iter()
            .find(|span| span.chapter_id == row.chapter_id)
            .map(|span| span.start_ms)
            .unwrap_or(0);
        shift.insert(row.chapter_id.clone(), starts[position] - nominal_start);
    }

    let mut spans = timeline.segments;
    for span in &mut spans {
        let delta = shift.get(&span.chapter_id).copied().unwrap_or(0);
        span.start_ms += delta;
        span.end_ms += delta;
    }
    let total_ms: i64 = ordered
        .iter()
        .map(|row| row.encoded_ms.unwrap_or(row.duration_ms))
        .sum::<i64>();

    Ok((
        Layout {
            origin: TimelineOrigin::Measured,
            spans,
            total_ms,
            order,
        },
        segments,
    ))
}

fn words_for(
    alignment: Option<&job::Alignment>,
    span: &SegmentSpan,
    duration_ms: i64,
) -> (WordProvenance, Vec<ReadingWord>) {
    let Some(alignment) = alignment else {
        return (WordProvenance::Missing, Vec::new());
    };
    let payload = alignment.words_json.parse();
    let provenance = words::classify(&payload, &alignment.source);
    if provenance != WordProvenance::Fresh {
        return (provenance, Vec::new());
    }
    let mut out = Vec::new();
    for entry in &payload.words {
        let (Some(text), Some(start), Some(end)) = (
            entry.get("w").and_then(|v| v.as_str()),
            entry.get("s").and_then(|v| v.as_i64()),
            entry.get("e").and_then(|v| v.as_i64()),
        ) else {
            continue;
        };
        let start = start.clamp(0, duration_ms);
        let end = end.clamp(start, duration_ms);
        if end == start {
            continue;
        }
        out.push(ReadingWord {
            w: text.to_string(),
            start_ms: span.start_ms + start,
            end_ms: span.start_ms + end,
            rect: None,
        });
    }
    if out.is_empty() {
        return (WordProvenance::Missing, Vec::new());
    }
    (WordProvenance::Fresh, out)
}

pub async fn index(
    pool: &SqlitePool,
    job_id: &str,
    cache_dir: &Path,
) -> Result<ReadingIndex, ReadingError> {
    let record = job::get_job(pool, job_id)
        .await?
        .ok_or_else(|| ReadingError::JobNotFound(job_id.to_string()))?;
    let (layout, _) = layout(pool, job_id, cache_dir).await?;
    let chapters: HashMap<String, book::Chapter> = book::list_chapters(pool, &record.book_id)
        .await?
        .into_iter()
        .map(|chapter| (chapter.id.clone(), chapter))
        .collect();

    let mut summaries = Vec::new();
    for (position, chapter_id) in layout.order.iter().enumerate() {
        let spans: Vec<&SegmentSpan> = layout
            .spans
            .iter()
            .filter(|span| &span.chapter_id == chapter_id)
            .collect();
        let start_ms = spans.first().map(|span| span.start_ms).unwrap_or(0);
        let end_ms = spans.last().map(|span| span.end_ms).unwrap_or(start_ms);
        let chapter = chapters.get(chapter_id);
        summaries.push(ReadingChapterSummary {
            chapter_id: chapter_id.clone(),
            idx: position as i64,
            title: chapter
                .map(|c| c.title.clone())
                .unwrap_or_else(|| chapter_id.clone()),
            page_start: chapter.map(|c| c.page_start).unwrap_or(0),
            page_end: chapter.map(|c| c.page_end).unwrap_or(0),
            start_ms,
            end_ms,
            segment_count: spans.len() as i64,
        });
    }

    Ok(ReadingIndex {
        job_id: job_id.to_string(),
        book_id: record.book_id,
        origin: layout.origin,
        total_ms: layout.total_ms,
        chapters: summaries,
    })
}

fn counts_toward_rate(segment: &job::ScriptSegment) -> bool {
    if segment.kind == "figure_cue" {
        return false;
    }
    let Some(raw) = segment.flags_json.as_deref() else {
        return true;
    };
    let Ok(flags) = serde_json::from_str::<Vec<String>>(raw) else {
        return true;
    };
    !flags.iter().any(|flag| flag == "code_block")
}

/// The whole ensure step: batch the chapter's pages under the request bound, run each batch,
/// then decide the reported outcome from **what actually landed**, not from the calls' return
/// values. Lives here rather than in the Tauri command because a closure in `lib.rs` is not
/// reachable by a test — the first version of this was tested only through a closure the test
/// supplied itself, which pinned the test's own chunking and left production's unexercised.
///
/// `run_batch` is the only injected part: it is the engine round trip.
pub async fn ensure_geometry_for_chapter<F, Fut>(
    pool: &SqlitePool,
    book_id: &str,
    pages: &[i64],
    mut run_batch: F,
) -> EnsureReport
where
    F: FnMut(Vec<i64>) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let started = std::time::Instant::now();
    let mut batches = 0usize;
    let mut delivered = true;
    for batch in pages.chunks(MAX_REQUESTED_ITEMS) {
        batches += 1;
        if run_batch(batch.to_vec()).await.is_err() {
            delivered = false;
        }
    }
    let covered = geometry_coverage(pool, book_id, pages).await.unwrap_or(0);
    EnsureReport {
        pages: pages.len(),
        batches,
        covered,
        ensured: delivered && covered == pages.len(),
        elapsed_ms: started.elapsed().as_millis(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureReport {
    pub pages: usize,
    pub batches: usize,
    pub covered: usize,
    pub ensured: bool,
    pub elapsed_ms: u128,
}

impl EnsureReport {
    pub fn outcome(&self) -> GeometryOutcome {
        if self.ensured {
            GeometryOutcome::Ensured
        } else {
            GeometryOutcome::Unavailable
        }
    }

    pub fn log_line(&self, book_id: &str) -> String {
        anchor::format_geometry_ensure(book_id, self.pages, self.ensured, self.elapsed_ms)
    }
}

/// Post-ensure truth, not the ensure call's return value: `ensure_pages_inner` answers `Ok`
/// on the engine-down and render-failed arms too, so `is_ok()` would report
/// `geometry-ensured` while the walk runs against missing geometry. Count what actually
/// landed instead.
pub async fn geometry_coverage(
    pool: &SqlitePool,
    book_id: &str,
    page_numbers: &[i64],
) -> Result<usize, ReadingError> {
    let mut covered = 0;
    for page_no in page_numbers {
        let Some(row) = book::get_page(pool, book_id, *page_no).await? else {
            continue;
        };
        if book::get_page_geometry(pool, &row.id).await?.is_some() {
            covered += 1;
        }
    }
    Ok(covered)
}

struct PageSurface {
    geometry: anchor::PageGeometry,
    blocks: Vec<anchor::BlockRef>,
}

async fn page_surfaces(
    pool: &SqlitePool,
    book_id: &str,
    page_numbers: &[i64],
) -> Result<HashMap<i64, PageSurface>, ReadingError> {
    let mut out = HashMap::new();
    for page_no in page_numbers {
        let Some(row) = book::get_page(pool, book_id, *page_no).await? else {
            continue;
        };
        let geometry = match book::get_page_geometry(pool, &row.id).await? {
            Some(stored) => anchor::parse_geometry(&stored.geometry_json).unwrap_or_default(),
            None => anchor::PageGeometry::default(),
        };
        let blocks = row
            .text_json
            .as_deref()
            .map(anchor::parse_blocks)
            .unwrap_or_default();
        out.insert(*page_no, PageSurface { geometry, blocks });
    }
    Ok(out)
}

fn unanchored(count: usize) -> anchor::SegmentAnchor {
    anchor::SegmentAnchor {
        rung: anchor::Rung::Page,
        rects: Vec::new(),
        word_rects: vec![None; count],
        located: 0,
        tokens: 0,
        rate_tokens: 0,
        demoted: 0,
        unplaced: 0,
    }
}

fn block_for<'a>(
    surface: &'a PageSurface,
    segment: &job::ScriptSegment,
) -> Option<&'a anchor::BlockRef> {
    let span = segment.source_span_json.as_deref();
    let index = anchor::block_index_of(span)
        .or_else(|| anchor::recover_block_index(&surface.blocks, span, &segment.text))?;
    surface.blocks.iter().find(|b| b.reading_order == index)
}

/// Resolve every segment printed on one page in one walk, so the cursor that disambiguates a
/// repeated word survives a segment boundary.
///
/// The page's **whole** segment set is walked, across chapters, and only the caller's segments
/// are read back. A page straddling a chapter boundary would otherwise be walked once per
/// chapter, each from slot zero, and the second chapter's opening segments would reproduce the
/// defect this exists to remove.
fn anchor_pages(
    surfaces: &HashMap<i64, PageSurface>,
    ordered: &[(&job::ScriptSegment, Vec<String>)],
) -> HashMap<String, anchor::SegmentAnchor> {
    let mut by_page: HashMap<i64, Vec<usize>> = HashMap::new();
    for (position, (segment, _)) in ordered.iter().enumerate() {
        by_page.entry(segment.page_no).or_default().push(position);
    }
    let mut out = HashMap::new();
    for (page_no, positions) in by_page {
        let Some(surface) = surfaces.get(&page_no) else {
            for position in positions {
                let (segment, spoken) = &ordered[position];
                out.insert(segment.id.clone(), unanchored(spoken.len()));
            }
            continue;
        };
        let references: Vec<Vec<String>> = positions
            .iter()
            .map(|position| vec![ordered[*position].0.text.clone()])
            .collect();
        let inputs: Vec<anchor::SegmentInput<'_>> = positions
            .iter()
            .zip(references.iter())
            .map(|(position, reference)| {
                let (segment, spoken) = &ordered[*position];
                anchor::SegmentInput {
                    spoken,
                    block: block_for(surface, segment),
                    kind: &segment.kind,
                    counts_toward_rate: counts_toward_rate(segment),
                    reference: Some(reference),
                }
            })
            .collect();
        for (position, resolved) in positions
            .iter()
            .zip(anchor::anchor_page(&inputs, &surface.geometry))
        {
            out.insert(ordered[*position].0.id.clone(), resolved);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GeometryOutcome {
    Ensured,
    Unavailable,
}

/// Anchoring needs geometry for **every** page the chapter touches, and geometry is
/// populated lazily by the page feeder for the one or two pages on screen. Measured on the
/// real app before this argument existed: a first open anchored 54 of 469 segments
/// (`rate=0.1112`) because 21 of the chapter's 24 pages had none; with all 24 warm the same
/// chapter reached `rate=0.9525`. The ensure therefore happens **here**, before the anchor
/// walk, so the ordering is structural rather than a convention the caller has to remember.
///
/// `ensure` failing is not an error: the chapter still returns, with whatever rungs the
/// available geometry supports. An engine that is down must degrade the highlight, never
/// refuse to open the book.
pub async fn chapter<F, Fut>(
    pool: &SqlitePool,
    job_id: &str,
    chapter_id: &str,
    cache_dir: &Path,
    ensure: F,
) -> Result<ReadingChapter, ReadingError>
where
    F: FnOnce(String, Vec<i64>) -> Fut,
    Fut: std::future::Future<Output = GeometryOutcome>,
{
    let Some(job_row) = job::get_job(pool, job_id).await? else {
        return Err(ReadingError::JobNotFound(job_id.to_string()));
    };
    let (layout, segments) = layout(pool, job_id, cache_dir).await?;
    if !layout.order.iter().any(|id| id == chapter_id) {
        return Err(ReadingError::ChapterNotInJob(chapter_id.to_string()));
    }
    let by_id: HashMap<&str, &job::ScriptSegment> = segments
        .iter()
        .map(|segment| (segment.id.as_str(), segment))
        .collect();
    let alignments = job::list_alignments(pool, job_id).await?;
    let aligned: HashMap<&str, &job::Alignment> = alignments
        .iter()
        .map(|row| (row.segment_id.as_str(), row))
        .collect();

    let wanted_pages: Vec<i64> = {
        let mut seen = HashSet::new();
        layout
            .spans
            .iter()
            .filter(|s| s.chapter_id == chapter_id)
            .filter_map(|s| by_id.get(s.segment_id.as_str()).map(|seg| seg.page_no))
            .filter(|page_no| seen.insert(*page_no))
            .collect()
    };
    let anchor_geometry = ensure(job_row.book_id.clone(), wanted_pages.clone()).await;
    let surfaces = page_surfaces(pool, &job_row.book_id, &wanted_pages).await?;

    let on_page: HashSet<i64> = wanted_pages.iter().copied().collect();
    let mut prepared: Vec<(&job::ScriptSegment, WordProvenance, Vec<ReadingWord>)> = Vec::new();
    for span in layout.spans.iter() {
        let Some(segment) = by_id.get(span.segment_id.as_str()) else {
            continue;
        };
        if !on_page.contains(&segment.page_no) {
            continue;
        }
        let (provenance, words) = words_for(
            aligned.get(span.segment_id.as_str()).copied(),
            span,
            span.end_ms - span.start_ms,
        );
        prepared.push((segment, provenance, words));
    }
    prepared.sort_by_key(|(segment, _, _)| segment.idx);
    let ordered: Vec<(&job::ScriptSegment, Vec<String>)> = prepared
        .iter()
        .map(|(segment, _, words)| {
            (
                *segment,
                words
                    .iter()
                    .map(|word| word.w.clone())
                    .collect::<Vec<String>>(),
            )
        })
        .collect();
    let is_teacher =
        job_row.mode.eq_ignore_ascii_case("teacher") || job_row.mode.eq_ignore_ascii_case("teach");
    let mut anchors = if is_teacher {
        HashMap::new()
    } else {
        anchor_pages(&surfaces, &ordered)
    };
    let mut carried: HashMap<&str, (WordProvenance, Vec<ReadingWord>)> = prepared
        .into_iter()
        .map(|(segment, provenance, words)| (segment.id.as_str(), (provenance, words)))
        .collect();

    let mut tally = anchor::RungTally::default();
    let mut out = Vec::new();
    for span in layout.spans.iter().filter(|s| s.chapter_id == chapter_id) {
        let Some(segment) = by_id.get(span.segment_id.as_str()) else {
            continue;
        };
        let Some((provenance, mut words)) = carried.remove(segment.id.as_str()) else {
            continue;
        };
        if is_teacher {
            for word in words.iter_mut() {
                word.rect = None;
            }
            let (rects, rung) = if let Some(surface) = surfaces.get(&segment.page_no) {
                if let Some(block) = block_for(surface, segment) {
                    let page_h = surface.geometry.page_height_pt;
                    let top_fraction = if page_h > 0.0 {
                        block.bbox.y0 / page_h
                    } else {
                        0.5
                    };
                    let is_extreme_margin = (top_fraction < 0.08 || top_fraction > 0.92)
                        && block.text.trim().len() < 25;
                    if !is_extreme_margin {
                        (vec![block.bbox], anchor::Rung::Block)
                    } else {
                        (Vec::new(), anchor::Rung::Page)
                    }
                } else {
                    (Vec::new(), anchor::Rung::Page)
                }
            } else {
                (Vec::new(), anchor::Rung::Page)
            };
            let teacher_anchor = anchor::SegmentAnchor {
                rung,
                rects: rects.clone(),
                word_rects: vec![None; words.len()],
                located: 0,
                tokens: words.len(),
                rate_tokens: 0,
                demoted: 0,
                unplaced: 0,
            };
            tally.record(&teacher_anchor);
            out.push(ReadingSegment {
                segment_id: segment.id.clone(),
                idx: segment.idx,
                page_no: segment.page_no,
                kind: segment.kind.clone(),
                figure_id: segment.figure_id.clone(),
                text: segment.text.clone(),
                start_ms: span.start_ms,
                end_ms: span.end_ms,
                provenance,
                words,
                anchor: rung,
                rects,
            });
        } else {
            let resolved = anchors
                .remove(segment.id.as_str())
                .unwrap_or_else(|| unanchored(words.len()));
            for (word, rect) in words.iter_mut().zip(resolved.word_rects.iter()) {
                word.rect = *rect;
            }
            tally.record(&resolved);
            out.push(ReadingSegment {
                segment_id: segment.id.clone(),
                idx: segment.idx,
                page_no: segment.page_no,
                kind: segment.kind.clone(),
                figure_id: segment.figure_id.clone(),
                text: segment.text.clone(),
                start_ms: span.start_ms,
                end_ms: span.end_ms,
                provenance,
                words,
                anchor: resolved.rung,
                rects: resolved.rects,
            });
        }
    }
    tracing::debug!("{}", anchor::format_anchor_rungs(chapter_id, &tally));

    let start_ms = out.first().map(|s| s.start_ms).unwrap_or(0);
    let end_ms = out.last().map(|s| s.end_ms).unwrap_or(start_ms);
    Ok(ReadingChapter {
        anchor_geometry,
        chapter_id: chapter_id.to_string(),
        start_ms,
        end_ms,
        origin: layout.origin,
        segments: out,
        mode: Some(job_row.mode),
    })
}

/// The voice session's lean chapter loader (VOICE_AGENT §4: the session
/// snapshot is "built once at session start from the reading structures";
/// `voice::excerpt::source::SessionSnapshot::from_reading` is its consumer).
///
/// Deliberately loads text and timeline spans only — no geometry ensure, no
/// word alignments, no anchor walk — because the session snapshot never
/// renders anything: pulling [`chapter`]'s per-page geometry pipeline here
/// would rasterize every page of the book at session start and couple voice
/// session startup to the page engine being alive. A session must open on a
/// machine whose page engine is down exactly like the reading view does
/// (that module's "degrade the highlight, never refuse to open the book").
/// F7 book-direct snapshot source: the voice session's chapter loader for a
/// book with NO narration job — `chapter` rows plus each page's canonical
/// `text_json` blocks, straight from import (traversal precedent:
/// `cast_plan::scoped_source_chars`). No render, no timeline, no geometry.
///
/// The segment `start_ms`/`end_ms` fields are reused as **cumulative
/// character offsets** into the book's canonical text (spec §5.1: the
/// e-reader text-offset location model — a synthetic monotonic coordinate
/// where no audio timeline exists), so `ExcerptSelector`, `page_at`, and
/// `figures_on_page` work untouched. Positions produced here are only ever
/// interpreted by a jobless session (`job_id IS NULL` is the discriminator,
/// 0018/0024 convention) and must never feed an audio player.
///
/// Pages that fall inside two chapters' page ranges (a chapter boundary
/// mid-page) are attributed to the FIRST chapter that reaches them, so no
/// text block enters the offset space twice.
pub async fn book_chapters(
    pool: &SqlitePool,
    book_id: &str,
) -> Result<Vec<ReadingChapter>, ReadingError> {
    let mut chapters = crate::storage::book::list_chapters(pool, book_id)
        .await
        .map_err(ReadingError::Database)?;
    chapters.sort_by_key(|chapter| chapter.idx);
    let pages = crate::storage::book::list_pages(pool, book_id)
        .await
        .map_err(ReadingError::Database)?;
    // `(text, teach)`: running headers and footers ("Chapter 1. Introduction
    // 17", "DS 4.01 downloaded by …") are layout furniture, not book text —
    // in the 2026-10-09 field book they were ~2 300 of ~10 900 blocks, and
    // every window fed them to the tutor as prose (review 2026-10-09, T4).
    // They still advance the offset, so positions journaled before this
    // change (Continue, coverage) keep meaning the same text.
    let block_texts: HashMap<i64, Vec<(String, bool)>> = pages
        .into_iter()
        .filter_map(|page| {
            let raw = page.text_json?;
            let envelope: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let texts: Vec<(String, bool)> = envelope["blocks"]
                .as_array()?
                .iter()
                .filter_map(|block| {
                    let text = block["text"].as_str()?;
                    let furniture =
                        matches!(block["label"].as_str(), Some("page_header" | "page_footer"));
                    Some((text.to_string(), !furniture))
                })
                .filter(|(text, _)| !text.trim().is_empty())
                .collect();
            Some((page.page_no, texts))
        })
        .collect();

    let mut out = Vec::with_capacity(chapters.len());
    let mut consumed_pages: HashSet<i64> = HashSet::new();
    let mut offset: i64 = 0;
    let mut idx: i64 = 0;
    for chapter in &chapters {
        let mut segments = Vec::new();
        for page_no in chapter.page_start..=chapter.page_end {
            if !consumed_pages.insert(page_no) {
                continue;
            }
            let Some(texts) = block_texts.get(&page_no) else {
                continue;
            };
            for (block_at, (text, teach)) in texts.iter().enumerate() {
                let start_ms = offset;
                offset += text.chars().count() as i64;
                if !teach {
                    continue;
                }
                segments.push(ReadingSegment {
                    segment_id: format!("book:{page_no}:{block_at}"),
                    idx,
                    page_no,
                    kind: "body".to_string(),
                    figure_id: None,
                    text: text.clone(),
                    start_ms,
                    end_ms: offset,
                    provenance: WordProvenance::Missing,
                    words: Vec::new(),
                    anchor: anchor::Rung::Page,
                    rects: Vec::new(),
                });
                idx += 1;
            }
        }
        let start_ms = segments.first().map_or(offset, |s| s.start_ms);
        let end_ms = segments.last().map_or(start_ms, |s| s.end_ms);
        out.push(ReadingChapter {
            anchor_geometry: GeometryOutcome::Unavailable,
            chapter_id: chapter.id.clone(),
            start_ms,
            end_ms,
            origin: TimelineOrigin::Unverified,
            segments,
            mode: None,
        });
    }
    Ok(out)
}

pub async fn session_chapters(
    pool: &SqlitePool,
    job_id: &str,
    cache_dir: &Path,
) -> Result<Vec<ReadingChapter>, ReadingError> {
    let (layout, segments) = layout(pool, job_id, cache_dir).await?;
    let by_id: HashMap<&str, &job::ScriptSegment> = segments
        .iter()
        .map(|segment| (segment.id.as_str(), segment))
        .collect();
    let index_of: HashMap<&str, usize> = layout
        .order
        .iter()
        .enumerate()
        .map(|(at, id)| (id.as_str(), at))
        .collect();
    let mut chapters: Vec<ReadingChapter> = layout
        .order
        .iter()
        .map(|chapter_id| ReadingChapter {
            anchor_geometry: GeometryOutcome::Unavailable,
            chapter_id: chapter_id.clone(),
            start_ms: 0,
            end_ms: 0,
            origin: layout.origin,
            segments: Vec::new(),
            mode: None,
        })
        .collect();
    for span in &layout.spans {
        let Some(&at) = index_of.get(span.chapter_id.as_str()) else {
            continue;
        };
        let Some(segment) = by_id.get(span.segment_id.as_str()) else {
            continue;
        };
        chapters[at].segments.push(ReadingSegment {
            segment_id: segment.id.clone(),
            idx: segment.idx,
            page_no: segment.page_no,
            kind: segment.kind.clone(),
            figure_id: segment.figure_id.clone(),
            text: segment.text.clone(),
            start_ms: span.start_ms,
            end_ms: span.end_ms,
            provenance: WordProvenance::Missing,
            words: Vec::new(),
            anchor: anchor::Rung::Page,
            rects: Vec::new(),
        });
    }
    for chapter in &mut chapters {
        chapter.start_ms = chapter.segments.first().map_or(0, |s| s.start_ms);
        chapter.end_ms = chapter.segments.last().map_or(0, |s| s.end_ms);
    }
    Ok(chapters)
}

/// Consolidates all measured/aligned segments, word timings, and chapters into
/// an alignment.json payload for cast export / LAN transfer.
pub async fn build_export_alignment(
    pool: &SqlitePool,
    job_id: &str,
    cache_dir: &Path,
) -> Result<serde_json::Value, ReadingError> {
    let (layout, segments) = layout(pool, job_id, cache_dir).await?;
    let by_id: HashMap<&str, &job::ScriptSegment> = segments
        .iter()
        .map(|segment| (segment.id.as_str(), segment))
        .collect();
    let alignments = job::list_alignments(pool, job_id).await?;
    let aligned: HashMap<&str, &job::Alignment> = alignments
        .iter()
        .map(|row| (row.segment_id.as_str(), row))
        .collect();

    let mut segment_payloads = Vec::new();
    for span in &layout.spans {
        let Some(segment) = by_id.get(span.segment_id.as_str()) else {
            continue;
        };
        let (provenance, words) = words_for(
            aligned.get(span.segment_id.as_str()).copied(),
            span,
            span.end_ms - span.start_ms,
        );
        segment_payloads.push(serde_json::json!({
            "segmentId": segment.id,
            "idx": segment.idx,
            "chapterId": span.chapter_id,
            "pageNo": segment.page_no,
            "kind": segment.kind,
            "figureId": segment.figure_id,
            "text": segment.text,
            "startMs": span.start_ms,
            "endMs": span.end_ms,
            "provenance": provenance,
            "words": words,
        }));
    }

    Ok(serde_json::json!({
        "origin": layout.origin,
        "totalMs": layout.total_ms,
        "order": layout.order,
        "segments": segment_payloads,
    }))
}

pub const PAGE_MODE_LOG_PREFIX: &str = "page mode";

pub const FOLLOW_STATE_LOG_PREFIX: &str = "follow state";

pub const READING_TAP_LOG_PREFIX: &str = "tap at";

pub const MAX_REQUESTED_ITEMS: usize = 64;

pub fn refuse_oversized(count: usize) -> Option<String> {
    if count <= MAX_REQUESTED_ITEMS {
        return None;
    }
    Some(format!(
        "a reading request asked for {count} items; the view fetches a page and its neighbour, so \
         anything past {MAX_REQUESTED_ITEMS} is a caller defect rather than a workload"
    ))
}

fn image_path_for(cache_dir: &Path, render_hash: Option<&str>) -> Option<String> {
    let hash = render_hash?;
    if hash.is_empty() {
        return None;
    }
    let path = crate::cache_path::cache_path(cache_dir, hash);
    if !path.is_file() {
        return None;
    }
    Some(path.to_string_lossy().into_owned())
}

fn parse_bbox(bbox_json: &str) -> Option<[f64; 4]> {
    serde_json::from_str(bbox_json).ok()
}

pub const ENSURE_LOG_PREFIX: &str = "page ensure";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureOutcome {
    Hit,
    Rendered,
    Absent,
    EngineDown,
    RenderFailed,
}

impl EnsureOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Rendered => "rendered",
            Self::Absent => "absent",
            Self::EngineDown => "engine-down",
            Self::RenderFailed => "render-failed",
        }
    }
}

pub fn format_ensure_line(
    book_id: &str,
    page_no: i64,
    outcome: EnsureOutcome,
    render_ms: Option<u128>,
    round_trip_ms: u128,
    bytes: Option<i64>,
) -> String {
    let short: String = book_id.chars().take(8).collect();
    let render = match render_ms {
        Some(value) => value.to_string(),
        None => "n/a".to_string(),
    };
    let size = match bytes {
        Some(value) => value.to_string(),
        None => "n/a".to_string(),
    };
    let label = outcome.as_str();
    format!(
        "{ENSURE_LOG_PREFIX}: book={short} page={page_no} outcome={label} renderMs={render} \
         roundTripMs={round_trip_ms} bytes={size}"
    )
}

pub const GEOMETRY_LOG_PREFIX: &str = "page geometry";

#[derive(Debug, Clone, PartialEq)]
pub struct GeometrySlot {
    pub page_no: i64,
    pub page_id: String,
    pub version: Option<String>,
    pub words: usize,
    pub lines: usize,
    pub bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryFreshness {
    Fresh,
    Stale,
    Absent,
    Unverified,
}

impl GeometryFreshness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "false",
            Self::Stale => "true",
            Self::Absent => "true",
            Self::Unverified => "unverified",
        }
    }

    pub fn needs_fetch(self) -> bool {
        !matches!(self, Self::Fresh | Self::Unverified)
    }
}

pub fn classify_geometry(stored: Option<&str>, expected: Option<&str>) -> GeometryFreshness {
    match (stored, expected) {
        (_, None) => GeometryFreshness::Unverified,
        (None, Some(_)) => GeometryFreshness::Absent,
        (Some(stored), Some(expected)) if stored == expected => GeometryFreshness::Fresh,
        (Some(_), Some(_)) => GeometryFreshness::Stale,
    }
}

pub fn pages_to_ensure(
    pages: &[ReadingPage],
    geometry: &[GeometrySlot],
    expected: Option<&str>,
) -> Vec<i64> {
    let mut wanted = pages_to_render(pages);
    for slot in geometry {
        if classify_geometry(slot.version.as_deref(), expected).needs_fetch()
            && !wanted.contains(&slot.page_no)
        {
            wanted.push(slot.page_no);
        }
    }
    wanted.sort_unstable();
    wanted
}

pub fn format_ensure_plan(book_id: &str, pages: usize, to_rasterise: usize) -> String {
    let short: String = book_id.chars().take(8).collect();
    let geometry_only = pages.saturating_sub(to_rasterise);
    format!(
        "{ENSURE_LOG_PREFIX}: book={short} pages={pages} toRasterise={to_rasterise} \
         geometryOnly={geometry_only}"
    )
}

pub fn format_geometry_line(
    book_id: &str,
    page_no: i64,
    words: usize,
    lines: usize,
    bytes: usize,
    version: Option<&str>,
    freshness: GeometryFreshness,
) -> String {
    let short: String = book_id.chars().take(8).collect();
    let version = version.unwrap_or("none");
    let stale = freshness.as_str();
    format!(
        "{GEOMETRY_LOG_PREFIX}: book={short} page={page_no} words={words} lines={lines} \
         bytes={bytes} version={version} stale={stale}"
    )
}

pub fn count_geometry(geometry_json: &str) -> (usize, usize) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(geometry_json) else {
        return (0, 0);
    };
    let words = value["words"].as_array().map(Vec::len).unwrap_or(0);
    let lines = value["lines"].as_array().map(Vec::len).unwrap_or(0);
    (words, lines)
}

pub async fn geometry_slots(
    pool: &SqlitePool,
    book_id: &str,
    page_numbers: &[i64],
) -> Result<Vec<GeometrySlot>, ReadingError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(page_numbers.len());
    for page_no in page_numbers {
        if !seen.insert(*page_no) {
            continue;
        }
        let Some(row) = book::get_page(pool, book_id, *page_no).await? else {
            continue;
        };
        let stored = book::get_page_geometry(pool, &row.id).await?;
        let (words, lines, bytes, version) = match stored {
            Some(geometry) => {
                let (words, lines) = count_geometry(&geometry.geometry_json);
                (
                    words,
                    lines,
                    geometry.geometry_json.len(),
                    Some(geometry.geometry_version),
                )
            }
            None => (0, 0, 0, None),
        };
        out.push(GeometrySlot {
            page_no: row.page_no,
            page_id: row.id,
            version,
            words,
            lines,
            bytes,
        });
    }
    Ok(out)
}

pub fn pages_to_render(pages: &[ReadingPage]) -> Vec<i64> {
    pages
        .iter()
        .filter(|page| page.image_path.is_none())
        .map(|page| page.page_no)
        .collect()
}

pub async fn pages(
    pool: &SqlitePool,
    book_id: &str,
    page_numbers: &[i64],
    cache_dir: &Path,
) -> Result<Vec<ReadingPage>, ReadingError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(page_numbers.len());
    for page_no in page_numbers {
        if !seen.insert(*page_no) {
            continue;
        }
        let Some(row) = book::get_page(pool, book_id, *page_no).await? else {
            continue;
        };
        let image_path = image_path_for(cache_dir, row.render_hash.as_deref());
        if image_path.is_some() {
            if let Some(hash) = row.render_hash.as_deref() {
                crate::storage::cache::touch(pool, hash).await?;
            }
        }
        out.push(ReadingPage {
            page_no: row.page_no,
            width_pt: row.width_pt,
            height_pt: row.height_pt,
            image_path,
        });
    }
    Ok(out)
}

pub async fn figures(
    pool: &SqlitePool,
    book_id: &str,
    figure_ids: &[String],
) -> Result<Vec<ReadingFigure>, ReadingError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(figure_ids.len());
    for figure_id in figure_ids {
        if !seen.insert(figure_id.as_str()) {
            continue;
        }
        let Some(row) = book::get_figure_region(pool, book_id, figure_id).await? else {
            continue;
        };
        out.push(ReadingFigure {
            figure_id: row.id,
            page_no: row.page_no,
            bbox: parse_bbox(&row.bbox_json),
            crop_hash: row.crop_hash,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(chapter: &str, idx: i64, encoded: Option<i64>) -> chapter_render::ChapterRender {
        chapter_render::ChapterRender {
            job_id: "job".into(),
            chapter_id: chapter.into(),
            idx,
            hash: format!("h{idx}"),
            artifact: if encoded.is_some() {
                chapter_render::ARTIFACT_MP3.into()
            } else {
                chapter_render::ARTIFACT_NONE.into()
            },
            duration_ms: 2200,
            encoded_ms: encoded,
        }
    }

    fn flagged(kind: &str, flags: Option<&str>) -> job::ScriptSegment {
        job::ScriptSegment {
            id: "seg".into(),
            job_id: "job".into(),
            chapter_id: "chap".into(),
            idx: 0,
            page_no: 1,
            kind: kind.into(),
            figure_id: None,
            text: "Body sentence.".into(),
            source_span_json: None,
            flags_json: flags.map(str::to_string),
            audio_hash: None,
            duration_ms: Some(900),
            original_text: None,
        }
    }

    #[test]
    fn the_diagnostic_prefixes_are_the_literals_the_log_review_greps_for() {
        assert_eq!(PAGE_MODE_LOG_PREFIX, "page mode");
        assert_eq!(FOLLOW_STATE_LOG_PREFIX, "follow state");
        assert_eq!(READING_TAP_LOG_PREFIX, "tap at");
        assert_eq!(ENSURE_LOG_PREFIX, "page ensure");
        assert_eq!(GEOMETRY_LOG_PREFIX, "page geometry");
        assert_eq!(anchor::ANCHOR_LOG_PREFIX, "anchor rungs");
        assert_eq!(anchor::GEOMETRY_ENSURE_LOG_PREFIX, "anchor geometry");
    }

    #[test]
    fn the_rate_denominator_reads_the_flag_list_rather_than_the_raw_json() {
        assert!(counts_toward_rate(&flagged("text", None)));
        assert!(counts_toward_rate(&flagged("text", Some("[]"))));
        assert!(!counts_toward_rate(&flagged(
            "text",
            Some(r#"["code_block"]"#)
        )));
        assert!(!counts_toward_rate(&flagged("figure_cue", None)));
        assert!(
            counts_toward_rate(&flagged("text", Some(r#"["no_code_block"]"#))),
            "a substring test on the raw JSON would exclude this row, understating the located \
             rate against a flag nothing in the pipeline emits; membership of the parsed list is \
             the contract"
        );
        assert!(
            counts_toward_rate(&flagged("text", Some(r#"{"code_block":true}"#))),
            "an unparsable flags column falls back to counting the segment, because dropping \
             rows on a shape we cannot read would silently shrink the denominator"
        );
    }

    #[test]
    fn measured_starts_place_a_chapter_at_its_speech_not_at_its_silence() {
        let rows = vec![
            row("a", 0, Some(2252)),
            row("b", 1, Some(3452)),
            row("c", 2, Some(3452)),
        ];
        let MeasuredStarts::Ok(starts) = measured_starts(&rows) else {
            panic!("all rows measured");
        };
        assert_eq!(
            starts,
            vec![0, 2252 + CHAPTER_GAP_MS, 5704 + CHAPTER_GAP_MS]
        );
    }

    #[test]
    fn one_unmeasured_chapter_refuses_the_whole_measured_basis() {
        let rows = vec![row("a", 0, Some(2252)), row("b", 1, None)];
        assert!(
            matches!(measured_starts(&rows), MeasuredStarts::PartiallyMeasured),
            "mixing a measured origin with a nominal one inside a book puts two bases in one \
             timeline, which is worse than using the nominal basis throughout"
        );
    }

    #[test]
    fn a_book_with_no_measured_chapter_is_dynamic_rather_than_unverified() {
        let rows = vec![row("a", 0, None), row("b", 1, None)];
        assert!(
            matches!(measured_starts(&rows), MeasuredStarts::NoneMeasured),
            "no row carries encoded_ms only when the export was one dynamic encode, which is a \
             different claim from a book that measured some chapters and lost others"
        );
    }

    #[test]
    fn the_ensure_line_names_its_outcome_and_reports_a_missing_size_as_not_available() {
        let line = format_ensure_line(
            "0123456789abcdef",
            25,
            EnsureOutcome::EngineDown,
            None,
            7,
            None,
        );
        assert_eq!(
            line,
            "page ensure: book=01234567 page=25 outcome=engine-down renderMs=n/a roundTripMs=7 \
             bytes=n/a"
        );
    }

    fn playable(chapter: &str, index: usize) -> crate::render::mp3::RenderSegment {
        crate::render::mp3::RenderSegment {
            segment_id: format!("s{index}"),
            chapter_id: chapter.into(),
            audio_hash: format!("h{index}"),
            duration_ms: 900,
        }
    }

    #[test]
    fn chapter_order_follows_the_audio_and_collapses_runs() {
        let order = chapter_order(&[playable("a", 0), playable("a", 1), playable("b", 2)])
            .expect("contiguous chapters");
        assert_eq!(order, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn an_interleaved_chapter_is_refused_by_the_same_rule_the_renderer_uses() {
        let error = chapter_order(&[playable("a", 0), playable("b", 1), playable("a", 2)])
            .expect_err("the renderer rejects this shape, so the reader cannot accept it");
        assert!(
            error.to_string().contains("interleaved"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn an_evicted_page_render_reports_no_image_even_though_the_row_kept_its_hash() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            image_path_for(dir.path(), Some("abcdef")),
            None,
            "a hash the cache has since evicted must not become a src the webview cannot load"
        );
    }

    #[test]
    fn a_page_render_still_in_the_cache_reports_its_absolute_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let expected = crate::cache_path::cache_path(dir.path(), "abcdef");
        std::fs::create_dir_all(expected.parent().expect("shard")).expect("shard dir");
        std::fs::write(&expected, b"png").expect("write");
        assert_eq!(
            image_path_for(dir.path(), Some("abcdef")),
            Some(expected.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn a_page_that_was_never_rendered_reports_no_image() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(image_path_for(dir.path(), None), None);
    }

    #[test]
    fn an_empty_render_hash_never_resolves_to_the_cache_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            image_path_for(dir.path(), Some("")),
            None,
            "an empty hash composes to the cache directory itself, which is not a page image"
        );
    }

    #[test]
    fn a_request_the_view_would_never_make_is_refused_with_its_own_size() {
        assert_eq!(refuse_oversized(0), None);
        assert_eq!(refuse_oversized(MAX_REQUESTED_ITEMS), None);
        let refusal =
            refuse_oversized(MAX_REQUESTED_ITEMS + 1).expect("one item past the cap is refused");
        assert!(
            refusal.contains(&(MAX_REQUESTED_ITEMS + 1).to_string()),
            "the refusal names what was asked for, because one query per item means the caller's \
             list length sets the query count: {refusal}"
        );
    }

    #[test]
    fn a_bbox_that_is_not_four_numbers_is_dropped_rather_than_guessed() {
        assert_eq!(parse_bbox("[1.0,2.0,3.0,4.0]"), Some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(parse_bbox("[1.0,2.0,3.0]"), None);
        assert_eq!(parse_bbox("{}"), None);
        assert_eq!(parse_bbox(""), None);
    }
}
