use std::path::Path;

use txtcaster_lib::reading::{self, TimelineOrigin};
use txtcaster_lib::render::mp3::{CHAPTER_GAP_MS, SEGMENT_GAP_MS};
use txtcaster_lib::storage::{self, book, chapter_render, job, new_id, now_ms, voice, SqlitePool};
use txtcaster_lib::words::{Timebase, WordProvenance, WordsJson};

const SEGMENT_MS: i64 = 900;
const CHAPTERS: usize = 6;
const SEGMENTS_PER_CHAPTER: usize = 3;

struct Fixture {
    job_id: String,
    chapter_ids: Vec<String>,
    segment_ids: Vec<String>,
}

async fn pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    (dir, pool)
}

async fn seed(pool: &SqlitePool) -> Fixture {
    let b = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(CHAPTERS as i64 * 2),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: Some("fast".into()),
        cover_path: None,
        content_hash: new_id(),
        blocks_version: None,
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    book::insert_book(pool, &b).await.expect("book");
    let mut chapter_ids = Vec::new();
    for index in 0..CHAPTERS {
        let chapter = book::Chapter {
            id: format!("chap-{index}"),
            book_id: b.id.clone(),
            idx: index as i64,
            title: format!("Chapter {index}"),
            page_start: index as i64 * 2 + 1,
            page_end: index as i64 * 2 + 2,
            is_pseudo: false,
            word_count: Some(30),
        };
        book::insert_chapter(pool, &chapter).await.expect("chapter");
        chapter_ids.push(chapter.id);
    }
    let profile = voice::VoiceProfile {
        id: new_id(),
        name: "Default".into(),
        engine: "kokoro-82m".into(),
        kind: "preset".into(),
        preset_voice_id: Some("af_heart".into()),
        reference_wav_path: None,
        params_json: None,
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &profile)
        .await
        .expect("voice");
    let record = job::RenderJob {
        id: new_id(),
        book_id: b.id.clone(),
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":12}"#.into(),
        mode: "narrator".into(),
        voice_profile_id: profile.id,
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
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(pool, &record).await.expect("job");

    let mut segments = Vec::new();
    let mut segment_ids = Vec::new();
    for (chapter_index, chapter_id) in chapter_ids.iter().enumerate() {
        for position in 0..SEGMENTS_PER_CHAPTER {
            let idx = (chapter_index * SEGMENTS_PER_CHAPTER + position) as i64;
            let id = format!("seg-{idx:04}");
            segment_ids.push(id.clone());
            segments.push(job::ScriptSegment {
                id,
                job_id: record.id.clone(),
                chapter_id: chapter_id.clone(),
                idx,
                page_no: chapter_index as i64 * 2 + 1,
                kind: "text".into(),
                figure_id: None,
                text: format!("Sentence number {idx} reads aloud."),
                source_span_json: None,
                flags_json: None,
                audio_hash: Some(format!("hash{idx:04}")),
                duration_ms: Some(SEGMENT_MS),
                original_text: None,
            });
        }
    }
    job::insert_segments(pool, &segments)
        .await
        .expect("segments");

    Fixture {
        job_id: record.id,
        chapter_ids,
        segment_ids,
    }
}

fn words(count: usize) -> Vec<serde_json::Value> {
    (0..count)
        .map(|index| {
            let start = index as i64 * 300;
            serde_json::json!({"w": format!("w{index}"), "s": start, "e": start + 250})
        })
        .collect()
}

async fn declare_words(pool: &SqlitePool, segment_id: &str, entries: &[serde_json::Value]) {
    job::upsert_alignment(
        pool,
        &job::Alignment {
            segment_id: segment_id.to_string(),
            source: "tts_native".into(),
            words_json: WordsJson::declare(entries, Timebase::Trimmed).expect("declare"),
            wer: None,
        },
    )
    .await
    .expect("alignment");
}

async fn publish_rows(pool: &SqlitePool, fixture: &Fixture, encoded: &[Option<i64>]) {
    let rows: Vec<chapter_render::ChapterRender> = fixture
        .chapter_ids
        .iter()
        .enumerate()
        .map(|(index, chapter_id)| chapter_render::ChapterRender {
            job_id: fixture.job_id.clone(),
            chapter_id: chapter_id.clone(),
            idx: index as i64,
            hash: format!("hash-{index}"),
            artifact: if encoded[index].is_some() {
                chapter_render::ARTIFACT_MP3.into()
            } else {
                chapter_render::ARTIFACT_NONE.into()
            },
            duration_ms: nominal_chapter_ms(index),
            encoded_ms: encoded[index],
        })
        .collect();
    chapter_render::replace_all(pool, &fixture.job_id, &rows)
        .await
        .expect("rows");
}

fn nominal_chapter_ms(index: usize) -> i64 {
    let body = SEGMENTS_PER_CHAPTER as i64 * SEGMENT_MS
        + SEGMENT_GAP_MS * (SEGMENTS_PER_CHAPTER as i64 - 1);
    if index == 0 {
        body
    } else {
        body + CHAPTER_GAP_MS
    }
}

fn cache() -> &'static Path {
    Path::new("/nonexistent-cache")
}

