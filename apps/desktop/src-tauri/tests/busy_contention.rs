#[path = "../src/test_support/mock_http.rs"]
mod mock_http;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use txtcaster_lib::orchestrator::{
    classify_failure, residency_check, BusyRetryPolicy, Checkpoint, JobOrchestrator, JobState,
    JobUpdate, OrchestratorError, ProgressEvent, ProgressSink, Scope, Stage, StageContext,
    StageFailure, StageOutcome, StageRegistry, StartOptions, UpdateSink, BUSY_NOTICE,
    BUSY_PAUSE_NOTICE, CONTENTION_KEY, MAX_CONTENTION_PAUSES, RECOVERY_NOTICE,
};
use txtcaster_lib::sidecar::client::{
    post_sse, SidecarError, REFUSAL_READ_TIMEOUT, UNSTRUCTURED_CODE,
};
use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice, SqlitePool};

const BUSY_DETAIL: &str = r#"{"detail":{"code":"ERR_BUSY","message":"model slot held by 'phi4-mini'; retry after it is released"}}"#;

const BUSY_MESSAGE: &str = "model slot held by 'phi4-mini'; retry after it is released";

fn engine_busy() -> OrchestratorError {
    OrchestratorError::Engine {
        status: Some(409),
        code: "ERR_BUSY".into(),
        message: BUSY_MESSAGE.into(),
    }
}

async fn spawn_refusing_server(status: &'static str, body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let _ = mock_http::write_response(
                    &mut sock,
                    status,
                    "application/json",
                    body.as_bytes(),
                )
                .await;
            });
        }
    });
    port
}

#[tokio::test]
async fn a_stream_refused_before_it_opens_carries_the_engine_error_code() {
    let port = spawn_refusing_server("409 Conflict", BUSY_DETAIL).await;
    let client = reqwest::Client::new();

    let error = post_sse(
        &client,
        port,
        "token",
        "/vision",
        "job-1",
        &serde_json::json!({}),
        |_| {},
    )
    .await
    .expect_err("the refusal must surface as an error");

    assert_eq!(
        error,
        SidecarError::Refused {
            status: 409,
            path: "/vision".into(),
            code: "ERR_BUSY".into(),
            message: BUSY_MESSAGE.into(),
        },
        "the engine refuses a contended slot with 409 before the stream opens, so a refusal \
         that drops or flattens the body leaves the orchestrator unable to tell contention \
         from any other 409"
    );
    assert_eq!(
        classify_failure(&OrchestratorError::from(error)),
        StageFailure::Contention,
        "the refusal the transport actually produces has to reach the backoff; asserting only \
         that it mentions the code lets a format change break classification silently"
    );
}

#[tokio::test]
async fn a_refusal_whose_body_never_arrives_gives_up_instead_of_hanging_the_stage() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let _ = mock_http::write_head(
                    &mut sock,
                    "409 Conflict",
                    &[("Content-Type", "application/json")],
                    Some(400),
                )
                .await;
                tokio::time::sleep(Duration::from_secs(120)).await;
            });
        }
    });
    let client = reqwest::Client::new();

    let started = Instant::now();
    let error = tokio::time::timeout(
        REFUSAL_READ_TIMEOUT * 4,
        post_sse(
            &client,
            port,
            "token",
            "/vision",
            "job-1",
            &serde_json::json!({}),
            |_| {},
        ),
    )
    .await
    .expect("reading a refusal body must be bounded, not block the stage forever")
    .expect_err("the refusal must still surface as an error");

    assert_eq!(
        error,
        SidecarError::Refused {
            status: 409,
            path: "/vision".into(),
            code: UNSTRUCTURED_CODE.into(),
            message: String::new(),
        },
        "a refusal whose body never arrives still has to report its status, and must not \
         invent a code it never read"
    );
    assert_eq!(
        classify_failure(&OrchestratorError::from(error)),
        StageFailure::Terminal,
        "an unread body is not evidence of contention"
    );
    assert!(
        started.elapsed() >= REFUSAL_READ_TIMEOUT,
        "the guard is a timeout, so it cannot have returned before the timeout elapsed"
    );
}

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

async fn seed_book_and_voice(pool: &SqlitePool) -> (String, String) {
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
    book::insert_book(pool, &book).await.expect("insert book");
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
        .expect("insert voice");
    (book.id, profile.id)
}

