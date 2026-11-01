use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use tokio::sync::{oneshot, Semaphore};
use txtcaster_lib::orchestrator::{
    residency_check, Checkpoint, CreateJobRequest, JobOrchestrator, JobState, JobUpdate,
    OrchestratorError, PausedReason, ProgressEvent, ProgressSink, RateLimitClass, Scope, Stage,
    StageContext, StageOutcome, StageRegistry, StartOptions, UpdateSink,
};

fn test_options() -> StartOptions {
    StartOptions {
        vram_sampler: residency_check::disabled_sampler(),
        ..StartOptions::default()
    }
}

use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice, SqlitePool};

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

fn request(book_id: &str, voice_id: &str) -> CreateJobRequest {
    CreateJobRequest {
        book_id: book_id.to_string(),
        scope: Scope::Pages {
            page_start: 1,
            page_end: 10,
        },
        mode: "narrator".into(),
        voice_profile_id: voice_id.to_string(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        overnight: false,
        verify: false,
    }
}

#[derive(Default)]
struct Recorder {
    updates: Mutex<Vec<JobUpdate>>,
    progress: Mutex<Vec<ProgressEvent>>,
}

fn sinks(recorder: &Arc<Recorder>) -> (ProgressSink, UpdateSink) {
    let progress_recorder = recorder.clone();
    let update_recorder = recorder.clone();
    (
        Arc::new(move |event| {
            progress_recorder.progress.lock().unwrap().push(event);
        }),
        Arc::new(move |update| {
            update_recorder.updates.lock().unwrap().push(update);
        }),
    )
}

const WAIT_BUDGET: Duration = Duration::from_secs(60);

async fn wait_for_update(recorder: &Arc<Recorder>, target: JobState) {
    let deadline = Instant::now() + WAIT_BUDGET;
    while Instant::now() < deadline {
        if recorder
            .updates
            .lock()
            .unwrap()
            .iter()
            .any(|update| update.state == target)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("update sink never reported {}", target.as_str());
}

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

struct PauseGate {
    at_unit: i64,
    reached: Mutex<Option<oneshot::Sender<()>>>,
    release: Semaphore,
}

impl PauseGate {
    fn parked_at(at_unit: i64) -> (Arc<Self>, oneshot::Receiver<()>) {
        let (reached_tx, reached_rx) = oneshot::channel();
        (
            Arc::new(PauseGate {
                at_unit,
                reached: Mutex::new(Some(reached_tx)),
                release: Semaphore::new(0),
            }),
            reached_rx,
        )
    }
}

struct CountingStage {
    units: i64,
    unit_delay: Duration,
    executed: Arc<Mutex<Vec<i64>>>,
    pause_gate: Option<Arc<PauseGate>>,
}

impl Stage for CountingStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, txtcaster_lib::orchestrator::OrchestratorError>> {
        Box::pin(async move {
            while ctx.checkpoint.done_segments < self.units {
                if let Some(gate) = &self.pause_gate {
                    if ctx.checkpoint.done_segments == gate.at_unit {
                        let parked = gate.reached.lock().unwrap().take();
                        if let Some(reached) = parked {
                            let _ = reached.send(());
                            gate.release
                                .acquire()
                                .await
                                .expect("pause gate release")
                                .forget();
                        }
                    }
                }
                if let Some(outcome) = ctx.control.interruption() {
                    return Ok(outcome);
                }
                tokio::time::sleep(self.unit_delay).await;
                let unit = ctx.checkpoint.done_segments;
                self.executed.lock().unwrap().push(unit);
                ctx.checkpoint.done_segments = unit + 1;
                ctx.checkpoint.total_segments = self.units;
                ctx.save_checkpoint().await?;
                ctx.emit_progress(
                    ctx.checkpoint.done_segments as f64 / self.units as f64,
                    None,
                    &format!("segment {}/{}", ctx.checkpoint.done_segments, self.units),
                );
            }
            Ok(StageOutcome::Completed)
        })
    }
}

struct HangingStage;

impl Stage for HangingStage {
    fn run<'a>(
        &'a self,
        _ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, txtcaster_lib::orchestrator::OrchestratorError>> {
        Box::pin(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        })
    }
}

#[tokio::test]
async fn happy_path_walks_all_stages_to_done() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        StageRegistry::default(),
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    let record = wait_for_state(&pool, &job_id, JobState::Done).await;
    wait_for_update(&recorder, JobState::Done).await;

    let states: Vec<JobState> = recorder
        .updates
        .lock()
        .unwrap()
        .iter()
        .map(|u| u.state)
        .collect();
    assert_eq!(
        states,
        vec![
            JobState::Ingesting,
            JobState::Layout,
            JobState::Vision,
            JobState::Scripting,
            JobState::Synthesizing,
            JobState::Aligning,
            JobState::Rendering,
            JobState::Done,
        ]
    );
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().expect("checkpoint")).unwrap();
    assert_eq!(checkpoint.stage, JobState::Rendering);
}

#[tokio::test]
async fn create_job_stamps_a_seed_that_survives_re_rendering_the_same_book() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        StageRegistry::default(),
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );

    let first = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    let second = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    assert_ne!(first, second);

    let load = |id: String| {
        let pool = pool.clone();
        async move {
            job::get_job(&pool, &id)
                .await
                .expect("get job")
                .expect("job")
                .tts_seed
        }
    };
    let book = book::get_book(&pool, &book_id)
        .await
        .expect("get book")
        .expect("book");
    let expected = Some(job::default_tts_seed(&voice_id, &book.content_hash));
    assert_eq!(load(first).await, expected);
    assert_eq!(
        load(second).await,
        expected,
        "a second render of the same book and voice must reuse the seed, otherwise every \
         cached segment is invalidated"
    );
}

