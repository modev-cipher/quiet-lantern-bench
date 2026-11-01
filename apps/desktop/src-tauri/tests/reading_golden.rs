use std::path::{Path, PathBuf};

use txtcaster_lib::cache_path;
use txtcaster_lib::reading::anchor::Rung;
use txtcaster_lib::reading::{
    self, GeometryOutcome, ReadingChapter, ReadingFigure, ReadingIndex, ReadingPage, TimelineOrigin,
};
use txtcaster_lib::render::mp3::{CHAPTER_GAP_MS, SEGMENT_GAP_MS};
use txtcaster_lib::storage::{self, book, chapter_render, job, now_ms, voice, SqlitePool};
use txtcaster_lib::words::{Timebase, WordProvenance, WordsJson};

const BOOK_ID: &str = "book-golden";
const JOB_ID: &str = "job-golden";
const SEGMENT_MS: i64 = 900;
const ENCODED_FIRST: i64 = 2252;
const ENCODED_SECOND: i64 = 3456;
const UPDATE_ENV: &str = "UPDATE_READING_GOLDEN";

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Golden {
    index: ReadingIndex,
    chapters: Vec<ReadingChapter>,
    pages: Vec<ReadingPage>,
    rendered: Vec<ReadingPage>,
    figures: Vec<ReadingFigure>,
}

const RENDERED_PAGES: [i64; 2] = [1, 4];
const ALL_PAGES: [i64; 4] = [1, 2, 3, 4];
const ANCHORED_PAGE: i64 = 1;
const BOUNDARY_PAGE: i64 = 2;
const FRAMED_PAGE: i64 = 4;

fn page_hash(page_no: i64) -> String {
    format!("page-hash-{page_no}")
}

fn geometry_json() -> String {
    serde_json::json!({
        "v": 1,
        "pageWidthPt": 612.0,
        "pageHeightPt": 792.0,
        "lines": [
            [72.0, 100.0, 240.0, 112.0],
            [72.0, 130.0, 292.0, 142.0]
        ],
        "words": [
            [72.0, 100.0, 98.0, 112.0, "The", 0],
            [102.0, 100.0, 134.0, 112.0, "first", 0],
            [138.0, 100.0, 184.0, 112.0, "reads", 0],
            [188.0, 100.0, 240.0, 112.0, "aloud", 0],
            [72.0, 130.0, 98.0, 142.0, "The", 1],
            [102.0, 130.0, 152.0, 142.0, "second", 1],
            [156.0, 130.0, 224.0, 142.0, "sentence", 1],
            [228.0, 130.0, 292.0, 142.0, "follows", 1]
        ]
    })
    .to_string()
}

fn text_json() -> String {
    serde_json::json!({
        "quality": {"source": "pymupdf", "multi_column": false, "low_confidence": false},
        "blocks": [
            {"bbox": [70.0, 96.0, 244.0, 116.0], "text": "The first reads aloud",
             "font_size": 11.0, "reading_order": 0, "column": 0},
            {"bbox": [70.0, 126.0, 296.0, 146.0], "text": "The second sentence follows",
             "font_size": 11.0, "reading_order": 1, "column": 0}
        ]
    })
    .to_string()
}

fn source_span(page: i64, block_index: i64, char_end: i64) -> String {
    serde_json::json!({
        "page": page,
        "char_start": 0,
        "char_end": char_end,
        "block_index": block_index,
    })
    .to_string()
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../e2e/fixtures/reading-timeline.golden.json")
}

