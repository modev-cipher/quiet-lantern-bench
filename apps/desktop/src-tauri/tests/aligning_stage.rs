#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use txtcaster_lib::orchestrator::align::{
    align_words_cache_key, FLAG_FAILED, FLAG_SKIPPED_NO_MODEL,
};
use txtcaster_lib::orchestrator::stage::{
    control_channel, ProgressEvent, ProgressSink, Stage, StageContext, StageOutcome,
};
use txtcaster_lib::orchestrator::state::{Checkpoint, JobState, OrchestratorError, Scope};
use txtcaster_lib::orchestrator::{AligningStage, SidecarProvider};
use txtcaster_lib::storage::{self, book, cache, job, models, new_id, now_ms, voice, SqlitePool};
use txtcaster_lib::words::{self, Timebase, WordProvenance, WordsJson};

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

fn materialize_align_model(dir: &tempfile::TempDir, model_id: &str) {
    let entry = txtcaster_lib::catalog::bundled_catalog()
        .models
        .iter()
        .find(|e| e.id == model_id)
        .expect("catalog entry");
    let root = dir.path().join("models").join(format!("align/{model_id}/"));
    std::fs::create_dir_all(&root).expect("mkdir");
    for file in &entry.download.files {
        std::fs::write(root.join(&file.dest_path), b"stub").expect("write");
    }
}

async fn seed(pool: &SqlitePool, dir: &tempfile::TempDir, install_model: bool) -> (String, String) {
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
        params_json: None,
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &profile)
        .await
        .expect("voice");
    if install_model {
        let install = models::ModelInstall {
            model_id: "whisperx-small-int8".into(),
            path: "align/whisperx-small-int8/".into(),
            sha256: "deadbeef".into(),
            disk_bytes: 1,
            source: "curated".into(),
            installed_at: now_ms(),
        };
        models::insert_model_install(pool, &install)
            .await
            .expect("install");
        materialize_align_model(dir, "whisperx-small-int8");
    }
    (book.id, profile.id)
}

async fn insert_verify_job(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
    let job_id = insert_job(pool, book_id, voice_id).await;
    sqlx::query("UPDATE render_job SET verify = 1 WHERE id = ?")
        .bind(&job_id)
        .execute(pool)
        .await
        .expect("enable verify");
    job_id
}

fn verify_report(record: &job::RenderJob) -> serde_json::Value {
    let raw = record.report_json.as_deref().expect("report json");
    let report: serde_json::Value = serde_json::from_str(raw).expect("valid report");
    report["verify"].clone()
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
        align_plan_json: Some(
            r#"{"model_id":"whisperx-small-int8","device":"cpu","align_workers":2,"badge":"green"}"#
                .into(),
        ),
        state: JobState::Aligning.as_str().into(),
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

fn make_segment(
    job_id: &str,
    idx: i64,
    text: &str,
    audio_hash: Option<&str>,
) -> job::ScriptSegment {
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
        audio_hash: audio_hash.map(str::to_string),
        duration_ms: audio_hash.map(|_| 800),
        original_text: None,
    }
}

fn sse_body(events: &[(&str, &str)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

type CapturedRequests = Arc<Mutex<Vec<(String, String)>>>;

fn spawn_align_server(align_sse: String) -> (u16, CapturedRequests) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let requests: CapturedRequests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let sse_bytes = align_sse.into_bytes();
    tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("tokio listener");
        while let Ok((mut sock, _)) = listener.accept().await {
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
                captured.lock().unwrap().push((path.clone(), request_body));
                if path == "/health" {
                    let health = r#"{"status":"ok","ct2_cuda":true}"#;
                    let _ = mock_http::write_response(
                        &mut sock,
                        "200 OK",
                        "application/json",
                        health.as_bytes(),
                    )
                    .await;
                    return;
                }
                let _ =
                    mock_http::write_response(&mut sock, "200 OK", "text/event-stream", &sse_bytes)
                        .await;
            });
        }
    });
    (port, requests)
}

fn noop_progress() -> (ProgressSink, Arc<Mutex<Vec<ProgressEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = events.clone();
    let sink: ProgressSink = Arc::new(move |event| sink_events.lock().unwrap().push(event));
    (sink, events)
}

const FIXTURE_AUDIO_HASHES: [&str; 4] = ["aaaa", "bbbb", "cccc", "cafe"];

fn make_stage(provider: Arc<dyn SidecarProvider>, dir: &tempfile::TempDir) -> AligningStage {
    let cache_dir = dir.path().join("cache");
    for hash in FIXTURE_AUDIO_HASHES {
        if !txtcaster_lib::render::mp3::is_valid_wav_artifact(&cache_dir, hash) {
            write_cache_file(&cache_dir, hash, 64);
        }
    }
    AligningStage::new(provider, dir.path().join("models"), cache_dir)
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
        JobState::Aligning,
        Checkpoint::new(JobState::Aligning, now_ms()),
        receiver,
        progress,
    );
    stage.run(&mut ctx).await
}

