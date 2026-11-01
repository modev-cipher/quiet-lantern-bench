//! The session's view of the book: an immutable snapshot of playable segments
//! (VOICE_AGENT §4 — "same granularity the pipeline produces").
//!
//! Built once at session start from the reading structures and never mutated:
//! within one session the render is fixed, so a segment's **position** (`idx`)
//! is a complete, stable identity. The batch `segment_id` string is
//! deliberately absent here — it has zero cross-render stability by
//! construction (the scripting stage mints fresh ids every render), so nothing
//! session-scoped may key on it or the key rots the moment the book is
//! re-rendered.
//!
//! Segment text is private to the excerpt module: other voice code can hold and
//! pass a snapshot but cannot read book text out of it. `ExcerptSelector` is
//! the only exit.

use crate::reading::ReadingChapter;

pub struct SessionSegment {
    /// Dense position in the session snapshot (0-based, gap-free), assigned at
    /// snapshot build time. The window and summary machinery key on this.
    pub idx: usize,
    pub page_no: i64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub(in crate::voice::excerpt) text: String,
}

/// One chapter's position on the session timeline — navigation and outline
/// data only, no body text and no cross-render identity (the render-local
/// chapter id is as unstable as `segment_id` and is deliberately absent). The
/// voice tool set (VOICE_AGENT §5 `jump_to_chapter`) existence-checks and
/// resolves jump targets against this list; the F8 D1 chapter map renders the
/// title and page span into the session context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionChapter {
    /// Dense position in the session's chapter list (0-based), assigned at
    /// snapshot build time — the `jump_to_chapter` index vocabulary.
    pub idx: usize,
    /// The book's chapter title (F8 D1) — display/outline data, not identity.
    /// Falls back to the chapter's render-local id when the book rows carry
    /// no title for it (the `reading::index` fallback, same rule).
    pub title: String,
    /// Page span actually carried by this chapter's segments (first/last
    /// segment page, honest to what the session can reach; 0–0 on a
    /// segment-less chapter).
    pub page_start: i64,
    pub page_end: i64,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// One parsed visual element of the book (figure, chart, diagram, table,
/// rendered formula — whatever ingestion labelled), as TEXT: the caption and
/// the ingestion-time description. F7 tier 0 (owner ruling 2026-08-30):
/// these labels ride the excerpt window through THIS seam — text egress
/// through the one enforcement point, no new egress category.
pub struct SessionFigure {
    pub id: String,
    pub page_no: i64,
    /// Ingestion's element class ("figure", "table", "formula", …).
    pub kind: String,
    /// Caption + description, joined at construction; may be empty when
    /// ingestion labelled the region but produced no text (skipped then).
    pub note: String,
}

pub struct SessionSnapshot {
    segments: Vec<SessionSegment>,
    chapters: Vec<SessionChapter>,
    figures: Vec<SessionFigure>,
}

impl SessionSnapshot {
    /// Flatten the reading chapters (already in playback order, segments
    /// carrying timeline positions) into the session's segment list.
    /// `figures` carries every parsed visual element's text labels (F7
    /// tier 0) — callers assemble them from `storage::book::FigureRegion`
    /// rows; empty is a valid state (unanalysed book).
    /// `title_of` resolves a chapter's render-local id to the book's chapter
    /// title (F8 D1) — `None` falls back to the id itself, the
    /// `reading::index` rule.
    pub fn from_reading(
        chapters: &[ReadingChapter],
        figures: Vec<SessionFigure>,
        title_of: impl Fn(&str) -> Option<String>,
    ) -> Self {
        Self::from_reading_with_pages(chapters, figures, title_of, |_| None)
    }

