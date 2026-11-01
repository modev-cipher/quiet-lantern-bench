#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use txtcaster_lib::orchestrator::stage::{PassthroughStage, Stage, StageContext, StageOutcome};
use txtcaster_lib::orchestrator::state::{JobState, OrchestratorError, Scope};
use txtcaster_lib::orchestrator::{
    residency_check, JobOrchestrator, JobUpdate, ProgressEvent, ProgressSink, SegmentEdit,
    SidecarProvider, StageRegistry, StartOptions, SynthesizingStage, UpdateSink,
};

fn test_options() -> StartOptions {
    StartOptions {
        vram_sampler: residency_check::disabled_sampler(),
        ..StartOptions::default()
    }
}

use txtcaster_lib::render::mp3::cache_wav_path;
use txtcaster_lib::storage::{
    self, book, cache, chapter_render, export, job, models, new_id, now_ms, voice, SqlitePool,
};
use txtcaster_lib::words::WordsJson;

type Captured = Arc<Mutex<Vec<(String, String)>>>;

struct FakeSidecar {
    port: u16,
}

impl SidecarProvider for FakeSidecar {
    fn connection(&self) -> Option<(u16, String)> {
        Some((self.port, "test-token".into()))
    }
}

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

fn tone_wav_at(duration_ms: i64, amplitude: f64) -> Vec<u8> {
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
        let sample = if amplitude < 0.0 {
            if frame % 6000 == 0 {
                30_000.0
            } else {
                phase.sin() * 120.0
            }
        } else {
            phase.sin() * amplitude
        };
        out.extend_from_slice(&(sample as i16).to_le_bytes());
    }
    out
}

fn write_cached_wav(dir: &tempfile::TempDir, hash: &str, duration_ms: i64) {
    write_cached_wav_at(dir, hash, duration_ms, 12_000.0);
}

fn write_cached_wav_at(dir: &tempfile::TempDir, hash: &str, duration_ms: i64, amplitude: f64) {
    let path = cache_wav_path(&dir.path().join("cache"), hash);
    std::fs::create_dir_all(path.parent().expect("shard")).expect("shard");
    std::fs::write(path, tone_wav_at(duration_ms, amplitude)).expect("wav");
}

fn sse_body(events: &[(&str, &str)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn spawn_capturing_server(body: String) -> (u16, Captured) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let bytes = body.into_bytes();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let bytes = bytes.clone();
            let sink = sink.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let mut req = Vec::new();
                let mut header_end = None;
                let mut content_length = 0usize;
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if header_end.is_none() {
                        if let Some(position) =
                            req.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            header_end = Some(position + 4);
                            let head = String::from_utf8_lossy(&req[..position]).to_lowercase();
                            for line in head.lines() {
                                if let Some(value) = line.strip_prefix("content-length:") {
                                    content_length = value.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                    if let Some(start) = header_end {
                        if req.len() >= start + content_length {
                            let head = String::from_utf8_lossy(&req[..start]).to_string();
                            let path = head
                                .lines()
                                .next()
                                .and_then(|line| line.split_whitespace().nth(1))
                                .unwrap_or_default()
                                .to_string();
                            let body = String::from_utf8_lossy(&req[start..start + content_length])
                                .to_string();
                            sink.lock().unwrap().push((path, body));
                            break;
                        }
                    }
                }
                let _ = mock_http::write_response(&mut sock, "200 OK", "text/event-stream", &bytes)
                    .await;
            });
        }
    });
    (port, captured)
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

fn sinks() -> (ProgressSink, UpdateSink) {
    let progress: ProgressSink = Arc::new(|_: ProgressEvent| {});
    let updates: UpdateSink = Arc::new(|_: JobUpdate| {});
    (progress, updates)
}

