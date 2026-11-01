//! Production-path recovery pins (Hermes round-2 verdict blocker on F7):
//! these drive `JobOrchestrator::recover_failed` — the exact function the
//! `retry_failed_job` IPC command calls — not a storage helper beside it.

use std::sync::Arc;
use std::time::Duration;

use txtcaster_lib::orchestrator::{
    JobOrchestrator, JobState, OrchestratorError, Scope, Stage, StageContext, StageOutcome,
    StageRegistry, STAGE_ORDER,
};
use txtcaster_lib::storage::{self, book, job, new_id, now_ms, voice, SqlitePool};

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    (dir, pool)
}

/// Paused-worker rig: enqueues succeed but nothing runs, so the row a
/// recovery entry point wrote can be asserted deterministically (load-flake
/// adjudication 2026-08-29: the live queue consumer raced these readbacks —
/// 5/8 parallel failures on clean main).
fn paused_orchestrator(pool: &SqlitePool, dir: &std::path::Path) -> JobOrchestrator {
    JobOrchestrator::start_with_options(
        pool.clone(),
        StageRegistry::default(),
        Arc::new(|_| {}),
        Arc::new(|_| {}),
        Duration::from_millis(10),
        dir.to_path_buf(),
        txtcaster_lib::orchestrator::StartOptions {
            consume_queue: false,
            ..Default::default()
        },
    )
}

fn orchestrator(pool: &SqlitePool, dir: &std::path::Path) -> JobOrchestrator {
    JobOrchestrator::start(
        pool.clone(),
        StageRegistry::default(),
        Arc::new(|_| {}),
        Arc::new(|_| {}),
        Duration::from_millis(10),
        dir.to_path_buf(),
    )
}

/// An orchestrator whose worker tick is an hour out: recovery/cancel races
/// stay deterministic.
fn slow_orchestrator(pool: &SqlitePool, dir: &std::path::Path) -> JobOrchestrator {
    JobOrchestrator::start(
        pool.clone(),
        StageRegistry::default(),
        Arc::new(|_| {}),
        Arc::new(|_| {}),
        Duration::from_secs(3600),
        dir.to_path_buf(),
    )
}

