#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use txtcaster_lib::catalog::bundled_catalog;
use txtcaster_lib::model_manager::install_rel_dir;
use txtcaster_lib::orchestrator::stage::{
    control_channel, ProgressEvent, ProgressSink, Stage, StageContext, StageOutcome,
};
use txtcaster_lib::orchestrator::state::{Checkpoint, JobState, OrchestratorError, Scope};
use txtcaster_lib::orchestrator::{SidecarProvider, VisionStage};
use txtcaster_lib::storage::{self, book, job, models, new_id, now_ms, voice, SqlitePool};

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
        content_hash: "a".repeat(64),
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
        params_json: None,
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &profile)
        .await
        .expect("voice");
    (book.id, profile.id)
}

async fn insert_job(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
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
        state: JobState::Vision.as_str().into(),
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

async fn insert_figure(pool: &SqlitePool, book_id: &str, id: &str, page: i64, crop: Option<&str>) {
    let figure = book::FigureRegion {
        id: id.into(),
        book_id: book_id.into(),
        page_no: page,
        bbox_json: "[0,0,10,10]".into(),
        kind: "chart".into(),
        confidence: 0.9,
        crop_hash: crop.map(|s| s.to_string()),
        caption: None,
        explanation: None,
        caption_confidence: None,
    };
    book::insert_figure_region_ignore_conflict(pool, &figure)
        .await
        .expect("figure");
}

fn install_vision_llm(models_root: &std::path::Path) -> models::ModelInstall {
    let entry = bundled_catalog()
        .models
        .iter()
        .find(|e| e.id == "gemma4-e4b-q4km")
        .expect("entry");
    let rel = install_rel_dir(entry);
    let dir = models_root.join(&rel);
    std::fs::create_dir_all(&dir).expect("mkdir");
    for file in &entry.download.files {
        std::fs::write(dir.join(&file.dest_path), b"stub").expect("write");
    }
    models::ModelInstall {
        model_id: entry.id.clone(),
        path: format!("{rel}/"),
        sha256: "deadbeef".into(),
        disk_bytes: 1,
        source: "curated".into(),
        installed_at: now_ms(),
    }
}

fn sse_body(events: &[(&str, &str)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn spawn_sse_server(body: String) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    let bytes = body.into_bytes();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
            let bytes = bytes.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let mut req = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    let header_end = req.windows(4).position(|w| w == b"\r\n\r\n");
                    if let Some(pos) = header_end {
                        let head = String::from_utf8_lossy(&req[..pos]).to_string();
                        if head.starts_with("GET /health") {
                            let _ = mock_http::write_response(
                                &mut sock,
                                "200 OK",
                                "application/json",
                                b"{\"status\":\"ok\"}",
                            )
                            .await;
                            return;
                        }
                        let content_length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if req.len() >= pos + 4 + content_length {
                            let body_bytes = &req[pos + 4..pos + 4 + content_length];
                            seen.lock()
                                .unwrap()
                                .push(String::from_utf8_lossy(body_bytes).to_string());
                            break;
                        }
                    }
                }
                let _ = mock_http::write_response(&mut sock, "200 OK", "text/event-stream", &bytes)
                    .await;
            });
        }
    });
    (port, bodies)
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
) -> (
    Result<StageOutcome, OrchestratorError>,
    Arc<Mutex<Vec<ProgressEvent>>>,
) {
    run_stage_with_checkpoint(
        stage,
        pool,
        job_id,
        Checkpoint::new(JobState::Vision, now_ms()),
    )
    .await
}

async fn run_stage_with_checkpoint(
    stage: &dyn Stage,
    pool: &SqlitePool,
    job_id: &str,
    checkpoint: Checkpoint,
) -> (
    Result<StageOutcome, OrchestratorError>,
    Arc<Mutex<Vec<ProgressEvent>>>,
) {
    let (_handle, receiver) = control_channel();
    let (progress, events) = progress_sink();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.to_string(),
        JobState::Vision,
        checkpoint,
        receiver,
        progress,
    );
    (stage.run(&mut ctx).await, events)
}

