use std::path::Path;
use std::sync::{Arc, Mutex};

use txtcaster_lib::orchestrator::stage::{
    control_channel, ProgressEvent, ProgressSink, Stage, StageContext, StageOutcome,
};
use txtcaster_lib::orchestrator::state::{Checkpoint, JobState, OrchestratorError, Scope};
use txtcaster_lib::orchestrator::{record_contention, ContentionDelta, RenderingStage};
use txtcaster_lib::render::mp3::silence_wav;
use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice, SqlitePool};

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

async fn seed(pool: &SqlitePool) -> (String, String) {
    let book = book::Book {
        id: new_id(),
        title: "Fixture: Book?".into(),
        author: Some("Ada".into()),
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(10),
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
    for idx in 0..2 {
        let chapter = book::Chapter {
            id: format!("chap-{idx}"),
            book_id: book.id.clone(),
            idx,
            title: format!("Chapter {idx}"),
            page_start: idx * 5 + 1,
            page_end: idx * 5 + 5,
            is_pseudo: false,
            word_count: Some(100),
        };
        book::insert_chapter(pool, &chapter).await.expect("chapter");
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
    (book.id, profile.id)
}

async fn insert_job_with_report(
    pool: &SqlitePool,
    book_id: &str,
    voice_id: &str,
    report_json: Option<&str>,
) -> String {
    let job_id = new_id();
    let record = job::RenderJob {
        id: job_id.clone(),
        book_id: book_id.into(),
        scope_json: serde_json::to_string(&Scope::Pages {
            page_start: 1,
            page_end: 10,
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
        report_json: report_json.map(|s| s.to_string()),
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(pool, &record).await.expect("job");
    job_id
}

async fn insert_job(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
    insert_job_with_report(pool, book_id, voice_id, None).await
}

fn tone_wav(duration_ms: i64, amplitude: f64, freq: f64) -> Vec<u8> {
    let mut wav = silence_wav(duration_ms);
    let frames = (wav.len() - 44) / 2;
    for i in 0..frames {
        let t = i as f64 / 24_000.0;
        let value = (amplitude * (2.0 * std::f64::consts::PI * freq * t).sin() * 32767.0) as i16;
        let bytes = value.to_le_bytes();
        wav[44 + i * 2] = bytes[0];
        wav[44 + i * 2 + 1] = bytes[1];
    }
    wav
}

fn write_cache_wav(cache_dir: &Path, hash: &str, data: &[u8]) {
    let shard = cache_dir.join(&hash[..2]);
    std::fs::create_dir_all(&shard).expect("shard");
    std::fs::write(shard.join(hash), data).expect("wav");
}

async fn insert_segment(
    pool: &SqlitePool,
    job_id: &str,
    chapter_id: &str,
    idx: i64,
    audio_hash: Option<&str>,
    duration_ms: Option<i64>,
    flags_json: Option<&str>,
) {
    let segment = job::ScriptSegment {
        id: new_id(),
        job_id: job_id.into(),
        chapter_id: chapter_id.into(),
        idx,
        page_no: 1,
        kind: "text".into(),
        figure_id: None,
        text: format!("segment {idx}"),
        source_span_json: None,
        flags_json: flags_json.map(|s| s.to_string()),
        audio_hash: audio_hash.map(|s| s.to_string()),
        duration_ms,
        original_text: None,
    };
    job::insert_segments(pool, std::slice::from_ref(&segment))
        .await
        .expect("segment");
}

fn progress_sink() -> (ProgressSink, Arc<Mutex<Vec<ProgressEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = events.clone();
    let sink: ProgressSink = Arc::new(move |event| sink_events.lock().unwrap().push(event));
    (sink, events)
}

async fn run_stage(
    stage: &dyn Stage,
    pool: &SqlitePool,
    job_id: &str,
) -> Result<StageOutcome, OrchestratorError> {
    let (_handle, control) = control_channel();
    let (sink, _events) = progress_sink();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.to_string(),
        JobState::Rendering,
        Checkpoint::new(JobState::Rendering, now_ms()),
        control,
        sink,
    );
    stage.run(&mut ctx).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_render_report_keeps_what_was_written_after_the_stage_started() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let cache_dir = dir.path().join("cache");
    write_cache_wav(&cache_dir, "aa11", &tone_wav(1200, 0.6, 220.0));
    insert_segment(&pool, &job_id, "chap-0", 0, Some("aa11"), Some(1200), None).await;

    let writer_pool = pool.clone();
    let writer_job = job_id.clone();
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sink: ProgressSink = Arc::new(move |event: ProgressEvent| {
        if event.pct < 0.5 || fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let pool = writer_pool.clone();
        let job_id = writer_job.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                record_contention(
                    &pool,
                    &job_id,
                    JobState::Rendering,
                    ContentionDelta {
                        episodes: 1,
                        retries: 2,
                        waited_s: 9,
                    },
                )
                .await
                .expect("write during the render stage");
            })
        });
    });

    let (_handle, control) = control_channel();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.clone(),
        JobState::Rendering,
        Checkpoint::new(JobState::Rendering, now_ms()),
        control,
        sink,
    );
    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    assert_eq!(
        stage.run(&mut ctx).await.expect("stage"),
        StageOutcome::Completed
    );

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).expect("json");
    assert_eq!(
        report["contention"]["retries"], 2,
        "the stage read report_json when it started and writes it back at the end, so anything \
         written in between is destroyed unless the write re-reads; the block was committed \
         while the encode was running: {report}"
    );
    assert!(
        report["durations_s"]["render"].is_number(),
        "re-reading must not cost the stage its own report: {report}"
    );
    assert_eq!(report["segment_count"], 1, "{report}");
}

