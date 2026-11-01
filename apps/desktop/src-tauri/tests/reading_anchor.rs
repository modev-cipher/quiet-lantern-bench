use txtcaster_lib::reading::anchor::Rung;
use txtcaster_lib::storage::{self, book, job, now_ms, voice, SqlitePool};
use txtcaster_lib::words::{Timebase, WordsJson};

const BOOK_ID: &str = "book-anchor";
const JOB_ID: &str = "job-anchor";

fn geometry_json() -> String {
    serde_json::json!({
        "v": 1,
        "pageWidthPt": 612.0,
        "pageHeightPt": 792.0,
        "lines": [
            [72.0, 100.0, 540.0, 112.0],
            [72.0, 300.0, 540.0, 312.0]
        ],
        "words": [
            [72.0, 100.0, 120.0, 112.0, "systems", 0],
            [124.0, 100.0, 160.0, 112.0, "scale", 0],
            [72.0, 300.0, 120.0, 312.0, "systems", 1],
            [124.0, 300.0, 160.0, 312.0, "fail", 1]
        ]
    })
    .to_string()
}

fn text_json() -> String {
    serde_json::json!({
        "quality": {"source": "pymupdf", "multi_column": false, "low_confidence": false},
        "blocks": [
            {"bbox": [70.0, 96.0, 545.0, 116.0], "text": "systems scale",
             "font_size": 11.0, "reading_order": 0, "column": 0},
            {"bbox": [70.0, 296.0, 545.0, 316.0], "text": "systems\nfail",
             "font_size": 11.0, "reading_order": 1, "column": 0}
        ]
    })
    .to_string()
}

