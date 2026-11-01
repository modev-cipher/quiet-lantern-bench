use std::time::{Duration, Instant};

use sqlx::SqlitePool;

use super::state::{JobState, OrchestratorError};
use crate::storage::job;

pub const BUSY_CODE: &str = "ERR_BUSY";
pub const OOM_CODE: &str = "ERR_OOM";
pub const DEVICE_UNAVAILABLE_CODE: &str = "ERR_DEVICE_UNAVAILABLE";
pub const BUSY_NOTICE: &str = "Engine busy — waiting to retry";
pub const BUSY_PAUSE_NOTICE: &str = "Paused — the engine stayed busy";
pub const MAX_CONTENTION_PAUSES: i64 = 3;
pub const ENGINE_RESERVATION_GRACE: Duration = Duration::from_secs(120);
pub const UNLOAD_DRAIN_RETRIES: u32 = 3;
pub const UNLOAD_DRAIN_DELAY: Duration = Duration::from_secs(2);

pub const TRANSPORT_FAILURES: [&str; 4] = [
    "stream failed",
    "stream timed out",
    "request failed",
    "stream ended without completion",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageFailure {
    Degradable,
    DeviceUnusable,
    Contention,
    Terminal,
}

impl StageFailure {
    pub fn recovers(self) -> bool {
        matches!(
            self,
            StageFailure::Degradable | StageFailure::DeviceUnusable
        )
    }
}

pub fn classify_code(code: &str) -> StageFailure {
    match code {
        BUSY_CODE => StageFailure::Contention,
        OOM_CODE => StageFailure::Degradable,
        DEVICE_UNAVAILABLE_CODE => StageFailure::DeviceUnusable,
        _ => StageFailure::Terminal,
    }
}

pub fn classify_failure(error: &OrchestratorError) -> StageFailure {
    match error {
        OrchestratorError::SidecarUnavailable => StageFailure::Degradable,
        OrchestratorError::Engine { code, .. } => classify_code(code),
        OrchestratorError::Sidecar(message) => {
            if TRANSPORT_FAILURES
                .iter()
                .any(|failure| message.contains(failure))
            {
                StageFailure::Degradable
            } else {
                StageFailure::Terminal
            }
        }
        _ => StageFailure::Terminal,
    }
}

pub const CONTENTION_KEY: &str = "contention";
pub const CONTENTION_BY_STAGE_KEY: &str = "by_stage";
pub const CONTENTION_PAUSED_KEY: &str = "paused";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContentionDelta {
    pub episodes: u64,
    pub retries: u64,
    pub waited_s: u64,
}

impl ContentionDelta {
    pub fn is_empty(&self) -> bool {
        *self == ContentionDelta::default()
    }
}

fn add_into(target: &mut serde_json::Value, delta: &ContentionDelta) {
    for (key, value) in [
        ("episodes", delta.episodes),
        ("retries", delta.retries),
        ("waited_s", delta.waited_s),
    ] {
        let held = target[key].as_u64().unwrap_or(0);
        target[key] = serde_json::json!(held.saturating_add(value));
    }
}

fn object_at<'a>(parent: &'a mut serde_json::Value, key: &str) -> &'a mut serde_json::Value {
    if !parent[key].is_object() {
        parent[key] = serde_json::json!({});
    }
    &mut parent[key]
}

pub async fn record_contention(
    pool: &SqlitePool,
    job_id: &str,
    stage: JobState,
    delta: ContentionDelta,
) -> Result<(), OrchestratorError> {
    if delta.is_empty() {
        return Ok(());
    }
    let Some(record) = job::get_job(pool, job_id).await? else {
        return Ok(());
    };
    let mut report: serde_json::Value = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let block = object_at(&mut report, CONTENTION_KEY);
    add_into(block, &delta);
    let row = object_at(object_at(block, CONTENTION_BY_STAGE_KEY), stage.as_str());
    add_into(row, &delta);
    job::set_job_report_only(pool, job_id, &serde_json::to_string(&report)?).await?;
    Ok(())
}

