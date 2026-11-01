use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use txtcaster_lib::orchestrator::{
    residency_check, Checkpoint, JobOrchestrator, JobState, JobUpdate, OrchestratorError,
    ProgressEvent, ProgressSink, Scope, Stage, StageContext, StageOutcome, StageRegistry,
    StartOptions, UpdateSink, RECOVERY_NOTICE,
};

fn test_options() -> StartOptions {
    StartOptions {
        vram_sampler: residency_check::disabled_sampler(),
        ..StartOptions::default()
    }
}

use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice, SqlitePool};

fn engine_oom() -> OrchestratorError {
    OrchestratorError::Engine {
        status: None,
        code: "ERR_OOM".into(),
        message: "CUDA out of memory".into(),
    }
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

async fn insert_planned_job(
    pool: &SqlitePool,
    book_id: &str,
    voice_id: &str,
    plan: &serde_json::Value,
    checkpoint: Option<&Checkpoint>,
) -> String {
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
        resource_plan_json: Some(plan.to_string()),
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Paused.as_str().to_string(),
        checkpoint_json: checkpoint.map(|c| serde_json::to_string(c).unwrap()),
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

const WAIT_BUDGET: Duration = Duration::from_secs(60);

async fn wait_for_state(pool: &SqlitePool, job_id: &str, target: JobState) -> job::RenderJob {
    let deadline = Instant::now() + WAIT_BUDGET;
    while Instant::now() < deadline {
        let record = job::get_job(pool, job_id)
            .await
            .expect("get job")
            .expect("job exists");
        if record.state == target.as_str() {
            return record;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {job_id} never reached {}", target.as_str());
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

fn plan_device_name(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return "cpu".into();
    };
    let plan: serde_json::Value = serde_json::from_str(raw).expect("a stored plan is json");
    txtcaster_lib::hardware::device::plan_device(&plan)
        .expect("a stored plan names a readable device")
        .legacy_string()
}

fn current_device(record: &job::RenderJob) -> String {
    plan_device_name(record.resource_plan_json.as_deref())
}

struct OomUntilDegraded {
    units: i64,
    fail_at_unit: i64,
    executed: Arc<Mutex<Vec<i64>>>,
}

impl Stage for OomUntilDegraded {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let record = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            let workers = record
                .resource_plan_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|plan| plan.get("tts_workers").and_then(serde_json::Value::as_i64))
                .unwrap_or(1);
            while ctx.checkpoint.done_segments < self.units {
                let unit = ctx.checkpoint.done_segments;
                if workers > 2 && unit >= self.fail_at_unit {
                    return Err(engine_oom());
                }
                self.executed.lock().unwrap().push(unit);
                ctx.checkpoint.done_segments = unit + 1;
                ctx.checkpoint.total_segments = self.units;
                ctx.save_checkpoint().await?;
            }
            Ok(StageOutcome::Completed)
        })
    }
}

struct OomWhileCuda {
    attempts: Arc<Mutex<Vec<String>>>,
}

impl Stage for OomWhileCuda {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let record = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            let device = current_device(&record);
            self.attempts.lock().unwrap().push(device.clone());
            if device.starts_with("cuda") {
                return Err(engine_oom());
            }
            Ok(StageOutcome::Completed)
        })
    }
}

struct FailsUnlessCpu {
    attempts: Arc<Mutex<Vec<String>>>,
}

impl Stage for FailsUnlessCpu {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let record = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            let raw = record.resource_plan_json.as_deref().unwrap_or("{}");
            let plan: serde_json::Value = serde_json::from_str(raw).expect("plan json");
            match txtcaster_lib::hardware::device::plan_device(&plan) {
                Err(error) => {
                    self.attempts.lock().unwrap().push("unreadable".into());
                    Err(OrchestratorError::Engine {
                        status: None,
                        code: "ERR_OOM".into(),
                        message: error.to_string(),
                    })
                }
                Ok(device) if device.is_gpu() => {
                    self.attempts.lock().unwrap().push(device.legacy_string());
                    Err(engine_oom())
                }
                Ok(device) => {
                    self.attempts.lock().unwrap().push(device.legacy_string());
                    Ok(StageOutcome::Completed)
                }
            }
        })
    }
}

struct VisionOomWhileCuda {
    attempts: Arc<Mutex<Vec<String>>>,
}