fn align_request_body(requests: &CapturedRequests) -> serde_json::Value {
    let captured = requests.lock().unwrap();
    let (_, body) = captured
        .iter()
        .find(|(path, _)| path == "/align")
        .expect("align request captured");
    serde_json::from_str(body).expect("valid json body")
}

#[tokio::test]
async fn a_plan_from_a_newer_revision_is_not_overwritten_by_this_binary() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let future = r#"{"model_id":"whisperx-small-int8","plan_revision":99,"device":{"backend":"cuda","ordinal":0},"align_workers":2,"badge":"green"}"#;
    job::set_job_align_plan(&pool, &job_id, future)
        .await
        .expect("seed a future plan");
    job::insert_segments(&pool, &[make_segment(&job_id, 0, "Boom.", Some("aaaa"))])
        .await
        .expect("segments");

    let (port, _requests) = spawn_align_server(sse_body(&[("done", r#"{"done":0,"total":0}"#)]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await;

    let stored = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row")
        .align_plan_json
        .expect("align plan");
    let parsed: serde_json::Value = serde_json::from_str(&stored).expect("plan json");
    assert_eq!(
        parsed["plan_revision"], 99,
        "the unit test forbids downgrading a newer revision, but the stage swallowed the \
         error, recomputed and wrote revision 2 straight over it: {outcome:?}"
    );
}

#[tokio::test]
async fn a_legacy_align_plan_is_upcast_and_rewritten_at_the_read() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let legacy =
        r#"{"model_id":"whisperx-small-int8","device":"cuda:1","align_workers":2,"badge":"green"}"#;
    job::set_job_align_plan(&pool, &job_id, legacy)
        .await
        .expect("seed legacy align plan");
    let segments = vec![make_segment(&job_id, 0, "Boom.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"done":1,"total":1,"result":{"segment_idx":0,"words":[]}}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[
        ("result", result),
        ("done", r#"{"done":1,"total":1}"#),
    ]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let _ = run_stage(&stage, &pool, &job_id).await;

    let stored = job::get_job(&pool, &job_id)
        .await
        .expect("job")
        .expect("row")
        .align_plan_json
        .expect("align plan");
    let stored: serde_json::Value = serde_json::from_str(&stored).expect("plan json");
    assert_eq!(
        stored["plan_revision"], 2,
        "the align column bypassed the upcast choke point, so a legacy row stayed legacy \
         however many stages ran over it"
    );
    assert_eq!(stored["device"]["backend"], "cuda");
    assert_eq!(stored["device"]["ordinal"], 1);
}

#[tokio::test]
async fn an_align_result_without_a_declared_timebase_is_stored_stale() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "First sentence.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let undeclared = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"First","s":0,"e":400}],"cache_hit":true,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", undeclared), ("done", done)]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    assert_eq!(
        run_stage(&stage, &pool, &job_id).await.expect("stage runs"),
        StageOutcome::Completed
    );

    let row = job::get_alignment(&pool, &segments[0].id)
        .await
        .expect("query")
        .expect("alignment row");
    let payload = row.words_json.parse();
    assert_eq!(
        payload.timebase,
        Timebase::Unknown,
        concat!(
            "an engine build that predates the declaration must not be read as trimmed here ",
            "just because whisperx happens to align the written WAV; the consumer never ",
            "infers, and the align producer is what declares (engine align/pipeline.py ",
            "cache-hit branch)"
        )
    );
    assert_eq!(
        words::classify(&payload, &row.source),
        WordProvenance::Stale
    );
}

#[tokio::test]
async fn an_align_result_that_declares_the_trimmed_timebase_is_stored_fresh() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "First sentence.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let declared = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"First","s":0,"e":400}],"cache_hit":true,"flags":[],"words_timebase":"trimmed"}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", declared), ("done", done)]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    assert_eq!(
        run_stage(&stage, &pool, &job_id).await.expect("stage runs"),
        StageOutcome::Completed
    );

    let row = job::get_alignment(&pool, &segments[0].id)
        .await
        .expect("query")
        .expect("alignment row");
    let payload = row.words_json.parse();
    assert!(payload.versioned);
    assert_eq!(payload.timebase, Timebase::Trimmed);
    assert_eq!(
        words::classify(&payload, &row.source),
        WordProvenance::Fresh
    );
}

#[tokio::test]
async fn aligning_stage_persists_alignment_and_cache_entry() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![
        make_segment(&job_id, 0, "First sentence.", Some("aaaa")),
        make_segment(&job_id, 1, "Second sentence.", Some("bbbb")),
    ];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let first = r#"{"stage":"align","done":1,"total":2,"result":{"idx":0,"words":[{"w":"First","s":0,"e":400},{"w":"sentence.","s":400,"e":800}],"cache_hit":false,"flags":[]}}"#;
    let second = r#"{"stage":"align","done":2,"total":2,"result":{"idx":1,"words":[{"w":"Second","s":0,"e":420}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":2,"total":2}"#;
    let (port, requests) = spawn_align_server(sse_body(&[
        ("result", first),
        ("result", second),
        ("done", done),
    ]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let alignment = job::get_alignment(&pool, &segments[0].id)
        .await
        .expect("query")
        .expect("alignment row");
    assert_eq!(alignment.source, "whisperx");
    let words: Vec<serde_json::Value> = alignment.words_json.parse().words;
    assert_eq!(words.len(), 2);

    let key = align_words_cache_key("aaaa", "whisperx-small-int8");
    let entry = cache::get_entry(&pool, &key).await.expect("query");
    assert!(entry.is_some());

    let body = align_request_body(&requests);
    assert_eq!(body["model_id"], "whisperx-small-int8");
    assert_eq!(body["language"], "en");
    let sent = body["segments"].as_array().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["audio_hash"], "aaaa");
    let audio_path = sent[0]["audio_path"].as_str().unwrap();
    assert!(audio_path.contains("aa"));
    assert!(audio_path.ends_with("aaaa"));
}