#[tokio::test]
async fn create_job_validates_inputs() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        StageRegistry::default(),
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );

    let mut bad_book = request("missing-book", &voice_id);
    bad_book.scope = Scope::Pages {
        page_start: 1,
        page_end: 2,
    };
    assert!(orchestrator.create_job(bad_book).await.is_err());

    let bad_voice = request(&book_id, "missing-voice");
    assert!(orchestrator.create_job(bad_voice).await.is_err());

    let mut bad_mode = request(&book_id, &voice_id);
    bad_mode.mode = "podcast".into();
    assert!(orchestrator.create_job(bad_mode).await.is_err());

    let mut bad_scope = request(&book_id, &voice_id);
    bad_scope.scope = Scope::Pages {
        page_start: 9,
        page_end: 3,
    };
    assert!(orchestrator.create_job(bad_scope).await.is_err());
}

#[tokio::test]
async fn pause_then_resume_continues_from_checkpoint() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let executed = Arc::new(Mutex::new(Vec::new()));
    let (gate, reached) = PauseGate::parked_at(3);
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(CountingStage {
            units: 40,
            unit_delay: Duration::from_millis(10),
            executed: executed.clone(),
            pause_gate: Some(gate.clone()),
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

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    tokio::time::timeout(WAIT_BUDGET, reached)
        .await
        .expect("stage never reached the pause gate")
        .expect("pause gate sender dropped");
    orchestrator.pause_job(&job_id).await.expect("pause");
    gate.release.add_permits(1);
    let record = wait_for_state(&pool, &job_id, JobState::Paused).await;

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    let done_at_pause = checkpoint.done_segments;
    assert_eq!(done_at_pause, 3);

    orchestrator.resume_job(&job_id).await.expect("resume");
    wait_for_state(&pool, &job_id, JobState::Done).await;

    let units = executed.lock().unwrap().clone();
    assert_eq!(units.len(), 40, "no unit may be redone: {units:?}");
    let mut sorted = units.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 40);
}

fn slow_sampler(delay: Duration) -> residency_check::VramSampler {
    Arc::new(move || {
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            None
        })
    })
}

#[tokio::test]
async fn pause_point_stays_exact_when_vram_sampling_is_slow() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let executed = Arc::new(Mutex::new(Vec::new()));
    let (gate, reached) = PauseGate::parked_at(3);
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(CountingStage {
            units: 40,
            unit_delay: Duration::from_millis(10),
            executed: executed.clone(),
            pause_gate: Some(gate.clone()),
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
            vram_sampler: slow_sampler(Duration::from_millis(400)),
            ..StartOptions::default()
        },
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    tokio::time::timeout(WAIT_BUDGET, reached)
        .await
        .expect("stage never reached the pause gate")
        .expect("pause gate sender dropped");
    orchestrator.pause_job(&job_id).await.expect("pause");
    gate.release.add_permits(1);
    let paused_at = Instant::now();
    let record = wait_for_state(&pool, &job_id, JobState::Paused).await;
    let pause_latency = paused_at.elapsed();

    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        checkpoint.done_segments, 3,
        "a slow vram sampler must not move the pause point"
    );
    assert!(
        pause_latency < Duration::from_secs(10),
        "pause settle must not scale with sampler latency: {pause_latency:?}"
    );

    orchestrator.resume_job(&job_id).await.expect("resume");
    wait_for_state(&pool, &job_id, JobState::Done).await;

    let units = executed.lock().unwrap().clone();
    assert_eq!(units.len(), 40, "no unit may be redone: {units:?}");
    let mut sorted = units.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 40);
}

#[tokio::test]
async fn cancel_stops_job_and_rejects_resume() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(CountingStage {
            units: 200,
            unit_delay: Duration::from_millis(10),
            executed: executed.clone(),
            pause_gate: None,
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

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    wait_for_state(&pool, &job_id, JobState::Synthesizing).await;
    orchestrator.cancel_job(&job_id).await.expect("cancel");
    wait_for_state(&pool, &job_id, JobState::Cancelled).await;

    let count_after_cancel = executed.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(executed.lock().unwrap().len(), count_after_cancel);
    assert!(orchestrator.resume_job(&job_id).await.is_err());
    assert!(orchestrator.cancel_job(&job_id).await.is_err());
}

#[tokio::test]
async fn progress_events_flow_during_stage() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(CountingStage {
            units: 5,
            unit_delay: Duration::from_millis(1),
            executed,
            pause_gate: None,
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

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    wait_for_state(&pool, &job_id, JobState::Done).await;

    let events = recorder.progress.lock().unwrap().clone();
    let synth: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| e.stage == JobState::Synthesizing)
        .collect();
    assert_eq!(
        synth.len(),
        6,
        "one stage-entry event plus five unit events"
    );
    assert!(
        synth[0].pct.abs() < f64::EPSILON,
        "the stage must publish 0% on entry so the ui never shows the previous stage"
    );
    assert_eq!(synth[0].current_item, JobState::Synthesizing.start_label());
    assert!((synth.last().unwrap().pct - 1.0).abs() < f64::EPSILON);
    assert_eq!(synth.last().unwrap().current_item, "segment 5/5");
}

#[tokio::test]
async fn kill_and_relaunch_resumes_from_checkpoint() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(CountingStage {
            units: 1000,
            unit_delay: Duration::from_millis(5),
            executed: executed.clone(),
            pause_gate: None,
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
    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    wait_for_state(&pool, &job_id, JobState::Synthesizing).await;
    let deadline = Instant::now() + WAIT_BUDGET;
    while executed.lock().unwrap().len() < 3 {
        if Instant::now() >= deadline {
            panic!("stage never completed three units before the kill");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(orchestrator);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let done_before = executed.lock().unwrap().len();
    assert!(
        done_before > 0,
        "some units must have completed before kill"
    );

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(record.state, JobState::Synthesizing.as_str());
    let checkpoint: Checkpoint =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert!(checkpoint.done_segments > 0);

    let executed_after = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(CountingStage {
            units: 1000,
            unit_delay: Duration::from_millis(0),
            executed: executed_after.clone(),
            pause_gate: None,
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
    wait_for_state(&pool, &job_id, JobState::Done).await;

    let redone: Vec<i64> = executed_after
        .lock()
        .unwrap()
        .iter()
        .filter(|unit| **unit < checkpoint.done_segments)
        .copied()
        .collect();
    assert!(redone.is_empty(), "checkpointed units redone: {redone:?}");
}

#[tokio::test]
async fn shutdown_pauses_active_job_without_recovery_and_leaves_queued_job_queued() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let mut registry = StageRegistry::default();
    registry.register(JobState::Synthesizing, Arc::new(HangingStage));
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(30),
        _dir.path().join("cache"),
        test_options(),
    );

    let active_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create active job");
    wait_for_state(&pool, &active_id, JobState::Synthesizing).await;
    let queued_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create queued job");

    orchestrator
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
    assert!(orchestrator.shutdown_started());

    let active = job::get_job(&pool, &active_id).await.unwrap().unwrap();
    assert_eq!(active.state, JobState::Paused.as_str());
    let checkpoint: Checkpoint =
        serde_json::from_str(active.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint.resource_plan_rev, 0);
    assert!(checkpoint.recovery_log.is_empty());

    let queued = job::get_job(&pool, &queued_id).await.unwrap().unwrap();
    assert_eq!(queued.state, JobState::Queued.as_str());
}

#[tokio::test]
async fn shutdown_is_idempotent_and_rejects_new_jobs() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool,
        StageRegistry::default(),
        progress,
        updates,
        Duration::from_secs(30),
        _dir.path().join("cache"),
        test_options(),
    );

    orchestrator
        .shutdown(Duration::from_secs(1))
        .await
        .expect("first shutdown");
    orchestrator
        .shutdown(Duration::from_secs(1))
        .await
        .expect("second shutdown");
    let error = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect_err("job creation must stop after shutdown");
    assert!(error.to_string().contains("shutting down"));
}

#[tokio::test]
async fn pause_after_completion_does_not_clobber_done() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        StageRegistry::default(),
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    wait_for_state(&pool, &job_id, JobState::Done).await;

    assert!(orchestrator.pause_job(&job_id).await.is_err());
    assert!(orchestrator.cancel_job(&job_id).await.is_err());
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(record.state, JobState::Done.as_str());
}

#[tokio::test]
async fn new_jobs_are_persisted_as_queued() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        StageRegistry::default(),
        progress,
        updates,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );

    let executed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Ingesting,
        Arc::new(CountingStage {
            units: 1,
            unit_delay: Duration::from_millis(200),
            executed,
            pause_gate: None,
        }),
    );
    drop(orchestrator);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        sinks(&recorder).0,
        sinks(&recorder).1,
        Duration::from_secs(5),
        _dir.path().join("cache"),
        test_options(),
    );
    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert!(
        record.state == JobState::Queued.as_str() || record.state == JobState::Ingesting.as_str(),
        "unexpected initial state {}",
        record.state
    );
    wait_for_state(&pool, &job_id, JobState::Done).await;
}

