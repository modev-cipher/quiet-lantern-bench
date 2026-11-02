#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};

/// Why every scripting run now opens with a `/health` GET.
///
/// The stage asks the engine for its extraction identity so the blocks line can say
/// `generation=current|stale`: a book left on an older extractor is scripted from the blocks
/// that build stored, and before this the log gave a listener complaint nothing to attribute
/// to. It is one localhost GET per job against a stage that runs for minutes, it never fails
/// the run (a `None` answer reads as "current"), and it is **not** a model endpoint — the
/// tests whose subject is model loading assert that separately so their intent survives the
/// longer sequence.
const GENERATION_PROBE: &str =
    "the stage probes /health once for the generation stamp on the blocks line";

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use txtcaster_lib::ingest_fallback::FALLBACK_ENABLED_KEY;
use txtcaster_lib::orchestrator::contention::UNLOAD_DRAIN_RETRIES;
use txtcaster_lib::orchestrator::stage::{
    control_channel, ProgressEvent, ProgressSink, Stage, StageContext, StageOutcome,
};
use txtcaster_lib::orchestrator::state::{Checkpoint, JobState, OrchestratorError, Scope};
use txtcaster_lib::orchestrator::{
    classify_failure, ScriptingStage, SidecarProvider, StageFailure, CONTENTION_KEY,
};
use txtcaster_lib::storage::{self, book, job, models, new_id, now_ms, voice, SqlitePool};
use txtcaster_lib::voice::keys::KeyManager;

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

fn fixture_envelope(page_no: i64) -> String {
    serde_json::json!({
        "quality": {},
        "blocks": [{
            "bbox": [10.0, 10.0, 300.0, 30.0],
            "text": format!("Fixture page {page_no} body text."),
            "font_size": 11.0,
            "reading_order": 0,
            "column": 0
        }]
    })
    .to_string()
}

async fn seed(pool: &SqlitePool) -> (String, String) {
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
        blocks_version: Some("test-v1".into()),
        blocks_page_count: Some(10),
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    book::insert_book(pool, &book).await.expect("book");
    for page_no in 1..=10 {
        book::insert_page(
            pool,
            &book::Page {
                id: new_id(),
                book_id: book.id.clone(),
                page_no,
                width_pt: Some(612.0),
                height_pt: Some(792.0),
                render_hash: None,
                text_json: Some(fixture_envelope(page_no)),
                needs_upgrade: false,
            },
        )
        .await
        .expect("page");
    }
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

async fn insert_job(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
    insert_mode_job(pool, book_id, voice_id, "narrator", None).await
}

async fn insert_mode_job(
    pool: &SqlitePool,
    book_id: &str,
    voice_id: &str,
    mode: &str,
    resource_plan_json: Option<String>,
) -> String {
    insert_full_job(pool, book_id, voice_id, mode, resource_plan_json, None).await
}

async fn insert_full_job(
    pool: &SqlitePool,
    book_id: &str,
    voice_id: &str,
    mode: &str,
    resource_plan_json: Option<String>,
    model_overrides_json: Option<String>,
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
        mode: mode.into(),
        voice_profile_id: voice_id.into(),
        output_format: "mp3".into(),
        model_overrides_json,
        resource_plan_json,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Scripting.as_str().into(),
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
    job_id
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

type CapturedRequests = Arc<Mutex<Vec<(String, String)>>>;

fn spawn_router_server(script_sse: String) -> (u16, Arc<Mutex<Vec<String>>>) {
    let (port, paths, _requests) = spawn_router_server_with(script_sse, false);
    (port, paths)
}

#[derive(Clone, Copy, PartialEq)]
enum UnloadBehaviour {
    Succeeds,
    InternalError,
    Busy,
}

fn spawn_router_server_with(
    script_sse: String,
    fail_unload: bool,
) -> (u16, Arc<Mutex<Vec<String>>>, CapturedRequests) {
    let unload = if fail_unload {
        UnloadBehaviour::InternalError
    } else {
        UnloadBehaviour::Succeeds
    };
    spawn_router_server_unload(script_sse, unload)
}

fn spawn_router_server_unload(
    script_sse: String,
    unload: UnloadBehaviour,
) -> (u16, Arc<Mutex<Vec<String>>>, CapturedRequests) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let paths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let requests: CapturedRequests = Arc::new(Mutex::new(Vec::new()));
    let seen = paths.clone();
    let captured = requests.clone();
    let sse_bytes = script_sse.into_bytes();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let seen = seen.clone();
            let captured = captured.clone();
            let sse_bytes = sse_bytes.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let mut req = Vec::new();
                let header_end = loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&req[..header_end]).to_string();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while req.len() < header_end + content_length {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let path = head
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let request_body =
                    String::from_utf8_lossy(&req[header_end..header_end + content_length])
                        .to_string();
                seen.lock().unwrap().push(path.clone());
                captured.lock().unwrap().push((path.clone(), request_body));
                let (status, content_type, body): (&str, &str, Vec<u8>) = match path.as_str() {
                    "/script" => ("200 OK", "text/event-stream", sse_bytes),
                    "/models/load" => (
                        "200 OK",
                        "application/json",
                        br#"{"model_id":"fake-llm","loaded":true}"#.to_vec(),
                    ),
                    "/models/unload" if unload == UnloadBehaviour::InternalError => (
                        "500 Internal Server Error",
                        "application/json",
                        br#"{"detail":{"code":"ERR_INTERNAL","message":"boom"}}"#.to_vec(),
                    ),
                    "/models/unload" if unload == UnloadBehaviour::Busy => (
                        "409 Conflict",
                        "application/json",
                        br#"{"detail":{"code":"ERR_BUSY","message":"model 'fake-llm' is in use by 1 active request(s)"}}"#.to_vec(),
                    ),
                    "/models/unload" => (
                        "200 OK",
                        "application/json",
                        br#"{"model_id":"fake-llm","unloaded":true}"#.to_vec(),
                    ),
                    _ => ("200 OK", "application/json", b"{}".to_vec()),
                };
                let _ = mock_http::write_response(&mut sock, status, content_type, &body).await;
            });
        }
    });
    (port, paths, requests)
}

fn noop_progress() -> (ProgressSink, Arc<Mutex<Vec<ProgressEvent>>>) {
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
    let (_handle, receiver) = control_channel();
    let (progress, _events) = noop_progress();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.to_string(),
        JobState::Scripting,
        Checkpoint::new(JobState::Scripting, now_ms()),
        receiver,
        progress,
    );
    stage.run(&mut ctx).await
}

#[tokio::test]
async fn scripting_stage_persists_segments() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"heading","text":"Intro","source_span":{"page":1,"char_start":0,"char_end":5},"flags":[]},{"chapter_idx":1,"page_no":6,"kind":"text","text":"Body sentence.","source_span":{"page":6,"char_start":0,"char_end":14},"flags":["code_block"]}]}"#;
    let port = spawn_sse_server(sse_body(&[
        ("progress", r#"{"chapter":1,"total":2}"#),
        ("segments", payload),
    ]));

    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].kind, "heading");
    assert_eq!(segments[0].chapter_id, "chap-0");
    assert_eq!(segments[1].chapter_id, "chap-1");
    assert_eq!(segments[1].page_no, 6);
    assert_eq!(segments[1].flags_json.as_deref(), Some(r#"["code_block"]"#));

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.total_segments, 2);
    assert_eq!(checkpoint.done_segments, 2);
}

#[tokio::test]
async fn scripting_stage_persists_suppressed_records_with_uncertain_first() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Body.","source_span":{"page":1,"char_start":0,"char_end":5},"flags":[]}],"suppressed":[{"page_no":2,"role":"header","reason":"repetition","certainty":"certain","text":"RUNNING HEAD","words":2},{"page_no":9,"role":"footer","reason":"layout-label","certainty":"uncertain","text":"Access for free at example.org","words":5},{"page_no":4,"role":"footer","reason":"repetition","certainty":"certain","text":"4 CHAPTER ONE","words":3}]}"#;
    let port = spawn_sse_server(sse_body(&[("segments", payload)]));

    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let records = job::list_suppressed_blocks(&pool, &job_id)
        .await
        .expect("list suppressed");
    assert_eq!(
        records.len(),
        3,
        "the engine's suppression audit must survive the job instead of being dropped on the SSE floor"
    );
    assert_eq!(
        records[0].certainty, "uncertain",
        "uncertain rows sort first so a model-vs-heuristic disagreement never passes unnoticed"
    );
    assert_eq!(records[0].text, "Access for free at example.org");
    assert_eq!(records[1].page_no, 2);
    assert_eq!(records[2].page_no, 4);
    assert_eq!(records[1].reason, "repetition");
    assert_eq!(records[0].words, 5);
}