async fn seed(pool: &SqlitePool) {
    let b = book::Book {
        id: BOOK_ID.into(),
        title: "The Golden Fixture".into(),
        author: Some("A Narrator".into()),
        source_path: "/books/golden.pdf".into(),
        format: "pdf".into(),
        page_count: Some(4),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: Some("fast".into()),
        cover_path: None,
        content_hash: "golden-content-hash".into(),
        blocks_version: None,
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    book::insert_book(pool, &b).await.expect("book");
    for index in 0..2 {
        book::insert_chapter(
            pool,
            &book::Chapter {
                id: format!("chap-{index}"),
                book_id: BOOK_ID.into(),
                idx: index,
                title: format!("Chapter {}", index + 1),
                page_start: index * 2 + 1,
                page_end: index * 2 + 2,
                is_pseudo: false,
                word_count: Some(24),
            },
        )
        .await
        .expect("chapter");
    }
    voice::insert_voice_profile(
        pool,
        &voice::VoiceProfile {
            id: "voice-golden".into(),
            name: "Default".into(),
            engine: "kokoro-82m".into(),
            kind: "preset".into(),
            preset_voice_id: Some("af_heart".into()),
            reference_wav_path: None,
            params_json: None,
            created_at: now_ms(),
        },
    )
    .await
    .expect("voice");
    job::insert_job(
        pool,
        &job::RenderJob {
            id: JOB_ID.into(),
            book_id: BOOK_ID.into(),
            scope_json: r#"{"kind":"pages","page_start":1,"page_end":4}"#.into(),
            mode: "narrator".into(),
            voice_profile_id: "voice-golden".into(),
            output_format: "mp3".into(),
            model_overrides_json: None,
            resource_plan_json: None,
            vision_plan_json: None,
            tts_plan_json: None,
            align_plan_json: None,
            state: "DONE".into(),
            checkpoint_json: None,
            overnight: false,
            eta_seconds: None,
            report_json: None,
            output_paths_json: None,
            tts_seed: None,
            verify: true,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    )
    .await
    .expect("job");

    for page_no in ALL_PAGES {
        book::insert_page(
            pool,
            &book::Page {
                id: format!("page-{page_no:04}"),
                book_id: BOOK_ID.into(),
                page_no,
                width_pt: Some(612.0),
                height_pt: Some(792.0),
                render_hash: if RENDERED_PAGES.contains(&page_no) {
                    Some(page_hash(page_no))
                } else {
                    None
                },
                text_json: if [ANCHORED_PAGE, BOUNDARY_PAGE, FRAMED_PAGE].contains(&page_no) {
                    Some(text_json())
                } else {
                    None
                },
                needs_upgrade: false,
            },
        )
        .await
        .expect("page");
    }

    for page_no in [ANCHORED_PAGE, BOUNDARY_PAGE, FRAMED_PAGE] {
        book::upsert_page_geometry(
            pool,
            &book::PageGeometry {
                page_id: format!("page-{page_no:04}"),
                geometry_version: "v1|geom=1|golden".into(),
                geometry_json: geometry_json(),
            },
        )
        .await
        .expect("page geometry");
    }

    book::insert_figure_region(
        pool,
        &book::FigureRegion {
            id: "figure-7".into(),
            book_id: BOOK_ID.into(),
            page_no: 4,
            bbox_json: "[72.0,120.5,468.0,396.25]".into(),
            kind: "chart".into(),
            confidence: 0.92,
            crop_hash: Some("crop-hash-7".into()),
            caption: Some("Figure 7. Throughput against context length.".into()),
            explanation: Some("A line chart whose throughput falls as context grows.".into()),
            caption_confidence: Some(0.81),
        },
    )
    .await
    .expect("figure region");

    book::insert_figure_region(
        pool,
        &book::FigureRegion {
            id: "figure-9".into(),
            book_id: BOOK_ID.into(),
            page_no: 2,
            bbox_json: "[90.0,240.0,522.0,540.0]".into(),
            kind: "table".into(),
            confidence: 0.88,
            crop_hash: Some("crop-hash-9".into()),
            caption: Some("Table 9. Latency by batch size.".into()),
            explanation: Some("A table of latencies rising with batch size.".into()),
            caption_confidence: Some(0.77),
        },
    )
    .await
    .expect("second figure region");

    book::insert_figure_region(
        pool,
        &book::FigureRegion {
            id: "figure-11".into(),
            book_id: BOOK_ID.into(),
            page_no: FRAMED_PAGE,
            bbox_json: "[100.0,300.0,500.0,600.0]".into(),
            kind: "figure".into(),
            confidence: 0.9,
            crop_hash: Some("crop-hash-11".into()),
            caption: Some("Figure 11. A diagram on the page it is discussed on.".into()),
            explanation: Some("A diagram whose page is the narrating segment's own.".into()),
            caption_confidence: Some(0.8),
        },
    )
    .await
    .expect("same-page figure region");

    let plan = [
        (
            "chap-0",
            "text",
            None,
            "The first sentence reads aloud.",
            Some(source_span(ANCHORED_PAGE, 0, 31)),
        ),
        (
            "chap-0",
            "text",
            None,
            "The second sentence follows it.",
            Some(source_span(BOUNDARY_PAGE, 1, 31)),
        ),
        (
            "chap-1",
            "figure_cue",
            Some("figure-7"),
            "A chart appears here.",
            None,
        ),
        (
            "chap-1",
            "text",
            Some("figure-9"),
            "The lecture resumes after it.",
            None,
        ),
        (
            "chap-1",
            "text",
            Some("figure-11"),
            "A diagram sits beside this sentence.",
            Some(source_span(FRAMED_PAGE, 0, 36)),
        ),
        (
            "chap-1",
            "text",
            None,
            "The lecture keeps going on the same page.",
            Some(source_span(FRAMED_PAGE, 1, 41)),
        ),
    ];
    let segment_pages = [ANCHORED_PAGE, BOUNDARY_PAGE, 3, 3, FRAMED_PAGE, FRAMED_PAGE];
    let segments: Vec<job::ScriptSegment> = plan
        .iter()
        .enumerate()
        .map(
            |(idx, (chapter_id, kind, figure_id, text, span))| job::ScriptSegment {
                id: format!("seg-{idx:04}"),
                job_id: JOB_ID.into(),
                chapter_id: (*chapter_id).into(),
                idx: idx as i64,
                page_no: segment_pages[idx],
                kind: (*kind).into(),
                figure_id: figure_id.map(str::to_string),
                text: (*text).into(),
                source_span_json: span.clone(),
                flags_json: None,
                audio_hash: Some(format!("hash-{idx:04}")),
                duration_ms: Some(SEGMENT_MS),
                original_text: None,
            },
        )
        .collect();
    job::insert_segments(pool, &segments)
        .await
        .expect("segments");

    let fresh = vec![
        serde_json::json!({"w": "The", "s": 0, "e": 180}),
        serde_json::json!({"w": "first", "s": 200, "e": 430}),
        serde_json::json!({"w": "sentence", "s": 450, "e": 880}),
        serde_json::json!({"w": "aloud", "s": 890, "e": SEGMENT_MS + 400}),
        serde_json::json!({"w": "", "s": SEGMENT_MS, "e": SEGMENT_MS}),
    ];
    job::upsert_alignment(
        pool,
        &job::Alignment {
            segment_id: "seg-0000".into(),
            source: "tts_native".into(),
            words_json: WordsJson::declare(&fresh, Timebase::Trimmed).expect("declare"),
            wer: Some(0.0),
        },
    )
    .await
    .expect("fresh alignment");
    job::upsert_alignment(
        pool,
        &job::Alignment {
            segment_id: "seg-0001".into(),
            source: "tts_native".into(),
            words_json: WordsJson::from_stored(
                r#"[{"w":"The","s":0,"e":200},{"w":"second","s":200,"e":600}]"#.into(),
            ),
            wer: None,
        },
    )
    .await
    .expect("stale alignment");
    let cue = vec![
        serde_json::json!({"w": "A", "s": 0, "e": 120}),
        serde_json::json!({"w": "chart", "s": 140, "e": 520}),
    ];
    job::upsert_alignment(
        pool,
        &job::Alignment {
            segment_id: "seg-0002".into(),
            source: "whisperx".into(),
            words_json: WordsJson::declare(&cue, Timebase::Trimmed).expect("declare"),
            wer: Some(0.04),
        },
    )
    .await
    .expect("cue alignment");

    let body = 2 * SEGMENT_MS + SEGMENT_GAP_MS;
    chapter_render::replace_all(
        pool,
        JOB_ID,
        &[
            chapter_render::ChapterRender {
                job_id: JOB_ID.into(),
                chapter_id: "chap-0".into(),
                idx: 0,
                hash: "chapter-hash-0".into(),
                artifact: chapter_render::ARTIFACT_MP3.into(),
                duration_ms: body,
                encoded_ms: Some(ENCODED_FIRST),
            },
            chapter_render::ChapterRender {
                job_id: JOB_ID.into(),
                chapter_id: "chap-1".into(),
                idx: 1,
                hash: "chapter-hash-1".into(),
                artifact: chapter_render::ARTIFACT_MP3.into(),
                duration_ms: body + CHAPTER_GAP_MS,
                encoded_ms: Some(ENCODED_SECOND),
            },
        ],
    )
    .await
    .expect("chapter rows");
}

async fn build() -> Golden {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    seed(&pool).await;
    let cache = dir.path().join("cache");
    for page_no in RENDERED_PAGES {
        let path = cache_path::cache_path(&cache, &page_hash(page_no));
        std::fs::create_dir_all(path.parent().expect("shard")).expect("shard dir");
        std::fs::write(&path, b"png").expect("page image");
    }
    let index = reading::index(&pool, JOB_ID, &cache).await.expect("index");
    let pool_ref = &pool;
    let mut chapters = Vec::new();
    for summary in &index.chapters {
        chapters.push(
            reading::chapter(
                pool_ref,
                JOB_ID,
                &summary.chapter_id,
                &cache,
                |book_id, pages| async move {
                    reading::ensure_geometry_for_chapter(
                        pool_ref,
                        &book_id,
                        &pages,
                        |_batch| async { Ok(()) },
                    )
                    .await
                    .outcome()
                },
            )
            .await
            .expect("chapter"),
        );
    }
    let mut figure_ids = Vec::new();
    for chapter in &chapters {
        for segment in &chapter.segments {
            if let Some(figure_id) = &segment.figure_id {
                if !figure_ids.contains(figure_id) {
                    figure_ids.push(figure_id.clone());
                }
            }
        }
    }
    let pages = reading::pages(&pool, BOOK_ID, &ALL_PAGES, &cache)
        .await
        .expect("pages")
        .into_iter()
        .map(|page| ReadingPage {
            image_path: page
                .image_path
                .as_deref()
                .map(|path| cache_path::to_sentinel(&cache, Path::new(path))),
            ..page
        })
        .collect();
    for page_no in ALL_PAGES {
        let path = cache_path::cache_path(&cache, &page_hash(page_no));
        std::fs::create_dir_all(path.parent().expect("shard")).expect("shard dir");
        std::fs::write(&path, b"png").expect("page image");
        book::set_page_render_hash(&pool, &format!("page-{page_no:04}"), &page_hash(page_no))
            .await
            .expect("render hash");
    }
    let rendered = reading::pages(&pool, BOOK_ID, &ALL_PAGES, &cache)
        .await
        .expect("rendered pages")
        .into_iter()
        .map(|page| ReadingPage {
            image_path: page
                .image_path
                .as_deref()
                .map(|path| cache_path::to_sentinel(&cache, Path::new(path))),
            ..page
        })
        .collect();
    let figures = reading::figures(&pool, BOOK_ID, &figure_ids)
        .await
        .expect("figures");
    Golden {
        index,
        chapters,
        pages,
        rendered,
        figures,
    }
}

#[tokio::test]
async fn the_golden_reading_payload_is_the_one_the_mock_serves() {
    let golden = build().await;
    let rendered = serde_json::to_string_pretty(&golden).expect("serialize") + "\n";
    let path = golden_path();

    if std::env::var(UPDATE_ENV).is_ok() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture dir");
        }
        std::fs::write(&path, &rendered).expect("write golden");
        return;
    }

    let stored = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "the e2e mock serves this file, so it is generated from the Rust payload rather than \
             hand-written: {} ({error}). Regenerate with {UPDATE_ENV}=1 cargo test --test \
             reading_golden",
            path.display()
        )
    });
    assert_eq!(
        stored, rendered,
        "the committed golden payload no longer matches what the reading commands produce. A \
         hand-written mock that drifts in the safe direction turns a missing test into a passing \
         one (#111, #107), so both ends read this one literal. Regenerate with {UPDATE_ENV}=1 \
         cargo test --test reading_golden and review the diff."
    );
}