fn ffprobe(args: &[&str]) -> String {
    let out = std::process::Command::new("ffprobe")
        .args(args)
        .output()
        .expect("ffprobe");
    assert!(out.status.success(), "ffprobe failed");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn id3v2_tag_bytes(path: &Path) -> u64 {
    let data = std::fs::read(path).expect("read mp3");
    if data.len() < 10 || &data[0..3] != b"ID3" {
        return 0;
    }
    let size = ((data[6] as u64) << 21)
        | ((data[7] as u64) << 14)
        | ((data[8] as u64) << 7)
        | (data[9] as u64);
    size + 10
}

#[tokio::test]
async fn rendering_stage_downscales_the_embedded_cover() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let (dir, pool) = test_pool().await;
    let cover = dir.path().join("cover.png");
    let made = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1200x1700,noise=alls=60:allf=t+u",
            "-frames:v",
            "1",
            cover.to_str().unwrap(),
        ])
        .status()
        .expect("ffmpeg cover");
    assert!(made.success());
    let cover_bytes = std::fs::metadata(&cover).expect("cover").len();
    assert!(cover_bytes > 300_000, "fixture cover must be large");

    let (book_id, voice_id) = seed(&pool).await;
    book::set_book_cover(&pool, &book_id, cover.to_str().unwrap())
        .await
        .expect("cover path");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let cache_dir = dir.path().join("cache");
    write_cache_wav(&cache_dir, "aa11", &tone_wav(1500, 0.6, 220.0));
    insert_segment(&pool, &job_id, "chap-0", 0, Some("aa11"), Some(1500), None).await;

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    assert_eq!(
        run_stage(&stage, &pool, &job_id).await.expect("stage"),
        StageOutcome::Completed
    );

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let output_paths: Vec<String> =
        serde_json::from_str(record.output_paths_json.as_deref().unwrap()).unwrap();
    let output = Path::new(&output_paths[0]);

    let tag = id3v2_tag_bytes(output);
    assert!(tag > 0, "cover must still be embedded");
    assert!(
        tag < 150_000,
        "embedded cover must be downscaled, got {tag} bytes of ID3v2 from a {cover_bytes} byte source"
    );

    let codec = ffprobe(&[
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=codec_name,width,height",
        "-of",
        "default=nw=1:nk=1",
        output.to_str().unwrap(),
    ]);
    let fields: Vec<&str> = codec.split_whitespace().collect();
    assert_eq!(fields[0], "mjpeg", "cover must be re-encoded, not copied");
    let width: u32 = fields[1].parse().expect("width");
    let height: u32 = fields[2].parse().expect("height");
    assert!(
        width <= 600 && height <= 600,
        "cover must fit inside 600px, got {width}x{height}"
    );
}

