use crate::hardware::device::DevicePlan;
use serde_json::Value;

pub const MIN_BATCH: u64 = 1;
pub const CPU_FALLBACK_AFTER: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeKind {
    Llm,
    Tts,
    Align,
}

pub fn degrade_plan(plan: &Value, kind: DegradeKind, consecutive_failures: i64) -> Option<Value> {
    let plan = &crate::hardware::device::upcast_plan(plan).unwrap_or_else(|_| plan.clone());
    let Ok(device) = crate::hardware::device::plan_device(plan) else {
        return Some(cpu_fallback(plan));
    };
    if !device.is_gpu() {
        return None;
    }
    if consecutive_failures >= CPU_FALLBACK_AFTER {
        return Some(cpu_fallback(plan));
    }
    match kind {
        DegradeKind::Llm => degrade_llm(plan),
        DegradeKind::Tts => degrade_tts(plan),
        DegradeKind::Align => degrade_align(plan),
    }
}

pub fn cpu_rung(plan: &Value) -> Option<Value> {
    let plan = &crate::hardware::device::upcast_plan(plan).unwrap_or_else(|_| plan.clone());
    match crate::hardware::device::plan_device(plan) {
        Ok(device) if !device.is_gpu() => None,
        _ => Some(cpu_fallback(plan)),
    }
}

fn degrade_llm(plan: &Value) -> Option<Value> {
    let layers = plan
        .get("n_gpu_layers")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if layers == 0 {
        return Some(cpu_fallback(plan));
    }
    let reduced = layers.saturating_sub((layers / 5).max(1));
    if reduced == 0 {
        return Some(cpu_fallback(plan));
    }
    let mut next = plan.clone();
    set_u64(&mut next, "n_gpu_layers", reduced);
    drop_stale_ram_figure(&mut next);
    Some(next)
}

fn degrade_tts(plan: &Value) -> Option<Value> {
    let workers = plan.get("tts_workers").and_then(Value::as_u64).unwrap_or(1);
    let batch = plan.get("batch").and_then(Value::as_u64).unwrap_or(0);
    let mut next = plan.clone();
    let mut changed = false;
    if workers > 1 {
        set_u64(&mut next, "tts_workers", (workers / 2).max(1));
        changed = true;
    }
    if batch > MIN_BATCH {
        set_u64(&mut next, "batch", (batch / 2).max(MIN_BATCH));
        changed = true;
    }
    if changed {
        Some(next)
    } else {
        Some(cpu_fallback(plan))
    }
}

fn degrade_align(plan: &Value) -> Option<Value> {
    let workers = plan
        .get("align_workers")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if workers > 1 {
        let mut next = plan.clone();
        set_u64(&mut next, "align_workers", (workers / 2).max(1));
        Some(next)
    } else {
        Some(cpu_fallback(plan))
    }
}

pub fn cpu_fallback(plan: &Value) -> Value {
    let mut next = plan.clone();
    crate::hardware::device::set_device(&mut next, DevicePlan::cpu());
    if next.get("n_gpu_layers").is_some() {
        set_u64(&mut next, "n_gpu_layers", 0);
    }
    if next.get("kv_offload").is_some() {
        if let Some(map) = next.as_object_mut() {
            map.insert("kv_offload".into(), Value::Bool(false));
        }
    }
    set_str(&mut next, "badge", "orange");
    if let Some(map) = next.as_object_mut() {
        map.remove("tok_s_est");
    }
    drop_stale_ram_figure(&mut next);
    next
}

fn drop_stale_ram_figure(plan: &mut Value) {
    if let Some(map) = plan.as_object_mut() {
        map.remove("host_ram_planned_mb");
    }
}

fn set_u64(plan: &mut Value, key: &str, value: u64) {
    if let Some(map) = plan.as_object_mut() {
        map.insert(key.into(), Value::from(value));
    }
}