#[tokio::test]
async fn the_golden_payload_obeys_the_composition_the_suite_asserts() {
    let golden = build().await;

    assert_eq!(golden.index.origin, TimelineOrigin::Measured);
    assert_eq!(golden.index.total_ms, ENCODED_FIRST + ENCODED_SECOND);
    assert_eq!(golden.index.chapters[0].start_ms, 0);
    assert_eq!(
        golden.index.chapters[1].start_ms,
        ENCODED_FIRST + CHAPTER_GAP_MS,
        "a measured chapter starts at the cumulative encoded length plus its own leading gap"
    );

    let pages: Vec<i64> = golden
        .chapters
        .iter()
        .flat_map(|chapter| chapter.segments.iter().map(|segment| segment.page_no))
        .collect();
    assert_eq!(
        pages,
        vec![1, 2, 3, 3, 4, 4],
        "the facsimile pane is driven by page_no and nothing else in the payload constrains it, \
         so a corrupted page_no would survive regeneration and every other assertion here \
         (PIPELINE_SPEC 8, DATA_MODELS 2.4a); the 1->2 pair inside chapter 0 is the \
         segment-boundary page turn fixture (DATA_MODELS 2.4e) and nothing else carries it"
    );
    let chapter_ids: Vec<&str> = golden
        .chapters
        .iter()
        .map(|chapter| chapter.chapter_id.as_str())
        .collect();
    assert_eq!(chapter_ids, vec!["chap-0", "chap-1"]);
    for (position, chapter) in golden.chapters.iter().enumerate() {
        assert_eq!(
            chapter.chapter_id,
            golden.index.chapters[position].chapter_id
        );
        assert_eq!(chapter.start_ms, golden.index.chapters[position].start_ms);
        assert_eq!(chapter.end_ms, golden.index.chapters[position].end_ms);
        assert_eq!(
            chapter.segments.len() as i64,
            golden.index.chapters[position].segment_count,
            "the index promises a segment count the chapter read has to deliver"
        );
        for segment in &chapter.segments {
            assert!(!segment.text.is_empty(), "the view renders this text");
            assert!(segment.end_ms > segment.start_ms);
            if segment.kind == "figure_cue" {
                assert!(
                    segment.figure_id.is_some(),
                    "a cue with nothing to point at is a scripting bug, not a framing case"
                );
            }
        }
    }
    let indices: Vec<i64> = golden
        .chapters
        .iter()
        .flat_map(|chapter| chapter.segments.iter().map(|segment| segment.idx))
        .collect();
    assert_eq!(
        indices,
        vec![0, 1, 2, 3, 4, 5],
        "reading order is the segment idx"
    );

    let first = &golden.chapters[0];
    assert_eq!(first.segments[0].provenance, WordProvenance::Fresh);
    assert_eq!(
        first.segments[0].words[0].start_ms, first.segments[0].start_ms,
        "a word onset is its segment's start plus the word's own offset"
    );
    assert_eq!(
        first.segments[0].words.last().expect("words").end_ms,
        first.segments[0].end_ms,
        "the clamp pins an overlong span to the end of its segment"
    );
    assert_eq!(
        first.segments[0].words.len(),
        4,
        "the zero-length span at the segment end carries no interval and is dropped"
    );
    assert_eq!(first.segments[1].provenance, WordProvenance::Stale);
    assert!(first.segments[1].words.is_empty());

    let second = &golden.chapters[1];
    assert_eq!(second.start_ms, ENCODED_FIRST + CHAPTER_GAP_MS);
    assert_eq!(second.segments[0].kind, "figure_cue");
    assert_eq!(second.segments[0].figure_id.as_deref(), Some("figure-7"));
    assert_eq!(second.segments[1].kind, "text");
    assert_eq!(
        second.segments[1].figure_id.as_deref(),
        Some("figure-9"),
        "framing is kind-independent by ruling (PIPELINE_SPEC 8, D8), so the fixture has to carry \
         a text segment that anchors a figure or the teacher-mode path is never exercised"
    );
    assert_eq!(second.segments[0].provenance, WordProvenance::Fresh);
    assert_eq!(second.segments[1].provenance, WordProvenance::Missing);
    assert!(second.segments[1].words.is_empty());
    assert_eq!(
        second.segments[1].start_ms,
        second.segments[0].end_ms + SEGMENT_GAP_MS
    );
}