#[tokio::test]
async fn default_voice_profile_seed_is_idempotent() {
    let (_dir, pool) = test_pool().await;
    txtcaster_lib::orchestrator::seed_default_voice_profile(&pool)
        .await
        .expect("first seed");
    txtcaster_lib::orchestrator::seed_default_voice_profile(&pool)
        .await
        .expect("second seed");
    let profiles = voice::list_voice_profiles(&pool).await.expect("list");
    assert_eq!(profiles.len(), 1);
    let profile = &profiles[0];
    assert_eq!(
        profile.id,
        txtcaster_lib::orchestrator::DEFAULT_VOICE_PROFILE_ID
    );
    assert_eq!(profile.engine, "kokoro-82m");
    assert_eq!(profile.kind, "preset");
    assert_eq!(profile.preset_voice_id.as_deref(), Some("af_heart"));
}

#[tokio::test]
async fn hung_stage_is_cancelled_after_grace_period() {
    let (_dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let mut registry = StageRegistry::default();
    registry.register(JobState::Scripting, Arc::new(HangingStage));
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_millis(100),
        _dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");
    wait_for_state(&pool, &job_id, JobState::Scripting).await;
    orchestrator.cancel_job(&job_id).await.expect("cancel");
    wait_for_state(&pool, &job_id, JobState::Cancelled).await;
}

// F11-U6 C9/C10 (docs/notes/f11-u6-mechanism.md §0): the queue is the
// persisted `render_job` rows. A relaunch re-enqueues QUEUED casts in
// creation order — the order the Jobs panel shows as queue position — and a
// cast cancelled while queued is settled and never re-enqueued.
fn paused_rig(pool: &SqlitePool, dir: &tempfile::TempDir) -> JobOrchestrator {
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    JobOrchestrator::start_with_options(
        pool.clone(),
        StageRegistry::default(),
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        StartOptions {
            consume_queue: false,
            ..test_options()
        },
    )
}

async fn set_created_at(pool: &SqlitePool, job_id: &str, created_at: i64) {
    sqlx::query("UPDATE render_job SET created_at = ? WHERE id = ?")
        .bind(created_at)
        .bind(job_id)
        .execute(pool)
        .await
        .expect("set created_at");
}

#[tokio::test]
async fn f11_u6_relaunch_restores_the_queue_in_creation_order() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let first = paused_rig(&pool, &dir);
    let a = first
        .create_job(request(&book_id, &voice_id))
        .await
        .unwrap();
    let b = first
        .create_job(request(&book_id, &voice_id))
        .await
        .unwrap();
    let c = first
        .create_job(request(&book_id, &voice_id))
        .await
        .unwrap();
    // Creation stamps can tie within a millisecond; make the order explicit
    // and different from insertion order so the pin reads the column.
    set_created_at(&pool, &a, 3_000).await;
    set_created_at(&pool, &b, 1_000).await;
    set_created_at(&pool, &c, 2_000).await;
    drop(first);

    let relaunched = paused_rig(&pool, &dir);
    let resumed = relaunched.resume_on_launch().await.expect("resume");
    assert_eq!(resumed, vec![b.clone(), c.clone(), a.clone()]);
    for id in [&a, &b, &c] {
        let record = job::get_job(&pool, id).await.unwrap().unwrap();
        assert_eq!(record.state, JobState::Queued.as_str(), "{id}");
    }
}

