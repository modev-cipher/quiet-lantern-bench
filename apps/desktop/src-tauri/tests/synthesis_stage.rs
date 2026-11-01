#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use txtcaster_lib::orchestrator::stage::{
    control_channel, ProgressEvent, ProgressSink, Stage, StageContext, StageOutcome,
};
use txtcaster_lib::orchestrator::state::{Checkpoint, JobState, OrchestratorError, Scope};
use txtcaster_lib::orchestrator::{SidecarProvider, SynthesizingStage};
use txtcaster_lib::storage::{self, book, cache, job, models, new_id, now_ms, voice, SqlitePool};
use txtcaster_lib::words::{self, Timebase, WordProvenance};

struct FakeSidecar {
    port: u16,
}

impl SidecarProvider for FakeSidecar {
    fn connection(&self) -> Option<(u16, String)> {
        Some((self.port, "test-token".into()))
    }
}

struct NoSidecar;

impl SidecarProvider for NoSidecar {
    fn connection(&self) -> Option<(u16, String)> {
        None
    }
}

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

async fn seed(pool: &SqlitePool, install_model: bool) -> (String, String) {
    let book = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
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
    let chapter = book::Chapter {
        id: "chap-0".into(),
        book_id: book.id.clone(),
        idx: 0,
        title: "Chapter 0".into(),
        page_start: 1,
        page_end: 10,
        is_pseudo: false,
        word_count: Some(100),
    };
    book::insert_chapter(pool, &chapter).await.expect("chapter");
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
    if install_model {
        let install = models::ModelInstall {
            model_id: "kokoro-82m".into(),
            path: "tts/kokoro-82m".into(),
            sha256: "deadbeef".into(),
            disk_bytes: 1,
            source: "curated".into(),
            installed_at: now_ms(),
        };
        models::insert_model_install(pool, &install)
            .await
            .expect("install");
    }
    (book.id, profile.id)
}

async fn insert_job(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
    insert_job_seeded(pool, book_id, voice_id, None).await
}

async fn insert_job_seeded(
    pool: &SqlitePool,
    book_id: &str,
    voice_id: &str,
    tts_seed: Option<i64>,
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
        tts_plan_json: Some(
            r#"{"model_id":"kokoro-82m","device":"cpu","tts_workers":2,"batch":2,"badge":"green"}"#
                .into(),
        ),
        align_plan_json: None,
        state: JobState::Synthesizing.as_str().into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(pool, &record).await.expect("job");
    job_id
}

fn make_segment(job_id: &str, idx: i64, text: &str) -> job::ScriptSegment {
    job::ScriptSegment {
        id: new_id(),
        job_id: job_id.into(),
        chapter_id: "chap-0".into(),
        idx,
        page_no: 1,
        kind: "text".into(),
        figure_id: None,
        text: text.into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: None,
        duration_ms: None,
        original_text: None,
    }
}

fn sse_body(events: &[(&str, &str)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn spawn_sse_server(body: String) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let bytes = body.into_bytes();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let bytes = bytes.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut req = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = mock_http::write_response(&mut sock, "200 OK", "text/event-stream", &bytes)
                    .await;
            });
        }
    });
    port
}

fn noop_progress() -> (ProgressSink, Arc<Mutex<Vec<ProgressEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = events.clone();
    let sink: ProgressSink = Arc::new(move |event| sink_events.lock().unwrap().push(event));
    (sink, events)
}

fn make_stage(provider: Arc<dyn SidecarProvider>, dir: &tempfile::TempDir) -> SynthesizingStage {
    SynthesizingStage::new(
        provider,
        dir.path().join("models"),
        dir.path().join("cache"),
    )
}

async fn run_stage(
    stage: &dyn Stage,
    pool: &SqlitePool,
    job_id: &str,
) -> Result<StageOutcome, OrchestratorError> {
    let (_handle, receiver) = control_channel();
    let (progress, _events) = noop_progress();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.to_string(),
        JobState::Synthesizing,
        Checkpoint::new(JobState::Synthesizing, now_ms()),
        receiver,
        progress,
    );
    stage.run(&mut ctx).await
}

#[tokio::test]
async fn an_engine_that_declares_no_timebase_leaves_its_words_stale() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "First sentence.")])
        .await
        .expect("segments");

    let undeclared = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":true,"words":[{"w":"First","s":0,"e":400}],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", undeclared),
        ("done", r#"{"done":1,"total":1}"#),
    ]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    assert_eq!(
        run_stage(&stage, &pool, &job_id).await.expect("run"),
        StageOutcome::Completed
    );

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    let alignment = job::get_alignment(&pool, &segments[0].id)
        .await
        .expect("query")
        .expect("alignment row");
    let payload = alignment.words_json.parse();
    assert_eq!(payload.timebase, Timebase::Unknown);
    assert_eq!(
        words::classify(&payload, &alignment.source),
        WordProvenance::Stale
    );
}

#[tokio::test]
async fn an_engine_that_declares_the_trimmed_timebase_makes_its_words_fresh() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "First sentence.")])
        .await
        .expect("segments");

    let declared = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":false,"words":[{"w":"First","s":0,"e":400}],"flags":[],"words_timebase":"trimmed"}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", declared),
        ("done", r#"{"done":1,"total":1}"#),
    ]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    assert_eq!(
        run_stage(&stage, &pool, &job_id).await.expect("run"),
        StageOutcome::Completed
    );

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    let alignment = job::get_alignment(&pool, &segments[0].id)
        .await
        .expect("query")
        .expect("alignment row");
    let payload = alignment.words_json.parse();
    assert!(payload.versioned);
    assert_eq!(payload.timebase, Timebase::Trimmed);
    assert_eq!(
        words::classify(&payload, &alignment.source),
        WordProvenance::Fresh
    );
}

#[tokio::test]
async fn synthesizing_stage_persists_audio_alignment_and_cache() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(
        &pool,
        &[
            make_segment(&job_id, 0, "First sentence."),
            make_segment(&job_id, 1, "Second sentence."),
        ],
    )
    .await
    .expect("segments");

    let first = r#"{"done":1,"total":2,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":false,"words":[{"w":"First","s":0,"e":400},{"w":"sentence.","s":400,"e":800}],"flags":[]}}"#;
    let second = r#"{"done":2,"total":2,"result":{"idx":1,"audio_hash":"bbbb","wav_bytes":120,"duration_ms":900,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", first),
        ("result", second),
        ("done", r#"{"done":2,"total":2}"#),
    ]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments[0].audio_hash.as_deref(), Some("aaaa"));
    assert_eq!(segments[0].duration_ms, Some(800));
    assert_eq!(segments[1].audio_hash.as_deref(), Some("bbbb"));

    let alignment = job::get_alignment(&pool, &segments[0].id)
        .await
        .expect("alignment query")
        .expect("alignment row");
    assert_eq!(alignment.source, "tts_native");
    assert!(alignment.words_json.as_str().contains("First"));
    assert!(job::get_alignment(&pool, &segments[1].id)
        .await
        .expect("alignment query")
        .is_none());

    let entry = cache::get_entry(&pool, "aaaa").await.expect("cache query");
    assert_eq!(entry.map(|e| e.kind), Some("wav".into()));

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.total_segments, 2);
    assert_eq!(checkpoint.done_segments, 2);
}