async fn seed(pool: &SqlitePool) -> (String, String, Vec<String>) {
    let book = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: Some("Author".into()),
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
    let mut chapter_ids = Vec::new();
    for idx in 0..3 {
        let chapter = book::Chapter {
            id: format!("chap-{idx}"),
            book_id: book.id.clone(),
            idx,
            title: format!("Chapter {idx}"),
            page_start: 1 + idx * 3,
            page_end: 3 + idx * 3,
            is_pseudo: false,
            word_count: Some(100),
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
        params_json: Some(r#"{"speed":1.0}"#.into()),
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &profile)
        .await
        .expect("voice");
    models::insert_model_install(
        pool,
        &models::ModelInstall {
            model_id: "kokoro-82m".into(),
            path: "tts/kokoro-82m".into(),
            sha256: "deadbeef".into(),
            disk_bytes: 1,
            source: "curated".into(),
            installed_at: now_ms(),
        },
    )
    .await
    .expect("install");
    (book.id, profile.id, chapter_ids)
}

async fn insert_done_job(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
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
        tts_plan_json: Some(
            r#"{"model_id":"kokoro-82m","device":"cpu","tts_workers":2,"batch":2,"badge":"green"}"#
                .into(),
        ),
        align_plan_json: None,
        state: JobState::Done.as_str().into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: Some(7),
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(pool, &record).await.expect("job");
    job_id
}

async fn seed_segments(
    pool: &SqlitePool,
    dir: &tempfile::TempDir,
    job_id: &str,
    chapter_ids: &[String],
) -> Vec<job::ScriptSegment> {
    let mut segments = Vec::new();
    for (position, chapter_id) in chapter_ids.iter().enumerate() {
        for within in 0..2 {
            let idx = (position * 2 + within) as i64;
            let hash = format!("seg{idx:04}");
            write_cached_wav(dir, &hash, 900);
            cache::upsert_entry(pool, &hash, "wav", 100)
                .await
                .expect("cache row");
            segments.push(job::ScriptSegment {
                id: new_id(),
                job_id: job_id.into(),
                chapter_id: chapter_id.clone(),
                idx,
                page_no: 1 + idx,
                kind: "text".into(),
                figure_id: None,
                text: format!("Sentence number {idx}."),
                source_span_json: None,
                flags_json: None,
                audio_hash: Some(hash),
                duration_ms: Some(900),
                original_text: None,
            });
        }
    }
    job::insert_segments(pool, &segments)
        .await
        .expect("segments");
    segments
}

#[derive(Clone)]
struct Gate {
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

impl Gate {
    fn new() -> Self {
        Gate {
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }

    async fn wait_until_entered(&self) {
        self.entered
            .acquire()
            .await
            .expect("the gated stage was never entered")
            .forget();
    }

    fn release(&self) {
        self.release.add_permits(1);
    }
}

struct GatedStage {
    inner: Arc<dyn Stage>,
    gate: Gate,
}

impl Stage for GatedStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> futures_util::future::BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            self.gate.entered.add_permits(1);
            self.gate
                .release
                .acquire()
                .await
                .expect("the gate was dropped before release")
                .forget();
            self.inner.run(ctx).await
        })
    }
}

fn synthesis_stage(dir: &tempfile::TempDir, port: u16) -> Arc<dyn Stage> {
    Arc::new(SynthesizingStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    ))
}

fn rendering_stage(dir: &tempfile::TempDir) -> Arc<dyn Stage> {
    Arc::new(txtcaster_lib::orchestrator::RenderingStage::new(
        &txtcaster_lib::storage::paths::defaults(dir.path()),
    ))
}

fn registry_gated_at(
    dir: &tempfile::TempDir,
    port: u16,
    at: JobState,
    gate: &Gate,
) -> StageRegistry {
    let mut registry = StageRegistry::default();
    let synthesis = synthesis_stage(dir, port);
    let rendering = rendering_stage(dir);
    let gated = |inner: Arc<dyn Stage>| -> Arc<dyn Stage> {
        Arc::new(GatedStage {
            inner,
            gate: gate.clone(),
        })
    };
    registry.register(
        JobState::Synthesizing,
        if at == JobState::Synthesizing {
            gated(synthesis)
        } else {
            synthesis
        },
    );
    registry.register(JobState::Aligning, Arc::new(PassthroughStage));
    registry.register(
        JobState::Rendering,
        if at == JobState::Rendering {
            gated(rendering)
        } else {
            rendering
        },
    );
    registry
}

fn registry(dir: &tempfile::TempDir, port: u16) -> StageRegistry {
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(SynthesizingStage::new(
            Arc::new(FakeSidecar { port }),
            dir.path().join("models"),
            dir.path().join("cache"),
        )),
    );
    registry.register(JobState::Aligning, Arc::new(PassthroughStage));
    registry.register(
        JobState::Rendering,
        Arc::new(txtcaster_lib::orchestrator::RenderingStage::new(
            &txtcaster_lib::storage::paths::defaults(dir.path()),
        )),
    );
    registry
}

const PIPELINE_BUDGET_FLOOR: Duration = Duration::from_secs(30);
const RENDER_BUDGET_PER_CHAPTER: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

