use serde_json::{json, Value};

use super::state::OrchestratorError;
use crate::catalog::gguf::GgufModelInfo;
use crate::catalog::{Catalog, CatalogEntry, ModelTask};
use crate::hardware::plan;
use crate::storage::models::ModelInstall;

pub const FALLBACK_CTX: u64 = 4_096;
pub const FALLBACK_BATCH: u64 = 256;
pub const FALLBACK_N_LAYERS: u32 = 32;

/// Batch rungs for the fallback plan, largest first.
///
/// Batch is the fallback's only remaining lever, and that is not arbitrary: the
/// planner's ctx ladder bottoms out at `CTX_LADDER`'s smallest rung — the same
/// 4096 `FALLBACK_CTX` uses — and it only ever tries `DEFAULT_BATCH` (512). So
/// by the time control reaches here every ctx has already been refused at batch
/// 512, and no batch below 512 has been tried at all. Re-walking ctx would
/// re-ask a question that was just answered; walking batch asks a new one.
///
/// Shrinking batch first is also the order the spec ranks. HARDWARE_PROFILER §4
/// measured that `n_batch` scales the compute buffer directly (893 MB → 510 MB
/// → 351 MB at 512 → 256 → 128) and states plainly that "reducing batch is a
/// cheaper degradation step than reducing ctx — it costs prompt-processing
/// throughput only, while ctx costs chunk coherence".
///
/// Both rungs are values that table actually ran. 128 is its smallest, so the
/// ladder stops there rather than inventing an operating point nobody has
/// measured; below it the charge is dominated by weights plus the flat
/// `COMPUTE_SLACK_MB`, neither of which shrinks at all.
pub const FALLBACK_BATCH_LADDER: [u64; 2] = [FALLBACK_BATCH, 128];