#[tokio::test]
async fn synthesizing_stage_skips_already_synthesized_segments() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let mut done_segment = make_segment(&job_id, 0, "Done already.");
    done_segment.audio_hash = Some("cccc".into());
    done_segment.duration_ms = Some(500);

    let cache_dir = dir.path().join("cache");
    let wav_path = txtcaster_lib::render::mp3::cache_wav_path(&cache_dir, "cccc");
    std::fs::create_dir_all(wav_path.parent().unwrap()).unwrap();
    std::fs::write(&wav_path, txtcaster_lib::render::mp3::silence_wav(500)).unwrap();

    job::insert_segments(
        &pool,
        &[done_segment, make_segment(&job_id, 1, "Still pending.")],
    )
    .await
    .expect("segments");

    let request_log = Arc::new(Mutex::new(Vec::new()));
    let payload = r#"{"done":1,"total":1,"result":{"idx":1,"audio_hash":"dddd","wav_bytes":90,"duration_ms":700,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_recording_sse_server(
        sse_body(&[("result", payload), ("done", r#"{"done":1,"total":1}"#)]),
        request_log.clone(),
    );

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments[0].audio_hash.as_deref(), Some("cccc"));
    assert_eq!(segments[1].audio_hash.as_deref(), Some("dddd"));

    let requests = request_log.lock().unwrap().join("");
    assert!(requests.contains("Still pending."));
    assert!(!requests.contains("Done already."));
}

#[tokio::test]
async fn synthesizing_stage_completes_when_all_segments_have_audio() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let mut segment = make_segment(&job_id, 0, "Cached.");
    segment.audio_hash = Some("eeee".into());

    let cache_dir = dir.path().join("cache");
    let wav_path = txtcaster_lib::render::mp3::cache_wav_path(&cache_dir, "eeee");
    std::fs::create_dir_all(wav_path.parent().unwrap()).unwrap();
    std::fs::write(&wav_path, txtcaster_lib::render::mp3::silence_wav(100)).unwrap();

    job::insert_segments(&pool, &[segment]).await.expect("seg");

    let stage = make_stage(Arc::new(NoSidecar), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);
}

#[tokio::test]
async fn synthesizing_stage_flags_failed_segment_and_continues() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(
        &pool,
        &[
            make_segment(&job_id, 0, "Good sentence."),
            make_segment(&job_id, 1, "Poisoned sentence."),
        ],
    )
    .await
    .expect("segments");

    let good = r#"{"done":1,"total":2,"result":{"idx":0,"audio_hash":"ffff","wav_bytes":80,"duration_ms":600,"cache_hit":false,"words":[],"flags":[]}}"#;
    let failed = r#"{"done":2,"total":2,"result":{"idx":1,"audio_hash":null,"wav_bytes":0,"duration_ms":0,"cache_hit":false,"words":[],"flags":["tts_failed"]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", good),
        ("result", failed),
        ("done", r#"{"done":2,"total":2}"#),
    ]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments[0].audio_hash.as_deref(), Some("ffff"));
    assert!(segments[1].audio_hash.is_none());
    assert_eq!(segments[1].flags_json.as_deref(), Some(r#"["tts_failed"]"#));
}

#[tokio::test]
async fn synthesizing_stage_errors_when_model_not_installed() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Hello.")])
        .await
        .expect("segments");

    let stage = make_stage(Arc::new(NoSidecar), &dir);
    let result = run_stage(&stage, &pool, &job_id).await;
    assert!(matches!(
        result,
        Err(OrchestratorError::ModelNotInstalled(id)) if id == "kokoro-82m"
    ));
}

#[tokio::test]
async fn synthesizing_stage_without_sidecar_honors_cancel_while_waiting() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Hello.")])
        .await
        .expect("segments");

    let stage = make_stage(Arc::new(NoSidecar), &dir);
    let (handle, receiver) = control_channel();
    let (progress, _events) = noop_progress();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.clone(),
        JobState::Synthesizing,
        Checkpoint::new(JobState::Synthesizing, now_ms()),
        receiver,
        progress,
    );
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.cancel();
    });
    let outcome = stage.run(&mut ctx).await.expect("run");
    canceller.await.expect("canceller");
    assert_eq!(outcome, StageOutcome::Cancelled);
}

#[tokio::test]
async fn synthesizing_stage_reports_malformed_results() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Hello.")])
        .await
        .expect("segments");

    let payload = r#"{"done":1,"total":1,"result":{"idx":"not-a-number"}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", payload),
        ("done", r#"{"done":1,"total":1}"#),
    ]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let result = run_stage(&stage, &pool, &job_id).await;
    match result {
        Err(OrchestratorError::Sidecar(msg)) => {
            assert!(msg.contains("failed to parse tts result"), "got: {msg}")
        }
        other => panic!("expected sidecar parse error, got {other:?}"),
    }
}

#[tokio::test]
async fn synthesizing_stage_propagates_sidecar_error() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Hello.")])
        .await
        .expect("segments");

    let port = spawn_sse_server(sse_body(&[(
        "error",
        r#"{"code":"ERR_TTS_MODEL","message":"missing"}"#,
    )]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let result = run_stage(&stage, &pool, &job_id).await;
    let error = result.expect_err("expected sidecar error");
    assert_eq!(error.engine_code(), Some("ERR_TTS_MODEL"), "{error}");
}

fn spawn_stalling_sse_server(body: String) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let bytes = body.into_bytes();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let bytes = bytes.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut req = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = mock_http::write_head(
                    &mut sock,
                    "200 OK",
                    &[("Content-Type", "text/event-stream")],
                    Some(bytes.len() + 4096),
                )
                .await;
                let _ = sock.write_all(&bytes).await;
                let _ = sock.flush().await;
                let mut hold = vec![0u8; 1];
                let _ = sock.read(&mut hold).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn synthesizing_stage_pause_mid_stream_persists_completed_segments() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(
        &pool,
        &[
            make_segment(&job_id, 0, "First sentence."),
            make_segment(&job_id, 1, "Second sentence."),
        ],
    )
    .await
    .expect("segments");

    let first = r#"{"done":1,"total":2,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_stalling_sse_server(sse_body(&[("result", first)]));

    let (handle, receiver) = control_channel();
    let (progress, _events) = noop_progress();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.clone(),
        JobState::Synthesizing,
        Checkpoint::new(JobState::Synthesizing, now_ms()),
        receiver,
        progress,
    );
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);

    let pause_pool = pool.clone();
    let pause_job = job_id.clone();
    let pauser = tokio::spawn(async move {
        loop {
            let segments = job::list_segments(&pause_pool, &pause_job)
                .await
                .expect("list");
            if segments[0].audio_hash.is_some() {
                handle.pause();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });

    let outcome = stage.run(&mut ctx).await.expect("run");
    pauser.await.expect("pauser");
    assert_eq!(outcome, StageOutcome::Paused);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments[0].audio_hash.as_deref(), Some("aaaa"));
    assert!(segments[1].audio_hash.is_none());

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.done_segments, 1);
    assert_eq!(checkpoint.total_segments, 2);
}

