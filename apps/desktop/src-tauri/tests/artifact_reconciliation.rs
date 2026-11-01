use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use txtcaster_lib::orchestrator::{
    JobOrchestrator, JobState, OrchestratorError, Scope, Stage, StageContext, StageOutcome,
    StageRegistry,
};
use txtcaster_lib::render::mp3::{cache_wav_path, silence_wav};
use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice, SqlitePool};

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

async fn seed_job(pool: &SqlitePool) -> (String, String, String) {
    let book = book::Book {
        id: new_id(),
        title: "Test Book".into(),
        author: Some("Author".into()),
        source_path: "/books/test.pdf".into(),
        format: "pdf".into(),
        page_count: Some(5),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: Some("fast".into()),
        cover_path: None,
        content_hash: "book-hash-1".into(),
        blocks_version: None,
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    book::insert_book(pool, &book).await.expect("book");
    let chapter = book::Chapter {
        id: "chap-0".into(),
        book_id: book.id.clone(),
        idx: 0,
        title: "Chapter 1".into(),
        page_start: 1,
        page_end: 5,
        is_pseudo: false,
        word_count: Some(50),
    };
    book::insert_chapter(pool, &chapter).await.expect("chapter");

    let voice_profile = voice::VoiceProfile {
        id: "voice-1".into(),
        name: "Voice 1".into(),
        engine: "kokoro".into(),
        kind: "preset".into(),
        reference_wav_path: None,
        preset_voice_id: Some("af_heart".into()),
        params_json: None,
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &voice_profile)
        .await
        .expect("voice");

    let job_id = new_id();
    let render_job = job::RenderJob {
        id: job_id.clone(),
        book_id: book.id.clone(),
        scope_json: serde_json::to_string(&Scope::Pages {
            page_start: 1,
            page_end: 5,
        })
        .expect("scope"),
        mode: "narrator".into(),
        voice_profile_id: voice_profile.id.clone(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: "FAILED".into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: Some(42),
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(pool, &render_job).await.expect("job");

    (book.id, voice_profile.id, job_id)
}

fn write_cache_wav(cache_dir: &std::path::Path, hash: &str, bytes: &[u8]) {
    let path = cache_wav_path(cache_dir, hash);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, bytes).expect("write wav");
}

#[tokio::test]
async fn test_reconcile_purged_cache_jobs() {
    let (dir, pool) = test_pool().await;
    let (_book_id, _voice_id, job_id) = seed_job(&pool).await;

    // Create segments with audio_hash
    let segments = vec![
        job::ScriptSegment {
            id: "seg-1".into(),
            job_id: job_id.clone(),
            chapter_id: "chap-0".into(),
            idx: 0,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "Hello".into(),
            original_text: None,
            source_span_json: None,
            flags_json: None,
            audio_hash: Some("hash-1".into()),
            duration_ms: Some(1000),
        },
        job::ScriptSegment {
            id: "seg-2".into(),
            job_id: job_id.clone(),
            chapter_id: "chap-0".into(),
            idx: 1,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "World".into(),
            original_text: None,
            source_span_json: None,
            flags_json: None,
            audio_hash: Some("hash-2".into()),
            duration_ms: Some(1200),
        },
    ];
    job::insert_segments(&pool, &segments)
        .await
        .expect("insert");

    // Add alignment
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: "seg-1".into(),
            source: "tts_native".into(),
            words_json: txtcaster_lib::words::WordsJson::from_stored("[]".into()),
            wer: None,
        },
    )
    .await
    .expect("alignment");

    // Add chapter_render
    sqlx::query(
        "INSERT INTO chapter_render (job_id, chapter_id, idx, hash, artifact, duration_ms, encoded_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&job_id)
    .bind("chap-0")
    .bind(0)
    .bind("hash-chap")
    .bind("mp3")
    .bind(2200)
    .bind(2200)
    .execute(&pool)
    .await
    .expect("chapter_render");

    // Set checkpoint at RENDERING
    let checkpoint = serde_json::json!({
        "stage": "RENDERING",
        "total_segments": 2,
        "done_segments": 2,
        "started_at": now_ms(),
        "updated_at": now_ms(),
    });
    job::update_job_checkpoint(&pool, &job_id, &checkpoint.to_string())
        .await
        .expect("checkpoint");

    // Run reconcile_purged_cache_jobs
    let affected = job::reconcile_purged_cache_jobs(&pool)
        .await
        .expect("reconcile");
    assert_eq!(affected, 2);

    // Verify segments audio cleared
    let read_segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert!(read_segments[0].audio_hash.is_none());
    assert!(read_segments[0].duration_ms.is_none());
    assert!(read_segments[1].audio_hash.is_none());
    assert!(read_segments[1].duration_ms.is_none());

    // Verify alignment deleted
    let align = job::get_alignment(&pool, "seg-1").await.expect("align");
    assert!(align.is_none());

    // Verify chapter_render deleted
    let chapter_renders: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM chapter_render WHERE job_id = ?")
            .bind(&job_id)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(chapter_renders, 0);

    // Verify checkpoint stage rewound to SYNTHESIZING
    let job_row = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let cp: serde_json::Value = serde_json::from_str(&job_row.checkpoint_json.unwrap()).unwrap();
    assert_eq!(cp["stage"], "SYNTHESIZING");
    assert_eq!(cp["done_segments"], 0);

    let _ = dir;
}

#[tokio::test]
async fn test_preflight_rewinds_when_audio_missing() {
    let (dir, pool) = test_pool().await;
    let (_book_id, _voice_id, job_id) = seed_job(&pool).await;
    let cache_dir = dir.path().join("cache");

    // Write a valid wav for segment 1, but segment 2 wav is missing
    let valid_wav = silence_wav(1000);
    write_cache_wav(&cache_dir, "hash-1", &valid_wav);

    let segments = vec![
        job::ScriptSegment {
            id: "seg-1".into(),
            job_id: job_id.clone(),
            chapter_id: "chap-0".into(),
            idx: 0,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "Hello".into(),
            original_text: None,
            source_span_json: None,
            flags_json: None,
            audio_hash: Some("hash-1".into()),
            duration_ms: Some(1000),
        },
        job::ScriptSegment {
            id: "seg-2".into(),
            job_id: job_id.clone(),
            chapter_id: "chap-0".into(),
            idx: 1,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "World".into(),
            original_text: None,
            source_span_json: None,
            flags_json: None,
            audio_hash: Some("hash-2".into()),
            duration_ms: Some(1000),
        },
    ];
    job::insert_segments(&pool, &segments)
        .await
        .expect("insert");

    // Checkpoint at RENDERING
    let checkpoint = serde_json::json!({
        "stage": "RENDERING",
        "total_segments": 2,
        "done_segments": 2,
        "started_at": now_ms(),
        "updated_at": now_ms(),
    });
    job::update_job_checkpoint(&pool, &job_id, &checkpoint.to_string())
        .await
        .expect("checkpoint");

    let orchestrator_live = JobOrchestrator::start_with_options(
        pool.clone(),
        StageRegistry::default(),
        Arc::new(|_| {}),
        Arc::new(|_| {}),
        Duration::from_millis(50),
        cache_dir,
        txtcaster_lib::orchestrator::StartOptions {
            consume_queue: true,
            ..Default::default()
        },
    );

    orchestrator_live
        .recover_failed(&job_id, false)
        .await
        .expect("recover");

    // Wait for preflight rewind to happen
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let job_row = job::get_job(&pool, &job_id).await.unwrap().unwrap();
        if let Some(cp_str) = job_row.checkpoint_json {
            let cp: serde_json::Value = serde_json::from_str(&cp_str).unwrap();
            if cp["stage"] == "SYNTHESIZING" {
                // Preflight successfully detected missing wav, rewound checkpoint to SYNTHESIZING
                let read_segments = job::list_segments(&pool, &job_id).await.expect("list");
                // Segment 1 (valid wav) preserved
                assert_eq!(read_segments[0].audio_hash.as_deref(), Some("hash-1"));
                // Segment 2 (missing wav) cleared
                assert!(read_segments[1].audio_hash.is_none());
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for preflight rewind to SYNTHESIZING");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let _ = orchestrator_live;
}

struct MockFailStage {
    stage: JobState,
}

impl Stage for MockFailStage {
    fn run<'a>(
        &'a self,
        _ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            Err(OrchestratorError::MissingAudioArtifact {
                stage: self.stage,
                count: 1,
                message: "mock missing artifact".into(),
            })
        })
    }
}