#[tokio::test]
async fn the_golden_carries_a_real_word_anchor_so_an_overlay_can_be_drawn_against_it() {
    let golden = build().await;

    assert_eq!(
        golden.chapters[0].anchor_geometry,
        GeometryOutcome::Ensured,
        "page 1 carries geometry and the outcome is derived from production coverage, not \
         declared by the fixture"
    );
    assert_eq!(
        golden.chapters[1].anchor_geometry,
        GeometryOutcome::Unavailable,
        "page 3 carries none, so the mock also serves the re-fetch case rather than only the \
         happy one"
    );

    let anchored = &golden.chapters[0].segments[0];
    assert_eq!(anchored.anchor, Rung::Word);
    assert!(
        anchored.rects.is_empty(),
        "a word-rung segment carries no segment-level rect (DATA_MODELS 2.4c)"
    );
    let located: Vec<bool> = anchored.words.iter().map(|w| w.rect.is_some()).collect();
    assert_eq!(
        located,
        vec![true, true, false, true],
        "'sentence' is spoken but sits in a different block on the page, so the golden carries a \
         real per-token gap in the middle of a segment; a gap only at the end would give the \
         fallback a ten-millisecond window nothing could observe"
    );
    let first = anchored.words[0].rect.expect("the first word located");
    assert_eq!(
        first.y0, 100.0,
        "block_index 0 scopes the walk to the first block, so 'The' anchors on line 0 rather \
         than on line 1's identical token"
    );

    let block_rung = &golden.chapters[0].segments[1];
    assert_eq!(block_rung.anchor, Rung::Block);
    assert_eq!(
        block_rung.rects.len(),
        1,
        "a stale segment has no words to walk, so its declared block is the whole anchor and it \
         is the fixture for the segment-level fallback"
    );
    assert_eq!(block_rung.rects[0].y0, 126.0);

    let cue = &golden.chapters[1].segments[0];
    assert_eq!(cue.anchor, Rung::Page);
    assert!(
        cue.rects.is_empty() && cue.words.iter().all(|w| w.rect.is_none()),
        "a figure cue never word-anchors and has no page rect either, so Page mode draws no \
         overlay at all for it"
    );
}