async fn write_contention_pause(
    pool: &SqlitePool,
    job_id: &str,
    stage: JobState,
    retries: u32,
    waited_s: u64,
    resume_attempts: i64,
) -> Result<(), OrchestratorError> {
    let Some(record) = job::get_job(pool, job_id).await? else {
        return Ok(());
    };
    let mut report: serde_json::Value = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let block = object_at(&mut report, CONTENTION_KEY);
    block[CONTENTION_PAUSED_KEY] = serde_json::json!({
        "stage": stage.as_str(),
        "retries": retries,
        "waited_s": waited_s,
        "resume_attempts": resume_attempts,
        "max_pauses": MAX_CONTENTION_PAUSES,
        "at": crate::storage::now_ms(),
    });
    job::set_job_report_only(pool, job_id, &serde_json::to_string(&report)?).await?;
    Ok(())
}

pub async fn record_contention_pause(
    pool: &SqlitePool,
    job_id: &str,
    stage: JobState,
    retries: u32,
    waited_s: u64,
    resume_attempts: i64,
) {
    if let Err(error) =
        write_contention_pause(pool, job_id, stage, retries, waited_s, resume_attempts).await
    {
        tracing::warn!("contention pause telemetry failed for job {job_id}: {error}");
    }
}

pub async fn clear_contention_pause(pool: &SqlitePool, job_id: &str) {
    if let Err(error) = wipe_contention_pause(pool, job_id).await {
        tracing::warn!("clearing the contention pause failed for job {job_id}: {error}");
    }
}

pub fn strip_contention_pause(report: &mut serde_json::Value) -> bool {
    report
        .get_mut(CONTENTION_KEY)
        .and_then(|block| block.as_object_mut())
        .is_some_and(|block| block.remove(CONTENTION_PAUSED_KEY).is_some())
}

