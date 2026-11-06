use std::sync::{Mutex, MutexGuard};

use serde::Deserialize;

use super::{parse_pci_address, DeviceBackend, DeviceInfo, MemorySource};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EngineGpu {
    pub backend: String,
    pub name: String,
    pub description: String,
    pub integrated: bool,
    pub memory_total: u64,
    pub memory_free: u64,
    pub pci: Option<String>,
}

struct Reported {
    generation: u64,
    gpus: Option<Vec<EngineGpu>>,
}

struct ReportStore {
    inner: Mutex<Reported>,
}

impl ReportStore {
    const fn new() -> Self {
        Self {
            inner: Mutex::new(Reported {
                generation: 0,
                gpus: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Reported> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn begin_engine(&self) -> u64 {
        let mut reported = self.lock();
        reported.generation += 1;
        reported.gpus = None;
        reported.generation
    }

    fn record(&self, generation: u64, gpus: Vec<EngineGpu>) -> bool {
        let mut reported = self.lock();
        if reported.generation != generation {
            return false;
        }
        reported.gpus = Some(gpus);
        true
    }

    fn end_engine(&self, generation: u64) {
        let mut reported = self.lock();
        if reported.generation == generation {
            reported.generation += 1;
            reported.gpus = None;
        }
    }

    fn reported(&self) -> Option<Vec<EngineGpu>> {
        self.lock().gpus.clone()
    }

    fn reported_count(&self) -> Option<usize> {
        self.lock().gpus.as_ref().map(Vec::len)
    }
}

static REPORTED: ReportStore = ReportStore::new();

pub fn begin_engine() -> u64 {
    REPORTED.begin_engine()
}

pub fn record(generation: u64, gpus: Vec<EngineGpu>) -> bool {
    REPORTED.record(generation, gpus)
}

pub fn end_engine(generation: u64) {
    REPORTED.end_engine(generation);
}

pub fn reported_count() -> Option<usize> {
    REPORTED.reported_count()
}

pub fn merge_reported(devices: &mut Vec<DeviceInfo>) {
    match REPORTED.reported() {
        Some(gpus) => merge(devices, &gpus),
        None => devices
            .iter_mut()
            .for_each(|device| device.llm_runtime = None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Pending,
    Ready(Vec<EngineGpu>),
    Absent,
}

pub fn report_in(health: &serde_json::Value) -> Report {
    match health.get("gpu_devices") {
        Some(serde_json::Value::Null) => Report::Pending,
        Some(list @ serde_json::Value::Array(_)) => {
            serde_json::from_value(list.clone()).map_or(Report::Absent, Report::Ready)
        }
        _ => Report::Absent,
    }
}

fn backend_of(raw: &str) -> DeviceBackend {
    match raw.to_ascii_lowercase().as_str() {
        "cuda" => DeviceBackend::Cuda,
        "rocm" | "hip" => DeviceBackend::Rocm,
        "metal" | "mtl" => DeviceBackend::Metal,
        _ => DeviceBackend::Vulkan,
    }
}

fn vendor_of(description: &str) -> &'static str {
    let lower = description.to_ascii_lowercase();
    if ["nvidia", "geforce", "quadro", "tesla"]
        .iter()
        .any(|mark| lower.contains(mark))
    {
        "NVIDIA"
    } else if ["amd", "radeon"].iter().any(|mark| lower.contains(mark)) {
        "AMD"
    } else if lower.contains("intel") {
        "Intel"
    } else if lower.contains("apple") {
        "Apple"
    } else {
        "Unknown"
    }
}

fn ordinal_of(name: &str, fallback: usize) -> u32 {
    let digits: String = name
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| u32::try_from(fallback).unwrap_or(u32::MAX))
}

fn mb(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

pub(super) fn device_of(gpu: &EngineGpu, index: usize) -> DeviceInfo {
    let backend = backend_of(&gpu.backend);
    let ordinal = ordinal_of(&gpu.name, index);
    let key = gpu
        .pci
        .as_deref()
        .and_then(parse_pci_address)
        .unwrap_or_else(|| format!("{}:{ordinal}", backend.as_str()));
    DeviceInfo {
        llm_runtime: Some(true),
        key,
        backend,
        ordinal,
        vendor: vendor_of(&gpu.description).into(),
        name: gpu.description.clone(),
        memory_total_mb: Some(mb(gpu.memory_total)),
        memory_free_mb: Some(mb(gpu.memory_free)),
        unified: gpu.integrated,
        discrete: !gpu.integrated,
        memory_source: match backend {
            DeviceBackend::Cuda => MemorySource::Nvml,
            _ => MemorySource::VkBudget,
        },
    }
}

pub(super) fn merge(devices: &mut Vec<DeviceInfo>, gpus: &[EngineGpu]) {
    for device in devices.iter_mut() {
        device.llm_runtime = device.backend.is_gpu().then_some(false);
    }
    for (index, gpu) in gpus.iter().enumerate() {
        let found = device_of(gpu, index);
        match devices.iter_mut().find(|known| known.key == found.key) {
            Some(known) => {
                if known.memory_total_mb.is_none() {
                    known.memory_total_mb = found.memory_total_mb;
                    known.memory_source = found.memory_source;
                }
                if known.memory_free_mb.is_none() {
                    known.memory_free_mb = found.memory_free_mb;
                }
                if known.vendor == "Unknown" {
                    known.vendor = found.vendor;
                    known.name = found.name;
                }
                known.discrete = found.discrete;
                known.unified = found.unified;
                known.llm_runtime = Some(true);
            }
            None => devices.push(found),
        }
    }
    devices.sort_by(|a, b| a.key.cmp(&b.key));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(backend: &str, name: &str, description: &str, integrated: bool) -> EngineGpu {
        EngineGpu {
            backend: backend.into(),
            name: name.into(),
            description: description.into(),
            integrated,
            memory_total: 8 << 30,
            memory_free: 6 << 30,
            pci: Some("0000:03:00.0".into()),
        }
    }

    #[test]
    fn the_engine_health_payload_parses_into_devices() {
        let raw = r#"[{"backend":"Vulkan","name":"Vulkan1","description":"AMD Radeon RX 7600",
            "integrated":false,"memory_total":8589934592,"memory_free":7516192768,
            "pci":"0000:03:00.0"}]"#;
        let gpus: Vec<EngineGpu> = serde_json::from_str(raw).expect("engine payload");
        let device = device_of(&gpus[0], 0);
        assert_eq!(device.key, parse_pci_address("0000:03:00.0").expect("pci"));
        assert_eq!(device.backend, DeviceBackend::Vulkan);
        assert_eq!(device.ordinal, 1);
        assert_eq!(device.vendor, "AMD");
        assert_eq!(device.memory_total_mb, Some(8192));
        assert_eq!(device.memory_free_mb, Some(7168));
        assert!(device.discrete && !device.unified);
        assert_eq!(device.memory_source, MemorySource::VkBudget);
    }

    #[test]
    fn a_gpu_no_vendor_tool_found_joins_the_inventory() {
        let mut devices = Vec::new();
        merge(
            &mut devices,
            &[gpu("Vulkan", "Vulkan0", "Intel(R) Arc(TM) Graphics", true)],
        );
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].vendor, "Intel");
        assert!(devices[0].unified && !devices[0].discrete);
        assert!(devices[0].backend.is_gpu());
    }

    #[test]
    fn a_gpu_the_vendor_tool_already_found_keeps_its_live_figures() {
        let mut nvidia = crate::hardware::test_device(Some(4096));
        nvidia.key = parse_pci_address("0000:03:00.0").expect("pci");
        nvidia.memory_free_mb = Some(3000);
        let mut devices = vec![nvidia];
        merge(
            &mut devices,
            &[gpu("Vulkan", "Vulkan1", "NVIDIA GeForce RTX 3050", false)],
        );
        assert_eq!(
            devices.len(),
            1,
            "one card reached through two APIs is one device"
        );
        assert_eq!(devices[0].backend, DeviceBackend::Cuda);
        assert_eq!(devices[0].memory_total_mb, Some(4096));
        assert_eq!(devices[0].memory_free_mb, Some(3000));
        assert!(devices[0].discrete);
    }

    #[test]
    fn a_report_says_which_gpus_local_ai_can_use() {
        let mut nvidia = crate::hardware::test_device(Some(4096));
        nvidia.key = parse_pci_address("0000:03:00.0").expect("pci");
        let mut devices = vec![nvidia];
        merge(&mut devices, &[]);
        assert_eq!(
            devices[0].llm_runtime,
            Some(false),
            "a card the engine did not report is out of local AI's reach"
        );
        merge(
            &mut devices,
            &[gpu("Vulkan", "Vulkan0", "NVIDIA GeForce RTX 3050", false)],
        );
        assert_eq!(devices[0].llm_runtime, Some(true));
    }

    #[test]
    fn a_device_without_a_pci_address_is_keyed_by_backend_and_ordinal() {
        let mut device = gpu("CUDA", "CUDA0", "NVIDIA GeForce RTX 4060", false);
        device.pci = None;
        let info = device_of(&device, 0);
        assert_eq!(info.key, "cuda:0");
        assert_eq!(info.backend, DeviceBackend::Cuda);
        assert_eq!(info.memory_source, MemorySource::Nvml);
    }

    #[test]
    fn health_says_whether_the_gpu_report_is_pending_ready_or_absent() {
        assert_eq!(
            report_in(&serde_json::json!({"gpu_offload": null, "gpu_devices": null})),
            Report::Pending
        );
        assert_eq!(
            report_in(&serde_json::json!({"gpu_offload": false, "gpu_devices": []})),
            Report::Ready(Vec::new())
        );
        assert_eq!(
            report_in(&serde_json::json!({"gpu_offload": true})),
            Report::Absent,
            "an engine from before the report never sends one, so the app stops asking"
        );
        assert_eq!(
            report_in(&serde_json::json!({"gpu_devices": [{"backend": "Vulkan"}]})),
            Report::Absent,
            "a malformed report is not polled forever"
        );
    }

    #[test]
    fn a_report_from_an_engine_that_already_stopped_is_dropped() {
        let store = ReportStore::new();
        let first = store.begin_engine();
        let second = store.begin_engine();
        assert_eq!(
            store.reported_count(),
            None,
            "nothing is known before the engine reports"
        );
        assert!(!store.record(first, vec![gpu("Vulkan", "Vulkan0", "stale", false)]));
        assert!(store.record(second, vec![gpu("Vulkan", "Vulkan0", "live", false)]));
        assert_eq!(
            store.reported().expect("live report")[0].description,
            "live"
        );
        assert_eq!(store.reported_count(), Some(1));
        store.end_engine(first);
        assert_eq!(
            store.reported_count(),
            Some(1),
            "ending an older engine leaves the live report"
        );
        store.end_engine(second);
        assert_eq!(store.reported_count(), None);
    }
}