#[tokio::test]
async fn synthesizing_stage_resume_after_pause_sends_fresh_run_id() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Hello.")])
        .await
        .expect("segments");

    let request_log = Arc::new(Mutex::new(Vec::new()));
    let payload = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"hhhh","wav_bytes":60,"duration_ms":500,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_recording_sse_server(
        sse_body(&[("result", payload), ("done", r#"{"done":1,"total":1}"#)]),
        request_log.clone(),
    );
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);

    let outcome = run_stage(&stage, &pool, &job_id).await.expect("first run");
    assert_eq!(outcome, StageOutcome::Completed);
    sqlx::query("UPDATE script_segment SET audio_hash = NULL WHERE job_id = ?")
        .bind(&job_id)
        .execute(&pool)
        .await
        .expect("reset");
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("second run");
    assert_eq!(outcome, StageOutcome::Completed);

    let requests = request_log.lock().unwrap();
    let run_ids: Vec<String> = requests
        .iter()
        .filter_map(|req| {
            req.lines().find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("x-job-id:")
                    .map(|_| {
                        line.split(':')
                            .skip(1)
                            .collect::<Vec<_>>()
                            .join(":")
                            .trim()
                            .to_string()
                    })
            })
        })
        .collect();
    assert_eq!(run_ids.len(), 2);
    assert_ne!(run_ids[0], run_ids[1]);
    assert!(run_ids[0].starts_with(&format!("{job_id}:")));
    assert!(run_ids[1].starts_with(&format!("{job_id}:")));
}

#[tokio::test]
async fn synthesizing_stage_errors_when_stream_ends_without_done() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Hello.")])
        .await
        .expect("segments");

    let payload = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"gggg","wav_bytes":50,"duration_ms":400,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[("result", payload)]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let result = run_stage(&stage, &pool, &job_id).await;
    match result {
        Err(OrchestratorError::Sidecar(msg)) => {
            assert!(msg.contains("ended without completion"), "got: {msg}")
        }
        other => panic!("expected incomplete stream error, got {other:?}"),
    }
}

fn spawn_recording_sse_server(body: String, log: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let bytes = body.into_bytes();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let bytes = bytes.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut req = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req);
                    if let Some(pos) = text.find("\r\n\r\n") {
                        let headers = &text[..pos];
                        let content_len = headers
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(str::trim)
                                    .map(String::from)
                            })
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        if req.len() >= pos + 4 + content_len {
                            break;
                        }
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req).into_owned());
                let _ = mock_http::write_response(&mut sock, "200 OK", "text/event-stream", &bytes)
                    .await;
            });
        }
    });
    port
}

#[tokio::test]
async fn a_legacy_tts_plan_is_upcast_and_rewritten_before_the_key_is_recorded() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let legacy =
        r#"{"model_id":"kokoro-82m","device":"cuda:1","tts_workers":2,"batch":2,"badge":"green"}"#;
    job::set_job_tts_plan(&pool, &job_id, legacy)
        .await
        .expect("seed legacy tts plan");
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "First sentence.")])
        .await
        .expect("segments");

    let result = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", result),
        ("done", r#"{"done":1,"total":1}"#),
    ]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let stored = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row")
        .tts_plan_json
        .expect("tts plan");
    let stored: serde_json::Value = serde_json::from_str(&stored).expect("plan json");
    assert_eq!(
        stored["plan_revision"], 2,
        "the tts column bypassed the upcast choke point, so a legacy row stayed legacy \
         however many stages ran over it"
    );
    assert_eq!(stored["device"]["backend"], "cuda");
    assert_eq!(stored["device"]["ordinal"], 1);
}

#[tokio::test]
async fn synthesizing_stage_records_learned_tts_rtf() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(
        &pool,
        &[
            make_segment(&job_id, 0, "First sentence."),
            make_segment(&job_id, 1, "Second sentence."),
        ],
    )
    .await
    .expect("segments");

    let first = r#"{"done":1,"total":2,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":false,"words":[],"flags":[]}}"#;
    let second = r#"{"done":2,"total":2,"result":{"idx":1,"audio_hash":"bbbb","wav_bytes":120,"duration_ms":900,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", first),
        ("result", second),
        ("done", r#"{"done":2,"total":2}"#),
    ]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let learned = models::load_learned(&pool).await.expect("learned");
    assert!(
        learned.tts_rtf("kokoro-82m", "cpu").is_some(),
        "expected a learned rtf sample for kokoro-82m:cpu"
    );
}

#[tokio::test]
async fn synthesizing_stage_skips_learning_when_all_cache_hits() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "First sentence.")])
        .await
        .expect("segments");

    let hit = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":true,"words":[],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", hit),
        ("done", r#"{"done":1,"total":1}"#),
    ]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let learned = models::load_learned(&pool).await.expect("learned");
    assert!(
        learned.tts_rtf("kokoro-82m", "cpu").is_none(),
        "cache-only run must not record an rtf sample"
    );
}

#[tokio::test]
async fn synthesizing_stage_records_cpu_fallback_flag_in_the_report() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "One sentence.")])
        .await
        .expect("segments");

    let result = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", result),
        (
            "done",
            r#"{"done":1,"total":1,"flags":["tts_cpu_fallback"]}"#,
        ),
    ]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).expect("json");
    assert_eq!(
        report["tts_flags"],
        serde_json::json!(["tts_cpu_fallback"]),
        "a cpu fallback must be visible in the report, not only in stderr"
    );
}

#[tokio::test]
async fn synthesizing_stage_omits_the_flag_key_when_there_are_no_flags() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "One sentence.")])
        .await
        .expect("segments");

    let result = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", result),
        ("done", r#"{"done":1,"total":1,"flags":[]}"#),
    ]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: Option<serde_json::Value> = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok());
    if let Some(report) = report {
        assert!(report.get("tts_flags").is_none());
    }
}

async fn seed_language_case(
    pool: &SqlitePool,
    book_language: Option<&str>,
    engine: &str,
    preset_voice_id: Option<&str>,
) -> (String, String) {
    let book = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/fixture.pdf".into(),
        format: "pdf".into(),
        page_count: Some(10),
        language: book_language.map(str::to_string),
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
        page_end: 10,
        is_pseudo: false,
        word_count: Some(100),
    };
    book::insert_chapter(pool, &chapter).await.expect("chapter");
    let profile = voice::VoiceProfile {
        id: new_id(),
        name: "Language case".into(),
        engine: engine.into(),
        kind: "preset".into(),
        preset_voice_id: preset_voice_id.map(str::to_string),
        reference_wav_path: None,
        params_json: None,
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &profile)
        .await
        .expect("voice");
    let install = models::ModelInstall {
        model_id: engine.into(),
        path: format!("tts/{engine}"),
        sha256: "deadbeef".into(),
        disk_bytes: 1,
        source: "curated".into(),
        installed_at: now_ms(),
    };
    models::insert_model_install(pool, &install)
        .await
        .expect("install");
    (book.id, profile.id)
}

async fn insert_job_for_engine(
    pool: &SqlitePool,
    book_id: &str,
    voice_id: &str,
    engine: &str,
) -> String {
    let job_id = insert_job(pool, book_id, voice_id).await;
    let plan = format!(
        r#"{{"model_id":"{engine}","device":"cpu","tts_workers":2,"batch":2,"badge":"green"}}"#
    );
    job::set_job_tts_plan(pool, &job_id, &plan)
        .await
        .expect("plan");
    job_id
}