#[tokio::test]
async fn the_golden_frames_each_figure_on_its_own_page_not_on_its_segment_page() {
    let golden = build().await;

    let segments: Vec<_> = golden
        .chapters
        .iter()
        .flat_map(|chapter| chapter.segments.iter())
        .collect();
    let mut elsewhere = 0;
    let mut on_its_own_page = 0;
    for segment in &segments {
        let Some(figure_id) = &segment.figure_id else {
            continue;
        };
        let figure = golden
            .figures
            .iter()
            .find(|figure| &figure.figure_id == figure_id)
            .expect("every referenced figure is served with the payload");
        if figure.page_no == segment.page_no {
            on_its_own_page += 1;
        } else {
            elsewhere += 1;
        }
    }
    assert!(
        elsewhere > 0,
        "the R4 ruling is that framing shows the figure's page; with every pair equal, an \
         implementation that framed segment.page_no would pass every framing assertion"
    );
    assert!(
        on_its_own_page > 0,
        "a figure on its own segment's page is the ordinary book layout, and it is the only \
         shape where the page-identity gate stays quiet while the zoom still unmounts the \
         overlay. Requiring every pair to differ left that path with no fixture at all, which is \
         how the remount defect reached review"
    );
    assert_eq!(
        golden.figures.len(),
        3,
        "figures on more than one page are what separate 'framing follows the figure' from \
         'framing is sticky from the previous segment'"
    );
    let figure_pages: Vec<i64> = golden.figures.iter().map(|figure| figure.page_no).collect();
    assert_eq!(figure_pages, vec![4, 2, 4]);
    assert_eq!(
        golden.figures[0].bbox,
        Some([72.0, 120.5, 468.0, 396.25]),
        "the viewport transform is computed from these page points"
    );
}

#[tokio::test]
async fn the_golden_carries_both_a_cached_page_and_an_evicted_one() {
    let golden = build().await;

    let numbers: Vec<i64> = golden.pages.iter().map(|page| page.page_no).collect();
    assert_eq!(numbers, ALL_PAGES.to_vec());
    for page in &golden.pages {
        assert!(
            page.width_pt.is_some() && page.height_pt.is_some(),
            "the placeholder sizes itself from these, so a null pair would let the pane jump"
        );
        assert_eq!(
            page.image_path.is_some(),
            RENDERED_PAGES.contains(&page.page_no),
            "a page whose render is not in the cache must report no image at all"
        );
    }
    let cached = golden
        .pages
        .iter()
        .find(|page| page.page_no == 1)
        .expect("page 1");
    assert_eq!(
        cached.image_path.as_deref(),
        Some("<cache>/pa/page-hash-1"),
        "the committed golden is compared byte for byte on windows too, so the cache prefix is a \
         sentinel and the separators are normalised"
    );
}