const SEEDED_PLAN: &str = r#"{"model_id":"text-llm","device":"cuda","n_gpu_layers":40}"#;

async fn insert_planned_job(pool: &SqlitePool, book_id: &str, voice_id: &str) -> String {
    let now = now_ms();
    let job_id = new_id();
    let record = job::RenderJob {
        id: job_id.clone(),
        book_id: book_id.to_string(),
        scope_json: serde_json::to_string(&Scope::Pages {
            page_start: 1,
            page_end: 10,
        })
        .unwrap(),
        mode: "narrator".into(),
        voice_profile_id: voice_id.to_string(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        resource_plan_json: Some(SEEDED_PLAN.to_string()),
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Paused.as_str().to_string(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now,
        updated_at: now,
    };
    job::insert_job(pool, &record).await.expect("insert job");
    job_id
}

fn contention_block(record: &job::RenderJob) -> serde_json::Value {
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap_or("{}")).unwrap_or_default();
    report
        .get(CONTENTION_KEY)
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

const WAIT_BUDGET: Duration = Duration::from_secs(60);

async fn wait_for_state(pool: &SqlitePool, job_id: &str, target: JobState) -> job::RenderJob {
    let deadline = Instant::now() + WAIT_BUDGET;
    loop {
        let record = job::get_job(pool, job_id)
            .await
            .expect("get job")
            .expect("job exists");
        if record.state == target.as_str() {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "job {job_id} never reached {} within {WAIT_BUDGET:?} (currently {})",
            target.as_str(),
            record.state
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_stage_entry(runs: &AtomicUsize) {
    let deadline = Instant::now() + WAIT_BUDGET;
    while runs.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "the stage never entered within {WAIT_BUDGET:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[derive(Default)]
struct Recorder {
    updates: Mutex<Vec<JobUpdate>>,
}

fn sinks(recorder: &Arc<Recorder>) -> (ProgressSink, UpdateSink) {
    let update_recorder = recorder.clone();
    (
        Arc::new(|_event: ProgressEvent| {}),
        Arc::new(move |update| {
            update_recorder.updates.lock().unwrap().push(update);
        }),
    )
}

struct BusyUntil {
    busy_runs: usize,
    runs: Arc<AtomicUsize>,
    entered: Arc<Mutex<Vec<Instant>>>,
}

impl Stage for BusyUntil {
    fn run<'a>(
        &'a self,
        _ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            self.entered.lock().unwrap().push(Instant::now());
            let run = self.runs.fetch_add(1, Ordering::SeqCst);
            if run < self.busy_runs {
                return Err(engine_busy());
            }
            Ok(StageOutcome::Completed)
        })
    }
}

const FAST_DELAY: Duration = Duration::from_millis(50);

fn fast_policy(max_retries: u32) -> BusyRetryPolicy {
    BusyRetryPolicy {
        first_delay: FAST_DELAY,
        max_delay: FAST_DELAY,
        total_budget: Duration::from_secs(60),
        max_retries,
    }
}

#[tokio::test]
async fn a_busy_engine_is_retried_on_the_same_plan_without_spending_the_recovery_ladder() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Scripting,
        Arc::new(BusyUntil {
            busy_runs: 2,
            runs: runs.clone(),
            entered: entered.clone(),
        }),
    );
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        StartOptions {
            consume_queue: true,
            busy_retry: fast_policy(8),
            vram_sampler: residency_check::disabled_sampler(),
        },
    );

    let job_id = insert_planned_job(&pool, &book_id, &voice_id).await;
    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;

    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert_eq!(
        record.resource_plan_json.as_deref(),
        Some(SEEDED_PLAN),
        "contention must re-run the same plan, not a degraded one"
    );

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        checkpoint.recovery_failures, 0,
        "a contention retry must not burn the recovery ladder's budget"
    );
    assert_eq!(
        checkpoint.resource_plan_rev, 0,
        "a contention retry must not revise the plan (report_json.plan_revisions reads this)"
    );
    assert!(
        checkpoint.recovery_log.is_empty(),
        "a contention retry is not a recovery episode: {:?}",
        checkpoint.recovery_log
    );

    let contention = contention_block(&record);
    assert_eq!(
        contention["episodes"], 1,
        "two refusals in one stage attempt run are one episode: {contention}"
    );
    assert_eq!(
        contention["retries"], 2,
        "a job that finished after waiting must not look identical to one that sailed \
         through; this is the only capacity signal a deployment has: {contention}"
    );
    assert_eq!(
        contention["by_stage"]["SCRIPTING"]["retries"], 2,
        "contention rate is only actionable per stage: {contention}"
    );
    assert!(
        contention["by_stage"].get("SYNTHESIZING").is_none(),
        "a stage that never contended must not appear: {contention}"
    );

    let seen = recorder.updates.lock().unwrap().clone();
    assert!(
        seen.iter().all(|update| update.state != JobState::Paused),
        "a contention retry keeps the job in its stage: {seen:?}"
    );
    assert!(
        seen.iter()
            .all(|update| update.notice.as_deref() != Some(RECOVERY_NOTICE)),
        "a contention retry must not claim the app reduced GPU usage: {seen:?}"
    );
    assert_eq!(
        seen.iter()
            .filter(|update| update.notice.as_deref() == Some(BUSY_NOTICE))
            .count(),
        2,
        "each wait tells the user why the stage is not progressing: {seen:?}"
    );
}