async fn run_language_case(
    book_language: Option<&str>,
    engine: &str,
    preset_voice_id: Option<&str>,
) -> Option<serde_json::Value> {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) =
        seed_language_case(&pool, book_language, engine, preset_voice_id).await;
    let job_id = insert_job_for_engine(&pool, &book_id, &voice_id, engine).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "One sentence.")])
        .await
        .expect("segments");

    let result = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", result),
        ("done", r#"{"done":1,"total":1,"flags":[]}"#),
    ]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: Option<serde_json::Value> = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok());
    report.and_then(|r| r.get("language_advisory").cloned())
}

#[tokio::test]
async fn a_book_in_a_language_the_engine_cannot_speak_is_flagged_in_the_report() {
    let advisory = run_language_case(Some("zh"), "chatterbox-0.5b", None)
        .await
        .expect("advisory");
    assert_eq!(
        advisory,
        serde_json::json!({"book": "zh", "voice_speaks": null}),
        "a zh book through an en-only engine must be flagged in the completion report"
    );
}

#[tokio::test]
async fn a_voice_speaking_the_books_language_raises_no_advisory() {
    let advisory = run_language_case(Some("zh"), "kokoro-82m", Some("zf_xiaobei")).await;
    assert_eq!(advisory, None);
}

#[tokio::test]
async fn a_preset_voice_mismatch_names_what_the_voice_speaks() {
    let advisory = run_language_case(Some("zh"), "kokoro-82m", Some("af_heart"))
        .await
        .expect("advisory");
    assert_eq!(
        advisory,
        serde_json::json!({"book": "zh", "voice_speaks": "en"}),
        "kokoro lists zh, but af_heart itself speaks English — the per-voice language decides"
    );
}

#[tokio::test]
async fn a_book_without_a_detected_language_raises_no_advisory() {
    let advisory = run_language_case(None, "chatterbox-0.5b", None).await;
    assert_eq!(advisory, None);
}

