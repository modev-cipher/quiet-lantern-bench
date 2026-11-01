use serde::Serialize;

use crate::orchestrator::verify::normalize;

pub const ANCHOR_LOG_PREFIX: &str = "anchor rungs";
pub const GEOMETRY_ENSURE_LOG_PREFIX: &str = "anchor geometry";

pub fn format_geometry_ensure(book_id: &str, pages: usize, ensured: bool, ms: u128) -> String {
    let short: String = book_id.chars().take(8).collect();
    let outcome = if ensured {
        "geometry-ensured"
    } else {
        "geometry-unavailable"
    };
    format!("{GEOMETRY_ENSURE_LOG_PREFIX}: book={short} pages={pages} outcome={outcome} ms={ms}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rung {
    Word,
    Line,
    Block,
    Page,
}

impl Rung {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Line => "line",
            Self::Block => "block",
            Self::Page => "page",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn from_slice(raw: &[f64]) -> Option<Self> {
        match raw {
            [x0, y0, x1, y1, ..] => Some(Self {
                x0: *x0,
                y0: *y0,
                x1: *x1,
                y1: *y1,
            }),
            _ => None,
        }
    }

    fn union(&self, other: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    fn contains_center_of(&self, other: &Rect) -> bool {
        let cx = (other.x0 + other.x1) / 2.0;
        let cy = (other.y0 + other.y1) / 2.0;
        cx >= self.x0 && cx <= self.x1 && cy >= self.y0 && cy <= self.y1
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeometryWord {
    pub rect: Rect,
    pub token: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageGeometry {
    pub page_width_pt: f64,
    pub page_height_pt: f64,
    pub lines: Vec<Rect>,
    pub words: Vec<GeometryWord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockRef {
    pub reading_order: i64,
    pub bbox: Rect,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentAnchor {
    pub rung: Rung,
    pub rects: Vec<Rect>,
    pub word_rects: Vec<Option<Rect>>,
    pub located: usize,
    pub tokens: usize,
    pub rate_tokens: usize,
    pub demoted: usize,
    pub unplaced: usize,
}

pub struct SegmentInput<'a> {
    pub spoken: &'a [String],
    pub block: Option<&'a BlockRef>,
    pub kind: &'a str,
    pub counts_toward_rate: bool,
    pub reference: Option<&'a [String]>,
}

pub fn parse_geometry(geometry_json: &str) -> Option<PageGeometry> {
    let value: serde_json::Value = serde_json::from_str(geometry_json).ok()?;
    let lines = value["lines"]
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let raw: Vec<f64> = entry
                .as_array()?
                .iter()
                .filter_map(|v| v.as_f64())
                .collect();
            Rect::from_slice(&raw)
        })
        .collect();
    let words = value["words"]
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let row = entry.as_array()?;
            let raw: Vec<f64> = row.iter().take(4).filter_map(|v| v.as_f64()).collect();
            Some(GeometryWord {
                rect: Rect::from_slice(&raw)?,
                token: row.get(4)?.as_str()?.to_string(),
                line: row.get(5)?.as_u64()? as usize,
            })
        })
        .collect();
    Some(PageGeometry {
        page_width_pt: value["pageWidthPt"].as_f64().unwrap_or(0.0),
        page_height_pt: value["pageHeightPt"].as_f64().unwrap_or(0.0),
        lines,
        words,
    })
}

pub fn block_index_of(source_span_json: Option<&str>) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_str(source_span_json?).ok()?;
    value["block_index"].as_i64()
}

fn collapsed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// Content matching as a **verifier of an authored offset, never as a locator**.
///
/// Rows written before the `block_index` writer landed still carry `char_start`/`char_end`
/// (`script/segment.py` writes them unconditionally), and they index the block's
/// whitespace-collapsed text, tiled monotonically and disjointly across that block's segments.
/// So the block is recoverable: slice each candidate block at the offsets the engine recorded
/// and ask whether the slice IS this segment's text. A unique answer is the block; zero or
/// several means no block, and the segment keeps its previous behaviour rather than being
/// assigned a guess. A Teacher paraphrase or a synthesized code cue equals no printed slice
/// and falls through here, which is the correct outcome and not a gap.
pub fn recover_block_index(
    blocks: &[BlockRef],
    source_span_json: Option<&str>,
    segment_text: &str,
) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_str(source_span_json?).ok()?;
    let start = usize::try_from(value["char_start"].as_i64()?).ok()?;
    let end = usize::try_from(value["char_end"].as_i64()?).ok()?;
    if end <= start {
        return None;
    }
    let wanted = normalize(segment_text);
    if wanted.is_empty() {
        return None;
    }
    let mut found: Option<i64> = None;
    for block in blocks {
        let text = collapsed(&block.text);
        if text.chars().count() < end {
            continue;
        }
        let slice: String = text.chars().skip(start).take(end - start).collect();
        if normalize(&slice) != wanted {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(block.reading_order);
    }
    found
}

/// The slots a segment may match, and the block that scoped them.
///
/// A `None` scope means **no block**, and it is returned both when the caller had no
/// `block_index` and when the block's bbox covers no printed word. Widening an empty scope to
/// the whole page while still calling it a block is how a page-wide cursor arrives by accident:
/// the scope decision and the cursor-sharing decision must be made by the same condition, or
/// they disagree exactly on the pages that are hardest.
fn candidate_span(geometry: &PageGeometry, block: Option<&BlockRef>) -> (Vec<usize>, Option<i64>) {
    let whole = || (0..geometry.words.len()).collect::<Vec<usize>>();
    let Some(block) = block else {
        return (whole(), None);
    };
    let scoped: Vec<usize> = geometry
        .words
        .iter()
        .enumerate()
        .filter(|(_, word)| block.bbox.contains_center_of(&word.rect))
        .map(|(slot, _)| slot)
        .collect();
    if scoped.is_empty() {
        return (whole(), None);
    }
    (scoped, Some(block.reading_order))
}

fn expand_symbol_aliases(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed == "-" || trimmed == "−" || trimmed == "–" {
        return "minus".to_string();
    }
    let mut out = String::with_capacity(raw.len() * 2);
    for ch in raw.chars() {
        match ch {
            '=' => out.push_str(" equals "),
            '+' => out.push_str(" plus "),
            '%' => out.push_str(" percent "),
            '&' => out.push_str(" and "),
            '*' | '×' => out.push_str(" times "),
            '÷' => out.push_str(" divided "),
            '<' => out.push_str(" less "),
            '>' => out.push_str(" greater "),
            '≤' => out.push_str(" less "),
            '≥' => out.push_str(" greater "),
            '≠' => out.push_str(" not "),
            '±' => out.push_str(" plus minus "),
            _ => out.push(ch),
        }
    }
    out
}

pub fn normalize_token(raw: &str) -> Vec<String> {
    normalize(&expand_symbol_aliases(raw))
}

fn haystack_of(geometry: &PageGeometry, span: &[usize]) -> Vec<(String, usize)> {
    span.iter()
        .flat_map(|slot| {
            normalize_token(&geometry.words[*slot].token)
                .into_iter()
                .map(move |token| (token, *slot))
        })
        .collect()
}

/// Resolve every segment printed on one page, sharing one cursor **per block run**.
///
/// Occurrence identity lives at block scope, so that is where it is resolved. Within a block
/// the printed slot order and the reading order agree — a block is one contiguous region and
/// `page_geometry` sorts words row-major — which is why sharing a cursor across the segments of
/// a block is safe. Across blocks they do **not** agree: `extract_blocks` orders blocks
/// column-major on a multi-column page while the word slots stay row-major, so a cursor carried
/// past a block boundary reads column 1 as already consumed. Measured on a two-column fixture:
/// page-wide sharing scores 46.7 % of words on the wrong occurrence where per-block scoring is
/// 0 %. The cursor therefore resets at every block boundary, and a segment with no block keeps
/// the per-segment reset it has always had.
pub fn anchor_page(inputs: &[SegmentInput<'_>], geometry: &PageGeometry) -> Vec<SegmentAnchor> {
    let mut out = Vec::with_capacity(inputs.len());
    let mut scope: Option<i64> = None;
    let mut primed = false;
    let mut haystack: Vec<(String, usize)> = Vec::new();
    let mut cursor = 0usize;
    for input in inputs {
        if input.kind == "figure_cue" {
            out.push(cue_anchor(input));
            continue;
        }
        let (span, next) = candidate_span(geometry, input.block);
        let rebuilt = !primed || next != scope;
        if rebuilt {
            haystack = haystack_of(geometry, &span);
            scope = next;
            primed = true;
        }
        if rebuilt || next.is_none() {
            cursor = 0;
        }
        out.push(resolve_one(input, geometry, &haystack, &mut cursor));
    }
    out
}

pub fn anchor_segment(
    spoken: &[String],
    geometry: &PageGeometry,
    block: Option<&BlockRef>,
    kind: &str,
    counts_toward_rate: bool,
    reference: Option<&[String]>,
) -> SegmentAnchor {
    let inputs = [SegmentInput {
        spoken,
        block,
        kind,
        counts_toward_rate,
        reference,
    }];
    anchor_page(&inputs, geometry)
        .pop()
        .expect("one input yields one anchor")
}

fn rate_tokens_of(input: &SegmentInput<'_>, total: usize) -> usize {
    if input.counts_toward_rate && input.kind != "figure_cue" {
        total
    } else {
        0
    }
}

fn cue_anchor(input: &SegmentInput<'_>) -> SegmentAnchor {
    let total = input
        .spoken
        .iter()
        .flat_map(|raw| normalize_token(raw))
        .count();
    SegmentAnchor {
        rung: Rung::Page,
        rects: Vec::new(),
        word_rects: vec![None; input.spoken.len()],
        located: 0,
        tokens: total,
        rate_tokens: rate_tokens_of(input, total),
        demoted: 0,
        unplaced: 0,
    }
}

fn resolve_one(
    input: &SegmentInput<'_>,
    geometry: &PageGeometry,
    haystack: &[(String, usize)],
    cursor: &mut usize,
) -> SegmentAnchor {
    let spoken = input.spoken;
    let block = input.block;
    let reference = input.reference;
    let total = spoken.iter().flat_map(|raw| normalize_token(raw)).count();
    let rate_tokens = rate_tokens_of(input, total);
    let guard = reference.map(|raw| {
        raw.iter()
            .flat_map(|token| normalize_token(token))
            .collect::<Vec<String>>()
    });

    let mut word_rects: Vec<Option<Rect>> = Vec::with_capacity(spoken.len());
    let mut located = 0usize;
    let mut demoted = 0usize;
    let mut unplaced = 0usize;
    let mut lines_touched: Vec<usize> = Vec::new();
    let mut guard_cursor = 0usize;

    for raw in spoken {
        let parts = normalize_token(raw);
        if parts.is_empty() {
            word_rects.push(None);
            continue;
        }
        let mut entry_rect: Option<Rect> = None;
        let mut entry_demoted = false;
        for needle in &parts {
            let hit = (*cursor..haystack.len()).find(|index| haystack[*index].0 == *needle);
            let Some(index) = hit else {
                unplaced += 1;
                continue;
            };
            let slot = haystack[index].1;
            if let Some(reference) = guard.as_ref() {
                let ahead = reference[guard_cursor.min(reference.len())..]
                    .iter()
                    .position(|token| token == needle);
                match ahead {
                    Some(offset) => guard_cursor = guard_cursor + offset + 1,
                    None => {
                        demoted += 1;
                        entry_demoted = true;
                        let line = geometry.words[slot].line;
                        if !lines_touched.contains(&line) {
                            lines_touched.push(line);
                        }
                        continue;
                    }
                }
            }
            *cursor = index + 1;
            located += 1;
            let word = &geometry.words[slot];
            if !lines_touched.contains(&word.line) {
                lines_touched.push(word.line);
            }
            entry_rect = Some(match entry_rect {
                Some(current) => current.union(&word.rect),
                None => word.rect,
            });
        }
        word_rects.push(if entry_demoted { None } else { entry_rect });
    }

    if located > 0 {
        return SegmentAnchor {
            rung: Rung::Word,
            rects: Vec::new(),
            word_rects,
            located,
            tokens: total,
            rate_tokens,
            demoted,
            unplaced,
        };
    }
    if !lines_touched.is_empty() {
        let rects = lines_touched
            .iter()
            .filter_map(|line| geometry.lines.get(*line).copied())
            .collect();
        return SegmentAnchor {
            rung: Rung::Line,
            rects,
            word_rects,
            located,
            tokens: total,
            rate_tokens,
            demoted,
            unplaced,
        };
    }
    if let Some(block) = block {
        return SegmentAnchor {
            rung: Rung::Block,
            rects: vec![block.bbox],
            word_rects,
            located,
            tokens: total,
            rate_tokens,
            demoted,
            unplaced,
        };
    }
    SegmentAnchor {
        rung: Rung::Page,
        rects: Vec::new(),
        word_rects,
        located,
        tokens: total,
        rate_tokens,
        demoted,
        unplaced,
    }
}

pub fn parse_blocks(text_json: &str) -> Vec<BlockRef> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text_json) else {
        return Vec::new();
    };
    let Some(blocks) = value["blocks"].as_array() else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| {
            let raw: Vec<f64> = block["bbox"]
                .as_array()?
                .iter()
                .filter_map(|v| v.as_f64())
                .collect();
            Some(BlockRef {
                reading_order: block["reading_order"].as_i64()?,
                bbox: Rect::from_slice(&raw)?,
                text: block["text"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RungTally {
    pub word: usize,
    pub line: usize,
    pub block: usize,
    pub page: usize,
    pub segments: usize,
    pub tokens: usize,
    pub rate_tokens: usize,
    pub located: usize,
    pub demoted: usize,
    pub unplaced: usize,
}

impl RungTally {
    pub fn record(&mut self, anchor: &SegmentAnchor) {
        match anchor.rung {
            Rung::Word => self.word += 1,
            Rung::Line => self.line += 1,
            Rung::Block => self.block += 1,
            Rung::Page => self.page += 1,
        }
        self.segments += 1;
        self.tokens += anchor.tokens;
        self.rate_tokens += anchor.rate_tokens;
        self.located += anchor.located;
        self.demoted += anchor.demoted;
        self.unplaced += anchor.unplaced;
    }

    pub fn rate(&self) -> f64 {
        if self.rate_tokens == 0 {
            return 0.0;
        }
        self.located as f64 / self.rate_tokens as f64
    }
}

pub fn format_anchor_rungs(chapter_id: &str, tally: &RungTally) -> String {
    let short: String = chapter_id.chars().take(8).collect();
    format!(
        "{ANCHOR_LOG_PREFIX}: chapter={short} word={} line={} block={} page={} segments={} \
         tokens={} rateTokens={} located={} demoted={} unplaced={} rate={:.4}",
        tally.word,
        tally.line,
        tally.block,
        tally.page,
        tally.segments,
        tally.tokens,
        tally.rate_tokens,
        tally.located,
        tally.demoted,
        tally.unplaced,
        tally.rate()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(x0: f64, token: &str, line: usize) -> GeometryWord {
        GeometryWord {
            rect: Rect {
                x0,
                y0: 100.0,
                x1: x0 + 20.0,
                y1: 112.0,
            },
            token: token.to_string(),
            line,
        }
    }

    fn page(tokens: &[(&str, usize)]) -> PageGeometry {
        let mut lines = Vec::new();
        let words: Vec<GeometryWord> = tokens
            .iter()
            .enumerate()
            .map(|(slot, (token, line))| {
                while lines.len() <= *line {
                    let idx = lines.len() as f64;
                    lines.push(Rect {
                        x0: 50.0,
                        y0: 100.0 + idx * 20.0,
                        x1: 550.0,
                        y1: 112.0 + idx * 20.0,
                    });
                }
                word(50.0 + slot as f64 * 25.0, token, *line)
            })
            .collect();
        PageGeometry {
            page_width_pt: 612.0,
            page_height_pt: 792.0,
            lines,
            words,
        }
    }

    fn spoken(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn a_located_token_gets_the_rect_of_the_word_it_matched() {
        let geometry = page(&[("Raw", 0), ("CPU", 0), ("power", 0)]);
        let anchor = anchor_segment(
            &spoken(&["CPU", "power"]),
            &geometry,
            None,
            "text",
            true,
            None,
        );

        assert_eq!(anchor.rung, Rung::Word);
        assert_eq!(anchor.located, 2);
        assert_eq!(anchor.word_rects[0], Some(geometry.words[1].rect));
        assert_eq!(anchor.word_rects[1], Some(geometry.words[2].rect));
    }

    #[test]
    fn a_figure_cue_never_word_anchors_even_when_its_words_are_on_the_page() {
        let geometry = page(&[("Figure", 0), ("one", 0), ("shows", 0)]);
        let anchor = anchor_segment(
            &spoken(&["Figure", "one", "shows"]),
            &geometry,
            None,
            "figure_cue",
            true,
            None,
        );

        assert_eq!(
            anchor.rung,
            Rung::Page,
            "S1-3: the figure frame is the surface for a cue, not a word rect"
        );
        assert_eq!(anchor.located, 0);
        assert!(anchor.word_rects.iter().all(Option::is_none));
    }

    #[test]
    fn the_block_scopes_the_search_to_the_right_occurrence_of_a_repeated_word() {
        let geometry = page(&[("system", 0), ("alpha", 0), ("system", 1), ("beta", 1)]);
        let block = BlockRef {
            reading_order: 4,
            bbox: Rect {
                x0: 90.0,
                y0: 95.0,
                x1: 560.0,
                y1: 118.0,
            },
            text: String::new(),
        };
        let anchor = anchor_segment(
            &spoken(&["system"]),
            &geometry,
            Some(&block),
            "text",
            true,
            None,
        );

        assert_eq!(
            anchor.word_rects[0],
            Some(geometry.words[2].rect),
            "S1-2 asserts POSITION, never rate: the unscoped search would have taken the first \
             'system' on the page, which is the wrong occurrence"
        );
    }

    fn owners_block() -> BlockRef {
        BlockRef {
            reading_order: 3,
            bbox: Rect {
                x0: 40.0,
                y0: 95.0,
                x1: 560.0,
                y1: 118.0,
            },
            text: String::new(),
        }
    }

    fn text_input<'a>(
        spoken: &'a [String],
        block: Option<&'a BlockRef>,
        reference: &'a [String],
    ) -> SegmentInput<'a> {
        SegmentInput {
            spoken,
            block,
            kind: "text",
            counts_toward_rate: true,
            reference: Some(reference),
        }
    }

    fn owners_page() -> PageGeometry {
        page(&[
            ("the", 0),
            ("memory", 0),
            ("model", 0),
            ("and", 0),
            ("atomic", 0),
            ("operations", 0),
            ("the", 1),
            ("memory", 1),
            ("model", 1),
            ("ensures", 1),
        ])
    }

    /// The owner's report: two sentences of one paragraph, the second repeating "memory".
    ///
    /// Before the fix this asserted slot 1 and reddened — the first line's "memory", which is
    /// the word he photographed while narration was on the second line. Kept as the unit that
    /// names WHICH occurrence, because "a highlight exists" and "the highlighted text is
    /// 'memory'" both pass with the defect present.
    #[test]
    fn a_repeated_word_anchors_to_the_occurrence_the_speaking_segment_is_reading() {
        let geometry = owners_page();
        let block = owners_block();
        let first_spoken = spoken(&["The", "memory", "model", "and", "atomic", "operations"]);
        let first_reference = spoken(&["The memory model and atomic operations."]);
        let second_spoken = spoken(&["The", "memory", "model", "ensures"]);
        let second_reference = spoken(&["The memory model ensures that as"]);
        let resolved = anchor_page(
            &[
                text_input(&first_spoken, Some(&block), &first_reference),
                text_input(&second_spoken, Some(&block), &second_reference),
            ],
            &geometry,
        );
        let (first, second) = (&resolved[0], &resolved[1]);

        assert_eq!(first.word_rects[1], Some(geometry.words[1].rect));
        assert_eq!(
            second.word_rects[1],
            Some(geometry.words[7].rect),
            "the speaking segment's 'memory' is slot 7. Slot 1 is the first line's 'memory', \
             which is what this asserted before the cursor stopped being reset per segment"
        );
    }

    #[test]
    fn control_a_repeat_inside_one_segment_resolves_to_the_second_occurrence_today() {
        let geometry = owners_page();
        let reference =
            spoken(&["The memory model and atomic operations. The memory model ensures"]);
        let one = anchor_segment(
            &spoken(&[
                "The",
                "memory",
                "model",
                "and",
                "atomic",
                "operations",
                "The",
                "memory",
                "model",
                "ensures",
            ]),
            &geometry,
            None,
            "text",
            true,
            Some(&reference),
        );

        assert_eq!(one.word_rects[1], Some(geometry.words[1].rect));
        assert_eq!(
            one.word_rects[7],
            Some(geometry.words[7].rect),
            "the walk IS monotone inside a segment, so the same repeat that misplaces across a \
             segment boundary is placed correctly when both occurrences are spoken by one call"
        );
    }

    #[test]
    fn control_the_reference_guard_is_not_what_misplaces_the_repeat() {
        let geometry = owners_page();
        let unguarded = anchor_segment(
            &spoken(&["The", "memory", "model", "ensures"]),
            &geometry,
            None,
            "text",
            true,
            None,
        );

        assert_eq!(
            unguarded.word_rects[1],
            Some(geometry.words[1].rect),
            "with the guard removed the second segment still takes slot 1, so the guard is not \
             the mechanism: proof A and this differ only in the guard and agree on the outcome"
        );
        assert_eq!(unguarded.demoted, 0);
    }

    /// Varies the one factor `a_repeated_word_anchors_to…` holds fixed: the second segment
    /// belongs to a **different** block.
    ///
    /// The cursor must therefore RESET, so the new block's own first occurrence wins rather than
    /// the walk continuing from where the previous block stopped. This is the block-boundary
    /// reset that keeps multi-column pages correct — `page_geometry` sorts words row-major while
    /// `extract_blocks` orders blocks column-major, so a cursor carried across a boundary reads
    /// the next block as already consumed. A page-scoped cursor leaves every word here unplaced.
    #[test]
    fn a_new_block_resets_the_cursor_so_its_own_first_occurrence_wins() {
        let geometry = owners_page();
        let first_block = BlockRef {
            reading_order: 0,
            bbox: Rect {
                x0: 40.0,
                y0: 95.0,
                x1: 195.0,
                y1: 118.0,
            },
            text: String::new(),
        };
        let second_block = BlockRef {
            reading_order: 1,
            bbox: Rect {
                x0: 196.0,
                y0: 95.0,
                x1: 300.0,
                y1: 118.0,
            },
            text: String::new(),
        };
        let first_spoken = spoken(&["The", "memory", "model", "and", "atomic", "operations"]);
        let first_reference = spoken(&["The memory model and atomic operations."]);
        let second_spoken = spoken(&["The", "memory", "model", "ensures"]);
        let second_reference = spoken(&["The memory model ensures that as"]);
        let resolved = anchor_page(
            &[
                text_input(&first_spoken, Some(&first_block), &first_reference),
                text_input(&second_spoken, Some(&second_block), &second_reference),
            ],
            &geometry,
        );

        assert_eq!(
            resolved[0].word_rects[1],
            Some(geometry.words[1].rect),
            "the first block's 'memory' is its own slot 1"
        );
        assert_eq!(
            resolved[1].word_rects[0],
            Some(geometry.words[6].rect),
            "the second block's 'the' is slot 6, the first word of ITS span"
        );
        assert_eq!(
            resolved[1].word_rects[1],
            Some(geometry.words[7].rect),
            "and its 'memory' is slot 7"
        );
        assert_eq!(
            resolved[1].unplaced, 0,
            "a cursor carried across the block boundary would index past the new block's four \
             tokens and place nothing at all, so unplaced is the detector for that shape"
        );
    }

    #[test]
    fn a_block_whose_text_contains_newlines_still_anchors_because_offsets_are_never_sliced() {
        let geometry = page(&[("tech", 0), ("nology", 1), ("moved", 1)]);
        let block = BlockRef {
            reading_order: 0,
            bbox: Rect {
                x0: 40.0,
                y0: 90.0,
                x1: 560.0,
                y1: 140.0,
            },
            text: String::new(),
        };
        let anchor = anchor_segment(
            &spoken(&["nology", "moved"]),
            &geometry,
            Some(&block),
            "text",
            true,
            None,
        );

        assert_eq!(anchor.rung, Rung::Word);
        assert_eq!(
            anchor.located, 2,
            "§9.5: char offsets index collapsed-whitespace space, so a \
             slicing implementation reads a shifted window; the token walk does not"
        );
    }

    #[test]
    fn a_hyphen_broken_page_word_is_not_joined_before_matching() {
        let geometry = page(&[("sys", 0), ("tems", 1)]);
        let anchor = anchor_segment(&spoken(&["systems"]), &geometry, None, "text", true, None);

        assert_eq!(
            anchor.located, 0,
            "S1-1b: dehyphenation measured -0.0327 on stream 2 because the TTS speaks the \
             fragments as separate words; joining only the haystack breaks the symmetry"
        );
    }

    #[test]
    fn a_token_that_regresses_against_the_spoken_order_is_demoted_rather_than_anchored() {
        let geometry = page(&[("alpha", 0), ("beta", 0), ("gamma", 0)]);
        let reference = spoken(&["alpha", "beta"]);
        let anchor = anchor_segment(
            &spoken(&["alpha", "beta", "gamma"]),
            &geometry,
            None,
            "text",
            true,
            Some(&reference),
        );

        assert_eq!(anchor.located, 2);
        assert_eq!(
            anchor.demoted, 1,
            "§9.2's mis-anchor blind spot: 'gamma' exists on the page but the segment never said \
             it, so anchoring it would highlight the wrong word with full confidence"
        );
        assert_eq!(anchor.word_rects[2], None);
    }

    #[test]
    fn a_segment_whose_every_match_was_demoted_counts_as_line_not_word_or_block() {
        let geometry = page(&[("alpha", 0), ("beta", 0)]);
        let reference = spoken(&["gamma"]);
        let anchor = anchor_segment(
            &spoken(&["alpha", "beta"]),
            &geometry,
            None,
            "text",
            true,
            Some(&reference),
        );

        assert_eq!(anchor.located, 0);
        assert_eq!(anchor.demoted, 2);
        assert_eq!(
            anchor.rung,
            Rung::Line,
            "the line rung MEANS word-level evidence existed but was untrustworthy; natural \
             line resolution is absorbed into the word rung as null gaps, so this is the only \
             way a segment reaches line and the semantic must be carried by a test, not prose"
        );
        assert!(anchor.word_rects.iter().all(Option::is_none));

        let mut tally = RungTally::default();
        tally.record(&anchor);
        assert_eq!((tally.line, tally.word, tally.block), (1, 0, 0));
        assert_eq!(tally.demoted, 2);
    }

    #[test]
    fn a_segment_with_nothing_located_falls_to_the_block_rung() {
        let geometry = page(&[("printed", 0), ("prose", 0)]);
        let block = BlockRef {
            reading_order: 2,
            bbox: Rect {
                x0: 40.0,
                y0: 90.0,
                x1: 560.0,
                y1: 140.0,
            },
            text: String::new(),
        };
        let anchor = anchor_segment(
            &spoken(&["lecture", "commentary"]),
            &geometry,
            Some(&block),
            "text",
            true,
            None,
        );

        assert_eq!(
            anchor.rung,
            Rung::Block,
            "Teacher's rung when the narration is not the printed text"
        );
        assert_eq!(anchor.rects, vec![block.bbox]);
    }

    #[test]
    fn a_segment_with_no_block_and_nothing_located_falls_to_the_page_rung() {
        let geometry = page(&[("printed", 0)]);
        let anchor = anchor_segment(&spoken(&["lecture"]), &geometry, None, "text", true, None);

        assert_eq!(anchor.rung, Rung::Page);
        assert!(anchor.rects.is_empty());
    }

    #[test]
    fn the_geometry_envelope_round_trips_the_engine_shape() {
        let raw = r#"{"v":1,"pageWidthPt":612.0,"pageHeightPt":792.0,
            "lines":[[50.0,100.0,550.0,112.0]],
            "words":[[50.0,100.0,70.0,112.0,"Raw",0]]}"#;
        let parsed = parse_geometry(raw).expect("the engine envelope must parse");

        assert_eq!(parsed.page_width_pt, 612.0);
        assert_eq!(parsed.lines.len(), 1);
        assert_eq!(parsed.words[0].token, "Raw");
        assert_eq!(parsed.words[0].line, 0);
    }

    #[test]
    fn the_block_index_is_read_from_the_span_and_absent_when_the_producer_declared_none() {
        assert_eq!(
            block_index_of(Some(
                r#"{"char_end":145,"char_start":0,"page":25,"block_index":7}"#
            )),
            Some(7)
        );
        assert_eq!(
            block_index_of(Some(r#"{"char_end":145,"char_start":0,"page":25}"#)),
            None,
            "every production row today predates the writer, and teacher units never declare one"
        );
        assert_eq!(block_index_of(None), None);
    }

    #[test]
    fn a_spoken_entry_that_normalises_to_several_tokens_searches_all_of_them() {
        let geometry = page(&[("man", 0), ("made", 0), ("systems", 0)]);
        let anchor = anchor_segment(&spoken(&["man-made"]), &geometry, None, "text", true, None);

        assert_eq!(anchor.tokens, 2, "the denominator counts both halves");
        assert_eq!(
            anchor.located, 2,
            "searching only the first part while counting both in the denominator is a silent \
             -2.4pp: measured on the real corpus, 140 of 9485 stream-2 entries normalise to more \
             than one token, and faster-whisper produces them constantly (`man -made`)"
        );
        let rect = anchor.word_rects[0].expect("the entry gets one rect");
        assert_eq!(
            (rect.x0, rect.x1),
            (50.0, 95.0),
            "the entry spans two page words, so its rect is their union - a hyphenated compound \
             is one spoken word and must highlight as one"
        );
    }

    #[test]
    fn the_haystack_is_token_granular_so_a_compound_page_word_can_serve_two_needles() {
        let geometry = page(&[("man-made", 0), ("systems", 0)]);
        let anchor = anchor_segment(
            &spoken(&["man", "made", "systems"]),
            &geometry,
            None,
            "text",
            true,
            None,
        );

        assert_eq!(
            anchor.located, 3,
            "the page word 'man-made' yields two tokens; a cursor that advances by whole page \
             words consumes both on the first hit and starves 'made'. Measured on the real app: \
             slot-granular cursor scored 0.9347 on the DDIA chapter where token-granular scores \
             higher, and the Python probe - which flattens both sides - was the reference that \
             exposed it"
        );
    }

    #[test]
    fn a_figure_cue_is_absent_from_the_rate_denominator_but_present_in_the_rung_counts() {
        let geometry = page(&[("Figure", 0), ("one", 0)]);
        let cue = anchor_segment(
            &spoken(&["Figure", "one"]),
            &geometry,
            None,
            "figure_cue",
            false,
            None,
        );

        assert_eq!(cue.tokens, 2, "the raw count stays honest");
        assert_eq!(
            cue.rate_tokens, 0,
            "a cue has no printed counterpart by construction; counting its tokens against the \
             match rate is the 228-token error §9.2 struck, and it would drag the rate under the \
             floor the grep exists to check"
        );

        let mut tally = RungTally::default();
        tally.record(&cue);
        assert_eq!(tally.page, 1);
        assert_eq!(tally.rate(), 0.0);
        assert_eq!(tally.tokens, 2);
        assert_eq!(tally.rate_tokens, 0);
    }

    #[test]
    fn a_code_block_placeholder_is_absent_from_the_rate_denominator_too() {
        let geometry = page(&[("printed", 0), ("prose", 0)]);
        let placeholder = anchor_segment(
            &spoken(&["A", "code", "block", "appears", "here"]),
            &geometry,
            None,
            "text",
            false,
            None,
        );

        assert_eq!(placeholder.tokens, 5);
        assert_eq!(
            placeholder.rate_tokens, 0,
            "§9.2 measured ~35 of the 89 absent tokens as synthesized code-block placeholder \
             prose - a category with no printed counterpart, not a matcher failure, and the \
             filter keys on the flag rather than the kind because these rows are kind='text'"
        );
    }

    #[test]
    fn the_rung_line_names_every_rung_and_the_rate_it_was_computed_from() {
        let mut tally = RungTally::default();
        tally.record(&SegmentAnchor {
            rung: Rung::Word,
            rects: Vec::new(),
            word_rects: Vec::new(),
            located: 9,
            tokens: 10,
            rate_tokens: 10,
            demoted: 1,
            unplaced: 0,
        });
        tally.record(&SegmentAnchor {
            rung: Rung::Block,
            rects: Vec::new(),
            word_rects: Vec::new(),
            located: 0,
            tokens: 5,
            rate_tokens: 5,
            demoted: 0,
            unplaced: 0,
        });

        assert_eq!(
            format_anchor_rungs("0123456789abcdef", &tally),
            "anchor rungs: chapter=01234567 word=1 line=0 block=1 page=0 segments=2 tokens=15 rateTokens=15 \
             located=9 demoted=1 unplaced=0 rate=0.6000"
        );
    }

    #[test]
    fn a_math_symbol_like_equals_anchors_to_spoken_equals_and_gets_rect() {
        let geometry = PageGeometry {
            page_width_pt: 612.0,
            page_height_pt: 792.0,
            lines: vec![Rect {
                x0: 100.0,
                y0: 100.0,
                x1: 300.0,
                y1: 120.0,
            }],
            words: vec![
                GeometryWord {
                    rect: Rect {
                        x0: 100.0,
                        y0: 100.0,
                        x1: 150.0,
                        y1: 120.0,
                    },
                    token: "dmodel".into(),
                    line: 0,
                },
                GeometryWord {
                    rect: Rect {
                        x0: 155.0,
                        y0: 100.0,
                        x1: 170.0,
                        y1: 120.0,
                    },
                    token: "=".into(),
                    line: 0,
                },
                GeometryWord {
                    rect: Rect {
                        x0: 175.0,
                        y0: 100.0,
                        x1: 210.0,
                        y1: 120.0,
                    },
                    token: "512".into(),
                    line: 0,
                },
            ],
        };
        let input_spoken = spoken(&["dmodel", "equals", "512"]);
        let anchor = anchor_segment(&input_spoken, &geometry, None, "text", true, None);

        assert_eq!(anchor.rung, Rung::Word);
        assert_eq!(anchor.located, 3);
        assert_eq!(anchor.word_rects[0], Some(geometry.words[0].rect));
        assert_eq!(
            anchor.word_rects[1],
            Some(geometry.words[1].rect),
            "the '=' symbol in geometry matches the spoken 'equals' token"
        );
        assert_eq!(anchor.word_rects[2], Some(geometry.words[2].rect));
    }
}