#[tokio::test]
async fn rendering_stage_never_upscales_a_small_cover() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let (dir, pool) = test_pool().await;
    let cover = dir.path().join("small.png");
    let made = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=300x400",
            "-frames:v",
            "1",
            cover.to_str().unwrap(),
        ])
        .status()
        .expect("ffmpeg cover");
    assert!(made.success());

    let (book_id, voice_id) = seed(&pool).await;
    book::set_book_cover(&pool, &book_id, cover.to_str().unwrap())
        .await
        .expect("cover path");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let cache_dir = dir.path().join("cache");
    write_cache_wav(&cache_dir, "aa11", &tone_wav(1500, 0.6, 220.0));
    insert_segment(&pool, &job_id, "chap-0", 0, Some("aa11"), Some(1500), None).await;

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    assert_eq!(
        run_stage(&stage, &pool, &job_id).await.expect("stage"),
        StageOutcome::Completed
    );

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let output_paths: Vec<String> =
        serde_json::from_str(record.output_paths_json.as_deref().unwrap()).unwrap();
    let dims = ffprobe(&[
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height",
        "-of",
        "default=nw=1:nk=1",
        &output_paths[0],
    ]);
    let fields: Vec<&str> = dims.split_whitespace().collect();
    assert_eq!(
        (fields[0], fields[1]),
        ("300", "400"),
        "a cover already under the bound must not be upscaled"
    );
}

#[tokio::test]
async fn rendering_stage_produces_mp3_with_chapters_and_target_loudness() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let cache_dir = dir.path().join("cache");

    let loud = tone_wav(1500, 0.6, 220.0);
    let quiet = tone_wav(1200, 0.2, 330.0);
    write_cache_wav(&cache_dir, "aa11", &loud);
    write_cache_wav(&cache_dir, "bb22", &quiet);
    write_cache_wav(&cache_dir, "cc33", &loud);
    insert_segment(&pool, &job_id, "chap-0", 0, Some("aa11"), Some(1500), None).await;
    insert_segment(&pool, &job_id, "chap-0", 1, Some("bb22"), Some(1200), None).await;
    insert_segment(&pool, &job_id, "chap-1", 2, Some("cc33"), Some(1500), None).await;
    insert_segment(
        &pool,
        &job_id,
        "chap-1",
        3,
        None,
        None,
        Some(r#"["tts_failed"]"#),
    )
    .await;

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage");
    assert_eq!(outcome, StageOutcome::Completed);

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let output_paths: Vec<String> =
        serde_json::from_str(record.output_paths_json.as_deref().unwrap()).unwrap();
    assert_eq!(output_paths.len(), 1);
    let output = Path::new(&output_paths[0]);
    assert!(output.is_file());
    // The visible name used to spell out the voice, the mode and a slice of the
    // book id: `Fixture_ Book_ - Pages 1-10 [vp-123] - narrator [b1234567].mp3`.
    // None of those told the user anything the shelf does not already read from
    // the row, and the identity they were standing in for now lives in the
    // directory — which is what this asserts, because a flat file cannot tell two
    // same-cardinality casts apart and the publisher renames over the target
    // before the row is written.
    assert_eq!(
        output.file_name().unwrap().to_str(),
        Some("Fixture_ Book_ - Pages 1-10.mp3"),
        "the visible name must carry the title and the scope and nothing else"
    );
    let cast_dir = output.parent().expect("<cast_key>/<name>");
    assert_eq!(
        cast_dir.file_name().unwrap().to_str(),
        Some(
            txtcaster_lib::orchestrator::render::cast_key(
                &book_id,
                &Scope::Pages {
                    page_start: 1,
                    page_end: 10,
                },
                &voice_id,
                "narrator",
            )
            .as_str()
        ),
        "the directory is the cast identity: book, scope, voice and mode"
    );
    assert_ne!(
        cast_dir.file_name().unwrap().to_str(),
        Some(&book_id[book_id.len() - 8..]),
        "the id slice left the filename, so nothing in the name is a fragment of the book id"
    );
    let exports_dir = storage::paths::defaults(dir.path())
        .exports_dir()
        .to_path_buf();
    assert_eq!(
        cast_dir.parent().unwrap(),
        exports_dir,
        "the cast directory is a direct child of the exports root, which is the only level \
         the walk descends into"
    );
    let rows = storage::export::list_exports(&pool).await.unwrap();
    assert_eq!(rows.len(), 1, "the published file has its row");
    assert_eq!(
        rows[0].path, output_paths[0],
        "the row must point at the file that was actually written: a nested layout is only \
         safe if nothing recomputes the path"
    );

    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    assert!(report["durations_s"]["render"].is_number());
    assert_eq!(report["flagged_segments"].as_array().unwrap().len(), 1);

    let chapters_json = ffprobe(&[
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_chapters",
        &output_paths[0],
    ]);
    let parsed: serde_json::Value = serde_json::from_str(&chapters_json).unwrap();
    let chapters = parsed["chapters"].as_array().unwrap();
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[0]["tags"]["title"], "Chapter 0");
    assert_eq!(chapters[1]["tags"]["title"], "Chapter 1");

    let tags_json = ffprobe(&[
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_format",
        &output_paths[0],
    ]);
    let parsed: serde_json::Value = serde_json::from_str(&tags_json).unwrap();
    assert_eq!(parsed["format"]["tags"]["title"], "Fixture: Book?");
    assert_eq!(parsed["format"]["tags"]["artist"], "Ada");

    let measure = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostdin",
            "-i",
            &output_paths[0],
            "-af",
            "loudnorm=I=-19:TP=-1.5:LRA=11:print_format=json",
            "-f",
            "null",
            "-",
        ])
        .output()
        .expect("measure");
    let stderr = String::from_utf8_lossy(&measure.stderr);
    let parsed = txtcaster_lib::render::mp3::parse_loudnorm_json(&stderr).expect("loudnorm json");
    let lufs: f64 = parsed.input_i.parse().unwrap();
    assert!(
        (lufs - (-19.0)).abs() <= 1.0,
        "integrated loudness {lufs} outside -19 +/- 1 LUFS"
    );
}

