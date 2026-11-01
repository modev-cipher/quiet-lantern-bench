use txtcaster_lib::orchestrator::verify;
use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice, SqlitePool};
use txtcaster_lib::words::{self, Timebase, WordProvenance, WordsJson};

async fn seed(pool: &SqlitePool) -> job::ScriptSegment {
    let b = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(1),
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
    let chapter = book::Chapter {
        id: new_id(),
        book_id: b.id.clone(),
        idx: 0,
        title: "Chapter 0".into(),
        page_start: 1,
        page_end: 1,
        is_pseudo: false,
        word_count: Some(4),
    };
    book::insert_chapter(pool, &chapter).await.expect("chapter");
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
        scope_json: r#"{"kind":"pages","page_start":1,"page_end":1}"#.into(),
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
        verify: true,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(pool, &record).await.expect("job");
    let segment = job::ScriptSegment {
        id: new_id(),
        job_id: record.id.clone(),
        chapter_id: chapter.id.clone(),
        idx: 0,
        page_no: 1,
        kind: "text".into(),
        figure_id: None,
        text: "the quick brown fox".into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: Some("aaaa".into()),
        duration_ms: Some(800),
        original_text: None,
    };
    job::insert_segments(pool, std::slice::from_ref(&segment))
        .await
        .expect("segments");
    segment
}

#[tokio::test]
async fn scoring_a_segment_changes_the_wer_and_not_one_byte_of_its_words() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let segment = seed(&pool).await;

    let words = vec![
        serde_json::json!({"w": "the", "s": 0, "e": 100}),
        serde_json::json!({"w": "quick", "s": 100, "e": 250}),
        serde_json::json!({"w": "brown", "s": 250, "e": 400}),
        serde_json::json!({"w": "fox", "s": 400, "e": 800}),
    ];
    let declared = WordsJson::declare(&words, Timebase::Trimmed).expect("declare");
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: segment.id.clone(),
            source: words::SOURCE_TTS_NATIVE.into(),
            words_json: declared.clone(),
            wer: None,
        },
    )
    .await
    .expect("alignment");

    verify::score_segment(&pool, &segment, "the quick brown fox")
        .await
        .expect("score");

    let stored = job::get_alignment(&pool, &segment.id)
        .await
        .expect("query")
        .expect("alignment row");
    assert_eq!(
        stored.words_json.as_str(),
        declared.as_str(),
        "the verify pass owns wer and nothing else; reserializing the payload here is what \
         would silently drop the timebase declaration the player classifies on"
    );
    assert_eq!(stored.wer, Some(0.0));
    let payload = stored.words_json.parse();
    assert_eq!(payload.timebase, Timebase::Trimmed);
    assert_eq!(
        words::classify(&payload, &stored.source),
        WordProvenance::Fresh
    );
}

#[tokio::test]
async fn scoring_a_legacy_row_leaves_it_legacy_rather_than_promoting_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let segment = seed(&pool).await;

    let legacy_bytes = r#"[{"w":"the","s":0,"e":100},{"w":"quick","s":100,"e":250}]"#;
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: segment.id.clone(),
            source: words::SOURCE_TTS_NATIVE.into(),
            words_json: WordsJson::from_stored(legacy_bytes.into()),
            wer: None,
        },
    )
    .await
    .expect("alignment");

    verify::score_segment(&pool, &segment, "the quick")
        .await
        .expect("score");

    let stored = job::get_alignment(&pool, &segment.id)
        .await
        .expect("query")
        .expect("alignment row");
    assert_eq!(stored.words_json.as_str(), legacy_bytes);
    let payload = stored.words_json.parse();
    assert!(!payload.versioned);
    assert_eq!(
        words::classify(&payload, &stored.source),
        WordProvenance::Stale
    );
}
