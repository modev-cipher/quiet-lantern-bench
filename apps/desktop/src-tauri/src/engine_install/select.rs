use serde::Serialize;

use super::manifest::{CPU_VARIANT, CUDA13_VARIANT};

pub const CUDA13_MIN_DRIVER_MAJOR: u32 = 580;
pub const CUDA13_MIN_COMPUTE: (u32, u32) = (7, 5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaGpu {
    pub name: String,
    pub driver_major: u32,
    pub compute: (u32, u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    NvidiaReady,
    NoNvidiaGpu,
    DriverTooOld,
    GpuTooOld,
    NotOffered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub variant: String,
    pub reason: Reason,
    pub gpu_name: Option<String>,
}

fn parse_compute(raw: &str) -> Option<(u32, u32)> {
    let (major, minor) = raw.trim().split_once('.')?;
    Some((major.trim().parse().ok()?, minor.trim().parse().ok()?))
}

pub fn parse_nvidia_facts(out: &str) -> Vec<NvidiaGpu> {
    out.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.rsplitn(3, ',').map(str::trim).collect();
            let [compute, driver, name] = fields[..] else {
                return None;
            };
            let driver_major = driver.split('.').next()?.trim().parse().ok()?;
            Some(NvidiaGpu {
                name: name.to_string(),
                driver_major,
                compute: parse_compute(compute)?,
            })
        })
        .collect()
}

pub async fn probe_nvidia() -> Vec<NvidiaGpu> {
    crate::hardware::run_probe(
        &crate::hardware::resolve_nvidia_smi(),
        &[
            "--query-gpu=name,driver_version,compute_cap",
            "--format=csv,noheader",
        ],
    )
    .await
    .map(|out| parse_nvidia_facts(&out))
    .unwrap_or_default()
}

/// The CUDA 13 build needs an NVIDIA driver from the R580 branch or newer and a
/// Turing-or-newer GPU; anything else gets the CPU build, which runs everywhere.
pub fn choose(gpus: &[NvidiaGpu], cuda_offered: bool) -> Choice {
    let cpu = |reason: Reason, gpu_name: Option<String>| Choice {
        variant: CPU_VARIANT.to_string(),
        reason,
        gpu_name,
    };
    let Some(best) = gpus
        .iter()
        .max_by_key(|gpu| (gpu.compute, gpu.driver_major))
    else {
        return cpu(Reason::NoNvidiaGpu, None);
    };
    if best.compute < CUDA13_MIN_COMPUTE {
        return cpu(Reason::GpuTooOld, Some(best.name.clone()));
    }
    if best.driver_major < CUDA13_MIN_DRIVER_MAJOR {
        return cpu(Reason::DriverTooOld, Some(best.name.clone()));
    }
    if !cuda_offered {
        return cpu(Reason::NotOffered, Some(best.name.clone()));
    }
    Choice {
        variant: CUDA13_VARIANT.to_string(),
        reason: Reason::NvidiaReady,
        gpu_name: Some(best.name.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(name: &str, driver_major: u32, compute: (u32, u32)) -> NvidiaGpu {
        NvidiaGpu {
            name: name.into(),
            driver_major,
            compute,
        }
    }

    #[test]
    fn nvidia_smi_rows_parse_including_names_with_commas() {
        let out = "NVIDIA GeForce RTX 3050 Laptop GPU, 580.178.04, 8.6\nQuadro P1000, Mobile, 535.104.05, 6.1\n\n";
        assert_eq!(
            parse_nvidia_facts(out),
            vec![
                gpu("NVIDIA GeForce RTX 3050 Laptop GPU", 580, (8, 6)),
                gpu("Quadro P1000, Mobile", 535, (6, 1)),
            ]
        );
    }

    #[test]
    fn unreadable_rows_are_skipped_not_guessed() {
        assert!(parse_nvidia_facts("No devices were found\n").is_empty());
        assert!(parse_nvidia_facts("GPU, [N/A], 8.6\n").is_empty());
        assert!(parse_nvidia_facts("GPU, 580.1, [N/A]\n").is_empty());
    }

    #[test]
    fn a_turing_gpu_on_a_580_driver_gets_the_cuda_build() {
        let choice = choose(&[gpu("RTX 3050", 580, (8, 6))], true);
        assert_eq!(choice.variant, CUDA13_VARIANT);
        assert_eq!(choice.reason, Reason::NvidiaReady);
        assert_eq!(
            choose(&[gpu("T4", 580, (7, 5))], true).variant,
            CUDA13_VARIANT
        );
    }

    #[test]
    fn everything_else_gets_the_cpu_build_with_the_reason() {
        assert_eq!(choose(&[], true).reason, Reason::NoNvidiaGpu);
        assert_eq!(
            choose(&[gpu("GTX 1080", 590, (6, 1))], true).reason,
            Reason::GpuTooOld
        );
        assert_eq!(
            choose(&[gpu("Titan V", 590, (7, 0))], true).reason,
            Reason::GpuTooOld
        );
        assert_eq!(
            choose(&[gpu("RTX 3050", 579, (8, 6))], true).reason,
            Reason::DriverTooOld
        );
        assert_eq!(
            choose(&[gpu("RTX 3050", 580, (8, 6))], false).reason,
            Reason::NotOffered
        );
        for choice in [
            choose(&[], true),
            choose(&[gpu("GTX 1080", 590, (6, 1))], true),
            choose(&[gpu("RTX 3050", 579, (8, 6))], true),
        ] {
            assert_eq!(choice.variant, CPU_VARIANT);
        }
    }

    #[test]
    fn with_several_gpus_the_most_capable_decides() {
        let choice = choose(
            &[gpu("GTX 1050", 580, (6, 1)), gpu("RTX 4060", 580, (8, 9))],
            true,
        );
        assert_eq!(choice.variant, CUDA13_VARIANT);
        assert_eq!(choice.gpu_name.as_deref(), Some("RTX 4060"));
    }
}