#[tokio::test]
async fn a_second_script_pass_replaces_the_suppression_audit_instead_of_duplicating_it() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let stale = vec![job::SuppressedBlock {
        id: new_id(),
        job_id: job_id.clone(),
        page_no: 99,
        role: "header".into(),
        reason: "repetition".into(),
        certainty: "certain".into(),
        text: "STALE FROM AN EARLIER PASS".into(),
        words: 4,
    }];
    job::insert_suppressed_blocks(&pool, &stale)
        .await
        .expect("seed stale audit");

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Body.","source_span":{"page":1,"char_start":0,"char_end":5},"flags":[]}],"suppressed":[{"page_no":2,"role":"header","reason":"repetition","certainty":"certain","text":"RUNNING HEAD","words":2}]}"#;
    let port = spawn_sse_server(sse_body(&[("segments", payload)]));

    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let records = job::list_suppressed_blocks(&pool, &job_id)
        .await
        .expect("list suppressed");
    assert_eq!(
        records.len(),
        1,
        "a re-script must not accrete audits across passes"
    );
    assert_eq!(records[0].text, "RUNNING HEAD");
}

#[tokio::test]
async fn scripting_stage_is_idempotent_on_resume() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let existing = vec![job::ScriptSegment {
        id: new_id(),
        job_id: job_id.clone(),
        chapter_id: "chap-0".into(),
        idx: 0,
        page_no: 1,
        kind: "text".into(),
        figure_id: None,
        text: "Already scripted.".into(),
        source_span_json: None,
        flags_json: None,
        audio_hash: None,
        duration_ms: None,
        original_text: None,
    }];
    job::insert_segments(&pool, &existing)
        .await
        .expect("seed segments");

    let stage = ScriptingStage::new(Arc::new(NoSidecar), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].text, "Already scripted.");
}

#[tokio::test]
async fn scripting_stage_without_sidecar_honors_cancel_while_waiting() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let stage = ScriptingStage::new(Arc::new(NoSidecar), std::env::temp_dir());
    let (handle, receiver) = control_channel();
    let (progress, _events) = noop_progress();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.clone(),
        JobState::Scripting,
        Checkpoint::new(JobState::Scripting, now_ms()),
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

async fn install_llm(pool: &SqlitePool, model_id: &str) {
    let install = models::ModelInstall {
        model_id: model_id.into(),
        path: format!("llm/{model_id}"),
        sha256: "deadbeef".into(),
        disk_bytes: 1,
        source: "curated".into(),
        installed_at: now_ms(),
    };
    models::insert_model_install(pool, &install)
        .await
        .expect("install");
}

fn stored_plan(model_id: &str) -> String {
    serde_json::json!({
        "model_id": model_id,
        "device": "cpu",
        "n_gpu_layers": 0,
        "n_layers_total": 32,
        "kv_offload": false,
        "ctx": 4096,
        "batch": 256,
        "badge": "orange",
    })
    .to_string()
}

#[tokio::test]
async fn teacher_mode_loads_llm_scripts_and_unloads() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    install_llm(&pool, "phi4-mini-q4km").await;
    let job_id = insert_mode_job(
        &pool,
        &book_id,
        &voice_id,
        "teacher",
        Some(stored_plan("phi4-mini-q4km")),
    )
    .await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"A lecture paragraph.","source_span":{"page":1,"char_start":0,"char_end":20},"flags":[]}]}"#;
    let (port, paths) = spawn_router_server(sse_body(&[
        ("progress", r#"{"stage":"script","chunk":1,"total":2}"#),
        ("progress", r#"{"stage":"script","chunk":2,"total":2}"#),
        ("segments", payload),
    ]));

    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let seen = paths.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec!["/health", "/models/load", "/script", "/models/unload"],
        "{GENERATION_PROBE}"
    );

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].text, "A lecture paragraph.");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.done_chunks, 2);
}

#[tokio::test]
async fn teacher_mode_without_installed_llm_fails_with_model_not_installed() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_mode_job(&pool, &book_id, &voice_id, "teacher", None).await;

    let (port, paths) = spawn_router_server(sse_body(&[("segments", r#"{"segments":[]}"#)]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let result = run_stage(&stage, &pool, &job_id).await;
    assert!(matches!(
        result,
        Err(OrchestratorError::ModelNotInstalled(_))
    ));
    assert!(paths.lock().unwrap().is_empty());
}

#[tokio::test]
async fn teacher_mode_unloads_llm_even_when_script_errors() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    install_llm(&pool, "phi4-mini-q4km").await;
    let job_id = insert_mode_job(
        &pool,
        &book_id,
        &voice_id,
        "teacher",
        Some(stored_plan("phi4-mini-q4km")),
    )
    .await;

    let (port, paths) = spawn_router_server(sse_body(&[
        ("progress", r#"{"stage":"script","chunk":1,"total":3}"#),
        ("error", r#"{"code":"ERR_OOM","message":"out of memory"}"#),
    ]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let result = run_stage(&stage, &pool, &job_id).await;
    let error = result.expect_err("expected sidecar error");
    assert_eq!(error.engine_code(), Some("ERR_OOM"), "{error}");
    let seen = paths.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec!["/health", "/models/load", "/script", "/models/unload"],
        "{GENERATION_PROBE}"
    );

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.done_chunks, 1);
}

#[tokio::test]
async fn teacher_mode_surfaces_unload_failure_on_success_path() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    install_llm(&pool, "phi4-mini-q4km").await;
    let job_id = insert_mode_job(
        &pool,
        &book_id,
        &voice_id,
        "teacher",
        Some(stored_plan("phi4-mini-q4km")),
    )
    .await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"A lecture paragraph.","source_span":{"page":1,"char_start":0,"char_end":20},"flags":[]}]}"#;
    let (port, paths, _requests) =
        spawn_router_server_with(sse_body(&[("segments", payload)]), true);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let result = run_stage(&stage, &pool, &job_id).await;
    let error = result.expect_err("expected unload failure");
    assert_eq!(error.engine_code(), Some("ERR_INTERNAL"), "{error}");
    assert!(
        error.to_string().contains("/models/unload"),
        "the failing call has to stay identifiable now that the wrapper prose is gone: {error}"
    );
    let seen = paths.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec!["/health", "/models/load", "/script", "/models/unload"],
        "{GENERATION_PROBE}"
    );
}

