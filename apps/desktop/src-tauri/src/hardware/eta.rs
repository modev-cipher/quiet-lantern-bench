use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::hardware::learned::LearnedCoefficients;

const NARRATOR_TOKENS_PER_WORD: f64 = 1.4;

pub const FALLBACK_TOK_S: f64 = 20.0;
pub const FALLBACK_RTF_GPU: f64 = 0.1;
pub const FALLBACK_RTF_CPU: f64 = 1.0;
const TEACHER_EXPANSION: f64 = 2.2;
const WORDS_PER_AUDIO_SECOND: f64 = 2.6;
const RTF_ALIGN_CPU_DEFAULT: f64 = 0.07;
const RENDER_FACTOR_MP3: f64 = 0.02;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobMode {
    Narrator,
    Teacher,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EtaInputs {
    pub mode: JobMode,
    pub words: u64,
    pub tok_s_est: f64,
    pub tts_rtf: f64,
    pub tts_workers: u32,
    #[serde(default)]
    pub align_rtf: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EtaBreakdown {
    pub script_s: f64,
    pub tts_s: f64,
    pub align_s: f64,
    pub render_s: f64,
    pub total_s: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EtaRequest {
    pub mode: JobMode,
    pub words: u64,
    pub voice_engine: String,
    #[serde(default)]
    pub llm_model_id: Option<String>,
    #[serde(default = "one")]
    pub tts_workers: u32,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EtaDevices {
    pub llm_key: String,
    pub llm_gpu: bool,
    pub tts_key: String,
    pub tts_gpu: bool,
}

impl EtaDevices {
    pub fn cpu() -> Self {
        EtaDevices {
            llm_key: "cpu".into(),
            llm_gpu: false,
            tts_key: "cpu".into(),
            tts_gpu: false,
        }
    }

    pub fn resolve(
        req: &EtaRequest,
        profile: &crate::hardware::HardwareProfile,
        catalog: &Catalog,
        torch_capable: bool,
    ) -> Self {
        let resolve_one = |id: &str| {
            catalog
                .models
                .iter()
                .find(|model| model.id == id)
                .map(|entry| crate::hardware::plan::planned_device(entry, profile))
        };
        let resolve_llm = |id: &str| {
            catalog
                .models
                .iter()
                .find(|model| model.id == id)
                .map(|entry| {
                    if entry.format == crate::catalog::ModelFormat::Gguf {
                        crate::hardware::plan::device_plan(crate::hardware::plan::capable_device(
                            entry, profile,
                        ))
                    } else {
                        crate::hardware::plan::planned_device(entry, profile)
                    }
                })
        };
        let mut devices = EtaDevices::cpu();
        if let Some(device) = resolve_one(&req.voice_engine) {
            if device.is_gpu() && torch_capable {
                devices.tts_gpu = true;
                devices.tts_key = device.learned_key();
            }
        }
        if let Some(device) = req.llm_model_id.as_deref().and_then(resolve_llm) {
            devices.llm_gpu = device.is_gpu();
            devices.llm_key = device.learned_key();
        }
        devices
    }
}

fn catalog_tok_s(catalog: &Catalog, model_id: &str, has_gpu: bool) -> Option<f64> {
    catalog
        .models
        .iter()
        .find(|m| m.id == model_id)
        .and_then(|m| m.perf)
        .and_then(|p| if has_gpu { p.tok_s_gpu } else { p.tok_s_cpu })
}

fn catalog_rtf(catalog: &Catalog, model_id: &str, has_gpu: bool) -> Option<f64> {
    catalog
        .models
        .iter()
        .find(|m| m.id == model_id)
        .and_then(|m| m.perf)
        .and_then(|p| if has_gpu { p.rtf_gpu } else { p.rtf_cpu })
}

pub fn resolve_inputs(
    req: &EtaRequest,
    devices: &EtaDevices,
    learned: &LearnedCoefficients,
    catalog: &Catalog,
    static_tok_s: Option<f64>,
) -> EtaInputs {
    let tok_s_est = req
        .llm_model_id
        .as_deref()
        .and_then(|id| learned.llm_tok_s(id, &devices.llm_key))
        .or_else(|| {
            req.llm_model_id
                .as_deref()
                .and_then(|id| catalog_tok_s(catalog, id, devices.llm_gpu))
        })
        .or(static_tok_s)
        .unwrap_or(FALLBACK_TOK_S);

    let tts_rtf = learned
        .tts_rtf(&req.voice_engine, &devices.tts_key)
        .or_else(|| catalog_rtf(catalog, &req.voice_engine, devices.tts_gpu))
        .unwrap_or(if devices.tts_gpu {
            FALLBACK_RTF_GPU
        } else {
            FALLBACK_RTF_CPU
        });

    EtaInputs {
        mode: req.mode,
        words: req.words,
        tok_s_est,
        tts_rtf,
        tts_workers: req.tts_workers.max(1),
        align_rtf: None,
    }
}

pub fn estimate_eta(inputs: &EtaInputs) -> EtaBreakdown {
    let words = inputs.words as f64;
    let est_tokens = match inputs.mode {
        JobMode::Narrator => words * NARRATOR_TOKENS_PER_WORD,
        JobMode::Teacher => words * NARRATOR_TOKENS_PER_WORD * TEACHER_EXPANSION,
    };
    let script_words = match inputs.mode {
        JobMode::Narrator => words,
        JobMode::Teacher => words * TEACHER_EXPANSION,
    };
    let est_audio_s = script_words / WORDS_PER_AUDIO_SECOND;

    let script_s = if inputs.tok_s_est > 0.0 {
        est_tokens / inputs.tok_s_est
    } else {
        0.0
    };
    let workers = f64::from(inputs.tts_workers.max(1));
    let tts_s = est_audio_s * inputs.tts_rtf.max(0.0) / workers;
    let align_s = est_audio_s * inputs.align_rtf.unwrap_or(RTF_ALIGN_CPU_DEFAULT).max(0.0);
    let render_s = est_audio_s * RENDER_FACTOR_MP3;

    EtaBreakdown {
        script_s,
        tts_s,
        align_s,
        render_s,
        total_s: script_s + tts_s + align_s + render_s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::bundled_catalog;

    fn inputs(mode: JobMode) -> EtaInputs {
        EtaInputs {
            mode,
            words: 26_000,
            tok_s_est: 40.0,
            tts_rtf: 0.03,
            tts_workers: 4,
            align_rtf: None,
        }
    }

    fn request() -> EtaRequest {
        EtaRequest {
            mode: JobMode::Narrator,
            words: 10_000,
            voice_engine: "kokoro-82m".into(),
            llm_model_id: Some("phi4-mini-q4km".into()),
            tts_workers: 1,
        }
    }

    fn on_card(key: &str) -> EtaDevices {
        EtaDevices {
            llm_key: key.into(),
            llm_gpu: true,
            tts_key: key.into(),
            tts_gpu: true,
        }
    }

    #[test]
    fn resolve_prefers_learned_over_catalog() {
        let mut learned = LearnedCoefficients::default();
        learned.record_tts_rtf("kokoro-82m", "pci:0000:01:00.0", 0.5);
        learned.record_llm_tok_s("phi4-mini-q4km", "pci:0000:01:00.0", 33.0);
        let resolved = resolve_inputs(
            &request(),
            &on_card("pci:0000:01:00.0"),
            &learned,
            bundled_catalog(),
            Some(99.0),
        );
        assert!((resolved.tts_rtf - 0.5).abs() < 1e-9);
        assert!((resolved.tok_s_est - 33.0).abs() < 1e-9);
    }

    #[test]
    fn resolve_falls_back_to_catalog_when_unlearned() {
        let learned = LearnedCoefficients::default();
        let catalog = bundled_catalog();
        let resolved = resolve_inputs(
            &request(),
            &on_card("pci:0000:01:00.0"),
            &learned,
            catalog,
            Some(99.0),
        );
        let cat_rtf = catalog_rtf(catalog, "kokoro-82m", true).unwrap();
        let cat_tok = catalog_tok_s(catalog, "phi4-mini-q4km", true).unwrap();
        assert!((resolved.tts_rtf - cat_rtf).abs() < 1e-9);
        assert!((resolved.tok_s_est - cat_tok).abs() < 1e-9);
    }

    #[test]
    fn resolve_uses_static_tok_s_when_no_llm_model() {
        let learned = LearnedCoefficients::default();
        let req = EtaRequest {
            llm_model_id: None,
            ..request()
        };
        let resolved = resolve_inputs(
            &req,
            &on_card("pci:0000:01:00.0"),
            &learned,
            bundled_catalog(),
            Some(42.0),
        );
        assert!((resolved.tok_s_est - 42.0).abs() < 1e-9);
    }

    #[test]
    fn resolve_uses_hard_fallbacks_when_nothing_known() {
        let learned = LearnedCoefficients::default();
        let req = EtaRequest {
            voice_engine: "unknown-tts".into(),
            llm_model_id: None,
            ..request()
        };
        let resolved = resolve_inputs(&req, &EtaDevices::cpu(), &learned, bundled_catalog(), None);
        assert!((resolved.tts_rtf - FALLBACK_RTF_CPU).abs() < 1e-9);
        assert!((resolved.tok_s_est - FALLBACK_TOK_S).abs() < 1e-9);
    }

    #[test]
    fn resolve_selects_cpu_device_coefficients_without_gpu() {
        let mut learned = LearnedCoefficients::default();
        learned.record_tts_rtf("kokoro-82m", "cpu", 0.9);
        learned.record_tts_rtf("kokoro-82m", "pci:0000:01:00.0", 0.02);
        let resolved = resolve_inputs(
            &request(),
            &EtaDevices::cpu(),
            &learned,
            bundled_catalog(),
            None,
        );
        assert!((resolved.tts_rtf - 0.9).abs() < 1e-9);
    }

    #[test]
    fn two_cards_do_not_blend_into_one_average() {
        let mut learned = LearnedCoefficients::default();
        learned.record_tts_rtf("kokoro-82m", "pci:0000:01:00.0", 0.02);
        learned.record_tts_rtf("kokoro-82m", "pci:0000:03:00.0", 0.40);
        let fast = resolve_inputs(
            &request(),
            &on_card("pci:0000:01:00.0"),
            &learned,
            bundled_catalog(),
            None,
        );
        let slow = resolve_inputs(
            &request(),
            &on_card("pci:0000:03:00.0"),
            &learned,
            bundled_catalog(),
            None,
        );
        assert!((fast.tts_rtf - 0.02).abs() < 1e-9);
        assert!((slow.tts_rtf - 0.40).abs() < 1e-9);
    }

    #[test]
    fn the_llm_and_the_voice_can_be_on_different_devices() {
        let mut learned = LearnedCoefficients::default();
        learned.record_llm_tok_s("phi4-mini-q4km", "pci:0000:00:02.0", 7.0);
        learned.record_tts_rtf("kokoro-82m", "cpu", 0.9);
        let devices = EtaDevices {
            llm_key: "pci:0000:00:02.0".into(),
            llm_gpu: true,
            tts_key: "cpu".into(),
            tts_gpu: false,
        };
        let resolved = resolve_inputs(&request(), &devices, &learned, bundled_catalog(), None);
        assert!((resolved.tok_s_est - 7.0).abs() < 1e-9);
        assert!((resolved.tts_rtf - 0.9).abs() < 1e-9);
    }

    fn gpu_profile() -> crate::hardware::HardwareProfile {
        crate::hardware::HardwareProfile {
            devices: vec![crate::hardware::test_device(Some(12_288))],
            ram: crate::hardware::RamInfo {
                total_mb: 32_768,
                free_mb: 24_000,
                cgroup: Default::default(),
            },
            cpu: crate::hardware::CpuInfo {
                cores_physical: 8,
                cores_logical: 16,
                avx2: true,
                avx512: false,
            },
            disk: crate::hardware::DiskInfo {
                models_free_gb: 100.0,
                cache_free_gb: 100.0,
            },
        }
    }

    #[test]
    fn resolve_names_the_gpu_for_both_consumers_on_a_gpu_machine() {
        let devices = EtaDevices::resolve(&request(), &gpu_profile(), bundled_catalog(), true);
        assert!(
            devices.tts_gpu,
            "a 12 GB card holds the voice; quoting a CPU real-time factor for it is a              wildly wrong estimate"
        );
        assert!(
            devices.llm_gpu,
            "the llm half resolved through a plan that is None whenever the gguf header is              not read, so it silently reported CPU on every machine"
        );
        assert_eq!(devices.llm_key, devices.tts_key);
        assert_ne!(devices.llm_key, "cpu");
    }

    #[test]
    fn resolve_reads_the_cpu_key_when_torch_has_no_cuda() {
        let devices = EtaDevices::resolve(&request(), &gpu_profile(), bundled_catalog(), false);
        assert!(!devices.tts_gpu);
        assert_eq!(
            devices.tts_key, "cpu",
            "synthesis clamps to the engine's torch_cuda answer before it records, so a \
             gpu key here is read but never written"
        );
    }

    #[test]
    fn resolve_reports_cpu_on_a_machine_with_no_devices() {
        let mut headless = gpu_profile();
        headless.devices.clear();
        let devices = EtaDevices::resolve(&request(), &headless, bundled_catalog(), true);
        assert_eq!(devices, EtaDevices::cpu());
    }

    #[test]
    fn narrator_eta_matches_spec_formulas() {
        let eta = estimate_eta(&inputs(JobMode::Narrator));
        let est_tokens = 26_000.0 * 1.4;
        let audio_s = 26_000.0 / 2.6;
        assert!((eta.script_s - est_tokens / 40.0).abs() < 1e-9);
        assert!((eta.tts_s - audio_s * 0.03 / 4.0).abs() < 1e-9);
        assert!((eta.align_s - audio_s * 0.07).abs() < 1e-9);
        assert!((eta.render_s - audio_s * 0.02).abs() < 1e-9);
        assert!(
            (eta.total_s - (eta.script_s + eta.tts_s + eta.align_s + eta.render_s)).abs() < 1e-9
        );
    }

    #[test]
    fn teacher_mode_expands_tokens_and_audio() {
        let narrator = estimate_eta(&inputs(JobMode::Narrator));
        let teacher = estimate_eta(&inputs(JobMode::Teacher));
        assert!((teacher.script_s - narrator.script_s * 2.2).abs() < 1e-6);
        assert!((teacher.tts_s - narrator.tts_s * 2.2).abs() < 1e-6);
        assert!(teacher.total_s > narrator.total_s);
    }

    #[test]
    fn more_tts_workers_reduce_tts_time() {
        let one = estimate_eta(&EtaInputs {
            tts_workers: 1,
            ..inputs(JobMode::Narrator)
        });
        let four = estimate_eta(&inputs(JobMode::Narrator));
        assert!((one.tts_s - four.tts_s * 4.0).abs() < 1e-6);
    }

    #[test]
    fn custom_align_rtf_overrides_default() {
        let fast = estimate_eta(&EtaInputs {
            align_rtf: Some(0.02),
            ..inputs(JobMode::Narrator)
        });
        let default = estimate_eta(&inputs(JobMode::Narrator));
        assert!(fast.align_s < default.align_s);
    }

    #[test]
    fn zero_words_gives_zero_eta() {
        let eta = estimate_eta(&EtaInputs {
            words: 0,
            ..inputs(JobMode::Narrator)
        });
        assert_eq!(eta.total_s, 0.0);
    }

    #[test]
    fn degenerate_inputs_never_panic_or_produce_negative() {
        let eta = estimate_eta(&EtaInputs {
            mode: JobMode::Teacher,
            words: 1,
            tok_s_est: 0.0,
            tts_rtf: -1.0,
            tts_workers: 0,
            align_rtf: Some(-5.0),
        });
        assert!(eta.total_s >= 0.0);
        assert!(eta.total_s.is_finite());
    }

    #[test]
    fn cpu_only_slow_machine_book_takes_hours() {
        let eta = estimate_eta(&EtaInputs {
            mode: JobMode::Teacher,
            words: 80_000,
            tok_s_est: 4.5,
            tts_rtf: 1.0,
            tts_workers: 2,
            align_rtf: None,
        });
        assert!(eta.total_s > 3600.0 * 10.0, "total {}", eta.total_s);
    }
}