impl Stage for VisionOomWhileCuda {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let record = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            let device = plan_device_name(record.vision_plan_json.as_deref());
            self.attempts.lock().unwrap().push(device.clone());
            if device.starts_with("cuda") {
                return Err(engine_oom());
            }
            Ok(StageOutcome::Completed)
        })
    }
}

struct AlwaysOom;

impl Stage for AlwaysOom {
    fn run<'a>(
        &'a self,
        _ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move { Err(engine_oom()) })
    }
}

const MISSING_CUBLAS: &str =
    "alignment failed: Library libcublas.so.12 is not found or cannot be loaded";

fn engine_device_fault() -> OrchestratorError {
    OrchestratorError::Engine {
        status: None,
        code: "ERR_DEVICE_UNAVAILABLE".into(),
        message: MISSING_CUBLAS.into(),
    }
}

struct AlignFaultsWhileCuda {
    attempts: Arc<Mutex<Vec<String>>>,
}

impl Stage for AlignFaultsWhileCuda {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let record = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            let device = plan_device_name(record.align_plan_json.as_deref());
            self.attempts.lock().unwrap().push(device.clone());
            if device.starts_with("cuda") {
                return Err(engine_device_fault());
            }
            Ok(StageOutcome::Completed)
        })
    }
}

struct TtsOomWhileCuda {
    attempts: Arc<Mutex<Vec<String>>>,
}

impl Stage for TtsOomWhileCuda {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let record = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            let device = plan_device_name(record.tts_plan_json.as_deref());
            self.attempts.lock().unwrap().push(device.clone());
            if device.starts_with("cuda") {
                return Err(engine_oom());
            }
            Ok(StageOutcome::Completed)
        })
    }
}

struct AlwaysDeviceFault;

impl Stage for AlwaysDeviceFault {
    fn run<'a>(
        &'a self,
        _ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move { Err(engine_device_fault()) })
    }
}

struct AlwaysInvalid;

impl Stage for AlwaysInvalid {
    fn run<'a>(
        &'a self,
        _ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move { Err(OrchestratorError::InvalidRequest("bad input".into())) })
    }
}

#[tokio::test]
async fn vision_oom_degrades_vision_plan_without_touching_scripting_plan() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Vision,
        Arc::new(VisionOomWhileCuda {
            attempts: attempts.clone(),
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
        test_options(),
    );

    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"model_id": "text-llm", "device": "cuda", "n_gpu_layers": 40}),
        None,
    )
    .await;
    let vision_plan = serde_json::json!({"model_id": "mllm", "device": "cuda", "n_gpu_layers": 29});
    job::set_job_vision_plan(&pool, &job_id, &vision_plan.to_string())
        .await
        .expect("seed vision plan");

    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;

    assert_eq!(
        attempts.lock().unwrap().clone(),
        vec!["cuda", "cuda", "cpu"]
    );

    let vision: serde_json::Value =
        serde_json::from_str(record.vision_plan_json.as_deref().unwrap()).unwrap();
    assert_eq!(plan_device_name(record.vision_plan_json.as_deref()), "cpu");
    assert_eq!(vision["n_gpu_layers"], 0);

    let scripting: serde_json::Value =
        serde_json::from_str(record.resource_plan_json.as_deref().unwrap()).unwrap();
    assert_eq!(current_device(&record), "cuda");
    assert_eq!(scripting["n_gpu_layers"], 40);

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.recovery_log.len(), 2);
    assert!(checkpoint
        .recovery_log
        .iter()
        .all(|entry| entry.stage == JobState::Vision));
}

#[tokio::test]
async fn oom_mid_stage_degrades_plan_and_completes_without_losing_segments() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(OomUntilDegraded {
            units: 10,
            fail_at_unit: 5,
            executed: executed.clone(),
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
        test_options(),
    );

    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"device": "cuda", "tts_workers": 4, "batch": 512}),
        None,
    )
    .await;
    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert!(checkpoint.resource_plan_rev >= 1);
    assert_eq!(checkpoint.recovery_log.len(), 1);
    assert_eq!(checkpoint.recovery_log[0].stage, JobState::Synthesizing);

    let plan: serde_json::Value =
        serde_json::from_str(record.resource_plan_json.as_deref().unwrap()).unwrap();
    assert_eq!(plan["tts_workers"], 2);
    assert_eq!(plan["batch"], 256);

    let mut units = executed.lock().unwrap().clone();
    units.sort_unstable();
    assert_eq!(units, (0..10).collect::<Vec<i64>>());

    let notices: Vec<JobUpdate> = recorder
        .updates
        .lock()
        .unwrap()
        .iter()
        .filter(|u| u.notice.is_some())
        .cloned()
        .collect();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].state, JobState::Paused);
    assert_eq!(notices[0].notice.as_deref(), Some(RECOVERY_NOTICE));
}

