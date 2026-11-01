#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use txtcaster_lib::orchestrator::layout::scoped_pages;
use txtcaster_lib::orchestrator::stage::{
    control_channel, ProgressEvent, ProgressSink, Stage, StageContext, StageOutcome,
};
use txtcaster_lib::orchestrator::state::{Checkpoint, JobState, OrchestratorError, Scope};
use txtcaster_lib::orchestrator::{LayoutStage, SidecarProvider};
use txtcaster_lib::storage::{self, book, cache, job, new_id, now_ms, voice, SqlitePool};

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
        state: JobState::Layout.as_str().into(),
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
) -> Result<StageOutcome, OrchestratorError> {
    let (_handle, receiver) = control_channel();
    let (progress, _events) = progress_sink();
    let mut ctx = StageContext::new(
        pool.clone(),
        job_id.to_string(),
        JobState::Layout,
        Checkpoint::new(JobState::Layout, now_ms()),
        receiver,
        progress,
    );
    stage.run(&mut ctx).await
}

fn region_json(id: &str, page: i64, kind: &str, crop_path: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "page": page,
        "bbox": [10.0, 20.0, 200.0, 300.0],
        "type": kind,
        "confidence": 0.9,
        "crop_png_path": crop_path,
    })
}

#[tokio::test]
async fn layout_stage_persists_regions_and_cache_entries() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let crop_hash = format!("{:0>64}", "abc123");
    let crop_dir = dir.path().join("cache").join(&crop_hash[..2]);
    std::fs::create_dir_all(&crop_dir).expect("crop dir");
    let crop_path = crop_dir.join(&crop_hash);
    std::fs::write(&crop_path, b"fake-png-bytes").expect("crop file");

    let result = serde_json::json!({
        "regions": [
            region_json("region-1", 1, "chart", crop_path.to_str()),
            region_json("region-2", 2, "decorative", None),
        ],
        "flags": [],
    });
    let (port, bodies) = spawn_sse_server(sse_body(&[
        ("progress", r#"{"stage":"layout","page":1,"total":2}"#),
        ("progress", r#"{"stage":"layout","page":2,"total":2}"#),
        ("result", &result.to_string()),
    ]));

    let stage = LayoutStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let page1 = book::list_figures_for_page(&pool, &book_id, 1)
        .await
        .expect("page1");
    assert_eq!(page1.len(), 1);
    assert_eq!(page1[0].id, "region-1");
    assert_eq!(page1[0].kind, "chart");
    assert_eq!(page1[0].confidence, 0.9);
    assert_eq!(page1[0].bbox_json, "[10.0,20.0,200.0,300.0]");
    assert_eq!(page1[0].crop_hash.as_deref(), Some(crop_hash.as_str()));

    let page2 = book::list_figures_for_page(&pool, &book_id, 2)
        .await
        .expect("page2");
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].kind, "decorative");
    assert_eq!(page2[0].crop_hash, None);

    let entry = cache::get_entry(&pool, &crop_hash)
        .await
        .expect("cache")
        .expect("entry");
    assert_eq!(entry.kind, "crop_png");
    assert_eq!(entry.bytes, 14);

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).unwrap();
    assert_eq!(report["layout_flags"], serde_json::json!([]));
    assert_eq!(report["figures"]["detected"], serde_json::json!(2));
    assert!(report["durations_s"]["layout"].is_number());

    let request: serde_json::Value = bodies
        .lock()
        .unwrap()
        .iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .find(|value| value.get("book_id").is_some())
        .expect("layout request json");
    assert_eq!(request["book_id"].as_str(), Some(book_id.as_str()));
    assert_eq!(request["content_hash"].as_str().unwrap().len(), 64);
    assert_eq!(request["pages"].as_array().unwrap().len(), 10);
    assert!(request["docling_artifacts_dir"].is_null());
    assert_eq!(
        request["device"].as_str(),
        Some("cpu"),
        "layout must pass an explicit accelerator device, never library defaults"
    );
}