#[tokio::test]
async fn measured_chapters_move_every_word_onto_the_audio_that_was_encoded() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;
    for segment_id in &fixture.segment_ids {
        declare_words(&pool, segment_id, &words(3)).await;
    }
    let encoded: Vec<Option<i64>> = (0..CHAPTERS)
        .map(|index| Some(nominal_chapter_ms(index) + 52 + 4 * index as i64))
        .collect();
    publish_rows(&pool, &fixture, &encoded).await;

    let index = reading::index(&pool, &fixture.job_id, cache())
        .await
        .expect("index");
    assert_eq!(index.origin, TimelineOrigin::Measured);
    assert_eq!(index.chapters.len(), CHAPTERS);

    let mut cursor = 0i64;
    for (position, summary) in index.chapters.iter().enumerate() {
        let leading = if position == 0 { 0 } else { CHAPTER_GAP_MS };
        assert_eq!(
            summary.start_ms,
            cursor + leading,
            "chapter {position} must start where the encoded audio puts its speech"
        );
        cursor += encoded[position].expect("measured");
    }

    let last = index.chapters.last().expect("chapters");
    let chapter = reading::chapter(
        &pool,
        &fixture.job_id,
        &last.chapter_id,
        cache(),
        |_b, _p| async { txtcaster_lib::reading::GeometryOutcome::Unavailable },
    )
    .await
    .expect("chapter");
    let first = chapter.segments.first().expect("segments");
    assert_eq!(first.start_ms, last.start_ms);
    assert_eq!(
        first.words.first().expect("words").start_ms,
        last.start_ms,
        "a word onset is its segment's measured start plus its own offset, so the first word of \
         the final chapter lands exactly on that chapter's measured speech start"
    );
    assert_eq!(first.provenance, WordProvenance::Fresh);
}

#[tokio::test]
async fn a_dynamic_render_keeps_the_nominal_layout_the_export_actually_used() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;
    publish_rows(&pool, &fixture, &[None; CHAPTERS]).await;

    let index = reading::index(&pool, &fixture.job_id, cache())
        .await
        .expect("index");
    assert_eq!(
        index.origin,
        TimelineOrigin::Dynamic,
        "no chapter row carries encoded_ms because the export was one dynamic encode, so the \
         nominal geometry is the geometry the file actually has"
    );
    let mut cursor = 0i64;
    for (position, summary) in index.chapters.iter().enumerate() {
        let leading = if position == 0 { 0 } else { CHAPTER_GAP_MS };
        assert_eq!(summary.start_ms, cursor + leading);
        cursor += nominal_chapter_ms(position);
    }
}

#[tokio::test]
async fn one_unmeasured_chapter_sends_the_whole_book_back_to_nominal() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;
    let mut encoded: Vec<Option<i64>> = (0..CHAPTERS)
        .map(|index| Some(nominal_chapter_ms(index) + 52))
        .collect();
    encoded[3] = None;
    publish_rows(&pool, &fixture, &encoded).await;

    let index = reading::index(&pool, &fixture.job_id, cache())
        .await
        .expect("index");
    assert_eq!(
        index.origin,
        TimelineOrigin::Unverified,
        "half a measured book is two timebases in one file; the nominal basis is at least \
         consistent with what a dynamic export contains, and nothing measured it"
    );
    assert_eq!(
        index.chapters[1].start_ms,
        nominal_chapter_ms(0) + CHAPTER_GAP_MS
    );
}