#[tokio::test]
async fn f11_u6_a_queued_cast_can_be_cancelled_and_is_not_restored() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let rig = paused_rig(&pool, &dir);
    let keep = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    let gone = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    set_created_at(&pool, &keep, 1_000).await;
    set_created_at(&pool, &gone, 2_000).await;
    rig.cancel_job(&gone).await.expect("cancel a queued cast");
    let record = job::get_job(&pool, &gone).await.unwrap().unwrap();
    assert_eq!(record.state, JobState::Cancelled.as_str());
    assert!(
        rig.cancel_job(&gone).await.is_err(),
        "cancel is not repeatable"
    );
    drop(rig);

    let relaunched = paused_rig(&pool, &dir);
    let resumed = relaunched.resume_on_launch().await.expect("resume");
    assert_eq!(resumed, vec![keep]);
}

/// F11-U6 queue-order key: equal `created_at` stamps are broken by `id`, the
/// key the Jobs panel derives its positions from — otherwise two casts created
/// in the same millisecond could display in an order the relaunch does not
/// restore. Inserted b-then-a so the old `ORDER BY created_at` (insertion
/// order on ties) fails this pin.
#[tokio::test]
async fn f11_u6_relaunch_breaks_created_at_ties_by_id() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let rig = paused_rig(&pool, &dir);
    let first = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    let second = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    // Rename so the later-inserted row has the smaller id.
    sqlx::query("UPDATE render_job SET id = 'tie-b', created_at = 5000 WHERE id = ?")
        .bind(&first)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE render_job SET id = 'tie-a', created_at = 5000 WHERE id = ?")
        .bind(&second)
        .execute(&pool)
        .await
        .unwrap();
    drop(rig);
    let relaunched = paused_rig(&pool, &dir);
    let resumed = relaunched.resume_on_launch().await.expect("resume");
    assert_eq!(resumed, vec!["tie-a".to_string(), "tie-b".to_string()]);
}

// C17 (V1-CLOSEOUT row C17): dispatch follows the queue key the Jobs panel and
// the relaunch already share — `render_job ORDER BY created_at, id` — not the
// order ids reached the worker channel. A stage that parks every cast until
// released and records the order casts started makes dispatch observable.
struct GatedStage {
    started: Mutex<Vec<String>>,
    release: Semaphore,
}

impl GatedStage {
    fn new() -> Arc<Self> {
        Arc::new(GatedStage {
            started: Mutex::new(Vec::new()),
            release: Semaphore::new(0),
        })
    }
    fn started(&self) -> Vec<String> {
        self.started.lock().unwrap().clone()
    }
    async fn wait_started(&self, count: usize) {
        let deadline = Instant::now() + WAIT_BUDGET;
        while self.started.lock().unwrap().len() < count {
            assert!(
                Instant::now() < deadline,
                "started so far: {:?}",
                self.started()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Stage for GatedStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, txtcaster_lib::orchestrator::OrchestratorError>> {
        Box::pin(async move {
            self.started.lock().unwrap().push(ctx.job_id.clone());
            self.release.acquire().await.expect("gate release").forget();
            Ok(StageOutcome::Completed)
        })
    }
}

fn gated_rig(pool: &SqlitePool, dir: &tempfile::TempDir, gate: Arc<GatedStage>) -> JobOrchestrator {
    let mut registry = StageRegistry::default();
    registry.register(JobState::Synthesizing, gate);
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);
    JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(30),
        dir.path().join("cache"),
        test_options(),
    )
}

/// C17 pin: A runs; B and C wait; B is paused while its stale channel token is
/// consumed (skipped), then resumed while C runs; D is created after C started.
/// The panel and the relaunch both place B before D (`created_at`), so the
/// worker must run B before D. At 05c76217 B re-enters at the channel tail and
/// runs last: started == [A, C, D, B].
#[tokio::test]
async fn c17_resumed_cast_keeps_its_creation_slot_in_dispatch() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let gate = GatedStage::new();
    let rig = gated_rig(&pool, &dir, gate.clone());
    let a = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    gate.wait_started(1).await;
    let b = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    let c = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    for (id, stamp) in [(&a, 1_000), (&b, 2_000), (&c, 3_000)] {
        set_created_at(&pool, id, stamp).await;
    }
    rig.pause_job(&b).await.expect("pause a queued cast");
    wait_for_state(&pool, &b, JobState::Paused).await;
    gate.release.add_permits(1); // A finishes; B's stale token is skipped; C starts
    gate.wait_started(2).await;
    assert_eq!(gate.started(), vec![a.clone(), c.clone()]);
    let d = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    set_created_at(&pool, &d, 4_000).await;
    rig.resume_job(&b).await.expect("resume");
    // What the panel shows (same SQL as `list_jobs`): B ahead of D.
    let queued = job::list_jobs_in_states(&pool, &[JobState::Queued.as_str()])
        .await
        .unwrap();
    let queued_ids: Vec<String> = queued.into_iter().map(|row| row.id).collect();
    assert_eq!(queued_ids, vec![b.clone(), d.clone()]);
    gate.release.add_permits(1); // C finishes; the worker picks the next cast
    gate.wait_started(3).await;
    assert_eq!(
        gate.started(),
        vec![a.clone(), c.clone(), b.clone()],
        "the resumed cast must run in its creation slot, before D"
    );
    gate.release.add_permits(2);
    wait_for_state(&pool, &d, JobState::Done).await;
    assert_eq!(gate.started(), vec![a, c, b, d]);
}

/// C17 guard (owner constraint a): a cast that is RUNNING is never pre-empted
/// when an older QUEUED row appears — the key only decides who runs *next*.
#[tokio::test]
async fn c17_a_running_cast_is_not_preempted_by_an_older_queued_row() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let gate = GatedStage::new();
    let rig = gated_rig(&pool, &dir, gate.clone());
    let running = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    set_created_at(&pool, &running, 5_000).await;
    gate.wait_started(1).await;
    let older = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    set_created_at(&pool, &older, 1_000).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let record = job::get_job(&pool, &running).await.unwrap().unwrap();
    assert_eq!(record.state, JobState::Synthesizing.as_str());
    let waiting = job::get_job(&pool, &older).await.unwrap().unwrap();
    assert_eq!(waiting.state, JobState::Queued.as_str());
    assert_eq!(gate.started(), vec![running.clone()]);
    gate.release.add_permits(2);
    wait_for_state(&pool, &older, JobState::Done).await;
    assert_eq!(gate.started(), vec![running, older]);
}