async fn seed_failed_cloud_job(pool: &SqlitePool, voice_engine: &str) -> String {
    let book = book::Book {
        id: new_id(),
        title: "Fixture".into(),
        author: None,
        source_path: "/books/f.pdf".into(),
        format: "pdf".into(),
        page_count: Some(1),
        language: None,
        is_scanned: false,
        ingest_path: None,
        cover_path: None,
        content_hash: "hash".into(),
        blocks_version: None,
        blocks_page_count: None,
        layout_analysed: false,
        reflowed: false,
        extracting_since: None,
        created_at: now_ms(),
    };
    book::insert_book(pool, &book).await.unwrap();
    let profile = voice::VoiceProfile {
        id: new_id(),
        name: "Voice".into(),
        engine: voice_engine.into(),
        kind: "preset".into(),
        preset_voice_id: Some("alloy".into()),
        reference_wav_path: None,
        params_json: Some(r#"{"connection_id":"c-1"}"#.into()),
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &profile).await.unwrap();
    let job_id = new_id();
    let overrides = serde_json::json!({
        "llm": "phi-4-mini",
        "tts_binding": {
            "kind": "connection", "connection_id": "c-1", "model": "m", "voice": "v",
            "provider": "cartesia_tts", "endpoint": "https://api.cartesia.ai",
            "adapter_rev": "cartesia_tts/1",
        }
    })
    .to_string();
    let record = job::RenderJob {
        id: job_id.clone(),
        book_id: book.id,
        scope_json: serde_json::to_string(&Scope::Pages {
            page_start: 1,
            page_end: 1,
        })
        .unwrap(),
        mode: "narrator".into(),
        voice_profile_id: profile.id,
        output_format: "mp3".into(),
        model_overrides_json: Some(overrides),
        resource_plan_json: None,
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Failed.as_str().into(),
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
    job::insert_job(pool, &record).await.unwrap();
    job_id
}

#[tokio::test]
async fn production_switch_to_local_lands_in_the_atomic_transaction() {
    // The pin Hermes asked for: the PRODUCTION entry point (the same fn the
    // IPC command calls) strips bindings, appends recovery_log with an
    // incremented plan_rev, and requeues — all through the one conditional
    // UPDATE, verified by the resulting row.
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;

    orchestrator
        .recover_failed(&job_id, true)
        .await
        .expect("recovers");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(record.state, "QUEUED");
    let overrides: serde_json::Value =
        serde_json::from_str(record.model_overrides_json.as_deref().unwrap()).unwrap();
    assert!(overrides.get("tts_binding").is_none(), "bindings stripped");
    assert_eq!(overrides["llm"], "phi-4-mini", "unowned keys survive");
    assert_eq!(overrides["recovery_log"][0]["action"], "switch_to_local");
    assert_eq!(overrides["recovery_log"][0]["plan_rev"], 1);
}

#[tokio::test]
async fn production_same_binding_retry_keeps_bindings_and_logs_the_revision() {
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;

    orchestrator
        .recover_failed(&job_id, false)
        .await
        .expect("requeues");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(record.state, "QUEUED");
    let overrides: serde_json::Value =
        serde_json::from_str(record.model_overrides_json.as_deref().unwrap()).unwrap();
    assert!(
        overrides.get("tts_binding").is_some(),
        "same-binding retry preserves the recorded binding"
    );
    assert_eq!(overrides["recovery_log"][0]["action"], "retry_same_binding");

    // A second recovery appends and increments — the log is history, not a flag.
    sqlx::query("UPDATE render_job SET state = 'FAILED' WHERE id = ?")
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();
    orchestrator
        .recover_failed(&job_id, false)
        .await
        .expect("requeues again");
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let overrides: serde_json::Value =
        serde_json::from_str(record.model_overrides_json.as_deref().unwrap()).unwrap();
    assert_eq!(overrides["recovery_log"].as_array().unwrap().len(), 2);
    assert_eq!(overrides["recovery_log"][1]["plan_rev"], 2);
}

#[tokio::test]
async fn production_recovery_refuses_non_failed_jobs_without_mutation() {
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;
    sqlx::query("UPDATE render_job SET state = 'DONE' WHERE id = ?")
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();

    orchestrator
        .recover_failed(&job_id, true)
        .await
        .expect_err("only FAILED recovers");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(record.state, "DONE");
    assert!(
        record
            .model_overrides_json
            .as_deref()
            .unwrap()
            .contains("tts_binding"),
        "a refused recovery mutates nothing"
    );
}

const SPENT_THROTTLE: &str =
    r#"{"stage":"SYNTHESIZING","rate_limit_pauses":3,"last_pause_done":40,"updated_at":1}"#;

#[tokio::test]
async fn a_refused_recovery_leaves_the_throttle_budget_untouched() {
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;
    sqlx::query("UPDATE render_job SET state = 'DONE', checkpoint_json = ? WHERE id = ?")
        .bind(SPENT_THROTTLE)
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();

    orchestrator
        .recover_failed(&job_id, false)
        .await
        .expect_err("only FAILED recovers");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(
        record.checkpoint_json.as_deref(),
        Some(SPENT_THROTTLE),
        "the checkpoint belongs to the same conditional write as the state"
    );
}

#[tokio::test]
async fn an_accepted_recovery_resets_the_throttle_budget_with_the_state() {
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;
    sqlx::query("UPDATE render_job SET checkpoint_json = ? WHERE id = ?")
        .bind(SPENT_THROTTLE)
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();

    orchestrator
        .recover_failed(&job_id, false)
        .await
        .expect("a FAILED job recovers");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(record.state, "QUEUED");
    let checkpoint: serde_json::Value =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint["rate_limit_pauses"], 0);
    assert_eq!(checkpoint["last_pause_done"], 0);
}

#[tokio::test]
async fn production_recovery_refuses_cloud_voice_jobs_with_the_remedy() {
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "cloud").await;

    let error = orchestrator
        .recover_failed(&job_id, true)
        .await
        .expect_err("cloud voice cannot switch to local");
    assert!(error.to_string().contains("local voice"), "{error}");
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(record.state, "FAILED", "nothing mutated");
}