#[tokio::test]
async fn teacher_mode_script_error_wins_over_unload_failure() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    install_llm(&pool, "phi4-mini-q4km").await;
    let job_id = insert_mode_job(
        &pool,
        &book_id,
        &voice_id,
        "teacher",
        Some(stored_plan("phi4-mini-q4km")),
    )
    .await;

    let (port, _paths, _requests) = spawn_router_server_with(
        sse_body(&[("error", r#"{"code":"ERR_OOM","message":"out of memory"}"#)]),
        true,
    );
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let result = run_stage(&stage, &pool, &job_id).await;
    let error = result.expect_err("expected script error");
    assert_eq!(error.engine_code(), Some("ERR_OOM"), "{error}");
}

#[tokio::test]
async fn narrator_mode_with_installed_llm_loads_scripts_and_unloads() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    install_llm(&pool, "phi4-mini-q4km").await;
    storage::settings::set_setting(&pool, "narrator.llm_normalize", "true")
        .await
        .expect("setting");
    let job_id = insert_mode_job(
        &pool,
        &book_id,
        &voice_id,
        "narrator",
        Some(stored_plan("phi4-mini-q4km")),
    )
    .await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Five kilograms of flour.","source_span":{"page":1,"char_start":0,"char_end":24},"flags":["llm_normalized"]}]}"#;
    let (port, paths) = spawn_router_server(sse_body(&[
        ("progress", r#"{"stage":"script","chunk":1,"total":1}"#),
        ("segments", payload),
    ]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let seen = paths.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec!["/health", "/models/load", "/script", "/models/unload"],
        "{GENERATION_PROBE}"
    );

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].text, "Five kilograms of flour.");
}

#[tokio::test]
async fn narrator_mode_never_loads_an_llm_unless_the_setting_opts_in() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    install_llm(&pool, "phi4-mini-q4km").await;
    let job_id = insert_mode_job(
        &pool,
        &book_id,
        &voice_id,
        "narrator",
        Some(stored_plan("phi4-mini-q4km")),
    )
    .await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Plain.","source_span":{"page":1,"char_start":0,"char_end":6},"flags":[]}]}"#;
    let (port, paths) = spawn_router_server(sse_body(&[("segments", payload)]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let seen = paths.lock().unwrap().clone();
    assert_eq!(seen, vec!["/health", "/script"], "{GENERATION_PROBE}");
    assert!(
        !seen.iter().any(|path| path.starts_with("/models")),
        "the point of this test is that no model endpoint is touched, and that has to keep \
         holding independently of the exact sequence: {seen:?}"
    );
}

#[tokio::test]
async fn narrator_mode_with_uninstalled_override_falls_back_to_rules() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_full_job(
        &pool,
        &book_id,
        &voice_id,
        "narrator",
        None,
        Some(r#"{"llm": "gemma4-e2b-q4km"}"#.into()),
    )
    .await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Plain.","source_span":{"page":1,"char_start":0,"char_end":6},"flags":[]}]}"#;
    let (port, paths) = spawn_router_server(sse_body(&[("segments", payload)]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);
    let seen = paths.lock().unwrap().clone();
    assert_eq!(seen, vec!["/health", "/script"], "{GENERATION_PROBE}");
    assert!(
        !seen.iter().any(|path| path.starts_with("/models")),
        "the point of this test is that no model endpoint is touched, and that has to keep \
         holding independently of the exact sequence: {seen:?}"
    );
}

#[tokio::test]
async fn narrator_mode_without_installed_llm_never_touches_model_endpoints() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Plain.","source_span":{"page":1,"char_start":0,"char_end":6},"flags":[]}]}"#;
    let (port, paths) = spawn_router_server(sse_body(&[("segments", payload)]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);
    let seen = paths.lock().unwrap().clone();
    assert_eq!(seen, vec!["/health", "/script"], "{GENERATION_PROBE}");
    assert!(
        !seen.iter().any(|path| path.starts_with("/models")),
        "the point of this test is that no model endpoint is touched, and that has to keep \
         holding independently of the exact sequence: {seen:?}"
    );
}

async fn insert_figure(
    pool: &SqlitePool,
    book_id: &str,
    id: &str,
    page: i64,
    bbox_json: &str,
    caption: Option<&str>,
    explanation: Option<&str>,
) {
    let figure = book::FigureRegion {
        id: id.into(),
        book_id: book_id.into(),
        page_no: page,
        bbox_json: bbox_json.into(),
        kind: "chart".into(),
        confidence: 0.9,
        crop_hash: None,
        caption: caption.map(Into::into),
        explanation: explanation.map(Into::into),
        caption_confidence: caption.map(|_| 0.8),
    };
    book::insert_figure_region_ignore_conflict(pool, &figure)
        .await
        .expect("figure");
}

fn script_request_body(requests: &CapturedRequests) -> serde_json::Value {
    let captured = requests.lock().unwrap().clone();
    let (_, body) = captured
        .into_iter()
        .find(|(path, _)| path == "/script")
        .expect("script request");
    serde_json::from_str(&body).expect("json body")
}

#[tokio::test]
async fn scripting_stage_sends_scoped_figure_captions() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    insert_figure(
        &pool,
        &book_id,
        "fig-1",
        2,
        "[10.0,20.0,110.0,220.0]",
        Some("Figure 1. Sales."),
        Some("The line rises."),
    )
    .await;
    insert_figure(&pool, &book_id, "fig-out", 99, "[0,0,10,10]", None, None).await;
    insert_figure(&pool, &book_id, "fig-bad", 3, "not json", None, None).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Plain.","source_span":{"page":1,"char_start":0,"char_end":6},"flags":[]}]}"#;
    let (port, _paths, requests) =
        spawn_router_server_with(sse_body(&[("segments", payload)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let body = script_request_body(&requests);
    let captions = body["figure_captions"].as_array().expect("captions");
    assert_eq!(captions.len(), 1);
    assert_eq!(captions[0]["region_id"], "fig-1");
    assert_eq!(captions[0]["page_no"], 2);
    assert_eq!(captions[0]["region_type"], "chart");
    assert_eq!(captions[0]["caption"], "Figure 1. Sales.");
    assert_eq!(captions[0]["explanation"], "The line rises.");
    assert_eq!(
        captions[0]["bbox"],
        serde_json::json!([10.0, 20.0, 110.0, 220.0])
    );
    assert_eq!(captions[0]["confidence"], 0.9);
}

#[tokio::test]
async fn scripting_stage_never_sends_decorative_regions() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    insert_figure(&pool, &book_id, "fig-chart", 1, "[0,0,10,10]", None, None).await;
    let decorative = book::FigureRegion {
        id: "fig-logo".into(),
        book_id: book_id.clone(),
        page_no: 1,
        bbox_json: "[0,0,5,5]".into(),
        kind: "decorative".into(),
        confidence: 0.95,
        crop_hash: None,
        caption: None,
        explanation: None,
        caption_confidence: None,
    };
    book::insert_figure_region_ignore_conflict(&pool, &decorative)
        .await
        .expect("figure");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Plain.","source_span":{"page":1,"char_start":0,"char_end":6},"flags":[]}]}"#;
    let (port, _paths, requests) =
        spawn_router_server_with(sse_body(&[("segments", payload)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let body = script_request_body(&requests);
    let captions = body["figure_captions"].as_array().expect("captions");
    assert_eq!(captions.len(), 1);
    assert_eq!(captions[0]["region_id"], "fig-chart");
}

#[tokio::test]
async fn scripting_stage_sends_captionless_figures_for_generic_cues() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    insert_figure(&pool, &book_id, "fig-1", 2, "[0,0,10,10]", None, None).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Plain.","source_span":{"page":1,"char_start":0,"char_end":6},"flags":[]}]}"#;
    let (port, _paths, requests) =
        spawn_router_server_with(sse_body(&[("segments", payload)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let body = script_request_body(&requests);
    let captions = body["figure_captions"].as_array().expect("captions");
    assert_eq!(captions.len(), 1);
    assert_eq!(captions[0]["region_id"], "fig-1");
    assert!(captions[0]["caption"].is_null());
    assert!(captions[0]["explanation"].is_null());
}

#[tokio::test]
async fn scripting_stage_sends_empty_captions_without_figures() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Plain.","source_span":{"page":1,"char_start":0,"char_end":6},"flags":[]}]}"#;
    let (port, _paths, requests) =
        spawn_router_server_with(sse_body(&[("segments", payload)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let body = script_request_body(&requests);
    assert_eq!(
        body["figure_captions"],
        serde_json::Value::Array(Vec::new())
    );
}

#[tokio::test]
async fn scripting_stage_persists_figure_ids_from_segments() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    insert_figure(&pool, &book_id, "fig-1", 1, "[0,0,10,10]", None, None).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Before.","source_span":{"page":1,"char_start":0,"char_end":7},"flags":[]},{"chapter_idx":0,"page_no":1,"kind":"figure_cue","text":"A chart appears here.","source_span":{"page":1,"char_start":0,"char_end":0},"flags":[],"figure_id":"fig-1"}]}"#;
    let port = spawn_sse_server(sse_body(&[("segments", payload)]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].figure_id, None);
    assert_eq!(segments[1].kind, "figure_cue");
    assert_eq!(segments[1].figure_id.as_deref(), Some("fig-1"));
}

#[tokio::test]
async fn scripting_stage_persists_a_figure_id_on_a_text_segment() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    insert_figure(&pool, &book_id, "fig-1", 1, "[0,0,10,10]", None, None).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"This chart tracks sales.","source_span":{"page":1,"char_start":0,"char_end":24},"flags":[],"figure_id":"fig-1"}]}"#;
    let port = spawn_sse_server(sse_body(&[("segments", payload)]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].kind, "text");
    assert_eq!(
        segments[0].figure_id.as_deref(),
        Some("fig-1"),
        "teacher mode anchors a figure on its explanation, so the anchor must survive \
         the sidecar payload and SQLite without relying on kind = figure_cue"
    );
}

#[tokio::test]
async fn scripting_stage_tolerates_segments_without_figure_id_field() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"Old engine.","source_span":{"page":1,"char_start":0,"char_end":11},"flags":[]}]}"#;
    let port = spawn_sse_server(sse_body(&[("segments", payload)]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].figure_id, None);
}

#[tokio::test]
async fn scripting_stage_propagates_sidecar_error() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let port = spawn_sse_server(sse_body(&[(
        "error",
        r#"{"code":"ERR_SCANNED","message":"scanned"}"#,
    )]));
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let result = run_stage(&stage, &pool, &job_id).await;
    let error = result.expect_err("expected sidecar error");
    assert_eq!(error.engine_code(), Some("ERR_SCANNED"), "{error}");
}

fn spawn_slow_script_server_failing_unload() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let paths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = paths.clone();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let mut req = Vec::new();
                let header_end = loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&req[..header_end]).to_string();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while req.len() < header_end + content_length {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let path = head
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                seen.lock().unwrap().push(path.clone());
                if path == "/script" {
                    let _ = mock_http::write_head(
                        &mut sock,
                        "200 OK",
                        &[("Content-Type", "text/event-stream")],
                        None,
                    )
                    .await;
                    let _ = sock
                        .write_all(b"event: progress\ndata: {\"chunk\":1,\"total\":10}\n\n")
                        .await;
                    let _ = sock.flush().await;
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        if sock
                            .write_all(b"event: keepalive\ndata: {}\n\n")
                            .await
                            .is_err()
                        {
                            return;
                        }
                        let _ = sock.flush().await;
                    }
                }
                let (status, body): (&str, &[u8]) = match path.as_str() {
                    "/models/load" => ("200 OK", br#"{"model_id":"fake-llm","loaded":true}"#),
                    "/models/unload" => (
                        "500 Internal Server Error",
                        br#"{"detail":{"code":"ERR_BUSY","message":"model in use"}}"#,
                    ),
                    _ => ("200 OK", b"{}"),
                };
                let _ =
                    mock_http::write_response(&mut sock, status, "application/json", body).await;
            });
        }
    });
    (port, paths)
}