fn mock_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../e2e/tauriMock.ts");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

#[test]
fn the_e2e_mock_serves_the_golden_reading_payload() {
    let mock = mock_source();

    assert!(
        mock.contains("./fixtures/reading-timeline.golden.json"),
        "the mock must import the emitted golden; a hand-written payload that drifts in the safe \
         direction turns a missing test into a passing one (#111, #107)"
    );
    for command in [
        "get_reading_index",
        "get_reading_chapter",
        "get_reading_pages",
        "ensure_reading_pages",
        "get_reading_figures",
    ] {
        assert!(
            mock.contains(command),
            "the mock does not answer {command}, so any e2e that needs it passes vacuously"
        );
    }
}

fn desktop_source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

#[test]
fn the_page_mode_channel_is_spelled_the_same_at_all_three_ends() {
    const COMMAND: &str = "log_page_mode";

    assert!(
        desktop_source("src/readingPageDiagnostics.ts").contains(&format!("\"{COMMAND}\"")),
        "the webview is the only producer of this line"
    );
    assert!(
        desktop_source("src-tauri/src/lib.rs").contains(COMMAND),
        "an invoke the backend does not register throws, and the reporter swallows its own \
         errors, so the diagnostic would go missing silently"
    );
    assert!(
        desktop_source("e2e/tauriMock.ts").contains(&format!("case \"{COMMAND}\":")),
        "an unanswered command throws in the mock too, so any e2e that asserts on the page-mode \
         line would pass vacuously against an empty list (#111, #107)"
    );
}

#[test]
fn the_follow_channels_are_spelled_the_same_at_all_three_ends() {
    for command in ["log_follow_state", "log_reading_tap"] {
        assert!(
            desktop_source("src/readingFollowDiagnostics.ts").contains(&format!("\"{command}\"")),
            "the webview is the only producer of the {command} line"
        );
        assert!(
            desktop_source("src-tauri/src/lib.rs").contains(command),
            "an invoke the backend does not register throws, and the reporter swallows its own \
             errors, so the {command} diagnostic would go missing silently"
        );
        assert!(
            desktop_source("e2e/tauriMock.ts").contains(&format!("case \"{command}\":")),
            "an unanswered command throws in the mock too, so any e2e that asserts on the \
             {command} line would pass vacuously against an empty list (#111, #107)"
        );
    }
}

#[test]
fn the_crash_channel_is_spelled_the_same_at_all_three_ends() {
    const COMMAND: &str = "log_ui_error";

    assert!(
        desktop_source("src/uiCrashDiagnostics.ts").contains(&format!("\"{COMMAND}\"")),
        "the crash reporter is the only producer of a crash line"
    );
    assert!(
        desktop_source("src/ErrorBoundary.tsx").contains("uiCrashDiagnostics"),
        "the boundary must route a crash through the reporter that owns the channel"
    );
    assert!(
        desktop_source("src-tauri/src/lib.rs").contains(COMMAND),
        "an invoke the backend does not register rejects, and the reporter is fire-and-forget, \
         so a crash would leave no trace anywhere"
    );
    assert!(
        desktop_source("e2e/tauriMock.ts").contains(&format!("case \"{COMMAND}\":")),
        "an unanswered command throws in the mock too, so any e2e that forces a crash dies on \
         the unmocked command instead of on the crash (#111, #107)"
    );
}

const CACHE_SENTINEL: &str = "<cache>";

fn reading_handlers(mock: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for command in [
        "get_reading_index",
        "get_reading_chapter",
        "get_reading_pages",
        "ensure_reading_pages",
        "get_reading_figures",
    ] {
        let needle = format!("case \"{command}\":");
        let start = mock
            .find(&needle)
            .unwrap_or_else(|| panic!("the mock does not answer {command}"));
        let rest = &mock[start + needle.len()..];
        let end = rest.find("case \"").unwrap_or(rest.len());
        out.push((command.to_string(), rest[..end].to_string()));
    }
    out
}