#[tokio::test]
async fn a_settled_recovered_job_reports_its_recovery_history() {
    // Hermes F7 final-narrowing pin: recovery facts must be READABLE, not
    // write-only — after recovery, the job runs through the production
    // worker loop to a terminal settle (this harness has no stages, so it
    // terminates immediately), and the FINAL REPORT carries the provider
    // recovery history end-to-end.
    let (dir, pool) = test_pool().await;
    let orchestrator = orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;

    orchestrator
        .recover_failed(&job_id, true)
        .await
        .expect("recovers");

    // Wait for the worker to pick the job up and settle it terminally.
    let mut terminal_report: Option<String> = None;
    for _ in 0..100 {
        let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
        if record.state == "DONE" || record.state == "FAILED" {
            terminal_report = record.report_json.clone();
            if terminal_report
                .as_deref()
                .is_some_and(|report| report.contains("provider_recovery_log"))
            {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let report: serde_json::Value = serde_json::from_str(
        terminal_report
            .as_deref()
            .expect("the settled job carries a report"),
    )
    .expect("report parses");
    assert_eq!(
        report["provider_recovery_log"][0]["action"], "switch_to_local",
        "the recovery history is visible in the final report, not buried in overrides"
    );
    assert_eq!(report["provider_plan_rev"], 1);
}

#[tokio::test]
async fn a_malformed_stored_recovery_log_is_quarantined_never_swallowed() {
    // Hermes attempt-4 point 3: a corrupt stored log must not silently eat
    // the genuine recovery entry — it is quarantined under its own key and
    // the real history starts fresh.
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;
    sqlx::query("UPDATE render_job SET model_overrides_json = ? WHERE id = ?")
        .bind(r#"{"llm":"phi-4-mini","recovery_log":"corrupt"}"#)
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();

    orchestrator
        .recover_failed(&job_id, true)
        .await
        .expect("recovers despite the corrupt log");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let overrides: serde_json::Value =
        serde_json::from_str(record.model_overrides_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        overrides["recovery_log_quarantined"],
        serde_json::json!(["corrupt"]),
        "the corrupt whole-value is held in the quarantine array"
    );
    assert_eq!(overrides["recovery_log"][0]["action"], "switch_to_local");
    assert_eq!(overrides["recovery_log"][0]["plan_rev"], 1);
}

#[tokio::test]
async fn handleless_queued_cancellation_settles_with_the_recovery_history() {
    // Terminal matrix × CANCELLED: a recovered job cancelled while QUEUED
    // (no running handle) flows through the SAME settle and its final
    // report still carries the provider recovery history.
    let (dir, pool) = test_pool().await;
    let orchestrator = slow_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;
    // A recovered-then-paused job: the worker never enqueues it, so the
    // cancel below exercises the handle-less settlement route.
    sqlx::query(
        "UPDATE render_job SET state = 'PAUSED', model_overrides_json = ? WHERE id = ?",
    )
    .bind(
        r#"{"llm":"phi-4-mini","recovery_log":[{"action":"retry_same_binding","plan_rev":1,"at_unix":1}]}"#,
    )
    .bind(&job_id)
    .execute(&pool)
    .await
    .unwrap();

    orchestrator.cancel_job(&job_id).await.expect("cancels");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    assert_eq!(record.state, "CANCELLED");
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        report["provider_recovery_log"][0]["action"],
        "retry_same_binding"
    );
    assert_eq!(report["provider_plan_rev"], 1);
}

#[test]
fn no_terminal_state_write_bypasses_the_settle_path() {
    // The widened grep-proof (Hermes attempt-5 residual 4): across the
    // WHOLE crate, (a) no SQL literally SETs a terminal state — the one
    // settle write binds it as a parameter; (b) the storage fns able to
    // write the state column are exactly the audited allow-list; (c) every
    // caller of the state-writing fns stays non-terminal (4-line window,
    // multi-line calls covered), FAILED excepted only on the single
    // documented last-resort line; (d) job::settle_terminal is called only
    // from the worker's one settlement fn.
    fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let src = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(files.len() > 50, "source walk looks wrong: {}", files.len());

    let terminal_sql = ["state = 'FAILED'", "state = 'DONE'", "state = 'CANCELLED'"];
    let mut settle_callers = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        let name = path.display();
        // (a) No literal terminal-state SET anywhere (WHERE guards are
        // fine and don't match the "SET ... state = '...'" shape).
        for needle in terminal_sql {
            for (index, line) in source.lines().enumerate() {
                // Only the SET clause writes; a terminal name in the WHERE
                // guard is the conditional-update safety, not a write.
                let (Some(set_at), Some(hit_at)) = (line.find("SET "), line.find(needle)) else {
                    continue;
                };
                let where_at = line.find("WHERE").unwrap_or(line.len());
                assert!(
                    !(hit_at > set_at && hit_at < where_at),
                    "raw terminal SQL at {name}:{}: {line}",
                    index + 1
                );
            }
        }
        // (c) Callers of the state-writing fns: a 4-line window from the
        // call must not name a terminal state.
        let lines: Vec<&str> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.contains("fn update_job_state") {
                continue;
            }
            if line.contains("fn apply_recovery") {
                continue;
            }
            if line.contains("apply_recovery(") {
                let window = lines[index..lines.len().min(index + 8)].join(" ");
                for terminal in ["JobState::Done", "JobState::Cancelled", "JobState::Failed"] {
                    assert!(
                        !window.contains(terminal),
                        "terminal write via apply_recovery at {name}:{}",
                        index + 1
                    );
                }
            }
            if line.contains("update_job_state(") || line.contains("update_job_state_if(") {
                let window = lines[index..lines.len().min(index + 4)].join(" ");
                assert!(
                    !window.contains("Done") && !window.contains("Cancelled"),
                    "terminal bypass at {name}:{}",
                    index + 1
                );
                if window.contains("Failed") {
                    assert!(
                        lines[index.saturating_sub(2)..index]
                            .iter()
                            .any(|prior| prior.contains("last-resort")),
                        "undocumented FAILED bypass at {name}:{}",
                        index + 1
                    );
                }
            }
            if !line.contains("fn settle_terminal")
                && (line.contains("job::settle_terminal(") || line.contains(" settle_terminal("))
            {
                settle_callers.push(format!("{name}:{}", index + 1));
            }
        }
    }
    // (d) The atomic settle write has exactly one caller: the worker's
    // settlement fn (the storage definition itself doesn't match the
    // caller shapes above).
    assert_eq!(
        settle_callers.len(),
        1,
        "settle_terminal callers changed: {settle_callers:?}"
    );
    assert!(settle_callers[0].contains("orchestrator/worker.rs"));

    // (b) The storage fns whose SQL can write the state column are
    // exactly the audited allow-list.
    let storage = std::fs::read_to_string(src.join("storage/job.rs")).unwrap();
    let mut state_writers = Vec::new();
    for block in storage.split("pub async fn ").skip(1) {
        let fn_name = block.split('(').next().unwrap_or("").to_string();
        let writes_state = block.lines().any(|line| {
            let (Some(set_at), Some(hit_at)) = (line.find("SET "), line.find(" state =")) else {
                return false;
            };
            hit_at > set_at && hit_at < line.find("WHERE").unwrap_or(line.len())
        });
        if writes_state {
            state_writers.push(fn_name);
        }
    }
    state_writers.sort();
    assert_eq!(
        state_writers,
        vec![
            "apply_recovery".to_string(),
            "recover_failed_job_to_local".to_string(),
            "settle_terminal".to_string(),
            "update_job_state".to_string(),
            "update_job_state_if".to_string(),
        ],
        "state-writing storage surface changed — re-audit the settle design"
    );
}

struct FailingStage;

impl Stage for FailingStage {
    fn run<'a>(
        &'a self,
        _ctx: &'a mut StageContext,
    ) -> futures_util::future::BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async {
            Err(OrchestratorError::BookNotFound(
                "provider still down".into(),
            ))
        })
    }
}