fn spawn_hanging_ingest_server() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let paths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = paths.clone();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let mut req = Vec::new();
                let header_end = loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&req[..header_end]).to_string();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while req.len() < header_end + content_length {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let path = head
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                seen.lock().unwrap().push(path.clone());
                if path == "/ingest" {
                    let _ = mock_http::write_head(
                        &mut sock,
                        "200 OK",
                        &[("Content-Type", "text/event-stream")],
                        None,
                    )
                    .await;
                    let _ = sock
                        .write_all(b"event: progress\ndata: {\"page\":1,\"total\":10}\n\n")
                        .await;
                    let _ = sock.flush().await;
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        if sock
                            .write_all(b"event: keepalive\ndata: {}\n\n")
                            .await
                            .is_err()
                        {
                            return;
                        }
                        let _ = sock.flush().await;
                    }
                }
                let (status, body): (&str, &[u8]) = match path.as_str() {
                    "/models/load" => ("200 OK", br#"{"model_id":"fake-llm","loaded":true}"#),
                    _ => ("200 OK", b"{}"),
                };
                let _ =
                    mock_http::write_response(&mut sock, status, "application/json", body).await;
            });
        }
    });
    (port, paths)
}

#[tokio::test]
async fn a_contended_llm_unload_is_drained_in_place_instead_of_re_running_the_script() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    install_llm(&pool, "phi4-mini-q4km").await;
    let job_id = insert_mode_job(
        &pool,
        &book_id,
        &voice_id,
        "teacher",
        Some(stored_plan("phi4-mini-q4km")),
    )
    .await;

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"One.","source_span":{"page":1,"char_start":0,"char_end":4},"flags":[]}]}"#;
    let (port, paths, _requests) =
        spawn_router_server_unload(sse_body(&[("segments", payload)]), UnloadBehaviour::Busy);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());

    let error = run_stage(&stage, &pool, &job_id)
        .await
        .expect_err("a slot that never drains has to fail the stage eventually");

    let seen = paths.lock().unwrap().clone();
    assert_eq!(
        seen.iter().filter(|path| *path == "/models/unload").count(),
        1 + UNLOAD_DRAIN_RETRIES as usize,
        "the drain is retried where it happens: {seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|path| *path == "/script").count(),
        1,
        "scripting has no engine-side resume and persists its segments only at the end, so \
         letting the stage-level backoff retry a teardown call would regenerate the whole \
         script up to twelve times: {seen:?}"
    );
    assert_eq!(
        classify_failure(&error),
        StageFailure::Terminal,
        "an exhausted drain must not re-enter the stage-level backoff: {error}"
    );
    assert!(
        error.to_string().contains("engine stayed busy"),
        "the report must name the wait: {error}"
    );

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap_or("{}")).unwrap_or_default();
    let contention = &report[CONTENTION_KEY];
    assert_eq!(
        contention["episodes"], 1,
        "the drain never reaches the stage-level backoff, so if it does not record its own \
         episode the teardown contention is invisible in the report: {contention}"
    );
    assert_eq!(
        contention["retries"],
        u64::from(UNLOAD_DRAIN_RETRIES),
        "{contention}"
    );
    assert_eq!(
        contention["by_stage"]["SCRIPTING"]["episodes"], 1,
        "{contention}"
    );
}

#[tokio::test]
async fn pausing_during_the_unload_drain_abandons_it_instead_of_waiting_it_out() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    install_llm(&pool, "phi4-mini-q4km").await;
    let job_id = insert_mode_job(
        &pool,
        &book_id,
        &voice_id,
        "teacher",
        Some(stored_plan("phi4-mini-q4km")),
    )
    .await;

    const DRAIN_NEVER_ELAPSES: std::time::Duration = std::time::Duration::from_secs(600);
    const DRAIN_ABANDON_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

    let payload = r#"{"segments":[{"chapter_idx":0,"page_no":1,"kind":"text","text":"One.","source_span":{"page":1,"char_start":0,"char_end":4},"flags":[]}]}"#;
    let (port, paths, _requests) =
        spawn_router_server_unload(sse_body(&[("segments", payload)]), UnloadBehaviour::Busy);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir())
        .with_drain_delay(DRAIN_NEVER_ELAPSES);

    let (handle, receiver) = control_channel();
    let (progress, _events) = noop_progress();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.clone(),
        JobState::Scripting,
        Checkpoint::new(JobState::Scripting, now_ms()),
        receiver,
        progress,
    );

    let watched = paths.clone();
    let pauser = tokio::spawn(async move {
        let deadline = std::time::Instant::now() + DRAIN_ABANDON_BUDGET;
        loop {
            let seen_unload = watched
                .lock()
                .unwrap()
                .iter()
                .any(|path| path == "/models/unload");
            if seen_unload {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the stage never attempted the unload the pause is meant to interrupt"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let paused_at = std::time::Instant::now();
        handle.pause();
        paused_at
    });

    let _ = tokio::time::timeout(DRAIN_ABANDON_BUDGET, stage.run(&mut ctx))
        .await
        .expect("a paused drain must return instead of sitting out its delay");
    let returned_at = std::time::Instant::now();
    let paused_at = pauser.await.expect("pauser");

    let attempts = paths
        .lock()
        .unwrap()
        .iter()
        .filter(|path| *path == "/models/unload")
        .count();
    assert!(
        attempts < 1 + UNLOAD_DRAIN_RETRIES as usize,
        "the drain must stop as soon as the user interrupts; running it out spends \
         UNLOAD_DRAIN_RETRIES drain delays of a pause the user already asked for. \
         Exactly how many attempts land depends on whether the pause arrives while a request \
         is in flight or during a wait, so the invariant is that the drain does not run to \
         completion, not a fixed count: {attempts}"
    );
    assert!(
        returned_at.duration_since(paused_at) < DRAIN_ABANDON_BUDGET,
        "the drain wait must select against the control like the stage backoff does, not sit \
         out its sleep; with a {DRAIN_NEVER_ELAPSES:?} injected delay only an abandoned drain \
         returns this side of the budget: {:?}",
        returned_at.duration_since(paused_at)
    );
}

#[tokio::test]
async fn cancel_mid_script_settles_cancelled_even_when_unload_fails() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    install_llm(&pool, "phi4-mini-q4km").await;
    let job_id = insert_mode_job(
        &pool,
        &book_id,
        &voice_id,
        "teacher",
        Some(stored_plan("phi4-mini-q4km")),
    )
    .await;

    let (port, paths) = spawn_slow_script_server_failing_unload();
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());

    let (handle, receiver) = control_channel();
    let (progress, reported) = noop_progress();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.clone(),
        JobState::Scripting,
        Checkpoint::new(JobState::Scripting, now_ms()),
        receiver,
        progress,
    );
    let streamed = reported.clone();
    let canceller = tokio::spawn(async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline
            && !streamed
                .lock()
                .unwrap()
                .iter()
                .any(|event| event.current_item == "chunk 1/10")
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        handle.cancel();
    });

    let outcome = stage
        .run(&mut ctx)
        .await
        .expect("a failed unload must not turn a user cancel into a stage error");
    canceller.await.expect("canceller");

    assert_eq!(outcome, StageOutcome::Cancelled);
    let items: Vec<String> = reported
        .lock()
        .unwrap()
        .iter()
        .map(|event| event.current_item.clone())
        .collect();
    assert!(
        items.iter().any(|item| item == "chunk 1/10"),
        "the cancel has to land on an open stream that delivered an event; a mock that streams \
         nothing, or malformed bytes the parser drops, would otherwise leave this test asserting \
         the already-covered cancel-before-response path: {items:?}"
    );
    let seen = paths.lock().unwrap().clone();
    assert!(seen.contains(&"/cancel".to_string()), "paths: {seen:?}");
    assert!(
        seen.contains(&"/models/unload".to_string()),
        "unload is still attempted best-effort: {seen:?}"
    );
}