#[tokio::test]
async fn a_spent_contention_budget_pauses_the_job_instead_of_destroying_it() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Scripting,
        Arc::new(BusyUntil {
            busy_runs: usize::MAX,
            runs: runs.clone(),
            entered: entered.clone(),
        }),
    );
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        StartOptions {
            consume_queue: true,
            busy_retry: fast_policy(3),
            vram_sampler: residency_check::disabled_sampler(),
        },
    );

    let job_id = insert_planned_job(&pool, &book_id, &voice_id).await;
    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Paused).await;

    assert_eq!(
        runs.load(Ordering::SeqCst),
        4,
        "three retries after the first refusal, then a pause rather than a failure"
    );

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        checkpoint.stage,
        JobState::Scripting,
        "the pause is only better than FAILED if resuming re-enters the stage that gave up; \
         an unpinned checkpoint restarts the job at INGESTING and re-runs every sidecar"
    );
    assert_eq!(checkpoint.contention_pauses, 1);
    assert_eq!(
        checkpoint.recovery_failures, 0,
        "a contention pause is not a recovery episode"
    );
    assert_eq!(checkpoint.resource_plan_rev, 0);
    assert!(checkpoint.recovery_log.is_empty());
    assert_eq!(
        record.resource_plan_json.as_deref(),
        Some(SEEDED_PLAN),
        "a job that only ever hit contention must keep the plan it started on"
    );

    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    assert!(
        report.get("error").is_none(),
        "pausing is not failing, so nothing may write a failure into the report: {report}"
    );
    let paused = &report["contention"]["paused"];
    assert_eq!(
        paused["stage"], "SCRIPTING",
        "settle() writes a bare state, so without a persisted reason a self-pause is \
         indistinguishable from a user pause once the live event is gone: {report}"
    );
    assert_eq!(paused["resume_attempts"], 1, "{report}");
    assert_eq!(paused["retries"], 3, "{report}");
    assert!(paused["at"].as_i64().unwrap_or(0) > 0, "{report}");

    let seen = recorder.updates.lock().unwrap().clone();
    assert!(
        seen.iter().any(|update| update.state == JobState::Paused
            && update.notice.as_deref() == Some(BUSY_PAUSE_NOTICE)),
        "the pause has to explain itself the moment it happens: {seen:?}"
    );
}