#[tokio::test]
async fn second_consecutive_oom_falls_back_to_cpu() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(OomWhileCuda {
            attempts: attempts.clone(),
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
        test_options(),
    );

    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"device": "cuda", "tts_workers": 4, "batch": 512}),
        None,
    )
    .await;
    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;

    assert_eq!(
        attempts.lock().unwrap().clone(),
        vec!["cuda".to_string(), "cuda".to_string(), "cpu".to_string()]
    );
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.resource_plan_rev, 2);
    assert_eq!(checkpoint.recovery_log.len(), 2);
    assert_eq!(current_device(&record), "cpu");
}

#[tokio::test]
async fn a_plan_whose_device_is_corrupt_recovers_onto_the_cpu() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(FailsUnlessCpu {
            attempts: attempts.clone(),
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
        test_options(),
    );

    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"device": "banana", "tts_workers": 4, "batch": 512}),
        None,
    )
    .await;
    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;

    assert_eq!(
        current_device(&record),
        "cpu",
        "a plan column we cannot trust costs a rung of the ladder, not the whole job"
    );
    assert_eq!(
        attempts.lock().unwrap().clone(),
        vec!["unreadable".to_string(), "cpu".to_string()],
        "one attempt on the corrupt plan, then the cpu rung"
    );
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.recovery_log.len(), 1);
    assert_eq!(checkpoint.recovery_log[0].action, "fallback to cpu");
}

#[tokio::test]
async fn oom_on_cpu_plan_fails_the_job() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let mut registry = StageRegistry::default();
    registry.register(JobState::Synthesizing, Arc::new(AlwaysOom));
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );

    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"device": "cpu"}),
        None,
    )
    .await;
    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Failed).await;

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.resource_plan_rev, 0);
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    assert_eq!(report["failed_stage"], "SYNTHESIZING");
    assert!(report["error"].as_str().unwrap().contains("ERR_OOM"));
}

#[tokio::test]
async fn non_retryable_error_fails_without_recovery() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let mut registry = StageRegistry::default();
    registry.register(JobState::Synthesizing, Arc::new(AlwaysInvalid));
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );

    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"device": "cuda", "tts_workers": 4}),
        None,
    )
    .await;
    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Failed).await;

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.resource_plan_rev, 0);
    assert!(checkpoint.recovery_log.is_empty());
    assert_eq!(current_device(&record), "cuda");
    assert!(recorder
        .updates
        .lock()
        .unwrap()
        .iter()
        .all(|u| u.notice.is_none()));
}

#[tokio::test]
async fn recovery_counters_survive_app_relaunch() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;

    let now = now_ms();
    let mut checkpoint = Checkpoint::new(JobState::Synthesizing, now);
    checkpoint.recovery_failures = 1;
    checkpoint.resource_plan_rev = 1;
    checkpoint
        .recovery_log
        .push(txtcaster_lib::orchestrator::RecoveryEntry {
            stage: JobState::Synthesizing,
            rev: 1,
            action: "tts batch/workers halved".into(),
            at: now,
        });
    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"device": "cuda", "tts_workers": 2, "batch": 256}),
        Some(&checkpoint),
    )
    .await;

    let attempts = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(OomWhileCuda {
            attempts: attempts.clone(),
        }),
    );
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let relaunched = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );
    let resumed = relaunched.resume_on_launch().await.expect("resume");
    assert_eq!(resumed, vec![job_id.clone()]);
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;

    assert_eq!(
        attempts.lock().unwrap().clone(),
        vec!["cuda".to_string(), "cpu".to_string()]
    );
    assert_eq!(current_device(&record), "cpu");
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.resource_plan_rev, 2);
    assert_eq!(checkpoint.recovery_log.len(), 2);
    assert_eq!(checkpoint.recovery_log[1].action, "fallback to cpu");
}