#[tokio::test]
async fn a_stale_generation_fails_the_job_instead_of_re_extracting_silently() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    sqlx::query("UPDATE book SET blocks_version = NULL WHERE id = ?")
        .bind(&book_id)
        .execute(&pool)
        .await
        .expect("clear version");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let (port, paths, _requests) =
        spawn_router_server_with(sse_body(&[("segments", r#"{"segments":[]}"#)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());

    let err = run_stage(&stage, &pool, &job_id)
        .await
        .expect_err("a stale generation must not be scripted");

    let seen = paths.lock().unwrap().clone();
    assert!(
        seen.contains(&"/ingest".to_string()),
        "a stale generation must attempt a backfill, not fail straight through: {seen:?}"
    );
    assert!(
        !seen.contains(&"/script".to_string()),
        "a failed backfill must never reach scripting: {seen:?}"
    );
    assert!(
        job::list_segments(&pool, &job_id)
            .await
            .expect("segments")
            .is_empty(),
        "a failed backfill must not leave a half-scripted job: {err}"
    );
}

#[tokio::test]
async fn a_book_with_no_page_count_backfills_instead_of_failing_forever() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    sqlx::query("UPDATE book SET page_count = NULL, blocks_page_count = NULL WHERE id = ?")
        .bind(&book_id)
        .execute(&pool)
        .await
        .expect("clear page count");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let (port, paths, _requests) =
        spawn_router_server_with(sse_body(&[("segments", r#"{"segments":[]}"#)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());

    let _ = run_stage(&stage, &pool, &job_id).await;

    let seen = paths.lock().unwrap().clone();
    assert!(
        seen.contains(&"/ingest".to_string()),
        "a NULL page_count must be treated as stale and backfilled; if the two freshness \
         predicates disagree the job fails with ERR_BLOCKS_MISSING forever: {seen:?}"
    );
}

#[tokio::test]
async fn a_torn_generation_fails_the_job() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    sqlx::query("UPDATE book SET blocks_page_count = 3 WHERE id = ?")
        .bind(&book_id)
        .execute(&pool)
        .await
        .expect("torn generation");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let (port, paths, _requests) =
        spawn_router_server_with(sse_body(&[("segments", r#"{"segments":[]}"#)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());

    run_stage(&stage, &pool, &job_id)
        .await
        .expect_err("page 1 at v2 and page 40 at v3 must never be scripted");

    let seen = paths.lock().unwrap().clone();
    assert!(
        !seen.contains(&"/script".to_string()),
        "a torn generation must never reach scripting: {seen:?}"
    );
}

#[tokio::test]
async fn pseudo_chapters_never_drive_the_section_header_pass() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    sqlx::query("UPDATE chapter SET is_pseudo = 1 WHERE book_id = ?")
        .bind(&book_id)
        .execute(&pool)
        .await
        .expect("mark pseudo");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let (port, _paths, requests) =
        spawn_router_server_with(sse_body(&[("segments", r#"{"segments":[]}"#)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let sent = script_request_body(&requests);
    assert_eq!(
        sent["section_bounds"].as_array().map(Vec::len),
        Some(0),
        "'Pages 1-10' windows are not declared structure; treating them as sections \
         suppresses text that is not a running head"
    );
}

#[tokio::test]
async fn declared_chapters_do_drive_the_section_header_pass() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let (port, _paths, requests) =
        spawn_router_server_with(sse_body(&[("segments", r#"{"segments":[]}"#)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let sent = script_request_body(&requests);
    assert_eq!(sent["section_bounds"].as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn a_current_generation_is_never_re_extracted() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let (port, paths, _requests) =
        spawn_router_server_with(sse_body(&[("segments", r#"{"segments":[]}"#)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    run_stage(&stage, &pool, &job_id).await.expect("run");

    let requested = paths.lock().expect("lock").clone();
    assert!(
        !requested.iter().any(|path| path == "/ingest"),
        "blocks_are_current gates the backfill; if it were wrong every job would \
         re-extract the whole book, not just the first: {requested:?}"
    );
    let _ = book_id;
}

#[tokio::test]
async fn pausing_during_a_backfill_pauses_the_job_instead_of_failing_it() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    sqlx::query("UPDATE book SET blocks_version = NULL WHERE id = ?")
        .bind(&book_id)
        .execute(&pool)
        .await
        .expect("clear version");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let (port, paths) = spawn_hanging_ingest_server();
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir());
    let (handle, receiver) = control_channel();
    let (progress, reported) = noop_progress();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.clone(),
        JobState::Scripting,
        Checkpoint::new(JobState::Scripting, now_ms()),
        receiver,
        progress,
    );
    let pauser = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        handle.pause();
    });

    let outcome = stage
        .run(&mut ctx)
        .await
        .expect("a pause during the backfill is an outcome, not a stage error");
    pauser.await.expect("pauser");

    assert_eq!(
        outcome,
        StageOutcome::Paused,
        "flattening the interruption into an error drove the job to FAILED, because \
         ERR_CANCELLED matches no retryable pattern"
    );
    let items: Vec<String> = reported
        .lock()
        .unwrap()
        .iter()
        .map(|event| event.current_item.clone())
        .collect();
    assert!(
        items
            .iter()
            .any(|item| item.starts_with("re-extracting text (page 1/")),
        "inside the backfill means inside a stream that delivered an event; without this the \
         fixture could send no body, or bytes the parser drops, and the test would pass on the \
         pre-response path: {items:?}"
    );
    let seen = paths.lock().unwrap().clone();
    assert!(
        seen.contains(&"/ingest".to_string()),
        "the pause has to land inside the backfill to be the case under test: {seen:?}"
    );
    assert!(
        !seen.contains(&"/script".to_string()),
        "a paused backfill must not go on to script: {seen:?}"
    );
}

// ---------------------------------------------------------------------------
// S-1d wired-path corpus: the dev-only cloud script binding
// (provider-flexibility charter §4.4/T18; see orchestrator/scripting_cloud.rs).
// ---------------------------------------------------------------------------

const RETIRED_DEV_CLOUD_SCRIPT_SETTING: &str = "providers.dev_cloud_script";
use txtcaster_lib::orchestrator::stage::ControlHandle;
use txtcaster_lib::providers::ConnectionStore;
use txtcaster_lib::storage::settings::set_setting;

/// A loopback OpenAI-compatible "cloud" endpoint serving one valid teacher
/// lecture per chat request. When `cancel` holds a handle, the first request
/// cancels the job before the response is written — the deterministic
/// mid-stage-cancellation trigger (the cancel lands while generation is in
/// flight, exactly the timing the corpus row is about).
fn spawn_cloud_llm(cancel: Arc<Mutex<Option<ControlHandle>>>) -> (u16, CapturedRequests) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let requests: CapturedRequests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let captured = captured.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut req = Vec::new();
                let header_end = loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&req[..header_end]).to_string();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while req.len() < header_end + content_length {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let path = head
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let body = String::from_utf8_lossy(&req[header_end..header_end + content_length])
                    .to_string();
                captured.lock().unwrap().push((path, body));
                if let Some(handle) = cancel.lock().unwrap().take() {
                    handle.cancel();
                }
                let payload = serde_json::json!({
                    "id": "chatcmpl-corpus",
                    "object": "chat.completion",
                    "model": "bound-model",
                    "choices": [{
                        "index": 0,
                        "finish_reason": "stop",
                        "message": {
                            "role": "assistant",
                            "content": "[{\"text\": \"Cloud lecture sentence.\"}]"
                        }
                    }],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5}
                })
                .to_string();
                let _ = mock_http::write_response(
                    &mut sock,
                    "200 OK",
                    "application/json",
                    payload.as_bytes(),
                )
                .await;
            });
        }
    });
    (port, requests)
}

/// The engine side of the wired path: the local seam endpoints plus /health,
/// with every request captured so the rows can assert which seams ran.
fn spawn_seam_engine() -> (u16, CapturedRequests) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let requests: CapturedRequests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let captured = captured.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut req = Vec::new();
                let header_end = loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&req[..header_end]).to_string();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while req.len() < header_end + content_length {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let path = head
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let body = String::from_utf8_lossy(&req[header_end..header_end + content_length])
                    .to_string();
                captured.lock().unwrap().push((path.clone(), body));
                let response: Vec<u8> = match path.as_str() {
                    "/health" => br#"{"status":"ok"}"#.to_vec(),
                    "/script/units" => serde_json::json!({
                        "units": [{
                            "chapter_idx": 0,
                            "page_no": 1,
                            "kind": "text",
                            "text": "Fixture page 1 body text."
                        }]
                    })
                    .to_string()
                    .into_bytes(),
                    "/script/segments" => serde_json::json!({
                        "segments": [{
                            "idx": 0,
                            "chapter_idx": 0,
                            "page_no": 1,
                            "kind": "text",
                            "text": "Cloud lecture sentence.",
                            "source_span": {"page": 1, "char_start": 0, "char_end": 23},
                            "flags": []
                        }]
                    })
                    .to_string()
                    .into_bytes(),
                    _ => b"{}".to_vec(),
                };
                let _ =
                    mock_http::write_response(&mut sock, "200 OK", "application/json", &response)
                        .await;
            });
        }
    });
    (port, requests)
}

async fn seed_cloud_binding_job(pool: &SqlitePool, cloud_port: u16) -> (String, String) {
    set_setting(pool, RETIRED_DEV_CLOUD_SCRIPT_SETTING, "true")
        .await
        .expect("dev flag");
    // Loopback keyless is the A1-lawful credential-free shape, which keeps
    // these rows independent of the key vault.
    let connection = ConnectionStore::create(
        pool,
        "Corpus loopback",
        "openai_compatible",
        Some(&format!("http://127.0.0.1:{cloud_port}/v1")),
        true,
        "store-default-model",
        1_756_000_000,
    )
    .await
    .expect("connection");
    let (book_id, voice_id) = seed(pool).await;
    let overrides = serde_json::json!({
        "script_binding": {
            "kind": "connection",
            "connection_id": connection.id,
            "model": "bound-model",
            "provider": "openai_compatible",
            "endpoint": connection.key_base_url(),
            "adapter_rev": "openai_compat/1",
        }
    })
    .to_string();
    let job_id = insert_full_job(pool, &book_id, &voice_id, "teacher", None, Some(overrides)).await;
    (job_id, connection.id)
}

#[tokio::test]
async fn cloud_binding_runs_the_wired_path_with_the_recorded_model() {
    let (_dir, pool) = test_pool().await;
    let (cloud_port, cloud_requests) = spawn_cloud_llm(Arc::new(Mutex::new(None)));
    let (engine_port, engine_requests) = spawn_seam_engine();
    let (job_id, _connection_id) = seed_cloud_binding_job(&pool, cloud_port).await;

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].text, "Cloud lecture sentence.");
    assert_eq!(segments[0].chapter_id, "chap-0");

    // The recorded binding's model — never the connection's default_model —
    // is what reached the wire (charter §4.4: no re-resolution).
    let cloud = cloud_requests.lock().unwrap().clone();
    assert!(!cloud.is_empty(), "the cloud adapter was called");
    for (path, body) in &cloud {
        assert_eq!(path, "/v1/chat/completions");
        let parsed: serde_json::Value = serde_json::from_str(body).expect("cloud body");
        assert_eq!(parsed["model"], "bound-model");
    }

    // The engine served exactly the local seams: unit assembly and
    // segmentation — never /script (the local generation path) and never a
    // local model load.
    let engine_paths: Vec<String> = engine_requests
        .lock()
        .unwrap()
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    assert!(engine_paths.contains(&"/script/units".to_string()));
    assert!(engine_paths.contains(&"/script/segments".to_string()));
    assert!(!engine_paths.contains(&"/script".to_string()));
    assert!(!engine_paths.contains(&"/models/load".to_string()));

    // The segmentation request carried the lecture the cloud produced, with
    // the units the engine assembled.
    let seam = engine_requests.lock().unwrap().clone();
    let (_, segments_body) = seam
        .iter()
        .find(|(path, _)| path == "/script/segments")
        .expect("segments seam ran");
    let parsed: serde_json::Value = serde_json::from_str(segments_body).expect("seam body");
    assert_eq!(parsed["chunks"][0]["texts"][0], "Cloud lecture sentence.");
    assert_eq!(
        parsed["chunks"][0]["units"][0]["text"],
        "Fixture page 1 body text."
    );

    // Checkpoint row: chunk progress and segment totals recorded.
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.done_chunks, 1);
    assert_eq!(checkpoint.total_segments, 1);
    assert_eq!(checkpoint.done_segments, 1);
}