/// C17 guard: the slot a resumed cast keeps is the persisted one, so a restart
/// restores B before C even though B was the last id sent before shutdown.
#[tokio::test]
async fn c17_resumed_cast_slot_survives_a_relaunch() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let rig = paused_rig(&pool, &dir);
    let b = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    let c = rig.create_job(request(&book_id, &voice_id)).await.unwrap();
    set_created_at(&pool, &b, 1_000).await;
    set_created_at(&pool, &c, 2_000).await;
    rig.pause_job(&b).await.expect("pause");
    rig.resume_job(&b).await.expect("resume");
    drop(rig);
    let relaunched = paused_rig(&pool, &dir);
    let resumed = relaunched
        .resume_on_launch()
        .await
        .expect("resume on launch");
    assert_eq!(resumed, vec![b, c]);
}

struct AutoResumeStage {
    attempts: Arc<std::sync::atomic::AtomicU32>,
}

impl Stage for AutoResumeStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let attempt = self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if attempt == 0 {
                let retry_at = now_ms() + 100;
                let reason = PausedReason {
                    kind: "rate_limited".into(),
                    class: RateLimitClass::PerMinute,
                    retry_at_epoch_ms: Some(retry_at),
                    attempt: 1,
                    provider: "google".into(),
                    segment_idx: 0,
                    last_error_message: "Quota exceeded: requests per minute (RPM)".into(),
                };
                ctx.checkpoint.paused_reason = Some(reason);
                ctx.save_checkpoint().await?;
                return Ok(StageOutcome::Paused);
            }
            Ok(StageOutcome::Completed)
        })
    }
}

#[tokio::test]
async fn rate_limit_per_minute_exhaustion_pauses_and_auto_resumes_to_completion() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);

    let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(AutoResumeStage {
            attempts: attempts.clone(),
        }),
    );

    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");

    let paused_record = wait_for_state(&pool, &job_id, JobState::Paused).await;
    let cp: Checkpoint =
        serde_json::from_str(paused_record.checkpoint_json.as_deref().unwrap()).unwrap();
    let reason = cp.paused_reason.expect("paused reason present");
    assert_eq!(reason.class, RateLimitClass::PerMinute);
    assert!(reason.retry_at_epoch_ms.is_some());

    let done_record = wait_for_state(&pool, &job_id, JobState::Done).await;
    assert_eq!(done_record.state, JobState::Done.as_str());
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
}

struct StaleReasonStage {
    attempts: Arc<std::sync::atomic::AtomicU32>,
}

impl Stage for StaleReasonStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let attempt = self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if attempt == 0 {
                let reason = PausedReason {
                    kind: "rate_limited".into(),
                    class: RateLimitClass::PerMinute,
                    retry_at_epoch_ms: Some(now_ms() + 100),
                    attempt: 1,
                    provider: "google".into(),
                    segment_idx: 0,
                    last_error_message: "Quota exceeded: requests per minute (RPM)".into(),
                };
                ctx.checkpoint.paused_reason = Some(reason);
                ctx.save_checkpoint().await?;
                return Ok(StageOutcome::Paused);
            }
            if attempt == 1 {
                return Ok(StageOutcome::Paused);
            }
            Ok(StageOutcome::Completed)
        })
    }
}

#[tokio::test]
async fn a_user_pause_after_an_auto_resume_is_not_defeated_by_the_stale_paused_reason() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);

    let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(StaleReasonStage {
            attempts: attempts.clone(),
        }),
    );

    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");

    wait_for_state(&pool, &job_id, JobState::Paused).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while attempts.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second stage run");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job read")
        .expect("job row present");
    assert_eq!(
        record.state,
        JobState::Paused.as_str(),
        "the user's pause must stick instead of being auto-resumed by the stale reason"
    );
    let cp: Checkpoint = serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert!(
        cp.paused_reason.is_none(),
        "the auto-resume flip must clear the reason it consumed"
    );
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
}

struct OneShotPerMinutePauseStage;

impl Stage for OneShotPerMinutePauseStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let reason = PausedReason {
                kind: "rate_limited".into(),
                class: RateLimitClass::PerMinute,
                retry_at_epoch_ms: Some(now_ms() + 2000),
                attempt: 1,
                provider: "google".into(),
                segment_idx: 0,
                last_error_message: "Quota exceeded: requests per minute (RPM)".into(),
            };
            ctx.checkpoint.paused_reason = Some(reason);
            ctx.save_checkpoint().await?;
            Ok(StageOutcome::Paused)
        })
    }
}

#[tokio::test]
async fn an_auto_resume_whose_queue_receiver_is_dead_reverts_the_job_to_paused() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);

    let mut registry = StageRegistry::default();
    registry.register(JobState::Synthesizing, Arc::new(OneShotPerMinutePauseStage));

    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");

    wait_for_state(&pool, &job_id, JobState::Paused).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(orchestrator);
    tokio::time::sleep(Duration::from_millis(2600)).await;

    let record = job::get_job(&pool, &job_id)
        .await
        .expect("job read")
        .expect("job row present");
    assert_eq!(
        record.state,
        JobState::Paused.as_str(),
        "a failed queue send must revert the auto-resume flip to paused; a job left queued has \
          no worker consuming the queue and no timer left to save it"
    );
}

struct DailyLimitStage;

impl Stage for DailyLimitStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let reason = PausedReason {
                kind: "rate_limited".into(),
                class: RateLimitClass::Daily,
                retry_at_epoch_ms: None,
                attempt: 1,
                provider: "google".into(),
                segment_idx: 0,
                last_error_message: "Quota exceeded: requests per day (RPD) limit exceeded".into(),
            };
            ctx.checkpoint.paused_reason = Some(reason);
            ctx.save_checkpoint().await?;
            Ok(StageOutcome::Paused)
        })
    }
}