#[tokio::test]
async fn a_recovered_job_that_fails_again_keeps_its_recovery_history() {
    // Terminal matrix × FAILED-again (Hermes attempt-5 residual 1): the
    // recovered run fails deterministically, and the FINAL FAILED report
    // still carries the provider recovery history through the settle merge.
    let (dir, pool) = test_pool().await;
    let mut registry = StageRegistry::default();
    registry.register(STAGE_ORDER[0], Arc::new(FailingStage));
    let orchestrator = JobOrchestrator::start(
        pool.clone(),
        registry,
        Arc::new(|_| {}),
        Arc::new(|_| {}),
        Duration::from_millis(10),
        dir.path().to_path_buf(),
    );
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;

    orchestrator
        .recover_failed(&job_id, true)
        .await
        .expect("recovers");

    let mut settled = None;
    for _ in 0..200 {
        let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
        if record.state == "FAILED" {
            settled = Some(record);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let record = settled.expect("the recovered run fails again");
    let report: serde_json::Value =
        serde_json::from_str(record.report_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        report["provider_recovery_log"][0]["action"], "switch_to_local",
        "a FAILED-again settle keeps the recovery history: {report}"
    );
    assert_eq!(report["provider_plan_rev"], 1);
    assert!(
        report["error"]
            .as_str()
            .unwrap_or("")
            .contains("provider still down"),
        "the new failure and the old history coexist: {report}"
    );
}

#[tokio::test]
async fn plan_rev_comes_from_typed_max_revision_and_bad_entries_quarantine() {
    // Hermes attempt-5 residual 3: entries are TYPED — an invalid entry
    // mixed into a stored log quarantines individually (never published,
    // never swallowed), and the new revision is max(valid)+1, not
    // array-length arithmetic that invalid entries would skew.
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;
    sqlx::query("UPDATE render_job SET model_overrides_json = ? WHERE id = ?")
        .bind(
            r#"{"llm":"phi-4-mini","recovery_log":[
                {"action":"retry_same_binding","plan_rev":4,"at_unix":1},
                {"action":"melt_the_disk","plan_rev":5,"at_unix":2},
                "not even an object"
            ]}"#,
        )
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();

    orchestrator
        .recover_failed(&job_id, true)
        .await
        .expect("recovers");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let overrides: serde_json::Value =
        serde_json::from_str(record.model_overrides_json.as_deref().unwrap()).unwrap();
    let log = overrides["recovery_log"].as_array().unwrap();
    assert_eq!(log.len(), 2, "one valid survivor + the genuine new entry");
    assert_eq!(
        log[1]["plan_rev"], 5,
        "max(valid revision)=4 + 1, not len+1"
    );
    assert_eq!(log[1]["action"], "switch_to_local");
    let quarantined = overrides["recovery_log_quarantined"].as_array().unwrap();
    assert_eq!(
        quarantined.len(),
        2,
        "both invalid values held: {quarantined:?}"
    );
    assert!(quarantined
        .iter()
        .any(|value| value == "not even an object"));
}