async fn wait_for_done(pool: &SqlitePool, job_id: &str) {
    let chapters: std::collections::BTreeSet<String> = job::list_segments(pool, job_id)
        .await
        .expect("segments")
        .into_iter()
        .filter(|segment| segment.audio_hash.is_some())
        .map(|segment| segment.chapter_id)
        .collect();
    let budget = PIPELINE_BUDGET_FLOOR + RENDER_BUDGET_PER_CHAPTER * chapters.len().max(1) as u32;
    let started = std::time::Instant::now();
    let mut seen: Vec<String> = Vec::new();
    loop {
        let record = job::get_job(pool, job_id).await.expect("get").expect("job");
        if seen.last().map(String::as_str) != Some(record.state.as_str()) {
            seen.push(record.state.clone());
        }
        if record.state == JobState::Done.as_str() {
            return;
        }
        if record.state == JobState::Failed.as_str() {
            panic!("job failed: {:?}", record.report_json);
        }
        assert!(
            started.elapsed() < budget,
            "job never finished within {budget:?} for {} chapter(s); states seen: {seen:?}",
            chapters.len()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn tts_requests(captured: &Captured) -> Vec<serde_json::Value> {
    captured
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == "/tts")
        .filter_map(|(_, body)| serde_json::from_str(body).ok())
        .collect()
}

async fn render_once(dir: &tempfile::TempDir, pool: &SqlitePool, job_id: &str) {
    let (progress, updates) = sinks();
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry(dir, 0),
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );
    job::update_job_state(pool, job_id, JobState::Rendering.as_str())
        .await
        .expect("state");
    orchestrator.resume_on_launch().await.expect("resume");
    wait_for_done(pool, job_id).await;
    orchestrator
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn editing_one_sentence_resynthesizes_exactly_one_segment_and_leaves_other_chapters_alone() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    let segments = seed_segments(&pool, &dir, &job_id, &chapter_ids).await;

    render_once(&dir, &pool, &job_id).await;
    let before = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert_eq!(before.len(), 3, "one row per chapter");
    let untouched_before = before
        .iter()
        .find(|row| row.chapter_id == chapter_ids[1])
        .expect("chapter 2 row")
        .clone();

    let edited = segments
        .iter()
        .find(|segment| segment.chapter_id == chapter_ids[0])
        .expect("a chapter 1 segment")
        .clone();

    let reply = sse_body(&[
        (
            "result",
            r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"edited01","wav_bytes":100,"duration_ms":950,"cache_hit":false,"words":[],"flags":[]}}"#,
        ),
        ("done", r#"{"done":1,"total":1}"#),
    ]);
    let (port, captured) = spawn_capturing_server(reply);
    write_cached_wav(&dir, "edited01", 950);

    let (progress, updates) = sinks();
    let gate = Gate::new();
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry_gated_at(&dir, port, JobState::Synthesizing, &gate),
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );
    orchestrator
        .resynthesize_segment(
            &job_id,
            &edited.id,
            SegmentEdit::Replace("A completely different sentence.".into()),
        )
        .await
        .expect("resynthesize");
    gate.wait_until_entered().await;
    let queued = job::get_job(&pool, &job_id)
        .await
        .expect("get")
        .expect("job");
    let checkpoint: serde_json::Value =
        serde_json::from_str(queued.checkpoint_json.as_deref().expect(
            "a re-synthesis must persist a checkpoint, or the worker restarts the job at ingest",
        ))
        .expect("checkpoint json");
    assert_eq!(
        checkpoint["stage"],
        serde_json::json!(JobState::Synthesizing.as_str()),
        "the persisted checkpoint must resume at synthesis: any earlier stage re-runs layout \
         and vision over the whole book in production, where those stages are not passthrough"
    );
    gate.release();
    wait_for_done(&pool, &job_id).await;
    orchestrator
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");

    let paths: Vec<String> = captured
        .lock()
        .unwrap()
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    assert!(
        paths.iter().all(|path| path == "/tts"),
        "a re-synthesis must resume at tts, not replay ingest, layout or vision: {paths:?}"
    );
    let requests = tts_requests(&captured);
    assert_eq!(
        requests.len(),
        1,
        "one /tts round trip, not one per segment"
    );
    let sent = requests[0]["segments"].as_array().expect("segments");
    assert_eq!(
        sent.len(),
        1,
        "exactly one segment was re-synthesized, got {sent:?}"
    );
    assert_eq!(sent[0]["text"], "A completely different sentence.");

    let after = chapter_render::list(&pool, &job_id).await.expect("rows");
    let untouched_after = after
        .iter()
        .find(|row| row.chapter_id == chapter_ids[1])
        .expect("chapter 2 row");
    assert_eq!(
        untouched_after.hash, untouched_before.hash,
        "an untouched chapter must keep its hash"
    );
    let edited_after = after
        .iter()
        .find(|row| row.chapter_id == chapter_ids[0])
        .expect("chapter 1 row");
    let edited_before = before
        .iter()
        .find(|row| row.chapter_id == chapter_ids[0])
        .expect("chapter 1 row");
    assert_ne!(
        edited_after.hash, edited_before.hash,
        "the edited chapter must be rebuilt"
    );

    let stored = job::list_segments(&pool, &job_id).await.expect("segments");
    let row = stored
        .iter()
        .find(|segment| segment.id == edited.id)
        .expect("edited segment");
    assert_eq!(row.text, "A completely different sentence.");
    assert_eq!(row.original_text.as_deref(), Some(edited.text.as_str()));
    assert_eq!(row.audio_hash.as_deref(), Some("edited01"));
}