#[test]
fn the_e2e_mock_spells_no_payload_field_of_its_own() {
    let mock = mock_source();

    let camel = [
        "startMs",
        "endMs",
        "totalMs",
        "segmentId",
        "chapterId",
        "figureId",
        "pageNo",
        "imagePath",
        "widthPt",
        "heightPt",
        "cropHash",
        "provenance",
        "words",
        "bbox",
    ];
    let projection = mock
        .split("const readingPages")
        .nth(1)
        .expect("the page handlers serve a projection named readingPages");
    let assignment = projection
        .find('=')
        .expect("the projection is assigned something");
    let tail = &projection[assignment..];
    let initialiser = &tail[..tail.find(';').expect("the projection is initialised")];
    assert!(
        initialiser.contains("config.reading?.pages"),
        "the projection the page handlers serve must be seeded from the imported golden. Naming \
         it is not enough: an initialiser that builds rows of its own is exactly the second \
         payload #111 and #107 were made of"
    );
    assert!(
        !initialiser.contains(CACHE_SENTINEL),
        "the projection's initialiser writes a '{CACHE_SENTINEL}' path of its own, which is the \
         same second payload the handlers are forbidden to author"
    );
    for field in camel {
        for spelling in [
            format!("{field}:"),
            format!("\"{field}\""),
            format!("'{field}'"),
            format!("`{field}`"),
        ] {
            assert!(
                !initialiser.contains(&spelling),
                "the projection's initialiser authors '{spelling}'. Seeding from the golden means \
                 copying its rows, not restating their shape"
            );
        }
    }
    for (command, body) in reading_handlers(&mock) {
        assert!(
            !body.contains(CACHE_SENTINEL),
            "{command} writes a '{CACHE_SENTINEL}' path of its own. The golden already carries \
             the path every page gets once rendered, so composing one here is a second payload \
             that drifts in the safe direction exactly like #111 and #107"
        );
        assert!(
            body.contains("config.reading") || body.contains("readingPages"),
            "{command} must answer from the imported golden, directly or through the projection \
             seeded from it; anything else is a second payload that can drift in the safe \
             direction (#111, #107)"
        );
        for field in camel {
            for spelling in [
                format!("{field}:"),
                format!("\"{field}\""),
                format!("'{field}'"),
                format!("`{field}`"),
            ] {
                assert!(
                    !body.contains(&spelling),
                    "{command} authors '{spelling}' instead of serving the golden's own row. A \
                     plain key, a quoted key and a computed key like [\"{field}\"] are all the \
                     same defect: reading a field is fine, writing one is the drift #111 and \
                     #107 were made of"
                );
            }
        }
    }

    let snake = [
        "start_ms",
        "end_ms",
        "total_ms",
        "segment_id",
        "chapter_id",
        "figure_id",
        "page_no",
        "image_path",
        "width_pt",
        "height_pt",
        "crop_hash",
    ];
    for field in snake {
        assert!(
            !mock.contains(field),
            "the mock mentions '{field}', but the reading payload is camelCase on the wire; a \
             snake_case spelling means a hand-authored Rust-shaped payload (#107)"
        );
    }
}

#[test]
fn the_e2e_mock_refuses_a_stale_export_with_the_one_message() {
    let mock = mock_source();

    assert!(
        mock.contains(txtcaster_lib::STALE_EXPORT_MESSAGE),
        "get_media_url refuses a stale export with this exact message and the view renders it \
         verbatim, so both ends must read one literal (#118)"
    );
    assert!(
        mock.contains("__mockMarkExportStale"),
        "a one-shot staleOnNextRender flag cannot express 'fresh at open, stale later', so the \
         mid-session arm of the stale-export spec needs this hook or it is untestable"
    );
}

#[test]
fn the_e2e_mock_refuses_an_oversized_page_request_in_the_same_words_as_the_backend() {
    let mock = mock_source();
    let refusal =
        txtcaster_lib::reading::refuse_oversized(txtcaster_lib::reading::MAX_REQUESTED_ITEMS + 1)
            .expect("the backend refuses past the cap");
    let tail = refusal
        .split_once("; ")
        .map(|(_, rest)| rest)
        .unwrap_or(&refusal);

    assert!(
        mock.contains(tail),
        "the mock states its own refusal, so a reworded backend message leaves the e2e asserting \
         a sentence the product no longer says. Share the tail or the drift is silent: {tail}"
    );
    assert!(
        mock.contains(&format!(
            "> {}",
            txtcaster_lib::reading::MAX_REQUESTED_ITEMS
        )),
        "the mock must refuse at the same cap the backend uses"
    );
}

#[test]
fn the_e2e_mock_can_tell_a_cached_page_from_an_evicted_one() {
    let mock = mock_source();

    assert!(
        mock.contains("data:image/gif;base64,"),
        "convertFileSrc must return something the webview can actually load for a cached page, or \
         the placeholder spec cannot tell a hit from a miss"
    );
    assert!(
        mock.contains("brokenPages"),
        "without a way to declare a page whose image fails, the single onError retry has no test"
    );
}