#[tokio::test]
async fn history_invariants_quarantine_zero_dup_decreasing_negative_and_exhausted_revisions() {
    // Hermes attempt-6: shape-valid entries that violate HISTORY
    // invariants quarantine too — plan_rev 0, a duplicate [4,4], a
    // decreasing [4,2], a negative timestamp — and a u64::MAX revision
    // must not overflow-panic: the exhausted history quarantines and the
    // genuine entry restarts at revision 1.
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;
    sqlx::query("UPDATE render_job SET model_overrides_json = ? WHERE id = ?")
        .bind(
            r#"{"llm":"phi-4-mini","recovery_log":[
                {"action":"retry_same_binding","plan_rev":0,"at_unix":1},
                {"action":"retry_same_binding","plan_rev":4,"at_unix":1},
                {"action":"retry_same_binding","plan_rev":4,"at_unix":2},
                {"action":"retry_same_binding","plan_rev":2,"at_unix":3},
                {"action":"retry_same_binding","plan_rev":6,"at_unix":-5}
            ]}"#,
        )
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();

    orchestrator
        .recover_failed(&job_id, true)
        .await
        .expect("recovers past the corrupt history");

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let overrides: serde_json::Value =
        serde_json::from_str(record.model_overrides_json.as_deref().unwrap()).unwrap();
    let log = overrides["recovery_log"].as_array().unwrap();
    assert_eq!(
        log.len(),
        2,
        "only rev-4 survives + the genuine entry: {log:?}"
    );
    assert_eq!(log[0]["plan_rev"], 4);
    assert_eq!(log[1]["plan_rev"], 5, "max(valid)=4 + 1");
    assert_eq!(log[1]["action"], "switch_to_local");
    assert_eq!(
        overrides["recovery_log_quarantined"]
            .as_array()
            .unwrap()
            .len(),
        4,
        "zero, duplicate, decreasing, and negative-timestamp entries all held"
    );

    // Revision exhaustion: max-revision history quarantines, entry lands at 1.
    sqlx::query("UPDATE render_job SET state = 'FAILED', model_overrides_json = ? WHERE id = ?")
        .bind(format!(
            r#"{{"recovery_log":[{{"action":"retry_same_binding","plan_rev":{},"at_unix":9}}]}}"#,
            u64::MAX
        ))
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();
    orchestrator
        .recover_failed(&job_id, false)
        .await
        .expect("exhaustion never panics");
    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let overrides: serde_json::Value =
        serde_json::from_str(record.model_overrides_json.as_deref().unwrap()).unwrap();
    let log = overrides["recovery_log"].as_array().unwrap();
    assert_eq!(log.len(), 1, "fresh history after exhaustion: {log:?}");
    assert_eq!(log[0]["plan_rev"], 1);
    assert_eq!(
        overrides["recovery_log_quarantined"][0]["plan_rev"],
        serde_json::json!(u64::MAX),
        "the exhausted history is held, not lost"
    );
}