#[tokio::test]
async fn aligning_stage_skips_segments_with_native_alignment() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![
        make_segment(&job_id, 0, "Native.", Some("aaaa")),
        make_segment(&job_id, 1, "Needs align.", Some("bbbb")),
    ];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: segments[0].id.clone(),
            source: "tts_native".into(),
            words_json: WordsJson::from_stored(r#"[{"w":"Native.","s":0,"e":500}]"#.into()),
            wer: None,
        },
    )
    .await
    .expect("native alignment");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":1,"words":[{"w":"Needs","s":0,"e":200}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let body = align_request_body(&requests);
    let sent = body["segments"].as_array().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["idx"], 1);

    let native = job::get_alignment(&pool, &segments[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(native.source, "tts_native");
}

#[tokio::test]
async fn aligning_stage_completes_without_sidecar_call_when_all_aligned() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "Done.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: segments[0].id.clone(),
            source: "tts_native".into(),
            words_json: WordsJson::from_stored("[]".into()),
            wer: None,
        },
    )
    .await
    .expect("alignment");

    let (port, requests) = spawn_align_server(String::new());
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn aligning_stage_flags_segments_when_no_model_installed() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, false).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "No model.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let (port, requests) = spawn_align_server(String::new());
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);
    assert!(requests.lock().unwrap().is_empty());

    let rows = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = rows[0]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(flags.iter().any(|f| f == FLAG_SKIPPED_NO_MODEL));
    assert!(job::get_alignment(&pool, &rows[0].id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_reader_engine_skips_alignment_even_with_a_model_on_disk() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "Cloud voice.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let (port, requests) = spawn_align_server(String::new());
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir).with_local_models_check(|| false);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);
    assert!(requests.lock().unwrap().is_empty());

    let rows = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = rows[0]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(flags.iter().any(|f| f == FLAG_SKIPPED_NO_MODEL));
}