#[tokio::test]
async fn resume_after_completion_replays_the_recorded_outcome_not_the_settings() {
    let (_dir, pool) = test_pool().await;
    let (cloud_port, cloud_requests) = spawn_cloud_llm(Arc::new(Mutex::new(None)));
    let (engine_port, engine_requests) = spawn_seam_engine();
    let (job_id, connection_id) = seed_cloud_binding_job(&pool, cloud_port).await;

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("first run");
    assert_eq!(outcome, StageOutcome::Completed);
    let first_cloud_calls = cloud_requests.lock().unwrap().len();
    let first_engine_calls = engine_requests.lock().unwrap().len();

    // Hostile-settings mutation between runs: the connection is deleted and
    // the dev flag cleared. A resume must replay the recorded outcome from
    // its checkpointed segments — never re-resolve against what settings
    // have become (charter T18).
    ConnectionStore::delete(&pool, &connection_id)
        .await
        .expect("delete connection");
    set_setting(&pool, RETIRED_DEV_CLOUD_SCRIPT_SETTING, "false")
        .await
        .expect("clear flag");

    let resumed = run_stage(&stage, &pool, &job_id).await.expect("resume");
    assert_eq!(resumed, StageOutcome::Completed);
    assert_eq!(
        cloud_requests.lock().unwrap().len(),
        first_cloud_calls,
        "resume must not re-dial the provider"
    );
    assert_eq!(
        engine_requests.lock().unwrap().len(),
        first_engine_calls,
        "resume must not re-run the seams"
    );
    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1);
}

#[tokio::test]
async fn mid_stage_cancellation_persists_nothing_and_reports_cancelled() {
    let (_dir, pool) = test_pool().await;
    let cancel_slot: Arc<Mutex<Option<ControlHandle>>> = Arc::new(Mutex::new(None));
    let (cloud_port, _cloud_requests) = spawn_cloud_llm(cancel_slot.clone());
    let (engine_port, engine_requests) = spawn_seam_engine();
    let (job_id, _connection_id) = seed_cloud_binding_job(&pool, cloud_port).await;

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    let (handle, receiver) = control_channel();
    *cancel_slot.lock().unwrap() = Some(handle);
    let (progress, _events) = noop_progress();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.to_string(),
        JobState::Scripting,
        Checkpoint::new(JobState::Scripting, now_ms()),
        receiver,
        progress,
    );
    let outcome = stage.run(&mut ctx).await.expect("run");
    assert_eq!(outcome, StageOutcome::Cancelled);

    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert!(
        segments.is_empty(),
        "a cancelled cloud run persists no partial segments"
    );
    let engine_paths: Vec<String> = engine_requests
        .lock()
        .unwrap()
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    assert!(
        !engine_paths.contains(&"/script/segments".to_string()),
        "cancellation lands before segmentation"
    );
}

#[tokio::test]
async fn the_retired_dev_flag_no_longer_gates_an_enabled_connection() {
    // S-5: release readiness IS per-connection enablement plus PF-PH1 pool
    // custody — the providers.dev_cloud_script flag is retired. With the
    // flag explicitly false, an ENABLED connection still runs the wired
    // path (the disabled-connection refusal is pinned separately below).
    let (_dir, pool) = test_pool().await;
    let (cloud_port, cloud_requests) = spawn_cloud_llm(Arc::new(Mutex::new(None)));
    let (engine_port, _engine_requests) = spawn_seam_engine();
    let (job_id, _connection_id) = seed_cloud_binding_job(&pool, cloud_port).await;
    set_setting(&pool, RETIRED_DEV_CLOUD_SCRIPT_SETTING, "false")
        .await
        .expect("clear flag");

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    run_stage(&stage, &pool, &job_id).await.expect("runs");
    assert!(
        !cloud_requests.lock().unwrap().is_empty(),
        "the enabled connection reached the provider with the flag false"
    );
}