async fn wait_for_contention_pause(pool: &SqlitePool, job_id: &str) -> job::RenderJob {
    let deadline = Instant::now() + WAIT_BUDGET;
    loop {
        let record = job::get_job(pool, job_id)
            .await
            .expect("get job")
            .expect("job exists");
        let paused_on_contention = record.state == JobState::Paused.as_str()
            && contention_block(&record)["paused"].is_object();
        if paused_on_contention {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "job {job_id} never paused on contention within {WAIT_BUDGET:?} (currently {})",
            record.state
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_runs_above(runs: &AtomicUsize, floor: usize) {
    let deadline = Instant::now() + WAIT_BUDGET;
    while runs.load(Ordering::SeqCst) <= floor {
        assert!(
            Instant::now() < deadline,
            "the stage was never re-entered above {floor} runs within {WAIT_BUDGET:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_permanently_busy_engine_degrades_to_a_terminal_failure_once_its_pauses_are_spent() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Scripting,
        Arc::new(BusyUntil {
            busy_runs: usize::MAX,
            runs: runs.clone(),
            entered: entered.clone(),
        }),
    );
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        StartOptions {
            consume_queue: true,
            busy_retry: fast_policy(3),
            vram_sampler: residency_check::disabled_sampler(),
        },
    );

    let job_id = insert_planned_job(&pool, &book_id, &voice_id).await;
    orchestrator.resume_on_launch().await.expect("resume");

    let mut retries_seen: Vec<u64> = Vec::new();
    for attempt in 1..=MAX_CONTENTION_PAUSES {
        let record = wait_for_state(&pool, &job_id, JobState::Paused).await;
        let checkpoint: Checkpoint =
            serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            checkpoint.contention_pauses, attempt,
            "each self-pause has to be counted, or a permanently held slot cycles forever"
        );
        assert_eq!(checkpoint.stage, JobState::Scripting);
        let contention = contention_block(&record);
        assert_eq!(contention["paused"]["resume_attempts"], attempt);
        retries_seen.push(contention["retries"].as_u64().unwrap_or_default());

        let before = runs.load(Ordering::SeqCst);
        orchestrator.resume_job(&job_id).await.expect("resume");
        wait_for_runs_above(&runs, before).await;
    }

    let record = wait_for_state(&pool, &job_id, JobState::Failed).await;
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    assert_eq!(report["failed_stage"], "SCRIPTING");
    let error = report["error"].as_str().unwrap();
    assert!(
        error.contains("engine stayed busy"),
        "a slot nobody ever releases still has to reach a terminal answer: {error}"
    );
    assert!(
        report["contention"]["paused"].is_null(),
        "the job is no longer paused; a stale pause record would tell the UI to offer a \
         resume on a terminal job: {report}"
    );

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.contention_pauses, MAX_CONTENTION_PAUSES);
    assert_eq!(checkpoint.resource_plan_rev, 0);
    assert!(checkpoint.recovery_log.is_empty());
    assert_eq!(record.resource_plan_json.as_deref(), Some(SEEDED_PLAN));

    let total = report["contention"]["retries"].as_u64().unwrap_or_default();
    assert!(
        total > *retries_seen.last().unwrap(),
        "every resumed attempt contends again, so the additive block must keep growing across \
         pauses rather than restarting: saw {retries_seen:?} then {total}"
    );
}

enum Scripted {
    Busy,
    Oom,
    Complete,
}

struct ScriptedStage {
    outcomes: Mutex<std::collections::VecDeque<Scripted>>,
    runs: Arc<AtomicUsize>,
}

impl Stage for ScriptedStage {
    fn run<'a>(
        &'a self,
        _ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            self.runs.fetch_add(1, Ordering::SeqCst);
            match self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Scripted::Complete)
            {
                Scripted::Busy => Err(engine_busy()),
                Scripted::Oom => Err(OrchestratorError::Engine {
                    status: None,
                    code: "ERR_OOM".into(),
                    message: "CUDA out of memory".into(),
                }),
                Scripted::Complete => Ok(StageOutcome::Completed),
            }
        })
    }
}

#[tokio::test]
async fn contention_that_cleared_and_came_back_gets_a_fresh_budget() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Scripting,
        Arc::new(ScriptedStage {
            outcomes: Mutex::new(std::collections::VecDeque::from(vec![
                Scripted::Busy,
                Scripted::Busy,
                Scripted::Busy,
                Scripted::Oom,
                Scripted::Busy,
                Scripted::Busy,
                Scripted::Busy,
                Scripted::Complete,
            ])),
            runs: runs.clone(),
        }),
    );
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        StartOptions {
            consume_queue: true,
            busy_retry: fast_policy(3),
            vram_sampler: residency_check::disabled_sampler(),
        },
    );

    let job_id = insert_planned_job(&pool, &book_id, &voice_id).await;
    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;

    assert_eq!(
        runs.load(Ordering::SeqCst),
        8,
        "three retries, then an OOM that clears the episode, then three more retries"
    );

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    let contention = contention_block(&record);
    assert_eq!(
        contention["episodes"], 2,
        "the OOM ended the first episode, so the six retries are two episodes and the second \
         must add to the first rather than replace it: {contention}"
    );
    assert_eq!(contention["retries"], 6, "{contention}");
    assert_eq!(
        contention["by_stage"]["SCRIPTING"]["episodes"], 2,
        "both episodes were at the same stage, which is exactly the case a last-writer-wins \
         block would hide: {contention}"
    );

    assert_eq!(
        checkpoint.resource_plan_rev, 1,
        "only the OOM belongs to the ladder; six contention retries must revise nothing"
    );
    assert_eq!(checkpoint.recovery_log.len(), 1);
    assert_eq!(checkpoint.recovery_log[0].stage, JobState::Scripting);
}

