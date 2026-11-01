//! Coverage memory (F8 §2.1 D1): the media-player *played-ranges* model —
//! an interval union of positions actually traversed, HTMLMediaElement
//! `played: TimeRanges` semantics (WHATWG HTML §4.8.11.6), NOT a
//! high-watermark. The distinction is the honesty law: a forward seek must
//! leave the skipped gap uncovered, so the tutor can never claim unheard
//! content was covered. The same structure serves both session kinds — the
//! units are milliseconds on the narration timeline (BookGrounded) or
//! character offsets into the book text (BookDirect); this module never
//! interprets them beyond ordering.
//!
//! No book text lives here: coverage is arithmetic over positions the
//! session already owns. Text egress stays with `ExcerptSelector`.

use super::source::SessionChapter;

/// Two samples closer than this merge into one interval even across a tiny
/// numeric gap (report jitter between 1 s heartbeats). OWNER-TUNABLE.
/// Failure direction if too large: a short skip under the gap is bridged as
/// played (over-claims up to this many units); if too small: fragmented
/// intervals that still read correctly, only less compactly.
pub const MERGE_GAP: u64 = 250;

/// Upper bound on stored intervals. A listener produces very few (each seek
/// starts at most one new interval); anything near this bound is a degenerate
/// seek storm. On overflow the SHORTEST interval is shed — coverage may then
/// UNDER-claim (the honest failure direction: a played range reads as
/// skipped), never over-claim. OWNER-TUNABLE.
pub const MAX_INTERVALS: usize = 256;

/// A chapter is "fully played" when its uncovered remainder is at most this
/// many units — segment/chapter boundaries carry encoder gaps a listener
/// cannot hear. OWNER-TUNABLE. Failure direction if too large: a chapter
/// with a skipped tail up to this size reads as fully played (over-claims
/// that tail); if too small: a fully-heard chapter reads as partial (honest
/// direction).
pub const CHAPTER_FULL_SLACK: u64 = 1_500;

/// Interval union of positions actually traversed (module doc). Intervals
/// are half-open `[start, end)`, kept sorted and disjoint by construction.
#[derive(Debug, Clone, Default)]
pub struct PlayedRanges {
    intervals: Vec<(u64, u64)>,
}

impl PlayedRanges {
    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    /// The disjoint, sorted intervals.
    pub fn ranges(&self) -> &[(u64, u64)] {
        &self.intervals
    }

    /// The persisted form (review 2026-10-09, T3): the intervals plus the
    /// position units they are in (`"ms"` or `"offset"`), so a restore never
    /// mixes a narrated timeline with a text-offset one.
    pub fn to_json(&self, units: &str) -> String {
        serde_json::json!({ "units": units, "ranges": self.intervals }).to_string()
    }

    /// The inverse of [`Self::to_json`]; `None` for malformed input or
    /// other units. Every interval re-enters through [`Self::note`], so a
    /// hand-edited row cannot break the sorted/disjoint/bounded invariants.
    pub fn from_json(json: &str, units: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(json).ok()?;
        if value.get("units")?.as_str()? != units {
            return None;
        }
        let mut ranges = Self::default();
        for pair in value.get("ranges")?.as_array()? {
            let (start, end) = (pair.get(0)?.as_u64()?, pair.get(1)?.as_u64()?);
            ranges.note(start, end);
        }
        Some(ranges)
    }

    /// Record one traversed interval. Merge-on-insert: anything overlapping
    /// or within [`MERGE_GAP`] of the new interval folds into it. Empty and
    /// inverted inputs are ignored.
    pub fn note(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let (mut start, mut end) = (start, end);
        // Absorb every interval that overlaps or nearly touches [start, end).
        self.intervals.retain(|&(a, b)| {
            let disjoint = b.saturating_add(MERGE_GAP) < start || end.saturating_add(MERGE_GAP) < a;
            if !disjoint {
                start = start.min(a);
                end = end.max(b);
            }
            disjoint
        });
        let at = self.intervals.partition_point(|&(a, _)| a < start);
        self.intervals.insert(at, (start, end));
        if self.intervals.len() > MAX_INTERVALS {
            // Shed the shortest: under-claims, never over-claims (const doc).
            if let Some(shortest) = self
                .intervals
                .iter()
                .enumerate()
                .min_by_key(|(_, &(a, b))| b - a)
                .map(|(at, _)| at)
            {
                self.intervals.remove(shortest);
            }
        }
    }