#[tokio::test]
async fn re_synthesizing_a_failed_segment_drops_its_stale_verdict_from_the_report() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    let segments = seed_segments(&pool, &dir, &job_id, &chapter_ids).await;
    sqlx::query("UPDATE render_job SET verify = 1 WHERE id = ?")
        .bind(&job_id)
        .execute(&pool)
        .await
        .expect("verify on");

    let failed = segments[0].clone();
    let clean = segments[1].clone();
    for (segment, wer) in [(&failed, 0.42f64), (&clean, 0.0f64)] {
        job::upsert_alignment(
            &pool,
            &job::Alignment {
                segment_id: segment.id.clone(),
                source: "whisperx".into(),
                words_json: WordsJson::from_stored("[]".into()),
                wer: Some(wer),
            },
        )
        .await
        .expect("alignment");
    }
    job::set_segment_flags(&pool, &failed.id, r#"["verify_failed"]"#)
        .await
        .expect("flag");
    let stale = serde_json::json!({
        "enabled": true,
        "threshold": 0.15,
        "counts": {"ok": 5, "failed": 1, "missing": 0, "unscoreable_ref": 0},
        "worst": [{
            "segment_id": failed.id,
            "chapter_id": failed.chapter_id,
            "idx": failed.idx,
            "band": "failed",
            "wer": 0.42,
            "reference_words": 3,
        }],
    });
    job::merge_report_value(&pool, &job_id, "verify", stale.clone())
        .await
        .expect("stale report");

    let (port, _captured) = spawn_capturing_server(sse_body(&[
        (
            "result",
            r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"edited01","wav_bytes":100,"duration_ms":950,"cache_hit":false,"words":[],"flags":[]}}"#,
        ),
        ("done", r#"{"done":1,"total":1}"#),
    ]));
    write_cached_wav(&dir, "edited01", 950);
    let (progress, updates) = sinks();
    let gate = Gate::new();
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry_gated_at(&dir, port, JobState::Synthesizing, &gate),
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );
    orchestrator
        .resynthesize_segment(
            &job_id,
            &failed.id,
            SegmentEdit::Replace("A corrected sentence.".into()),
        )
        .await
        .expect("resynthesize");
    gate.wait_until_entered().await;

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("get")
        .expect("job");
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).expect("json");
    let verify = &report["verify"];
    assert_ne!(
        verify, &stale,
        "the editor path must recompute the shared verify block, not leave the pre-edit one"
    );
    assert_eq!(
        verify["counts"]["failed"], 0,
        "the edited segment's alignment row is gone, so nothing may still be counted as failed"
    );
    let worst = verify["worst"].as_array().expect("worst");
    assert!(
        worst.iter().all(|entry| entry["band"] != "failed"
            || entry["segment_id"] != serde_json::json!(failed.id)),
        "the edited segment must not still be listed with its pre-edit failure: {worst:?}"
    );
    assert_eq!(
        verify["counts"]["ok"], 1,
        "the untouched scored segment keeps its verdict"
    );

    gate.release();
    wait_for_done(&pool, &job_id).await;
    orchestrator
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn an_evicted_chapter_artifact_is_rebuilt_instead_of_failing_the_job() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    seed_segments(&pool, &dir, &job_id, &chapter_ids).await;

    render_once(&dir, &pool, &job_id).await;
    let before = chapter_render::list(&pool, &job_id).await.expect("rows");
    let materialised: Vec<_> = before
        .iter()
        .filter(|row| row.artifact == chapter_render::ARTIFACT_MP3)
        .collect();
    assert!(
        !materialised.is_empty(),
        "this fixture is meant to exercise the linear path"
    );
    let cache_dir = dir.path().join("cache");
    let mut evicted = 0;
    for entry in walk(&cache_dir) {
        if entry
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.len() == 64)
            .unwrap_or(false)
        {
            let hash = entry.file_name().unwrap().to_str().unwrap().to_string();
            if cache::get_entry(&pool, &hash)
                .await
                .expect("entry")
                .map(|row| row.kind == "chapter_mp3")
                .unwrap_or(false)
            {
                std::fs::remove_file(&entry).expect("evict");
                cache::remove(&pool, &hash).await.expect("evict row");
                evicted += 1;
            }
        }
    }
    assert!(
        evicted > 0,
        "the fixture must have materialised something to evict"
    );

    render_once(&dir, &pool, &job_id).await;

    let after = chapter_render::list(&pool, &job_id).await.expect("rows");
    let rebuilt = walk(&cache_dir)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.len() == 64)
                .unwrap_or(false)
        })
        .count();
    assert!(
        rebuilt >= evicted,
        "an evicted artifact must be rebuilt, not served missing"
    );
    assert_eq!(
        after.iter().map(|row| &row.hash).collect::<Vec<_>>(),
        before.iter().map(|row| &row.hash).collect::<Vec<_>>(),
        "rebuilding after eviction must reproduce the same content hashes"
    );
}

async fn loudness_block(pool: &SqlitePool, job_id: &str) -> serde_json::Value {
    let record = job::get_job(pool, job_id).await.expect("get").expect("job");
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).expect("json");
    report["loudness"].clone()
}

fn materialised_artifacts(dir: &tempfile::TempDir) -> usize {
    walk(&dir.path().join("cache"))
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.len() == 64)
                .unwrap_or(false)
        })
        .count()
}