#[tokio::test]
async fn rendering_stage_survives_malformed_prior_report() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job_with_report(
        &pool,
        &book_id,
        &voice_id,
        Some(r#"{"durations_s": "corrupt"}"#),
    )
    .await;
    let cache_dir = dir.path().join("cache");
    write_cache_wav(&cache_dir, "aa11", &tone_wav(800, 0.5, 220.0));
    insert_segment(&pool, &job_id, "chap-0", 0, Some("aa11"), Some(800), None).await;

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage");
    assert_eq!(outcome, StageOutcome::Completed);

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    assert!(report["durations_s"]["render"].is_number());
}

#[tokio::test]
async fn rendering_stage_errors_without_playable_segments() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    insert_segment(
        &pool,
        &job_id,
        "chap-0",
        0,
        None,
        None,
        Some(r#"["tts_failed"]"#),
    )
    .await;

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    let err = run_stage(&stage, &pool, &job_id).await.unwrap_err();
    assert!(matches!(err, OrchestratorError::InvalidRequest(_)));
}

#[tokio::test]
async fn rendering_stage_errors_when_cached_wav_missing() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    insert_segment(&pool, &job_id, "chap-0", 0, Some("aa11"), Some(1000), None).await;

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    let err = run_stage(&stage, &pool, &job_id).await.unwrap_err();
    match err {
        OrchestratorError::MissingAudioArtifact {
            stage,
            count,
            message,
        } => {
            assert_eq!(stage, JobState::Rendering);
            assert_eq!(count, 1);
            assert!(message.contains("cached wav missing"));
        }
        OrchestratorError::Render(msg) => assert!(msg.contains("cached wav missing")),
        other => panic!("expected MissingAudioArtifact or Render error, got {other}"),
    }
    let segments = job::list_segments(&pool, &job_id).await.unwrap();
    assert!(
        segments[0].audio_hash.is_none(),
        "reconciliation must clear missing segment audio"
    );
}

#[tokio::test]
async fn rendering_stage_respects_cancel_before_start() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    let (handle, control) = control_channel();
    handle.cancel();
    let (sink, _events) = progress_sink();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id,
        JobState::Rendering,
        Checkpoint::new(JobState::Rendering, now_ms()),
        control,
        sink,
    );
    let outcome = stage.run(&mut ctx).await.expect("stage");
    assert_eq!(outcome, StageOutcome::Cancelled);
}

#[tokio::test]
async fn rendering_stage_missing_job_errors() {
    let (dir, pool) = test_pool().await;
    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    let err = run_stage(&stage, &pool, "nope").await.unwrap_err();
    assert!(matches!(err, OrchestratorError::JobNotFound(_)));
}