#[tokio::test]
async fn rate_limit_daily_exhaustion_pauses_without_auto_resume_and_preserves_on_relaunch() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);

    let mut registry = StageRegistry::default();
    registry.register(JobState::Synthesizing, Arc::new(DailyLimitStage));

    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");

    let paused_record = wait_for_state(&pool, &job_id, JobState::Paused).await;
    let cp: Checkpoint =
        serde_json::from_str(paused_record.checkpoint_json.as_deref().unwrap()).unwrap();
    let reason = cp.paused_reason.expect("paused reason present");
    assert_eq!(reason.class, RateLimitClass::Daily);
    assert!(reason.retry_at_epoch_ms.is_none());

    tokio::time::sleep(Duration::from_millis(250)).await;
    let still_paused = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(still_paused.state, JobState::Paused.as_str());

    drop(orchestrator);
    let relaunched = paused_rig(&pool, &dir);
    let resumed = relaunched
        .resume_on_launch()
        .await
        .expect("resume on launch");
    assert!(!resumed.contains(&job_id));

    let after_relaunch = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(after_relaunch.state, JobState::Paused.as_str());
}

struct ThrottleCapStage {
    runs: Arc<std::sync::atomic::AtomicU32>,
}

impl Stage for ThrottleCapStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let run = self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as i64;
            let done_now = run.min(3);
            match txtcaster_lib::orchestrator::scripting_cloud::throttle_pause(
                &mut ctx.checkpoint,
                done_now,
                txtcaster_lib::orchestrator::scripting_cloud::MAX_RATE_LIMIT_PAUSE_ATTEMPTS,
            ) {
                txtcaster_lib::orchestrator::scripting_cloud::ThrottlePause::Exhausted => {
                    Err(OrchestratorError::Engine {
                        status: None,
                        code: "ERR_PROVIDER".into(),
                        message: "provider stayed rate-limited; pausing exhausted".into(),
                    })
                }
                txtcaster_lib::orchestrator::scripting_cloud::ThrottlePause::Pause(attempt) => {
                    let reason = PausedReason {
                        kind: "rate_limited".into(),
                        class: RateLimitClass::PerMinute,
                        retry_at_epoch_ms: Some(now_ms() + 100),
                        attempt,
                        provider: "google".into(),
                        segment_idx: done_now,
                        last_error_message: "Quota exceeded: requests per minute (RPM)".into(),
                    };
                    ctx.checkpoint.paused_reason = Some(reason);
                    ctx.save_checkpoint().await?;
                    Ok(StageOutcome::Paused)
                }
            }
        })
    }
}

#[tokio::test]
async fn pauses_without_provider_progress_exhaust_the_throttle_cap_and_fail_the_job() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);

    let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(ThrottleCapStage { runs: runs.clone() }),
    );

    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");

    let failed_record = wait_for_state(&pool, &job_id, JobState::Failed).await;
    assert_eq!(failed_record.state, JobState::Failed.as_str());
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::SeqCst),
        9,
        "three progress-separated pauses each re-arm the budget, then six decisions at the \
          same done count must refuse the sixth: 3 + 5 pauses + 1 refusal"
    );
}

type ThrottleEntryProbe = Arc<Mutex<Option<(u32, i64)>>>;

struct AdvanceThrottlePauseStage {
    runs: Option<Arc<std::sync::atomic::AtomicU32>>,
    entry_probe: Option<ThrottleEntryProbe>,
}

impl AdvanceThrottlePauseStage {
    fn pausing_then_completing() -> Arc<Self> {
        Arc::new(AdvanceThrottlePauseStage {
            runs: Some(Arc::new(std::sync::atomic::AtomicU32::new(0))),
            entry_probe: None,
        })
    }

    fn boundary_probe(entry_probe: ThrottleEntryProbe) -> Arc<Self> {
        Arc::new(AdvanceThrottlePauseStage {
            runs: None,
            entry_probe: Some(entry_probe),
        })
    }
}

impl Stage for AdvanceThrottlePauseStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            if let Some(runs) = &self.runs {
                let run = runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if run > 0 {
                    return Ok(StageOutcome::Completed);
                }
            }
            if let Some(entry_probe) = &self.entry_probe {
                entry_probe.lock().unwrap().get_or_insert((
                    ctx.checkpoint.rate_limit_pauses,
                    ctx.checkpoint.last_pause_done,
                ));
            }
            let done_now = 7;
            match txtcaster_lib::orchestrator::scripting_cloud::throttle_pause(
                &mut ctx.checkpoint,
                done_now,
                txtcaster_lib::orchestrator::scripting_cloud::MAX_RATE_LIMIT_PAUSE_ATTEMPTS,
            ) {
                txtcaster_lib::orchestrator::scripting_cloud::ThrottlePause::Exhausted => {
                    Err(OrchestratorError::Engine {
                        status: None,
                        code: "ERR_PROVIDER".into(),
                        message: "provider stayed rate-limited; pausing exhausted".into(),
                    })
                }
                txtcaster_lib::orchestrator::scripting_cloud::ThrottlePause::Pause(attempt) => {
                    let (class, retry_at_epoch_ms) = if self.entry_probe.is_some() {
                        (RateLimitClass::Daily, None)
                    } else {
                        (RateLimitClass::PerMinute, Some(now_ms() + 100))
                    };
                    let reason = PausedReason {
                        kind: "rate_limited".into(),
                        class,
                        retry_at_epoch_ms,
                        attempt,
                        provider: "google".into(),
                        segment_idx: done_now,
                        last_error_message: "Quota exceeded: requests per minute (RPM)".into(),
                    };
                    ctx.checkpoint.paused_reason = Some(reason);
                    ctx.save_checkpoint().await?;
                    Ok(StageOutcome::Paused)
                }
            }
        })
    }
}