#[tokio::test]
async fn a_job_that_flips_from_linear_to_dynamic_stops_serving_chapter_mp3s() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    let segments = seed_segments(&pool, &dir, &job_id, &chapter_ids).await;

    render_once(&dir, &pool, &job_id).await;
    assert_eq!(loudness_block(&pool, &job_id).await["linear"], true);
    let before = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert!(before
        .iter()
        .all(|row| row.artifact == chapter_render::ARTIFACT_MP3));
    assert!(before.iter().all(|row| row.encoded_ms.is_some()));
    let stale_artifacts = materialised_artifacts(&dir);
    assert!(stale_artifacts > 0);

    for segment in &segments {
        write_cached_wav_at(&dir, segment.audio_hash.as_deref().unwrap(), 900, -1.0);
    }
    render_once(&dir, &pool, &job_id).await;

    let block = loudness_block(&pool, &job_id).await;
    assert_eq!(block["linear"], false, "quiet audio must revert to dynamic");
    assert_eq!(block["chapter_artifacts"], "none");
    let after = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert_eq!(after.len(), before.len());
    assert!(
        after
            .iter()
            .all(|row| row.artifact == chapter_render::ARTIFACT_NONE),
        "no row may still claim an mp3 artifact after the gate flips"
    );
    assert!(
        after.iter().all(|row| row.encoded_ms.is_none()),
        "a row with no artifact cannot carry an encoded duration"
    );
    assert_eq!(
        after.iter().map(|row| row.hash.clone()).collect::<Vec<_>>(),
        before
            .iter()
            .map(|row| row.hash.clone())
            .collect::<Vec<_>>(),
        "chapter identity is chapter-local, so a loudness flip must not disturb it"
    );
}

#[tokio::test]
async fn a_job_that_flips_from_dynamic_to_linear_materialises_fresh_artifacts() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    let segments = seed_segments(&pool, &dir, &job_id, &chapter_ids).await;
    for segment in &segments {
        write_cached_wav_at(&dir, segment.audio_hash.as_deref().unwrap(), 900, -1.0);
    }

    render_once(&dir, &pool, &job_id).await;
    assert_eq!(loudness_block(&pool, &job_id).await["linear"], false);
    let before = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert!(before
        .iter()
        .all(|row| row.artifact == chapter_render::ARTIFACT_NONE));
    assert_eq!(
        materialised_artifacts(&dir),
        0,
        "the dynamic path must not leave chapter artifacts behind"
    );

    for segment in &segments {
        write_cached_wav_at(&dir, segment.audio_hash.as_deref().unwrap(), 900, 12_000.0);
    }
    render_once(&dir, &pool, &job_id).await;

    let block = loudness_block(&pool, &job_id).await;
    assert_eq!(block["linear"], true);
    let after = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert!(after
        .iter()
        .all(|row| row.artifact == chapter_render::ARTIFACT_MP3));
    assert!(after.iter().all(|row| row.encoded_ms.is_some()));
    assert_eq!(
        materialised_artifacts(&dir),
        after.len(),
        "one fresh artifact per chapter"
    );
}

#[tokio::test]
async fn a_truncated_chapter_artifact_is_re_encoded_instead_of_trusted() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    seed_segments(&pool, &dir, &job_id, &chapter_ids).await;

    render_once(&dir, &pool, &job_id).await;
    let before = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert!(before
        .iter()
        .all(|row| row.artifact == chapter_render::ARTIFACT_MP3));
    let artifacts: Vec<std::path::PathBuf> = walk(&dir.path().join("cache"))
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.len() == 64)
                .unwrap_or(false)
        })
        .collect();
    assert!(!artifacts.is_empty());

    let victim = artifacts[0].clone();
    let full = std::fs::read(&victim).expect("read");
    let truncated = &full[..full.len() / 3];
    std::fs::write(&victim, truncated).expect("truncate");
    let truncated_len = std::fs::metadata(&victim).expect("meta").len();
    assert!(truncated_len < full.len() as u64);

    render_once(&dir, &pool, &job_id).await;

    let restored = std::fs::metadata(&victim).expect("meta").len();
    assert_eq!(
        restored,
        full.len() as u64,
        "a truncated artifact must be re-encoded, not trusted because the xing header \
         still declares the full duration"
    );
    let after = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert_eq!(
        after.iter().map(|row| row.encoded_ms).collect::<Vec<_>>(),
        before.iter().map(|row| row.encoded_ms).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn editing_the_only_chapter_of_a_job_refuses_to_serve_the_pre_edit_export() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    let single = vec![chapter_ids[0].clone()];
    let segments = seed_segments(&pool, &dir, &job_id, &single).await;

    render_once(&dir, &pool, &job_id).await;
    let rendered = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert_eq!(
        rendered.len(),
        1,
        "this fixture is the single-chapter scope"
    );
    let record = job::get_job(&pool, &job_id)
        .await
        .expect("get")
        .expect("job");
    let exported: Vec<String> =
        serde_json::from_str(record.output_paths_json.as_deref().expect("paths")).expect("json");
    let path = exported[0].clone();
    assert!(
        !export::is_path_stale(&pool, &path).await.expect("check"),
        "the freshly rendered export must be playable"
    );

    let reply = sse_body(&[
        (
            "result",
            r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"edited01","wav_bytes":100,"duration_ms":950,"cache_hit":false,"words":[],"flags":[]}}"#,
        ),
        ("done", r#"{"done":1,"total":1}"#),
    ]);
    let (port, _captured) = spawn_capturing_server(reply);
    write_cached_wav(&dir, "edited01", 950);
    let (progress, updates) = sinks();
    let gate = Gate::new();
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry_gated_at(&dir, port, JobState::Synthesizing, &gate),
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );
    orchestrator
        .resynthesize_segment(
            &job_id,
            &segments[0].id,
            SegmentEdit::Replace("A different opening sentence.".into()),
        )
        .await
        .expect("resynthesize");
    gate.wait_until_entered().await;

    assert!(
        chapter_render::list(&pool, &job_id)
            .await
            .expect("rows")
            .is_empty(),
        "the edit drops the only chapter row, which is the state that used to be amnestied"
    );
    assert!(
        export::is_path_stale(&pool, &path).await.expect("check"),
        "while the re-render is pending, the pre-edit audio must not be served: it no longer \
         matches the sentence the user just changed"
    );

    gate.release();
    wait_for_done(&pool, &job_id).await;
    orchestrator
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
    assert!(
        !export::is_path_stale(&pool, &path).await.expect("check"),
        "a completed re-render republishes the digest and heals the export"
    );
}