#[tokio::test]
async fn a_missing_cuda_library_at_aligning_takes_the_cpu_rung_instead_of_the_oom_ladder() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Aligning,
        Arc::new(AlignFaultsWhileCuda {
            attempts: attempts.clone(),
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
        test_options(),
    );

    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"model_id": "text-llm", "device": "cuda", "n_gpu_layers": 40}),
        None,
    )
    .await;
    let align_plan = serde_json::json!({
        "model_id": "whisperx-small-int8",
        "device": "cuda",
        "align_workers": 4,
    });
    job::set_job_align_plan(&pool, &job_id, &align_plan.to_string())
        .await
        .expect("seed align plan");

    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;

    assert_eq!(
        attempts.lock().unwrap().clone(),
        vec!["cuda".to_string(), "cpu".to_string()],
        "a library that will not load is not memory pressure: the ladder's gpu rungs cannot \
         fix it, so the first recovery has to be the cpu one"
    );

    let align: serde_json::Value =
        serde_json::from_str(record.align_plan_json.as_deref().unwrap()).unwrap();
    assert_eq!(plan_device_name(record.align_plan_json.as_deref()), "cpu");
    assert_eq!(
        align["align_workers"], 4,
        "halving the workers is the memory rung; taking it here would spend a retry on a \
         fault no reduction can fix"
    );
    assert_eq!(
        align["model_id"], "whisperx-small-int8",
        "the degraded rung has to stay the plan the stage recognises, or the stage re-plans \
         itself back onto the gpu it just failed on"
    );

    assert_eq!(
        current_device(&record),
        "cuda",
        "the fault was in the aligning stage; nothing else has to leave the gpu"
    );

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.recovery_log.len(), 1);
    assert_eq!(checkpoint.recovery_log[0].stage, JobState::Aligning);
    assert_eq!(checkpoint.recovery_log[0].action, "fallback to cpu");
}

#[tokio::test]
async fn a_device_fault_the_cpu_cannot_fix_fails_with_its_own_words_not_out_of_memory() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let mut registry = StageRegistry::default();
    registry.register(JobState::Aligning, Arc::new(AlwaysDeviceFault));
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );

    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"model_id": "text-llm", "device": "cpu"}),
        None,
    )
    .await;
    let align_plan = serde_json::json!({
        "model_id": "whisperx-small-int8",
        "device": "cpu",
        "align_workers": 1,
    });
    job::set_job_align_plan(&pool, &job_id, &align_plan.to_string())
        .await
        .expect("seed align plan");

    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Failed).await;

    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().expect("a failure report")).unwrap();
    let error = report["error"].as_str().expect("an error line");
    assert!(
        error.contains("libcublas.so.12"),
        "the machine had memory to spare; naming the library is the difference between an \
         actionable failure and a false out-of-memory: {error}"
    );
    assert!(
        !error.contains("out of memory"),
        "reporting a loader failure as out of memory is what sent this job down the ladder \
         in the first place: {error}"
    );

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert!(
        checkpoint.recovery_log.is_empty(),
        "there is no rung below the cpu, so nothing should have been spent trying"
    );
}

#[tokio::test]
async fn an_oom_at_a_stage_that_owns_a_plan_degrades_that_plan_and_not_the_scripting_one() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(TtsOomWhileCuda {
            attempts: attempts.clone(),
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
        test_options(),
    );

    let job_id = insert_planned_job(
        &pool,
        &book_id,
        &voice_id,
        &serde_json::json!({"model_id": "text-llm", "device": "cuda", "n_gpu_layers": 40}),
        None,
    )
    .await;
    let tts_plan = serde_json::json!({
        "model_id": "kokoro-82m",
        "device": "cuda",
        "tts_workers": 4,
        "batch": 512,
    });
    job::set_job_tts_plan(&pool, &job_id, &tts_plan.to_string())
        .await
        .expect("seed tts plan");

    orchestrator.resume_on_launch().await.expect("resume");
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;

    assert_eq!(
        attempts.lock().unwrap().clone(),
        vec!["cuda".to_string(), "cuda".to_string(), "cpu".to_string()],
        "the ladder is right for memory pressure; what was wrong was writing its rungs to a \
         column the stage never reads, so every retry re-read the same gpu plan"
    );
    assert_eq!(plan_device_name(record.tts_plan_json.as_deref()), "cpu");
    assert_eq!(
        current_device(&record),
        "cuda",
        "the scripting plan did not fail and must keep its device"
    );
}