#[tokio::test]
async fn a_stage_advance_re_arms_the_throttle_cap_even_without_new_progress() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);

    let entry_probe = Arc::new(Mutex::new(None));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Scripting,
        AdvanceThrottlePauseStage::pausing_then_completing(),
    );
    registry.register(
        JobState::Synthesizing,
        AdvanceThrottlePauseStage::boundary_probe(entry_probe.clone()),
    );

    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");

    let first_pause = wait_for_state(&pool, &job_id, JobState::Paused).await;
    let cp: Checkpoint =
        serde_json::from_str(first_pause.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(cp.rate_limit_pauses, 1);
    assert_eq!(cp.last_pause_done, 7);
    assert_eq!(cp.paused_reason.expect("paused reason").attempt, 1);

    let deadline = Instant::now() + WAIT_BUDGET;
    let entry = loop {
        if let Some(entry) = *entry_probe.lock().unwrap() {
            break entry;
        }
        assert!(
            Instant::now() < deadline,
            "the synthesizing stage never ran after the advance"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let second_pause = wait_for_state(&pool, &job_id, JobState::Paused).await;
    let cp: Checkpoint =
        serde_json::from_str(second_pause.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        cp.paused_reason.expect("paused reason").attempt,
        1,
        "both pauses passed the same done count, so only the stage-advance reset can \
          re-arm the budget here"
    );
    assert_eq!(entry, (0, 0));
    assert_eq!(cp.rate_limit_pauses, 1);
    assert_eq!(cp.last_pause_done, 7);
}

struct ManualResumeRearmStage {
    runs: Arc<std::sync::atomic::AtomicU32>,
    entry: Arc<Mutex<Option<(u32, i64)>>>,
}

impl Stage for ManualResumeRearmStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let run = self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if run > 0 {
                self.entry.lock().unwrap().get_or_insert((
                    ctx.checkpoint.rate_limit_pauses,
                    ctx.checkpoint.last_pause_done,
                ));
                return Ok(StageOutcome::Completed);
            }
            let done_now = 4;
            match txtcaster_lib::orchestrator::scripting_cloud::throttle_pause(
                &mut ctx.checkpoint,
                done_now,
                txtcaster_lib::orchestrator::scripting_cloud::MAX_RATE_LIMIT_PAUSE_ATTEMPTS,
            ) {
                txtcaster_lib::orchestrator::scripting_cloud::ThrottlePause::Exhausted => {
                    Err(OrchestratorError::Engine {
                        status: None,
                        code: "ERR_PROVIDER".into(),
                        message: "provider stayed rate-limited; pausing exhausted".into(),
                    })
                }
                txtcaster_lib::orchestrator::scripting_cloud::ThrottlePause::Pause(attempt) => {
                    let reason = PausedReason {
                        kind: "rate_limited".into(),
                        class: RateLimitClass::Daily,
                        retry_at_epoch_ms: None,
                        attempt,
                        provider: "google".into(),
                        segment_idx: done_now,
                        last_error_message: "Quota exceeded: requests per day (RPD)".into(),
                    };
                    ctx.checkpoint.paused_reason = Some(reason);
                    ctx.save_checkpoint().await?;
                    Ok(StageOutcome::Paused)
                }
            }
        })
    }
}

#[tokio::test]
async fn a_manual_resume_re_arms_the_throttle_cap() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);

    let entry = Arc::new(Mutex::new(None));
    let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Synthesizing,
        Arc::new(ManualResumeRearmStage {
            runs: runs.clone(),
            entry: entry.clone(),
        }),
    );

    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");

    let paused_record = wait_for_state(&pool, &job_id, JobState::Paused).await;
    let cp: Checkpoint =
        serde_json::from_str(paused_record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(cp.rate_limit_pauses, 1);
    assert_eq!(cp.last_pause_done, 4);
    assert_eq!(cp.paused_reason.expect("paused reason").attempt, 1);

    orchestrator.resume_job(&job_id).await.expect("resume");

    wait_for_state(&pool, &job_id, JobState::Done).await;
    assert_eq!(
        *entry.lock().unwrap(),
        Some((0, 0)),
        "a deliberate human resume re-arms the cap; the budget bounds unattended cycles only"
    );
}

struct HardFailingStage;

impl Stage for HardFailingStage {
    fn run<'a>(
        &'a self,
        _ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            Err(OrchestratorError::Engine {
                status: Some(500),
                code: "ERR_PROVIDER".into(),
                message: "Google TTS error (HTTP 500): Internal server error on segment 2".into(),
            })
        })
    }
}

#[tokio::test]
async fn hard_provider_failure_surfaces_segment_error_in_job_report() {
    let (dir, pool) = test_pool().await;
    let (book_id, voice_id) = seed_book_and_voice(&pool).await;
    let recorder = Arc::new(Recorder::default());
    let (progress, updates) = sinks(&recorder);

    let mut registry = StageRegistry::default();
    registry.register(JobState::Synthesizing, Arc::new(HardFailingStage));

    let orchestrator = JobOrchestrator::start_with_options(
        pool.clone(),
        registry,
        progress,
        updates,
        Duration::from_secs(5),
        dir.path().join("cache"),
        test_options(),
    );

    let job_id = orchestrator
        .create_job(request(&book_id, &voice_id))
        .await
        .expect("create job");

    let failed_record = wait_for_state(&pool, &job_id, JobState::Failed).await;
    assert_eq!(failed_record.state, JobState::Failed.as_str());

    let report: serde_json::Value =
        serde_json::from_str(failed_record.report_json.as_deref().unwrap()).unwrap();
    let err_str = report["error"].as_str().unwrap();
    assert!(err_str.contains("ERR_PROVIDER"));
    assert!(err_str.contains("Internal server error on segment 2"));
    assert_eq!(report["failed_stage"], JobState::Synthesizing.as_str());
}