#[tokio::test]
async fn a_chapter_whose_row_an_edit_deleted_sends_the_book_back_to_nominal() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;
    let rows: Vec<chapter_render::ChapterRender> = fixture
        .chapter_ids
        .iter()
        .enumerate()
        .skip(1)
        .map(|(index, chapter_id)| chapter_render::ChapterRender {
            job_id: fixture.job_id.clone(),
            chapter_id: chapter_id.clone(),
            idx: index as i64,
            hash: format!("hash-{index}"),
            artifact: chapter_render::ARTIFACT_MP3.into(),
            duration_ms: nominal_chapter_ms(index),
            encoded_ms: Some(nominal_chapter_ms(index) + 52),
        })
        .collect();
    chapter_render::replace_all(&pool, &fixture.job_id, &rows)
        .await
        .expect("rows");

    let index = reading::index(&pool, &fixture.job_id, cache())
        .await
        .expect("an edit that dropped a chapter row must not be read as a reordering");
    assert_eq!(
        index.origin,
        TimelineOrigin::Unverified,
        "editing a segment deletes that chapter's render row until the re-render lands, so a \
         missing row is the normal mid-edit state, not a corrupt one, and not a measured one"
    );
    let expected: i64 = (0..CHAPTERS).map(nominal_chapter_ms).sum();
    assert_eq!(index.total_ms, expected);
    let last = index.chapters.last().expect("chapters");
    assert!(
        index.total_ms >= last.end_ms,
        "a timeline whose total is shorter than its last chapter's end cannot represent the \
         tail of the book to a seek bar: total {} vs end {}",
        index.total_ms,
        last.end_ms
    );
}

#[tokio::test]
async fn a_row_deleted_from_the_middle_of_the_book_is_not_a_reordering() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;
    let rows: Vec<chapter_render::ChapterRender> = fixture
        .chapter_ids
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 3)
        .map(|(index, chapter_id)| chapter_render::ChapterRender {
            job_id: fixture.job_id.clone(),
            chapter_id: chapter_id.clone(),
            idx: index as i64,
            hash: format!("hash-{index}"),
            artifact: chapter_render::ARTIFACT_MP3.into(),
            duration_ms: nominal_chapter_ms(index),
            encoded_ms: Some(nominal_chapter_ms(index) + 52),
        })
        .collect();
    chapter_render::replace_all(&pool, &fixture.job_id, &rows)
        .await
        .expect("rows");

    let index = reading::index(&pool, &fixture.job_id, cache())
        .await
        .expect("a hole in the middle is a missing row, not a reordered one");
    assert_eq!(
        index.origin,
        TimelineOrigin::Unverified,
        "a hole in the row set means the measured starts cannot be composed, so the layout is \
         nominal and unverified rather than dynamic"
    );
    let mut cursor = 0i64;
    for (position, summary) in index.chapters.iter().enumerate() {
        let leading = if position == 0 { 0 } else { CHAPTER_GAP_MS };
        assert_eq!(summary.start_ms, cursor + leading);
        cursor += nominal_chapter_ms(position);
    }
}

#[tokio::test]
async fn a_job_rendered_before_chapter_rows_existed_reads_as_legacy() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;

    let index = reading::index(&pool, &fixture.job_id, cache())
        .await
        .expect("index");
    assert_eq!(index.origin, TimelineOrigin::Legacy);
    assert_eq!(index.chapters.len(), CHAPTERS);
    let expected: i64 = (0..CHAPTERS).map(nominal_chapter_ms).sum();
    assert_eq!(index.total_ms, expected);
}