async fn seed(pool: &SqlitePool, span: Option<&str>) {
    book::insert_book(
        pool,
        &book::Book {
            id: BOOK_ID.into(),
            title: "Anchored".into(),
            author: None,
            source_path: "/books/anchored.pdf".into(),
            format: "pdf".into(),
            page_count: Some(1),
            language: Some("en".into()),
            is_scanned: false,
            ingest_path: Some("fast".into()),
            cover_path: None,
            content_hash: "anchor-content-hash".into(),
            blocks_version: Some("v1".into()),
            blocks_page_count: Some(1),
            layout_analysed: true,
            reflowed: false,
            extracting_since: None,
            created_at: now_ms(),
        },
    )
    .await
    .expect("book");
    book::insert_chapter(
        pool,
        &book::Chapter {
            id: "chap-0".into(),
            book_id: BOOK_ID.into(),
            idx: 0,
            title: "One".into(),
            page_start: 1,
            page_end: 1,
            is_pseudo: false,
            word_count: Some(4),
        },
    )
    .await
    .expect("chapter");
    book::insert_page(
        pool,
        &book::Page {
            id: "page-0001".into(),
            book_id: BOOK_ID.into(),
            page_no: 1,
            width_pt: Some(612.0),
            height_pt: Some(792.0),
            render_hash: Some("page-hash-1".into()),
            text_json: Some(text_json()),
            needs_upgrade: false,
        },
    )
    .await
    .expect("page");
    book::upsert_page_geometry(
        pool,
        &book::PageGeometry {
            page_id: "page-0001".into(),
            geometry_version: "v1|geom=1".into(),
            geometry_json: geometry_json(),
        },
    )
    .await
    .expect("geometry");
    voice::insert_voice_profile(
        pool,
        &voice::VoiceProfile {
            id: "voice-anchor".into(),
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
            scope_json: r#"{"kind":"pages","page_start":1,"page_end":1}"#.into(),
            mode: "narrator".into(),
            voice_profile_id: "voice-anchor".into(),
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
    job::insert_segments(
        pool,
        &[job::ScriptSegment {
            id: "seg-0000".into(),
            job_id: JOB_ID.into(),
            chapter_id: "chap-0".into(),
            idx: 0,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "Systems fail.".into(),
            source_span_json: span.map(str::to_string),
            flags_json: None,
            audio_hash: Some("hash-0000".into()),
            duration_ms: Some(1000),
            original_text: None,
        }],
    )
    .await
    .expect("segments");
    job::upsert_alignment(
        pool,
        &job::Alignment {
            segment_id: "seg-0000".into(),
            source: "tts_native".into(),
            words_json: WordsJson::declare(
                &[serde_json::json!({"w": "systems", "s": 0, "e": 400})],
                Timebase::Trimmed,
            )
            .expect("declare"),
            wer: Some(0.0),
        },
    )
    .await
    .expect("alignment");
}

async fn open(dir: &tempfile::TempDir) -> SqlitePool {
    storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool")
}

#[tokio::test]
async fn a_stored_block_index_anchors_the_word_to_its_own_block_not_the_first_match_on_the_page() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(
        &pool,
        Some(r#"{"char_start":0,"char_end":13,"page":1,"block_index":1}"#),
    )
    .await;

    let chapter =
        txtcaster_lib::reading::chapter(&pool, JOB_ID, "chap-0", dir.path(), |_b, _p| async {
            txtcaster_lib::reading::GeometryOutcome::Unavailable
        })
        .await
        .expect("chapter");
    let segment = &chapter.segments[0];

    assert_eq!(segment.anchor, Rung::Word);
    let rect = segment.words[0]
        .rect
        .expect("the stored block index resolved a rect");
    assert_eq!(
        (rect.x0, rect.y0),
        (72.0, 300.0),
        "block_index=1 is the second block; 'systems' also appears in block 0 at y=100, and an \
         unscoped walk would have taken that one. This is the end-to-end proof of the STORED \
         path — every production row predates the writer, so nothing else exercises it"
    );
}

#[tokio::test]
async fn a_span_without_a_block_index_still_anchors_page_scoped_so_old_rows_keep_working() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(&pool, Some(r#"{"char_start":0,"char_end":13,"page":1}"#)).await;

    let chapter =
        txtcaster_lib::reading::chapter(&pool, JOB_ID, "chap-0", dir.path(), |_b, _p| async {
            txtcaster_lib::reading::GeometryOutcome::Unavailable
        })
        .await
        .expect("chapter");
    let segment = &chapter.segments[0];

    assert_eq!(segment.anchor, Rung::Word);
    let rect = segment.words[0]
        .rect
        .expect("a page-scoped walk still resolves");
    assert_eq!(
        (rect.x0, rect.y0),
        (72.0, 100.0),
        "page-scoped takes the first occurrence, which is the honest answer when the producer \
         declared no block; §9.2 measured these page-fallback segments at 0.7059, which is why \
         block_index is a position-correctness win rather than a rate win"
    );
}

#[tokio::test]
async fn a_segment_with_no_span_at_all_is_not_an_error() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(&pool, None).await;

    let chapter =
        txtcaster_lib::reading::chapter(&pool, JOB_ID, "chap-0", dir.path(), |_b, _p| async {
            txtcaster_lib::reading::GeometryOutcome::Unavailable
        })
        .await
        .expect("chapter");

    assert_eq!(chapter.segments[0].anchor, Rung::Word);
}

#[tokio::test]
async fn an_unavailable_engine_degrades_the_rungs_and_still_returns_the_chapter() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(
        &pool,
        Some(r#"{"char_start":0,"char_end":13,"page":1,"block_index":1}"#),
    )
    .await;
    sqlx::query("DELETE FROM page_geometry")
        .execute(&pool)
        .await
        .expect("strip geometry");

    let chapter =
        txtcaster_lib::reading::chapter(&pool, JOB_ID, "chap-0", dir.path(), |_b, _p| async {
            txtcaster_lib::reading::GeometryOutcome::Unavailable
        })
        .await
        .expect("an engine that is down must degrade the highlight, never refuse the book");

    assert_eq!(chapter.segments.len(), 1);
    assert_eq!(
        chapter.segments[0].anchor,
        Rung::Block,
        "no geometry means no word or line evidence; the block is still known from text_json, \
         so the reader still gets 'the narration is discussing this passage'"
    );
    assert!(chapter.segments[0].words[0].rect.is_none());
}

#[tokio::test]
async fn the_ensure_runs_before_the_anchor_walk_not_after_it() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(
        &pool,
        Some(r#"{"char_start":0,"char_end":13,"page":1,"block_index":1}"#),
    )
    .await;
    sqlx::query("DELETE FROM page_geometry")
        .execute(&pool)
        .await
        .expect("strip geometry");

    let restored = geometry_json();
    let pool_ref = &pool;
    let chapter =
        txtcaster_lib::reading::chapter(&pool, JOB_ID, "chap-0", dir.path(), move |_b, pages| {
            let restored = restored.clone();
            async move {
                assert_eq!(
                    pages,
                    vec![1],
                    "the ensure is asked for the chapter's own pages"
                );
                book::upsert_page_geometry(
                    pool_ref,
                    &book::PageGeometry {
                        page_id: "page-0001".into(),
                        geometry_version: "v1|geom=1".into(),
                        geometry_json: restored,
                    },
                )
                .await
                .expect("late geometry");
                txtcaster_lib::reading::GeometryOutcome::Ensured
            }
        })
        .await
        .expect("chapter");

    assert_eq!(
        chapter.segments[0].anchor,
        Rung::Word,
        "geometry that lands during the ensure must be visible to the walk; if the surfaces \
         were read first this would still be Block, which is the 0.1112 defect the real app \
         showed on a first open"
    );
}

use std::sync::{Arc, Mutex};

async fn seed_pages(pool: &SqlitePool, upto: i64) {
    for page_no in 2..=upto {
        book::insert_page(
            pool,
            &book::Page {
                id: format!("page-{page_no:04}"),
                book_id: BOOK_ID.into(),
                page_no,
                width_pt: Some(612.0),
                height_pt: Some(792.0),
                render_hash: None,
                text_json: Some(text_json()),
                needs_upgrade: false,
            },
        )
        .await
        .expect("page");
    }
}

#[tokio::test]
async fn the_production_ensure_batches_the_pages_under_the_request_bound() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(&pool, None).await;
    let over = (txtcaster_lib::reading::MAX_REQUESTED_ITEMS + 5) as i64;
    seed_pages(&pool, over).await;
    let pages: Vec<i64> = (1..=over).collect();

    let seen: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    let report =
        txtcaster_lib::reading::ensure_geometry_for_chapter(&pool, BOOK_ID, &pages, move |batch| {
            let observed = observed.clone();
            async move {
                observed.lock().expect("lock").push(batch.len());
                Ok(())
            }
        })
        .await;

    let sizes = seen.lock().expect("lock").clone();
    assert_eq!(report.batches, sizes.len());
    assert!(
        report.batches > 1,
        "{over} pages against a bound of {} must not be one request - refuse_oversized exists \
         to stop exactly that, and the chapter path used to walk around it",
        txtcaster_lib::reading::MAX_REQUESTED_ITEMS
    );
    assert!(sizes
        .iter()
        .all(|size| *size <= txtcaster_lib::reading::MAX_REQUESTED_ITEMS));
    assert_eq!(
        sizes.iter().sum::<usize>(),
        pages.len(),
        "batching must not drop pages: every page the chapter touches still needs geometry"
    );
}

#[tokio::test]
async fn the_ensure_outcome_is_derived_from_coverage_not_from_the_calls_returning_ok() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(&pool, None).await;
    sqlx::query("DELETE FROM page_geometry")
        .execute(&pool)
        .await
        .expect("strip geometry");

    let report =
        txtcaster_lib::reading::ensure_geometry_for_chapter(&pool, BOOK_ID, &[1], |_| async {
            Ok(())
        })
        .await;

    assert_eq!(report.covered, 0);
    assert!(
        !report.ensured,
        "ensure_pages_inner answers Ok on the engine-down and render-failed arms too, so a \
         status read off the return value reports geometry-ensured while the walk runs against \
         nothing - the log line this PR's own LOG REVIEW depends on would be lying"
    );
    assert!(report
        .log_line(BOOK_ID)
        .contains("outcome=geometry-unavailable"));
}