fn char_floor(source: &str, index: usize) -> usize {
    let mut index = index.min(source.len());
    while !source.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn char_ceil(source: &str, index: usize) -> usize {
    let mut index = index.min(source.len());
    while !source.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[test]
fn the_rate_limited_script_pause_escalates_and_caps_its_attempts() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestrator/scripting_cloud.rs"
    ))
    .expect("scripting cloud source");
    assert!(
        source.contains("MAX_RATE_LIMIT_PAUSE_ATTEMPTS: u32"),
        "the rate-limited pause needs its own bounded attempt cap"
    );
    let start = source
        .find("PAUSED_KIND_RATE_LIMITED.to_string()")
        .expect("the rate-limited pause arm");
    let arm = &source[char_floor(&source, start.saturating_sub(2400))
        ..char_ceil(&source, source.len().min(start + 1200))];
    assert!(
        arm.contains("throttle_pause("),
        "the cap must be decided by the progress-derived pause budget, never by the \
          persisted pause display the auto-resume clears"
    );
    assert!(
        arm.contains("auto_resume_class"),
        "the pause must classify its wait against the auto-resume ceiling instead of \
          clamping it"
    );
    assert!(
        !arm.contains("paused_reason"),
        "post-fix arms never read the pause display; it is cleared before every \
          auto-resume, so sourcing the attempt from it resets the cap each cycle and \
          makes it unreachable"
    );
    assert!(
        !arm.contains(".min(300)"),
        "a clamp would retry before the provider's recovery window and spend the \
          no-progress budget on a wait that would have succeeded"
    );
    assert!(
        arm.contains("attempt,"),
        "the persisted reason must carry the escalated attempt, not a hardcoded 1"
    );
    assert!(
        !arm.contains("attempt: 1"),
        "a hardcoded attempt resets the cap every cycle and makes it unreachable"
    );
}

fn worker_method<'a>(source: &'a str, signature: &'a str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("worker declares {signature}"));
    let tail = &source[start..];
    let mut end = tail.len();
    for boundary in ["\n    fn ", "\n    pub "] {
        if let Some(offset) = tail[signature.len()..].find(boundary) {
            end = end.min(offset + signature.len());
        }
    }
    &tail[..end]
}

#[test]
fn human_resume_and_recover_re_arm_the_throttle_cap_but_automatic_paths_do_not() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestrator/worker.rs"
    ))
    .expect("worker source");
    assert_eq!(
        source.matches("reset_throttle_progress").count(),
        1,
        "exactly one in-memory re-arm is allowed: the manual resume (failure recovery re-arms \
          inside its conditional UPDATE)"
    );
    let resume = worker_method(&source, "pub async fn resume_job");
    assert!(
        resume.contains("reset_throttle_progress()"),
        "a deliberate human resume must re-arm the cap: a post-Exhausted retry must never \
          insta-fail on the budget the previous run spent"
    );
    let recover = worker_method(&source, "pub async fn recover_failed");
    assert!(
        recover.contains("recover_failed_job_to_local(") && recover.contains("reset_throttle,"),
        "recovering a failed cast must re-arm the cap in the same write that re-queues it"
    );
    let relaunch = worker_method(&source, "pub async fn resume_on_launch");
    assert!(
        !relaunch.contains("reset_throttle_progress"),
        "an automatic relaunch must not re-arm the cap; the budget bounds unattended \
          cycles only"
    );
}

#[test]
fn progress_clears_the_persisted_quota_pause_reason() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestrator/synthesis.rs"
    ))
    .expect("synthesis source");
    let start = source
        .find("SegmentRun::Done(outcome)")
        .expect("the segment-done arm");
    let arm = &source[start..char_ceil(&source, source.len().min(start + 700))];
    assert!(
        arm.contains("strip_report_paused_reason"),
        "segment progress must also clear the report's paused_reason block; read_prior_attempt \
          falls back to it, so a job-lifetime counter builds and a long book dies at \
          ERR_PROVIDER_QUOTA after five progress-separated pauses"
    );
}

#[test]
fn script_completion_strips_the_pause_display_at_both_exits() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestrator/scripting.rs"
    ))
    .expect("scripting source");
    let cached_start = source
        .find("if !existing.is_empty()")
        .expect("the cached short-circuit");
    let cached_end = source
        .find("script cached")
        .expect("the cached short-circuit progress label");
    let cached = &source[cached_start..cached_end];
    assert!(
        cached.contains("strip_report_paused_reason"),
        "the cached short-circuit completes the stage with no progress events; the persisted \
          pause display must be stripped there or a resumed job shows a pause it left behind"
    );
    let completed_start = source
        .find("CloudScriptRun::Completed(output)")
        .expect("the cloud completed arm");
    let completed =
        &source[completed_start..char_ceil(&source, source.len().min(completed_start + 900))];
    assert!(
        completed.contains("strip_report_paused_reason"),
        "a cloud script run that completes after a pause must clear the persisted pause \
          display; read_prior_attempt falls back to it and escalates future pauses"
    );
}

#[test]
fn the_auto_resume_timer_matches_the_queue_send_and_reverts_the_flip_on_failure() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestrator/worker.rs"
    ))
    .expect("worker source");
    let start = source
        .find("fn schedule_auto_resume")
        .expect("the auto-resume timer declaration");
    let end = source
        .find("async fn clear_checkpoint_paused_reason")
        .expect("the checkpoint clear helper declaration");
    let timer = &source[start..end];
    assert_eq!(
        timer.matches("update_job_state_if").count(),
        2,
        "the timer must both flip the job paused->queued and revert it queued->paused when \
          the queue send fails"
    );
    assert_eq!(
        timer.matches("JobState::Paused.as_str()").count(),
        2,
        "one transition must land on paused and one must leave it"
    );
    assert_eq!(
        timer.matches("JobState::Queued.as_str()").count(),
        2,
        "one transition must land on queued and one must leave it"
    );
    let flip_from = timer
        .find("JobState::Paused.as_str()")
        .expect("the flip's from-state");
    let flip_to = timer
        .find("JobState::Queued.as_str()")
        .expect("the flip's to-state");
    assert!(
        flip_from < flip_to,
        "the auto-resume flip must transition paused -> queued"
    );
    let revert_from = timer
        .rfind("JobState::Queued.as_str()")
        .expect("the revert's from-state");
    let revert_to = timer
        .rfind("JobState::Paused.as_str()")
        .expect("the revert's to-state");
    assert!(
        revert_from < revert_to,
        "the send-failure revert must transition queued -> paused"
    );
    assert!(
        timer.contains("match shared.queue_tx.send"),
        "the send result must be matched so a dead receiver triggers the revert"
    );
    assert!(
        !timer.contains("let _ = shared.queue_tx.send"),
        "a dropped send leaves the job queued with no consumer and no timer to save it"
    );
}
