use crate::catalog::{Catalog, ModelTask};
use crate::hardware::HardwareProfile;

use serde::Serialize;

const REF_TOK_S: f64 = 100.0;
const REF_VRAM_MB: f64 = 24_576.0;
const REF_RAM_MB: f64 = 32_768.0;
const REF_CORES: f64 = 16.0;
const GPU_FIT_HEADROOM: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ScoreBand {
    Low,
    Medium,
    High,
    Max,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputeEstimate {
    pub compute_score: u8,
    pub band: ScoreBand,
    pub tok_s_est: Option<f64>,
}

pub fn compute_estimate(profile: &HardwareProfile, catalog: &Catalog) -> ComputeEstimate {
    let tok_s = static_tok_s(profile, catalog);
    let compute_score = compute_score(profile, tok_s);
    ComputeEstimate {
        compute_score,
        band: band(compute_score),
        tok_s_est: tok_s,
    }
}

pub fn compute_score(profile: &HardwareProfile, tok_s: Option<f64>) -> u8 {
    let vram_mb = profile
        .primary()
        .and_then(|device| device.memory_total_mb)
        .unwrap_or(0) as f64;
    let cores_factor = (f64::from(profile.cpu.cores_physical) / REF_CORES).min(1.0);
    let score = 100.0
        * (0.5 * norm(tok_s.unwrap_or(0.0), REF_TOK_S)
            + 0.3 * norm(vram_mb, REF_VRAM_MB)
            + 0.2 * norm(profile.ram.total_mb as f64 * cores_factor, REF_RAM_MB));
    score.clamp(0.0, 100.0).round() as u8
}

pub fn band(score: u8) -> ScoreBand {
    match score {
        0..=24 => ScoreBand::Low,
        25..=54 => ScoreBand::Medium,
        55..=79 => ScoreBand::High,
        _ => ScoreBand::Max,
    }
}

pub fn static_tok_s(profile: &HardwareProfile, catalog: &Catalog) -> Option<f64> {
    let vram_mb = profile.primary().and_then(|device| device.memory_total_mb);
    let llms = catalog
        .models
        .iter()
        .filter(|m| m.task == ModelTask::Llm && !m.deprecated && m.perf.is_some());

    let gpu_pick = vram_mb.and_then(|vram| {
        llms.clone()
            .filter(|m| {
                m.full_gpu_vram_mb
                    .is_some_and(|need| need as f64 <= vram as f64 * GPU_FIT_HEADROOM)
            })
            .filter_map(|m| m.perf.and_then(|p| p.tok_s_gpu))
            .fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| a.max(v)))
            })
    });
    if gpu_pick.is_some() {
        return gpu_pick;
    }

    // Deliberately `total_mb`, and deliberately not cgroup-clamped.
    //
    // R-AM's first survey recorded that this site would "benefit from the
    // cgroup fix automatically". That was wrong, and stating why matters more
    // than the site: `total_mb` is *never* cgroup-clamped by design, because
    // `HardwareProfile::fingerprint` keys the `hw_benchmark` cache off it and a
    // benchmark measuring tok/s is a CPU property that entering a cgroup does
    // not invalidate. The cgroup figures live beside it, so nothing that reads
    // `total_mb` becomes limit-aware on its own — it has to call
    // `plan::usable_ram_mb`, and this site does not.
    //
    // Left as-is on purpose. This picks a tok/s estimate for the machine score;
    // it emits no plan, so under a limit it can recommend a model that will not
    // run but it cannot overcommit memory. That is mis-recommendation, not the
    // freeze class, and it is the same carve-out the task makes for
    // `heuristic_fit` (#110). Filed rather than folded in.
    let ram_budget = profile.ram.total_mb as f64 * 0.8;
    let cpu_pick = llms
        .clone()
        .filter(|m| m.min_free_ram_mb as f64 <= ram_budget)
        .filter_map(|m| m.perf.and_then(|p| p.tok_s_cpu))
        .fold(None, |acc: Option<f64>, v| {
            Some(acc.map_or(v, |a| a.max(v)))
        });
    if cpu_pick.is_some() {
        return cpu_pick;
    }

    llms.filter(|m| m.perf.and_then(|p| p.tok_s_cpu).is_some())
        .min_by_key(|m| m.min_free_ram_mb)
        .and_then(|m| m.perf.and_then(|p| p.tok_s_cpu))
}

