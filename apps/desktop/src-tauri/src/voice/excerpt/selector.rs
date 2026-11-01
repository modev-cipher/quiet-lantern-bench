//! The single egress point for book text (VOICE_AGENT §4, task VA4).
//!
//! `ExcerptSelector` is the only code path allowed to turn book text into a
//! [`DelimitedExcerpt`]. Every excerpt passes the **hard byte cap** here — the
//! final backstop below the per-provider tokenizer-measured caps. Nothing in
//! this module sends anything: consuming an excerpt is VA5's channel adapters,
//! gated by per-first-use-per-book consent (VOICE_AGENT §5).

use crate::voice::channel::types::DelimitedExcerpt;

use super::coverage::{coverage_line, PlayedRanges, PositionUnits};
use super::policy::{EgressEvent, TruncationNotice, TruncationPolicy};
use super::sentinel::{self, Sentinel};
use super::source::SessionSnapshot;
use super::summary::{RollingSummaryCache, Summarize};
use super::window::{window_around, WindowPolicy, WindowSelection};

/// Hard ceiling on book-text bytes in one excerpt — the final backstop, not the
/// working cap (VOICE_AGENT §4: caps are tokenizer-measured per provider,
/// excerpt window ≤ 4K tokens; "a hard byte ceiling exists only as a final
/// backstop"). The spec names no byte figure, so the value is derived here:
/// 4096 tokens at a worst-case ~4 bytes/token (CJK-dense multilingual text)
/// ≈ 16 KiB of legitimate payload; 64 KiB gives that estimate 4× headroom so
/// the backstop can never bite a legitimate tokenizer-capped excerpt, while
/// still capping a runaway caller at well under a chapter of text.
pub const HARD_EGRESS_BYTE_CAP: usize = 64 * 1024;

/// Book text between segments is joined with a blank line: the segments are
/// paragraph-grained, and the join must not glue sentences together.
const SEGMENT_JOIN: &str = "\n\n";

/// The line that tells the tutor which physical page the following text is
/// on (see `join_segments`); the prompt's visual-verbal law names it.
pub fn page_marker(page_no: i64) -> String {
    format!("[page {page_no}]\n")
}