fn crop_hash(seed: &str) -> String {
    format!("{seed:0>64}")
}

fn write_crop(cache_dir: &std::path::Path, hash: &str) {
    let dir = cache_dir.join(&hash[..2]);
    std::fs::create_dir_all(&dir).expect("crop dir");
    std::fs::write(dir.join(hash), b"png").expect("crop file");
}

#[tokio::test]
async fn vision_stage_persists_plan_then_reuses_degraded_plan() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let install = install_vision_llm(&dir.path().join("models"));
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let hash = crop_hash("f1");
    write_crop(&dir.path().join("cache"), &hash);
    insert_figure(&pool, &book_id, "fig-1", 2, Some(&hash)).await;

    let caption =
        r#"{"region_id":"fig-1","caption":"A bar chart.","explanation":"Up.","confidence":0.9}"#;
    let (port, _bodies) = spawn_sse_server(sse_body(&[
        ("caption", caption),
        ("result", r#"{"captions":[],"flags":[]}"#),
    ]));
    let stage = VisionStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, _events) = run_stage(&stage, &pool, &job_id).await;
    result.expect("first run");

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row");
    let persisted: serde_json::Value = serde_json::from_str(
        record
            .vision_plan_json
            .as_deref()
            .expect("vision plan persisted"),
    )
    .expect("plan json");
    assert_eq!(persisted["model_id"], "gemma4-e4b-q4km");
    assert!(record.resource_plan_json.is_none());

    let degraded = serde_json::json!({
        "model_id": "gemma4-e4b-q4km",
        "device": "cpu",
        "n_gpu_layers": 0,
        "n_layers_total": 42,
        "kv_offload": false,
        "ctx": 4096,
        "batch": 256,
        "badge": "orange"
    });
    job::set_job_vision_plan(&pool, &job_id, &degraded.to_string())
        .await
        .expect("seed degraded plan");

    let hash2 = crop_hash("f2");
    write_crop(&dir.path().join("cache"), &hash2);
    insert_figure(&pool, &book_id, "fig-2", 3, Some(&hash2)).await;
    let caption2 =
        r#"{"region_id":"fig-2","caption":"A line chart.","explanation":"Down.","confidence":0.8}"#;
    let (port2, bodies2) = spawn_sse_server(sse_body(&[
        ("caption", caption2),
        ("result", r#"{"captions":[],"flags":[]}"#),
    ]));
    let stage2 = VisionStage::new(
        Arc::new(FakeSidecar { port: port2 }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result2, _events2) = run_stage(&stage2, &pool, &job_id).await;
    result2.expect("second run");

    let seen = bodies2.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&seen[0]).expect("body json");
    assert_eq!(
        sent["resource_plan"]["device"]["backend"], "cpu",
        "a legacy stored plan must be upcast before it reaches the engine, or the engine \
         parses a bare string it is no longer contracted to accept"
    );
    assert_eq!(sent["resource_plan"]["plan_revision"], 2);
    assert_eq!(sent["resource_plan"]["n_gpu_layers"], 0);

    let rewritten = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row")
        .vision_plan_json
        .expect("vision plan persisted");
    let rewritten: serde_json::Value = serde_json::from_str(&rewritten).expect("plan json");
    assert_eq!(
        rewritten["plan_revision"], 2,
        "the upcast must be written back, or every later stage upcasts the same row again"
    );
    assert_eq!(
        rewritten["device"]["backend"], "cpu",
        "a row stamped revision 2 while still holding a legacy device string would be \
         loaded as current and handed to an engine that no longer accepts that shape"
    );
    assert_eq!(sent["resource_plan"]["n_layers_total"], 42);
    assert_eq!(sent["resource_plan"]["ctx"], 4096);
    assert_eq!(sent["resource_plan"]["batch"], 256);
    assert_eq!(sent["resource_plan"]["kv_offload"], false);
}