    /// The sub-intervals of `[start, end)` NOT covered by any played range
    /// (F8 D1b gap honesty): the exact complement, in order. Empty when the
    /// whole request is covered; the whole request when nothing in it played.
    /// The bounded-range fetch names these gaps so a recap or quiz can never
    /// silently claim unheard content (the D1 quiz-bounding law).
    pub fn gaps_within(&self, start: u64, end: u64) -> Vec<(u64, u64)> {
        if end <= start {
            return Vec::new();
        }
        let mut gaps = Vec::new();
        let mut cursor = start;
        for &(a, b) in &self.intervals {
            if b <= cursor {
                continue;
            }
            if a >= end {
                break;
            }
            if a > cursor {
                gaps.push((cursor, a.min(end)));
            }
            cursor = cursor.max(b);
            if cursor >= end {
                return gaps;
            }
        }
        if cursor < end {
            gaps.push((cursor, end));
        }
        gaps
    }

    /// The outermost played span `[first_start, last_end)`, `None` while
    /// nothing has played. The bounded-range fetch's played-range selector
    /// resolves to this span; interior gaps stay visible through
    /// [`PlayedRanges::gaps_within`].
    pub fn outer_span(&self) -> Option<(u64, u64)> {
        let first = self.intervals.first()?.0;
        let last = self.intervals.last()?.1;
        Some((first, last))
    }

    /// How much of `[start, end)` is covered, in units.
    pub fn covered_within(&self, start: u64, end: u64) -> u64 {
        self.intervals
            .iter()
            .map(|&(a, b)| b.min(end).saturating_sub(a.max(start)))
            .sum()
    }
}

/// One chapter's coverage status against the played ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterCoverage {
    Played,
    Partial,
    Untouched,
}

pub fn chapter_status(ranges: &PlayedRanges, chapter: &SessionChapter) -> ChapterCoverage {
    let span = chapter.end_ms.saturating_sub(chapter.start_ms);
    let covered = ranges.covered_within(chapter.start_ms, chapter.end_ms);
    if covered == 0 {
        ChapterCoverage::Untouched
    } else if span.saturating_sub(covered) <= CHAPTER_FULL_SLACK {
        ChapterCoverage::Played
    } else {
        ChapterCoverage::Partial
    }
}

/// One playback-position sample folded into the accumulator's state machine
/// (the grounded feed; the runtime calls this on its sampling cadence).
/// Returns the interval to record, if the step from `prev` to `pos` was
/// continuous forward motion: a pause reads as a zero step (nothing to
/// record), a seek in either direction reads as a discontinuity (nothing to
/// record — the NEXT step measures from the landing point). `max_step` is
/// the continuity bound: the largest advance one sample period can honestly
/// produce (period × max playback speed, plus slack).
pub fn coverage_step(prev: Option<u64>, pos: u64, max_step: u64) -> Option<(u64, u64)> {
    let prev = prev?;
    (pos > prev && pos - prev <= max_step).then_some((prev, pos))
}

/// How positions render in the coverage line: the narration clock
/// (BookGrounded) or character offsets (BookDirect).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionUnits {
    Millis,
    Offset,
}

pub fn position_label(units: PositionUnits, value: u64) -> String {
    match units {
        PositionUnits::Millis => {
            let total_s = value / 1000;
            let (h, m, s) = (total_s / 3600, (total_s % 3600) / 60, total_s % 60);
            if h > 0 {
                format!("{h}:{m:02}:{s:02}")
            } else {
                format!("{m}:{s:02}")
            }
        }
        PositionUnits::Offset => format!("offset {value}"),
    }
}

fn chapter_list(
    chapters: &[SessionChapter],
    wanted: ChapterCoverage,
    ranges: &PlayedRanges,
) -> String {
    let picked: Vec<String> = chapters
        .iter()
        .filter(|chapter| chapter_status(ranges, chapter) == wanted)
        .map(|chapter| format!("{} ({})", chapter.idx, chapter.title))
        .collect();
    if picked.len() > CHAPTER_LIST_VERBATIM_MAX {
        let overflow = picked.len() - CHAPTER_LIST_VERBATIM_MAX;
        format!(
            "{}, and {overflow} more",
            picked[..CHAPTER_LIST_VERBATIM_MAX].join(", ")
        )
    } else {
        picked.join(", ")
    }
}