#[tokio::test]
async fn cloud_binding_refuses_a_disabled_connection() {
    let (_dir, pool) = test_pool().await;
    let (cloud_port, cloud_requests) = spawn_cloud_llm(Arc::new(Mutex::new(None)));
    let (engine_port, _engine_requests) = spawn_seam_engine();
    let (job_id, connection_id) = seed_cloud_binding_job(&pool, cloud_port).await;
    let record = ConnectionStore::get(&pool, &connection_id)
        .await
        .expect("get");
    ConnectionStore::update(
        &pool,
        &connection_id,
        &record.name,
        record.base_url.as_deref(),
        record.credential_not_required,
        &record.default_model,
        false,
        1_756_000_001,
    )
    .await
    .expect("disable");

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    let err = run_stage(&stage, &pool, &job_id).await.expect_err("refuse");
    assert!(
        err.to_string().contains("disabled"),
        "the refusal names the cause and remedy: {err}"
    );
    assert!(cloud_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_malformed_recorded_binding_refuses_instead_of_falling_back_local() {
    let (_dir, pool) = test_pool().await;
    let (engine_port, engine_requests) = spawn_seam_engine();
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_full_job(
        &pool,
        &book_id,
        &voice_id,
        "teacher",
        None,
        Some(r#"{"script_binding":"just-a-string"}"#.to_string()),
    )
    .await;

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    let err = run_stage(&stage, &pool, &job_id).await.expect_err("refuse");
    assert!(
        matches!(err, OrchestratorError::InvalidRequest(_)),
        "malformed recorded binding must refuse: {err}"
    );
    assert!(
        !engine_requests
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path == "/models/load" || path == "/script"),
        "no silent local fallback ran"
    );
}

#[tokio::test]
async fn resume_from_durable_chunk_checkpoints_never_re_requests_the_provider() {
    // S-3b MF11 never-re-pay proof at the wired path: a checkpoint row
    // covering every chunk means the provider is NEVER dialed on resume —
    // the recorded outcomes flow straight to segmentation.
    let (_dir, pool) = test_pool().await;
    let (cloud_port, cloud_requests) = spawn_cloud_llm(Arc::new(Mutex::new(None)));
    let (engine_port, engine_requests) = spawn_seam_engine();
    let (job_id, _connection_id) = seed_cloud_binding_job(&pool, cloud_port).await;

    // Reconstruct exactly what the seam engine serves, fingerprint it the
    // way the wired path does, and record chunk 0 as already completed.
    let unit = txtcaster_lib::providers::script::chunker::SourceUnit {
        chapter_idx: 0,
        page_no: 1,
        kind: "text".to_string(),
        text: "Fixture page 1 body text.".to_string(),
        figure_id: None,
        region_type: None,
        caption: None,
        explanation: None,
        block_index: None,
        label: None,
    };
    let units = vec![unit.clone()];
    use sha2::Digest;
    let fingerprint = format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_string(&units).unwrap().as_bytes())
    );
    let budget = txtcaster_lib::providers::script::teacher::teacher_chunk_budget_chars(
        txtcaster_lib::orchestrator::scripting_cloud::CLOUD_SCRIPT_CTX,
    );
    let total = txtcaster_lib::providers::script::chunker::chunk_units(&units, budget).len() as i64;
    assert_eq!(total, 1, "premise: this fixture is a single chunk");
    let outcome = txtcaster_lib::providers::script::teacher::ChunkOutcome::Lecture {
        units,
        texts: vec!["Cloud lecture sentence.".to_string()],
        markers_dropped: 0,
        markers_omitted: 0,
    };
    job::upsert_script_chunk_checkpoint(
        &pool,
        &job_id,
        &job::ScriptChunkCheckpoint {
            chunk_idx: 0,
            units_fingerprint: fingerprint,
            total_chunks: total,
            outcome_json: serde_json::to_string(&vec![outcome]).unwrap(),
            summary: "recorded".to_string(),
        },
    )
    .await
    .expect("seed checkpoint");

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);
    assert!(
        cloud_requests.lock().unwrap().is_empty(),
        "every chunk was checkpointed: the provider must never be dialed again"
    );
    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].text, "Cloud lecture sentence.");
    // The seam still segmented the RECORDED outcome.
    assert!(engine_requests
        .lock()
        .unwrap()
        .iter()
        .any(|(path, _)| path == "/script/segments"));
    assert!(
        job::list_script_chunk_checkpoints(&pool, &job_id)
            .await
            .expect("list checkpoints")
            .is_empty(),
        "settled checkpoints are cleaned up"
    );
}

#[tokio::test]
async fn stale_chunk_checkpoints_are_discarded_not_resumed_against_new_text() {
    let (_dir, pool) = test_pool().await;
    let (cloud_port, cloud_requests) = spawn_cloud_llm(Arc::new(Mutex::new(None)));
    let (engine_port, _engine_requests) = spawn_seam_engine();
    let (job_id, _connection_id) = seed_cloud_binding_job(&pool, cloud_port).await;
    job::upsert_script_chunk_checkpoint(
        &pool,
        &job_id,
        &job::ScriptChunkCheckpoint {
            chunk_idx: 0,
            units_fingerprint: "fingerprint-of-some-other-book".to_string(),
            total_chunks: 1,
            outcome_json: "[]".to_string(),
            summary: String::new(),
        },
    )
    .await
    .expect("seed stale checkpoint");

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);
    assert!(
        !cloud_requests.lock().unwrap().is_empty(),
        "a fingerprint mismatch regenerates rather than resuming against different text"
    );
}

#[tokio::test]
async fn an_empty_outcome_checkpoint_row_regenerates_instead_of_losing_content() {
    // Writer-side this row can no longer be produced; a hand-tampered or
    // legacy row must still not count as completed — the whole set is
    // discarded and the provider regenerates.
    let (_dir, pool) = test_pool().await;
    let (cloud_port, cloud_requests) = spawn_cloud_llm(Arc::new(Mutex::new(None)));
    let (engine_port, _engine_requests) = spawn_seam_engine();
    let (job_id, _connection_id) = seed_cloud_binding_job(&pool, cloud_port).await;

    let unit = txtcaster_lib::providers::script::chunker::SourceUnit {
        chapter_idx: 0,
        page_no: 1,
        kind: "text".to_string(),
        text: "Fixture page 1 body text.".to_string(),
        figure_id: None,
        region_type: None,
        caption: None,
        explanation: None,
        block_index: None,
        label: None,
    };
    use sha2::Digest;
    let fingerprint = format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_string(&vec![unit]).unwrap().as_bytes())
    );
    job::upsert_script_chunk_checkpoint(
        &pool,
        &job_id,
        &job::ScriptChunkCheckpoint {
            chunk_idx: 0,
            units_fingerprint: fingerprint,
            total_chunks: 1,
            outcome_json: "[]".to_string(),
            summary: String::new(),
        },
    )
    .await
    .expect("seed tampered checkpoint");

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);
    assert!(
        !cloud_requests.lock().unwrap().is_empty(),
        "an empty 'completed' chunk must regenerate, never silently drop its text"
    );
    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1, "the chunk's content is present");
}

/// A cloud LLM that refuses one specific key with 401 and serves everyone
/// else — the pool-scale rotation fixture (S-5 blocker 5 / ledger row
/// "pool-scale credential states / multi-key rotation").
fn spawn_cloud_llm_auth_gate(dead_key: &'static str) -> (u16, CapturedRequests) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let requests: CapturedRequests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let captured = captured.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut req = Vec::new();
                let header_end = loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&req[..header_end]).to_string();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while req.len() < header_end + content_length {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                captured
                    .lock()
                    .unwrap()
                    .push(("/v1/chat/completions".to_string(), head.clone()));
                if head.contains(dead_key) {
                    let _ = mock_http::write_response(
                        &mut sock,
                        "401 Unauthorized",
                        "application/json",
                        br#"{"error":{"message":"bad key"}}"#,
                    )
                    .await;
                    return;
                }
                let payload = serde_json::json!({
                    "id": "chatcmpl-rotate",
                    "object": "chat.completion",
                    "model": "bound-model",
                    "choices": [{
                        "index": 0,
                        "finish_reason": "stop",
                        "message": {
                            "role": "assistant",
                            "content": "[{\"text\": \"Cloud lecture sentence.\"}]"
                        }
                    }],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5}
                })
                .to_string();
                let _ = mock_http::write_response(
                    &mut sock,
                    "200 OK",
                    "application/json",
                    payload.as_bytes(),
                )
                .await;
            });
        }
    });
    (port, requests)
}