async fn wipe_contention_pause(pool: &SqlitePool, job_id: &str) -> Result<(), OrchestratorError> {
    let Some(record) = job::get_job(pool, job_id).await? else {
        return Ok(());
    };
    let mut report: serde_json::Value = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    if !strip_contention_pause(&mut report) {
        return Ok(());
    }
    job::set_job_report_only(pool, job_id, &serde_json::to_string(&report)?).await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusyRetryPolicy {
    pub first_delay: Duration,
    pub max_delay: Duration,
    pub total_budget: Duration,
    pub max_retries: u32,
}

impl BusyRetryPolicy {
    pub const fn production() -> Self {
        BusyRetryPolicy {
            first_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            total_budget: Duration::from_secs(ENGINE_RESERVATION_GRACE.as_secs() + 30),
            max_retries: 12,
        }
    }
}

#[derive(Debug)]
struct Episode {
    stage: JobState,
    started: Instant,
    retries: u32,
    credited: bool,
    credited_retries: u32,
    credited_waited_s: u64,
}

impl Episode {
    fn uncredited(&mut self, now: Instant) -> ContentionDelta {
        let waited_s = now.saturating_duration_since(self.started).as_secs();
        let delta = ContentionDelta {
            episodes: u64::from(!self.credited),
            retries: u64::from(self.retries.saturating_sub(self.credited_retries)),
            waited_s: waited_s.saturating_sub(self.credited_waited_s),
        };
        self.credited = true;
        self.credited_retries = self.retries;
        self.credited_waited_s = waited_s;
        delta
    }
}

#[derive(Debug)]
pub struct BusyRetry {
    policy: BusyRetryPolicy,
    episode: Option<Episode>,
}

impl BusyRetry {
    pub fn new(policy: BusyRetryPolicy) -> Self {
        BusyRetry {
            policy,
            episode: None,
        }
    }

    pub fn retries(&self) -> u32 {
        self.episode.as_ref().map_or(0, |episode| episode.retries)
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        self.episode.as_ref().map_or(Duration::ZERO, |episode| {
            now.saturating_duration_since(episode.started)
        })
    }

    pub fn take_delta(&mut self, now: Instant) -> Option<(JobState, ContentionDelta)> {
        let episode = self.episode.as_mut()?;
        let stage = episode.stage;
        let delta = episode.uncredited(now);
        (!delta.is_empty()).then_some((stage, delta))
    }

    pub fn close(&mut self, now: Instant) -> Option<(JobState, ContentionDelta)> {
        let closing = self.take_delta(now);
        self.episode = None;
        closing
    }

    pub fn next_delay(&mut self, stage: JobState, now: Instant) -> Option<Duration> {
        let episode = match self.episode.take() {
            Some(episode) if episode.stage == stage => episode,
            _ => Episode {
                stage,
                started: now,
                retries: 0,
                credited: false,
                credited_retries: 0,
                credited_waited_s: 0,
            },
        };
        let episode = self.episode.insert(episode);
        if episode.retries >= self.policy.max_retries {
            return None;
        }
        let factor = 1u32.checked_shl(episode.retries).unwrap_or(u32::MAX);
        let delay = self
            .policy
            .first_delay
            .saturating_mul(factor)
            .min(self.policy.max_delay);
        let elapsed = now.saturating_duration_since(episode.started);
        if elapsed.saturating_add(delay) > self.policy.total_budget {
            return None;
        }
        episode.retries += 1;
        Some(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidecar(message: &str) -> OrchestratorError {
        OrchestratorError::Sidecar(message.to_string())
    }

    fn engine(code: &str, message: &str) -> OrchestratorError {
        OrchestratorError::Engine {
            status: None,
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    fn drain(policy: BusyRetryPolicy) -> Vec<Duration> {
        let mut retry = BusyRetry::new(policy);
        let mut now = Instant::now();
        let mut delays = Vec::new();
        while let Some(delay) = retry.next_delay(JobState::Scripting, now) {
            delays.push(delay);
            now += delay;
            if delays.len() > 64 {
                panic!("the backoff never gave up");
            }
        }
        delays
    }

    #[test]
    fn an_out_of_memory_stage_walks_the_degradation_ladder() {
        assert_eq!(
            classify_failure(&engine(OOM_CODE, "CUDA out of memory")),
            StageFailure::Degradable
        );
        assert_eq!(
            classify_failure(&OrchestratorError::SidecarUnavailable),
            StageFailure::Degradable
        );
        for transport in [
            "sidecar stream failed: connection reset",
            "sidecar stream timed out",
            "sidecar request failed: connection refused",
            "align stream ended without completion",
        ] {
            assert_eq!(
                classify_failure(&sidecar(transport)),
                StageFailure::Degradable,
                "a transport failure carries no engine code, so it is the one thing the \
                 substring matcher still exists for: {transport}"
            );
        }
    }

    #[test]
    fn a_busy_engine_is_contention_and_never_walks_the_degradation_ladder() {
        for status in [None, Some(409)] {
            let error = OrchestratorError::Engine {
                status,
                code: BUSY_CODE.to_string(),
                message: "model slot held by 'phi4-mini'; retry after it is released".to_string(),
            };
            assert_eq!(
                classify_failure(&error),
                StageFailure::Contention,
                "contention must not shrink a plan that was never the problem: {error}"
            );
        }
    }

    #[test]
    fn only_the_code_decides_and_a_message_that_quotes_one_cannot_reclassify_it() {
        for quoted in [
            "ERR_BUSY",
            "ERR_OOM",
            r#"{"detail":{"code":"ERR_BUSY","message":"slot reserved"}}"#,
        ] {
            assert_eq!(
                classify_failure(&engine("ERR_PLAN", quoted)),
                StageFailure::Terminal,
                "the /script request body carries raw book text, so a 422 validation detail can \
                 echo any code back at us; only the engine's own code field may decide: {quoted}"
            );
        }
        assert_eq!(
            classify_failure(&engine(BUSY_CODE, "ERR_OOM was not the problem")),
            StageFailure::Contention,
            "the code decides even when the prose names a different one"
        );
        assert_eq!(
            classify_failure(&sidecar(
                r#"sidecar returned 409 Conflict for /vision: {"code":"ERR_BUSY"}"#
            )),
            StageFailure::Terminal,
            "an engine refusal reaches classification as Engine{{..}}; anything still arriving \
             as an opaque string is our own transport prose, and must not be code-matched"
        );
    }

    #[test]
    fn every_code_the_classifier_acts_on_is_declared_once() {
        assert_eq!(classify_code(BUSY_CODE), StageFailure::Contention);
        assert_eq!(classify_code(OOM_CODE), StageFailure::Degradable);
        for terminal in [
            "ERR_PLAN",
            "ERR_RESIDENCY",
            "ERR_INTERNAL",
            "ERR_LLM_UNLOADED",
            crate::orchestrator::state::MISSING_EVENT_CODE,
            crate::sidecar::client::UNSTRUCTURED_CODE,
            "",
        ] {
            assert_eq!(
                classify_code(terminal),
                StageFailure::Terminal,
                "an unrecognised code must fail the job honestly rather than enter a retry or \
                 the degradation ladder: {terminal}"
            );
        }
    }

    #[test]
    fn an_engine_event_without_a_code_is_terminal_rather_than_silently_degradable() {
        let error = OrchestratorError::from_engine_event(
            &serde_json::json!({"message": "layout failed"}),
            "fallback",
        );
        assert_eq!(
            error.engine_code(),
            Some(crate::orchestrator::state::MISSING_EVENT_CODE)
        );
        assert_eq!(classify_failure(&error), StageFailure::Terminal);
        let error = OrchestratorError::from_engine_event(&serde_json::json!({}), "align failed");
        assert!(
            error.to_string().contains("align failed"),
            "an event with neither field still has to say which stage produced it: {error}"
        );
    }

    #[test]
    fn an_exhausted_contention_budget_is_terminal() {
        assert_eq!(
            classify_failure(&OrchestratorError::EngineBusy {
                stage: JobState::Scripting,
                retries: 8,
                waited_s: 121,
                cause: "ERR_BUSY: slot held".into(),
            }),
            StageFailure::Terminal,
            "the give-up error still names ERR_BUSY, so it must be classified structurally or \
             the backoff restarts itself"
        );
    }

    #[test]
    fn an_ordinary_sidecar_error_is_terminal() {
        assert_eq!(
            classify_failure(&engine("ERR_TTS_MODEL", "voice not installed")),
            StageFailure::Terminal
        );
        assert_eq!(
            classify_failure(&sidecar("script produced no segments")),
            StageFailure::Terminal
        );
        assert_eq!(
            classify_failure(&OrchestratorError::InvalidRequest("bad input".into())),
            StageFailure::Terminal
        );
    }

    #[test]
    fn the_delay_doubles_until_it_reaches_the_cap() {
        let delays = drain(BusyRetryPolicy {
            first_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(4),
            total_budget: Duration::from_secs(100),
            max_retries: 6,
        });
        assert_eq!(
            delays,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(4),
                Duration::from_secs(4),
                Duration::from_secs(4),
            ]
        );
    }

    #[test]
    fn the_wall_clock_budget_stops_retries_the_attempt_cap_would_still_allow() {
        let policy = BusyRetryPolicy {
            first_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(10),
            total_budget: Duration::from_secs(25),
            max_retries: 100,
        };
        let delays = drain(policy);
        assert_eq!(delays.len(), 2);
        assert_eq!(
            delays.iter().sum::<Duration>(),
            Duration::from_secs(20),
            "a third delay would overshoot the budget, so it is refused rather than truncated"
        );
    }

    #[test]
    fn the_budget_measures_the_stage_time_between_retries_not_only_the_delays() {
        let policy = BusyRetryPolicy {
            first_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(1),
            total_budget: Duration::from_secs(60),
            max_retries: 100,
        };
        let mut retry = BusyRetry::new(policy);
        let start = Instant::now();
        let mut now = start;
        let mut granted = 0;
        while let Some(delay) = retry.next_delay(JobState::Synthesizing, now) {
            granted += 1;
            now += delay + Duration::from_secs(20);
            if granted > 10 {
                panic!("a slow stage must still exhaust the budget");
            }
        }
        assert_eq!(
            granted, 3,
            "a stage that takes 20s to report contention spends the budget in 3 retries, not 60; \
             an accounted-only budget would have granted 60 and run for 21 minutes"
        );
        assert!(retry.elapsed(now) >= Duration::from_secs(60));
    }

    #[test]
    fn the_attempt_cap_stops_retries_the_wall_clock_budget_would_still_allow() {
        let policy = BusyRetryPolicy {
            first_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
            total_budget: Duration::from_secs(3600),
            max_retries: 3,
        };
        let delays = drain(policy);
        assert_eq!(delays.len(), 3);
        assert!(delays.iter().sum::<Duration>() < policy.total_budget);
    }

    #[test]
    fn a_policy_that_refuses_every_retry_gives_up_immediately() {
        let delays = drain(BusyRetryPolicy {
            first_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(1),
            total_budget: Duration::from_secs(60),
            max_retries: 0,
        });
        assert!(delays.is_empty());
    }

    #[test]
    fn each_stage_gets_its_own_budget() {
        let policy = BusyRetryPolicy {
            first_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(1),
            total_budget: Duration::from_secs(60),
            max_retries: 2,
        };
        let mut retry = BusyRetry::new(policy);
        let now = Instant::now();
        assert!(retry.next_delay(JobState::Scripting, now).is_some());
        assert!(retry.next_delay(JobState::Scripting, now).is_some());
        assert!(retry.next_delay(JobState::Scripting, now).is_none());

        assert!(retry.next_delay(JobState::Synthesizing, now).is_some());
        assert_eq!(retry.retries(), 1);
    }

    #[test]
    fn clearing_starts_a_fresh_episode_for_the_same_stage() {
        let policy = BusyRetryPolicy {
            first_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(1),
            total_budget: Duration::from_secs(60),
            max_retries: 2,
        };
        let mut retry = BusyRetry::new(policy);
        let now = Instant::now();
        assert!(retry.next_delay(JobState::Scripting, now).is_some());
        assert!(retry.next_delay(JobState::Scripting, now).is_some());
        assert!(retry.next_delay(JobState::Scripting, now).is_none());

        retry.close(now);
        assert_eq!(retry.retries(), 0);
        assert_eq!(retry.elapsed(now), Duration::ZERO);
        assert!(
            retry.next_delay(JobState::Scripting, now).is_some(),
            "contention that cleared and came back is a new episode; carrying the spent budget \
             forward would give the second episode less than the engine needs to self-heal"
        );
    }

    #[test]
    fn an_episode_is_credited_once_and_its_retries_are_never_double_counted() {
        let policy = BusyRetryPolicy {
            first_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(1),
            total_budget: Duration::from_secs(600),
            max_retries: 10,
        };
        let mut retry = BusyRetry::new(policy);
        let start = Instant::now();

        assert_eq!(
            retry.take_delta(start),
            None,
            "no episode, nothing to credit"
        );

        retry.next_delay(JobState::Scripting, start);
        assert_eq!(
            retry.take_delta(start),
            Some((
                JobState::Scripting,
                ContentionDelta {
                    episodes: 1,
                    retries: 1,
                    waited_s: 0
                }
            )),
            "the first refusal opens the episode and grants one retry, with nothing waited yet"
        );
        assert_eq!(
            retry.take_delta(start),
            None,
            "crediting twice without progress would inflate every number in the report"
        );

        let second = start + Duration::from_secs(4);
        retry.next_delay(JobState::Scripting, second);
        assert_eq!(
            retry.take_delta(second),
            Some((
                JobState::Scripting,
                ContentionDelta {
                    episodes: 0,
                    retries: 1,
                    waited_s: 4
                }
            )),
            "the second refusal is the same episode, so only the new retry and the newly \
             measured wait are credited"
        );
    }

    #[test]
    fn a_second_episode_at_the_same_stage_is_counted_separately() {
        let policy = BusyRetryPolicy {
            first_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(1),
            total_budget: Duration::from_secs(600),
            max_retries: 10,
        };
        let mut retry = BusyRetry::new(policy);
        let start = Instant::now();
        retry.next_delay(JobState::Scripting, start);
        retry.take_delta(start);
        let closed = retry.close(start + Duration::from_secs(3));
        assert_eq!(
            closed,
            Some((
                JobState::Scripting,
                ContentionDelta {
                    episodes: 0,
                    retries: 0,
                    waited_s: 3
                }
            )),
            "closing credits the delay that was slept after the last refusal; without it the \
             final wait of every episode is missing from the report"
        );

        let later = start + Duration::from_secs(60);
        retry.next_delay(JobState::Scripting, later);
        assert_eq!(
            retry.take_delta(later).map(|(_, delta)| delta.episodes),
            Some(1),
            "contention that cleared and came back is a second episode, not a continuation"
        );
    }

    #[test]
    fn crediting_in_whole_seconds_does_not_lose_time_across_a_long_episode() {
        let policy = BusyRetryPolicy {
            first_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
            total_budget: Duration::from_secs(3600),
            max_retries: 1000,
        };
        let mut retry = BusyRetry::new(policy);
        let start = Instant::now();
        let mut now = start;
        let mut credited = 0u64;
        for _ in 0..40 {
            retry.next_delay(JobState::Synthesizing, now);
            now += Duration::from_millis(1500);
            if let Some((_, delta)) = retry.take_delta(now) {
                credited += delta.waited_s;
            }
        }
        assert_eq!(
            credited,
            now.saturating_duration_since(start).as_secs(),
            "each credit is the difference of two floors, not a floor of the difference, so 40 \
             half-second remainders must not vanish from waited_s"
        );
    }

    #[test]
    fn the_delta_is_added_to_the_block_rather_than_replacing_it() {
        let mut report = serde_json::json!({
            "contention": {
                "episodes": 2, "retries": 5, "waited_s": 71,
                "by_stage": { "SCRIPTING": { "episodes": 2, "retries": 5, "waited_s": 71 } }
            }
        });
        let block = object_at(&mut report, CONTENTION_KEY);
        let delta = ContentionDelta {
            episodes: 1,
            retries: 3,
            waited_s: 29,
        };
        add_into(block, &delta);
        add_into(
            object_at(
                object_at(block, CONTENTION_BY_STAGE_KEY),
                JobState::Synthesizing.as_str(),
            ),
            &delta,
        );
        assert_eq!(report["contention"]["episodes"], 3);
        assert_eq!(report["contention"]["retries"], 8);
        assert_eq!(report["contention"]["waited_s"], 100);
        assert_eq!(report["contention"]["by_stage"]["SCRIPTING"]["retries"], 5);
        assert_eq!(
            report["contention"]["by_stage"]["SYNTHESIZING"]["retries"], 3,
            "a stage that contends after another one must not overwrite its row"
        );
    }

    #[test]
    fn a_corrupt_or_absent_block_starts_from_zero_instead_of_being_dropped() {
        for existing in [
            serde_json::json!({}),
            serde_json::json!({ "contention": 7 }),
            serde_json::json!({ "contention": { "retries": "many", "by_stage": [] } }),
        ] {
            let mut report = existing.clone();
            let block = object_at(&mut report, CONTENTION_KEY);
            let delta = ContentionDelta {
                episodes: 1,
                retries: 1,
                waited_s: 2,
            };
            add_into(block, &delta);
            add_into(
                object_at(
                    object_at(block, CONTENTION_BY_STAGE_KEY),
                    JobState::Aligning.as_str(),
                ),
                &delta,
            );
            assert_eq!(report["contention"]["retries"], 1, "{existing}");
            assert_eq!(
                report["contention"]["by_stage"]["ALIGNING"]["waited_s"], 2,
                "a hand-edited report must not make the telemetry panic or silently stop \
                 recording: {existing}"
            );
        }
    }

    #[test]
    fn the_production_backoff_outlasts_the_engine_reservation_grace() {
        let policy = BusyRetryPolicy::production();
        let delays = drain(policy);
        let total: Duration = delays.iter().sum();
        assert!(
            total > ENGINE_RESERVATION_GRACE,
            "the engine reclaims a stale reservation only after {}s, so a shorter budget gives \
             up before the contention it is waiting on can clear: {total:?} over {} retries",
            ENGINE_RESERVATION_GRACE.as_secs(),
            delays.len()
        );
        assert!(
            (delays.len() as u32) < policy.max_retries,
            "the wall-clock budget is meant to be the bound that fires in production; if the \
             attempt cap fires first the budget is unreachable and untested"
        );
    }

    fn engine_source(relative: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../engine/txtcaster_engine")
            .join(relative);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("engine source {}: {error}", path.display()))
    }

    fn engine_residency_source() -> String {
        engine_source("residency.py")
    }

    fn engine_method(source: &str, name: &str) -> String {
        let opener = format!("    def {name}(");
        let start = source
            .find(&opener)
            .unwrap_or_else(|| panic!("engine declares {name}"));
        let body = &source[start..];
        let end = body[1..]
            .find("\n    def ")
            .map(|offset| offset + 1)
            .unwrap_or(body.len());
        body[..end].to_string()
    }

    #[test]
    fn the_engine_still_names_every_contention_with_the_code_we_match_on() {
        let source = engine_residency_source();
        for raiser in ["_make_room_for", "_require_loaded", "unload_key"] {
            let body = engine_method(&source, raiser);
            assert!(
                body.contains(&format!("\"{BUSY_CODE}\"")),
                "classification reads the engine's `code` field, so this literal is the shared \
                 value the two sides agree on, not prose we happen to grep. Each of these raise \
                 sites reaches a different retry path (stage admission, a slot still loading, \
                 and the scripting teardown drain); renaming it in {raiser} makes classify_code \
                 fall through to Terminal and silently turns that path's contention into a \
                 failed job: {body}"
            );
        }
    }

    #[test]
    fn the_engine_still_names_every_out_of_memory_with_the_code_we_match_on() {
        let source = engine_source("stage.py");
        let emitted = source.matches(&format!("\"code\": \"{OOM_CODE}\"")).count();
        assert_eq!(
            emitted, 2,
            "every SSE stage funnels its failures through stage_events, which is the only place \
             that labels one as out of memory (an IngestError the oom sniffer recognises, and a \
             bare exception it recognises). classify_code routes exactly this value into the \
             degradation ladder, so renaming or losing either emit turns every OOM into a \
             terminal failure and the ladder becomes unreachable: {source}"
        );
    }

    #[test]
    fn the_engine_still_names_every_device_fault_with_the_code_we_match_on() {
        let source = engine_source("stage.py");
        let emitted = source
            .matches(&format!("\"code\": \"{DEVICE_UNAVAILABLE_CODE}\""))
            .count();
        assert_eq!(
            emitted, 2,
            "a library or device that will not load is not memory pressure. classify_code \
             routes exactly this value straight to the cpu rung; renaming or losing either \
             emit puts the fault back on the oom ladder, where every gpu rung is spent on a \
             fault no memory reduction can fix: {source}"
        );
    }

    #[test]
    fn the_oom_markers_name_the_failure_and_never_the_library() {
        let source = engine_source("oom.py");
        let markers = source
            .split("OOM_MARKERS = (")
            .nth(1)
            .expect("engine declares OOM_MARKERS")
            .split(')')
            .next()
            .expect("the marker tuple closes");
        for library in ["\"cublas\"", "\"cudnn\"", "\"cuda\"", "\"libcu"] {
            assert!(
                !markers.contains(library),
                "a marker that names a library rather than an allocation matches \
                 'Library libcublas.so.12 is not found or cannot be loaded', which is how a \
                 loader failure was reported to the user as out of memory (#114): {markers}"
            );
        }
    }

    const STAGES_READING_AN_SSE_ERROR_EVENT: [&str; 5] = [
        "align.rs",
        "layout.rs",
        "scripting.rs",
        "synthesis.rs",
        "vision.rs",
    ];

    #[test]
    fn every_stage_that_reads_an_sse_error_event_keeps_its_code() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/orchestrator");
        let flattened = ["format!(\"{", "code", "}: {", "message", "}\")"].concat();
        let mut readers: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("orchestrator sources") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("file name")
                .to_string();
            let source = std::fs::read_to_string(&path).expect("source");
            assert!(
                !source.contains(&flattened),
                "{name} flattens the engine's code back into a sentence; classification then \
                 re-derives it by substring, which is what R-AJ removed"
            );
            if source.contains("\"error\" =>") {
                assert!(
                    source.contains("OrchestratorError::from_engine_event"),
                    "{name} reads an SSE error event but does not build the error through the \
                     one constructor that carries `code` through to classify_failure"
                );
                readers.push(name);
            }
        }
        readers.sort();
        assert_eq!(
            readers, STAGES_READING_AN_SSE_ERROR_EVENT,
            "this list is the scan's own witness: a stage that stops reading error events, or a \
             new one that starts, has to be re-checked by hand rather than silently passing a \
             loop that found nothing"
        );
    }

    #[test]
    fn the_engine_reservation_grace_matches_the_engine() {
        let source = engine_residency_source();
        let declared = source
            .lines()
            .find_map(|line| line.strip_prefix("RESERVATION_GRACE_S = "))
            .expect("engine declares RESERVATION_GRACE_S")
            .trim()
            .parse::<f64>()
            .expect("engine grace parses");
        assert_eq!(
            declared,
            ENGINE_RESERVATION_GRACE.as_secs_f64(),
            "the retry budget is derived from the engine's stale-reservation grace; if the \
             engine changes it, the budget stops outlasting the window it exists to outlast"
        );
    }
}