/// How many chapter-list entries the long-form line renders verbatim
/// before capping with an "and N more" tail. OWNER-TUNABLE. Failure
/// direction if too low: the model loses named titles it could have used
/// (the indices still ride); if too high: long sessions bloat the recap
/// payload again — the defect the cap exists to bound.
const CHAPTER_LIST_VERBATIM_MAX: usize = 3;

/// How many played spans the HEADER renders verbatim before capping.
/// OWNER-TUNABLE, same failure directions as the chapter cap.
const HEADER_SPAN_LIST_MAX: usize = 3;

/// How many played spans the long-form line renders verbatim before capping.
const LONG_FORM_SPAN_LIST_MAX: usize = 8;

fn span_list(ranges: &PlayedRanges, units: PositionUnits, max: usize) -> String {
    let spans: Vec<String> = ranges
        .ranges()
        .iter()
        .map(|&(a, b)| format!("{}–{}", position_label(units, a), position_label(units, b)))
        .collect();
    if spans.len() > max {
        let overflow = spans.len() - max;
        format!("{}, and {overflow} more", spans[..max].join(", "))
    } else {
        spans.join(", ")
    }
}

/// The bounded per-fetch coverage header (2026-09-24 context diet): ONE
/// line — position, chapter, and the played-span truth (capped at
/// [`HEADER_SPAN_LIST_MAX`] verbatim entries). No chapter-list triads, no
/// fetch instructions: those live on the recap tool's long-form
/// [`coverage_line`] and the chapter-map law block. The empty-accumulator
/// wordings are the field-tested honesty strings kept verbatim.
pub fn coverage_header(
    chapters: &[SessionChapter],
    timeline_end: u64,
    ranges: &PlayedRanges,
    pos: u64,
    units: PositionUnits,
) -> String {
    let narrated = units == PositionUnits::Millis;
    let mut line = format!(
        "Coverage: {} {} of {}",
        if narrated {
            "narration position"
        } else {
            "position"
        },
        position_label(units, pos),
        position_label(units, timeline_end),
    );
    if let Some(current) = chapters
        .iter()
        .find(|chapter| pos >= chapter.start_ms && pos < chapter.end_ms)
        .or_else(|| chapters.last().filter(|chapter| pos >= chapter.end_ms))
    {
        line.push_str(&format!(" (chapter {} of {})", current.idx, chapters.len()));
    }
    line.push_str(". ");
    if ranges.is_empty() {
        if narrated && pos > 0 {
            line.push_str("Played DURING THIS SESSION: none.");
        } else {
            line.push_str(
                "Nothing has been played in this session yet: no content may be \n                 described as covered.",
            );
        }
        return line;
    }
    let spans = span_list(ranges, units, HEADER_SPAN_LIST_MAX);
    line.push_str(&if narrated {
        format!("Played DURING THIS SESSION: {spans}.")
    } else {
        format!("Played ranges: {spans}.")
    });
    line
}