/// Test-side inverse of the page marking: the book text alone, for pins
/// whose subject is budgets and caps (which count book bytes, never the
/// metadata riding on top).
#[cfg(test)]
pub(crate) fn strip_page_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("[page ") {
        let line_start = at == 0 || rest[..at].ends_with('\n');
        let end = rest[at..].find("]\n").map(|close| at + close + 2);
        match end {
            Some(end)
                if line_start && rest[at + "[page ".len()..end - 2].parse::<i64>().is_ok() =>
            {
                out.push_str(&rest[..at]);
                rest = &rest[end..];
            }
            _ => {
                out.push_str(&rest[..at + 1]);
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// F7 tier 0 bounds, OWNER-TUNABLE: how many visual-element labels ride one
/// window, and how long each may be.
const FIGURE_NOTES_MAX: usize = 8;
const FIGURE_NOTE_CHAR_CAP: usize = 500;

/// The hard cap fired. Loud by construction: the notice rides the selection
/// result, so no caller can receive a capped excerpt without receiving the
/// fact that content was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardCapNotice {
    pub kept_bytes: usize,
    pub dropped_bytes: usize,
}

/// One bounded-range fetch (F8 D1b `get_context_for_range`). Either the whole
/// range fit the reply budget, or the honest split: the first and last chunks
/// plus explicit omission counts — the RFC 9110 partial-content shape adapted
/// to a token budget. Silent truncation is unrepresentable: the only lossy
/// variant carries the loss on its face.
pub enum RangeFetch {
    /// The full range's text, sealed as one excerpt.
    Full(SelectedExcerpt),
    /// The range exceeded the budget: leading and trailing chunks, each
    /// within half the budget, with the omitted middle counted explicitly.
    Split {
        head: SelectedExcerpt,
        /// The head's own single segment exceeded half the budget and was cut
        /// to a prefix — disclosed, never silent.
        head_partially_cut: bool,
        /// The tail chunk: a true SUFFIX of the range, so an oversized final
        /// segment still ends where the request ends.
        tail: SelectedExcerpt,
        tail_partially_cut: bool,
        /// Whole segments between the head and tail chunks that were NOT
        /// returned.
        omitted_segments: usize,
        /// Segments the requested range spans in total.
        total_segments: usize,
        /// F10-D1 round 4: the position (session units) where the head
        /// chunk's text ENDS — the segment boundary for a whole head, or a
        /// point INSIDE the first segment when it was cut to a prefix.
        head_end: u64,
        /// Where the tail chunk's text STARTS — the segment boundary for a
        /// whole tail, or a point inside the last segment when cut to a
        /// suffix. `[head_end, tail_start)` is the interior not returned;
        /// a follow-up fetch over it is served by position-clipping the
        /// cut segments, so recursion always makes progress.
        tail_start: u64,
    },
}

pub struct SelectedExcerpt {
    pub excerpt: DelimitedExcerpt,
    /// Inclusive snapshot-idx bounds of the segments whose text (possibly
    /// tail-capped) is inside the excerpt.
    pub segment_range: (usize, usize),
    pub hard_cap: Option<HardCapNotice>,
    /// Set whenever the per-provider token budget dropped content
    /// ([`ExcerptSelector::budgeted_excerpt_at`]); the same fact also reaches
    /// the caller's event sink — content loss is never silent.
    pub truncation: Option<TruncationNotice>,
}

/// Where teaching continues after a window ([`ExcerptSelector::continue_after_window`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinueAt {
    pub page: i64,
    /// The first unread segment's position (ms, or a text offset in
    /// book-only sessions — the unit every position tool takes).
    pub position: u64,
    /// The window ended mid-page: the rest of `page` comes next.
    pub rest_of_page: bool,
}

pub struct ExcerptSelector {
    snapshot: SessionSnapshot,
    sentinel: Sentinel,
    window: WindowPolicy,
    current: Option<WindowSelection>,
    summaries: RollingSummaryCache,
    /// F8 D1 coverage memory: interval union of positions actually traversed
    /// this session (played narration for BookGrounded, cursor-visited
    /// windows for BookDirect). Session-scoped like the rolling-summary
    /// cache; survives channel renewal because the selector does.
    coverage: PlayedRanges,
    page_windows: bool,
    rendered: Option<(usize, usize)>,
}

impl ExcerptSelector {
    pub fn new(snapshot: SessionSnapshot, window: WindowPolicy) -> Self {
        Self {
            snapshot,
            sentinel: Sentinel::generate(),
            window,
            current: None,
            summaries: RollingSummaryCache::new(),
            coverage: PlayedRanges::default(),
            page_windows: false,
            rendered: None,
        }
    }

    pub fn set_page_windows(&mut self, on: bool) {
        self.page_windows = on;
    }

    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    pub fn has_anchor(&self, kind: &crate::voice::channel::event::AnchorKind, id: &str) -> bool {
        self.snapshot.has_anchor(kind, id)
    }

    pub fn anchor_page(
        &self,
        kind: &crate::voice::channel::event::AnchorKind,
        id: &str,
    ) -> Option<i64> {
        self.snapshot.anchor_page(kind, id)
    }

    /// Record one traversed interval into the coverage memory (F8 D1): the
    /// grounded runtime feeds continuous playback steps, the BookDirect path
    /// feeds rendered-window spans. Positions only — no text.
    pub fn note_played(&mut self, start: u64, end: u64) {
        self.coverage.note(start, end);
    }

    /// Mark the CURRENT window's span as visited (the BookDirect coverage
    /// feed: text the session actually rendered to the tutor). No-op before
    /// any window resolved.
    pub fn note_window_visited(&mut self) {
        let Some(selection) = self.current else {
            return;
        };
        let (first_idx, last_idx) = self
            .rendered
            .unwrap_or((selection.first_idx, selection.last_idx));
        let segments = self.snapshot.segments();
        let (Some(first), Some(last)) = (segments.get(first_idx), segments.get(last_idx)) else {
            return;
        };
        self.coverage.note(first.start_ms, last.end_ms);
    }

    /// The pages a forward jump to `target` skips (field session
    /// 2026-10-09, `01a121e0`: "show me the figure" jumped from the chapter
    /// opening to the figure's page, and the tutor went on teaching from
    /// there — the pages between were never covered). The reading frontier
    /// is the end of the furthest covered text before `target`, in ANY
    /// chapter: a jump into the next chapter with the current chapter's
    /// tail unread skips that tail too (review 2026-10-09, T2 — the
    /// frontier used to be looked up only inside the landing chapter).
    /// Skipped pages are the ones wholly between the frontier and the
    /// landing page, inside the frontier's chapter or the landing chapter
    /// (front matter and whole chapters in between are the user's own
    /// navigation, never "owed" pages). `None` when nothing before
    /// `target` was covered, when the jump is not forward of the frontier,
    /// or when no whole page lies between.
    pub fn skipped_pages_before(&self, target: u64) -> Option<(i64, i64)> {
        let chapters = self.snapshot.chapters();
        let chapter_of = |pos: u64| {
            chapters
                .iter()
                .find(|c| pos >= c.start_ms && pos < c.end_ms)
                .map(|c| (c.start_ms, c.end_ms))
        };
        let landing_chapter = chapter_of(target)?;
        let frontier = self
            .coverage
            .ranges()
            .iter()
            .filter(|&&(start, _)| start < target)
            .map(|&(_, end)| end.min(target))
            .max()?;
        let frontier_chapter = frontier.checked_sub(1).and_then(chapter_of);
        let in_scope = |pos: u64| {
            let inside = |(start, end): (u64, u64)| pos >= start && pos < end;
            inside(landing_chapter) || frontier_chapter.is_some_and(inside)
        };
        let landing = self.page_at(target)?;
        let mut skipped: Option<(i64, i64)> = None;
        let mut partial_page: Option<i64> = None;
        for segment in self.snapshot.segments() {
            if segment.start_ms >= target || segment.page_no <= 0 || segment.page_no >= landing {
                continue;
            }
            if segment.start_ms < frontier {
                // This page holds covered text: it is not skipped, even if
                // its tail is.
                partial_page = Some(segment.page_no);
                continue;
            }
            if partial_page == Some(segment.page_no) || !in_scope(segment.start_ms) {
                continue;
            }
            skipped = Some(match skipped {
                None => (segment.page_no, segment.page_no),
                Some((first, last)) => (first.min(segment.page_no), last.max(segment.page_no)),
            });
        }
        skipped
    }

    /// Where reading continues after the current window: the first segment
    /// past the window's end. `rest_of_page` is set when that segment is on
    /// the window's own last page — the window ended mid-page, and the rest
    /// of the page comes next (review 2026-10-09, T1: naming only the NEXT
    /// page made the tutor skip that rest). `None` before any window, or at
    /// the end of the book.
    pub fn continue_after_window(&self) -> Option<ContinueAt> {
        let selection = self.current?;
        let last_idx = self.rendered.map_or(selection.last_idx, |(_, last)| last);
        let segments = self.snapshot.segments();
        let last_page = segments.get(last_idx)?.page_no;
        let next = segments
            .iter()
            .skip(last_idx + 1)
            .find(|segment| segment.page_no > 0)?;
        Some(ContinueAt {
            page: next.page_no,
            position: next.start_ms,
            rest_of_page: next.page_no == last_page,
        })
    }

    /// The played ranges accumulated so far (coverage memory, F8 D1).
    pub fn played_ranges(&self) -> &PlayedRanges {
        &self.coverage
    }

    /// The honest one-line coverage header for the excerpt window (F8 D1
    /// piece 3): derived entirely from the chapter map and the accumulator.
    pub fn coverage_line(&self, pos: u64, units: PositionUnits) -> String {
        let timeline_end = self.snapshot.segments().last().map_or(0, |s| s.end_ms);
        coverage_line(
            self.snapshot.chapters(),
            timeline_end,
            &self.coverage,
            pos,
            units,
        )
    }

    /// The bounded per-fetch header (2026-09-24 context diet): one line —
    /// position, chapter, capped span truth — for every window payload.
    /// The long-form [`Self::coverage_line`] stays the recap tool's answer.
    pub fn coverage_header(&self, pos: u64, units: PositionUnits) -> String {
        let timeline_end = self.snapshot.segments().last().map_or(0, |s| s.end_ms);
        super::coverage::coverage_header(
            self.snapshot.chapters(),
            timeline_end,
            &self.coverage,
            pos,
            units,
        )
    }

    /// Re-resolve the window for a new playback position (the reading clock,
    /// in ms — delivered by the harness from `ContextUpdate::PlaybackPosition`
    /// / the player's position, VOICE_AGENT §4 "sync as playback moves").
    /// Stateless in both directions: seeks backward re-anchor exactly like
    /// forward progress. `None` only on an empty snapshot.
    pub fn advance_to(&mut self, pos_ms: u64) -> Option<WindowSelection> {
        self.rendered = None;
        self.current = if self.page_windows {
            super::window::page_window_around(&self.snapshot, pos_ms)
        } else {
            window_around(&self.snapshot, pos_ms, self.window)
        };
        self.current
    }

    /// The window resolved by the last [`ExcerptSelector::advance_to`].
    pub fn current_window(&self) -> Option<WindowSelection> {
        self.current
    }

    /// Advance to `pos_ms` and render the resulting window as one excerpt.
    pub fn excerpt_at(&mut self, pos_ms: u64) -> Option<SelectedExcerpt> {
        let selection = self.advance_to(pos_ms)?;
        Some(self.excerpt_for_range(selection.first_idx, selection.last_idx))
    }

    /// Wrap an untrusted prompt field in the session sentinel (VOICE_AGENT §4:
    /// excerpts, rolling summaries, and transcript text re-injected on renewal
    /// all enter the prompt as delimited untrusted data). For text the caller
    /// already holds — summaries the summarizer produced, journal text — not a
    /// second way to read book text; the same collision re-roll applies.
    pub fn delimit_untrusted(&mut self, text: &str) -> DelimitedExcerpt {
        sentinel::delimit(&mut self.sentinel, text)
    }

    /// The cached rolling summary for everything before the current window
    /// (VOICE_AGENT §4). `None` before the first [`ExcerptSelector::advance_to`]
    /// and while the window sits at the top of the book. The summarizer is only
    /// invoked on a cache miss — the caller decides *when* a summary is worth
    /// its session-LLM call by calling this at all; within a session the same
    /// window boundary never pays twice.
    pub fn rolling_summary_before_window(&mut self, summarize: &mut dyn Summarize) -> Option<&str> {
        let boundary = self.current?.first_idx;
        self.summaries
            .summary_before(boundary, &self.snapshot, summarize)
    }

    /// Render the inclusive segment range `first..=last` as one sentinel-
    /// delimited excerpt, enforcing the hard byte cap on the book-text payload
    /// (markers excluded: the cap is on egressed *book* bytes, and counting the
    /// wrapper would let marker length silently eat payload budget).
    ///
    /// Out-of-range bounds clamp to the snapshot; an empty snapshot yields an
    /// empty payload rather than a panic (a session on a zero-segment render is
    /// degenerate but must not crash the harness).
    pub fn excerpt_for_range(&mut self, first: usize, last: usize) -> SelectedExcerpt {
        let (first, last, text) = self.joined_range_text(first, last);
        self.seal(&text, (first, last), None, true)
    }

    /// Advance to `pos_ms` and render the window under a provider's truncation
    /// policy (VOICE_AGENT §4: tokenizer-measured caps). Content loss is loud
    /// twice over: an [`EgressEvent`] into `events` *and* the notice on the
    /// returned selection.
    ///
    /// Trim order: whole segments from the window edge **farthest from the
    /// anchor** first; on a tie the **leading** edge goes, because everything
    /// before the window is recoverable through the rolling summary while
    /// upcoming text is not. If the anchor segment alone still exceeds the
    /// budget, its text is cut to the largest prefix the budget accepts.
    pub fn budgeted_excerpt_at(
        &mut self,
        pos_ms: u64,
        policy: &TruncationPolicy,
        events: &mut dyn FnMut(EgressEvent),
    ) -> Option<SelectedExcerpt> {
        let selection = self.advance_to(pos_ms)?;
        Some(self.budget_selection(selection, policy, events))
    }

    /// Render the **current** window (the one the last
    /// [`ExcerptSelector::advance_to`] resolved) under a provider's truncation
    /// policy, without re-anchoring — the `summarize_section` shape
    /// (VOICE_AGENT §5: "scope within already-injected window only, no
    /// widening"). `None` before any window is resolved. Same trim order and
    /// same double loudness as [`ExcerptSelector::budgeted_excerpt_at`].
    pub fn budgeted_excerpt_for_window(
        &mut self,
        policy: &TruncationPolicy,
        events: &mut dyn FnMut(EgressEvent),
    ) -> Option<SelectedExcerpt> {
        let selection = self.current_window()?;
        Some(self.budget_selection(selection, policy, events))
    }

    /// The shared budget pass behind both budgeted entry points.
    fn budget_selection(
        &mut self,
        selection: WindowSelection,
        policy: &TruncationPolicy,
        events: &mut dyn FnMut(EgressEvent),
    ) -> SelectedExcerpt {
        let anchor = selection.anchor_idx;
        let (mut first, mut last) = (selection.first_idx, selection.last_idx);
        let full_window_text = self.join_segments(first, last);
        let window_tokens = policy.count(&full_window_text);

        let mut dropped_leading = 0usize;
        let mut dropped_trailing = 0usize;
        let mut anchor_partially_cut = false;
        let mut text = full_window_text;
        while !policy.fits(&text) {
            if first == last {
                text = policy.largest_fitting_prefix(&text).to_string();
                anchor_partially_cut = true;
                break;
            }
            if anchor - first >= last - anchor {
                first += 1;
                dropped_leading += 1;
            } else {
                last -= 1;
                dropped_trailing += 1;
            }
            text = self.join_segments(first, last);
        }

        let truncation = (dropped_leading + dropped_trailing > 0 || anchor_partially_cut)
            .then_some(TruncationNotice {
                budget_tokens: policy.max_excerpt_tokens(),
                window_tokens,
                dropped_leading_segments: dropped_leading,
                dropped_trailing_segments: dropped_trailing,
                anchor_partially_cut,
            });
        if let Some(notice) = truncation {
            events(EgressEvent::ExcerptTruncated {
                budget_tokens: notice.budget_tokens,
                window_tokens: notice.window_tokens,
                dropped_leading_segments: notice.dropped_leading_segments,
                dropped_trailing_segments: notice.dropped_trailing_segments,
                anchor_partially_cut: notice.anchor_partially_cut,
            });
        }

        if self.current == Some(selection) {
            self.rendered = Some((first, last));
        }
        let selected = self.seal(&text, (first, last), truncation, true);
        if let Some(notice) = selected.hard_cap {
            events(EgressEvent::HardByteCapApplied {
                kept_bytes: notice.kept_bytes,
                dropped_bytes: notice.dropped_bytes,
            });
        }
        selected
    }

    /// The one place every excerpt leaves through: hard byte cap, then the
    /// sentinel delimiters. F7 tier 0 (owner ruling 2026-08-30): the window
    /// gains the ingestion-time labels of every visual element on its pages
    /// — figures, charts, diagrams, tables, rendered formulas — appended
    /// AFTER the token trim (they are small and bounded) and BEFORE the hard
    /// byte cap, which therefore still bounds total egress.
    fn seal(
        &mut self,
        text: &str,
        segment_range: (usize, usize),
        truncation: Option<TruncationNotice>,
        page_marked: bool,
    ) -> SelectedExcerpt {
        let book_len = text.len();
        let notes = self.figure_notes(segment_range);
        let text = if notes.is_empty() {
            text.to_string()
        } else {
            format!("{text}{notes}")
        };
        let text = text.as_str();
        let (kept, capped) = cap_at_char_boundary(text, HARD_EGRESS_BYTE_CAP);
        let hard_cap = capped.then_some(HardCapNotice {
            kept_bytes: kept.len(),
            dropped_bytes: text.len() - kept.len(),
        });
        let kept = if page_marked {
            self.mark_pages(kept, segment_range, book_len.min(kept.len()))
        } else {
            kept.to_string()
        };
        SelectedExcerpt {
            excerpt: sentinel::delimit(&mut self.sentinel, &kept),
            segment_range,
            hard_cap,
            truncation,
        }
    }

    /// Visual-element labels for the pages the sealed window spans, bounded:
    /// at most [`FIGURE_NOTES_MAX`] notes, each clipped to
    /// [`FIGURE_NOTE_CHAR_CAP`] chars — the labels help, never dominate.
    fn figure_notes(&self, (first, last): (usize, usize)) -> String {
        let segments = self.snapshot.segments();
        if segments.is_empty() {
            return String::new();
        }
        let top = segments.len() - 1;
        let mut pages: Vec<i64> = segments[first.min(top)..=last.min(top)]
            .iter()
            .map(|segment| segment.page_no)
            .collect();
        pages.sort_unstable();
        pages.dedup();
        let mut notes = String::new();
        let mut count = 0usize;
        for page_no in pages {
            for figure in self.snapshot.figures_on_page(page_no) {
                if count == FIGURE_NOTES_MAX {
                    return notes;
                }
                let clipped: String = figure.note.chars().take(FIGURE_NOTE_CHAR_CAP).collect();
                if notes.is_empty() {
                    notes
                        .push_str("\n\nVisual elements on these pages, as described at ingestion:");
                }
                // The id is what `teacher_anchor` resolves against the
                // reader's figure regions — without it the tutor could name
                // a figure but never point the preview at it.
                notes.push_str(&format!(
                    "\n- {} on page {} (anchor id {}): {}",
                    figure.kind,
                    page_no,
                    figure.id,
                    clipped.trim()
                ));
                count += 1;
            }
        }
        notes
    }

    /// The inclusive segment-index span intersecting the half-open position
    /// range `[start, end)` (F8 D1b): every segment with any overlap counts.
    /// `None` on an empty/inverted range or one falling entirely between or
    /// outside segments. Positions are the session's position space —
    /// milliseconds (BookGrounded) or character offsets (BookDirect); this
    /// resolver only orders them, like the coverage module.
    pub fn segment_span_for_positions(&self, start: u64, end: u64) -> Option<(usize, usize)> {
        if end <= start {
            return None;
        }
        let segments = self.snapshot.segments();
        let overlaps = |segment: &super::source::SessionSegment| {
            segment.end_ms > start && segment.start_ms < end
        };
        let first = segments.iter().position(overlaps)?;
        let last = segments.iter().rposition(overlaps)?;
        Some((first, last))
    }

    /// Render the inclusive segment span `first..=last` as one bounded-range
    /// fetch (F8 D1b `get_context_for_range`), under the reply budget: the
    /// provider token policy when installed, the hard byte cap otherwise.
    ///
    /// A fitting range seals whole ([`RangeFetch::Full`]). An oversized one
    /// returns [`RangeFetch::Split`] — the leading and trailing chunks, each
    /// grown segment-by-segment while it stays within HALF the budget (so the
    /// two together never exceed it), with the omitted middle counted
    /// explicitly. Both paths exit through [`ExcerptSelector::seal`], so the
    /// sentinel wrapping and the hard byte backstop apply exactly as for
    /// every other excerpt; a fired backstop is reported into `events`.
    pub fn range_fetch(
        &mut self,
        first: usize,
        last: usize,
        policy: Option<&TruncationPolicy>,
        events: &mut dyn FnMut(EgressEvent),
    ) -> RangeFetch {
        let (first, last, _) = self.joined_range_text(first, last);
        let (start, end) = match self
            .snapshot
            .segments()
            .get(first)
            .zip(self.snapshot.segments().get(last))
        {
            Some((a, b)) => (a.start_ms, b.end_ms),
            None => (0, 0),
        };
        self.range_fetch_within(first, last, start, end, policy, events)
    }

    /// [`ExcerptSelector::range_fetch`] over the segment span `first..=last`,
    /// CLIPPED to the position range `[start, end)` (F10-D1 round 4). A
    /// segment only partly inside the range contributes only the proportional
    /// slice of its text — characters map linearly onto the segment's
    /// position span — so a follow-up fetch over a Split's
    /// `[head_end, tail_start)` interior returns exactly the text that was
    /// not returned before, even when that interior lies INSIDE one oversized
    /// segment. Starts round down and ends round up, so pieces touch or
    /// overlap by at most a character, never leave a gap.
    pub fn range_fetch_within(
        &mut self,
        first: usize,
        last: usize,
        start: u64,
        end: u64,
        policy: Option<&TruncationPolicy>,
        events: &mut dyn FnMut(EgressEvent),
    ) -> RangeFetch {
        let (first, last, _) = self.joined_range_text(first, last);
        let segments = self.snapshot.segments();
        if segments.is_empty() {
            return RangeFetch::Full(self.seal_with_events("", (0, 0), events));
        }
        // Per-segment clipped texts plus the absolute char bounds kept.
        let clipped: Vec<(String, usize, usize)> = (first..=last)
            .map(|idx| {
                let segment = &segments[idx];
                let total = segment.text.chars().count();
                let from = if start > segment.start_ms {
                    char_of_position(segment.start_ms, segment.end_ms, total, start)
                } else {
                    0
                };
                let to = if end < segment.end_ms {
                    char_of_position(segment.start_ms, segment.end_ms, total, end).max(from)
                } else {
                    total
                };
                let text: String = segment.text.chars().skip(from).take(to - from).collect();
                (text, from, to)
            })
            .collect();
        let join = |a: usize, b: usize| -> String {
            clipped[a - first..=b - first]
                .iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<Vec<_>>()
                .join(SEGMENT_JOIN)
        };
        let text = join(first, last);
        let fits = match policy {
            Some(policy) => policy.fits(&text),
            None => text.len() <= HARD_EGRESS_BYTE_CAP,
        };
        if fits {
            return RangeFetch::Full(self.seal_with_events(&text, (first, last), events));
        }

        let within_half = |text: &str| match policy {
            Some(policy) => policy.count(text) <= policy.max_excerpt_tokens() / 2,
            None => text.len() <= HARD_EGRESS_BYTE_CAP / 2,
        };
        // Head: grow from `first` while the joined chunk stays within half
        // the budget; a single oversized segment degrades to its largest
        // half-fitting PREFIX — and says so (`head_partially_cut`).
        let mut head_last = first;
        while head_last < last && within_half(&join(first, head_last + 1)) {
            head_last += 1;
        }
        let head_full = join(first, head_last);
        let (head_text, head_partially_cut) = half_cap_prefix(policy, &head_full);
        // Tail: grow backward from `last`, never into the head. An oversized
        // final segment keeps its SUFFIX — the tail chunk exists to carry the
        // true end of the requested range.
        let mut tail_first = last;
        while tail_first > head_last + 1 && within_half(&join(tail_first - 1, last)) {
            tail_first -= 1;
        }
        let tail_full = join(tail_first, last);
        let (tail_text, tail_partially_cut) = half_cap_suffix(policy, &tail_full);
        // Positional cut points. A cut only ever happens when the chunk is a
        // single segment (a multi-segment chunk fit by construction).
        let head_end = if head_partially_cut {
            let segment = &segments[first];
            let total = segment.text.chars().count();
            let kept = clipped[0].1 + head_text.chars().count();
            position_of_char(segment.start_ms, segment.end_ms, total, kept, false)
        } else {
            segments[head_last].end_ms.min(end)
        };
        let tail_start = if tail_partially_cut {
            let segment = &segments[last];
            let total = segment.text.chars().count();
            let kept = clipped[last - first].2 - tail_text.chars().count();
            position_of_char(segment.start_ms, segment.end_ms, total, kept, true)
        } else {
            segments[tail_first].start_ms.max(start)
        };
        let (head_text, tail_text) = (head_text.to_string(), tail_text.to_string());
        let head = self.seal_with_events(&head_text, (first, head_last), events);
        let tail = self.seal_with_events(&tail_text, (tail_first, last), events);
        RangeFetch::Split {
            head,
            head_partially_cut,
            tail,
            tail_partially_cut,
            omitted_segments: tail_first.saturating_sub(head_last + 1),
            total_segments: last - first + 1,
            head_end,
            tail_start,
        }
    }

    /// [`ExcerptSelector::seal`] plus the loudness twin: a fired hard-byte
    /// backstop also reaches the caller's event sink, exactly like the
    /// budgeted window paths.
    fn seal_with_events(
        &mut self,
        text: &str,
        segment_range: (usize, usize),
        events: &mut dyn FnMut(EgressEvent),
    ) -> SelectedExcerpt {
        let selected = self.seal(text, segment_range, None, false);
        if let Some(notice) = selected.hard_cap {
            events(EgressEvent::HardByteCapApplied {
                kept_bytes: notice.kept_bytes,
                dropped_bytes: notice.dropped_bytes,
            });
        }
        selected
    }

    /// The page under a playback position — navigation data only, no text
    /// (F7: the sight paths resolve "the current page" here, the same
    /// anchoring rule as the excerpt window: last segment starting at or
    /// before the position, clamped to the ends).
    pub fn page_at(&self, pos_ms: u64) -> Option<i64> {
        let segments = self.snapshot.segments();
        if segments.is_empty() {
            return None;
        }
        let anchored = segments
            .iter()
            .rev()
            .find(|segment| segment.start_ms <= pos_ms)
            .unwrap_or(&segments[0]);
        Some(anchored.page_no)
    }

    fn joined_range_text(&self, first: usize, last: usize) -> (usize, usize, String) {
        if self.snapshot.is_empty() {
            return (0, 0, String::new());
        }
        let top = self.snapshot.len() - 1;
        let first = first.min(top);
        let last = last.clamp(first, top);
        (first, last, self.join_segments(first, last))
    }

    /// `first..=last` must be in range (window selections always are).
    fn join_segments(&self, first: usize, last: usize) -> String {
        self.snapshot.segments()[first..=last]
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(SEGMENT_JOIN)
    }

    /// Every window tells the tutor which physical page its text is on
    /// (field defect 2026-10-09, session 01a1212b: the tutor explained
    /// Figure 1.1 from page 19 while the reader sat on page 17 — nothing in
    /// the window said where a passage lives, so it could not turn there).
    /// A `[page N]` line ([`page_marker`]) opens the window and precedes each
    /// segment whose page differs from the one before it; pageless segments
    /// (`page_no <= 0`) carry none. Markers are metadata, like the sentinel:
    /// inserted AFTER the token trim and the hard byte cap, so they never
    /// spend book-text budget. `text` must be a prefix of the segments'
    /// `SEGMENT_JOIN` join (every window path's shape: the full join, a
    /// trimmed join, or a cut anchor prefix — optionally followed by the
    /// visual-element notes); a segment starting at or past the end of the
    /// book part is not marked.
    fn mark_pages(&self, text: &str, (first, last): (usize, usize), book_len: usize) -> String {
        let segments = self.snapshot.segments();
        if segments.is_empty() {
            return text.to_string();
        }
        let top = segments.len() - 1;
        let mut marked = String::with_capacity(text.len() + 16 * (last - first + 1));
        let mut cursor = 0usize;
        let mut offset = 0usize;
        let mut previous_page: Option<i64> = None;
        for segment in &segments[first.min(top)..=last.min(top)] {
            if offset >= book_len && offset > 0 {
                break;
            }
            if segment.page_no > 0 && previous_page != Some(segment.page_no) {
                marked.push_str(&text[cursor..offset]);
                marked.push_str(&page_marker(segment.page_no));
                cursor = offset;
            }
            if segment.page_no > 0 {
                previous_page = Some(segment.page_no);
            }
            offset += segment.text.len() + SEGMENT_JOIN.len();
            if offset > book_len {
                break;
            }
        }
        marked.push_str(&text[cursor..]);
        marked
    }
}

/// The character index (0..=total) that position `pos` maps to inside a
/// segment spanning `[start_ms, end_ms)` with `total` characters — linear,
/// rounded DOWN. Degenerate spans (no positions, no text) map to 0.
fn char_of_position(start_ms: u64, end_ms: u64, total: usize, pos: u64) -> usize {
    let span = end_ms.saturating_sub(start_ms);
    if span == 0 || total == 0 {
        return 0;
    }
    let offset = pos.clamp(start_ms, end_ms) - start_ms;
    ((offset as u128 * total as u128) / span as u128) as usize
}

/// The inverse of [`char_of_position`]: the position of character `index`
/// inside the segment, rounded down (`round_up == false`, for range starts)
/// or up (for range ends), so `[start, end)` pieces never leave a gap.
fn position_of_char(start_ms: u64, end_ms: u64, total: usize, index: usize, round_up: bool) -> u64 {
    let span = end_ms.saturating_sub(start_ms);
    if span == 0 || total == 0 {
        return if round_up { end_ms } else { start_ms };
    }
    let scaled = index.min(total) as u128 * span as u128;
    let offset = if round_up {
        scaled.div_ceil(total as u128)
    } else {
        scaled / total as u128
    };
    start_ms + (offset as u64).min(span)
}

/// Cut `text` to the largest PREFIX within half the reply budget — the
/// head-chunk bound of [`RangeFetch::Split`]. Returns whether anything was
/// cut, so the loss is never silent. Token-measured with a policy
/// ([`TruncationPolicy::largest_prefix_within`]), byte-measured against half
/// the hard cap without one.
fn half_cap_prefix<'t>(policy: Option<&TruncationPolicy>, text: &'t str) -> (&'t str, bool) {
    match policy {
        Some(policy) => {
            let kept = policy.largest_prefix_within(text, policy.max_excerpt_tokens() / 2);
            let cut = kept.len() != text.len();
            (kept, cut)
        }
        None => cap_at_char_boundary(text, HARD_EGRESS_BYTE_CAP / 2),
    }
}