#[tokio::test]
async fn aligning_stage_skip_override_flags_segments() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = new_id();
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
        model_overrides_json: Some(r#"{"align": null}"#.into()),
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Aligning.as_str().into(),
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
    job::insert_job(&pool, &record).await.expect("job");
    let segments = vec![make_segment(&job_id, 0, "Skipped.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let (port, requests) = spawn_align_server(String::new());
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);
    assert!(requests.lock().unwrap().is_empty());

    let rows = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = rows[0]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(flags.iter().any(|f| f == FLAG_SKIPPED_NO_MODEL));
}

#[tokio::test]
async fn aligning_stage_records_failure_flags_without_alignment_row() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "Fails.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[],"cache_hit":false,"flags":["align_failed","error:boom"]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let rows = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = rows[0]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(flags.iter().any(|f| f == "align_failed"));
    assert!(job::get_alignment(&pool, &rows[0].id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn aligning_stage_error_on_cpu_plan_falls_back_and_completes() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "Boom.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let error = r#"{"code":"ERR_ALIGN_MODEL","message":"unsupported align model"}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("error", error)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let rows = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = rows[0]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(flags.iter().any(|f| f == FLAG_FAILED));
}

#[tokio::test]
async fn aligning_stage_contention_errors_even_on_a_cpu_plan_instead_of_downgrading_forever() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = new_id();
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
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: Some(
            r#"{"model_id":"whisperx-small-int8","device":"cpu","align_workers":2,"badge":"green"}"#
                .into(),
        ),
        state: JobState::Aligning.as_str().into(),
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
    job::insert_job(&pool, &record).await.expect("job");
    let segments = vec![make_segment(&job_id, 0, "Contended.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let error = r#"{"code":"ERR_BUSY","message":"model slot held by 'kokoro-82m'; retry after it is released"}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("error", error)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let err = run_stage(&stage, &pool, &job_id)
        .await
        .expect_err("contention must reach the orchestrator, not the fallback");
    assert_eq!(
        err.engine_code(),
        Some("ERR_BUSY"),
        "the orchestrator's backoff switches on the code, so propagating a sentence that \
         merely mentions it is no longer enough: {err}"
    );

    let stored = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = stored[0]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(
        !flags.iter().any(|f| f == FLAG_FAILED),
        "the sentence-highlight fallback flags segments terminally, so treating a transient \
         contention as a failed alignment destroys word timings for the whole book and a \
         resume skips those segments forever: {flags:?}"
    );
}

#[tokio::test]
async fn an_align_oom_degrades_to_sentence_highlight_rather_than_failing_the_job() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = new_id();
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
        model_overrides_json: None,
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: Some(
            r#"{"model_id":"whisperx-small-int8","device":"cuda:0","align_workers":2,"badge":"green"}"#
                .into(),
        ),
        state: JobState::Aligning.as_str().into(),
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
    job::insert_job(&pool, &record).await.expect("job");
    let segments = vec![make_segment(&job_id, 0, "Boom.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let error = r#"{"code":"ERR_OOM","message":"out of memory"}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("error", error)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id)
        .await
        .expect("alignment never fails a job");
    assert!(
        job::get_alignment(&pool, &segments[0].id)
            .await
            .expect("alignment lookup")
            .is_none(),
        "sentence highlight is the fallback because there are no word timestamps to highlight \
         with; an alignment row here would mean the stage kept something it never aligned"
    );
    let stored = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = stored[0]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(
        flags.iter().any(|f| f == FLAG_FAILED),
        "the flag is the fallback: render reads it to fall back to sentence-level highlighting \
         (PIPELINE_SPEC section 8). Asserting only Completed let this test pass without any of \
         the degradation its name claims: {flags:?}"
    );
    assert_eq!(
        outcome,
        StageOutcome::Completed,
        "known trade-off of the cpu-int8 ruling, recorded deliberately: align no longer reaches \
         the oom ladder, so an align oom degrades straight to sentence highlighting instead of \
         retrying with align_workers halved. The ladder cannot be used here because its terminal \
         rung is job failure, which PIPELINE_SPEC section 7 forbids for alignment. Contention is \
         the one code that still propagates on a cpu plan -- see \
         aligning_stage_contention_errors_even_on_a_cpu_plan_instead_of_downgrading_forever"
    );
}

const MISSING_CUBLAS: &str =
    "alignment failed: Library libcublas.so.12 is not found or cannot be loaded";

async fn insert_job_on_gpu(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
    let job_id = insert_job(pool, book_id, voice_id).await;
    job::set_job_align_plan(
        pool,
        &job_id,
        r#"{"model_id":"whisperx-small-int8","device":"cuda:0","align_workers":2,"badge":"green"}"#,
    )
    .await
    .expect("gpu align plan");
    job_id
}

#[tokio::test]
async fn a_device_fault_is_absorbed_because_cpu_int8_align_has_no_gpu_rung_to_reach() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job_on_gpu(&pool, &book_id, &voice_id).await;
    sqlx::query("UPDATE render_job SET verify = 1 WHERE id = ?")
        .bind(&job_id)
        .execute(&pool)
        .await
        .expect("enable verify");
    let segments = vec![make_segment(&job_id, 0, "Boom.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let error = serde_json::json!({"code": "ERR_DEVICE_UNAVAILABLE", "message": MISSING_CUBLAS})
        .to_string();
    let (port, _requests) = spawn_align_server(sse_body(&[("error", &error)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id)
        .await
        .expect("align is already on its last rung, so the stage absorbs the fault");
    assert_eq!(
        outcome,
        StageOutcome::Completed,
        "this job's align_plan_json still says cuda:0, but align now declares cpu-int8 and the \
         plan is clamped before the request leaves, so there is no gpu rung left to escalate to. \
         Propagating here would hand the ladder a fault it has no rung for, and PIPELINE_SPEC \
         section 7 forbids failing a job over alignment"
    );

    let stored = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = stored[0]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(
        flags.iter().any(|f| f == FLAG_FAILED),
        "the old assertion here was the opposite, and deliberately so: while a gpu rung existed, \
         flagging on the way out would have made the cpu rung skip the very segments it ran for. \
         Under cpu-int8 align there is no later rung, so the flag is what routes this segment to \
         the sentence-highlight fallback. Inverting that invariant without asserting the new one \
         left it unpinned: {flags:?}"
    );

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report = verify_report(&record);
    assert_eq!(
        report["counts"]["unchecked"], 1,
        "the accuracy check was on and the backend never read this segment back, so the verify \
         block is what has to say the check did not happen: {report}"
    );
    assert_eq!(report["counts"]["missing"], 0);
    assert_eq!(
        report["outcome"], "nothing_scored",
        "the previous assertion here was report_json.is_some(), which cannot fail: both \
         write_verify_report and merge_report_duration run unconditionally on this path: {report}"
    );
}

#[tokio::test]
async fn aligning_stage_device_fault_on_a_cpu_plan_falls_back_rather_than_failing_the_job() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "Boom.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let error = serde_json::json!({"code": "ERR_DEVICE_UNAVAILABLE", "message": MISSING_CUBLAS})
        .to_string();
    let (port, _requests) = spawn_align_server(sse_body(&[("error", &error)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id)
        .await
        .expect("the cpu rung is the last one, so the stage absorbs the fault");
    assert_eq!(
        outcome,
        StageOutcome::Completed,
        "alignment never fails a job (PIPELINE_SPEC section 7); on the last rung the words \
         degrade to sentence highlighting and the verify report is what has to say the \
         accuracy check did not run"
    );
}

#[tokio::test]
async fn aligning_stage_declares_its_stage_so_the_engine_can_evict() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "One sentence.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"One.","s":0,"e":300}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let body = align_request_body(&requests);
    assert_eq!(
        body["stage"], "ALIGNING",
        "batch alignment must identify its stage, otherwise the engine treats it as an \
         interactive call, keeps the align backend warm and never calls evict_stage: {body}"
    );
}

#[tokio::test]
async fn aligning_stage_excludes_terminally_flagged_segments_on_resume() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let mut failed = make_segment(&job_id, 0, "Failed before.", Some("aaaa"));
    failed.flags_json = Some(r#"["align_failed","error:boom"]"#.into());
    let segments = vec![failed, make_segment(&job_id, 1, "Fresh.", Some("bbbb"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":1,"words":[{"w":"Fresh.","s":0,"e":300}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let body = align_request_body(&requests);
    let sent = body["segments"].as_array().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["idx"], 1);
}

#[tokio::test]
async fn aligning_stage_ignores_duplicate_result_events() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "Once.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"Once.","s":0,"e":300}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[
        ("result", result),
        ("result", result),
        ("done", done),
    ]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let alignment = job::get_alignment(&pool, &segments[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alignment.source, "whisperx");
}

#[tokio::test]
async fn aligning_stage_flags_segments_missing_from_results() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![
        make_segment(&job_id, 0, "Comes back.", Some("aaaa")),
        make_segment(&job_id, 1, "Never comes back.", Some("bbbb")),
    ];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":2,"result":{"idx":0,"words":[{"w":"Comes","s":0,"e":300}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":2}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let rows = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = rows[1]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(flags.iter().any(|f| f == FLAG_FAILED));
    assert!(job::get_alignment(&pool, &rows[1].id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn aligning_stage_skips_when_model_assets_missing_on_disk() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let models_dir = dir.path().join("models/align/whisperx-small-int8");
    std::fs::remove_dir_all(&models_dir).expect("remove assets");
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "Assets gone.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let (port, requests) = spawn_align_server(String::new());
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);
    assert!(requests.lock().unwrap().is_empty());

    let rows = job::list_segments(&pool, &job_id).await.expect("segments");
    let flags: Vec<String> = rows[0]
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    assert!(flags.iter().any(|f| f == FLAG_SKIPPED_NO_MODEL));
}

#[tokio::test]
async fn aligning_stage_skips_segments_without_audio() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![
        make_segment(&job_id, 0, "No audio.", None),
        make_segment(&job_id, 1, "Has audio.", Some("bbbb")),
    ];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":1,"words":[{"w":"Has","s":0,"e":100}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let body = align_request_body(&requests);
    let sent = body["segments"].as_array().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["idx"], 1);
}

#[tokio::test]
async fn aligning_stage_computes_a_fresh_plan_when_the_column_is_null() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = new_id();
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
        model_overrides_json: None,
        resource_plan_json: Some(r#"{"model_id":"some-llm","device":"cuda","ctx":16384}"#.into()),
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Aligning.as_str().into(),
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
    job::insert_job(&pool, &record).await.expect("job");
    let segments = vec![make_segment(&job_id, 0, "One sentence.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"done":1,"total":1,"result":{"idx":0,"words":[{"word":"One","start_ms":0,"end_ms":100}]}}"#;
    let (port, requests) = spawn_align_server(sse_body(&[
        ("result", result),
        ("done", r#"{"done":1,"total":1}"#),
    ]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("stage runs");

    let stored = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let plan: serde_json::Value = serde_json::from_str(
        stored
            .align_plan_json
            .as_deref()
            .expect("align plan persisted"),
    )
    .expect("valid json");
    assert_eq!(plan["model_id"], "whisperx-small-int8");
    assert!(plan["align_workers"].is_number());
    assert!(
        plan.get("ctx").is_none(),
        "must not inherit gguf plan fields"
    );
    assert_eq!(
        stored.resource_plan_json.as_deref(),
        Some(r#"{"model_id":"some-llm","device":"cuda","ctx":16384}"#),
        "the script plan must never be modified by the align stage"
    );

    let sent: serde_json::Value = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == "/align")
        .filter_map(|(_, body)| serde_json::from_str::<serde_json::Value>(body).ok())
        .next()
        .expect("align request");
    assert_eq!(sent["resource_plan"]["model_id"], "whisperx-small-int8");
}

fn write_cache_file(cache_dir: &std::path::Path, hash: &str, bytes: usize) {
    let shard = cache_dir.join(&hash[..2]);
    std::fs::create_dir_all(&shard).expect("shard");
    std::fs::write(shard.join(hash), vec![b'x'; bytes]).expect("artifact");
}

async fn claimed_hashes(pool: &SqlitePool, job_id: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT hash FROM cache_ref WHERE job_id = ? ORDER BY hash")
        .bind(job_id)
        .fetch_all(pool)
        .await
        .expect("cache_ref")
}

#[tokio::test]
async fn aligning_stage_claims_a_reference_for_the_audio_it_reads() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "First sentence.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");
    let cache_dir = dir.path().join("cache");
    write_cache_file(&cache_dir, "aaaa", 4096);

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"First","s":0,"e":400}],"cache_hit":false,"flags":[]}}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[
        ("result", result),
        ("done", r#"{"stage":"align","done":1,"total":1}"#),
    ]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let claimed = claimed_hashes(&pool, &job_id).await;
    assert!(
        claimed.contains(&"aaaa".to_string()),
        "aligning read the WAV at aaaa without claiming it: {claimed:?}"
    );
    let key = align_words_cache_key("aaaa", "whisperx-small-int8");
    assert!(
        claimed.contains(&key),
        "aligning wrote {key} without claiming it: {claimed:?}"
    );
}

#[tokio::test]
async fn verify_off_leaves_native_timestamps_unscored() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "Native only.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: segments[0].id.clone(),
            source: "tts_native".into(),
            words_json: WordsJson::from_stored(r#"[{"w":"Native","s":0,"e":500}]"#.into()),
            wer: None,
        },
    )
    .await
    .expect("native alignment");

    let stage = make_stage(Arc::new(FakeSidecar { port: 0 }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let row = job::get_alignment(&pool, &segments[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.source, "tts_native");
    assert_eq!(row.wer, None);

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(verify_report(&record)["enabled"], false);
    assert_eq!(verify_report(&record)["counts"]["ok"], 0);
}

#[tokio::test]
async fn verify_on_forces_asr_over_native_timestamps_and_keeps_the_native_words() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_verify_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(
        &job_id,
        0,
        "The line rises steadily.",
        Some("aaaa"),
    )];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");
    let native_words = r#"[{"w":"The","s":0,"e":100},{"w":"line","s":100,"e":200}]"#;
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: segments[0].id.clone(),
            source: "tts_native".into(),
            words_json: WordsJson::from_stored(native_words.into()),
            wer: None,
        },
    )
    .await
    .expect("native alignment");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"The","s":0,"e":90},{"w":"line","s":90,"e":180},{"w":"rises","s":180,"e":260},{"w":"steadily","s":260,"e":400}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let body = align_request_body(&requests);
    assert_eq!(body["segments"].as_array().unwrap().len(), 1);

    let row = job::get_alignment(&pool, &segments[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.source, "tts_native");
    assert_eq!(row.words_json.as_str(), native_words);
    assert_eq!(row.wer, Some(0.0));

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report = verify_report(&record);
    assert_eq!(report["enabled"], true);
    assert_eq!(report["counts"]["ok"], 1);
    assert_eq!(report["counts"]["failed"], 0);
    assert_eq!(report["threshold"], 0.15);
}

#[tokio::test]
async fn verify_flags_a_mismatched_segment_and_reports_its_reference_length() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_verify_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "one two three four", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"one","s":0,"e":90},{"w":"two","s":90,"e":180},{"w":"three","s":180,"e":260},{"w":"banana","s":260,"e":400}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("stage runs");

    let row = job::get_alignment(&pool, &segments[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.source, "whisperx");
    assert_eq!(row.wer, Some(0.25));

    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    let flags: Vec<String> =
        serde_json::from_str(stored[0].flags_json.as_deref().unwrap_or("[]")).unwrap();
    assert!(flags.iter().any(|f| f == "verify_failed"));

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report = verify_report(&record);
    assert_eq!(report["counts"]["failed"], 1);
    assert_eq!(report["worst"][0]["wer"], 0.25);
    assert_eq!(report["worst"][0]["reference_words"], 4);
    assert_eq!(report["worst"][0]["band"], "failed");
}