#[tokio::test]
async fn vision_stage_persists_captions_and_report() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let install = install_vision_llm(&dir.path().join("models"));
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let hash = crop_hash("f1");
    write_crop(&dir.path().join("cache"), &hash);
    insert_figure(&pool, &book_id, "fig-1", 2, Some(&hash)).await;

    let caption = r#"{"region_id":"fig-1","caption":"A bar chart.","explanation":"Upward trend from Q1 to Q4.","confidence":0.9}"#;
    let (port, bodies) = spawn_sse_server(sse_body(&[
        ("caption", caption),
        ("progress", r#"{"stage":"vision","figure":1,"total":1}"#),
        ("result", r#"{"captions":[],"flags":[]}"#),
    ]));

    let stage = VisionStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, _events) = run_stage(&stage, &pool, &job_id).await;
    let outcome = result.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let figures = book::list_figures_for_page(&pool, &book_id, 2)
        .await
        .expect("figures");
    assert_eq!(figures.len(), 1);
    assert_eq!(figures[0].caption.as_deref(), Some("A bar chart."));
    assert_eq!(
        figures[0].explanation.as_deref(),
        Some("Upward trend from Q1 to Q4.")
    );
    assert_eq!(figures[0].caption_confidence, Some(0.9));

    let seen = bodies.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&seen[0]).expect("body json");
    assert_eq!(sent["model_id"], "gemma4-e4b-q4km");
    assert!(sent["mmproj_path"]
        .as_str()
        .expect("mmproj path")
        .contains("mmproj"));
    assert_eq!(sent["regions"][0]["region_id"], "fig-1");
    assert_eq!(sent["regions"][0]["region_type"], "chart");

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row");
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).expect("json");
    assert_eq!(report["figures"]["explained"], 1);
    assert!(report["figures"]["badge"].is_string());
    assert!(report["durations_s"]["vision"].is_number());
}

#[tokio::test]
async fn vision_stage_skips_with_flag_when_no_model_installed() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let hash = crop_hash("f2");
    write_crop(&dir.path().join("cache"), &hash);
    insert_figure(&pool, &book_id, "fig-1", 2, Some(&hash)).await;

    struct NoSidecar;
    impl SidecarProvider for NoSidecar {
        fn connection(&self) -> Option<(u16, String)> {
            panic!("sidecar must not be contacted when no vision model is installed");
        }
    }

    let stage = VisionStage::new(
        Arc::new(NoSidecar),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, _events) = run_stage(&stage, &pool, &job_id).await;
    let outcome = result.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let figures = book::list_figures_for_page(&pool, &book_id, 2)
        .await
        .expect("figures");
    assert_eq!(figures[0].caption, None);

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row");
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).expect("json");
    assert_eq!(report["vision_flags"][0], "vision_skipped_no_model");
    assert_eq!(report["figures"]["explained"], 0);
    assert!(report["figures"]["badge"].is_null());
}

#[tokio::test]
async fn vision_stage_completes_when_no_figures_in_scope() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    insert_figure(&pool, &book_id, "fig-out", 99, Some(&crop_hash("f3"))).await;

    struct NoSidecar;
    impl SidecarProvider for NoSidecar {
        fn connection(&self) -> Option<(u16, String)> {
            panic!("sidecar must not be contacted without figures");
        }
    }

    let stage = VisionStage::new(
        Arc::new(NoSidecar),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, _events) = run_stage(&stage, &pool, &job_id).await;
    let outcome = result.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);
}

#[tokio::test]
async fn vision_stage_ignores_already_captioned_figures() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let install = install_vision_llm(&dir.path().join("models"));
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let hash = crop_hash("f4");
    write_crop(&dir.path().join("cache"), &hash);
    insert_figure(&pool, &book_id, "fig-done", 2, Some(&hash)).await;
    book::set_figure_vision_output(&pool, "fig-done", "Existing.", "Kept.", 0.8)
        .await
        .expect("preset");

    struct NoSidecar;
    impl SidecarProvider for NoSidecar {
        fn connection(&self) -> Option<(u16, String)> {
            panic!("sidecar must not be contacted when all figures are captioned");
        }
    }

    let stage = VisionStage::new(
        Arc::new(NoSidecar),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, events) = run_stage(&stage, &pool, &job_id).await;
    let outcome = result.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);
    let last_message = events
        .lock()
        .unwrap()
        .last()
        .map(|event| event.current_item.clone())
        .expect("progress event");
    assert_eq!(last_message, "1 figure already explained");

    let figures = book::list_figures_for_page(&pool, &book_id, 2)
        .await
        .expect("figures");
    assert_eq!(figures[0].caption.as_deref(), Some("Existing."));

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row");
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).expect("json");
    assert_eq!(report["figures"]["explained"], 1);
}