#[tokio::test]
async fn layout_stage_records_skip_flag_without_regions() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let (port, _bodies) = spawn_sse_server(sse_body(&[(
        "result",
        r#"{"regions":[],"flags":["layout_skipped"]}"#,
    )]));

    let stage = LayoutStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let page1 = book::list_figures_for_page(&pool, &book_id, 1)
        .await
        .expect("page1");
    assert!(page1.is_empty());

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).unwrap();
    assert_eq!(
        report["layout_flags"],
        serde_json::json!(["layout_skipped"])
    );
    assert_eq!(report["figures"]["detected"], serde_json::json!(0));
}

#[tokio::test]
async fn layout_stage_records_classifier_absent_flag() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let result = serde_json::json!({
        "regions": [region_json("region-1", 1, "figure", None)],
        "flags": ["layout_classifier_absent"],
    });
    let (port, _bodies) = spawn_sse_server(sse_body(&[("result", &result.to_string())]));

    let stage = LayoutStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("report")).unwrap();
    assert_eq!(
        report["layout_flags"],
        serde_json::json!(["layout_classifier_absent"])
    );
}

#[tokio::test]
async fn layout_stage_rerun_preserves_existing_vision_output() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let existing = book::FigureRegion {
        id: "region-1".into(),
        book_id: book_id.clone(),
        page_no: 1,
        bbox_json: "[10.0,20.0,200.0,300.0]".into(),
        kind: "chart".into(),
        confidence: 0.9,
        crop_hash: None,
        caption: Some("A bar chart".into()),
        explanation: Some("Sales rise".into()),
        caption_confidence: Some(0.8),
    };
    book::insert_figure_region(&pool, &existing)
        .await
        .expect("seed region");

    let result = serde_json::json!({
        "regions": [region_json("region-1", 1, "chart", None)],
        "flags": [],
    });
    let (port, _bodies) = spawn_sse_server(sse_body(&[("result", &result.to_string())]));

    let stage = LayoutStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Completed);

    let page1 = book::list_figures_for_page(&pool, &book_id, 1)
        .await
        .expect("page1");
    assert_eq!(page1.len(), 1);
    assert_eq!(page1[0].caption.as_deref(), Some("A bar chart"));
    assert_eq!(page1[0].explanation.as_deref(), Some("Sales rise"));
    assert_eq!(page1[0].caption_confidence, Some(0.8));
}

#[tokio::test]
async fn layout_stage_surfaces_sidecar_error() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let (port, _bodies) = spawn_sse_server(sse_body(&[(
        "error",
        r#"{"code":"ERR_LAYOUT","message":"conversion failed"}"#,
    )]));

    let stage = LayoutStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let error = run_stage(&stage, &pool, &job_id).await.expect_err("error");
    assert_eq!(error.engine_code(), Some("ERR_LAYOUT"), "{error}");
}

#[tokio::test]
async fn layout_stage_returns_cancelled_on_engine_cancel() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;

    let (port, _bodies) = spawn_sse_server(sse_body(&[("cancelled", r#"{"job_id":"whatever"}"#)]));

    let stage = LayoutStage::new(
        Arc::new(FakeSidecar { port }),
        dir.path().join("models"),
        dir.path().join("cache"),
    );
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("run");
    assert_eq!(outcome, StageOutcome::Cancelled);
}

#[test]
fn scoped_pages_from_page_range_clamps_to_page_count() {
    let scope = Scope::Pages {
        page_start: 8,
        page_end: 99,
    };
    let pages = scoped_pages(&scope, &[], Some(10));
    assert_eq!(pages, vec![8, 9, 10]);
}

#[test]
fn scoped_pages_from_chapters_dedups_overlap() {
    let chapters = vec![
        book::Chapter {
            id: "chap-0".into(),
            book_id: "book".into(),
            idx: 0,
            title: "One".into(),
            page_start: 1,
            page_end: 4,
            is_pseudo: false,
            word_count: None,
        },
        book::Chapter {
            id: "chap-1".into(),
            book_id: "book".into(),
            idx: 1,
            title: "Two".into(),
            page_start: 4,
            page_end: 6,
            is_pseudo: false,
            word_count: None,
        },
    ];
    let scope = Scope::Chapters {
        chapter_ids: vec!["chap-0".into(), "chap-1".into()],
    };
    let pages = scoped_pages(&scope, &chapters, Some(10));
    assert_eq!(pages, vec![1, 2, 3, 4, 5, 6]);
}