#[tokio::test]
async fn synthesizing_stage_declares_its_stage_so_the_engine_can_evict() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "One sentence.")])
        .await
        .expect("segments");

    let request_log = Arc::new(Mutex::new(Vec::new()));
    let payload = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":90,"duration_ms":700,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_recording_sse_server(
        sse_body(&[("result", payload), ("done", r#"{"done":1,"total":1}"#)]),
        request_log.clone(),
    );

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let requests = request_log.lock().unwrap().join("");
    assert!(
        requests.contains("\"stage\":\"SYNTHESIZING\""),
        "batch synthesis must identify its stage, otherwise the engine treats it as an \
         interactive call, keeps the tts backend warm and never calls evict_stage: {requests}"
    );
}

async fn claimed_hashes(pool: &SqlitePool, job_id: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT hash FROM cache_ref WHERE job_id = ? ORDER BY hash")
        .bind(job_id)
        .fetch_all(pool)
        .await
        .expect("cache_ref")
}

#[tokio::test]
async fn synthesizing_stage_claims_the_native_word_timings_it_leaves_in_the_cache() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "First sentence.")])
        .await
        .expect("segments");
    let cache_dir = dir.path().join("cache");
    let words_key = txtcaster_lib::orchestrator::align::align_cache_key("aaaa", "tts_native");
    let shard = cache_dir.join(&words_key[..2]);
    std::fs::create_dir_all(&shard).expect("shard");
    std::fs::write(shard.join(&words_key), b"[{\"w\":\"First\"}]").expect("words json");

    let result = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":100,"duration_ms":800,"cache_hit":false,"words":[{"w":"First","s":0,"e":400}],"flags":[]}}"#;
    let port = spawn_sse_server(sse_body(&[
        ("result", result),
        ("done", r#"{"done":1,"total":1}"#),
    ]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let claimed = claimed_hashes(&pool, &job_id).await;
    assert!(
        claimed.contains(&"aaaa".to_string()),
        "the synthesized WAV was not claimed: {claimed:?}"
    );
    assert!(
        claimed.contains(&words_key),
        "the sidecar's native word timings were left unaccounted: {claimed:?}"
    );
    let entry = cache::get_entry(&pool, &words_key)
        .await
        .expect("query")
        .expect("the word timings must be an accounted cache entry");
    assert_eq!(entry.kind, "align");
}

async fn insert_cloned_voice(pool: &SqlitePool, reference: &str) -> String {
    let profile = voice::VoiceProfile {
        id: new_id(),
        name: "Cloned".into(),
        engine: "kokoro-82m".into(),
        kind: "cloned".into(),
        preset_voice_id: None,
        reference_wav_path: Some(reference.into()),
        params_json: Some(r#"{"exaggeration":0.5}"#.into()),
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &profile)
        .await
        .expect("cloned voice");
    profile.id
}

async fn record_voice_payload(
    pool: &SqlitePool,
    dir: &tempfile::TempDir,
    job_id: &str,
) -> serde_json::Value {
    job::insert_segments(pool, &[make_segment(job_id, 0, "One sentence.")])
        .await
        .expect("segments");
    let request_log = Arc::new(Mutex::new(Vec::new()));
    let payload = r#"{"done":1,"total":1,"result":{"idx":0,"audio_hash":"aaaa","wav_bytes":90,"duration_ms":700,"cache_hit":false,"words":[],"flags":[]}}"#;
    let port = spawn_recording_sse_server(
        sse_body(&[("result", payload), ("done", r#"{"done":1,"total":1}"#)]),
        request_log.clone(),
    );
    let stage = make_stage(Arc::new(FakeSidecar { port }), dir);
    let outcome = run_stage(&stage, pool, job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);
    let requests = request_log.lock().unwrap().join("");
    let body = requests
        .split_once("\r\n\r\n")
        .expect("request body")
        .1
        .to_string();
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json body");
    parsed["voice_profile"].clone()
}

#[tokio::test]
async fn synthesizing_stage_sends_the_cloned_reference_and_the_job_seed() {
    let (dir, pool) = test_pool().await;
    let (book_id, _) = seed(&pool, true).await;
    let voice_id = insert_cloned_voice(&pool, "/voices/voice-1/reference.wav").await;
    let job_id = insert_job_seeded(&pool, &book_id, &voice_id, Some(4242)).await;

    let voice_payload = record_voice_payload(&pool, &dir, &job_id).await;
    assert_eq!(voice_payload["id"], serde_json::json!(voice_id));
    assert_eq!(voice_payload["kind"], serde_json::json!("cloned"));
    assert_eq!(
        voice_payload["reference_wav_path"],
        serde_json::json!("/voices/voice-1/reference.wav")
    );
    assert_eq!(
        voice_payload["params"],
        serde_json::json!({"exaggeration": 0.5}),
        "the profile's cloning controls must reach the engine; dropping them would \
         silently re-render every segment with the defaults"
    );
    assert_eq!(voice_payload["seed"], serde_json::json!(4242));
}

#[tokio::test]
async fn synthesizing_stage_derives_a_seed_for_a_job_that_predates_the_column() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    assert!(job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("job")
        .tts_seed
        .is_none());

    let voice_payload = record_voice_payload(&pool, &dir, &job_id).await;
    assert_eq!(
        voice_payload["seed"],
        serde_json::json!(job::default_tts_seed(&voice_id, "hash"))
    );
}

#[tokio::test]
async fn the_seed_follows_the_voice_and_the_book_not_the_job() {
    let first = job::default_tts_seed("voice-a", "hash-1");
    assert_eq!(first, job::default_tts_seed("voice-a", "hash-1"));
    assert_ne!(first, job::default_tts_seed("voice-b", "hash-1"));
    assert_ne!(first, job::default_tts_seed("voice-a", "hash-2"));
    assert_ne!(
        job::default_tts_seed("a", "bc"),
        job::default_tts_seed("ab", "c"),
        "the separator must keep the two fields apart"
    );
    assert!((0..=u32::MAX as i64).contains(&first));
}

// ---------------------------------------------------------------------------
// S-3 cloud TTS corpus (A15/A21/A23 — orchestrator/synthesis_cloud.rs). The
// two S-3 providers live on fixed official hosts, so the wire behavior is
// pinned in their adapter fixture matrices; these rows drive the seam the
// stage calls (persistence, timing gate, refusals) with a fake adapter and
// zero network.
// ---------------------------------------------------------------------------

use txtcaster_lib::orchestrator::synthesis_cloud::synthesize_segment;
const RETIRED_DEV_CLOUD_TTS_SETTING: &str = "providers.dev_cloud_tts";
use txtcaster_lib::providers::registry::{ScriptError, TtsArtifact, TtsSynth};
use txtcaster_lib::storage::settings::set_setting;

struct FakeTts {
    dir: std::path::PathBuf,
    words: Option<serde_json::Value>,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait::async_trait]
impl TtsSynth for FakeTts {
    async fn synthesize(&mut self, text: &str, voice: &str) -> Result<TtsArtifact, ScriptError> {
        self.calls
            .lock()
            .unwrap()
            .push((text.to_string(), voice.to_string()));
        let path = self.dir.join(format!("fake-{}.wav", new_id()));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 24_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(&path, spec).unwrap();
        for sample in 0..2400i16 {
            wav.write_sample(sample).unwrap();
        }
        wav.finalize().unwrap();
        Ok(TtsArtifact {
            audio_path: path,
            native_words: self.words.clone(),
        })
    }
}

fn stage_ctx(pool: &SqlitePool, job_id: &str) -> StageContext {
    let (_handle, receiver) = control_channel();
    let (progress, _events) = noop_progress();
    StageContext::new(
        pool.clone(),
        job_id.to_string(),
        JobState::Synthesizing,
        Checkpoint::new(JobState::Synthesizing, now_ms()),
        receiver,
        progress,
    )
}

#[tokio::test]
async fn a_cloud_segment_lands_in_the_cache_with_admitted_native_timing() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segment = make_segment(&job_id, 0, "Hello world.");
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .unwrap();
    let cache_dir = dir.path().join("cache");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut adapter = FakeTts {
        dir: dir.path().to_path_buf(),
        words: Some(serde_json::json!([
            {"w": "Hello", "s": 0, "e": 40},
            {"w": "world", "s": 50, "e": 90},
        ])),
        calls: calls.clone(),
    };
    let ctx = stage_ctx(&pool, &job_id);
    let outcome = synthesize_segment(&ctx, &mut adapter, &segment, "voice-9", &cache_dir, "Conn")
        .await
        .expect("synthesizes");

    assert!(outcome.native_timing);
    assert_eq!(outcome.duration_ms, 100, "2400 samples at 24kHz");
    let wav = txtcaster_lib::render::mp3::cache_wav_path(&cache_dir, &outcome.audio_hash);
    assert!(wav.exists(), "audio landed under its content hash");
    assert_eq!(
        calls.lock().unwrap()[0],
        ("Hello world.".to_string(), "voice-9".to_string())
    );

    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    assert_eq!(
        stored[0].audio_hash.as_deref(),
        Some(outcome.audio_hash.as_str())
    );
    let alignment = job::get_alignment(&pool, &segment.id)
        .await
        .unwrap()
        .expect("native alignment row");
    assert_eq!(alignment.source, words::SOURCE_TTS_NATIVE);
    let payload = alignment.words_json.parse();
    assert_eq!(payload.timebase, Timebase::Trimmed);
    assert_eq!(
        words::classify(&payload, &alignment.source),
        WordProvenance::Fresh,
        "admitted native timing is fresh in the envelope, not stale"
    );
}

#[tokio::test]
async fn rejected_native_timing_falls_back_whole_and_keeps_the_audio() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segment = make_segment(&job_id, 0, "Hello world.");
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .unwrap();
    let cache_dir = dir.path().join("cache");

    // Nonmonotonic: the whole set must fall back to align (A21), and no
    // partial subset may sneak into the envelope.
    let mut adapter = FakeTts {
        dir: dir.path().to_path_buf(),
        words: Some(serde_json::json!([
            {"w": "Hello", "s": 500, "e": 900},
            {"w": "world", "s": 100, "e": 400},
        ])),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let ctx = stage_ctx(&pool, &job_id);
    let outcome = synthesize_segment(&ctx, &mut adapter, &segment, "v", &cache_dir, "Conn")
        .await
        .expect("audio still succeeds");
    assert!(!outcome.native_timing);
    assert!(
        job::get_alignment(&pool, &segment.id)
            .await
            .unwrap()
            .is_none(),
        "no alignment row — the align phase owns this segment now"
    );
    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    assert!(
        stored[0].audio_hash.is_some(),
        "audio is kept; only timing fell back"
    );
}

#[tokio::test]
async fn identical_audio_bytes_deduplicate_in_the_cache() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let first = make_segment(&job_id, 0, "Same text.");
    let second = make_segment(&job_id, 1, "Same text.");
    job::insert_segments(&pool, &[first.clone(), second.clone()])
        .await
        .unwrap();
    let cache_dir = dir.path().join("cache");
    let mut adapter = FakeTts {
        dir: dir.path().to_path_buf(),
        words: None,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let ctx = stage_ctx(&pool, &job_id);
    let a = synthesize_segment(&ctx, &mut adapter, &first, "v", &cache_dir, "Conn")
        .await
        .unwrap();
    let b = synthesize_segment(&ctx, &mut adapter, &second, "v", &cache_dir, "Conn")
        .await
        .unwrap();
    assert_eq!(
        a.audio_hash, b.audio_hash,
        "content-addressed: same bytes, same key"
    );
    assert!(txtcaster_lib::render::mp3::cache_wav_path(&cache_dir, &a.audio_hash).exists());
}

async fn insert_cloud_bound_job(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
    let job_id = new_id();
    let overrides = serde_json::json!({
        "tts_binding": {
            "kind": "connection",
            "connection_id": "c-missing",
            "model": "sonic-3.6",
            "voice": "v-1",
            "provider": "cartesia_tts",
            "endpoint": "https://api.cartesia.ai",
            "adapter_rev": "cartesia_tts/1",
        }
    })
    .to_string();
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
        model_overrides_json: Some(overrides),
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Synthesizing.as_str().into(),
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
    job::insert_segments(pool, &[make_segment(&job_id, 0, "Text.")])
        .await
        .unwrap();
    job_id
}

#[tokio::test]
async fn a_cloud_tts_binding_refuses_without_the_dev_flag_and_never_runs_local() {
    // S-5: the dev flag is retired — the gate is the connection itself.
    // The missing recorded connection refuses BY NAME regardless of the
    // flag's value, and never silently runs local (the NoSidecar provider
    // would hang a local attempt).
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_cloud_bound_job(&pool, &book_id, &voice_id).await;
    let stage = SynthesizingStage::new(
        Arc::new(NoSidecar),
        std::env::temp_dir(),
        std::env::temp_dir(),
    );
    for flag in ["false", "true"] {
        set_setting(&pool, RETIRED_DEV_CLOUD_TTS_SETTING, flag)
            .await
            .unwrap();
        let err = run_stage(&stage, &pool, &job_id).await.expect_err("refuse");
        assert!(err.to_string().contains("recorded connection"), "{err}");
    }
    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    assert!(stored[0].audio_hash.is_none(), "nothing was synthesized");
}

#[tokio::test]
async fn a_malformed_tts_binding_refuses_instead_of_local_fallback() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Text.")])
        .await
        .unwrap();
    sqlx::query("UPDATE render_job SET model_overrides_json = ? WHERE id = ?")
        .bind(r#"{"tts_binding":"kokoro-82m"}"#)
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();
    let stage = SynthesizingStage::new(
        Arc::new(NoSidecar),
        std::env::temp_dir(),
        std::env::temp_dir(),
    );
    let err = run_stage(&stage, &pool, &job_id).await.expect_err("refuse");
    assert!(matches!(err, OrchestratorError::InvalidRequest(_)), "{err}");
}

#[tokio::test]
async fn a_local_tts_binding_names_the_model_verbatim() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Text.")])
        .await
        .unwrap();
    sqlx::query("UPDATE render_job SET model_overrides_json = ? WHERE id = ?")
        .bind(r#"{"tts_binding":{"kind":"local","model_id":"not-installed-tts"}}"#)
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();
    let stage = SynthesizingStage::new(
        Arc::new(NoSidecar),
        std::env::temp_dir(),
        std::env::temp_dir(),
    );
    let err = run_stage(&stage, &pool, &job_id).await.expect_err("refuse");
    assert!(
        matches!(&err, OrchestratorError::ModelNotInstalled(id) if id == "not-installed-tts"),
        "the recorded local binding is what resolves, not the profile engine: {err}"
    );
}

struct HangingTts {
    started: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl TtsSynth for HangingTts {
    async fn synthesize(&mut self, _text: &str, _voice: &str) -> Result<TtsArtifact, ScriptError> {
        // Models an in-flight provider request that never completes; the
        // caller's select must be what ends it.
        self.started.notify_one();
        std::future::pending::<()>().await;
        unreachable!("a dropped in-flight request never resumes");
    }
}

#[tokio::test]
async fn cancel_arriving_mid_request_aborts_promptly_and_persists_nothing() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segment = make_segment(&job_id, 0, "Hello world.");
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .unwrap();

    let (handle, receiver) = control_channel();
    let (progress, _events) = noop_progress();
    let ctx = StageContext::new(
        pool.clone(),
        job_id.clone(),
        JobState::Synthesizing,
        Checkpoint::new(JobState::Synthesizing, now_ms()),
        receiver,
        progress,
    );
    let started = Arc::new(tokio::sync::Notify::new());
    let adapter = HangingTts {
        started: started.clone(),
    };
    // The cancel fires only once the request is IN FLIGHT — this is the
    // mid-request timing, not the between-segments boundary.
    let canceller = tokio::spawn({
        let started = started.clone();
        async move {
            started.notified().await;
            handle.cancel();
        }
    });
    let mut plan = txtcaster_lib::orchestrator::synthesis_cloud::CloudTtsPlan::for_testing(
        txtcaster_lib::providers::ConnectionRecord::draft("gemini", None, false).unwrap(),
        Box::new(adapter),
        "test_account".into(),
        "gemini-2.5-flash".into(),
        dir.path().join("staging"),
    );
    let run = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        txtcaster_lib::orchestrator::synthesis_cloud::synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "v",
            &dir.path().join("cache"),
            "req-key-fixture",
            None,
        ),
    )
    .await
    .expect("the cancel must end the in-flight request promptly, not hang")
    .expect("no error");
    canceller.await.unwrap();
    assert!(matches!(
        run,
        txtcaster_lib::orchestrator::synthesis_cloud::SegmentRun::Interrupted(
            StageOutcome::Cancelled
        )
    ));
    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    assert!(
        stored[0].audio_hash.is_none(),
        "a segment whose request was dropped persists nothing"
    );
}

struct GarbageTts {
    dir: std::path::PathBuf,
}

#[async_trait::async_trait]
impl TtsSynth for GarbageTts {
    async fn synthesize(&mut self, _text: &str, _voice: &str) -> Result<TtsArtifact, ScriptError> {
        let path = self.dir.join(format!("garbage-{}.wav", new_id()));
        std::fs::write(&path, b"this is not a wav").unwrap();
        Ok(TtsArtifact {
            audio_path: path,
            native_words: None,
        })
    }
}

#[tokio::test]
async fn invalid_provider_audio_is_never_published_and_staging_is_cleaned() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segment = make_segment(&job_id, 0, "Text.");
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .unwrap();
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let cache_dir = dir.path().join("cache");
    let mut adapter = GarbageTts {
        dir: staging.clone(),
    };
    let ctx = stage_ctx(&pool, &job_id);
    let err = synthesize_segment(&ctx, &mut adapter, &segment, "v", &cache_dir, "Conn")
        .await
        .expect_err("invalid audio refuses");
    assert!(err.to_string().contains("wav"), "{err}");
    assert!(
        !cache_dir.exists() || walkdir_is_empty(&cache_dir),
        "nothing was published to the content-addressed cache"
    );
    assert!(
        std::fs::read_dir(&staging).unwrap().next().is_none(),
        "the invalid staged artifact was removed"
    );
    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    assert!(stored[0].audio_hash.is_none(), "no audio facts persisted");
}

fn walkdir_is_empty(dir: &std::path::Path) -> bool {
    fn walk(dir: &std::path::Path) -> bool {
        match std::fs::read_dir(dir) {
            Err(_) => true,
            Ok(entries) => entries.flatten().all(|entry| {
                let path = entry.path();
                path.is_dir() && walk(&path)
            }),
        }
    }
    walk(dir)
}

#[tokio::test]
async fn a_malformed_tts_binding_refuses_even_when_all_audio_is_cached() {
    // Hermes R2 blocker 4 pin: "an all-audio path returns before binding
    // validation" — the binding parses (and refuses) BEFORE the cached
    // early return.
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let mut segment = make_segment(&job_id, 0, "Text.");
    segment.audio_hash = Some("cached-hash".to_string());
    segment.duration_ms = Some(1000);
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .unwrap();
    sqlx::query("UPDATE render_job SET model_overrides_json = ? WHERE id = ?")
        .bind(r#"{"tts_binding":"corrupted"}"#)
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();
    let stage = SynthesizingStage::new(
        Arc::new(NoSidecar),
        std::env::temp_dir(),
        std::env::temp_dir(),
    );
    let err = run_stage(&stage, &pool, &job_id).await.expect_err("refuse");
    assert!(matches!(err, OrchestratorError::InvalidRequest(_)), "{err}");
}

#[tokio::test]
async fn the_cloud_path_never_reads_the_profile_row() {
    // Hermes R2 blocker 4 pin: "synthesis still loads the mutable profile
    // before parsing the complete cloud binding" — attribution policy: the
    // FK itself enforces profile existence (deleting a referenced row is a
    // constraint error, proven here), and the CLOUD path never reads the
    // row: mutating it to a nonsense local engine changes nothing about
    // the cloud refusal.
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_cloud_bound_job(&pool, &book_id, &voice_id).await;
    let fk_violation = sqlx::query("DELETE FROM voice_profile WHERE id = ?")
        .bind(&voice_id)
        .execute(&pool)
        .await;
    assert!(
        fk_violation.is_err(),
        "the attribution FK forbids deleting a referenced profile — that IS the policy"
    );
    sqlx::query("UPDATE voice_profile SET engine = 'nonsense-not-installed' WHERE id = ?")
        .bind(&voice_id)
        .execute(&pool)
        .await
        .unwrap();
    set_setting(&pool, RETIRED_DEV_CLOUD_TTS_SETTING, "true")
        .await
        .unwrap();
    let stage = SynthesizingStage::new(
        Arc::new(NoSidecar),
        std::env::temp_dir(),
        std::env::temp_dir(),
    );
    // The refusal is about the MISSING CONNECTION (the binding's own
    // problem), not VoiceProfileNotFound — proof the profile row was never
    // consulted on the cloud path.
    let err = run_stage(&stage, &pool, &job_id).await.expect_err("refuse");
    assert!(
        err.to_string().contains("recorded connection"),
        "cloud path must fail on its own facts, not the profile: {err}"
    );
}

#[tokio::test]
async fn a_timing_bearing_cache_hit_replays_its_alignment_not_timestampless() {
    // Hermes R2 blocker 6 pin: "every timing-bearing entry degrades to
    // timestamp-less replay" — a hit whose mapping carries the settled
    // WordsJson envelope must recreate the alignment row.
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segment = make_segment(&job_id, 0, "Hello world.");
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .unwrap();
    let cache_dir = dir.path().join("cache");

    // First synthesis settles the mapping (with timing) through the real path.
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut adapter = FakeTts {
        dir: dir.path().to_path_buf(),
        words: Some(serde_json::json!([
            {"w": "Hello", "s": 0, "e": 40},
            {"w": "world", "s": 50, "e": 90},
        ])),
        calls: calls.clone(),
    };
    let ctx = stage_ctx(&pool, &job_id);
    let first = synthesize_segment(&ctx, &mut adapter, &segment, "voice-9", &cache_dir, "Conn")
        .await
        .unwrap();
    assert!(first.native_timing);
    let request_key = txtcaster_lib::orchestrator::synthesis_cloud::request_cache_key(
        "cartesia_tts",
        "https://api.cartesia.ai",
        "sonic-3.6",
        "voice-9",
        &segment.text,
        "cartesia_tts/1",
    );
    // Manually settle under that key (the seam test drives synthesize_segment
    // directly, which uses an empty key) so replay has a real mapping.
    job::upsert_provider_request_cache(
        &pool,
        &request_key,
        &job::ProviderRequestCacheRow {
            audio_hash: first.audio_hash.clone(),
            wav_bytes: first.wav_bytes,
            duration_ms: first.duration_ms,
            words_json: Some(
                txtcaster_lib::words::WordsJson::declare(
                    &[
                        serde_json::json!({"w": "Hello", "s": 0, "e": 40}),
                        serde_json::json!({"w": "world", "s": 50, "e": 90}),
                    ],
                    Timebase::Trimmed,
                )
                .unwrap()
                .as_str()
                .to_string(),
            ),
        },
    )
    .await
    .unwrap();
    // Second segment replays from the mapping: alignment MUST come back.
    let second = make_segment(&job_id, 1, "Hello world.");
    job::insert_segments(&pool, std::slice::from_ref(&second))
        .await
        .unwrap();
    let hit = txtcaster_lib::orchestrator::synthesis_cloud::replay_cached_request(
        &ctx,
        &request_key,
        &second,
        &cache_dir,
    )
    .await
    .unwrap()
    .expect("hit");
    assert!(hit.native_timing, "timing-bearing hits replay their timing");
    let alignment = job::get_alignment(&pool, &second.id)
        .await
        .unwrap()
        .expect("alignment recreated from the mapping");
    assert_eq!(alignment.source, words::SOURCE_TTS_NATIVE);
    assert_eq!(alignment.words_json.parse().timebase, Timebase::Trimmed);
}

#[tokio::test]
async fn corrupt_or_widowed_cache_mappings_quarantine_as_misses() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segment = make_segment(&job_id, 0, "Hello world.");
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .unwrap();
    let cache_dir = dir.path().join("cache");
    let ctx = stage_ctx(&pool, &job_id);

    // Widowed: mapping exists, wav does not → quarantined, row gone.
    job::upsert_provider_request_cache(
        &pool,
        "widowed-key",
        &job::ProviderRequestCacheRow {
            audio_hash: "no-such-wav".to_string(),
            wav_bytes: 10,
            duration_ms: 100,
            words_json: None,
        },
    )
    .await
    .unwrap();
    let miss = txtcaster_lib::orchestrator::synthesis_cloud::replay_cached_request(
        &ctx,
        "widowed-key",
        &segment,
        &cache_dir,
    )
    .await
    .unwrap();
    assert!(miss.is_none());
    assert!(
        job::get_provider_request_cache(&pool, "widowed-key")
            .await
            .unwrap()
            .is_none(),
        "the dead mapping was deleted, not left to shadow future syntheses"
    );

    // Malformed timing: a real wav but garbage words_json → quarantined miss,
    // never replayed as timestamp-less.
    let mut adapter = FakeTts {
        dir: dir.path().to_path_buf(),
        words: None,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let settled = synthesize_segment(&ctx, &mut adapter, &segment, "v", &cache_dir, "Conn")
        .await
        .unwrap();
    job::upsert_provider_request_cache(
        &pool,
        "malformed-key",
        &job::ProviderRequestCacheRow {
            audio_hash: settled.audio_hash.clone(),
            wav_bytes: settled.wav_bytes,
            duration_ms: settled.duration_ms,
            words_json: Some("{not the envelope".to_string()),
        },
    )
    .await
    .unwrap();
    let miss = txtcaster_lib::orchestrator::synthesis_cloud::replay_cached_request(
        &ctx,
        "malformed-key",
        &segment,
        &cache_dir,
    )
    .await
    .unwrap();
    assert!(
        miss.is_none(),
        "malformed timing must not replay as no-timing"
    );
    assert!(job::get_provider_request_cache(&pool, "malformed-key")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_rebound_endpoint_is_refused_before_any_custody_access() {
    // Hermes R2 F12 pin: the stage runs with keys=None; if custody were
    // touched before snapshot enforcement, the error would be "key custody
    // is unavailable" — instead the rebound endpoint refusal fires first.
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = new_id();
    let connection = txtcaster_lib::providers::ConnectionStore::create(
        &pool,
        "Cartesia",
        "cartesia_tts",
        None,
        false,
        "sonic-3.6",
        1,
    )
    .await
    .unwrap();
    let overrides = serde_json::json!({
        "tts_binding": {
            "kind": "connection",
            "connection_id": connection.id,
            "model": "sonic-3.6",
            "voice": "v-1",
            "provider": "cartesia_tts",
            // The recorded endpoint differs from the live one: T18 refusal.
            "endpoint": "https://old.cartesia.example",
            "adapter_rev": "cartesia_tts/1",
        }
    })
    .to_string();
    let record = job::RenderJob {
        id: job_id.clone(),
        book_id: book_id.clone(),
        scope_json: serde_json::to_string(&Scope::Pages {
            page_start: 1,
            page_end: 10,
        })
        .unwrap(),
        mode: "narrator".into(),
        voice_profile_id: voice_id,
        output_format: "mp3".into(),
        model_overrides_json: Some(overrides),
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Synthesizing.as_str().into(),
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
    job::insert_job(&pool, &record).await.unwrap();
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Text.")])
        .await
        .unwrap();
    set_setting(&pool, RETIRED_DEV_CLOUD_TTS_SETTING, "true")
        .await
        .unwrap();
    let stage = SynthesizingStage::new(
        Arc::new(NoSidecar),
        std::env::temp_dir(),
        std::env::temp_dir(),
    );
    let err = run_stage(&stage, &pool, &job_id).await.expect_err("refuse");
    let text = err.to_string();
    assert!(
        text.contains("rebound") || text.contains("planned against"),
        "{text}"
    );
    assert!(
        !text.contains("custody"),
        "custody must never be touched before snapshot enforcement: {text}"
    );
}

#[tokio::test]
async fn recovery_to_local_is_atomic_and_failed_only() {
    // Hermes R2 blocker 7 pin: "can mutate before FAILED validation" — the
    // conditional UPDATE's WHERE clause IS the validation: a non-FAILED job
    // is not mutated at all, and a FAILED one moves overrides+state as one
    // write.
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_cloud_bound_job(&pool, &book_id, &voice_id).await;

    // Not FAILED yet: recovery refuses and mutates nothing.
    let recovered = job::recover_failed_job_to_local(&pool, &job_id, Some("{}"), false)
        .await
        .unwrap();
    assert!(!recovered);
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert!(record
        .model_overrides_json
        .as_deref()
        .unwrap()
        .contains("tts_binding"));

    sqlx::query("UPDATE render_job SET state = 'FAILED' WHERE id = ?")
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();
    let stripped = serde_json::json!({"recovery": {"to_local": true}}).to_string();
    let recovered = job::recover_failed_job_to_local(&pool, &job_id, Some(&stripped), false)
        .await
        .unwrap();
    assert!(recovered);
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(record.state, "QUEUED");
    assert!(!record
        .model_overrides_json
        .as_deref()
        .unwrap()
        .contains("tts_binding"));
    assert!(record
        .model_overrides_json
        .as_deref()
        .unwrap()
        .contains("recovery"));
}

#[tokio::test]
async fn synthesis_stage_reconciles_missing_wav_on_disk() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let seg0 = make_segment(&job_id, 0, "Segment zero.");
    let seg1 = make_segment(&job_id, 1, "Segment one.");
    job::insert_segments(&pool, &[seg0.clone(), seg1.clone()])
        .await
        .unwrap();

    let cache_dir = dir.path().join("cache");
    let valid_wav = txtcaster_lib::render::mp3::silence_wav(500);
    let seg0_hash = "hash-seg-0";
    let p0 = txtcaster_lib::render::mp3::cache_wav_path(&cache_dir, seg0_hash);
    std::fs::create_dir_all(p0.parent().unwrap()).unwrap();
    std::fs::write(&p0, &valid_wav).unwrap();
    job::set_segment_audio(&pool, &seg0.id, seg0_hash, 500)
        .await
        .unwrap();

    job::set_segment_audio(&pool, &seg1.id, "hash-seg-1-missing", 600)
        .await
        .unwrap();

    let segments_before = job::list_segments(&pool, &job_id).await.unwrap();
    assert!(segments_before[0].audio_hash.is_some());
    assert!(segments_before[1].audio_hash.is_some());

    let missing_ids: Vec<&str> = segments_before
        .iter()
        .filter(|s| {
            s.audio_hash
                .as_deref()
                .is_some_and(|h| !txtcaster_lib::render::mp3::is_valid_wav_artifact(&cache_dir, h))
        })
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(missing_ids, vec![seg1.id.as_str()]);
    job::clear_segments_audio(&pool, &missing_ids)
        .await
        .unwrap();

    let segments_after = job::list_segments(&pool, &job_id).await.unwrap();
    assert_eq!(segments_after[0].audio_hash.as_deref(), Some(seg0_hash));
    assert!(segments_after[1].audio_hash.is_none());
}

#[tokio::test]
async fn first_segment_progress_strips_the_orphaned_pause_display_from_the_report() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, false).await;
    let connection = txtcaster_lib::providers::ConnectionStore::create(
        &pool,
        "Fixture TTS",
        "gemini",
        None,
        false,
        "gemini-2.5-flash-preview-tts",
        1_756_000_000,
    )
    .await
    .expect("connection");
    let keys = Arc::new(txtcaster_lib::voice::keys::KeyManager::session_only_for_tests());
    keys.add_key(&pool, "gemini", None, "gemini-fixture-key".into(), None)
        .await
        .expect("key");
    let job_id = new_id();
    let overrides = serde_json::json!({
        "tts_binding": {
            "kind": "connection",
            "connection_id": connection.id,
            "model": "gemini-2.5-flash-preview-tts",
            "voice": "Puck",
            "provider": "gemini",
            "endpoint": "https://generativelanguage.googleapis.com",
            "adapter_rev": "gemini/1",
        }
    })
    .to_string();
    let record = job::RenderJob {
        id: job_id.clone(),
        book_id,
        scope_json: serde_json::to_string(&Scope::Pages {
            page_start: 1,
            page_end: 10,
        })
        .unwrap(),
        mode: "narrator".into(),
        voice_profile_id: voice_id,
        output_format: "mp3".into(),
        model_overrides_json: Some(overrides),
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Synthesizing.as_str().into(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: Some(
            serde_json::json!({
                "paused_reason": {
                    "kind": "rate_limited",
                    "class": "per_minute",
                    "provider": "google",
                    "segment_idx": 0,
                    "last_error_message": "Quota exceeded: requests per minute (RPM)",
                    "attempt": 1
                },
                "progress_marker": true,
            })
            .to_string(),
        ),
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    job::insert_job(&pool, &record).await.expect("job");
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "...")])
        .await
        .expect("segments");

    let stage = SynthesizingStage::new(
        Arc::new(NoSidecar),
        dir.path().join("models"),
        dir.path().join("cache"),
    )
    .with_key_manager(keys);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    assert!(
        stored[0].audio_hash.is_some(),
        "the segment really synthesized; the strip must ride real progress"
    );
    let after = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(after.report_json.as_deref().expect("report")).expect("json");
    assert!(
        report.get("paused_reason").is_none(),
        "the auto-resume shape (clean checkpoint, stale report) must not keep the pause \
          display after the first synthesized segment: {report}"
    );
    assert_eq!(report["progress_marker"], serde_json::json!(true));
}