#[tokio::test]
async fn vision_stage_rejects_unrequested_region_id() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let install = install_vision_llm(&dir.path().join("models"));
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let hash = crop_hash("f8");
    write_crop(&dir.path().join("cache"), &hash);
    insert_figure(&pool, &book_id, "fig-1", 2, Some(&hash)).await;

    let rogue =
        r#"{"region_id":"some-other-figure","caption":"X.","explanation":"Y.","confidence":0.9}"#;
    let (port, _bodies) = spawn_sse_server(sse_body(&[
        ("caption", rogue),
        ("result", r#"{"captions":[],"flags":[]}"#),
    ]));

    let stage = VisionStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, _events) = run_stage(&stage, &pool, &job_id).await;
    let err = result.unwrap_err();
    assert_eq!(err.engine_code(), Some("ERR_INVALID_REGION"), "{err}");

    let figures = book::list_figures_for_page(&pool, &book_id, 2)
        .await
        .expect("figures");
    assert_eq!(figures[0].caption, None);
}

#[tokio::test]
async fn vision_stage_skips_null_caption_for_degraded_region() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let install = install_vision_llm(&dir.path().join("models"));
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let hash = crop_hash("f9");
    write_crop(&dir.path().join("cache"), &hash);
    insert_figure(&pool, &book_id, "fig-1", 2, Some(&hash)).await;

    let degraded = r#"{"region_id":"fig-1","caption":null,"explanation":null,"confidence":0.0}"#;
    let (port, _bodies) = spawn_sse_server(sse_body(&[
        ("caption", degraded),
        ("progress", r#"{"stage":"vision","figure":1,"total":1}"#),
        (
            "result",
            r#"{"captions":[],"flags":["vision_low_confidence"]}"#,
        ),
    ]));

    let stage = VisionStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, _events) = run_stage(&stage, &pool, &job_id).await;
    let outcome = result.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let figures = book::list_figures_for_page(&pool, &book_id, 2)
        .await
        .expect("figures");
    assert_eq!(figures[0].caption, None);

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row");
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).expect("json");
    assert_eq!(report["vision_flags"][0], "vision_low_confidence");
    assert_eq!(report["figures"]["explained"], 0);
}

#[tokio::test]
async fn vision_stage_surfaces_sidecar_error() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let install = install_vision_llm(&dir.path().join("models"));
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let hash = crop_hash("f5");
    write_crop(&dir.path().join("cache"), &hash);
    insert_figure(&pool, &book_id, "fig-1", 2, Some(&hash)).await;

    let (port, _bodies) = spawn_sse_server(sse_body(&[(
        "error",
        r#"{"code":"ERR_OOM","message":"CUDA out of memory"}"#,
    )]));

    let stage = VisionStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, _events) = run_stage(&stage, &pool, &job_id).await;
    let err = result.unwrap_err();
    assert_eq!(err.engine_code(), Some("ERR_OOM"), "{err}");
}