#[tokio::test]
async fn the_ensure_reports_ensured_only_when_every_page_really_carries_geometry() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(&pool, None).await;
    seed_pages(&pool, 2).await;

    let report =
        txtcaster_lib::reading::ensure_geometry_for_chapter(&pool, BOOK_ID, &[1, 2], |_| async {
            Ok(())
        })
        .await;

    assert_eq!((report.covered, report.pages), (1, 2));
    assert!(!report.ensured, "page 2 has a row but no geometry");
    assert!(report.log_line(BOOK_ID).contains("pages=2"));

    book::upsert_page_geometry(
        &pool,
        &book::PageGeometry {
            page_id: "page-0002".into(),
            geometry_version: "v1|geom=1".into(),
            geometry_json: geometry_json(),
        },
    )
    .await
    .expect("geometry");
    let after =
        txtcaster_lib::reading::ensure_geometry_for_chapter(&pool, BOOK_ID, &[1, 2], |_| async {
            Ok(())
        })
        .await;
    assert!(after.ensured);
    assert!(after.log_line(BOOK_ID).contains("outcome=geometry-ensured"));
}

#[tokio::test]
async fn a_batch_that_fails_denies_the_ensured_outcome_even_if_coverage_is_complete() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(&pool, None).await;

    let report =
        txtcaster_lib::reading::ensure_geometry_for_chapter(&pool, BOOK_ID, &[1], |_| async {
            Err("sidecar request failed".to_string())
        })
        .await;

    assert_eq!(report.covered, 1, "page 1's geometry was already there");
    assert!(
        !report.ensured,
        "coverage alone is not the whole predicate: a failed batch means some page the reader \
         will turn to was not attempted, and the line must not claim otherwise"
    );
}