#[tokio::test]
async fn reverting_the_only_chapter_of_a_job_also_refuses_the_pre_edit_export() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    let single = vec![chapter_ids[0].clone()];
    let segments = seed_segments(&pool, &dir, &job_id, &single).await;
    let original = segments[0].text.clone();

    render_once(&dir, &pool, &job_id).await;
    let record = job::get_job(&pool, &job_id)
        .await
        .expect("get")
        .expect("job");
    let exported: Vec<String> =
        serde_json::from_str(record.output_paths_json.as_deref().expect("paths")).expect("json");
    let path = exported[0].clone();

    let reply = sse_body(&[
        (
            "result",
            r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"edited02","wav_bytes":100,"duration_ms":940,"cache_hit":false,"words":[],"flags":[]}}"#,
        ),
        ("done", r#"{"done":1,"total":1}"#),
    ]);
    let (port, _captured) = spawn_capturing_server(reply);
    write_cached_wav(&dir, "edited02", 940);
    sqlx::query("UPDATE render_job SET verify = 1 WHERE id = ?")
        .bind(&job_id)
        .execute(&pool)
        .await
        .expect("verify on");
    let (progress, updates) = sinks();
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry(&dir, port),
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );
    orchestrator
        .resynthesize_segment(
            &job_id,
            &segments[0].id,
            SegmentEdit::Replace("Something else entirely.".into()),
        )
        .await
        .expect("edit");
    wait_for_done(&pool, &job_id).await;

    let reverted = sse_body(&[
        (
            "result",
            r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"seg0000","wav_bytes":100,"duration_ms":900,"cache_hit":true,"words":[],"flags":[]}}"#,
        ),
        ("done", r#"{"done":1,"total":1}"#),
    ]);
    let (revert_port, _revert_captured) = spawn_capturing_server(reverted);
    let gate = Gate::new();
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: segments[0].id.clone(),
            source: "whisperx".into(),
            words_json: WordsJson::from_stored("[]".into()),
            wer: Some(0.51),
        },
    )
    .await
    .expect("alignment");
    job::merge_report_value(
        &pool,
        &job_id,
        "verify",
        serde_json::json!({
            "enabled": true,
            "threshold": 0.15,
            "counts": {"ok": 1, "failed": 1, "missing": 0, "unscoreable_ref": 0},
            "worst": [{
                "segment_id": segments[0].id,
                "chapter_id": segments[0].chapter_id,
                "idx": segments[0].idx,
                "band": "failed",
                "wer": 0.51,
                "reference_words": 3,
            }],
        }),
    )
    .await
    .expect("stale verify block");
    let (progress, updates) = sinks();
    let reverting = JobOrchestrator::start_with_options(
        pool.clone(),
        registry_gated_at(&dir, revert_port, JobState::Synthesizing, &gate),
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );
    reverting
        .resynthesize_segment(&job_id, &segments[0].id, SegmentEdit::Revert)
        .await
        .expect("revert");
    gate.wait_until_entered().await;

    let stored = job::list_segments(&pool, &job_id).await.expect("segments");
    let row = stored
        .iter()
        .find(|segment| segment.id == segments[0].id)
        .expect("reverted segment");
    assert_eq!(
        row.text, original,
        "revert must restore the text the pipeline generated"
    );
    assert_eq!(
        row.original_text, None,
        "once reverted the row is unedited again, so nothing offers to revert it twice"
    );
    assert_eq!(row.audio_hash, None, "the revert clears the stale audio");
    let queued = job::get_job(&pool, &job_id)
        .await
        .expect("get")
        .expect("job");
    let checkpoint: serde_json::Value =
        serde_json::from_str(queued.checkpoint_json.as_deref().expect("checkpoint"))
            .expect("checkpoint json");
    assert_eq!(
        checkpoint["stage"],
        serde_json::json!(JobState::Synthesizing.as_str()),
        "a revert resumes at synthesis exactly like an edit"
    );
    assert!(
        chapter_render::list(&pool, &job_id)
            .await
            .expect("rows")
            .is_empty(),
        "a revert invalidates its chapter too"
    );
    assert!(
        export::is_path_stale(&pool, &path).await.expect("check"),
        "a revert changes the audio just as much as an edit does, so the same refusal applies"
    );
    let report: serde_json::Value =
        serde_json::from_str(queued.report_json.as_deref().expect("report")).expect("json");
    assert_eq!(
        report["verify"]["counts"]["failed"], 0,
        "a revert must recompute the shared verify block too: its alignment row is gone, so          the pre-revert failure describes nothing"
    );
    assert!(
        report["verify"]["worst"]
            .as_array()
            .expect("worst")
            .iter()
            .all(|entry| entry["band"] != "failed"),
        "the reverted segment must not still be listed as failed: {}",
        report["verify"]["worst"]
    );

    gate.release();
    wait_for_done(&pool, &job_id).await;
    reverting
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
    assert!(
        !export::is_path_stale(&pool, &path).await.expect("check"),
        "the re-render after a revert heals the export as well"
    );
}