async fn claimed_hashes(pool: &SqlitePool, job_id: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT hash FROM cache_ref WHERE job_id = ? ORDER BY hash")
        .bind(job_id)
        .fetch_all(pool)
        .await
        .expect("cache_ref")
}

#[tokio::test]
async fn rendering_stage_claims_a_reference_for_every_wav_in_its_timeline() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let cache_dir = dir.path().join("cache");
    let tone = tone_wav(600, 0.4, 220.0);
    write_cache_wav(&cache_dir, "aa11", &tone);
    write_cache_wav(&cache_dir, "bb22", &tone);
    insert_segment(&pool, &job_id, "chap-0", 0, Some("aa11"), Some(600), None).await;
    insert_segment(&pool, &job_id, "chap-1", 1, Some("bb22"), Some(600), None).await;
    insert_segment(&pool, &job_id, "chap-1", 2, None, None, None).await;

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    let outcome = run_stage(&stage, &pool, &job_id).await;
    if ffmpeg_available() {
        assert_eq!(outcome.expect("stage runs"), StageOutcome::Completed);
    }

    assert_eq!(
        claimed_hashes(&pool, &job_id).await,
        vec!["aa11".to_string(), "bb22".to_string()],
        "rendering read every playable WAV without claiming it"
    );
}

#[test]
fn the_final_export_is_published_by_rename_and_no_failure_arm_touches_the_published_path() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestrator/render.rs"
    ))
    .expect("render stage source");
    let lines: Vec<&str> = source.lines().collect();
    let pass2_line = lines
        .iter()
        .position(|line| line.contains("let pass2 = match run_ffmpeg"))
        .expect("pass 2 invocation");

    let mut arms = 0;
    for anchor in [
        "if !pass2.status.success()",
        "FfmpegRun::Interrupted(outcome) =>",
    ] {
        let mut found = false;
        for (index, line) in lines.iter().enumerate() {
            if index < pass2_line || !line.contains(anchor) {
                continue;
            }
            let window = lines[index..lines.len().min(index + 4)].join(" ");
            assert!(
                window.contains("remove_file(&partial_path)"),
                "each pass-2 arm must clear the in-progress encode, never the published export \
                 (orchestrator/render.rs:{})",
                index + 1
            );
            arms += 1;
            found = true;
        }
        assert!(
            found,
            "the pass-2 {anchor} arm disappeared; update this source-shape pin to track its new home"
        );
    }
    assert_eq!(arms, 2, "both pass-2 arms are pinned");

    assert!(
        !source.contains("remove_file(&output_path)"),
        "the published export is never deleted by the render stage; a failed or interrupted \
         re-render must leave the last good export byte-identical"
    );

    let publish = source
        .find("std::fs::rename(&partial_path, &output_path)")
        .expect("pass 2 publishes by rename");
    assert!(
        source[char_floor(&source, publish)..].contains("remove_file(&partial_path)"),
        "a failed publish must clear the partial rather than strand it in the exports directory"
    );
    assert!(
        source.contains("export could not be published"),
        "a failed publish must say what the user still has"
    );

    let format_flag = source
        .find("\"-f\", \"mp3\", &partial_arg")
        .expect("pass 2 names its output format");
    assert!(
        format_flag < publish,
        "the output format must be declared before the partial path: the partial name ends in \
         .partial, which is not an extension ffmpeg can infer a muxer from, so without -f mp3 \
         every export fails at the publish step instead"
    );

    let assigned = source
        .find("let output_arg =")
        .expect("the reported output path is bound once");
    let binding = &source[assigned..source[assigned..].find('\n').expect("a line") + assigned];
    assert!(
        binding.contains("output_path"),
        "the path recorded in report.output_paths_json and in the export row must be the published \
         export, not the partial the rename already moved: {binding}"
    );
    assert!(
        source.contains("json!([output_arg])") && source.contains("&output_arg,"),
        "the report and the export row must both record that same published path"
    );
    assert!(
        !source.contains("\"-f\", \"mp3\", &output_arg"),
        "ffmpeg must be handed the partial path; handing it the published path is what let a \
         failed encode truncate the last good export"
    );
}