#[tokio::test]
async fn verify_reports_unchecked_not_nothing_heard_when_the_align_model_is_absent() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, false).await;
    let job_id = insert_verify_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(
        &job_id,
        0,
        "Nothing can check this.",
        Some("aaaa"),
    )];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let stage = make_stage(Arc::new(FakeSidecar { port: 0 }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report = verify_report(&record);
    assert_eq!(report["enabled"], true);
    assert_eq!(
        report["counts"]["unchecked"], 1,
        "no checker ran on this segment. Counting it as 'missing' told the user his audio was \
         silent when the truth is that nothing listened to it: {report}"
    );
    assert_eq!(
        report["counts"]["missing"], 0,
        "'missing' is reserved for a checker that ran and heard nothing, which is the only \
         band in this report the user can act on: {report}"
    );
    assert_eq!(report["counts"]["ok"], 0);
}

#[tokio::test]
async fn verify_never_writes_a_null_wer_for_an_unscoreable_reference() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_verify_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "'", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"All","s":0,"e":90},{"w":"right","s":90,"e":180}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("stage runs");

    let row = job::get_alignment(&pool, &segments[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.wer, None);

    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    let flags: Vec<String> =
        serde_json::from_str(stored[0].flags_json.as_deref().unwrap_or("[]")).unwrap();
    assert!(!flags.iter().any(|f| f == "verify_failed"));

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report = verify_report(&record);
    assert_eq!(report["counts"]["unscoreable_ref"], 1);
    assert_eq!(report["counts"]["failed"], 0);
    assert!(serde_json::to_string(&report)
        .unwrap()
        .find("NaN")
        .is_none());
}