fn norm(value: f64, reference: f64) -> f64 {
    (value / reference).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::bundled_catalog;
    use crate::hardware::{CpuInfo, DiskInfo, RamInfo};

    fn profile(vram_mb: Option<u64>, ram_mb: u64, cores: u32) -> HardwareProfile {
        HardwareProfile {
            devices: vram_mb
                .map(|v| crate::hardware::test_device(Some(v)))
                .into_iter()
                .collect(),
            ram: RamInfo {
                total_mb: ram_mb,
                free_mb: ram_mb / 2,
                cgroup: Default::default(),
            },
            cpu: CpuInfo {
                cores_physical: cores,
                cores_logical: cores * 2,
                avx2: true,
                avx512: false,
            },
            disk: DiskInfo {
                models_free_gb: 100.0,
                cache_free_gb: 100.0,
            },
        }
    }

    #[test]
    fn cpu_only_low_ram_machine_scores_low() {
        let p = profile(None, 8192, 4);
        let est = compute_estimate(&p, bundled_catalog());
        assert_eq!(est.band, ScoreBand::Low, "score {}", est.compute_score);
    }

    #[test]
    fn four_gb_laptop_scores_medium_to_high() {
        let p = profile(Some(4096), 16384, 14);
        let est = compute_estimate(&p, bundled_catalog());
        assert!(
            est.band == ScoreBand::Medium || est.band == ScoreBand::High,
            "score {}",
            est.compute_score
        );
        assert!(est.tok_s_est.is_some());
    }

    #[test]
    fn eight_gb_desktop_scores_high() {
        let p = profile(Some(8192), 32768, 8);
        let est = compute_estimate(&p, bundled_catalog());
        assert_eq!(est.band, ScoreBand::High, "score {}", est.compute_score);
    }

    #[test]
    fn twenty_four_gb_workstation_scores_high_or_max() {
        let p = profile(Some(24576), 65536, 16);
        let est = compute_estimate(&p, bundled_catalog());
        assert!(
            est.band == ScoreBand::High || est.band == ScoreBand::Max,
            "score {}",
            est.compute_score
        );
    }

    #[test]
    fn more_hardware_never_lowers_score() {
        let small = compute_estimate(&profile(Some(4096), 8192, 4), bundled_catalog());
        let big = compute_estimate(&profile(Some(24576), 65536, 16), bundled_catalog());
        assert!(big.compute_score > small.compute_score);
    }

    #[test]
    fn static_tok_s_uses_fastest_gpu_fitting_llm() {
        let catalog = bundled_catalog();
        let p = profile(Some(24576), 65536, 16);
        let tok_s = static_tok_s(&p, catalog).unwrap();
        let expected = catalog
            .models
            .iter()
            .filter(|m| {
                m.task == ModelTask::Llm
                    && m.full_gpu_vram_mb
                        .is_some_and(|need| need as f64 <= 24576.0 * 0.8)
            })
            .filter_map(|m| m.perf.and_then(|p| p.tok_s_gpu))
            .fold(f64::MIN, f64::max);
        assert_eq!(tok_s, expected);
    }

    #[test]
    fn static_tok_s_falls_back_to_cpu_without_gpu() {
        let catalog = bundled_catalog();
        let p = profile(None, 32768, 8);
        let tok_s = static_tok_s(&p, catalog).unwrap();
        let expected = catalog
            .models
            .iter()
            .filter(|m| m.task == ModelTask::Llm && m.min_free_ram_mb as f64 <= 32768.0 * 0.8)
            .filter_map(|m| m.perf.and_then(|p| p.tok_s_cpu))
            .fold(f64::MIN, f64::max);
        assert_eq!(tok_s, expected);
    }

    #[test]
    fn static_tok_s_tiny_ram_uses_smallest_llm() {
        let catalog = bundled_catalog();
        let p = profile(None, 2048, 2);
        assert!(static_tok_s(&p, catalog).is_some());
    }

    #[test]
    fn band_boundaries() {
        assert_eq!(band(0), ScoreBand::Low);
        assert_eq!(band(24), ScoreBand::Low);
        assert_eq!(band(25), ScoreBand::Medium);
        assert_eq!(band(54), ScoreBand::Medium);
        assert_eq!(band(55), ScoreBand::High);
        assert_eq!(band(79), ScoreBand::High);
        assert_eq!(band(80), ScoreBand::Max);
        assert_eq!(band(100), ScoreBand::Max);
    }

    #[test]
    fn score_never_exceeds_100() {
        let p = profile(Some(1_000_000), 1_000_000, 128);
        let est = compute_estimate(&p, bundled_catalog());
        assert!(est.compute_score <= 100);
        assert_eq!(est.band, ScoreBand::Max);
    }
}