fn char_floor(source: &str, index: usize) -> usize {
    let mut index = index.min(source.len());
    while !source.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[test]
fn the_final_encode_checks_free_space_first() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestrator/render.rs"
    ))
    .expect("render stage source");
    let pass2 = source
        .find("let pass2 = match run_ffmpeg")
        .expect("pass 2 invocation");
    let prelude = &source[char_floor(&source, pass2.saturating_sub(2400))..pass2];
    assert!(
        prelude.contains("not enough disk space"),
        "pass 2 must pre-check free space so a disk-full export fails with the required-space \
         estimate the degradation matrix promises, instead of a raw ffmpeg or sqlx error"
    );
    assert!(
        prelude.contains("fs4::available_space"),
        "the check must measure the actual free space on the exports volume"
    );
}

fn mp3_frame_bitrates_kbps(bytes: &[u8]) -> Vec<u32> {
    const MPEG1_L3: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const MPEG2_L3: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let mut pos = 0usize;
    if bytes.starts_with(b"ID3") && bytes.len() > 10 {
        let size = (usize::from(bytes[6]) << 21)
            | (usize::from(bytes[7]) << 14)
            | (usize::from(bytes[8]) << 7)
            | usize::from(bytes[9]);
        pos = 10 + size;
    }
    let mut rates = Vec::new();
    while pos + 4 <= bytes.len() {
        let header =
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        let version = (header >> 19) & 3;
        let layer = (header >> 17) & 3;
        let bitrate_index = ((header >> 12) & 15) as usize;
        let rate_index = ((header >> 10) & 3) as usize;
        if (header >> 21) & 0x7FF != 0x7FF
            || layer != 1
            || version == 1
            || bitrate_index == 0
            || bitrate_index == 15
            || rate_index == 3
        {
            pos += 1;
            continue;
        }
        let mpeg1 = version == 3;
        let kbps = if mpeg1 {
            MPEG1_L3[bitrate_index]
        } else {
            MPEG2_L3[bitrate_index]
        };
        let sample_rate = match (version, rate_index) {
            (3, 0) => 44_100,
            (3, 1) => 48_000,
            (3, _) => 32_000,
            (2, 0) => 22_050,
            (2, 1) => 24_000,
            (2, _) => 16_000,
            (_, 0) => 11_025,
            (_, 1) => 12_000,
            _ => 8_000,
        };
        let padding = ((header >> 9) & 1) as usize;
        let length = (if mpeg1 { 144 } else { 72 }) * kbps as usize * 1000 / sample_rate + padding;
        rates.push(kbps);
        pos += length.max(1);
    }
    rates
}

#[tokio::test]
async fn the_export_is_constant_bitrate_so_a_seek_lands_where_the_highlight_says() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let cache_dir = dir.path().join("cache");

    write_cache_wav(&cache_dir, "aa11", &tone_wav(1800, 0.7, 220.0));
    write_cache_wav(&cache_dir, "bb22", &silence_wav(1800));
    write_cache_wav(&cache_dir, "cc33", &tone_wav(1800, 0.05, 3_000.0));
    insert_segment(&pool, &job_id, "chap-0", 0, Some("aa11"), Some(1800), None).await;
    insert_segment(&pool, &job_id, "chap-0", 1, Some("bb22"), Some(1800), None).await;
    insert_segment(&pool, &job_id, "chap-1", 2, Some("cc33"), Some(1800), None).await;
    insert_segment(&pool, &job_id, "chap-1", 3, Some("bb22"), Some(1800), None).await;

    let stage = RenderingStage::new(&storage::paths::defaults(dir.path()));
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage");
    assert_eq!(outcome, StageOutcome::Completed);

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let output_paths: Vec<String> =
        serde_json::from_str(record.output_paths_json.as_deref().unwrap()).unwrap();
    let rates = mp3_frame_bitrates_kbps(&std::fs::read(&output_paths[0]).expect("export"));
    assert!(rates.len() > 100, "the export must carry real audio frames");
    let distinct: std::collections::BTreeSet<u32> = rates.iter().copied().collect();
    assert_eq!(
        distinct.into_iter().collect::<Vec<_>>(),
        vec![128],
        "a variable-bitrate export makes the player estimate a seek from a 100-entry table, \
         so after a resume the audio plays seconds behind the time the highlight follows"
    );
}