#[tokio::test]
async fn a_job_that_degraded_then_paused_carries_the_degradation_in_its_report() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Scripting,
        Arc::new(ScriptedStage {
            outcomes: Mutex::new(std::collections::VecDeque::from(vec![
                Scripted::Oom,
                Scripted::Busy,
                Scripted::Busy,
                Scripted::Busy,
                Scripted::Busy,
            ])),
            runs: runs.clone(),
        }),
    );
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        StartOptions {
            consume_queue: true,
            busy_retry: fast_policy(3),
            vram_sampler: residency_check::disabled_sampler(),
        },
    );

    let job_id = insert_planned_job(&pool, &book_id, &voice_id).await;
    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_contention_pause(&pool, &job_id).await;

    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        report["plan_revisions"], 1,
        "the job degraded and then stopped before rendering; render.rs was the only writer of \
         this key, so a job that never reaches it had no degradation to show at all: {report}"
    );
    let log = report["recovery_log"]
        .as_array()
        .unwrap_or_else(|| panic!("the recovery log has to reach the report too: {report}"));
    assert_eq!(log.len(), 1, "{report}");
    assert_eq!(log[0]["stage"], "SCRIPTING", "{report}");
    assert!(
        log[0]["action"].as_str().is_some_and(|a| !a.is_empty()),
        "{report}"
    );
    assert!(
        report["contention"]["paused"].is_object(),
        "the pause and the degradation are both execution facts and both have to survive on \
         the same paused job: {report}"
    );
}