#[tokio::test]
async fn the_verify_report_leaves_the_rest_of_the_report_untouched() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_verify_job(&pool, &book_id, &voice_id).await;
    job::set_job_report_only(
        &pool,
        &job_id,
        r#"{"segment_count":7,"flag_counts":{"math_dropped":2},"durations_s":{"tts":11}}"#,
    )
    .await
    .expect("seed report");
    let segments = vec![make_segment(&job_id, 0, "one two three four", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"one","s":0,"e":90},{"w":"two","s":90,"e":180},{"w":"three","s":180,"e":260},{"w":"four","s":260,"e":400}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("stage runs");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    assert_eq!(report["segment_count"], 7);
    assert_eq!(report["flag_counts"]["math_dropped"], 2);
    assert_eq!(report["durations_s"]["tts"], 11);
    assert_eq!(report["verify"]["counts"]["ok"], 1);
}

#[tokio::test]
async fn a_scoring_only_segment_survives_a_missing_model_and_scores_on_the_next_run() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, false).await;
    let job_id = insert_verify_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "one two three four", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");
    let native_words = r#"[{"w":"one","s":0,"e":100}]"#;
    job::upsert_alignment(
        &pool,
        &job::Alignment {
            segment_id: segments[0].id.clone(),
            source: "tts_native".into(),
            words_json: WordsJson::from_stored(native_words.into()),
            wer: None,
        },
    )
    .await
    .expect("native alignment");

    let stage = make_stage(Arc::new(FakeSidecar { port: 0 }), &dir);
    assert_eq!(
        run_stage(&stage, &pool, &job_id).await.expect("first run"),
        StageOutcome::Completed
    );

    let stored = job::list_segments(&pool, &job_id).await.unwrap();
    let flags: Vec<String> =
        serde_json::from_str(stored[0].flags_json.as_deref().unwrap_or("[]")).unwrap();
    assert!(
        flags.is_empty(),
        "a segment that only wanted a score must not be terminally flagged: {flags:?}"
    );
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(verify_report(&record)["counts"]["unchecked"], 1);
    assert_eq!(verify_report(&record)["counts"]["missing"], 0);

    let install = models::ModelInstall {
        model_id: "whisperx-small-int8".into(),
        path: "align/whisperx-small-int8/".into(),
        sha256: "deadbeef".into(),
        disk_bytes: 1,
        source: "curated".into(),
        installed_at: now_ms(),
    };
    models::insert_model_install(&pool, &install)
        .await
        .expect("install");
    materialize_align_model(&dir, "whisperx-small-int8");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"one","s":0,"e":90},{"w":"two","s":90,"e":180},{"w":"three","s":180,"e":260},{"w":"four","s":260,"e":400}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    assert_eq!(
        run_stage(&stage, &pool, &job_id).await.expect("second run"),
        StageOutcome::Completed
    );

    let row = job::get_alignment(&pool, &segments[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.wer, Some(0.0));
    assert_eq!(row.source, "tts_native");
    assert_eq!(row.words_json.as_str(), native_words);
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(verify_report(&record)["counts"]["ok"], 1);
    assert_eq!(verify_report(&record)["counts"]["missing"], 0);
    assert_eq!(
        verify_report(&record)["counts"]["unchecked"],
        0,
        "once the checker runs, nothing is left unchecked; a count that sticks would keep \
         warning about a check that has since happened"
    );
}