/// The one honest coverage line the window header carries (F8 §2.1 D1 piece
/// 3): current position, per-chapter played/partial/untouched split, and the
/// raw played ranges. Everything in it derives from the accumulator and the
/// chapter map — nothing is generated.
///
/// F9-D1: in grounded sessions (`Millis` — the narration timeline) the line
/// carries BOTH truths as internal bookkeeping: the narration position
/// (the playback surface's clock, which exists whether or not this voice
/// session saw any playback) AND the ranges played DURING THIS SESSION. The
/// two can honestly disagree — playback before this session is invisible to
/// the accumulator. F10-D1: the disagreement is tagged internal bookkeeping;
/// the line tells the model to fetch [0, pos), never to explain accounting. BookDirect (`Offset`) wording is
/// unchanged: its position IS the session's cursor, there is no second truth.
pub fn coverage_line(
    chapters: &[SessionChapter],
    timeline_end: u64,
    ranges: &PlayedRanges,
    pos: u64,
    units: PositionUnits,
) -> String {
    let narrated = units == PositionUnits::Millis;
    let mut line = format!(
        "Coverage this session — {} {} of {}",
        if narrated {
            "narration position"
        } else {
            "position"
        },
        position_label(units, pos),
        position_label(units, timeline_end),
    );
    if let Some(current) = chapters
        .iter()
        .find(|chapter| pos >= chapter.start_ms && pos < chapter.end_ms)
        .or_else(|| chapters.last().filter(|chapter| pos >= chapter.end_ms))
    {
        line.push_str(&format!(" (chapter {} of {})", current.idx, chapters.len()));
    }
    line.push_str(". ");
    if ranges.is_empty() {
        if narrated && pos > 0 {
            // F9-D1 / F10-D1: both truths as internal bookkeeping — the narration stands
            // somewhere, this session just saw none of it play.
            // F10-D1: bookkeeping, tagged as such — the model reads it, the
            // user never hears it; the hint says ACT, not explain.
            line.push_str(&format!(
                "Played DURING THIS SESSION: none (internal bookkeeping — do \
                 not tell the user). For \"covered so far\", call \
                 get_context_for_range for 0–{} (or omit its bounds) and \
                 answer from it.",
                position_label(units, pos),
            ));
        } else {
            line.push_str(
                "Nothing has been played in this session yet: no content may be \
                 described as covered.",
            );
        }
        return line;
    }
    let played = chapter_list(chapters, ChapterCoverage::Played, ranges);
    let partial = chapter_list(chapters, ChapterCoverage::Partial, ranges);
    let untouched = chapter_list(chapters, ChapterCoverage::Untouched, ranges);
    if !played.is_empty() {
        line.push_str(&format!("Fully played chapters: {played}. "));
    }
    if !partial.is_empty() {
        line.push_str(&format!("Partially played chapters: {partial}. "));
    }
    if !untouched.is_empty() {
        line.push_str(&format!("Not played this session: {untouched}. "));
    }
    let spans = span_list(ranges, units, LONG_FORM_SPAN_LIST_MAX);
    line.push_str(&if narrated {
        format!("Played DURING THIS SESSION: {spans}.")
    } else {
        format!("Played ranges: {spans}.")
    });
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review 2026-10-09, T3: coverage survives a round trip only in its
    /// own units, and malformed input never panics.
    #[test]
    fn coverage_round_trips_in_its_own_units_only() {
        let mut ranges = PlayedRanges::default();
        ranges.note(100, 200);
        ranges.note(5_000, 9_000);
        let json = ranges.to_json("offset");
        let back = PlayedRanges::from_json(&json, "offset").unwrap();
        assert_eq!(back.ranges(), ranges.ranges());
        assert!(PlayedRanges::from_json(&json, "ms").is_none());
        assert!(PlayedRanges::from_json("not json", "offset").is_none());
        assert!(
            PlayedRanges::from_json(r#"{"units":"offset","ranges":[[9,3]]}"#, "offset")
                .unwrap()
                .is_empty()
        );
    }

    fn chapter(idx: usize, title: &str, start_ms: u64, end_ms: u64) -> SessionChapter {
        SessionChapter {
            idx,
            title: title.to_string(),
            page_start: 1,
            page_end: 1,
            start_ms,
            end_ms,
        }
    }

    // P1: interval-union semantics — the watermark failure direction is
    // impossible by construction.

    #[test]
    fn overlapping_and_adjacent_intervals_merge_into_one() {
        let mut ranges = PlayedRanges::default();
        ranges.note(0, 1000);
        ranges.note(900, 2000);
        ranges.note(2000 + MERGE_GAP, 3000); // within the merge gap
        assert_eq!(ranges.ranges(), &[(0, 3000)]);
    }

    #[test]
    fn a_forward_seek_leaves_the_skipped_gap_uncovered() {
        let mut ranges = PlayedRanges::default();
        ranges.note(0, 10_000);
        // Seek from 10 s to 60 s: only post-landing playback records.
        ranges.note(60_000, 70_000);
        assert_eq!(ranges.ranges(), &[(0, 10_000), (60_000, 70_000)]);
        assert_eq!(
            ranges.covered_within(10_000, 60_000),
            0,
            "a watermark would claim this gap; the union must not"
        );
    }

    #[test]
    fn out_of_order_and_backward_notes_still_union_correctly() {
        let mut ranges = PlayedRanges::default();
        ranges.note(50_000, 60_000);
        ranges.note(0, 10_000);
        ranges.note(5_000, 55_000); // replay bridging both
        assert_eq!(ranges.ranges(), &[(0, 60_000)]);
    }

    #[test]
    fn empty_and_inverted_intervals_are_ignored() {
        let mut ranges = PlayedRanges::default();
        ranges.note(5, 5);
        ranges.note(10, 3);
        assert!(ranges.is_empty());
    }

    #[test]
    fn the_interval_cap_sheds_the_shortest_range_and_under_claims() {
        let mut ranges = PlayedRanges::default();
        // MAX_INTERVALS long disjoint ranges, then one short outlier.
        let stride = 10_000 + 2 * MERGE_GAP;
        for at in 0..MAX_INTERVALS as u64 {
            ranges.note(at * stride, at * stride + 10_000);
        }
        let outlier = MAX_INTERVALS as u64 * stride + 10 * MERGE_GAP;
        ranges.note(outlier, outlier + 100);
        assert_eq!(ranges.ranges().len(), MAX_INTERVALS);
        assert_eq!(
            ranges.covered_within(outlier, outlier + 100),
            0,
            "overflow shed the shortest interval — coverage under-claims, \
             never over-claims"
        );
    }

    #[test]
    fn per_chapter_status_derives_correctly_at_chapter_boundaries() {
        let chapters = [
            chapter(0, "Alpha", 0, 60_000),
            chapter(1, "Bravo", 60_000, 120_000),
            chapter(2, "Charlie", 120_000, 180_000),
        ];
        let mut ranges = PlayedRanges::default();
        // Alpha fully (up to boundary slack), Bravo half, Charlie untouched.
        ranges.note(0, 60_000 - CHAPTER_FULL_SLACK);
        ranges.note(60_000, 90_000);
        assert_eq!(
            chapter_status(&ranges, &chapters[0]),
            ChapterCoverage::Played
        );
        assert_eq!(
            chapter_status(&ranges, &chapters[1]),
            ChapterCoverage::Partial
        );
        assert_eq!(
            chapter_status(&ranges, &chapters[2]),
            ChapterCoverage::Untouched
        );
    }

    // F8 D1b: gap honesty for the bounded-range fetch.

    #[test]
    fn gaps_within_names_the_exact_uncovered_complement() {
        let mut ranges = PlayedRanges::default();
        ranges.note(10_000, 20_000);
        ranges.note(40_000, 50_000);
        assert_eq!(
            ranges.gaps_within(0, 60_000),
            vec![(0, 10_000), (20_000, 40_000), (50_000, 60_000)]
        );
        assert_eq!(ranges.gaps_within(12_000, 18_000), Vec::<(u64, u64)>::new());
        assert_eq!(ranges.gaps_within(15_000, 45_000), vec![(20_000, 40_000)]);
    }

    #[test]
    fn gaps_within_an_empty_accumulator_is_the_whole_request() {
        let ranges = PlayedRanges::default();
        assert_eq!(ranges.gaps_within(5, 50), vec![(5, 50)]);
        assert_eq!(ranges.gaps_within(50, 5), Vec::<(u64, u64)>::new());
    }

    #[test]
    fn the_outer_span_covers_first_to_last_and_is_none_when_empty() {
        let mut ranges = PlayedRanges::default();
        assert_eq!(ranges.outer_span(), None);
        ranges.note(10_000, 20_000);
        ranges.note(40_000, 50_000);
        assert_eq!(ranges.outer_span(), Some((10_000, 50_000)));
    }

    // The grounded sampling state machine.

    #[test]
    fn a_continuous_forward_step_records_the_traversed_interval() {
        assert_eq!(
            coverage_step(Some(1_000), 2_000, 5_000),
            Some((1_000, 2_000))
        );
    }

    #[test]
    fn a_pause_a_seek_and_a_first_sample_record_nothing() {
        assert_eq!(coverage_step(None, 2_000, 5_000), None, "first sample");
        assert_eq!(coverage_step(Some(2_000), 2_000, 5_000), None, "paused");
        assert_eq!(
            coverage_step(Some(2_000), 60_000, 5_000),
            None,
            "a forward seek is a discontinuity, not played content"
        );
        assert_eq!(
            coverage_step(Some(60_000), 2_000, 5_000),
            None,
            "a backward seek records nothing until playback advances"
        );
    }

    // P3: the coverage line renders played/current/untouched truthfully.

    #[test]
    fn the_coverage_line_renders_played_partial_and_untouched() {
        let chapters = [
            chapter(0, "Alpha", 0, 60_000),
            chapter(1, "Bravo", 60_000, 120_000),
            chapter(2, "Charlie", 120_000, 180_000),
        ];
        let mut ranges = PlayedRanges::default();
        ranges.note(0, 60_000);
        ranges.note(60_000, 90_000);
        let line = coverage_line(&chapters, 180_000, &ranges, 90_000, PositionUnits::Millis);
        assert!(line.contains("narration position 1:30 of 3:00"), "{line}");
        assert!(line.contains("(chapter 1 of 3)"), "{line}");
        assert!(line.contains("Fully played chapters: 0 (Alpha)"), "{line}");
        assert!(
            line.contains("Partially played chapters: 1 (Bravo)"),
            "{line}"
        );
        assert!(
            line.contains("Not played this session: 2 (Charlie)"),
            "{line}"
        );
        assert!(
            line.contains("Played DURING THIS SESSION: 0:00–1:30"),
            "{line}"
        );
    }

    // F9-D1 pin (F10-D1 wording): grounded coverage with an EMPTY session
    // accumulator carries BOTH truths — the live narration position and the
    // session-played "none" — tagged internal, with the [0, pos) fetch hint,
    // so the model acts instead of explaining accounting to the user.
    #[test]
    fn f9_an_empty_grounded_accumulator_states_both_truths_and_the_fetch_hint() {
        let chapters = [
            chapter(0, "Alpha", 0, 60_000),
            chapter(1, "Bravo", 60_000, 3_328_000),
        ];
        let line = coverage_line(
            &chapters,
            3_328_000,
            &PlayedRanges::default(),
            1_339_000, // the owner-device shape: narration at 22:19 of 55:28
            PositionUnits::Millis,
        );
        assert!(
            line.contains("narration position 22:19 of 55:28"),
            "the narration truth rides: {line}"
        );
        assert!(
            line.contains("Played DURING THIS SESSION: none"),
            "the session truth rides, self-labeled: {line}"
        );
        // F10-D1: the line is bookkeeping for the model, tagged as such —
        // the sentence the device relayed to the user ("this session only
        // tracks playback since it started; the user may have listened
        // before") is gone; the fetch hint names [0, pos) and says ACT.
        assert!(
            line.contains("internal bookkeeping"),
            "the session truth is tagged internal: {line}"
        );
        assert!(
            !line.contains("may have listened before")
                && !line.contains("only tracks playback since it started"),
            "no relayable accounting sentence: {line}"
        );
        assert!(
            line.contains("call get_context_for_range for 0–22:19"),
            "the covered-so-far fetch hint names [0, pos): {line}"
        );
    }

    // F9-D1 regression pin: BookDirect wording is unchanged — its position
    // IS the session cursor, so the empty branch keeps the honest one-truth
    // sentence even at a nonzero position.
    #[test]
    fn f9_book_direct_empty_wording_is_unchanged_even_at_a_nonzero_position() {
        let chapters = [chapter(0, "Alpha", 0, 500)];
        let line = coverage_line(
            &chapters,
            500,
            &PlayedRanges::default(),
            400,
            PositionUnits::Offset,
        );
        assert!(
            line.contains("Nothing has been played in this session yet"),
            "{line}"
        );
        assert!(!line.contains("narration"), "{line}");
        assert!(!line.contains("get_context_for_range"), "{line}");
    }

    #[test]
    fn an_empty_accumulator_renders_an_honest_nothing_played_line() {
        let chapters = [chapter(0, "Alpha", 0, 60_000)];
        let line = coverage_line(
            &chapters,
            60_000,
            &PlayedRanges::default(),
            0,
            PositionUnits::Millis,
        );
        assert!(
            line.contains("Nothing has been played in this session yet"),
            "{line}"
        );
        assert!(!line.contains("Fully played"), "{line}");
    }

    #[test]
    fn a_skipped_gap_is_never_claimed_in_the_line() {
        let chapters = [
            chapter(0, "Alpha", 0, 60_000),
            chapter(1, "Bravo", 60_000, 120_000),
        ];
        let mut ranges = PlayedRanges::default();
        ranges.note(0, 10_000);
        ranges.note(60_000, 120_000); // skipped ahead over Alpha's tail
        let line = coverage_line(&chapters, 120_000, &ranges, 120_000, PositionUnits::Millis);
        assert!(
            line.contains("Partially played chapters: 0 (Alpha)"),
            "{line}"
        );
        assert!(line.contains("Fully played chapters: 1 (Bravo)"), "{line}");
        assert!(
            line.contains("Played DURING THIS SESSION: 0:00–0:10, 1:00–2:00"),
            "the gap 0:10–1:00 must stay visible as unplayed: {line}"
        );
    }

    #[test]
    fn book_direct_positions_render_as_offsets() {
        let chapters = [chapter(0, "Alpha", 0, 500)];
        let mut ranges = PlayedRanges::default();
        ranges.note(0, 400);
        let line = coverage_line(&chapters, 500, &ranges, 400, PositionUnits::Offset);
        assert!(line.contains("position offset 400 of offset 500"), "{line}");
        assert!(line.contains("offset 0–offset 400"), "{line}");
    }

    #[test]
    fn hour_long_positions_render_with_hours() {
        assert_eq!(position_label(PositionUnits::Millis, 3_723_000), "1:02:03");
    }

    // ---- 2026-09-24 overhaul: bounded coverage surfaces (context diet) ----
    // Per-fetch window payloads carry the HEADER (one bounded line); the
    // long-form line stays the recap tool's payload and gets list caps so a
    // long session cannot stuff the server-side history with span lists.

    #[test]
    fn the_coverage_header_is_one_bounded_line_under_many_spans() {
        let chapters = [
            chapter(0, "Alpha", 0, 60_000),
            chapter(1, "Bravo", 60_000, 120_000),
        ];
        let mut ranges = PlayedRanges::default();
        for i in 0..12u64 {
            ranges.note(i * 10_000, i * 10_000 + 5_000);
        }
        let header = coverage_header(&chapters, 120_000, &ranges, 90_000, PositionUnits::Millis);
        assert!(header.contains("(chapter 1 of 2)"), "{header}");
        assert!(header.contains("Played DURING THIS SESSION: "), "{header}");
        assert!(header.contains("and 9 more"), "{header}");
        assert!(!header.contains("Fully played chapters"), "{header}");
    }

    #[test]
    fn the_coverage_header_keeps_the_no_playback_honesty() {
        let chapters = [chapter(0, "Alpha", 0, 60_000)];
        let grounded = coverage_header(
            &chapters,
            60_000,
            &PlayedRanges::default(),
            30_000,
            PositionUnits::Millis,
        );
        assert!(
            grounded.contains("Played DURING THIS SESSION: none"),
            "{grounded}"
        );
        let book_direct = coverage_header(
            &chapters,
            60_000,
            &PlayedRanges::default(),
            30_000,
            PositionUnits::Offset,
        );
        assert!(
            book_direct.contains("Nothing has been played in this session yet"),
            "{book_direct}"
        );
    }

    #[test]
    fn the_coverage_header_lists_few_spans_verbatim() {
        let chapters = [chapter(0, "Alpha", 0, 500)];
        let mut ranges = PlayedRanges::default();
        ranges.note(0, 200);
        ranges.note(500, 700);
        let header = coverage_header(&chapters, 1_000, &ranges, 700, PositionUnits::Offset);
        assert!(
            header.contains("Played ranges: offset 0–offset 200, offset 500–offset 700"),
            "{header}"
        );
        assert!(!header.contains("more"), "{header}");
    }

    #[test]
    fn the_long_form_line_caps_its_chapter_lists_and_span_list() {
        let chapters: Vec<SessionChapter> = (0..10)
            .map(|idx| {
                let title = format!("Chapter {idx}");
                chapter(idx, &title, idx as u64 * 60_000, (idx + 1) as u64 * 60_000)
            })
            .collect();
        let mut ranges = PlayedRanges::default();
        for i in 0..12u64 {
            ranges.note(i * 10_000, i * 10_000 + 5_000);
        }
        let line = coverage_line(&chapters, 600_000, &ranges, 90_000, PositionUnits::Millis);
        let untouched_at = line
            .find("Not played this session: ")
            .expect("untouched list rides");
        let untouched = &line[untouched_at
            ..line[untouched_at..]
                .find("Played DURING THIS SESSION")
                .map(|i| untouched_at + i)
                .unwrap_or(line.len())];
        assert!(
            untouched.contains("and 5 more"),
            "untouched list must cap: {line}"
        );
        assert!(
            untouched.matches("Chapter").count() <= 3,
            "at most three untouched titles render verbatim: {line}"
        );
        assert!(line.contains("and 4 more"), "span list must cap: {line}");
    }
}