/// Ledger row "pool-scale credential states / multi-key rotation", stage
/// level: the FIRST run dials slot 0, the provider refuses it (401), the
/// outcome demotes the slot and the stage rotates in-run to slot 1 and
/// completes without sleeping; the SECOND run resolves slot 1 directly —
/// the demotion is durable and rotation is outcome-aware through the
/// production wired path.
#[tokio::test]
async fn a_dead_key_is_demoted_by_its_outcome_and_the_next_run_rotates_past_it() {
    let (_dir, pool) = test_pool().await;
    let (cloud_port, cloud_requests) = spawn_cloud_llm_auth_gate("sk-dead-0000aaaa");
    let (engine_port, _engine_requests) = spawn_seam_engine();

    let base_url = format!("http://127.0.0.1:{cloud_port}/v1");
    let keys =
        std::sync::Arc::new(txtcaster_lib::voice::keys::KeyManager::session_only_for_tests());
    keys.record_consent(&pool, &base_url)
        .await
        .expect("consent");
    keys.add_key(
        &pool,
        "openai_compat",
        Some(&base_url),
        "sk-dead-0000aaaa".into(),
        None,
    )
    .await
    .expect("slot 0");
    keys.add_key(
        &pool,
        "openai_compat",
        Some(&base_url),
        "sk-live-1111bbbb".into(),
        None,
    )
    .await
    .expect("slot 1");

    let connection = ConnectionStore::create(
        &pool,
        "Keyed loopback",
        "openai_compatible",
        Some(&base_url),
        false,
        "store-default-model",
        1_756_000_000,
    )
    .await
    .expect("connection");
    let (book_id, voice_id) = seed(&pool).await;
    let overrides = serde_json::json!({
        "script_binding": {
            "kind": "connection",
            "connection_id": connection.id,
            "model": "bound-model",
            "provider": "openai_compatible",
            "endpoint": connection.key_base_url(),
            "adapter_rev": "openai_compat/1",
        }
    })
    .to_string();
    let job_id =
        insert_full_job(&pool, &book_id, &voice_id, "teacher", None, Some(overrides)).await;

    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    )
    .with_key_manager(keys.clone());

    // Run 1: the dead key is dialed and refused; the outcome demotes the
    // slot and the stage rotates in-run to the live key, completing the
    // run instead of failing it.
    let outcome = run_stage(&stage, &pool, &job_id)
        .await
        .expect("in-run rotation completes the first run");
    assert_eq!(outcome, StageOutcome::Completed);

    // The settled outcome demoted slot 0: the pool now resolves slot 1.
    let next = keys
        .resolve_pool_key(&pool, "openai_compat", &base_url)
        .await
        .expect("a live slot remains");
    assert_eq!(next.slot, 1, "rotation learned the outcome");

    // Run 2 completes over the live key; the demoted dead key is never
    // re-dialed.
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("second run");
    assert_eq!(outcome, StageOutcome::Completed);
    let heads = cloud_requests.lock().unwrap().clone();
    assert!(
        heads
            .first()
            .is_some_and(|(_, head)| head.contains("sk-dead-0000aaaa")),
        "run 1 dialed the dead key first"
    );
    assert_eq!(
        heads
            .iter()
            .filter(|(_, head)| head.contains("sk-dead-0000aaaa"))
            .count(),
        1,
        "the demoted dead key is never re-dialed"
    );
    assert!(
        heads
            .last()
            .is_some_and(|(_, head)| head.contains("sk-live-1111bbbb")),
        "the completing dial used the rotated-to key"
    );
}

/// Ledger row "Cloud/cloud execution" (stage-level fixture; LIVE cloud
/// stays owner-run): ONE job records BOTH cloud bindings; the Script phase
/// runs the production wired path over a real loopback wire, and the TTS
/// phase settles the SAME job's segments through the production cloud
/// persistence seam with a timestamp-less artifact (the OpenAI shape) —
/// audio cached, timing honestly left to align. Nothing local ran.
#[tokio::test]
async fn one_job_settles_cloud_script_and_cloud_tts_together() {
    use txtcaster_lib::providers::registry::{ScriptError, TtsArtifact, TtsSynth};

    struct TimestamplessTts {
        dir: std::path::PathBuf,
    }
    #[async_trait::async_trait]
    impl TtsSynth for TimestamplessTts {
        async fn synthesize(
            &mut self,
            _text: &str,
            _voice: &str,
        ) -> Result<TtsArtifact, ScriptError> {
            let path = self.dir.join(format!("cloud-{}.wav", new_id()));
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
            // native_words: None ALWAYS — the pinned OpenAI TTS contract.
            Ok(TtsArtifact {
                audio_path: path,
                native_words: None,
            })
        }
    }

    let (dir, pool) = test_pool().await;
    let (cloud_port, _cloud_requests) = spawn_cloud_llm(Arc::new(Mutex::new(None)));
    let (engine_port, _engine_requests) = spawn_seam_engine();
    let (job_id, connection_id) = seed_cloud_binding_job(&pool, cloud_port).await;
    // The SAME job also records a cloud TTS binding (openai_tts, the
    // timestamp-less provider).
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let mut overrides: serde_json::Value =
        serde_json::from_str(record.model_overrides_json.as_deref().unwrap()).unwrap();
    overrides["tts_binding"] = serde_json::json!({
        "kind": "connection",
        "connection_id": connection_id,
        "model": "gpt-4o-mini-tts",
        "voice": "alloy",
        "provider": "openai_tts",
        "endpoint": "https://api.openai.com",
        "adapter_rev": "openai_tts/1",
    });
    sqlx::query("UPDATE render_job SET model_overrides_json = ? WHERE id = ?")
        .bind(overrides.to_string())
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();

    // Cloud Script over the wire (the production stage).
    let stage = ScriptingStage::new(
        Arc::new(FakeSidecar { port: engine_port }),
        std::env::temp_dir(),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("script");
    assert_eq!(outcome, StageOutcome::Completed);
    let segments = job::list_segments(&pool, &job_id).await.expect("list");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].text, "Cloud lecture sentence.");

    // Cloud TTS settlement over the SAME job's segments (the production
    // persistence seam; the fixed-host wire is the owner-run live half).
    let (_handle, receiver) = control_channel();
    let (progress, _events) = noop_progress();
    let ctx = StageContext::new(
        pool.clone(),
        job_id.clone(),
        JobState::Synthesizing,
        Checkpoint::new(JobState::Synthesizing, now_ms()),
        receiver,
        progress,
    );
    let cache_dir = dir.path().join("cache");
    let mut adapter = TimestamplessTts {
        dir: dir.path().to_path_buf(),
    };
    let settled = txtcaster_lib::orchestrator::synthesis_cloud::synthesize_segment(
        &ctx,
        &mut adapter,
        &segments[0],
        "alloy",
        &cache_dir,
        "OpenAI TTS",
    )
    .await
    .expect("cloud tts settles");
    assert!(!settled.native_timing, "timestamp-less: align owns timing");
    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    assert_eq!(
        stored[0].audio_hash.as_deref(),
        Some(settled.audio_hash.as_str()),
        "the cloud-script segment carries its cloud-tts audio"
    );
    assert!(
        job::get_alignment(&pool, &segments[0].id)
            .await
            .unwrap()
            .is_none(),
        "no native alignment row — the local align phase picks this up \
         (chained in aligning_stage.rs)"
    );
}

const JOB_PIN_KEY: &str = "AIzaSy-job-pin-material-0123456789abcdef";

#[tokio::test]
async fn a_backfill_hands_stored_keys_to_the_wire_and_keeps_them_out_of_the_job_record() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    sqlx::query("UPDATE book SET blocks_version = NULL WHERE id = ?")
        .bind(&book_id)
        .execute(&pool)
        .await
        .expect("clear version");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let keys = Arc::new(KeyManager::session_only_for_tests());
    keys.add_key(&pool, "gemini", None, JOB_PIN_KEY.to_string(), None)
        .await
        .expect("add key");
    storage::settings::set_setting(&pool, FALLBACK_ENABLED_KEY, "true")
        .await
        .expect("enable");
    let (port, _paths, requests) =
        spawn_router_server_with(sse_body(&[("segments", r#"{"segments":[]}"#)]), false);
    let stage = ScriptingStage::new(Arc::new(FakeSidecar { port }), std::env::temp_dir())
        .with_key_manager(keys);

    let err = run_stage(&stage, &pool, &job_id)
        .await
        .expect_err("the fake /ingest streams no manifest, so the backfill fails");

    let ingest_body = requests
        .lock()
        .unwrap()
        .iter()
        .find(|(path, _)| path == "/ingest")
        .map(|(_, body)| body.clone())
        .expect("an /ingest request was captured");
    let ingest: serde_json::Value = serde_json::from_str(&ingest_body).expect("ingest json");
    assert_eq!(ingest["fallback"]["google_key"], JOB_PIN_KEY, "{ingest}");
    assert!(ingest["fallback"].get("openai_key").is_none(), "{ingest}");

    let message = err.to_string();
    assert!(!message.contains(JOB_PIN_KEY), "{message}");
    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job query")
        .expect("job row");
    let persisted = format!("{record:?}");
    assert!(!persisted.contains(JOB_PIN_KEY), "{persisted}");
}