#[tokio::test]
async fn a_loader_fault_leaves_every_segment_unchecked_and_never_reports_a_clean_check() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_verify_job(&pool, &book_id, &voice_id).await;
    let segments = vec![
        make_segment(&job_id, 0, "The line rises steadily.", Some("aaaa")),
        make_segment(&job_id, 1, "Slope measures that rise.", Some("bbbb")),
        make_segment(&job_id, 2, "Every reader hears it.", Some("cccc")),
    ];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let bodies: Vec<String> = (0..3)
        .map(|idx| {
            format!(
                r#"{{"stage":"align","done":{},"total":3,"result":{{"idx":{idx},"words":[],"cache_hit":false,"flags":["align_failed","error:Library libcublas.so.12 is not found or cannot be loaded"]}}}}"#,
                idx + 1
            )
        })
        .chain(std::iter::once(
            r#"{"stage":"align","done":3,"total":3}"#.to_string(),
        ))
        .collect();
    let mut events: Vec<(&str, &str)> = bodies[..3]
        .iter()
        .map(|body| ("result", body.as_str()))
        .collect();
    events.push(("done", bodies[3].as_str()));
    let (port, _requests) = spawn_align_server(sse_body(&events));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect(
        "PIPELINE_SPEC section 7 says alignment never fails a job; the verify block is what \
         has to carry the truth",
    );
    assert_eq!(outcome, StageOutcome::Completed);

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report = verify_report(&record);
    assert_eq!(
        report["counts"]["missing"], 0,
        "the backend failed before it read any of these segments back, so calling them \
         'missing' tells the user his audio was silent. PIPELINE_SPEC: 'missing is the user's \
         problem; unchecked is ours': {report}"
    );
    assert_eq!(
        report["counts"]["unchecked"], 3,
        "a segment the backend never got through is unchecked: {report}"
    );
    assert_eq!(report["counts"]["ok"], 0);
    assert_eq!(report["counts"]["failed"], 0);
    assert_eq!(
        report["outcome"], "nothing_scored",
        "this is the {{ok:0, failed:0}} shape that reported DONE in production; the report has \
         to say so in a field a caller can read, not only in a count: {report}"
    );
    assert_eq!(report["scored"], 0);
    assert_eq!(report["scoreable"], 3);
    assert!(
        report["worst"].as_array().expect("worst list").is_empty(),
        "a checker failure applies to every segment at once, so the worst-first list must stay \
         empty rather than bury the sentences the user can act on: {report}"
    );
}