#[tokio::test]
async fn test_infinite_rewind_protection_exhausts_at_limit() {
    let (dir, pool) = test_pool().await;
    let (_book_id, _voice_id, job_id) = seed_job(&pool).await;

    // Checkpoint at RENDERING
    let checkpoint = serde_json::json!({
        "stage": "RENDERING",
        "total_segments": 1,
        "done_segments": 1,
        "started_at": now_ms(),
        "updated_at": now_ms(),
    });
    job::update_job_checkpoint(&pool, &job_id, &checkpoint.to_string())
        .await
        .expect("checkpoint");

    // Build registry where RENDERING and SYNTHESIZING always return MissingAudioArtifact
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(MockFailStage {
            stage: JobState::Synthesizing,
        }),
    );
    registry.register(
        JobState::Rendering,
        Arc::new(MockFailStage {
            stage: JobState::Rendering,
        }),
    );

    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        Arc::new(|_| {}),
        Arc::new(|_| {}),
        Duration::from_millis(50),
        dir.path().to_path_buf(),
        txtcaster_lib::orchestrator::StartOptions {
            consume_queue: true,
            ..Default::default()
        },
    );

    orchestrator
        .recover_failed(&job_id, false)
        .await
        .expect("recover");

    // Wait for terminal settlement
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let job_row = job::get_job(&pool, &job_id).await.unwrap().unwrap();
        if job_row.state == "FAILED" {
            let report: serde_json::Value =
                serde_json::from_str(job_row.report_json.as_deref().unwrap()).unwrap();
            assert!(
                report["error"]
                    .as_str()
                    .unwrap()
                    .contains("artifact recovery exhausted"),
                "expected ArtifactRecoveryExhausted error in report, got: {:?}",
                report["error"]
            );
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for job failure settlement");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