#[tokio::test]
async fn a_chapter_with_more_pages_than_the_request_bound_is_batched_not_refused() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(&pool, Some(r#"{"char_start":0,"char_end":13,"page":1}"#)).await;

    let over = txtcaster_lib::reading::MAX_REQUESTED_ITEMS + 5;
    for page_no in 2..=over as i64 {
        book::insert_page(
            &pool,
            &book::Page {
                id: format!("page-{page_no:04}"),
                book_id: BOOK_ID.into(),
                page_no,
                width_pt: Some(612.0),
                height_pt: Some(792.0),
                render_hash: None,
                text_json: Some(text_json()),
                needs_upgrade: false,
            },
        )
        .await
        .expect("page");
    }
    let extra: Vec<job::ScriptSegment> = (2..=over as i64)
        .map(|page_no| job::ScriptSegment {
            id: format!("seg-{page_no:04}"),
            job_id: JOB_ID.into(),
            chapter_id: "chap-0".into(),
            idx: page_no,
            page_no,
            kind: "text".into(),
            figure_id: None,
            text: "Systems fail.".into(),
            source_span_json: Some(r#"{"char_start":0,"char_end":13,"page":1}"#.into()),
            flags_json: None,
            audio_hash: Some(format!("hash-{page_no:04}")),
            duration_ms: Some(1000),
            original_text: None,
        })
        .collect();
    job::insert_segments(&pool, &extra).await.expect("segments");

    let batches: std::sync::Arc<std::sync::Mutex<Vec<usize>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = batches.clone();
    let chapter =
        txtcaster_lib::reading::chapter(&pool, JOB_ID, "chap-0", dir.path(), move |_b, pages| {
            let seen = seen.clone();
            async move {
                for batch in pages.chunks(txtcaster_lib::reading::MAX_REQUESTED_ITEMS) {
                    seen.lock().expect("lock").push(batch.len());
                }
                txtcaster_lib::reading::GeometryOutcome::Ensured
            }
        })
        .await
        .expect("a long chapter must open, not be refused");

    let observed = batches.lock().expect("lock").clone();
    assert!(
        observed.len() > 1,
        "the chapter has {over} pages against a bound of {}; one oversized render request is \
         exactly what refuse_oversized exists to prevent, so the ensure must batch",
        txtcaster_lib::reading::MAX_REQUESTED_ITEMS
    );
    assert!(observed
        .iter()
        .all(|size| *size <= txtcaster_lib::reading::MAX_REQUESTED_ITEMS));
    assert_eq!(
        observed.iter().sum::<usize>(),
        over,
        "batching must not drop pages: every page the chapter touches still needs geometry"
    );
    assert_eq!(chapter.segments.len(), over);
}

#[tokio::test]
async fn the_payload_says_whether_its_anchors_were_computed_against_complete_geometry() {
    let dir = tempfile::tempdir().expect("tmp");
    let pool = open(&dir).await;
    seed(&pool, None).await;

    let degraded =
        txtcaster_lib::reading::chapter(&pool, JOB_ID, "chap-0", dir.path(), |_b, _p| async {
            txtcaster_lib::reading::GeometryOutcome::Unavailable
        })
        .await
        .expect("chapter");
    let complete =
        txtcaster_lib::reading::chapter(&pool, JOB_ID, "chap-0", dir.path(), |_b, _p| async {
            txtcaster_lib::reading::GeometryOutcome::Ensured
        })
        .await
        .expect("chapter");

    assert_eq!(
        degraded.anchor_geometry,
        txtcaster_lib::reading::GeometryOutcome::Unavailable
    );
    assert_eq!(
        complete.anchor_geometry,
        txtcaster_lib::reading::GeometryOutcome::Ensured,
        "readingChapterCache freezes this payload for the session. Freezing anchors computed \
         against complete geometry is correct; freezing a degraded first open taken while the \
         engine was down is a silent downgrade the reader can never escape, and 1c cannot know \
         to refresh unless the payload says which one it holds"
    );
}