    /// [`Self::from_reading`] plus the book's own chapter page spans
    /// (F10-D2): `pages_of` resolves a chapter's render-local id to the
    /// `(page_start, page_end)` its chapter row declares. A chapter that
    /// carries segments keeps the segment-derived span (honest to what the
    /// session can read); a chapter with NO segments — its pages have no
    /// extracted text — falls back to the row's span, so a jump onto it can
    /// still turn the reader to a real page instead of resolving to the
    /// previous segment's page or to nothing.
    pub fn from_reading_with_pages(
        chapters: &[ReadingChapter],
        figures: Vec<SessionFigure>,
        title_of: impl Fn(&str) -> Option<String>,
        pages_of: impl Fn(&str) -> Option<(i64, i64)>,
    ) -> Self {
        let mut segments = Vec::new();
        let mut chapter_spans = Vec::new();
        for chapter in chapters {
            chapter_spans.push(SessionChapter {
                idx: chapter_spans.len(),
                title: title_of(&chapter.chapter_id).unwrap_or_else(|| chapter.chapter_id.clone()),
                page_start: chapter
                    .segments
                    .first()
                    .map(|s| s.page_no)
                    .or_else(|| pages_of(&chapter.chapter_id).map(|span| span.0))
                    .unwrap_or(0),
                page_end: chapter
                    .segments
                    .last()
                    .map(|s| s.page_no)
                    .or_else(|| pages_of(&chapter.chapter_id).map(|span| span.1))
                    .unwrap_or(0),
                start_ms: chapter.start_ms.max(0) as u64,
                end_ms: chapter.end_ms.max(0) as u64,
            });
            for segment in &chapter.segments {
                let idx = segments.len();
                segments.push(SessionSegment {
                    idx,
                    page_no: segment.page_no,
                    start_ms: segment.start_ms.max(0) as u64,
                    end_ms: segment.end_ms.max(0) as u64,
                    text: segment.text.clone(),
                });
            }
        }
        Self {
            segments,
            chapters: chapter_spans,
            figures,
        }
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Word count of one segment, for span validation at the Agent tool
    /// boundary (charter §4.2 "validated word span") — a count only; the
    /// text itself never leaves the excerpt tree this way.
    #[cfg(feature = "book-agent")]
    pub fn segment_word_count(&self, idx: usize) -> Option<usize> {
        self.segments
            .get(idx)
            .map(|s| s.text.split_whitespace().count())
    }

    pub fn segments(&self) -> &[SessionSegment] {
        &self.segments
    }

    /// Chapter spans on the session timeline, in playback order (navigation
    /// data only — see [`SessionChapter`]).
    pub fn chapters(&self) -> &[SessionChapter] {
        &self.chapters
    }

    /// Visual-element labels for one page, in insertion order.
    pub fn figures_on_page(&self, page_no: i64) -> impl Iterator<Item = &SessionFigure> {
        self.figures
            .iter()
            .filter(move |figure| figure.page_no == page_no && !figure.note.trim().is_empty())
    }

    pub fn has_anchor(&self, kind: &crate::voice::channel::event::AnchorKind, id: &str) -> bool {
        match kind {
            crate::voice::channel::event::AnchorKind::Section => false,
            crate::voice::channel::event::AnchorKind::Figure
            | crate::voice::channel::event::AnchorKind::Equation
            | crate::voice::channel::event::AnchorKind::Table => {
                self.figures.iter().any(|f| f.id == id)
            }
        }
    }

    pub fn anchor_page(
        &self,
        kind: &crate::voice::channel::event::AnchorKind,
        id: &str,
    ) -> Option<i64> {
        if !self.has_anchor(kind, id) {
            return None;
        }
        self.figures
            .iter()
            .find(|figure| figure.id == id)
            .map(|figure| figure.page_no)
    }

    /// Test rigs build snapshots directly; production snapshots come only from
    /// [`SessionSnapshot::from_reading`]. Scoped to the excerpt module so no
    /// other code grows a book-text ingestion path through here. The chapter
    /// list is a single span covering the whole timeline — range/window tests
    /// don't navigate chapters; rigs that do use
    /// [`SessionSnapshot::from_voice_test_parts`].
    #[cfg(test)]
    pub(in crate::voice::excerpt) fn from_test_parts(parts: Vec<(i64, u64, u64, &str)>) -> Self {
        let end_ms = parts.iter().map(|part| part.2).max().unwrap_or(0);
        Self::from_voice_test_parts(parts, vec![(0, end_ms)])
    }

    /// Test-only snapshot builder for sibling voice modules (the tool
    /// dispatcher's navigation rigs, VOICE_AGENT §5). `chapters` are
    /// `(start_ms, end_ms)` spans in playback order. cfg(test)-gated and
    /// crate-private: it opens no production ingestion path — live snapshots
    /// still come only from [`SessionSnapshot::from_reading`], and book text
    /// still exits only through `ExcerptSelector`.
    #[cfg(test)]
    pub(crate) fn from_voice_test_parts(
        parts: Vec<(i64, u64, u64, &str)>,
        chapters: Vec<(u64, u64)>,
    ) -> Self {
        let segments = parts
            .into_iter()
            .enumerate()
            .map(|(idx, (page_no, start_ms, end_ms, text))| SessionSegment {
                idx,
                page_no,
                start_ms,
                end_ms,
                text: text.to_string(),
            })
            .collect();
        let chapters = chapters
            .into_iter()
            .enumerate()
            .map(|(idx, (start_ms, end_ms))| SessionChapter {
                idx,
                title: format!("chapter {idx}"),
                page_start: 0,
                page_end: 0,
                start_ms,
                end_ms,
            })
            .collect();
        Self {
            segments,
            chapters,
            figures: Vec::new(),
        }
    }

    /// Test-only: attach visual-element labels — `(page_no, kind, note)`.
    #[cfg(test)]
    pub(in crate::voice::excerpt) fn with_test_figures(
        mut self,
        figures: Vec<(i64, &str, &str)>,
    ) -> Self {
        self.figures = figures
            .into_iter()
            .enumerate()
            .map(|(idx, (page_no, kind, note))| SessionFigure {
                id: format!("fig-{idx}"),
                page_no,
                kind: kind.to_string(),
                note: note.to_string(),
            })
            .collect();
        self
    }

    /// Test-only: attach visual-element labels with explicit IDs — `(id, page_no, kind, note)`.
    #[cfg(test)]
    pub(crate) fn with_test_figures_with_id(
        mut self,
        figures: Vec<(&str, i64, &str, &str)>,
    ) -> Self {
        self.figures = figures
            .into_iter()
            .map(|(id, page_no, kind, note)| SessionFigure {
                id: id.to_string(),
                page_no,
                kind: kind.to_string(),
                note: note.to_string(),
            })
            .collect();
        self
    }

    /// Test-only: give the chapters the book's own page span — `(page_start,
    /// page_end)` per chapter in order — the `from_reading` book-row fallback
    /// a segment-less chapter navigates by (F10-D2).
    #[cfg(test)]
    pub(crate) fn with_test_chapter_pages(mut self, pages: Vec<(i64, i64)>) -> Self {
        for (chapter, (page_start, page_end)) in self.chapters.iter_mut().zip(pages) {
            chapter.page_start = page_start;
            chapter.page_end = page_end;
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_chapter_titles(mut self, titles: Vec<String>) -> Self {
        for (chapter, title) in self.chapters.iter_mut().zip(titles) {
            chapter.title = title;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reading::{GeometryOutcome, ReadingSegment, TimelineOrigin};
    use crate::words::WordProvenance;

    fn reading_segment(idx: i64, page_no: i64, start_ms: i64, end_ms: i64) -> ReadingSegment {
        ReadingSegment {
            segment_id: format!("render-local-{idx}"),
            idx,
            page_no,
            kind: "body".into(),
            figure_id: None,
            text: format!("segment {idx} text"),
            start_ms,
            end_ms,
            provenance: WordProvenance::Missing,
            words: Vec::new(),
            anchor: crate::reading::anchor::Rung::Page,
            rects: Vec::new(),
        }
    }

    fn chapter(chapter_id: &str, segments: Vec<ReadingSegment>) -> ReadingChapter {
        let start_ms = segments.first().map_or(0, |segment| segment.start_ms);
        let end_ms = segments.last().map_or(0, |segment| segment.end_ms);
        ReadingChapter {
            anchor_geometry: GeometryOutcome::Unavailable,
            chapter_id: chapter_id.into(),
            start_ms,
            end_ms,
            origin: TimelineOrigin::Measured,
            segments,
            mode: None,
        }
    }

    #[test]
    fn chapters_flatten_in_order_with_dense_reindexing() {
        let chapters = vec![
            chapter(
                "ch-1",
                vec![
                    reading_segment(0, 1, 0, 900),
                    reading_segment(1, 1, 900, 2000),
                ],
            ),
            chapter("ch-2", vec![reading_segment(0, 2, 2600, 4000)]),
        ];
        let snapshot = SessionSnapshot::from_reading(&chapters, Vec::new(), |_| None);
        assert_eq!(snapshot.len(), 3);
        let indices: Vec<usize> = snapshot.segments().iter().map(|s| s.idx).collect();
        assert_eq!(
            indices,
            vec![0, 1, 2],
            "per-chapter idx restarts must not survive into the session identity"
        );
        assert_eq!(snapshot.segments()[2].page_no, 2);
        assert_eq!(snapshot.segments()[2].start_ms, 2600);
    }

    #[test]
    fn chapter_spans_ride_the_snapshot_in_playback_order() {
        let chapters = vec![
            chapter(
                "ch-1",
                vec![
                    reading_segment(0, 1, 0, 900),
                    reading_segment(1, 1, 900, 2000),
                ],
            ),
            chapter("ch-2", vec![reading_segment(0, 2, 2600, 4000)]),
        ];
        let snapshot = SessionSnapshot::from_reading(&chapters, Vec::new(), |_| None);
        let spans: Vec<(usize, u64, u64)> = snapshot
            .chapters()
            .iter()
            .map(|c| (c.idx, c.start_ms, c.end_ms))
            .collect();
        assert_eq!(spans, vec![(0, 0, 2000), (1, 2600, 4000)]);
    }

    #[test]
    fn negative_chapter_timeline_values_clamp_to_zero_rather_than_wrap() {
        let mut broken = chapter("ch-1", vec![reading_segment(0, 1, -5, -1)]);
        broken.start_ms = -5;
        broken.end_ms = -1;
        let snapshot = SessionSnapshot::from_reading(&[broken], Vec::new(), |_| None);
        assert_eq!(snapshot.chapters()[0].start_ms, 0);
        assert_eq!(snapshot.chapters()[0].end_ms, 0);
    }

    #[test]
    fn negative_timeline_values_clamp_to_zero_rather_than_wrap() {
        let chapters = vec![chapter("ch-1", vec![reading_segment(0, 1, -5, -1)])];
        let snapshot = SessionSnapshot::from_reading(&chapters, Vec::new(), |_| None);
        assert_eq!(snapshot.segments()[0].start_ms, 0);
        assert_eq!(snapshot.segments()[0].end_ms, 0);
    }

    #[test]
    fn the_snapshot_does_not_carry_the_render_local_segment_id() {
        // Compile-time-ish pin of the no-cross-render-keys decision: the field
        // list is the contract, and segment_id is not in it.
        let chapters = vec![chapter("ch-1", vec![reading_segment(0, 1, 0, 100)])];
        let snapshot = SessionSnapshot::from_reading(&chapters, Vec::new(), |_| None);
        let SessionSegment {
            idx: _,
            page_no: _,
            start_ms: _,
            end_ms: _,
            text: _,
        } = &snapshot.segments()[0];
    }
}