#[tokio::test]
async fn recovery_re_arms_the_cap_on_the_checkpoint_as_it_is_when_written() {
    let (_dir, pool) = test_pool().await;
    let job_id = seed_failed_cloud_job(&pool, "kokoro-82m").await;
    sqlx::query("UPDATE render_job SET checkpoint_json = ? WHERE id = ?")
        .bind(SPENT_THROTTLE)
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        job::recover_failed_job_to_local(&pool, &job_id, Some("{}"), true)
            .await
            .unwrap()
    );

    let record = job::get_job(&pool, &job_id).await.unwrap().unwrap();
    let checkpoint: serde_json::Value =
        serde_json::from_str(record.checkpoint_json.as_deref().unwrap()).unwrap();
    assert_eq!(checkpoint["rate_limit_pauses"], 0);
    assert_eq!(checkpoint["last_pause_done"], 0);
    assert_eq!(
        checkpoint["stage"], "SYNTHESIZING",
        "fields another writer owns survive the re-arm instead of being replaced by a stale copy"
    );
    assert_eq!(checkpoint["updated_at"], 1);
}

async fn seed_rate_limited_pause(pool: &SqlitePool, class: &str, retry_at: Option<i64>) -> String {
    let job_id = seed_failed_cloud_job(pool, "kokoro-82m").await;
    let checkpoint = serde_json::json!({
        "stage": "SYNTHESIZING",
        "updated_at": 1,
        "paused_reason": {
            "kind": "rate_limit", "class": class, "retry_at_epoch_ms": retry_at,
            "provider": "google", "segment_idx": 4, "last_error_message": "429", "attempt": 2,
        },
    });
    sqlx::query("UPDATE render_job SET state = 'PAUSED', checkpoint_json = ? WHERE id = ?")
        .bind(checkpoint.to_string())
        .bind(&job_id)
        .execute(pool)
        .await
        .unwrap();
    job_id
}

#[tokio::test]
async fn switching_a_daily_quota_pause_to_overnight_does_not_redial_the_provider() {
    let (dir, pool) = test_pool().await;
    let orchestrator = paused_orchestrator(&pool, dir.path());
    let daily = seed_rate_limited_pause(&pool, "daily", None).await;
    let (control_dir, control_pool) = test_pool().await;
    let control = paused_orchestrator(&control_pool, control_dir.path());
    let per_minute = seed_rate_limited_pause(&control_pool, "per_minute", Some(now_ms() - 1)).await;

    orchestrator.switch_to_overnight(&daily).await.unwrap();
    control.switch_to_overnight(&per_minute).await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while job::get_job(&control_pool, &per_minute)
        .await
        .unwrap()
        .unwrap()
        .state
        == "PAUSED"
    {
        assert!(
            std::time::Instant::now() < deadline,
            "control: a per-minute pause past its retry time resumes on the switch"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let record = job::get_job(&pool, &daily).await.unwrap().unwrap();
    assert_eq!(
        record.state, "PAUSED",
        "a daily quota has no retry time, so the switch must not re-dial it"
    );
    assert!(record.overnight);
}