#[tokio::test]
async fn reading_a_page_keeps_only_that_page_warm_in_the_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    seed(&pool).await;
    let cache = dir.path().join("cache");
    for page_no in RENDERED_PAGES {
        let path = cache_path::cache_path(&cache, &page_hash(page_no));
        std::fs::create_dir_all(path.parent().expect("shard")).expect("shard dir");
        std::fs::write(&path, b"png").expect("page image");
    }
    let stale = now_ms() - 86_400_000;
    for page_no in RENDERED_PAGES {
        storage::cache::upsert_entry(&pool, &page_hash(page_no), "page_png", 3)
            .await
            .expect("entry");
    }
    storage::cache::upsert_entry(&pool, "unrelated-wav", "wav", 11)
        .await
        .expect("entry");
    for hash in [page_hash(1), page_hash(4), "unrelated-wav".to_string()] {
        sqlx::query("UPDATE cache_entry SET last_used_at = ? WHERE hash = ?")
            .bind(stale)
            .bind(&hash)
            .execute(&pool)
            .await
            .expect("age the entry");
    }

    let pages = reading::pages(&pool, BOOK_ID, &[1], &cache)
        .await
        .expect("pages");
    assert_eq!(pages.len(), 1);
    assert!(pages[0].image_path.is_some(), "page 1 is in the cache");

    let read = storage::cache::get_entry(&pool, &page_hash(1))
        .await
        .expect("entry")
        .expect("page 1 entry");
    let untouched_page = storage::cache::get_entry(&pool, &page_hash(4))
        .await
        .expect("entry")
        .expect("page 4 entry");
    let untouched_wav = storage::cache::get_entry(&pool, "unrelated-wav")
        .await
        .expect("entry")
        .expect("wav entry");

    assert!(
        read.last_used_at > stale,
        "a page the reader just displayed must not stay at the head of the eviction queue, or \
         the pages in use are the first ones evicted"
    );
    assert_eq!(
        untouched_page.last_used_at, stale,
        "touching every page instead of the one that was read would defeat the ordering the \
         eviction pass depends on"
    );
    assert_eq!(
        untouched_wav.last_used_at, stale,
        "reading a page must not disturb artifacts of another kind"
    );
}

#[tokio::test]
async fn an_unrendered_page_is_the_one_the_reader_asks_the_engine_for() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    seed(&pool).await;
    let cache = dir.path().join("cache");
    let path = cache_path::cache_path(&cache, &page_hash(1));
    std::fs::create_dir_all(path.parent().expect("shard")).expect("shard dir");
    std::fs::write(&path, b"png").expect("page image");

    let pages = reading::pages(&pool, BOOK_ID, &[1, 2, 4], &cache)
        .await
        .expect("pages");

    assert_eq!(
        reading::pages_to_render(&pages),
        vec![2, 4],
        "page 2 has no render hash and page 4's file was evicted; both are work for the engine, \
         and a predicate that only reads the hash would silently skip the evicted one"
    );
}

#[tokio::test]
async fn the_golden_export_alignment_matches_the_wire_schema() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    seed(&pool).await;
    let cache = dir.path().join("cache");

    let export = reading::build_export_alignment(&pool, JOB_ID, &cache)
        .await
        .expect("export alignment");

    assert_eq!(export["origin"], "measured");
    assert_eq!(export["totalMs"], ENCODED_FIRST + ENCODED_SECOND);
    assert_eq!(export["order"], serde_json::json!(["chap-0", "chap-1"]));

    let segments = export["segments"].as_array().expect("segments array");
    assert_eq!(segments.len(), 6);

    assert_eq!(segments[0]["segmentId"], "seg-0000");
    assert_eq!(segments[0]["idx"], 0);
    assert_eq!(segments[0]["chapterId"], "chap-0");
    assert_eq!(segments[0]["pageNo"], 1);
    assert_eq!(segments[0]["kind"], "text");
    assert_eq!(segments[0]["figureId"], serde_json::Value::Null);
    assert_eq!(segments[0]["text"], "The first sentence reads aloud.");
    assert_eq!(segments[0]["startMs"], 0);
    assert_eq!(segments[0]["endMs"], 900);
    assert_eq!(segments[0]["provenance"], "fresh");

    let words0 = segments[0]["words"].as_array().expect("words array");
    assert_eq!(words0.len(), 4);
    assert_eq!(words0[0]["w"], "The");
    assert_eq!(words0[0]["startMs"], 0);
    assert_eq!(words0[0]["endMs"], 180);
    assert_eq!(words0[1]["w"], "first");
    assert_eq!(words0[1]["startMs"], 200);
    assert_eq!(words0[1]["endMs"], 430);
    assert_eq!(words0[2]["w"], "sentence");
    assert_eq!(words0[2]["startMs"], 450);
    assert_eq!(words0[2]["endMs"], 880);
    assert_eq!(words0[3]["w"], "aloud");
    assert_eq!(words0[3]["startMs"], 890);
    assert_eq!(words0[3]["endMs"], 900);

    assert_eq!(segments[1]["segmentId"], "seg-0001");
    assert_eq!(segments[1]["provenance"], "stale");
    assert_eq!(segments[1]["words"], serde_json::json!([]));

    let ch1_offset = ENCODED_FIRST + CHAPTER_GAP_MS;
    assert_eq!(segments[2]["segmentId"], "seg-0002");
    assert_eq!(segments[2]["chapterId"], "chap-1");
    assert_eq!(segments[2]["startMs"], ch1_offset);
    assert_eq!(segments[2]["endMs"], ch1_offset + SEGMENT_MS);
    assert_eq!(segments[2]["figureId"], "figure-7");
    assert_eq!(segments[2]["provenance"], "fresh");
    let words2 = segments[2]["words"].as_array().expect("words array");
    assert_eq!(words2.len(), 2);
    assert_eq!(words2[0]["w"], "A");
    assert_eq!(words2[0]["startMs"], ch1_offset);
    assert_eq!(words2[0]["endMs"], ch1_offset + 120);
}
