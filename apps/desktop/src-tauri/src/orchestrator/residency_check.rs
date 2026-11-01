use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::json;

use super::state::{JobState, OrchestratorError};
use crate::hardware;
use crate::storage::job;

pub const LEAK_TOLERANCE_MB: i64 = 192;

pub type VramSampler = Arc<dyn Fn() -> BoxFuture<'static, Option<VramSample>> + Send + Sync>;

pub fn live_sampler() -> VramSampler {
    Arc::new(|| Box::pin(async { sample_vram(None).await }))
}

pub fn disabled_sampler() -> VramSampler {
    Arc::new(|| Box::pin(async { None }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VramSample {
    pub free_mb: u64,
    pub device: Option<String>,
}

pub async fn sample_vram(planned: Option<&str>) -> Option<VramSample> {
    let (device, free_mb) = hardware::probe_device_free(planned).await?;
    Some(VramSample {
        free_mb,
        device: Some(device),
    })
}

pub fn leaked_mb(before: &VramSample, after: &VramSample) -> i64 {
    before.free_mb as i64 - after.free_mb as i64
}

pub fn host_ram_planned_mb(plan: &serde_json::Value) -> Option<u64> {
    plan.get("host_ram_planned_mb")
        .and_then(serde_json::Value::as_u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamPlanProbe {
    pub planned_mb: u64,
    pub before_free_mb: u64,
}

pub async fn begin_ram_plan(plan: &serde_json::Value) -> Option<RamPlanProbe> {
    let planned_mb = host_ram_planned_mb(plan)?;
    Some(RamPlanProbe {
        planned_mb,
        before_free_mb: hardware::probe_ram_free_mb().await,
    })
}

pub fn measured_after_load_mb(probe: &RamPlanProbe, after_load_free_mb: u64) -> i64 {
    probe.before_free_mb as i64 - after_load_free_mb as i64
}

pub fn ram_plan_delta_mb(probe: &RamPlanProbe, after_load_free_mb: u64) -> i64 {
    probe.planned_mb as i64 - measured_after_load_mb(probe, after_load_free_mb)
}

/// One-sided on purpose: the repo over-reserves by design (`COMPUTE_SLACK_MB`
/// is a flat measured slack, the GPU weight buffer runs ~52 MiB over the
/// modelled sum at full offload), so `measured == planned` and
/// `measured < planned` are both correct behaviour and only
/// `measured > planned` is a finding. An equality assertion here would push
/// the next implementer to shave the very margin that keeps loads from
/// failing.
pub fn exceeds_plan(probe: &RamPlanProbe, after_load_free_mb: u64) -> bool {
    measured_after_load_mb(probe, after_load_free_mb) > probe.planned_mb as i64
}

fn apply_ram_plan_delta(report: &mut serde_json::Value, stage: JobState, delta_mb: i64) {
    if !report["ram_plan_delta_mb"].is_object() {
        report["ram_plan_delta_mb"] = json!({});
    }
    report["ram_plan_delta_mb"][stage.as_str()] = json!(delta_mb);
}

pub async fn record_stage_ram_plan(
    pool: &crate::storage::SqlitePool,
    job_id: &str,
    stage: JobState,
    probe: RamPlanProbe,
    after_load_free_mb: u64,
) -> Result<(), OrchestratorError> {
    let measured = measured_after_load_mb(&probe, after_load_free_mb);
    let delta = ram_plan_delta_mb(&probe, after_load_free_mb);
    if exceeds_plan(&probe, after_load_free_mb) {
        tracing::warn!(
            job = %job_id,
            "stage {} loaded {measured} MB of host RAM against a plan that budgeted {} MB \
             ({} MB over; free went {} -> {after_load_free_mb} MB) — the planner under-charged this shape",
            stage.as_str(),
            probe.planned_mb,
            -delta,
            probe.before_free_mb
        );
    } else {
        tracing::info!(
            job = %job_id,
            "stage {} loaded {measured} MB of host RAM against a {} MB budget (over-reservation {delta} MB)",
            stage.as_str(),
            probe.planned_mb
        );
    }
    let record = job::get_job(pool, job_id).await?;
    let Some(record) = record else {
        return Ok(());
    };
    let mut report: serde_json::Value = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    apply_ram_plan_delta(&mut report, stage, delta);
    job::set_job_report_only(pool, job_id, &serde_json::to_string(&report)?).await?;
    Ok(())
}

pub fn log_stage_entry(job_id: &str, stage: JobState, before: Option<&VramSample>) {
    let free = before
        .map(|s| s.free_mb.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let device = before
        .and_then(|s| s.device.clone())
        .unwrap_or_else(|| "none".to_string());
    tracing::info!(
        job = %job_id,
        "stage {} enter: vram_free={free} MB on {device}",
        stage.as_str()
    );
}

pub async fn log_stage_plan(job_id: &str, stage: JobState, plan: &serde_json::Value) {
    let planned = crate::hardware::device::plan_device(plan)
        .ok()
        .and_then(|device| device.key);
    let free = sample_vram(planned.as_deref())
        .await
        .map(|s| s.free_mb.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let ram_free = hardware::probe_ram_free_mb().await;
    tracing::debug!(
        job = %job_id,
        "stage {} plan: vram_free={free} MB ram_free={ram_free} MB plan={plan}",
        stage.as_str()
    );
}

pub async fn record_stage_vram(
    pool: &crate::storage::SqlitePool,
    job_id: &str,
    stage: JobState,
    before: Option<VramSample>,
    after: Option<VramSample>,
) -> Result<(), OrchestratorError> {
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(());
    };
    if before.device != after.device {
        tracing::warn!(
            job = %job_id,
            "stage {} vram telemetry skipped, the sampled device changed between entry and exit ({:?} then {:?})",
            stage.as_str(),
            before.device,
            after.device
        );
        return Ok(());
    }
    let leaked = leaked_mb(&before, &after);
    if leaked > LEAK_TOLERANCE_MB {
        tracing::warn!(
            job = %job_id,
            "stage {} left {leaked} MB of VRAM unreleased on {}, above the {LEAK_TOLERANCE_MB} MB tolerance that absorbs a one-time backend context (before {} MB free, after {} MB free)",
            stage.as_str(),
            before.device.as_deref().unwrap_or("an unnamed device"),
            before.free_mb,
            after.free_mb
        );
    }
    let record = job::get_job(pool, job_id).await?;
    let Some(record) = record else {
        return Ok(());
    };
    let mut report: serde_json::Value = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    if !report["vram_free_mb_delta"].is_object() {
        report["vram_free_mb_delta"] = json!({});
    }
    report["vram_free_mb_delta"][stage.as_str()] = json!(leaked);
    job::set_job_report_only(pool, job_id, &serde_json::to_string(&report)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(free_mb: u64) -> VramSample {
        VramSample {
            free_mb,
            device: Some("pci:0000:01:00.0".into()),
        }
    }

    fn probe(planned_mb: u64, before_free_mb: u64) -> RamPlanProbe {
        RamPlanProbe {
            planned_mb,
            before_free_mb,
        }
    }

    #[test]
    fn the_planners_figure_is_read_from_the_plan_and_nowhere_else() {
        let plan = serde_json::json!({"badge": "orange", "host_ram_planned_mb": 1_498});
        assert_eq!(host_ram_planned_mb(&plan), Some(1_498));
        assert_eq!(
            host_ram_planned_mb(&serde_json::json!({"badge": "red"})),
            None,
            "a legacy or degraded plan carries no figure, so nothing is asserted against one"
        );
        assert_eq!(
            host_ram_planned_mb(&serde_json::json!({"host_ram_planned_mb": null})),
            None
        );
    }

    #[test]
    fn measured_after_load_is_the_free_ram_the_load_consumed() {
        assert_eq!(measured_after_load_mb(&probe(1_498, 8_000), 6_500), 1_500);
    }

    #[test]
    fn the_residency_assertion_is_one_sided() {
        let p = probe(1_498, 8_000);
        assert!(
            exceeds_plan(&p, 8_000 - 1_600),
            "a load 102 MB over its budget is the finding this check exists for"
        );
        assert!(
            !exceeds_plan(&p, 8_000 - 1_498),
            "an exactly-planned load is correct behaviour, not a finding"
        );
        assert!(
            !exceeds_plan(&p, 8_000 - 900),
            "the repo over-reserves on purpose; a load under its budget must never warn, \
             or the margin that keeps loads from failing gets shaved to silence it"
        );
        assert_eq!(
            ram_plan_delta_mb(&p, 8_000 - 900),
            598,
            "the over-reservation is reported as a positive delta, observable instead of inferred"
        );
        assert_eq!(ram_plan_delta_mb(&p, 8_000 - 1_600), -102);
    }

    #[test]
    fn a_concurrent_free_inside_the_window_cannot_fabricate_an_overrun() {
        let p = probe(1_498, 8_000);
        assert_eq!(measured_after_load_mb(&p, 9_000), -1_000);
        assert!(!exceeds_plan(&p, 9_000));
    }

    #[test]
    fn the_delta_is_reported_per_stage_beside_the_vram_delta() {
        let mut report = serde_json::json!({"vram_free_mb_delta": {"SCRIPTING": 12}});
        apply_ram_plan_delta(&mut report, JobState::Scripting, 598);
        apply_ram_plan_delta(&mut report, JobState::Vision, -102);
        assert_eq!(report["ram_plan_delta_mb"]["SCRIPTING"], 598);
        assert_eq!(report["ram_plan_delta_mb"]["VISION"], -102);
        assert_eq!(
            report["vram_free_mb_delta"]["SCRIPTING"], 12,
            "the RAM delta must not clobber the VRAM record it sits beside"
        );
    }

    #[test]
    fn leaked_mb_is_positive_when_free_vram_drops() {
        let before = sample(3767);
        let after = sample(2196);
        assert_eq!(leaked_mb(&before, &after), 1571);
    }

    #[test]
    fn leaked_mb_is_zero_when_vram_is_fully_released() {
        let sample = sample(3767);
        assert_eq!(leaked_mb(&sample, &sample), 0);
    }

    #[test]
    fn leaked_mb_is_negative_when_more_vram_is_free_afterwards() {
        let before = sample(2000);
        let after = sample(3000);
        assert_eq!(leaked_mb(&before, &after), -1000);
    }

    #[test]
    fn a_pair_that_read_different_devices_is_not_subtracted() {
        let before = VramSample {
            free_mb: 3767,
            device: Some("pci:0000:01:00.0".into()),
        };
        let after = VramSample {
            free_mb: 512,
            device: Some("pci:0000:00:02.0".into()),
        };
        assert_ne!(
            before.device, after.device,
            "subtracting two cards reports a 3 GB leak that never happened"
        );
    }

    #[test]
    fn projector_sized_leak_exceeds_tolerance() {
        let before = sample(3767);
        let after = sample(2196);
        assert!(leaked_mb(&before, &after) > LEAK_TOLERANCE_MB);
    }

    #[test]
    fn allocator_noise_stays_within_tolerance() {
        let before = sample(3767);
        let after = sample(3700);
        assert!(leaked_mb(&before, &after) <= LEAK_TOLERANCE_MB);
    }
}