#[tokio::test]
async fn a_stale_or_absent_alignment_carries_no_words_for_the_view_to_trust() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;
    publish_rows(&pool, &fixture, &[None; CHAPTERS]).await;

    let legacy = r#"[{"w":"one","s":0,"e":250}]"#;
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: fixture.segment_ids[0].clone(),
            source: "tts_native".into(),
            words_json: WordsJson::from_stored(legacy.into()),
            wer: None,
        },
    )
    .await
    .expect("legacy alignment");
    declare_words(&pool, &fixture.segment_ids[1], &words(2)).await;

    let chapter = reading::chapter(
        &pool,
        &fixture.job_id,
        &fixture.chapter_ids[0],
        cache(),
        |_b, _p| async { txtcaster_lib::reading::GeometryOutcome::Unavailable },
    )
    .await
    .expect("chapter");
    assert_eq!(chapter.segments[0].provenance, WordProvenance::Stale);
    assert!(chapter.segments[0].words.is_empty());
    assert_eq!(chapter.segments[1].provenance, WordProvenance::Fresh);
    assert_eq!(chapter.segments[1].words.len(), 2);
    assert_eq!(chapter.segments[2].provenance, WordProvenance::Missing);
    assert!(chapter.segments[2].words.is_empty());
}

#[tokio::test]
async fn a_zero_length_or_out_of_range_span_never_reaches_the_view() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;
    publish_rows(&pool, &fixture, &[None; CHAPTERS]).await;

    let entries = vec![
        serde_json::json!({"w": "kept", "s": 0, "e": 250}),
        serde_json::json!({"w": "clamped", "s": 800, "e": SEGMENT_MS + 700}),
        serde_json::json!({"w": "collapsed", "s": SEGMENT_MS, "e": SEGMENT_MS}),
        serde_json::json!({"w": "beyond", "s": SEGMENT_MS + 400, "e": SEGMENT_MS + 900}),
    ];
    declare_words(&pool, &fixture.segment_ids[0], &entries).await;

    let chapter = reading::chapter(
        &pool,
        &fixture.job_id,
        &fixture.chapter_ids[0],
        cache(),
        |_b, _p| async { txtcaster_lib::reading::GeometryOutcome::Unavailable },
    )
    .await
    .expect("chapter");
    let kept: Vec<&str> = chapter.segments[0]
        .words
        .iter()
        .map(|word| word.w.as_str())
        .collect();
    assert_eq!(
        kept,
        vec!["kept", "clamped"],
        "a span that collapses to zero after clamping carries no time to highlight, and a span \
         entirely past the segment collapses at its end (DATA_MODELS 2.4)"
    );
    let clamped = &chapter.segments[0].words[1];
    assert_eq!(clamped.end_ms, chapter.segments[0].end_ms);
}

#[tokio::test]
async fn a_chapter_the_job_never_rendered_is_refused_rather_than_returned_empty() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;
    let error = reading::chapter(
        &pool,
        &fixture.job_id,
        "chap-absent",
        cache(),
        |_b, _p| async { txtcaster_lib::reading::GeometryOutcome::Unavailable },
    )
    .await
    .expect_err("an unknown chapter must be refused");
    assert!(
        error.to_string().contains("no audio in this job"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn render_rows_that_disagree_with_the_audio_order_are_an_error_not_a_guess() {
    let (_dir, pool) = pool().await;
    let fixture = seed(&pool).await;
    let rows: Vec<chapter_render::ChapterRender> = fixture
        .chapter_ids
        .iter()
        .rev()
        .enumerate()
        .map(|(index, chapter_id)| chapter_render::ChapterRender {
            job_id: fixture.job_id.clone(),
            chapter_id: chapter_id.clone(),
            idx: index as i64,
            hash: format!("hash-{index}"),
            artifact: chapter_render::ARTIFACT_MP3.into(),
            duration_ms: nominal_chapter_ms(index),
            encoded_ms: Some(nominal_chapter_ms(index) + 52),
        })
        .collect();
    chapter_render::replace_all(&pool, &fixture.job_id, &rows)
        .await
        .expect("rows");

    let error = reading::index(&pool, &fixture.job_id, cache())
        .await
        .expect_err("a reordered render row set must not silently rebase the wrong chapter");
    assert!(
        error.to_string().contains("ordered differently"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn an_unknown_job_is_refused_by_both_reads() {
    let (_dir, pool) = pool().await;
    seed(&pool).await;
    assert!(reading::index(&pool, "no-such-job", cache()).await.is_err());
    assert!(
        reading::chapter(&pool, "no-such-job", "chap-0", cache(), |_b, _p| async {
            txtcaster_lib::reading::GeometryOutcome::Unavailable
        })
        .await
        .is_err()
    );
}