#[tokio::test]
async fn a_stage_that_completes_gives_the_next_stage_a_full_pause_budget() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let scripting_runs = Arc::new(AtomicUsize::new(0));
    let synthesis_runs = Arc::new(AtomicUsize::new(0));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Scripting,
        Arc::new(ScriptedStage {
            outcomes: Mutex::new(std::collections::VecDeque::from(vec![
                Scripted::Busy,
                Scripted::Busy,
                Scripted::Busy,
                Scripted::Busy,
                Scripted::Complete,
            ])),
            runs: scripting_runs.clone(),
        }),
    );
    registry.register(
        JobState::Synthesizing,
        Arc::new(BusyUntil {
            busy_runs: usize::MAX,
            runs: synthesis_runs.clone(),
            entered: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        StartOptions {
            consume_queue: true,
            busy_retry: fast_policy(3),
            vram_sampler: residency_check::disabled_sampler(),
        },
    );

    let job_id = insert_planned_job(&pool, &book_id, &voice_id).await;
    orchestrator.resume_on_launch().await.expect("resume");

    let first = wait_for_state(&pool, &job_id, JobState::Paused).await;
    let checkpoint: Checkpoint =
        serde_json::from_str(first.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.stage, JobState::Scripting);
    assert_eq!(checkpoint.contention_pauses, 1);

    let before = synthesis_runs.load(Ordering::SeqCst);
    orchestrator.resume_job(&job_id).await.expect("resume");
    wait_for_runs_above(&synthesis_runs, before).await;

    let second = wait_for_state(&pool, &job_id, JobState::Paused).await;
    let checkpoint: Checkpoint =
        serde_json::from_str(second.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.stage, JobState::Synthesizing);
    assert_eq!(
        checkpoint.contention_pauses, 1,
        "the budget is three pauses per stuck stage, not three per job for its whole life; \
         without the reset a job that already survived contention in scripting arrives at \
         synthesis with part of its budget already spent"
    );
    assert_eq!(
        second.report_json.as_deref().map(|raw| {
            let value: serde_json::Value = serde_json::from_str(raw).unwrap();
            value["contention"]["paused"]["stage"].clone()
        }),
        Some(serde_json::json!("SYNTHESIZING")),
        "the persisted reason has to name the stage that is actually stuck"
    );
}

const OBSERVABLE_DELAY: Duration = Duration::from_secs(8);

#[tokio::test]
async fn a_retry_waits_out_its_delay_instead_of_hammering_a_contended_engine() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Scripting,
        Arc::new(BusyUntil {
            busy_runs: usize::MAX,
            runs: runs.clone(),
            entered: entered.clone(),
        }),
    );
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        StartOptions {
            consume_queue: true,
            busy_retry: BusyRetryPolicy {
                first_delay: OBSERVABLE_DELAY,
                max_delay: OBSERVABLE_DELAY,
                total_budget: Duration::from_secs(60),
                max_retries: 1,
            },
            vram_sampler: residency_check::disabled_sampler(),
        },
    );

    let job_id = insert_planned_job(&pool, &book_id, &voice_id).await;
    orchestrator.resume_on_launch().await.expect("resume");
    wait_for_stage_entry(&runs).await;
    let first_entry = entered.lock().unwrap()[0];
    wait_for_state(&pool, &job_id, JobState::Paused).await;

    assert_eq!(runs.load(Ordering::SeqCst), 2);
    let entries = entered.lock().unwrap().clone();
    let gap = entries[1].duration_since(entries[0]);
    assert!(
        gap >= OBSERVABLE_DELAY,
        "the only thing that can hold a re-entry back for {OBSERVABLE_DELAY:?} is the backoff \
         delay; stage entry itself costs about a second of hardware probing: {gap:?}"
    );
    assert!(
        first_entry.elapsed() >= OBSERVABLE_DELAY,
        "the job must not give up before the delay it promised to wait has elapsed"
    );

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("get job")
        .expect("job exists");
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    let paused = &report["contention"]["paused"];
    let waited_s = paused["waited_s"]
        .as_u64()
        .unwrap_or_else(|| panic!("the persisted pause must report how long it waited: {report}"));
    assert!(
        (OBSERVABLE_DELAY.as_secs()..OBSERVABLE_DELAY.as_secs() + 12).contains(&waited_s),
        "waited_s is measured, so the one {OBSERVABLE_DELAY:?} delay is its floor and stage \
         re-entry is the only thing that can add to it; a number below the floor means the \
         budget went back to accounting scheduled sleeps: {waited_s}s in {report}"
    );
}

#[tokio::test]
async fn pausing_during_a_contention_wait_does_not_wait_the_delay_out() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Scripting,
        Arc::new(BusyUntil {
            busy_runs: usize::MAX,
            runs: runs.clone(),
            entered: entered.clone(),
        }),
    );
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    const DELAY_NEVER_ELAPSES: Duration = Duration::from_secs(600);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        StartOptions {
            consume_queue: true,
            busy_retry: BusyRetryPolicy {
                first_delay: DELAY_NEVER_ELAPSES,
                max_delay: DELAY_NEVER_ELAPSES,
                total_budget: Duration::from_secs(3600),
                max_retries: 12,
            },
            vram_sampler: residency_check::disabled_sampler(),
        },
    );

    let job_id = insert_planned_job(&pool, &book_id, &voice_id).await;
    orchestrator.resume_on_launch().await.expect("resume");
    wait_for_stage_entry(&runs).await;
    let announced_by = Instant::now() + WAIT_BUDGET;
    loop {
        let announced = recorder
            .updates
            .lock()
            .unwrap()
            .iter()
            .any(|update| update.notice.as_deref() == Some(BUSY_NOTICE));
        if announced {
            break;
        }
        assert!(
            Instant::now() < announced_by,
            "the worker never announced the contention wait the pause is meant to interrupt"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let paused_at = Instant::now();
    orchestrator.pause_job(&job_id).await.expect("pause");
    wait_for_state(&pool, &job_id, JobState::Paused).await;
    let elapsed = paused_at.elapsed();

    assert!(
        elapsed < WAIT_BUDGET,
        "a pause must interrupt the contention wait, not sit through it; with a \
         {DELAY_NEVER_ELAPSES:?} retry delay only an interrupted wait settles this side of \
         the budget: {elapsed:?}"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the stage must not be re-entered after the pause"
    );
}
