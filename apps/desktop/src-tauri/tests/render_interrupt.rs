#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use txtcaster_lib::orchestrator::stage::{
    control_channel, ProgressEvent, ProgressSink, Stage, StageContext, StageOutcome,
};
use txtcaster_lib::orchestrator::state::{Checkpoint, JobState, Scope};
use txtcaster_lib::orchestrator::RenderingStage;
use txtcaster_lib::render::mp3::{cache_wav_path, silence_wav};
use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice, SqlitePool};

const FAKE_FFMPEG_SLEEP_SECONDS: u64 = 600;
const INTERRUPT_BUDGET: Duration = Duration::from_secs(60);

async fn seed(pool: &SqlitePool) -> (String, String) {
    let book = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(4),
        language: Some("en".into()),
        is_scanned: false,
        ingest_path: Some("fast".into()),
        cover_path: None,
        content_hash: "hash".into(),
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
        title: "Chapter 0".into(),
        page_start: 1,
        page_end: 4,
        is_pseudo: false,
        word_count: Some(10),
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
    (book.id, profile.id)
}

async fn insert_job(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
    let record = job::RenderJob {
        id: new_id(),
        book_id: book_id.into(),
        scope_json: serde_json::to_string(&Scope::Pages {
            page_start: 1,
            page_end: 4,
        })
        .unwrap(),
        mode: "narrator".into(),
        voice_profile_id: voice_id.into(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Rendering.as_str().into(),
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
    record.id
}

#[tokio::test]
async fn a_pause_interrupts_a_running_ffmpeg_instead_of_waiting_for_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let cache_dir = dir.path().join("cache");
    let wav = cache_wav_path(&cache_dir, "aa11");
    std::fs::create_dir_all(wav.parent().expect("shard")).expect("shard");
    std::fs::write(&wav, silence_wav(800)).expect("wav");
    let segment = job::ScriptSegment {
        id: new_id(),
        job_id: job_id.clone(),
        chapter_id: "chap-0".into(),
        idx: 0,
        page_no: 1,
        kind: "text".into(),
        figure_id: None,
        text: "one".into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: Some("aa11".into()),
        duration_ms: Some(800),
        original_text: None,
    };
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .expect("segment");

    let sentinel = dir.path().join("ffmpeg-started");
    let fake = dir.path().join("slow-ffmpeg");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\ntouch {}\nsleep {FAKE_FFMPEG_SLEEP_SECONDS}\n",
            sentinel.display()
        ),
    )
    .expect("fake ffmpeg");
    let mut permissions = std::fs::metadata(&fake).expect("meta").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&fake, permissions).expect("chmod");
    std::env::set_var("TXTCASTER_FFMPEG", &fake);

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    let (handle, control) = control_channel();
    let sink: ProgressSink = Arc::new(|_: ProgressEvent| {});
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id,
        JobState::Rendering,
        Checkpoint::new(JobState::Rendering, now_ms()),
        control,
        sink,
    );

    let watched = sentinel.clone();
    let pauser = tokio::spawn(async move {
        let deadline = Instant::now() + INTERRUPT_BUDGET;
        while !watched.exists() {
            assert!(
                Instant::now() < deadline,
                "the stage never started the ffmpeg the pause is meant to interrupt"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let paused_at = Instant::now();
        handle.pause();
        paused_at
    });
    let outcome = tokio::time::timeout(INTERRUPT_BUDGET, stage.run(&mut ctx))
        .await
        .expect("a paused render must return instead of waiting the encode out")
        .expect("stage");
    let returned_at = Instant::now();
    let paused_at = pauser.await.expect("pauser");
    std::env::remove_var("TXTCASTER_FFMPEG");
    let elapsed = returned_at.duration_since(paused_at);

    assert_eq!(
        outcome,
        StageOutcome::Paused,
        "a pause during the loudness pass must return Paused, not run to completion"
    );
    assert!(
        elapsed < INTERRUPT_BUDGET,
        "the stage waited {elapsed:?} after the pause; against a {FAKE_FFMPEG_SLEEP_SECONDS}s \
         ffmpeg only a stopped encode returns this side of the budget, anything slower is the \
         worker's kill grace or the sleep itself"
    );
}