#[tokio::test]
async fn a_rerender_resumes_at_render_and_is_refused_on_an_unfinished_job() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    seed_segments(&pool, &dir, &job_id, &chapter_ids).await;

    let (progress, updates) = sinks();
    let gate = Gate::new();
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry_gated_at(&dir, 0, JobState::Rendering, &gate),
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );

    job::update_job_state(&pool, &job_id, JobState::Rendering.as_str())
        .await
        .expect("state");
    let refused = orchestrator
        .rerender_job(&job_id)
        .await
        .expect_err("a running job cannot be re-rendered");
    assert!(
        refused.to_string().contains("finished"),
        "the refusal must say why: {refused}"
    );

    job::update_job_state(&pool, &job_id, JobState::Done.as_str())
        .await
        .expect("state");
    orchestrator.rerender_job(&job_id).await.expect("rerender");
    gate.wait_until_entered().await;
    let queued = job::get_job(&pool, &job_id)
        .await
        .expect("get")
        .expect("job");
    let checkpoint: serde_json::Value =
        serde_json::from_str(queued.checkpoint_json.as_deref().expect("checkpoint"))
            .expect("checkpoint json");
    assert_eq!(
        checkpoint["stage"],
        serde_json::json!(JobState::Rendering.as_str()),
        "a re-render must resume at render and reuse every cached segment and chapter artifact"
    );

    gate.release();
    wait_for_done(&pool, &job_id).await;
    orchestrator
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
    let rows = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert_eq!(rows.len(), chapter_ids.len());
}

#[tokio::test]
async fn a_chapter_that_loses_its_last_playable_segment_loses_its_row() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    seed_segments(&pool, &dir, &job_id, &chapter_ids).await;

    render_once(&dir, &pool, &job_id).await;
    let before = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert_eq!(before.len(), 3);
    let dropped = chapter_ids[2].clone();
    let stale = before
        .iter()
        .find(|row| row.chapter_id == dropped)
        .expect("the chapter about to be emptied")
        .clone();
    assert_eq!(stale.artifact, chapter_render::ARTIFACT_MP3);
    assert!(stale.encoded_ms.is_some());

    sqlx::query(
        "UPDATE script_segment SET audio_hash = NULL, duration_ms = NULL WHERE chapter_id = ?",
    )
    .bind(&dropped)
    .execute(&pool)
    .await
    .expect("empty the last chapter");

    render_once(&dir, &pool, &job_id).await;

    let after = chapter_render::list(&pool, &job_id).await.expect("rows");
    assert_eq!(
        after.len(),
        2,
        "an edit that removes a chapter's last playable segment must drop its row, not \
         leave a stale mp3 row claiming an encoded duration"
    );
    assert!(
        after.iter().all(|row| row.chapter_id != dropped),
        "the emptied chapter must not survive as a stale row: {after:?}"
    );
    assert_eq!(
        after.iter().map(|row| row.idx).collect::<Vec<_>>(),
        vec![0, 1],
        "the surviving rows must be renumbered into a contiguous export order"
    );
}

const EXPECTED_TOTAL_MS: i64 = 9000;
const CODEC_TOLERANCE_MS: i64 = 120;
const ENCODER_PADDING_PER_CHAPTER_MS: i64 = 60;

fn exported_chapters(dir: &tempfile::TempDir) -> Vec<(String, i64, i64)> {
    let export = exported_mp3(dir);
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_chapters"])
        .arg(&export)
        .output()
        .expect("ffprobe");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("show_chapters json");
    parsed["chapters"]
        .as_array()
        .expect("chapters")
        .iter()
        .map(|chapter| {
            let start: f64 = chapter["start_time"]
                .as_str()
                .expect("start_time")
                .parse()
                .expect("start seconds");
            let end: f64 = chapter["end_time"]
                .as_str()
                .expect("end_time")
                .parse()
                .expect("end seconds");
            (
                chapter["tags"]["title"]
                    .as_str()
                    .expect("title")
                    .to_string(),
                (start * 1000.0).round() as i64,
                (end * 1000.0).round() as i64,
            )
        })
        .collect()
}