#[tokio::test]
async fn vision_stage_returns_cancelled_on_engine_cancel() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let install = install_vision_llm(&dir.path().join("models"));
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let hash = crop_hash("f6");
    write_crop(&dir.path().join("cache"), &hash);
    insert_figure(&pool, &book_id, "fig-1", 2, Some(&hash)).await;

    let (port, _bodies) = spawn_sse_server(sse_body(&[("cancelled", r#"{}"#)]));

    let stage = VisionStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, _events) = run_stage(&stage, &pool, &job_id).await;
    let outcome = result.expect("run");
    assert_eq!(outcome, StageOutcome::Cancelled);
}

#[tokio::test]
async fn vision_stage_partial_captions_persist_before_error() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let install = install_vision_llm(&dir.path().join("models"));
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let hash_a = crop_hash("f7a");
    let hash_b = crop_hash("f7b");
    write_crop(&dir.path().join("cache"), &hash_a);
    write_crop(&dir.path().join("cache"), &hash_b);
    insert_figure(&pool, &book_id, "fig-a", 2, Some(&hash_a)).await;
    insert_figure(&pool, &book_id, "fig-b", 3, Some(&hash_b)).await;

    let caption =
        r#"{"region_id":"fig-a","caption":"First.","explanation":"Ok.","confidence":0.7}"#;
    let (port, _bodies) = spawn_sse_server(sse_body(&[
        ("caption", caption),
        ("progress", r#"{"stage":"vision","figure":1,"total":2}"#),
        (
            "error",
            r#"{"code":"ERR_OOM","message":"CUDA out of memory"}"#,
        ),
    ]));

    let stage = VisionStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let (result, _events) = run_stage(&stage, &pool, &job_id).await;
    let err = result.unwrap_err();
    assert_eq!(err.engine_code(), Some("ERR_OOM"), "{err}");

    let figures = book::list_figures_for_page(&pool, &book_id, 2)
        .await
        .expect("figures");
    assert_eq!(figures[0].caption.as_deref(), Some("First."));
    let checkpoint = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row")
        .checkpoint_json;
    let checkpoint: serde_json::Value =
        serde_json::from_str(checkpoint.as_deref().expect("checkpoint")).expect("json");
    assert_eq!(checkpoint["done_figures"], 1);
}

struct RecoveryMockSidecar {
    respawns: Arc<Mutex<u32>>,
    live_port: u16,
    dead_port: u16,
}

impl SidecarProvider for RecoveryMockSidecar {
    fn connection(&self) -> Option<(u16, String)> {
        let count = *self.respawns.lock().unwrap();
        if count == 0 {
            Some((self.dead_port, "test-token".into()))
        } else {
            Some((self.live_port, "test-token".into()))
        }
    }

    fn respawn(&self) {
        let mut count = self.respawns.lock().unwrap();
        *count += 1;
    }
}

#[tokio::test]
async fn recovery_revision_respawns_sidecar_and_completes() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let install = install_vision_llm(&dir.path().join("models"));
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");

    let dead_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead_port = dead_listener.local_addr().expect("addr").port();
    drop(dead_listener);

    let caption =
        r#"{"region_id":"fig-1","caption":"Recovered.","explanation":"Ok.","confidence":0.9}"#;
    let (live_port, _bodies) = spawn_sse_server(sse_body(&[
        ("caption", caption),
        ("result", r#"{"captions":[],"flags":[]}"#),
    ]));

    let hash = crop_hash("rec1");
    write_crop(&dir.path().join("cache"), &hash);
    insert_figure(&pool, &book_id, "fig-1", 2, Some(&hash)).await;

    let mut checkpoint = Checkpoint::new(JobState::Vision, now_ms());
    checkpoint.recovery_failures = 1;
    job::update_job_checkpoint(
        &pool,
        &job_id,
        &serde_json::to_string(&checkpoint).expect("json"),
    )
    .await
    .expect("set checkpoint");

    let respawns = Arc::new(Mutex::new(0));
    let stage = VisionStage::new(
        Arc::new(RecoveryMockSidecar {
            respawns: respawns.clone(),
            live_port,
            dead_port,
        }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );

    let (result, _events) = run_stage_with_checkpoint(&stage, &pool, &job_id, checkpoint).await;
    result.expect("completed successfully after respawn");

    assert!(*respawns.lock().unwrap() >= 1, "respawn was called");

    let figures = book::list_figures_for_page(&pool, &book_id, 2)
        .await
        .expect("figures");
    assert_eq!(figures[0].caption.as_deref(), Some("Recovered."));
}