/// The suffix twin of [`half_cap_prefix`] for the tail chunk: the largest
/// SUFFIX within half the budget, so the chunk carries the true END of the
/// requested range.
fn half_cap_suffix<'t>(policy: Option<&TruncationPolicy>, text: &'t str) -> (&'t str, bool) {
    match policy {
        Some(policy) => {
            let kept = policy.largest_suffix_within(text, policy.max_excerpt_tokens() / 2);
            let cut = kept.len() != text.len();
            (kept, cut)
        }
        None => cap_suffix_at_char_boundary(text, HARD_EGRESS_BYTE_CAP / 2),
    }
}

/// The suffix twin of [`cap_at_char_boundary`]: keep the trailing `cap` bytes,
/// advanced to the next char boundary so no code point is split.
fn cap_suffix_at_char_boundary(text: &str, cap: usize) -> (&str, bool) {
    if text.len() <= cap {
        return (text, false);
    }
    let mut start = text.len() - cap;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (&text[start..], true)
}

/// Cut at the greatest char boundary ≤ `cap` — a byte cap on UTF-8 must never
/// split a code point or the serialized excerpt stops being a `str`.
pub(super) fn cap_at_char_boundary(text: &str, cap: usize) -> (&str, bool) {
    if text.len() <= cap {
        return (text, false);
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::excerpt::source::SessionSnapshot;

    /// F7 tier 0: the window carries the ingestion-time labels of visual
    /// elements on ITS pages — every parsed class — and none from pages
    /// outside the window; page position resolves for the sight paths.
    /// Field defect 2026-10-09 (session 01a1212b): every window says which
    /// physical page each passage is on, so the tutor can turn the reader
    /// to it — a marker opens the window and precedes each page change, and
    /// a pageless segment carries none.
    #[test]
    fn a_window_marks_the_page_of_every_passage_it_carries() {
        let mut selector = selector_with(vec![
            (17, 0, 100, "chapter opener"),
            (18, 100, 200, "first half of page eighteen"),
            (18, 200, 300, "second half of page eighteen"),
            (0, 300, 350, "a pageless note"),
            (19, 350, 450, "as shown in Figure 1.1"),
        ]);
        let selected = selector.excerpt_for_range(0, 4);
        assert_eq!(
            payload(&selected.excerpt),
            "[page 17]\nchapter opener\n\n[page 18]\nfirst half of page eighteen\n\n\
             second half of page eighteen\n\na pageless note\n\n[page 19]\nas shown in Figure 1.1"
        );
        assert_eq!(
            book_payload(&selected.excerpt),
            "chapter opener\n\nfirst half of page eighteen\n\nsecond half of page eighteen\n\n\
             a pageless note\n\nas shown in Figure 1.1",
            "the markers are metadata on top of the unchanged book text"
        );
    }

    /// Field session 2026-10-09 (`01a121e0`): the tutor taught the chapter
    /// opening, jumped to the figure's page to show it, and went on from
    /// there. The jump knows which whole pages it skipped past the reading
    /// frontier; the partly read page holding the frontier is not skipped.
    #[test]
    fn a_page_window_reads_the_whole_page_and_continues_on_the_next() {
        let mut selector = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (5, 0, 100, "p5 one"),
                    (5, 100, 200, "p5 two"),
                    (5, 200, 300, "p5 three"),
                    (5, 300, 400, "p5 four"),
                    (5, 400, 500, "p5 five"),
                    (5, 500, 600, "p5 six"),
                    (5, 600, 700, "p5 seven"),
                    (6, 700, 800, "p6 one"),
                ],
                vec![(0, 800)],
            ),
            WindowPolicy {
                before: 2,
                after: 2,
            },
        );
        selector.set_page_windows(true);
        let excerpt = selector.excerpt_at(0).expect("a window");
        for text in ["p5 one", "p5 four", "p5 seven"] {
            assert!(excerpt.excerpt.delimited_text.contains(text), "{text}");
        }
        assert!(!excerpt.excerpt.delimited_text.contains("p6 one"));
        let next = selector.continue_after_window().expect("more book");
        assert_eq!((next.page, next.rest_of_page), (6, false));
    }

    #[test]
    fn a_budget_trimmed_window_continues_at_the_first_dropped_segment() {
        let mut selector = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (5, 0, 100, "alpha"),
                    (5, 100, 200, "bravo"),
                    (5, 200, 300, "charlie"),
                    (5, 300, 400, "delta"),
                    (6, 400, 500, "echo"),
                ],
                vec![(0, 500)],
            ),
            WindowPolicy {
                before: 0,
                after: 0,
            },
        );
        selector.set_page_windows(true);
        let policy = TruncationPolicy::new(2, |text: &str| text.split_whitespace().count());
        let excerpt = selector
            .budgeted_excerpt_at(0, &policy, &mut |_| {})
            .expect("a window");
        assert!(excerpt.excerpt.delimited_text.contains("bravo"));
        assert!(!excerpt.excerpt.delimited_text.contains("charlie"));
        let next = selector.continue_after_window().expect("more book");
        assert_eq!(
            (next.page, next.position, next.rest_of_page),
            (5, 200, true)
        );
    }

    #[test]
    fn a_forward_jump_knows_the_pages_it_skipped() {
        let mut selector = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (10, 0, 100, "chapter one"),
                    (20, 100, 200, "chapter two opens"),
                    (20, 200, 300, "rest of page twenty"),
                    (21, 300, 400, "page twenty-one"),
                    (22, 400, 500, "page twenty-two"),
                    (23, 500, 600, "the figure page"),
                ],
                vec![(0, 100), (100, 600)],
            ),
            WindowPolicy {
                before: 0,
                after: 0,
            },
        );
        assert_eq!(
            selector.skipped_pages_before(500),
            None,
            "nothing in the chapter covered yet: a first visit skips nothing"
        );
        selector.note_played(100, 250);
        assert_eq!(selector.skipped_pages_before(500), Some((21, 22)));
        assert_eq!(
            selector.skipped_pages_before(300),
            None,
            "the next page after the frontier skips nothing"
        );
        selector.note_played(250, 450);
        assert_eq!(
            selector.skipped_pages_before(500),
            None,
            "page 22 was read in part"
        );
    }

    #[test]
    fn a_window_names_the_page_reading_continues_on() {
        let mut selector = ExcerptSelector::new(
            SessionSnapshot::from_test_parts(vec![
                (20, 0, 100, "a"),
                (20, 100, 200, "b"),
                (21, 200, 300, "c"),
                (22, 300, 400, "d"),
            ]),
            WindowPolicy {
                before: 0,
                after: 1,
            },
        );
        assert_eq!(selector.continue_after_window(), None, "no window yet");
        selector.advance_to(0);
        assert_eq!(
            selector.continue_after_window(),
            Some(ContinueAt {
                page: 21,
                position: 200,
                rest_of_page: false
            })
        );
        selector.advance_to(300);
        assert_eq!(selector.continue_after_window(), None, "end of the book");
    }

    /// Review 2026-10-09, T1: a window that ends mid-page continues on the
    /// REST of that page, not on the next page.
    #[test]
    fn a_window_ending_mid_page_continues_on_the_rest_of_it() {
        let mut selector = ExcerptSelector::new(
            SessionSnapshot::from_test_parts(vec![
                (20, 0, 100, "a"),
                (20, 100, 200, "b"),
                (20, 200, 300, "c"),
                (21, 300, 400, "d"),
            ]),
            WindowPolicy {
                before: 0,
                after: 1,
            },
        );
        selector.advance_to(0);
        assert_eq!(
            selector.continue_after_window(),
            Some(ContinueAt {
                page: 20,
                position: 200,
                rest_of_page: true
            }),
            "the window held only the first two blocks of page 20"
        );
    }

    /// Review 2026-10-09, T2: the chapter-one tail is owed when a jump
    /// lands in chapter two; whole chapters jumped over are not.
    #[test]
    fn a_jump_into_the_next_chapter_owes_the_unread_tail_of_this_one() {
        let mut selector = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![
                    (1, 0, 50, "front matter"),
                    (10, 100, 200, "chapter one opens"),
                    (11, 200, 300, "chapter one middle"),
                    (12, 300, 400, "chapter one end"),
                    (13, 400, 500, "chapter two opens"),
                    (14, 500, 600, "chapter two figure"),
                    (15, 600, 700, "chapter three"),
                ],
                vec![(100, 400), (400, 600), (600, 700)],
            ),
            WindowPolicy {
                before: 0,
                after: 0,
            },
        );
        selector.note_played(100, 200);
        assert_eq!(selector.skipped_pages_before(400), Some((11, 12)));
        assert_eq!(
            selector.skipped_pages_before(600),
            Some((11, 12)),
            "chapter two (pages 13-14), jumped over whole, is not owed"
        );
        let mut front = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![(1, 0, 50, "front matter"), (10, 100, 200, "chapter one")],
                vec![(100, 200)],
            ),
            WindowPolicy {
                before: 0,
                after: 0,
            },
        );
        front.note_played(0, 50);
        assert_eq!(
            front.skipped_pages_before(100),
            None,
            "leaving the front matter for chapter one owes nothing"
        );
    }

    #[test]
    fn the_window_carries_its_pages_visual_element_labels() {
        let snapshot = SessionSnapshot::from_test_parts(vec![
            (1, 0, 1000, "page one text"),
            (2, 1000, 2000, "page two text"),
            (8, 8000, 9000, "filler eight"),
            (9, 9000, 9900, "far away text"),
        ])
        .with_test_figures(vec![
            (
                1,
                "figure",
                "Figure 3: the loss curve — a downward-sloping validation loss",
            ),
            (2, "table", "Table 1 compares learning rates"),
            (2, "formula", "the gradient update rule"),
            (9, "figure", "an out-of-window diagram"),
            (1, "figure", "   "),
        ]);
        let mut selector = ExcerptSelector::new(
            snapshot,
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let selected = selector.excerpt_at(500).expect("segments exist");
        let text = &selected.excerpt.delimited_text;
        assert!(
            text.contains("Visual elements on these pages")
                && text.contains("figure on page 1 (anchor id fig-0): Figure 3: the loss curve")
                && text
                    .contains("table on page 2 (anchor id fig-1): Table 1 compares learning rates")
                && text.contains("formula on page 2 (anchor id fig-2): the gradient update rule"),
            "every parsed visual class on the window's pages rides as text: {text}"
        );
        assert!(
            !text.contains("out-of-window"),
            "other pages' elements stay home: {text}"
        );
        assert_eq!(selector.page_at(1500), Some(2), "sight resolves the page");
        assert_eq!(selector.page_at(0), Some(1));
        assert_eq!(selector.page_at(999_999), Some(9), "clamped to the end");
    }

    fn selector_with(parts: Vec<(i64, u64, u64, &str)>) -> ExcerptSelector {
        // Range-level tests bypass the window; the policy is inert for them.
        ExcerptSelector::new(
            SessionSnapshot::from_test_parts(parts),
            WindowPolicy {
                before: 0,
                after: 0,
            },
        )
    }

    fn payload(excerpt: &DelimitedExcerpt) -> &str {
        // The payload is everything between the first and last marker lines.
        let text = &excerpt.delimited_text;
        let after_open = text.find('\n').expect("open marker line") + 1;
        let before_close = text.rfind('\n').expect("close marker line");
        &text[after_open..before_close]
    }

    /// The payload's book text, page markers removed — for pins whose
    /// subject is the budget or the cap, not the marking.
    fn book_payload(excerpt: &DelimitedExcerpt) -> String {
        strip_page_markers(payload(excerpt))
    }

    #[test]
    fn a_range_renders_its_segments_joined_as_paragraphs() {
        let mut selector = selector_with(vec![
            (1, 0, 100, "alpha"),
            (1, 100, 200, "beta"),
            (2, 200, 300, "gamma"),
        ]);
        let selected = selector.excerpt_for_range(0, 2);
        assert_eq!(
            payload(&selected.excerpt),
            "[page 1]\nalpha\n\nbeta\n\n[page 2]\ngamma"
        );
        assert_eq!(selected.segment_range, (0, 2));
        assert_eq!(selected.hard_cap, None);
    }

    #[test]
    fn out_of_range_bounds_clamp_instead_of_panicking() {
        let mut selector = selector_with(vec![(1, 0, 100, "alpha"), (1, 100, 200, "beta")]);
        let selected = selector.excerpt_for_range(1, 99);
        assert_eq!(payload(&selected.excerpt), "[page 1]\nbeta");
        assert_eq!(selected.segment_range, (1, 1));
    }

    #[test]
    fn an_empty_snapshot_yields_an_empty_payload() {
        let mut selector = selector_with(vec![]);
        let selected = selector.excerpt_for_range(0, 0);
        assert_eq!(payload(&selected.excerpt), "");
        assert_eq!(selected.hard_cap, None);
    }

    // Cap arithmetic pinned with test-local literals (the production constant
    // is asserted equal so a silent cap change reds here, not in the field).
    const CAP: usize = 65536;

    #[test]
    fn the_hard_cap_is_the_documented_64_kib() {
        assert_eq!(HARD_EGRESS_BYTE_CAP, CAP);
    }

    #[test]
    fn one_byte_over_the_cap_drops_exactly_one_byte_and_says_so() {
        let oversized = "x".repeat(CAP + 1);
        let mut selector = selector_with(vec![(1, 0, 100, oversized.as_str())]);
        let selected = selector.excerpt_for_range(0, 0);
        assert_eq!(book_payload(&selected.excerpt).len(), CAP);
        assert_eq!(
            selected.hard_cap,
            Some(HardCapNotice {
                kept_bytes: CAP,
                dropped_bytes: 1,
            })
        );
    }

    #[test]
    fn at_the_cap_exactly_nothing_is_dropped_and_no_notice_is_raised() {
        let exact = "y".repeat(CAP);
        let mut selector = selector_with(vec![(1, 0, 100, exact.as_str())]);
        let selected = selector.excerpt_for_range(0, 0);
        assert_eq!(book_payload(&selected.excerpt).len(), CAP);
        assert_eq!(selected.hard_cap, None);
    }

    #[test]
    fn a_multibyte_char_straddling_the_cap_is_dropped_whole() {
        // 65534 ASCII bytes + '語' (3 bytes UTF-8) puts the boundary mid-char
        // at 65536; the cut must retreat to 65534, never split the code point.
        let mut text = "z".repeat(CAP - 2);
        text.push('語');
        let mut selector = selector_with(vec![(1, 0, 100, text.as_str())]);
        let selected = selector.excerpt_for_range(0, 0);
        assert_eq!(book_payload(&selected.excerpt).len(), CAP - 2);
        assert_eq!(
            selected.hard_cap,
            Some(HardCapNotice {
                kept_bytes: CAP - 2,
                dropped_bytes: 3,
            })
        );
        assert!(book_payload(&selected.excerpt).is_char_boundary(CAP - 2));
    }

    /// The VA4 acceptance fixture session: the window follows the playback
    /// position as it advances (and seeks), payload-pinned — each step asserts
    /// exactly WHICH segments' text is inside the shipped excerpt. Positions
    /// arrive through the VA2 seam (`ContextUpdate::PlaybackPosition`), the
    /// provider-neutral carrier of the reading clock.
    #[test]
    fn the_window_follows_playback_through_a_fixture_session() {
        use crate::voice::channel::types::ContextUpdate;

        let snapshot = SessionSnapshot::from_test_parts(vec![
            (1, 0, 1000, "the opening paragraph"),
            (1, 1000, 2000, "the second paragraph"),
            (2, 2000, 3000, "the chapter one close"),
            (3, 3600, 4600, "the chapter two opening"),
            (3, 4600, 5600, "the chapter two middle"),
            (4, 5600, 6600, "the final paragraph"),
        ]);
        let mut selector = ExcerptSelector::new(
            snapshot,
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );

        // (position, expected window, exact expected payload)
        let session: Vec<(u64, (usize, usize), &str)> = vec![
            (0, (0, 1), "the opening paragraph\n\nthe second paragraph"),
            (
                1500,
                (0, 2),
                "the opening paragraph\n\nthe second paragraph\n\nthe chapter one close",
            ),
            (
                2500,
                (1, 3),
                "the second paragraph\n\nthe chapter one close\n\nthe chapter two opening",
            ),
            // 3200 ms sits in the chapter gap: the upcoming segment anchors.
            (
                3200,
                (2, 4),
                "the chapter one close\n\nthe chapter two opening\n\nthe chapter two middle",
            ),
            (
                6100,
                (4, 5),
                "the chapter two middle\n\nthe final paragraph",
            ),
            // A seek backward re-anchors just like forward progress.
            (
                1500,
                (0, 2),
                "the opening paragraph\n\nthe second paragraph\n\nthe chapter one close",
            ),
        ];

        for (pos_ms, expected_range, expected_payload) in session {
            let update = ContextUpdate::PlaybackPosition { pos_ms };
            let ContextUpdate::PlaybackPosition { pos_ms } = update else {
                unreachable!("the fixture feeds positions only");
            };
            let selected = selector.excerpt_at(pos_ms).expect("non-empty snapshot");
            assert_eq!(
                (selected.segment_range, book_payload(&selected.excerpt)),
                (expected_range, expected_payload.to_string()),
                "at {pos_ms} ms"
            );
            assert_eq!(
                selector.current_window().map(|w| (w.first_idx, w.last_idx)),
                Some(expected_range),
                "the engine's notion of the current window tracks the same step"
            );
        }
    }

    #[test]
    fn the_rolling_summary_tracks_the_window_and_never_pays_twice() {
        let snapshot = SessionSnapshot::from_test_parts(vec![
            (1, 0, 1000, "s0"),
            (1, 1000, 2000, "s1"),
            (2, 2000, 3000, "s2"),
            (3, 3600, 4600, "s3"),
            (3, 4600, 5600, "s4"),
            (4, 5600, 6600, "s5"),
        ]);
        let mut selector = ExcerptSelector::new(
            snapshot,
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let call_count = std::cell::Cell::new(0usize);
        let mut summarize = |previous: Option<&str>, new_text: &str| {
            call_count.set(call_count.get() + 1);
            format!("[{}]+({new_text})", previous.unwrap_or(""))
        };

        assert_eq!(
            selector.rolling_summary_before_window(&mut summarize),
            None,
            "no window resolved yet, so nothing precedes it"
        );

        selector.advance_to(500); // window 0..=1, boundary 0
        assert_eq!(selector.rolling_summary_before_window(&mut summarize), None);
        assert_eq!(call_count.get(), 0, "top-of-book costs nothing");

        selector.advance_to(4700); // anchor 4, window 3..=5, boundary 3
        assert_eq!(
            selector.rolling_summary_before_window(&mut summarize),
            Some("[]+(s0\n\ns1\n\ns2)")
        );
        assert_eq!(
            selector.rolling_summary_before_window(&mut summarize),
            Some("[]+(s0\n\ns1\n\ns2)")
        );
        assert_eq!(
            call_count.get(),
            1,
            "the re-request within the session is a hit"
        );

        selector.advance_to(6100); // anchor 5, window 4..=5, boundary 4
        assert_eq!(
            selector.rolling_summary_before_window(&mut summarize),
            Some("[[]+(s0\n\ns1\n\ns2)]+(s3)"),
            "the advance extends the cached prefix with only the newly-passed segment"
        );
        assert_eq!(call_count.get(), 2);
    }

    #[test]
    fn delimit_untrusted_wraps_with_the_session_sentinel() {
        let mut selector = selector_with(vec![(1, 0, 100, "alpha")]);
        let wrapped = selector.delimit_untrusted("a rolling summary the harness holds");
        assert_eq!(
            payload(&wrapped),
            "a rolling summary the harness holds",
            "the helper delimits what the caller already has; it reads nothing"
        );
        let book_wrapped = selector.excerpt_for_range(0, 0);
        let marker_line = |text: &str| text.lines().next().expect("open marker").to_string();
        assert_eq!(
            marker_line(&wrapped.delimited_text),
            marker_line(&book_wrapped.excerpt.delimited_text),
            "same session sentinel as the excerpt path, so the §4 collision re-roll \
             discipline covers both"
        );
    }

    // ---- U4: per-provider truncation policy ----
    // Word-count tokenizer throughout: every segment below is 3 words, and the
    // "\n\n" join is whitespace to it, so a k-segment window counts 3k tokens.

    fn word_policy(budget: usize) -> TruncationPolicy {
        TruncationPolicy::new(budget, |text: &str| text.split_whitespace().count())
    }

    fn five_segment_selector() -> ExcerptSelector {
        ExcerptSelector::new(
            SessionSnapshot::from_test_parts(vec![
                (1, 0, 1000, "seg zero words"),
                (1, 1000, 2000, "seg one words"),
                (1, 2000, 3000, "seg two words"),
                (1, 3000, 4000, "seg three words"),
                (1, 4000, 5000, "seg four words"),
            ]),
            WindowPolicy {
                before: 2,
                after: 2,
            },
        )
    }

    #[test]
    fn a_window_within_budget_is_untouched_and_raises_no_event() {
        let mut selector = five_segment_selector();
        let mut events = Vec::new();
        let selected = selector
            .budgeted_excerpt_at(2500, &word_policy(15), &mut |event| events.push(event))
            .expect("non-empty");
        assert_eq!(selected.segment_range, (0, 4));
        assert_eq!(selected.truncation, None);
        assert!(
            events.is_empty(),
            "no content loss, no event — the sink stays quiet so a raised event always means \
             something"
        );
    }

    #[test]
    fn an_over_budget_window_sheds_far_edges_first_and_says_so_twice() {
        // Anchor 2, window 0..=4 = 15 tokens, budget 9: tie drops leading
        // (first=1, 12 tokens), then trailing is farther (last=3, 9 tokens).
        let mut selector = five_segment_selector();
        let mut events = Vec::new();
        let selected = selector
            .budgeted_excerpt_at(2500, &word_policy(9), &mut |event| events.push(event))
            .expect("non-empty");
        assert_eq!(selected.segment_range, (1, 3));
        assert_eq!(
            payload(&selected.excerpt),
            "[page 1]\nseg one words\n\nseg two words\n\nseg three words",
            "the anchor stays; one segment gone from each edge"
        );
        let notice = TruncationNotice {
            budget_tokens: 9,
            window_tokens: 15,
            dropped_leading_segments: 1,
            dropped_trailing_segments: 1,
            anchor_partially_cut: false,
        };
        assert_eq!(selected.truncation, Some(notice), "loud on the result");
        assert_eq!(
            events,
            vec![EgressEvent::ExcerptTruncated {
                budget_tokens: 9,
                window_tokens: 15,
                dropped_leading_segments: 1,
                dropped_trailing_segments: 1,
                anchor_partially_cut: false,
            }],
            "and loud in the event stream"
        );
    }

    #[test]
    fn on_an_edge_tie_the_leading_segment_goes_because_the_summary_covers_it() {
        // Anchor 1 of segments 0..=2 (before=2 clamps to 0), 9 tokens, budget
        // 6: sides are 1 and 1 — the leading edge must be the one dropped.
        let mut selector = ExcerptSelector::new(
            SessionSnapshot::from_test_parts(vec![
                (1, 0, 1000, "seg zero words"),
                (1, 1000, 2000, "seg one words"),
                (1, 2000, 3000, "seg two words"),
            ]),
            WindowPolicy {
                before: 2,
                after: 2,
            },
        );
        let mut events = Vec::new();
        let selected = selector
            .budgeted_excerpt_at(1500, &word_policy(6), &mut |event| events.push(event))
            .expect("non-empty");
        assert_eq!(selected.segment_range, (1, 2));
        assert_eq!(
            payload(&selected.excerpt),
            "[page 1]\nseg one words\n\nseg two words",
            "upcoming text survives; already-heard text is the rolling summary's job"
        );
        let truncation = selected.truncation.expect("marked");
        assert_eq!(
            (
                truncation.dropped_leading_segments,
                truncation.dropped_trailing_segments
            ),
            (1, 0)
        );
    }

    #[test]
    fn an_anchor_alone_over_budget_is_cut_to_the_largest_fitting_prefix() {
        let mut selector = five_segment_selector();
        let mut events = Vec::new();
        let selected = selector
            .budgeted_excerpt_at(2500, &word_policy(2), &mut |event| events.push(event))
            .expect("non-empty");
        assert_eq!(
            selected.segment_range,
            (2, 2),
            "everything but the anchor shed"
        );
        assert_eq!(
            payload(&selected.excerpt),
            "[page 1]\nseg two ",
            "the largest prefix the word counter accepts at budget 2"
        );
        let truncation = selected.truncation.expect("marked");
        assert_eq!(
            (
                truncation.dropped_leading_segments,
                truncation.dropped_trailing_segments,
                truncation.anchor_partially_cut,
            ),
            (2, 2, true)
        );
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn the_current_window_can_be_budgeted_without_reanchoring() {
        let mut selector = five_segment_selector();
        let mut events = Vec::new();
        assert!(
            selector
                .budgeted_excerpt_for_window(&word_policy(9), &mut |event| events.push(event))
                .is_none(),
            "no window resolved yet, so there is nothing to scope to"
        );
        selector.advance_to(2500);
        let selected = selector
            .budgeted_excerpt_for_window(&word_policy(9), &mut |event| events.push(event))
            .expect("a window is resolved");
        assert_eq!(
            selected.segment_range,
            (1, 3),
            "the same trim the at-position path applies to the same window"
        );
        assert_eq!(
            selector.current_window().map(|w| (w.first_idx, w.last_idx)),
            Some((0, 4)),
            "budgeting renders a trimmed view; it never moves the injected window itself"
        );
    }

    #[test]
    fn the_hard_byte_cap_is_loud_on_the_budgeted_path_too() {
        // A token budget too permissive to trim (one flat token for any text)
        // must not let an oversized payload slip out silently: the backstop
        // fires and reaches the event stream.
        let oversized = "x".repeat(CAP + 10);
        let mut selector = ExcerptSelector::new(
            SessionSnapshot::from_test_parts(vec![(1, 0, 1000, oversized.as_str())]),
            WindowPolicy {
                before: 0,
                after: 0,
            },
        );
        let mut events = Vec::new();
        let selected = selector
            .budgeted_excerpt_at(0, &TruncationPolicy::new(1, |_: &str| 1), &mut |event| {
                events.push(event)
            })
            .expect("non-empty");
        assert_eq!(selected.truncation, None, "the token budget saw one token");
        assert_eq!(
            events,
            vec![EgressEvent::HardByteCapApplied {
                kept_bytes: CAP,
                dropped_bytes: 10,
            }]
        );
        assert_eq!(selected.hard_cap.map(|notice| notice.kept_bytes), Some(CAP));
    }

    // ---- F8 D1b: the bounded-range fetch (`get_context_for_range`) --------

    /// Word-counting policy: deterministic, provider-agnostic, and coarse
    /// enough that the split arithmetic is readable in the fixtures.
    fn words(budget: usize) -> TruncationPolicy {
        TruncationPolicy::new(budget, |text: &str| text.split_whitespace().count())
    }

    #[test]
    fn positions_resolve_to_every_overlapping_segment_and_only_those() {
        let selector = selector_with(vec![
            (1, 0, 1000, "alpha"),
            (1, 1000, 2000, "beta"),
            (2, 2000, 3000, "gamma"),
        ]);
        assert_eq!(selector.segment_span_for_positions(0, 3000), Some((0, 2)));
        assert_eq!(
            selector.segment_span_for_positions(500, 1500),
            Some((0, 1)),
            "partial overlap counts on both edges"
        );
        assert_eq!(
            selector.segment_span_for_positions(1000, 2000),
            Some((1, 1))
        );
        assert_eq!(selector.segment_span_for_positions(3000, 9000), None);
        assert_eq!(selector.segment_span_for_positions(200, 200), None);
        assert_eq!(selector.segment_span_for_positions(500, 100), None);
    }

    #[test]
    fn a_range_within_the_budget_returns_the_exact_snapshot_text_whole() {
        let mut selector = selector_with(vec![
            (1, 0, 1000, "alpha"),
            (1, 1000, 2000, "beta"),
            (2, 2000, 3000, "gamma"),
        ]);
        let policy = words(100);
        let mut events = Vec::new();
        let fetch = selector.range_fetch(0, 2, Some(&policy), &mut |event| events.push(event));
        let RangeFetch::Full(selected) = fetch else {
            panic!("a fitting range must seal whole");
        };
        assert_eq!(payload(&selected.excerpt), "alpha\n\nbeta\n\ngamma");
        assert_eq!(selected.segment_range, (0, 2));
        assert!(events.is_empty(), "nothing was dropped: {events:?}");
    }

    #[test]
    fn an_oversized_range_splits_into_head_and_tail_with_honest_omission_counts() {
        let mut selector = selector_with(vec![
            (1, 0, 1000, "seg zero"),
            (1, 1000, 2000, "seg one"),
            (2, 2000, 3000, "seg two"),
            (2, 3000, 4000, "seg three"),
            (3, 4000, 5000, "seg four"),
        ]);
        // 10 words total; budget 4 → half-budget 2 → one segment per chunk.
        let policy = words(4);
        let mut events = Vec::new();
        let fetch = selector.range_fetch(0, 4, Some(&policy), &mut |event| events.push(event));
        let RangeFetch::Split {
            head,
            head_partially_cut,
            tail,
            tail_partially_cut,
            omitted_segments,
            total_segments,
            ..
        } = fetch
        else {
            panic!("an oversized range must return the honest split");
        };
        assert_eq!(payload(&head.excerpt), "seg zero");
        assert_eq!(payload(&tail.excerpt), "seg four");
        assert_eq!(head.segment_range, (0, 0));
        assert_eq!(tail.segment_range, (4, 4));
        assert!(!head_partially_cut && !tail_partially_cut);
        assert_eq!(omitted_segments, 3);
        assert_eq!(total_segments, 5);
        assert!(events.is_empty(), "a split is not a hard-cap event");
    }

    #[test]
    fn split_chunks_never_overlap_even_when_half_budgets_could_meet() {
        let mut selector = selector_with(vec![
            (1, 0, 1000, "one"),
            (1, 1000, 2000, "two"),
            (2, 2000, 3000, "three"),
        ]);
        // 3 words total; budget 2 → doesn't fit, half-budget 1: head takes
        // segment 0, tail must stop before segment 1 is double-served.
        let policy = words(2);
        let fetch = selector.range_fetch(0, 2, Some(&policy), &mut |_| {});
        let RangeFetch::Split {
            head,
            tail,
            omitted_segments,
            total_segments,
            ..
        } = fetch
        else {
            panic!("expected a split");
        };
        assert!(head.segment_range.1 < tail.segment_range.0, "no overlap");
        assert_eq!(total_segments, 3);
        assert_eq!(
            omitted_segments,
            tail.segment_range.0 - head.segment_range.1 - 1
        );
    }

    #[test]
    fn without_a_policy_the_hard_byte_cap_is_the_range_budget() {
        let big = "z".repeat(CAP / 2);
        let mut selector = selector_with(vec![
            (1, 0, 1000, big.as_str()),
            (1, 1000, 2000, big.as_str()),
            (2, 2000, 3000, big.as_str()),
        ]);
        let fetch = selector.range_fetch(0, 2, None, &mut |_| {});
        let RangeFetch::Split { head, tail, .. } = fetch else {
            panic!("3 × half-cap exceeds the byte budget and must split");
        };
        assert!(payload(&head.excerpt).len() <= CAP / 2);
        assert!(payload(&tail.excerpt).len() <= CAP / 2);
    }

    /// F10-D1 round 4: a ONE-segment request over the budget used to compute
    /// `tail_first - head_last - 1` with equal indices — a debug underflow.
    /// It must answer a usable Split: zero omitted segments, both chunks cut,
    /// and positional cut points strictly inside the segment with
    /// `head_end < tail_start`.
    #[test]
    fn a_one_segment_over_budget_range_splits_without_underflow() {
        let text = (0..40)
            .map(|i| format!("w{i:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut selector = ExcerptSelector::new(
            SessionSnapshot::from_voice_test_parts(
                vec![(1, 0, 10_000, text.as_str())],
                vec![(0, 10_000)],
            ),
            WindowPolicy {
                before: 1,
                after: 1,
            },
        );
        let policy = word_policy(6);
        let mut events = Vec::new();
        let fetch = selector.range_fetch(0, 0, Some(&policy), &mut |event| events.push(event));
        let RangeFetch::Split {
            head_partially_cut,
            tail_partially_cut,
            omitted_segments,
            total_segments,
            head_end,
            tail_start,
            ..
        } = fetch
        else {
            panic!("one oversized segment answers Split");
        };
        assert!(head_partially_cut && tail_partially_cut);
        assert_eq!((omitted_segments, total_segments), (0, 1));
        assert!(0 < head_end && head_end < tail_start && tail_start < 10_000);
    }

    #[test]
    fn oversized_single_segments_cut_to_a_disclosed_prefix_and_suffix() {
        let mut selector = selector_with(vec![
            (1, 0, 1000, "alpha beta gamma delta epsilon zeta"),
            (1, 1000, 2000, "eta theta iota kappa lambda mu"),
        ]);
        // 12 words; budget 4 → half 2: the head keeps a 2-word PREFIX, the
        // tail keeps a 2-word SUFFIX (it must carry the range's true end),
        // and both cuts are disclosed — never silent.
        let policy = words(4);
        let fetch = selector.range_fetch(0, 1, Some(&policy), &mut |_| {});
        let RangeFetch::Split {
            head,
            head_partially_cut,
            tail,
            tail_partially_cut,
            ..
        } = fetch
        else {
            panic!("expected a split");
        };
        assert!(
            policy.count(payload(&head.excerpt)) <= 2,
            "head within half"
        );
        assert!(
            policy.count(payload(&tail.excerpt)) <= 2,
            "tail within half"
        );
        assert!(payload(&head.excerpt).starts_with("alpha"));
        assert!(
            payload(&tail.excerpt).ends_with("lambda mu"),
            "the tail is a suffix — the range's end survives: {}",
            payload(&tail.excerpt)
        );
        assert!(
            head_partially_cut && tail_partially_cut,
            "intra-segment cuts are disclosed"
        );
    }

    #[test]
    fn the_cap_counts_book_bytes_not_marker_bytes() {
        let exact = "w".repeat(CAP);
        let mut selector = selector_with(vec![(1, 0, 100, exact.as_str())]);
        let selected = selector.excerpt_for_range(0, 0);
        assert!(
            selected.excerpt.delimited_text.len() > CAP,
            "the serialized excerpt exceeds the cap by the marker overhead; if this ever \
             equals the payload length the markers were counted against book budget"
        );
        assert_eq!(selected.hard_cap, None);
    }
}