pub fn override_llm_id(model_overrides_json: Option<&str>) -> Option<String> {
    let raw = model_overrides_json?;
    let value: Value = serde_json::from_str(raw).ok()?;
    value
        .get("llm")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn resolve_llm_id(
    model_overrides_json: Option<&str>,
    catalog: &Catalog,
    installs: &[ModelInstall],
) -> Result<String, OrchestratorError> {
    if let Some(id) = override_llm_id(model_overrides_json) {
        return Ok(id);
    }
    let mut candidates: Vec<&CatalogEntry> = catalog
        .models
        .iter()
        .filter(|entry| entry.task == ModelTask::Llm)
        .filter(|entry| installs.iter().any(|i| i.model_id == entry.id))
        .collect();
    candidates.sort_by_key(|entry| (entry.deprecated, std::cmp::Reverse(entry.quality_score)));
    candidates
        .first()
        .map(|entry| entry.id.clone())
        .ok_or_else(|| OrchestratorError::ModelNotInstalled("<any llm>".to_string()))
}

/// The last-resort full-CPU plan, charged against `gate_mb` before it is
/// emitted.
///
/// This used to read no memory figure of any kind — it was a `json!` literal
/// over three constants — while its caller stamped `badge: "red"` over the
/// result. A plan stamped red has to be red because of a number, and this is
/// where that number comes from. It is the one path the planner reaches *after*
/// concluding nothing fits, so it was also the one path running with no budget
/// check at all.
///
/// The three outcomes TASKS.md requires of the gate, in order:
///
/// 1. the plan fits under the ceiling — emit it, Orange;
/// 2. it does not, but a smaller one does — emit the largest batch rung that
///    fits, Orange;
/// 3. nothing fits, or there is no header to charge — emit the smallest rung
///    anyway, Red.
///
/// Rung 3 emits rather than refuses on purpose: ARCHITECTURE §6.1 invariant 1
/// and AGENTS §5a forbid a memory figure making a model unavailable, so the
/// badge and the confirm dialog are the stop, never this function.
pub fn fallback_cpu_plan(model_id: &str, gguf: Option<&GgufModelInfo>, gate_mb: u64) -> Value {
    let n_layers = gguf.map_or(FALLBACK_N_LAYERS, |info| info.n_layers.max(1));

    let charge = |batch: u64| {
        gguf.map(|info| crate::hardware::plan::cpu_ram_charge(info, FALLBACK_CTX, batch).total())
    };
    // Rungs 1 and 2 collapse into one search: the largest rung that fits is the
    // plan, whether or not the first rung was it.
    let fitting = FALLBACK_BATCH_LADDER
        .iter()
        .copied()
        .find(|&batch| charge(batch).is_some_and(|bytes| plan::fits_ram(bytes, gate_mb)));

    let smallest = *FALLBACK_BATCH_LADDER
        .last()
        .expect("FALLBACK_BATCH_LADDER is never empty");
    let (batch, badge) = match fitting {
        Some(batch) => (batch, "orange"),
        None => (smallest, "red"),
    };

    if badge == "red" {
        // Rung 3 is the case the 🔴 confirm dialog exists for, so say why on the
        // way past rather than leaving the badge to be inferred.
        match charge(smallest) {
            Some(bytes) => tracing::warn!(
                "{model_id} fallback plan charges {} MB of host RAM at ctx \
                 {FALLBACK_CTX} batch {smallest}, against a {gate_mb} MB gate; \
                 emitting it red — it is expected to exceed available memory",
                bytes / (1024 * 1024)
            ),
            None => tracing::warn!(
                "{model_id} has no readable GGUF header, so the fallback plan \
                 cannot be charged against the {gate_mb} MB gate; emitting it red"
            ),
        }
    }

    let mut plan = json!({
        "model_id": model_id,
        "plan_revision": crate::hardware::device::PLAN_REVISION,
        "device": crate::hardware::device::DevicePlan::cpu(),
        "n_gpu_layers": 0,
        "n_layers_total": n_layers,
        "kv_offload": false,
        "ctx": FALLBACK_CTX,
        "batch": batch,
        "badge": badge,
    });
    if let Some(bytes) = charge(batch) {
        plan["host_ram_planned_mb"] = json!(bytes.div_ceil(1024 * 1024));
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::bundled_catalog;
    use crate::storage::models::ModelInstall;

    fn install(model_id: &str) -> ModelInstall {
        ModelInstall {
            model_id: model_id.to_string(),
            path: format!("llm/{model_id}"),
            sha256: "deadbeef".into(),
            disk_bytes: 1,
            source: "curated".into(),
            installed_at: 0,
        }
    }

    #[test]
    fn override_wins_over_installed_models() {
        let id = resolve_llm_id(
            Some(r#"{"llm": "gemma4-e2b-q4km"}"#),
            bundled_catalog(),
            &[install("qwen3.5-9b-instruct-q4km")],
        )
        .expect("resolved");
        assert_eq!(id, "gemma4-e2b-q4km");
    }

    #[test]
    fn best_installed_llm_by_quality_wins() {
        let id = resolve_llm_id(
            None,
            bundled_catalog(),
            &[
                install("phi4-mini-q4km"),
                install("qwen3.5-9b-instruct-q4km"),
                install("gemma4-e2b-q4km"),
                install("kokoro-82m"),
            ],
        )
        .expect("resolved");
        assert_eq!(id, "qwen3.5-9b-instruct-q4km");
    }

    #[test]
    fn deprecated_llm_loses_to_any_live_installed_llm() {
        let id = resolve_llm_id(
            None,
            bundled_catalog(),
            &[
                install("qwen3.5-32b-class-q4km"),
                install("gemma4-e2b-q4km"),
            ],
        )
        .expect("resolved");
        assert_eq!(id, "gemma4-e2b-q4km");
    }

    #[test]
    fn existing_install_of_a_deprecated_llm_still_resolves_when_it_is_the_only_one() {
        let id = resolve_llm_id(
            None,
            bundled_catalog(),
            &[install("qwen3.5-32b-class-q4km")],
        )
        .expect("resolved");
        assert_eq!(id, "qwen3.5-32b-class-q4km");
    }

    #[test]
    fn non_llm_installs_are_ignored() {
        let err = resolve_llm_id(None, bundled_catalog(), &[install("kokoro-82m")]).unwrap_err();
        assert!(matches!(err, OrchestratorError::ModelNotInstalled(_)));
    }

    #[test]
    fn no_installed_llm_is_model_not_installed() {
        let err = resolve_llm_id(None, bundled_catalog(), &[]).unwrap_err();
        assert!(matches!(err, OrchestratorError::ModelNotInstalled(_)));
    }

    #[test]
    fn malformed_overrides_fall_back_to_auto() {
        let id = resolve_llm_id(
            Some("not json"),
            bundled_catalog(),
            &[install("phi4-mini-q4km")],
        )
        .expect("resolved");
        assert_eq!(id, "phi4-mini-q4km");
    }

    #[test]
    fn overrides_without_llm_key_fall_back_to_auto() {
        let id = resolve_llm_id(
            Some(r#"{"tts": "kokoro-82m"}"#),
            bundled_catalog(),
            &[install("gemma4-e2b-q4km")],
        )
        .expect("resolved");
        assert_eq!(id, "gemma4-e2b-q4km");
    }

    #[test]
    fn fallback_cpu_plan_satisfies_engine_contract() {
        let plan = fallback_cpu_plan("phi4-mini-q4km", None, gate(16_384));
        assert_eq!(plan["model_id"], "phi4-mini-q4km");
        assert_eq!(plan["device"]["backend"], "cpu");
        assert_eq!(plan["n_gpu_layers"], 0);
        assert!(plan["n_layers_total"].as_u64().unwrap() >= 1);
        assert!(plan["ctx"].as_u64().unwrap() >= 512);
        assert!(plan["batch"].as_u64().unwrap() >= 1);
    }

    // -----------------------------------------------------------------
    // R-AM — the fallback is charged before it is emitted.
    //
    // One model across all three rungs, so the only thing that moves
    // between them is the machine. With `n_vocab = 151_936` and
    // `kv_bytes_per_token = 0`:
    //
    //   weights      = 1000 MiB
    //   compute(256) = 448 + 148.375 = 596.375 MiB   → charge 1596.375 MiB
    //   compute(128) = 448 +  74.188 = 522.188 MiB   → charge 1522.188 MiB
    //
    // so a gate placed above 1596, between the two, and below 1522 selects
    // rung 1, rung 2 and rung 3 respectively. The free-memory figures that
    // produce those gates are 3072 / 2560 / 2048 MB: all three sit below
    // `OS_RESERVE_MB / (1 - RAM_HEADROOM)` = 5120 MB, so the absolute reserve is
    // the binding term and `gate = free - 1024`. They were 2048 / 1950 / 1024
    // when the gate was `0.8 * free`. KV is zeroed on purpose: ctx is
    // fixed at `FALLBACK_CTX` on every rung, so a KV term would be a constant
    // added to all three and would only obscure which lever is moving.
    // -----------------------------------------------------------------

    const MIB: u64 = 1024 * 1024;

    /// The gate for a machine with `free_mb` free, derived from `ram_gate_mb`
    /// rather than written as a literal so these fixtures keep testing the
    /// planner's ceiling when that function gains its remaining terms.
    fn gate(free_mb: u64) -> u64 {
        plan::ram_gate_mb(&crate::hardware::RamInfo {
            total_mb: free_mb * 2,
            free_mb,
            cgroup: Default::default(),
        })
    }

    fn model() -> GgufModelInfo {
        GgufModelInfo {
            n_layers: 10,
            layer_bytes: vec![90 * MIB; 10],
            non_layer_bytes: 100 * MIB,
            output_bytes: 50 * MIB,
            kv_bytes_per_token: 0,
            n_vocab: 151_936,
        }
    }

    /// The arithmetic the three rung fixtures below stand on, asserted so they
    /// cannot rot into proving nothing if a constant moves.
    #[test]
    fn the_rung_fixtures_straddle_the_two_batch_rungs() {
        let info = model();
        assert_eq!(info.total_bytes(), 1_000 * MIB, "weights");
        let at = |batch| plan::cpu_ram_charge(&info, FALLBACK_CTX, batch).total();
        assert_eq!(FALLBACK_BATCH_LADDER, [256, 128]);
        assert!(
            at(128) < at(256),
            "the ladder must actually shrink the charge"
        );

        // Rung 1 clears the larger rung; rung 2 clears only the smaller; rung 3
        // clears neither. If any of these three premises breaks, the tests below
        // are testing a different rung than their names claim.
        assert!(at(256) <= gate(3_072) * MIB);
        assert!(at(256) > gate(2_560) * MIB && at(128) <= gate(2_560) * MIB);
        assert!(at(128) > gate(2_048) * MIB);
    }

    /// Rung 1 — the plan fits as it stands. Unchanged behaviour, pinned so the
    /// gate cannot start refusing plans that were always fine.
    #[test]
    fn a_fallback_that_fits_is_emitted_at_the_full_batch() {
        let plan = fallback_cpu_plan("m", Some(&model()), gate(3_072));
        assert_eq!(plan["batch"], FALLBACK_BATCH);
        assert_eq!(plan["ctx"], FALLBACK_CTX);
        assert_eq!(plan["badge"], "orange");
    }

    /// Box 3's other half at this site: the number the badge was decided on
    /// rides the plan, so the residency check can assert the load against the
    /// planner's own figure instead of recomputing one that could drift.
    /// 1596.375 MiB rounds up, never down — a budget floored below the charge
    /// would false-warn on an exactly-planned load.
    #[test]
    fn the_fallback_carries_the_charge_it_was_gated_on() {
        let plan = fallback_cpu_plan("m", Some(&model()), gate(3_072));
        assert_eq!(plan["badge"], "orange");
        assert_eq!(plan["host_ram_planned_mb"], 1_597);
    }

    /// A red plan is *expected* to exceed the gate, and that is exactly why its
    /// figure matters most: it is the number the 🔴 dialog's warning is about.
    #[test]
    fn a_red_fallback_still_states_its_expected_consumption() {
        let plan = fallback_cpu_plan("m", Some(&model()), gate(2_048));
        assert_eq!(plan["badge"], "red");
        assert_eq!(plan["host_ram_planned_mb"], 1_523);
    }

    /// No header, no charge, no figure — the residency check skips rather than
    /// asserting against a number nobody computed.
    #[test]
    fn a_headerless_fallback_carries_no_figure_to_assert_against() {
        let plan = fallback_cpu_plan("m", None, gate(8_192));
        assert!(plan.get("host_ram_planned_mb").is_none());
    }

    /// Rung 2 — it does not fit, but a smaller one does, so it shrinks instead
    /// of being emitted over the ceiling. On `origin/main` this machine got the
    /// batch-256 plan, 1596 MiB of charge against a 1536 MiB gate, unexamined.
    #[test]
    fn a_fallback_that_does_not_fit_shrinks_to_the_batch_that_does() {
        let plan = fallback_cpu_plan("m", Some(&model()), gate(2_560));
        assert_eq!(plan["batch"], 128, "shrunk to the largest rung that fits");
        assert_eq!(plan["ctx"], FALLBACK_CTX, "ctx was already at the bottom");
        assert_eq!(plan["badge"], "orange", "the emitted plan does fit");
    }

    /// Rung 3 — nothing fits. The plan is still emitted, because refusing a
    /// model over a memory figure is forbidden, and it is badged red because a
    /// number said so.
    #[test]
    fn a_fallback_that_cannot_fit_at_all_is_emitted_red_at_the_smallest_rung() {
        let plan = fallback_cpu_plan("m", Some(&model()), gate(2_048));
        assert_eq!(plan["batch"], 128, "the smallest rung, emitted anyway");
        assert_eq!(plan["badge"], "red");
        assert_eq!(plan["device"]["backend"], "cpu", "still runnable");
        assert_eq!(plan["n_layers_total"], 10);
    }

    /// A gate of zero is rung 3's limit case and must not become a refusal:
    /// TASKS.md clamps `gate_mb` at 0 precisely so that a machine with less free
    /// memory than the OS reserve still gets a runnable plan.
    #[test]
    fn a_zero_gate_still_emits_a_runnable_plan() {
        let plan = fallback_cpu_plan("m", Some(&model()), 0);
        assert_eq!(plan["badge"], "red");
        assert_eq!(plan["device"]["backend"], "cpu");
        assert!(plan["batch"].as_u64().unwrap() >= 1);
        assert!(plan["ctx"].as_u64().unwrap() >= 512);
    }

    /// No GGUF header means no byte count, so the gate cannot evaluate this plan
    /// at all. That is red, not orange: the badge says "unverified" in the only
    /// direction that is safe to be wrong in.
    #[test]
    fn a_fallback_with_no_header_to_charge_is_red_even_on_a_roomy_machine() {
        let plan = fallback_cpu_plan("m", None, gate(64_000));
        assert_eq!(plan["badge"], "red");
        assert_eq!(plan["n_layers_total"], FALLBACK_N_LAYERS);
    }

    /// The property, over a grid: an emitted badge is never a guess. Orange
    /// implies the emitted plan's own charge clears the gate; red implies it
    /// does not. This is what "no path emits a plan the gate has not seen"
    /// means, stated so that any future rung has to satisfy it too.
    #[test]
    fn the_emitted_badge_always_matches_the_emitted_plans_charge() {
        let mut orange = 0;
        let mut red = 0;
        for free_mb in [0u64, 512, 1_024, 1_600, 1_950, 2_048, 4_096, 32_768] {
            for (n_vocab, kv_per_tok, layer_mb) in [
                (0u64, 0u64, 90u64),
                (151_936, 0, 90),
                (151_936, 128_000, 30),
            ] {
                let info = GgufModelInfo {
                    n_layers: 10,
                    layer_bytes: vec![layer_mb * MIB; 10],
                    non_layer_bytes: 100 * MIB,
                    output_bytes: 50 * MIB,
                    kv_bytes_per_token: kv_per_tok,
                    n_vocab,
                };
                let plan = fallback_cpu_plan("m", Some(&info), gate(free_mb));
                let batch = plan["batch"].as_u64().expect("batch");
                let ctx = plan["ctx"].as_u64().expect("ctx");
                let charge = plan::cpu_ram_charge(&info, ctx, batch).total();
                let fits = plan::fits_ram(charge, gate(free_mb));
                match plan["badge"].as_str() {
                    Some("orange") => {
                        orange += 1;
                        assert!(
                            fits,
                            "orange plan charges {charge} B against a {} MB gate \
                             (free {free_mb}, vocab {n_vocab}, kv/tok {kv_per_tok})",
                            gate(free_mb)
                        );
                    }
                    Some("red") => {
                        red += 1;
                        assert!(
                            !fits,
                            "red plan at {charge} B actually fits the {} MB gate \
                             (free {free_mb}, vocab {n_vocab}, kv/tok {kv_per_tok}) \
                             — red must mean a number said so",
                            gate(free_mb)
                        );
                        assert_eq!(batch, 128, "red is emitted at the smallest rung");
                    }
                    other => panic!("unexpected badge {other:?}"),
                }
                assert_eq!(ctx, FALLBACK_CTX);
            }
        }
        // Without both outcomes present the assertions above are satisfiable by
        // a constant badge.
        assert!(orange > 0 && red > 0, "grid covered only one outcome");
    }
}
