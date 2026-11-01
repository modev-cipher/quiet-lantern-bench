#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use txtcaster_lib::orchestrator::stage::{
    control_channel, ProgressEvent, ProgressSink, Stage, StageContext, StageOutcome,
};
use txtcaster_lib::orchestrator::state::{Checkpoint, JobState, Scope};
use txtcaster_lib::orchestrator::OrchestratorError;
use txtcaster_lib::orchestrator::RenderingStage;
use txtcaster_lib::render::mp3::cache_wav_path;
use txtcaster_lib::storage::{
    self, book, cache, chapter_render, job, new_id, now_ms, voice, SqlitePool,
};

const CHAPTERS: i64 = 2;
const SEGMENT_MS: i64 = 900;
const SLOW_ENCODE_SECONDS: u64 = 600;
const INTERRUPT_BUDGET: Duration = Duration::from_secs(60);

/// Every file under a directory, at any depth.
///
/// The export is no longer written directly into the exports root — it is
/// `<exports_dir>/<cast_key>/<name>` — so a flat `read_dir` here would find
/// nothing and the transaction assertions below would pass over an empty list.
/// The depth bound mirrors the app's own walk: `Path::is_dir` follows symlinks,
/// so an unbounded recursion on a test fixture directory is not worth the risk.
fn files_under(dir: &std::path::Path, depth: usize) -> Vec<std::path::PathBuf> {
    if depth == 0 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .flat_map(|path| {
            if path.is_dir() {
                files_under(&path, depth - 1)
            } else {
                vec![path]
            }
        })
        .collect()
}

static FFMPEG_ENV_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn tone_wav(duration_ms: i64) -> Vec<u8> {
    let sample_rate = 24_000u32;
    let frames = (sample_rate as i64 * duration_ms / 1000) as u32;
    let data_bytes = frames * 2;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for frame in 0..frames {
        let phase = frame as f64 / sample_rate as f64 * 220.0 * std::f64::consts::TAU;
        out.extend_from_slice(&((phase.sin() * 12_000.0) as i16).to_le_bytes());
    }
    out
}

async fn seed(pool: &SqlitePool, dir: &std::path::Path) -> String {
    let book = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(8),
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
        book_id: book.id.clone(),
        scope_json: serde_json::to_string(&Scope::Pages {
            page_start: 1,
            page_end: 8,
        })
        .unwrap(),
        mode: "narrator".into(),
        voice_profile_id: profile.id.clone(),
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

    let mut segments = Vec::new();
    for chapter_idx in 0..CHAPTERS {
        let chapter = book::Chapter {
            id: format!("chap-{chapter_idx}"),
            book_id: book.id.clone(),
            idx: chapter_idx,
            title: format!("Chapter {chapter_idx}"),
            page_start: 1 + chapter_idx * 4,
            page_end: 4 + chapter_idx * 4,
            is_pseudo: false,
            word_count: Some(50),
        };
        book::insert_chapter(pool, &chapter).await.expect("chapter");
        for within in 0..2 {
            let idx = chapter_idx * 2 + within;
            let hash = format!("seg{idx:04}");
            let path = cache_wav_path(&dir.join("cache"), &hash);
            std::fs::create_dir_all(path.parent().expect("shard")).expect("shard");
            std::fs::write(path, tone_wav(SEGMENT_MS)).expect("wav");
            segments.push(job::ScriptSegment {
                id: new_id(),
                job_id: record.id.clone(),
                chapter_id: chapter.id.clone(),
                idx,
                page_no: 1 + idx,
                kind: "text".into(),
                figure_id: None,
                text: format!("Sentence {idx}."),
                source_span_json: None,
                flags_json: None,
                audio_hash: Some(hash),
                duration_ms: Some(SEGMENT_MS),
                original_text: None,
            });
        }
    }
    job::insert_segments(pool, &segments)
        .await
        .expect("segments");
    record.id
}

fn artifact_paths(cache_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![cache_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.len() == 64)
                .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }
    found
}

async fn run(
    stage: &RenderingStage,
    pool: &SqlitePool,
    job_id: &str,
    pause_when: Option<std::path::PathBuf>,
) -> (StageOutcome, Option<Instant>) {
    let (handle, control) = control_channel();
    let sink: ProgressSink = Arc::new(|_: ProgressEvent| {});
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.to_string(),
        JobState::Rendering,
        Checkpoint::new(JobState::Rendering, now_ms()),
        control,
        sink,
    );
    let pauser = pause_when.map(|sentinel| {
        let handle = handle.clone();
        tokio::spawn(async move {
            let deadline = Instant::now() + INTERRUPT_BUDGET;
            while !sentinel.exists() {
                assert!(
                    Instant::now() < deadline,
                    "the stage never started the encode the pause is meant to interrupt"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let paused_at = Instant::now();
            handle.pause();
            paused_at
        })
    });
    let outcome = tokio::time::timeout(INTERRUPT_BUDGET, stage.run(&mut ctx))
        .await
        .expect("a paused render must return instead of waiting the encode out")
        .expect("stage");
    let paused_at = match pauser {
        Some(pauser) => Some(pauser.await.expect("pause task")),
        None => None,
    };
    drop(handle);
    (outcome, paused_at)
}

async fn run_raw(
    stage: &RenderingStage,
    pool: &SqlitePool,
    job_id: &str,
) -> Result<StageOutcome, OrchestratorError> {
    let (handle, control) = control_channel();
    let sink: ProgressSink = Arc::new(|_: ProgressEvent| {});
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.to_string(),
        JobState::Rendering,
        Checkpoint::new(JobState::Rendering, now_ms()),
        control,
        sink,
    );
    let outcome = tokio::time::timeout(INTERRUPT_BUDGET, stage.run(&mut ctx))
        .await
        .expect("the stage must finish inside the interrupt budget");
    drop(handle);
    outcome
}