fn set_str(plan: &mut Value, key: &str, value: &str) {
    if let Some(map) = plan.as_object_mut() {
        map.insert(key.into(), Value::from(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::device::PLAN_REVISION;
    use serde_json::json;

    #[test]
    fn a_degraded_plan_drops_the_planners_ram_figure() {
        let plan = json!({
            "device": "cuda",
            "n_gpu_layers": 30,
            "host_ram_planned_mb": 1_598,
        });
        let next = degrade_plan(&plan, DegradeKind::Llm, 1).expect("a rung");
        assert_eq!(
            next["n_gpu_layers"], 24,
            "the rung moved, so the premise holds"
        );
        assert!(
            next.get("host_ram_planned_mb").is_none(),
            "fewer offloaded layers means a larger host charge nobody gated; \
             carrying the stale figure would make the residency check assert \
             the load against a budget the planner never approved"
        );
    }

    #[test]
    fn the_cpu_rung_drops_the_planners_ram_figure() {
        let plan = json!({
            "device": "cuda",
            "n_gpu_layers": 30,
            "host_ram_planned_mb": 150,
        });
        let next = cpu_rung(&plan).expect("a cpu rung");
        assert_eq!(next["device"]["backend"], "cpu");
        assert!(next.get("host_ram_planned_mb").is_none());
    }

    #[test]
    fn cpu_plan_is_exhausted() {
        let plan = json!({"device": "cpu", "n_gpu_layers": 0});
        assert_eq!(degrade_plan(&plan, DegradeKind::Llm, 1), None);
        assert_eq!(degrade_plan(&plan, DegradeKind::Tts, 1), None);
    }

    #[test]
    fn an_unreadable_device_degrades_to_cpu_rather_than_exhausting_recovery() {
        let plan = json!({"device": "banana", "n_gpu_layers": 30});
        let next = degrade_plan(&plan, DegradeKind::Llm, 1).expect("a corrupt plan has a rung");
        assert_eq!(
            next["device"]["backend"], "cpu",
            "returning None here reports recovery exhausted and fails the job over a plan \
             column we could simply have stopped trusting"
        );
        assert_eq!(next["plan_revision"], PLAN_REVISION);
    }

    #[test]
    fn every_rung_leaves_the_plan_at_the_live_revision() {
        for (plan, kind) in [
            (
                json!({"device": "cuda", "n_gpu_layers": 30}),
                DegradeKind::Llm,
            ),
            (
                json!({"device": "cuda", "tts_workers": 4, "batch": 512}),
                DegradeKind::Tts,
            ),
            (
                json!({"device": "cuda", "align_workers": 4}),
                DegradeKind::Align,
            ),
        ] {
            let next = degrade_plan(&plan, kind, 1).expect("a rung");
            assert_eq!(
                next["plan_revision"], PLAN_REVISION,
                "a degraded plan is persisted, so it must not be written back as legacy"
            );
        }
    }

    #[test]
    fn missing_device_is_exhausted() {
        assert_eq!(degrade_plan(&json!({}), DegradeKind::Tts, 1), None);
    }

    #[test]
    fn the_cpu_rung_skips_the_gpu_rungs_a_device_fault_cannot_use() {
        let plan = json!({"device": "cuda", "align_workers": 4, "model_id": "whisperx-small-int8"});
        let ladder = degrade_plan(&plan, DegradeKind::Align, 1).expect("a rung");
        assert_eq!(
            ladder["align_workers"], 2,
            "the ladder halves workers first, which is right for memory pressure and useless \
             for a library that will not load"
        );
        let direct = cpu_rung(&plan).expect("a rung");
        assert_eq!(direct["device"]["backend"], "cpu");
        assert_eq!(
            direct["model_id"], "whisperx-small-int8",
            "the rung has to stay the same model or the stage re-plans onto the gpu it just left"
        );
        assert_eq!(direct["plan_revision"], PLAN_REVISION);
    }

    #[test]
    fn the_cpu_rung_is_exhausted_once_the_plan_is_already_off_the_gpu() {
        assert_eq!(
            cpu_rung(&json!({"device": "cpu", "align_workers": 1})),
            None
        );
        assert_eq!(cpu_rung(&json!({})), None);
    }

    #[test]
    fn an_unreadable_device_still_has_a_cpu_rung() {
        let next = cpu_rung(&json!({"device": "banana"})).expect("a rung");
        assert_eq!(next["device"]["backend"], "cpu");
    }

    #[test]
    fn llm_first_rung_drops_twenty_percent_of_layers() {
        let plan = json!({"device": "cuda", "n_gpu_layers": 30});
        let next = degrade_plan(&plan, DegradeKind::Llm, 1).unwrap();
        assert_eq!(next["n_gpu_layers"], 24);
        assert_eq!(next["device"]["backend"], "cuda");
    }

    #[test]
    fn llm_reduces_by_at_least_one_layer() {
        let plan = json!({"device": "cuda", "n_gpu_layers": 3});
        let next = degrade_plan(&plan, DegradeKind::Llm, 1).unwrap();
        assert_eq!(next["n_gpu_layers"], 2);
    }

    #[test]
    fn llm_at_one_layer_falls_back_to_cpu() {
        let plan = json!({"device": "cuda", "n_gpu_layers": 1, "kv_offload": true});
        let next = degrade_plan(&plan, DegradeKind::Llm, 1).unwrap();
        assert_eq!(next["device"]["backend"], "cpu");
        assert_eq!(next["n_gpu_layers"], 0);
        assert_eq!(next["kv_offload"], false);
    }

    #[test]
    fn llm_zero_layers_on_cuda_falls_back_to_cpu() {
        let plan = json!({"device": "cuda", "n_gpu_layers": 0});
        let next = degrade_plan(&plan, DegradeKind::Llm, 1).unwrap();
        assert_eq!(next["device"]["backend"], "cpu");
    }

    #[test]
    fn second_consecutive_failure_forces_cpu() {
        let plan = json!({"device": "cuda", "n_gpu_layers": 30});
        let next = degrade_plan(&plan, DegradeKind::Llm, 2).unwrap();
        assert_eq!(next["device"]["backend"], "cpu");
        assert_eq!(next["n_gpu_layers"], 0);
        let tts = json!({"device": "cuda", "tts_workers": 4});
        let next = degrade_plan(&tts, DegradeKind::Tts, 2).unwrap();
        assert_eq!(next["device"]["backend"], "cpu");
    }

    #[test]
    fn tts_first_rung_halves_workers_and_batch() {
        let plan = json!({"device": "cuda", "tts_workers": 4, "batch": 512});
        let next = degrade_plan(&plan, DegradeKind::Tts, 1).unwrap();
        assert_eq!(next["tts_workers"], 2);
        assert_eq!(next["batch"], 256);
        assert_eq!(next["device"]["backend"], "cuda");
    }

    #[test]
    fn tts_batch_floors_at_min_batch() {
        let plan = json!({"device": "cuda", "tts_workers": 2, "batch": 100});
        let next = degrade_plan(&plan, DegradeKind::Tts, 1).unwrap();
        assert_eq!(next["batch"], 50);
    }

    #[test]
    fn tts_ladder_shape_degrades_batch_from_8_to_4() {
        let plan = json!({"device": "cuda", "tts_workers": 1, "batch": 8});
        let next = degrade_plan(&plan, DegradeKind::Tts, 1).unwrap();
        assert_eq!(next["batch"], 4);
        assert_eq!(next["device"]["backend"], "cuda");
    }

    #[test]
    fn tts_with_nothing_left_to_halve_falls_back_to_cpu() {
        let plan = json!({"device": "cuda", "tts_workers": 1, "batch": 1});
        let next = degrade_plan(&plan, DegradeKind::Tts, 1).unwrap();
        assert_eq!(next["device"]["backend"], "cpu");
    }

    #[test]
    fn cpu_fallback_sets_orange_badge_and_drops_tok_s_est() {
        let plan = json!({
            "device": "cuda",
            "n_gpu_layers": 30,
            "kv_offload": true,
            "badge": "green",
            "tok_s_est": 42.5
        });
        let next = cpu_fallback(&plan);
        assert_eq!(next["device"]["backend"], "cpu");
        assert_eq!(next["badge"], "orange");
        assert!(next.get("tok_s_est").is_none());
    }

    #[test]
    fn cpu_fallback_adds_orange_badge_when_absent() {
        let plan = json!({"device": "cuda", "tts_workers": 1});
        let next = cpu_fallback(&plan);
        assert_eq!(next["badge"], "orange");
    }

    #[test]
    fn degrade_preserves_unrelated_fields() {
        let plan = json!({"device": "cuda", "n_gpu_layers": 10, "ctx": 8192, "model_id": "m"});
        let next = degrade_plan(&plan, DegradeKind::Llm, 1).unwrap();
        assert_eq!(next["ctx"], 8192);
        assert_eq!(next["model_id"], "m");
    }
}
