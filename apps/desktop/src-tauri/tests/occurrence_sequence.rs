use std::collections::HashMap;
use std::path::{Path, PathBuf};

use txtcaster_lib::reading::anchor::{
    anchor_page, anchor_segment, recover_block_index, BlockRef, GeometryWord, PageGeometry, Rect,
    SegmentInput,
};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/occurrence_sequence.json")
}

fn fixture() -> serde_json::Value {
    let raw = std::fs::read_to_string(fixture_path()).expect("the fixture must be committed");
    serde_json::from_str(&raw).expect("fixture json")
}

struct Case {
    name: String,
    note: String,
    geometry: PageGeometry,
    blocks: Vec<BlockRef>,
    segments: Vec<Segment>,
}

struct Segment {
    text: String,
    char_start: i64,
    char_end: i64,
    block_index: i64,
    true_slots: Vec<Option<usize>>,
}

fn rect_of(raw: &serde_json::Value) -> Rect {
    let nums: Vec<f64> = raw
        .as_array()
        .expect("bbox array")
        .iter()
        .filter_map(|v| v.as_f64())
        .collect();
    Rect::from_slice(&nums).expect("four coordinates")
}

fn cases() -> Vec<Case> {
    fixture()["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| {
            let words: Vec<GeometryWord> = case["words"]
                .as_array()
                .expect("words")
                .iter()
                .map(|row| {
                    let row = row.as_array().expect("word row");
                    let nums: Vec<f64> = row.iter().take(4).filter_map(|v| v.as_f64()).collect();
                    GeometryWord {
                        rect: Rect::from_slice(&nums).expect("word rect"),
                        token: row[4].as_str().expect("token").to_string(),
                        line: row[5].as_u64().expect("line") as usize,
                    }
                })
                .collect();
            let lines: Vec<Rect> = case["lines"]
                .as_array()
                .expect("lines")
                .iter()
                .map(rect_of)
                .collect();
            Case {
                name: case["name"].as_str().expect("name").to_string(),
                note: case["note"].as_str().unwrap_or_default().to_string(),
                geometry: PageGeometry {
                    page_width_pt: case["pageWidthPt"].as_f64().unwrap_or(0.0),
                    page_height_pt: case["pageHeightPt"].as_f64().unwrap_or(0.0),
                    lines,
                    words,
                },
                blocks: case["blocks"]
                    .as_array()
                    .expect("blocks")
                    .iter()
                    .map(|block| BlockRef {
                        reading_order: block["readingOrder"].as_i64().expect("readingOrder"),
                        bbox: rect_of(&block["bbox"]),
                        text: block["text"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect(),
                segments: case["segments"]
                    .as_array()
                    .expect("segments")
                    .iter()
                    .map(|segment| Segment {
                        text: segment["text"].as_str().expect("text").to_string(),
                        char_start: segment["charStart"].as_i64().expect("charStart"),
                        char_end: segment["charEnd"].as_i64().expect("charEnd"),
                        block_index: segment["blockIndex"].as_i64().expect("blockIndex"),
                        true_slots: segment["trueSlots"]
                            .as_array()
                            .expect("trueSlots")
                            .iter()
                            .map(|slot| slot.as_u64().map(|value| value as usize))
                            .collect(),
                    })
                    .collect(),
            }
        })
        .collect()
}

fn span_json(segment: &Segment) -> String {
    serde_json::json!({
        "page": 1,
        "char_start": segment.char_start,
        "char_end": segment.char_end,
    })
    .to_string()
}

struct Outcome {
    wrong: usize,
    pinned: usize,
    unplaced: usize,
    recovered: usize,
    double_claimed: usize,
}

/// `paged == true` walks the page the way the product does; `paged == false` resolves each
/// segment on its own, which is the behaviour the defect lived in. `declared == true` supplies
/// the `block_index` a freshly scripted row carries; `declared == false` forces the recovery
/// path, which is the shape of every row written before that writer landed.
fn score(case: &Case, paged: bool, declared: bool) -> Outcome {
    let spoken: Vec<Vec<String>> = case
        .segments
        .iter()
        .map(|segment| {
            segment
                .text
                .split_whitespace()
                .map(str::to_string)
                .collect()
        })
        .collect();
    let references: Vec<Vec<String>> = case
        .segments
        .iter()
        .map(|segment| vec![segment.text.clone()])
        .collect();
    let spans: Vec<String> = case.segments.iter().map(span_json).collect();
    let blocks: Vec<Option<i64>> = spans
        .iter()
        .zip(case.segments.iter())
        .map(|(span, segment)| {
            if declared {
                Some(segment.block_index)
            } else {
                recover_block_index(&case.blocks, Some(span), &segment.text)
            }
        })
        .collect();
    let recovered = blocks.iter().filter(|index| index.is_some()).count();
    let resolved_blocks: Vec<Option<&BlockRef>> = blocks
        .iter()
        .map(|index| {
            index.and_then(|index| {
                case.blocks
                    .iter()
                    .find(|block| block.reading_order == index)
            })
        })
        .collect();

    let inputs: Vec<SegmentInput<'_>> = (0..case.segments.len())
        .map(|position| SegmentInput {
            spoken: &spoken[position],
            block: resolved_blocks[position],
            kind: "text",
            counts_toward_rate: true,
            reference: Some(&references[position]),
        })
        .collect();

    let anchors = if paged {
        anchor_page(&inputs, &case.geometry)
    } else {
        inputs
            .iter()
            .map(|input| {
                anchor_segment(
                    input.spoken,
                    &case.geometry,
                    input.block,
                    input.kind,
                    input.counts_toward_rate,
                    input.reference,
                )
            })
            .collect()
    };

    let mut wrong = 0;
    let mut pinned = 0;
    let mut unplaced = 0;
    let mut claims: HashMap<usize, usize> = HashMap::new();
    for (position, anchor) in anchors.iter().enumerate() {
        unplaced += anchor.unplaced;
        for (index, expected) in case.segments[position].true_slots.iter().enumerate() {
            let Some(slot) = expected else { continue };
            pinned += 1;
            let want = case.geometry.words[*slot].rect;
            match anchor.word_rects.get(index).copied().flatten() {
                Some(got) if got == want => {
                    *claims.entry(*slot).or_default() += 1;
                }
                _ => wrong += 1,
            }
        }
    }
    Outcome {
        wrong,
        pinned,
        unplaced,
        recovered,
        double_claimed: claims.values().filter(|count| **count > 1).count(),
    }
}

#[test]
fn the_fixture_still_exercises_the_defect_it_exists_to_catch() {
    let cases = cases();
    assert!(cases.len() >= 4, "four document classes were registered");
    let mut total_pinned = 0;
    let mut discriminating = Vec::new();
    for case in &cases {
        let per_segment = score(case, false, true);
        total_pinned += per_segment.pinned;
        assert!(
            per_segment.pinned >= 100,
            "{}: a case that pins almost nothing cannot fail either way ({} pinned)",
            case.name,
            per_segment.pinned
        );
        if per_segment.wrong > 0 {
            discriminating.push((case.name.clone(), per_segment.wrong, per_segment.pinned));
        }
    }
    assert!(
        total_pinned >= 900,
        "the fixture must stay substantial: {total_pinned} pinned words"
    );
    assert!(
        !discriminating.is_empty(),
        "NO case distinguishes per-segment resolution from the page walk, so this whole file \
         would pass with the defect restored. Regenerating the fixture must never reach this \
         state: {:?}",
        cases.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    assert!(
        discriminating
            .iter()
            .any(|(name, _, _)| name == "ddia_p25_one_block"),
        "the real page must be one of the discriminating cases, or the guard rests only on \
         generated fixtures: {discriminating:?}"
    );
}

#[test]
fn a_freshly_scripted_row_anchors_every_word_to_the_occurrence_being_read() {
    for case in cases() {
        let paged = score(&case, true, true);
        assert_eq!(
            paged.wrong, 0,
            "{} ({}): {} of {} words anchored to the wrong occurrence",
            case.name, case.note, paged.wrong, paged.pinned
        );
        assert_eq!(
            paged.unplaced, 0,
            "{}: {} tokens were not found ahead of the cursor. A non-zero count here is the \
             detector that the cursor's SCOPE is wrong, which is why it is pinned rather than \
             papered over with a nearest-match fallback",
            case.name, paged.unplaced
        );
        assert_eq!(
            paged.double_claimed, 0,
            "{}: {} printed slots are claimed by more than one spoken word. Two times mapping to \
             one pixel region is what makes tap-to-seek send a listener backwards",
            case.name, paged.double_claimed
        );
    }
}

/// The owner's existing audiobooks carry `char_start`/`char_end` and **no** `block_index`, so
/// this is the case that decides whether the fix reaches audio he already has.
///
/// Recovery is deliberately refusing rather than guessing: it accepts a block only when exactly
/// one block's slice at those offsets IS the segment's text. Two blocks holding byte-identical
/// text at the same offsets are genuinely indistinguishable from the offsets alone, so recovery
/// declines and the segment keeps the behaviour it had before this change. That is the measured
/// limit of the mechanism, asserted here so it cannot be mistaken for a fix that covers
/// everything.
#[test]
fn an_existing_row_is_fixed_where_its_block_is_recoverable_and_never_made_worse_where_it_is_not() {
    let mut ambiguous = Vec::new();
    for case in cases() {
        let recovered = score(&case, true, false);
        let before = score(&case, false, false);
        if recovered.recovered == case.segments.len() {
            assert_eq!(
                recovered.wrong, 0,
                "{}: every segment's block was recovered from the offsets the engine already \
                 wrote, so no re-creation is needed and nothing should be misplaced; {} of {} \
                 wrong",
                case.name, recovered.wrong, recovered.pinned
            );
            assert_eq!(recovered.double_claimed, 0, "{}", case.name);
            continue;
        }
        assert_eq!(
            recovered.wrong,
            before.wrong,
            "{}: recovery declined on {} of {} segments, so the outcome must be byte-identical \
             to the pre-fix behaviour — degrade, never lie. Got {} wrong against {} before",
            case.name,
            case.segments.len() - recovered.recovered,
            case.segments.len(),
            recovered.wrong,
            before.wrong
        );
        ambiguous.push((case.name.clone(), recovered.recovered, case.segments.len()));
    }
    assert_eq!(
        ambiguous
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect::<Vec<&str>>(),
        vec!["two_column_identical_text", "straddle_then_columns"],
        "exactly these two registered classes are recovery-ambiguous, and for one reason: the \
         repo's generators fill every block with the same BODY_PARAGRAPH, so their blocks are \
         byte-identical and the offsets cannot choose between them. That is a property of the \
         generators, not of multi-column pages - two_column_distinct_six_blocks is the same \
         geometry with distinct text and recovers all six blocks. If this list grows, the \
         recovery rule has become weaker than it was measured to be: {ambiguous:?}"
    );
}

#[test]
fn a_cursor_shared_across_block_boundaries_would_break_the_multi_column_pages() {
    let cases = cases();
    let multi: Vec<&Case> = cases
        .iter()
        .filter(|case| case.name.contains("column") || case.name.contains("straddle"))
        .collect();
    assert_eq!(
        multi.len(),
        4,
        "four multi-column shapes are registered; they are the reason the cursor is scoped to a \
         block rather than to the page"
    );
    for case in multi {
        let paged = score(case, true, true);
        assert_eq!(
            paged.wrong, 0,
            "{}: page_geometry sorts words row-major while extract_blocks orders blocks \
             column-major, so a cursor carried across a block boundary reads the second column \
             as already consumed. Measured before this guard existed: 46.7 % wrong on \
             two_column_distinct. {} of {} wrong",
            case.name, paged.wrong, paged.pinned
        );
    }
}