fn exported_mp3(dir: &tempfile::TempDir) -> std::path::PathBuf {
    walk(&dir.path().join("exports"))
        .into_iter()
        .find(|path| path.extension().and_then(|e| e.to_str()) == Some("mp3"))
        .expect("an exported mp3")
}

fn exported_duration_ms(dir: &tempfile::TempDir) -> i64 {
    let export = exported_mp3(dir);
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(&export)
        .output()
        .expect("ffprobe");
    let seconds: f64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("duration");
    (seconds * 1000.0).round() as i64
}

#[tokio::test]
async fn the_linear_path_keeps_every_chapter_gap() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    seed_segments(&pool, &dir, &job_id, &chapter_ids).await;

    render_once(&dir, &pool, &job_id).await;
    assert_eq!(loudness_block(&pool, &job_id).await["linear"], true);

    let total = exported_duration_ms(&dir);
    let rows = chapter_render::list(&pool, &job_id).await.expect("rows");
    let budget = ENCODER_PADDING_PER_CHAPTER_MS * rows.len() as i64;
    assert!(
        total - EXPECTED_TOTAL_MS >= 0 && total - EXPECTED_TOTAL_MS <= budget,
        "6 segments of 900 ms, 3 segment gaps of 400 ms and 2 chapter gaps of 1200 ms \
         must total {EXPECTED_TOTAL_MS} ms plus at most one encoder delay per chapter \
         ({budget} ms), got {total} ms"
    );
    let sum: i64 = rows.iter().map(|row| row.encoded_ms.unwrap_or(0)).sum();
    assert!(
        (sum - total).abs() <= CODEC_TOLERANCE_MS,
        "the markers must add up to the file they describe: {sum} vs {total}"
    );
}

#[tokio::test]
async fn the_dynamic_path_produces_the_same_length_as_the_linear_path() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    let segments = seed_segments(&pool, &dir, &job_id, &chapter_ids).await;
    for segment in &segments {
        write_cached_wav_at(&dir, segment.audio_hash.as_deref().unwrap(), 900, -1.0);
    }

    render_once(&dir, &pool, &job_id).await;
    assert_eq!(loudness_block(&pool, &job_id).await["linear"], false);

    let total = exported_duration_ms(&dir);
    assert!(
        (total - EXPECTED_TOTAL_MS).abs() <= CODEC_TOLERANCE_MS,
        "the two export paths must produce the same audiobook length, expected \
         {EXPECTED_TOTAL_MS} ms, got {total} ms"
    );
}

#[tokio::test]
async fn both_export_paths_place_every_chapter_marker_at_the_same_point() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id, chapter_ids) = seed(&pool).await;
    let job_id = insert_done_job(&pool, &book_id, &voice_id).await;
    let segments = seed_segments(&pool, &dir, &job_id, &chapter_ids).await;
    for segment in &segments {
        write_cached_wav_at(&dir, segment.audio_hash.as_deref().unwrap(), 900, -1.0);
    }
    render_once(&dir, &pool, &job_id).await;
    assert_eq!(loudness_block(&pool, &job_id).await["linear"], false);
    let dynamic = exported_chapters(&dir);

    for segment in &segments {
        write_cached_wav_at(&dir, segment.audio_hash.as_deref().unwrap(), 900, 12_000.0);
    }
    render_once(&dir, &pool, &job_id).await;
    assert_eq!(loudness_block(&pool, &job_id).await["linear"], true);
    let linear = exported_chapters(&dir);

    assert_eq!(
        linear.len(),
        chapter_ids.len(),
        "every chapter must be marked on the linear path"
    );
    assert_eq!(
        linear.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
        dynamic.iter().map(|c| c.0.clone()).collect::<Vec<_>>()
    );
    for (position, (both, gaps_before)) in linear.iter().zip(dynamic.iter()).zip(0..).enumerate() {
        let (linear, dynamic) = both;
        let budget = CODEC_TOLERANCE_MS + ENCODER_PADDING_PER_CHAPTER_MS * gaps_before;
        assert!(
            (linear.1 - dynamic.1).abs() <= budget,
            "chapter {position} ({}) starts at {} ms on the linear path and {} ms on the \
             dynamic path: seeking to a chapter must land on speech on both paths, so the \
             1200 ms gap belongs to the silence before the chapter, not inside it",
            linear.0,
            linear.1,
            dynamic.1
        );
        assert!(
            (linear.2 - dynamic.2).abs() <= budget + ENCODER_PADDING_PER_CHAPTER_MS,
            "chapter {position} ({}) ends at {} ms on the linear path and {} ms on the \
             dynamic path",
            linear.0,
            linear.2,
            dynamic.2
        );
    }
}