#[tokio::test]
async fn a_segment_the_checker_read_back_and_heard_nothing_on_is_still_missing() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_verify_job(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(
        &job_id,
        0,
        "The line rises steadily.",
        Some("aaaa"),
    )];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[],"cache_hit":false,"flags":["align_empty"]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    run_stage(&stage, &pool, &job_id).await.expect("stage runs");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let report = verify_report(&record);
    assert_eq!(
        report["counts"]["missing"], 1,
        "align_empty means the backend ran on this audio and produced no words, which is real \
         silence and the one thing in this report the user can act on. Folding it into \
         'unchecked' would hide his problem behind ours: {report}"
    );
    assert_eq!(report["counts"]["unchecked"], 0);
}

#[tokio::test]
async fn a_stored_gpu_align_plan_is_clamped_to_the_cpu_before_the_request_leaves() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job_on_gpu(&pool, &book_id, &voice_id).await;
    let segments = vec![make_segment(&job_id, 0, "One sentence.", Some("aaaa"))];
    job::insert_segments(&pool, &segments)
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"One.","s":0,"e":300}],"cache_hit":false,"flags":[]}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));

    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("stage runs");
    assert_eq!(outcome, StageOutcome::Completed);

    let body = align_request_body(&requests);
    assert_eq!(
        body["resource_plan"]["device"]["backend"], "cpu",
        "a job planned before align was declared cpu-int8 still carries cuda:0 in \
         align_plan_json. Sending it unchanged would spend this job's gpu rung on a \
         libcublas.so.12 that cannot load, which is the whole failure A1 exists to remove: {body}"
    );
}

/// Ledger row "OpenAI timestamp-less TTS -> local align -> read-along"
/// (stage-level fixture; the live OpenAI wire is owner-run): a job whose
/// recorded TTS binding is the timestamp-less openai_tts provider left its
/// segment with cached audio and NO alignment row — exactly the pinned
/// cloud-TTS fallback shape. The local align phase picks the segment up,
/// produces a Trimmed-timebase envelope, and the words classify Fresh —
/// the read-along contract (word highlighting) is served.
#[tokio::test]
async fn an_openai_timestampless_cloud_segment_aligns_locally_and_reads_along() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed(&pool, &dir, true).await;
    let job_id = insert_job(&pool, &book_id, &voice_id).await;
    // Stamp the cloud binding onto the job: the align phase must not care
    // that synthesis was cloud — only that timing is absent.
    let binding = serde_json::json!({
        "tts_binding": {
            "kind": "connection",
            "connection_id": "c-openai",
            "model": "gpt-4o-mini-tts",
            "voice": "alloy",
            "provider": "openai_tts",
            "endpoint": "https://api.openai.com",
            "adapter_rev": "openai_tts/1",
        }
    })
    .to_string();
    sqlx::query("UPDATE render_job SET model_overrides_json = ? WHERE id = ?")
        .bind(&binding)
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();
    let segment = make_segment(&job_id, 0, "Cloud lecture sentence.", Some("cafe"));
    job::insert_segments(&pool, std::slice::from_ref(&segment))
        .await
        .expect("segments");

    let result = r#"{"stage":"align","done":1,"total":1,"result":{"idx":0,"words":[{"w":"Cloud","s":0,"e":200},{"w":"lecture","s":200,"e":520},{"w":"sentence.","s":520,"e":790}],"cache_hit":false,"flags":[],"words_timebase":"trimmed"}}"#;
    let done = r#"{"stage":"align","done":1,"total":1}"#;
    let (port, _requests) = spawn_align_server(sse_body(&[("result", result), ("done", done)]));
    let stage = make_stage(Arc::new(FakeSidecar { port }), &dir);
    let outcome = run_stage(&stage, &pool, &job_id).await.expect("aligns");
    assert_eq!(outcome, StageOutcome::Completed);

    let row = job::get_alignment(&pool, &segment.id)
        .await
        .expect("query")
        .expect("alignment row exists for the cloud segment");
    assert_eq!(row.source, "whisperx", "local align, not native timing");
    let payload = row.words_json.parse();
    assert_eq!(payload.timebase, Timebase::Trimmed);
    assert_eq!(payload.words.len(), 3);
    assert_eq!(
        words::classify(&payload, &row.source),
        WordProvenance::Fresh,
        "read-along word highlighting is served from this envelope"
    );
}