#[tokio::test]
async fn an_interrupted_re_render_leaves_the_previous_chapter_rows_intact() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let _env = FFMPEG_ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let job_id = seed(&pool, dir.path()).await;
    let cache_dir = dir.path().join("cache");
    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));

    assert_eq!(
        run(&stage, &pool, &job_id, None).await.0,
        StageOutcome::Completed
    );
    let before = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert_eq!(before.len(), CHAPTERS as usize);
    assert!(
        before.iter().all(|row| row.encoded_ms.is_some()),
        "this fixture must exercise the linear path, where chapters are encoded"
    );

    let mut evicted = 0;
    for path in artifact_paths(&cache_dir) {
        let hash = path.file_name().unwrap().to_str().unwrap().to_string();
        if cache::get_entry(&pool, &hash)
            .await
            .expect("entry")
            .map(|row| row.kind == "chapter_mp3")
            .unwrap_or(false)
        {
            std::fs::remove_file(&path).expect("evict");
            cache::remove(&pool, &hash).await.expect("evict row");
            evicted += 1;
        }
    }
    assert_eq!(
        evicted, CHAPTERS as usize,
        "the re-render has to re-encode, or the pause never lands inside an encode"
    );

    let sentinel = dir.path().join("encode-started");
    let fake = dir.path().join("slow-chapter-encode");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"libmp3lame\" ]; then\n    \
             touch {}\n    sleep {SLOW_ENCODE_SECONDS}\n  fi\ndone\nexec \
             \"$(command -v ffmpeg)\" \"$@\"\n",
            sentinel.display()
        ),
    )
    .expect("fake ffmpeg");
    let mut permissions = std::fs::metadata(&fake).expect("meta").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&fake, permissions).expect("chmod");
    std::env::set_var("TXTCASTER_FFMPEG", &fake);

    let (outcome, paused_at) = run(&stage, &pool, &job_id, Some(sentinel.clone())).await;
    let returned_at = Instant::now();
    std::env::remove_var("TXTCASTER_FFMPEG");
    let elapsed = returned_at.duration_since(paused_at.expect("pause instant"));

    assert_eq!(
        outcome,
        StageOutcome::Paused,
        "the pause must land inside the chapter encode loop"
    );
    assert!(
        elapsed < INTERRUPT_BUDGET,
        "the encode loop waited {elapsed:?} after the pause; against a {SLOW_ENCODE_SECONDS}s \
         chapter encode only a stopped encode returns this side of the budget"
    );
    let during = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert_eq!(
        during, before,
        "a re-render interrupted mid-encode must leave every previous chapter row in \
         place: the clear and the inserts are one transaction, committed only after the \
         encodes succeed"
    );
}

#[tokio::test]
async fn a_failed_final_encode_keeps_the_previous_export_and_leaves_no_partial() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let _env = FFMPEG_ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let job_id = seed(&pool, dir.path()).await;
    let exports_dir = dir.path().join("exports");
    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));

    run_raw(&stage, &pool, &job_id)
        .await
        .expect("the first render publishes an export");

    let published = files_under(&exports_dir, 4)
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<std::path::PathBuf>>();
    assert_eq!(published.len(), 1, "the fixture renders exactly one export");
    let export_path = published[0].clone();
    let previous = std::fs::read(&export_path).expect("read the published export");

    let fake = dir.path().join("fail-final-encode");
    std::fs::write(
        &fake,
        "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"-id3v2_version\" ]; then\n    \
         exit 1\n  fi\ndone\nexec \"$(command -v ffmpeg)\" \"$@\"\n",
    )
    .expect("fake ffmpeg");
    let mut permissions = std::fs::metadata(&fake).expect("meta").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&fake, permissions).expect("chmod");
    std::env::set_var("TXTCASTER_FFMPEG", &fake);

    let failed = run_raw(&stage, &pool, &job_id).await;
    std::env::remove_var("TXTCASTER_FFMPEG");

    let error = failed.expect_err("a failed final encode must fail the stage");
    assert!(
        format!("{error}").contains("mp3 encode failed"),
        "unexpected failure: {error}"
    );

    assert!(
        export_path.exists(),
        "a failed final encode must never delete the previous export at the user-visible path"
    );
    let after = std::fs::read(&export_path).expect("re-read the previous export");
    assert_eq!(
        previous, after,
        "a failed re-render must leave the previous export's bytes byte-identical"
    );
    let partials: Vec<std::path::PathBuf> = files_under(&exports_dir, 4)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".mp3.partial"))
        })
        .collect();
    assert!(
        partials.is_empty(),
        "a failed final encode must not leave a partial export behind: {partials:?}"
    );
}
