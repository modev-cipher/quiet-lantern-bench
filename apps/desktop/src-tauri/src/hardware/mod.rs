pub mod benchmark;
pub mod cgroup;
pub mod degrade;
pub mod device;
pub mod engine_gpus;
pub mod eta;
pub mod learned;
pub mod plan;
pub mod score;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareProfile {
    pub devices: Vec<DeviceInfo>,
    pub ram: RamInfo,
    pub cpu: CpuInfo,
    pub disk: DiskInfo,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub key: String,
    pub backend: DeviceBackend,
    pub ordinal: u32,
    pub vendor: String,
    pub name: String,
    pub memory_total_mb: Option<u64>,
    pub memory_free_mb: Option<u64>,
    pub unified: bool,
    pub discrete: bool,
    pub memory_source: MemorySource,
    /// Whether the running engine's llama.cpp runtime reported this GPU; `None` until an
    /// engine has reported.
    #[serde(skip)]
    pub llm_runtime: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    Nvml,
    VkHeap,
    VkBudget,
    Rocm,
    Derived,
    Unknown,
}

pub fn memory_is_unreadable(device: &DeviceInfo) -> bool {
    device.memory_source == MemorySource::Unknown
}

pub fn usable_memory_mb(device: &DeviceInfo) -> u64 {
    device
        .memory_free_mb
        .or(device.memory_total_mb)
        .unwrap_or(UNKNOWN_VRAM_FLOOR_MB)
}

pub const UNKNOWN_VRAM_FLOOR_MB: u64 = 2048;

pub use crate::hardware::device::DeviceBackend;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RamInfo {
    /// What the host reports, always. `HardwareProfile::fingerprint` keys the
    /// `hw_benchmark` cache off this, and a benchmark measuring tok/s is a
    /// CPU property that entering a cgroup does not invalidate — so the limit
    /// lands beside these figures rather than replacing them.
    pub total_mb: u64,
    pub free_mb: u64,
    /// What the cgroup hierarchy allows on top of that, when it says anything.
    pub cgroup: cgroup::CgroupMemory,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuInfo {
    pub cores_physical: u32,
    pub cores_logical: u32,
    pub avx2: bool,
    pub avx512: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub models_free_gb: f64,
    pub cache_free_gb: f64,
}

pub async fn detect(
    models_dir: &Path,
    models_origin: DirOrigin,
    cache_dir: &Path,
) -> HardwareProfile {
    let (devices, ram, cpu) = tokio::join!(detect_devices(), detect_ram(), detect_cpu());
    HardwareProfile {
        devices,
        ram,
        cpu,
        disk: detect_disk(models_dir, models_origin, cache_dir),
    }
}

pub async fn probe_device_free(planned: Option<&str>) -> Option<(String, u64)> {
    let mut inventory = device_inventory().await.into_owned();
    engine_gpus::merge_reported(&mut inventory);
    let device = match planned {
        Some(key) => inventory.iter().find(|device| device.key == key)?,
        None => select_primary(&inventory)?,
    };
    let free_mb = live_free_mb(device).await?;
    Some((device.key.clone(), free_mb))
}

async fn live_free_mb(device: &DeviceInfo) -> Option<u64> {
    probe_memory(device.memory_source)
        .await
        .into_iter()
        .find(|fresh| fresh.key == device.key)
        .and_then(|fresh| fresh.memory_free_mb)
        .or(device.memory_free_mb)
}

pub async fn probe_ram_free_mb() -> u64 {
    detect_ram().await.free_mb
}

impl HardwareProfile {
    pub fn primary(&self) -> Option<&DeviceInfo> {
        select_primary(&self.devices)
    }

    pub fn select_for(&self, need_mb: Option<u64>) -> Option<&DeviceInfo> {
        select_for(&self.devices, need_mb, plan::effective_free_mb(&self.ram))
    }

    pub fn fingerprint(&self) -> String {
        let mut keys: Vec<String> = self
            .devices
            .iter()
            .map(|d| {
                format!(
                    "{}/{}/{}",
                    d.key,
                    d.backend.as_str(),
                    d.memory_total_mb.unwrap_or(0)
                )
            })
            .collect();
        keys.sort();
        let devices = if keys.is_empty() {
            "none".to_string()
        } else {
            keys.join(",")
        };
        format!(
            "{devices}|ram:{}|cores:{}",
            self.ram.total_mb, self.cpu.cores_physical
        )
    }
}

pub(crate) async fn run_probe(program: &str, args: &[&str]) -> Option<String> {
    run_probe_with_timeout(program, args, PROBE_TIMEOUT).await
}

const MAX_PROBE_STDOUT_BYTES: u64 = 256 * 1024;

async fn run_probe_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    use tokio::io::AsyncReadExt;

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(crate::process_priority::CREATE_NO_WINDOW);
    let mut child = command.spawn().ok()?;

    let mut stdout = child.stdout.take()?;

    let work = async {
        let mut buf = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_PROBE_STDOUT_BYTES)
            .read_to_end(&mut buf)
            .await;
        let status = child.wait().await.ok()?;
        if status.success() {
            Some(buf)
        } else {
            None
        }
    };

    let output = match tokio::time::timeout(timeout, work).await {
        Ok(Some(buf)) => buf,
        _ => return None,
    };
    String::from_utf8(output).ok()
}

static DEVICE_INVENTORY: tokio::sync::OnceCell<Vec<DeviceInfo>> =
    tokio::sync::OnceCell::const_new();

async fn device_inventory() -> Cow<'static, [DeviceInfo]> {
    if let Some(cached) = DEVICE_INVENTORY.get() {
        return Cow::Borrowed(cached);
    }
    let probed = probe_devices().await;
    if probed.is_empty() {
        return Cow::Owned(probed);
    }
    Cow::Borrowed(DEVICE_INVENTORY.get_or_init(|| async { probed }).await)
}

async fn detect_devices() -> Vec<DeviceInfo> {
    let mut devices = device_inventory().await.into_owned();
    refresh_memory(&mut devices).await;
    engine_gpus::merge_reported(&mut devices);
    devices
}

async fn probe_devices() -> Vec<DeviceInfo> {
    let (nvidia, rocm, vulkan) = tokio::join!(
        probe_memory(MemorySource::Nvml),
        probe_memory(MemorySource::Rocm),
        probe_vulkan_views(),
    );

    let mut devices: Vec<DeviceInfo> = nvidia;
    devices.extend(rocm);
    merge_vulkan(&mut devices, vulkan);
    if devices.is_empty() {
        devices = sysfs_devices();
    }

    devices.sort_by(|a, b| a.key.cmp(&b.key));
    devices
}

const SYSFS_DRM: &str = "/sys/class/drm";

fn sysfs_devices() -> Vec<DeviceInfo> {
    let Ok(entries) = std::fs::read_dir(SYSFS_DRM) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let device_dir = entry.path().join("device");
            let address = std::fs::canonicalize(&device_dir)
                .ok()
                .and_then(|target| target.file_name().map(|n| n.to_string_lossy().to_string()))?;
            let vendor = std::fs::read_to_string(device_dir.join("vendor")).ok();
            parse_drm_entry(&name, &address, vendor.as_deref())
        })
        .collect()
}

fn parse_drm_entry(name: &str, address: &str, vendor_raw: Option<&str>) -> Option<DeviceInfo> {
    let index = name.strip_prefix("card")?;
    if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let key = parse_pci_address(address)?;
    let vendor = vendor_raw
        .map(|raw| pci_vendor_name(raw.trim()))
        .unwrap_or("Unknown");
    Some(DeviceInfo {
        llm_runtime: None,
        key,
        backend: DeviceBackend::Vulkan,
        ordinal: index.parse().unwrap_or(0),
        vendor: vendor.into(),
        name: format!("{vendor} GPU at {address}"),
        memory_total_mb: None,
        memory_free_mb: None,
        unified: false,
        discrete: false,
        memory_source: MemorySource::Unknown,
    })
}

fn pci_vendor_name(raw: &str) -> &'static str {
    match raw {
        "0x10de" => "NVIDIA",
        "0x1002" | "0x1022" => "AMD",
        "0x8086" => "Intel",
        _ => "Unknown",
    }
}

async fn refresh_memory(devices: &mut [DeviceInfo]) {
    let mut sources: Vec<MemorySource> =
        devices.iter().map(|device| device.memory_source).collect();
    sources.sort_unstable();
    sources.dedup();

    let probed: Vec<DeviceInfo> =
        futures_util::future::join_all(sources.into_iter().map(probe_memory))
            .await
            .concat();

    for device in devices.iter_mut() {
        let fresh = probed
            .iter()
            .find(|fresh| fresh.key == device.key)
            .and_then(|fresh| fresh.memory_free_mb);
        if fresh.is_some() {
            device.memory_free_mb = fresh;
        }
    }
}

async fn probe_vulkan_views() -> Vec<VulkanView> {
    run_probe("vulkaninfo", &[])
        .await
        .map(|out| parse_vulkaninfo_views(&out))
        .unwrap_or_default()
}

fn nvidia_smi_candidates() -> Vec<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let roots = [
            std::env::var_os("ProgramFiles").map(std::path::PathBuf::from),
            std::env::var_os("ProgramFiles(x86)").map(std::path::PathBuf::from),
        ];
        roots
            .into_iter()
            .flatten()
            .map(|root| {
                root.join("NVIDIA Corporation")
                    .join("NVSMI")
                    .join("nvidia-smi.exe")
            })
            .collect()
    }
    #[cfg(target_os = "linux")]
    {
        ["/usr/bin/nvidia-smi", "/usr/local/bin/nvidia-smi"]
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        Vec::new()
    }
}

fn resolve_nvidia_smi_in(candidates: &[std::path::PathBuf]) -> String {
    candidates
        .iter()
        .find(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "nvidia-smi".to_string())
}

pub(crate) fn resolve_nvidia_smi() -> String {
    resolve_nvidia_smi_in(&nvidia_smi_candidates())
}

async fn probe_memory(source: MemorySource) -> Vec<DeviceInfo> {
    match source {
        MemorySource::Nvml => run_probe(
            &resolve_nvidia_smi(),
            &[
                "--query-gpu=name,memory.total,memory.free,pci.bus_id",
                "--format=csv,noheader,nounits",
            ],
        )
        .await
        .map(|out| parse_nvidia_smi(&out)),
        MemorySource::Rocm => run_probe(
            "rocm-smi",
            &["--showproductname", "--showmeminfo", "vram", "--showbus"],
        )
        .await
        .map(|out| parse_rocm_smi(&out)),
        MemorySource::VkBudget => run_probe("vulkaninfo", &[])
            .await
            .map(|out| parse_vulkaninfo(&out)),
        MemorySource::VkHeap | MemorySource::Derived | MemorySource::Unknown => None,
    }
    .unwrap_or_default()
}

pub struct VulkanView {
    device: DeviceInfo,
    classified: bool,
}

fn merge_vulkan(devices: &mut Vec<DeviceInfo>, found: Vec<VulkanView>) {
    for view in found {
        let device = view.device;
        match devices.iter_mut().find(|known| known.key == device.key) {
            Some(known) => {
                if known.memory_total_mb.is_none() {
                    known.memory_total_mb = device.memory_total_mb;
                    known.memory_source = device.memory_source;
                }
                if known.memory_free_mb.is_none() {
                    known.memory_free_mb = device.memory_free_mb;
                }
                if view.classified {
                    known.discrete = device.discrete;
                    known.unified = device.unified;
                }
            }
            None => devices.push(device),
        }
    }
}

pub fn select_for(
    devices: &[DeviceInfo],
    need_mb: Option<u64>,
    ram_free_mb: u64,
) -> Option<&DeviceInfo> {
    select_among(devices.iter(), need_mb, ram_free_mb)
}

pub fn select_among<'a>(
    devices: impl Iterator<Item = &'a DeviceInfo>,
    need_mb: Option<u64>,
    ram_free_mb: u64,
) -> Option<&'a DeviceInfo> {
    devices.max_by(|a, b| {
        let fit = |d: &DeviceInfo| {
            let usable = budget_mb(d, ram_free_mb);
            match need_mb {
                Some(need) if need > 0 => (usable as f64 / need as f64).min(1.0),
                _ => {
                    if usable > 0 {
                        1.0
                    } else {
                        0.0
                    }
                }
            }
        };
        fit(a)
            .partial_cmp(&fit(b))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.discrete.cmp(&b.discrete))
            .then(budget_mb(a, ram_free_mb).cmp(&budget_mb(b, ram_free_mb)))
            .then(b.key.cmp(&a.key))
    })
}

pub fn budget_mb(device: &DeviceInfo, ram_free_mb: u64) -> u64 {
    let own = usable_memory_mb(device);
    if device.unified {
        own.min(ram_free_mb)
    } else {
        own
    }
}

pub fn select_primary(devices: &[DeviceInfo]) -> Option<&DeviceInfo> {
    select_for(devices, None, u64::MAX)
}

fn mb_from_bytes(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

fn parse_nvidia_smi(out: &str) -> Vec<DeviceInfo> {
    let mut devices = Vec::new();
    for (index, line) in out.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let mut wide: Vec<&str> = line.rsplitn(4, ',').map(str::trim).collect();
        wide.reverse();
        let has_bus_column = wide.len() == 4 && wide[3].parse::<u64>().is_err();
        let mut parts: Vec<&str> = if has_bus_column {
            wide
        } else {
            let mut narrow: Vec<&str> = line.rsplitn(3, ',').map(str::trim).collect();
            narrow.reverse();
            narrow
        };
        parts.truncate(if has_bus_column { 4 } else { 3 });
        if parts.len() < 3 {
            continue;
        }
        let bus = parts.get(3).and_then(|raw| parse_pci_address(raw));
        let name = parts[0];
        if name.is_empty() {
            continue;
        }
        let total = parts[1].parse::<u64>().ok();
        let free = parts[2].parse::<u64>().ok();
        if total.is_none() && free.is_none() && bus.is_none() {
            continue;
        }
        devices.push(DeviceInfo {
            llm_runtime: None,
            key: bus.unwrap_or_else(|| format!("nvidia:{index}")),
            backend: DeviceBackend::Cuda,
            ordinal: index as u32,
            vendor: "NVIDIA".into(),
            name: name.into(),
            memory_total_mb: total,
            memory_free_mb: free,
            unified: false,
            discrete: true,
            memory_source: if total.is_some() || free.is_some() {
                MemorySource::Nvml
            } else {
                MemorySource::Unknown
            },
        });
    }
    devices
}

fn pci_key(domain: u32, bus: u32, slot: u32, function: u32) -> String {
    format!("pci:{domain:04x}:{bus:02x}:{slot:02x}.{function:x}")
}

fn pci_field(line: &str, field: &str) -> Option<u32> {
    line.strip_prefix(field)?
        .trim_start_matches(['=', ':', ' '])
        .trim()
        .parse::<u32>()
        .ok()
}

fn parse_pci_address(raw: &str) -> Option<String> {
    let mut fields: Vec<&str> = raw.trim().split(':').collect();
    let (slot, function) = fields.pop()?.split_once('.')?;
    let slot = u32::from_str_radix(slot, 16).ok()?;
    let function = u32::from_str_radix(function, 16).ok()?;
    let bus = u32::from_str_radix(fields.pop()?, 16).ok()?;
    let domain = match fields.pop() {
        Some(raw) => u32::from_str_radix(raw, 16).ok()?,
        None => 0,
    };
    Some(pci_key(domain, bus, slot, function))
}

fn parse_rocm_smi(out: &str) -> Vec<DeviceInfo> {
    let mut blocks: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
    for line in out.lines() {
        let Some(open) = line.find("GPU[") else {
            continue;
        };
        let Some(close) = line[open..].find(']') else {
            continue;
        };
        let Ok(id) = line[open + 4..open + close].trim().parse::<u32>() else {
            continue;
        };
        blocks.entry(id).or_default().push(line);
    }

    blocks
        .into_iter()
        .filter_map(|(index, lines)| {
            let field = |needle: &str| {
                lines
                    .iter()
                    .find(|l| l.contains(needle))
                    .and_then(|l| l.rsplit(':').next())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            };
            let name = lines
                .iter()
                .find(|l| l.contains("Card series") || l.contains("Card SKU"))
                .and_then(|l| l.rsplit(':').next())
                .map(str::trim)
                .filter(|value| !value.is_empty())?;

            let vram_total_mb = field("VRAM Total Memory")
                .and_then(|v| v.parse::<u64>().ok())
                .map(mb_from_bytes);
            let vram_free_mb = match (
                vram_total_mb,
                field("VRAM Total Used Memory")
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(mb_from_bytes),
            ) {
                (Some(total), Some(used)) => Some(total.saturating_sub(used)),
                _ => None,
            };

            let key = lines
                .iter()
                .find(|l| l.contains("PCI Bus"))
                .and_then(|l| l.split_whitespace().find_map(parse_pci_address))
                .unwrap_or_else(|| format!("rocm:{index}"));

            Some(DeviceInfo {
                llm_runtime: None,
                key,
                backend: DeviceBackend::Rocm,
                ordinal: index,
                vendor: "AMD".into(),
                name: name.into(),
                memory_total_mb: vram_total_mb,
                memory_free_mb: vram_free_mb,
                unified: false,
                discrete: true,
                memory_source: if vram_total_mb.is_some() {
                    MemorySource::Rocm
                } else {
                    MemorySource::Unknown
                },
            })
        })
        .collect()
}

#[derive(Default)]
struct VulkanHeap {
    size: Option<u64>,
    budget: Option<u64>,
    device_local: bool,
}

#[derive(Default)]
struct VulkanDevice {
    name: Option<String>,
    device_type: Option<String>,
    domain: Option<u32>,
    bus: Option<u32>,
    slot: Option<u32>,
    function: Option<u32>,
    heaps: Vec<VulkanHeap>,
    in_heaps: bool,
}

impl VulkanDevice {
    fn chosen_heap(&self) -> Option<&VulkanHeap> {
        self.heaps
            .iter()
            .filter(|heap| heap.device_local)
            .max_by_key(|heap| heap.size.unwrap_or(0))
            .or_else(|| self.heaps.iter().max_by_key(|heap| heap.size.unwrap_or(0)))
    }

    fn pci_key(&self) -> Option<String> {
        Some(pci_key(
            self.domain.unwrap_or(0),
            self.bus?,
            self.slot?,
            self.function?,
        ))
    }
}

fn parse_vulkaninfo(out: &str) -> Vec<DeviceInfo> {
    parse_vulkaninfo_views(out)
        .into_iter()
        .map(|view| view.device)
        .collect()
}

fn parse_vulkaninfo_views(out: &str) -> Vec<VulkanView> {
    let mut devices: Vec<VulkanDevice> = Vec::new();
    let mut current = VulkanDevice::default();
    let flush = |current: &mut VulkanDevice, devices: &mut Vec<VulkanDevice>| {
        if current.name.is_some() || current.device_type.is_some() {
            devices.push(std::mem::take(current));
        }
    };
    for line in out.lines() {
        let raw = line;
        let line = line.trim();
        if line.starts_with("GPU") && line.ends_with(':') && !raw.starts_with('\t') {
            flush(&mut current, &mut devices);
        } else if let Some(v) = line.strip_prefix("deviceName") {
            let candidate = v.trim_start_matches(['=', ':', ' ']).trim();
            if !candidate.is_empty() {
                if current.name.is_some() {
                    flush(&mut current, &mut devices);
                }
                current.name = Some(candidate.to_string());
            }
        } else if let Some(v) = line.strip_prefix("deviceType") {
            let candidate = v.trim_start_matches(['=', ':', ' ']).trim();
            if !candidate.is_empty() && current.device_type.is_none() {
                current.device_type = Some(candidate.to_string());
            }
        } else if let Some(v) = pci_field(line, "pciDomain") {
            current.domain.get_or_insert(v);
        } else if let Some(v) = pci_field(line, "pciBus") {
            current.bus.get_or_insert(v);
        } else if let Some(v) = pci_field(line, "pciDevice") {
            current.slot.get_or_insert(v);
        } else if let Some(v) = pci_field(line, "pciFunction") {
            current.function.get_or_insert(v);
        } else if line.starts_with("memoryHeaps[") {
            current.in_heaps = true;
            current.heaps.push(VulkanHeap::default());
        } else if line.starts_with("memoryTypes") {
            current.in_heaps = false;
        } else if current.in_heaps {
            if let Some(heap) = current.heaps.last_mut() {
                if line.starts_with("size") {
                    heap.size = heap_number(line);
                } else if line.starts_with("budget") {
                    heap.budget = heap_number(line);
                } else if line.contains("MEMORY_HEAP_DEVICE_LOCAL_BIT") {
                    heap.device_local = true;
                }
            }
        }
    }
    flush(&mut current, &mut devices);

    devices
        .into_iter()
        .filter(|d| {
            let name_ok = d
                .name
                .as_deref()
                .is_some_and(|n| !n.to_lowercase().contains("llvmpipe"));
            let type_ok = match d.device_type.as_deref() {
                Some(t) => t.contains("DISCRETE") || t.contains("INTEGRATED"),
                None => true,
            };
            name_ok && type_ok
        })
        .enumerate()
        .filter_map(|(index, d)| {
            let name = d.name.clone()?;
            let lower = name.to_lowercase();
            let vendor = if lower.contains("nvidia") {
                "NVIDIA"
            } else if lower.contains("amd") || lower.contains("radeon") {
                "AMD"
            } else if lower.contains("intel") {
                "Intel"
            } else {
                "Unknown"
            };
            let discrete = d
                .device_type
                .as_deref()
                .is_some_and(|t| t.contains("DISCRETE"));
            let heap = d.chosen_heap();
            let total = heap.and_then(|h| h.size).map(mb_from_bytes);
            let free = heap
                .and_then(|h| h.budget)
                .map(mb_from_bytes)
                .map(|budget| match total {
                    Some(t) => budget.min(t),
                    None => budget,
                });
            let source = if free.is_some() {
                MemorySource::VkBudget
            } else if total.is_some() {
                MemorySource::VkHeap
            } else {
                MemorySource::Unknown
            };
            Some(VulkanView {
                classified: d.device_type.is_some(),
                device: DeviceInfo {
                    llm_runtime: None,
                    key: d.pci_key().unwrap_or_else(|| format!("vulkan:{index}")),
                    backend: DeviceBackend::Vulkan,
                    ordinal: index as u32,
                    vendor: vendor.into(),
                    name,
                    memory_total_mb: total,
                    memory_free_mb: free,
                    unified: !discrete,
                    discrete,
                    memory_source: source,
                },
            })
        })
        .collect()
}

fn heap_number(line: &str) -> Option<u64> {
    line.split('=')
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
}

#[cfg(target_os = "linux")]
async fn detect_ram() -> RamInfo {
    let meminfo = tokio::fs::read_to_string("/proc/meminfo")
        .await
        .unwrap_or_default();
    RamInfo {
        cgroup: cgroup::detect(),
        ..parse_meminfo(&meminfo)
    }
}

#[cfg(target_os = "windows")]
fn native_windows_mem() -> Option<RamInfo> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a live, writable MEMORYSTATUSEX whose dwLength is set as the API requires.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok == 0 {
        return None;
    }
    Some(ram_from_bytes(status.ullTotalPhys, status.ullAvailPhys)).filter(|ram| ram.total_mb > 0)
}

#[cfg(any(target_os = "windows", test))]
fn ram_from_bytes(total_bytes: u64, available_bytes: u64) -> RamInfo {
    RamInfo {
        total_mb: total_bytes / (1024 * 1024),
        free_mb: available_bytes / (1024 * 1024),
        cgroup: cgroup::CgroupMemory::NONE,
    }
}

#[cfg(target_os = "windows")]
async fn detect_ram() -> RamInfo {
    if let Some(ram) = native_windows_mem() {
        return ram;
    }
    tracing::warn!("hardware ram probe=GlobalMemoryStatusEx failed; falling back to powershell");
    let out = run_probe(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "$m = Get-CimInstance Win32_OperatingSystem; \
             Write-Output \"$($m.TotalVisibleMemorySize) $($m.FreePhysicalMemory)\"",
        ],
    )
    .await
    .unwrap_or_default();
    parse_windows_mem(&out)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
async fn detect_ram() -> RamInfo {
    RamInfo {
        total_mb: 0,
        free_mb: 0,
        cgroup: cgroup::CgroupMemory::NONE,
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_meminfo(meminfo: &str) -> RamInfo {
    fn kb_value(meminfo: &str, key: &str) -> u64 {
        meminfo
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    }
    let total_kb = kb_value(meminfo, "MemTotal:");
    let available_kb = kb_value(meminfo, "MemAvailable:");
    RamInfo {
        total_mb: total_kb / 1024,
        free_mb: available_kb / 1024,
        cgroup: cgroup::CgroupMemory::NONE,
    }
}

#[cfg(target_os = "windows")]
fn parse_windows_mem(out: &str) -> RamInfo {
    let mut parts = out.split_whitespace();
    let total_kb: u64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let free_kb: u64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    RamInfo {
        total_mb: total_kb / 1024,
        free_mb: free_kb / 1024,
        cgroup: cgroup::CgroupMemory::NONE,
    }
}

async fn detect_cpu() -> CpuInfo {
    let cores_logical = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let cores_physical = detect_physical_cores().await.unwrap_or(cores_logical);
    CpuInfo {
        cores_physical,
        cores_logical,
        avx2: detect_avx2(),
        avx512: detect_avx512(),
    }
}

#[cfg(target_arch = "x86_64")]
fn detect_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

#[cfg(target_arch = "x86_64")]
fn detect_avx512() -> bool {
    std::arch::is_x86_feature_detected!("avx512f")
}

#[cfg(not(target_arch = "x86_64"))]
fn detect_avx2() -> bool {
    false
}

#[cfg(not(target_arch = "x86_64"))]
fn detect_avx512() -> bool {
    false
}

#[cfg(target_os = "linux")]
async fn detect_physical_cores() -> Option<u32> {
    let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await.ok()?;
    parse_cpuinfo_physical_cores(&cpuinfo)
}

#[cfg(target_os = "windows")]
async fn detect_physical_cores() -> Option<u32> {
    let out = run_probe(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfCores -Sum).Sum",
        ],
    )
    .await?;
    out.trim().parse().ok()
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
async fn detect_physical_cores() -> Option<u32> {
    None
}

#[cfg(any(target_os = "linux", test))]
fn parse_cpuinfo_physical_cores(cpuinfo: &str) -> Option<u32> {
    use std::collections::HashSet;
    let mut pairs = HashSet::new();
    let mut physical_id = None;
    for line in cpuinfo.lines() {
        if let Some(v) = line.strip_prefix("physical id") {
            physical_id = v
                .rsplit(':')
                .next()
                .and_then(|v| v.trim().parse::<u32>().ok());
        } else if let Some(v) = line.strip_prefix("core id") {
            if let (Some(pid), Some(cid)) = (
                physical_id,
                v.rsplit(':')
                    .next()
                    .and_then(|v| v.trim().parse::<u32>().ok()),
            ) {
                pairs.insert((pid, cid));
            }
        }
    }
    if pairs.is_empty() {
        None
    } else {
        Some(pairs.len() as u32)
    }
}

pub fn reserve_memory(profile: &mut HardwareProfile, reserved_mb: u64) {
    reserve_vram(profile, reserved_mb);
    reserve_ram(profile, reserved_mb);
}

pub fn test_device(memory_mb: Option<u64>) -> DeviceInfo {
    DeviceInfo {
        llm_runtime: None,
        key: "pci:01:00.0".into(),
        backend: DeviceBackend::Cuda,
        ordinal: 0,
        vendor: "NVIDIA".into(),
        name: "Test GPU".into(),
        memory_total_mb: memory_mb,
        memory_free_mb: memory_mb,
        unified: false,
        discrete: true,
        memory_source: MemorySource::Nvml,
    }
}

pub fn reserve_vram(profile: &mut HardwareProfile, reserved_mb: u64) {
    for device in profile.devices.iter_mut() {
        device.memory_free_mb = Some(usable_memory_mb(device).saturating_sub(reserved_mb));
        if device.memory_source != MemorySource::Unknown {
            device.memory_source = MemorySource::Derived;
        }
    }
}

pub fn reserve_ram(profile: &mut HardwareProfile, reserved_mb: u64) {
    profile.ram.free_mb = profile.ram.free_mb.saturating_sub(reserved_mb);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirOrigin {
    Default,
    Configured,
}

pub fn detect_disk(models_dir: &Path, models_origin: DirOrigin, cache_dir: &Path) -> DiskInfo {
    DiskInfo {
        models_free_gb: free_gb(models_dir, models_origin),
        cache_free_gb: free_gb(cache_dir, DirOrigin::Default),
    }
}

fn free_gb(dir: &Path, origin: DirOrigin) -> f64 {
    let mut probe = dir;
    loop {
        match std::fs::symlink_metadata(probe) {
            Ok(_) if probe.exists() => break,
            Ok(_) => return 0.0,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return 0.0,
        }
        if origin == DirOrigin::Configured {
            return 0.0;
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => break,
        }
    }
    let bytes = fs4::available_space(probe).unwrap_or(0);
    (bytes as f64 / (1024.0 * 1024.0 * 1024.0) * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_native_windows_figures_are_bytes_reported_as_whole_megabytes() {
        let ram = ram_from_bytes(16 * 1024 * 1024 * 1024 + 512 * 1024, 6 * 1024 * 1024 * 1024);
        assert_eq!(ram.total_mb, 16 * 1024);
        assert_eq!(ram.free_mb, 6 * 1024);
        assert_eq!(ram.cgroup, cgroup::CgroupMemory::NONE);
    }

    #[test]
    fn nvidia_smi_csv_parsed() {
        let out = "NVIDIA GeForce RTX 3060, 12288, 11020\n";
        let found = parse_nvidia_smi(out);
        let gpu = found.first().expect("gpu");
        assert_eq!(gpu.vendor, "NVIDIA");
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 3060");
        assert_eq!(gpu.memory_total_mb, Some(12288));
        assert_eq!(gpu.memory_free_mb, Some(11020));
        assert_eq!(gpu.backend, DeviceBackend::Cuda);
    }

    #[test]
    fn nvidia_smi_name_with_comma_parsed() {
        let out = "NVIDIA RTX, Special Edition, 8192, 4096\n";
        let found = parse_nvidia_smi(out);
        let gpu = found.first().expect("gpu");
        assert_eq!(gpu.name, "NVIDIA RTX, Special Edition");
        assert_eq!(gpu.memory_total_mb, Some(8192));
    }

    #[test]
    fn nvidia_smi_garbage_rejected() {
        for bad in [
            "",
            "\n",
            "error: not found",
            "name, abc, def",
            "Failed to initialize NVML, Driver, not loaded",
        ] {
            assert!(
                parse_nvidia_smi(bad).is_empty(),
                "keeping a card whose memory columns read [N/A] must not also invent a card \
                 out of three comma-separated words of prose: {bad:?}"
            );
        }
    }

    #[test]
    fn nvidia_smi_keeps_a_named_card_whose_memory_columns_are_unreadable() {
        let gpu = parse_nvidia_smi("NVIDIA A100-SXM4, [N/A], [N/A], 00000000:07:00.0\n").remove(0);
        assert_eq!(gpu.name, "NVIDIA A100-SXM4");
        assert_eq!(gpu.key, "pci:0000:07:00.0");
        assert_eq!(gpu.memory_total_mb, None);
        assert_eq!(gpu.memory_free_mb, None);
        assert_eq!(
            gpu.memory_source,
            MemorySource::Unknown,
            "nvidia-smi reports [N/A] on MIG parents and some virtualised GPUs; dropping \
             the record made the card vanish from the inventory entirely"
        );
        assert_eq!(usable_memory_mb(&gpu), UNKNOWN_VRAM_FLOOR_MB);
    }

    #[test]
    fn nvidia_smi_falls_back_to_an_index_when_the_bus_id_is_unreadable() {
        let gpu = parse_nvidia_smi("NVIDIA A100-SXM4, 40960, 40000, [N/A]\n").remove(0);
        assert_eq!(
            gpu.key, "nvidia:0",
            "an unparsable bus id must not be pasted into a key that pretends to be one"
        );
        assert_eq!(gpu.memory_total_mb, Some(40960));
    }

    #[test]
    fn nvidia_smi_candidates_cover_platform_install_paths() {
        let candidates = nvidia_smi_candidates();
        assert!(
            !candidates.is_empty(),
            "bare-PATH lookup is the failure this exists to remove"
        );
        #[cfg(target_os = "linux")]
        assert!(
            candidates.iter().any(|p| p == Path::new("/usr/bin/nvidia-smi")),
            "gui-launched apps inherit thin PATHs; the absolute tool path must be tried: {candidates:?}"
        );
        #[cfg(target_os = "windows")]
        assert!(
            candidates
                .iter()
                .any(|p| p.to_string_lossy().contains("NVSMI")),
            "nvidia-smi is not on PATH on windows; the driver install dir must be tried: {candidates:?}"
        );
    }

    #[test]
    fn resolve_nvidia_smi_prefers_an_existing_candidate() {
        let dir = std::env::temp_dir().join(format!("txtcaster-nvsmi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let fake = dir.join("nvidia-smi");
        std::fs::write(&fake, b"").expect("temp probe stand-in");
        let resolved = resolve_nvidia_smi_in(&[dir.join("missing"), fake.clone()]);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            resolved,
            fake.to_string_lossy(),
            "an installed tool at a known path must win over PATH lookup"
        );
    }

    #[test]
    fn resolve_nvidia_smi_falls_back_to_path_lookup() {
        let resolved = resolve_nvidia_smi_in(&[std::path::PathBuf::from(
            "/nonexistent-txtcaster/nvidia-smi",
        )]);
        assert_eq!(
            resolved, "nvidia-smi",
            "no known install present: keep the old PATH behaviour, never fail closed"
        );
    }

    #[test]
    fn rocm_smi_parsed() {
        let out = "\
GPU[0]          : Card series:          Radeon RX 7900 XTX
GPU[0]          : VRAM Total Memory (B): 25753026560
GPU[0]          : VRAM Total Used Memory (B): 1073741824
";
        let found = parse_rocm_smi(out);
        let gpu = found.first().expect("gpu");
        assert_eq!(gpu.vendor, "AMD");
        assert_eq!(gpu.name, "Radeon RX 7900 XTX");
        assert_eq!(gpu.memory_total_mb, Some(24560));
        assert_eq!(gpu.memory_free_mb, Some(24560 - 1024));
        assert_eq!(gpu.backend, DeviceBackend::Rocm);
    }

    #[test]
    fn rocm_smi_keys_on_the_bus_id_so_the_vulkan_view_merges() {
        let out = "\
GPU[0]          : Card series:          Radeon RX 7900 XTX
GPU[0]          : PCI Bus: 0000:03:00.0
GPU[0]          : VRAM Total Memory (B): 25753026560
GPU[0]          : VRAM Total Used Memory (B): 1073741824
";
        let mut devices = parse_rocm_smi(out);
        assert_eq!(devices[0].key, "pci:0000:03:00.0");

        let vulkan = "\
GPU0:
\tdeviceType        = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
\tdeviceName        = AMD Radeon RX 7900 XTX (RADV)
VkPhysicalDevicePCIBusInfoPropertiesEXT:
\tpciDomain   = 0
\tpciBus      = 3
\tpciDevice   = 0
\tpciFunction = 0
memoryHeaps: count = 1
\tmemoryHeaps[0]:
\t\tsize   = 25753026560 (0x5fe000000) (23.98 GiB)
\t\tflags: count = 1
\t\t\tMEMORY_HEAP_DEVICE_LOCAL_BIT
";
        merge_vulkan(&mut devices, parse_vulkaninfo_views(vulkan));
        assert_eq!(
            devices.len(),
            1,
            "one AMD card, not one rocm entry plus one vulkan twin: {:?}",
            devices.iter().map(|d| &d.key).collect::<Vec<_>>()
        );
        assert_eq!(devices[0].memory_source, MemorySource::Rocm);
    }

    #[test]
    fn rocm_smi_reports_every_card_not_just_the_first() {
        let out = "\
GPU[0]          : Card series:          Radeon RX 7900 XTX
GPU[0]          : PCI Bus: 0000:03:00.0
GPU[0]          : VRAM Total Memory (B): 25753026560
GPU[1]          : Card series:          Radeon RX 7900 XT
GPU[1]          : PCI Bus: 0000:0a:00.0
GPU[1]          : VRAM Total Memory (B): 21474836480
";
        let devices = parse_rocm_smi(out);
        assert_eq!(
            devices.len(),
            2,
            "a fixed rocm:0 key collapsed a multi-card box to a single device"
        );
        assert_eq!(devices[0].key, "pci:0000:03:00.0");
        assert_eq!(devices[1].key, "pci:0000:0a:00.0");
        assert_eq!(devices[1].name, "Radeon RX 7900 XT");
        assert_eq!(devices[1].memory_total_mb, Some(20_480));
    }

    #[test]
    fn a_vulkan_block_with_no_device_type_leaves_discreteness_alone() {
        let mut devices = parse_nvidia_smi("NVIDIA A100-SXM4, 40960, 40000, 00000000:07:00.0\n");
        assert!(devices[0].discrete && !devices[0].unified);

        let typeless = "\
GPU0:
\tdeviceName        = NVIDIA A100-SXM4
VkPhysicalDevicePCIBusInfoPropertiesEXT:
\tpciDomain   = 0
\tpciBus      = 7
\tpciDevice   = 0
\tpciFunction = 0
";
        merge_vulkan(&mut devices, parse_vulkaninfo_views(typeless));
        assert_eq!(devices.len(), 1);
        assert!(
            devices[0].discrete && !devices[0].unified,
            "a block that never reported a deviceType says nothing about discreteness; \
             flipping an nvml-confirmed 40 GB card to unified clamps it to ram_free and \
             loses the selection tiebreak"
        );
    }

    #[test]
    fn a_virtual_vulkan_device_does_not_reclassify_a_passthrough_card() {
        let mut devices = parse_nvidia_smi("NVIDIA A100-SXM4, 40960, 40000, 00000000:07:00.0\n");
        let virtualised = "\
GPU0:
\tdeviceType        = PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU
\tdeviceName        = NVIDIA A100-SXM4
VkPhysicalDevicePCIBusInfoPropertiesEXT:
\tpciDomain   = 0
\tpciBus      = 7
\tpciDevice   = 0
\tpciFunction = 0
";
        merge_vulkan(&mut devices, parse_vulkaninfo_views(virtualised));
        assert!(
            devices[0].discrete && !devices[0].unified,
            "SR-IOV and passthrough report VIRTUAL_GPU, which is neither discrete nor \
             integrated; it must not overwrite what nvidia-smi established"
        );
    }

    #[test]
    fn vulkan_corrects_the_discreteness_a_vendor_tool_had_to_guess() {
        let mut devices =
            parse_rocm_smi("GPU[0] : Card series: Radeon 780M\nGPU[0] : PCI Bus: 0000:0c:00.0\n");
        assert!(
            devices[0].discrete && !devices[0].unified,
            "rocm-smi has no device-type column, so the parser has to assume"
        );

        let apu = "\
GPU0:
\tdeviceType        = PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU
\tdeviceName        = AMD Radeon 780M
VkPhysicalDevicePCIBusInfoPropertiesEXT:
\tpciDomain   = 0
\tpciBus      = 12
\tpciDevice   = 0
\tpciFunction = 0
";
        merge_vulkan(&mut devices, parse_vulkaninfo_views(apu));
        assert_eq!(devices.len(), 1);
        assert!(
            !devices[0].discrete && devices[0].unified,
            "an APU's memory is system RAM; budgeting it as a second dedicated pool \
             promises capacity that does not exist"
        );
    }

    #[test]
    fn rocm_smi_orders_blocks_numerically_not_as_text() {
        let mut out = String::new();
        for gpu in [0_u32, 2, 10] {
            out.push_str(&format!("GPU[{gpu}] : Card series: Card {gpu}\n"));
            out.push_str(&format!("GPU[{gpu}] : PCI Bus: 0000:{gpu:02x}:00.0\n"));
        }
        let devices = parse_rocm_smi(&out);
        assert_eq!(
            devices.iter().map(|d| d.ordinal).collect::<Vec<_>>(),
            vec![0, 2, 10],
            "keying the blocks on their text sorts GPU[10] before GPU[2], so the ordinals \
             stop matching the tool that reported them"
        );
        assert_eq!(devices[2].name, "Card 10");
    }

    #[test]
    fn rocm_smi_without_a_bus_id_falls_back_to_its_index() {
        let out = "GPU[0] : Card series: Radeon VII\n";
        assert_eq!(parse_rocm_smi(out)[0].key, "rocm:0");
    }

    #[test]
    fn rocm_smi_without_meminfo_still_names_gpu() {
        let out = "GPU[0] : Card series: Radeon VII\n";
        let found = parse_rocm_smi(out);
        let gpu = found.first().expect("gpu");
        assert_eq!(gpu.name, "Radeon VII");
        assert_eq!(gpu.memory_total_mb, None);
        assert_eq!(gpu.memory_free_mb, None);
    }

    #[test]
    fn vulkaninfo_summary_parsed() {
        let out = "\
Devices:
========
GPU0:
        apiVersion         = 1.3.277
        deviceName         = Intel(R) Arc(tm) A770 Graphics (DG2)
        deviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
";
        let found = parse_vulkaninfo(out);
        let gpu = found.first().expect("gpu");
        assert_eq!(gpu.vendor, "Intel");
        assert_eq!(gpu.name, "Intel(R) Arc(tm) A770 Graphics (DG2)");
        assert_eq!(gpu.memory_total_mb, None);
        assert_eq!(gpu.backend, DeviceBackend::Vulkan);
    }

    #[test]
    fn vulkaninfo_llvmpipe_software_renderer_rejected() {
        let out = "\
GPU0:
        deviceName         = llvmpipe (LLVM 17.0.6, 256 bits)
        deviceType         = PHYSICAL_DEVICE_TYPE_CPU
";
        assert!(parse_vulkaninfo(out).is_empty());
    }

    #[test]
    fn vulkaninfo_llvmpipe_first_real_gpu_second_picks_real_gpu() {
        let out = "\
Devices:
========
GPU0:
        deviceType         = PHYSICAL_DEVICE_TYPE_CPU
        deviceName         = llvmpipe (LLVM 17.0.6, 256 bits)
GPU1:
        deviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
        deviceName         = AMD Radeon RX 6700 XT (RADV NAVI22)
";
        let found = parse_vulkaninfo(out);
        let gpu = found.first().expect("gpu");
        assert_eq!(gpu.name, "AMD Radeon RX 6700 XT (RADV NAVI22)");
        assert_eq!(gpu.vendor, "AMD");
    }

    const REFERENCE_VULKANINFO: &str = "\
GPU0:
\tdeviceType        = PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU
\tdeviceName        = Intel(R) Iris(R) Xe Graphics (ADL GT2)
VkPhysicalDevicePCIBusInfoPropertiesEXT:
\tpciDomain   = 0
\tpciBus      = 0
\tpciDevice   = 2
\tpciFunction = 0
memoryHeaps: count = 1
\tmemoryHeaps[0]:
\t\tsize   = 12315740160 (0x2de134800) (11.47 GiB)
\t\tbudget = 6565134336 (0x187500000) (6.11 GiB)
\t\tusage  = 0 (0x00000000) (0.00 B)
\t\tflags: count = 1
\t\t\tMEMORY_HEAP_DEVICE_LOCAL_BIT
memoryTypes: count = 1
\tmemoryTypes[0]:
\t\theapIndex     = 0
GPU1:
\tdeviceType        = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
\tdeviceName        = NVIDIA GeForce RTX 3050 Laptop GPU
VkPhysicalDevicePCIBusInfoPropertiesEXT:
\tpciDomain   = 0
\tpciBus      = 1
\tpciDevice   = 0
\tpciFunction = 0
memoryHeaps: count = 2
\tmemoryHeaps[0]:
\t\tsize   = 4294967296 (0x100000000) (4.00 GiB)
\t\tbudget = 3949789184 (0xeb6d0000) (3.68 GiB)
\t\tusage  = 0 (0x00000000) (0.00 B)
\t\tflags: count = 1
\t\t\tMEMORY_HEAP_DEVICE_LOCAL_BIT
\tmemoryHeaps[1]:
\t\tsize   = 12315740160 (0x2de134800) (11.47 GiB)
\t\tbudget = 12315740160 (0x2de134800) (11.47 GiB)
\t\tusage  = 0 (0x00000000) (0.00 B)
\t\tflags:
\t\t\tNone
memoryTypes: count = 1
\tmemoryTypes[0]:
\t\theapIndex     = 1
";

    #[test]
    fn vulkaninfo_takes_the_largest_device_local_heap_not_the_last_one_seen() {
        let found = parse_vulkaninfo(REFERENCE_VULKANINFO);
        let nvidia = found
            .iter()
            .find(|d| d.vendor == "NVIDIA")
            .expect("the discrete card");
        assert_eq!(
            nvidia.memory_total_mb,
            Some(4096),
            "the reference card's second heap is 11.47 GiB of host-visible system memory \
             carrying no DEVICE_LOCAL flag; taking the last heap seen reported the whole \
             laptop's RAM as a 4 GB card's VRAM"
        );
        assert_eq!(
            nvidia.memory_free_mb,
            Some(3766),
            "budget must be read from the same heap index as the size that was chosen"
        );
    }

    #[test]
    fn vulkaninfo_budget_larger_than_its_heap_is_clamped_to_the_heap() {
        let out = "\
GPU0:
\tdeviceType        = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
\tdeviceName        = AMD Radeon RX 6700 XT
memoryHeaps: count = 1
\tmemoryHeaps[0]:
\t\tsize   = 4294967296 (0x100000000) (4.00 GiB)
\t\tbudget = 8589934592 (0x200000000) (8.00 GiB)
\t\tflags: count = 1
\t\t\tMEMORY_HEAP_DEVICE_LOCAL_BIT
";
        let gpu = parse_vulkaninfo(out).remove(0);
        assert_eq!(gpu.memory_total_mb, Some(4096));
        assert_eq!(gpu.memory_free_mb, Some(4096));
    }

    #[test]
    fn vulkaninfo_without_the_budget_extension_reports_unknown_free_never_total() {
        let out = "\
GPU0:
\tdeviceType        = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
\tdeviceName        = AMD Radeon RX 6700 XT
memoryHeaps: count = 1
\tmemoryHeaps[0]:
\t\tsize   = 12884901888 (0x300000000) (12.00 GiB)
\t\tflags: count = 1
\t\t\tMEMORY_HEAP_DEVICE_LOCAL_BIT
";
        let gpu = parse_vulkaninfo(out).remove(0);
        assert_eq!(gpu.memory_total_mb, Some(12288));
        assert_eq!(gpu.memory_free_mb, None);
        assert_eq!(gpu.memory_source, MemorySource::VkHeap);
    }

    #[test]
    fn vulkaninfo_keys_devices_on_the_full_pci_address() {
        let found = parse_vulkaninfo(REFERENCE_VULKANINFO);
        let keys: Vec<&str> = found.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["pci:0000:00:02.0", "pci:0000:01:00.0"],
            "slot and function are part of the address; hardcoding 00.0 put the iGPU at \
             the root of bus 0 and would collide with anything else sitting there"
        );
    }

    #[test]
    fn one_card_seen_by_nvidia_smi_and_by_vulkan_is_a_single_device() {
        let mut devices =
            parse_nvidia_smi("NVIDIA GeForce RTX 3050 Laptop GPU, 4096, 3767, 00000000:01:00.0\n");
        merge_vulkan(&mut devices, parse_vulkaninfo_views(REFERENCE_VULKANINFO));

        assert_eq!(
            devices.len(),
            2,
            "one NVIDIA card plus the Intel iGPU, not the card twice: {:?}",
            devices.iter().map(|d| &d.key).collect::<Vec<_>>()
        );
        let nvidia = devices
            .iter()
            .find(|d| d.vendor == "NVIDIA")
            .expect("the discrete card");
        assert_eq!(nvidia.key, "pci:0000:01:00.0");
        assert_eq!(nvidia.backend, DeviceBackend::Cuda);
        assert_eq!(nvidia.memory_free_mb, Some(3767));
        assert_eq!(nvidia.memory_source, MemorySource::Nvml);
    }

    #[test]
    fn a_vulkan_twin_of_a_card_cannot_outrank_the_probe_that_measured_it() {
        let mut devices =
            parse_nvidia_smi("NVIDIA GeForce RTX 3050 Laptop GPU, 4096, 3767, 00000000:01:00.0\n");
        merge_vulkan(&mut devices, parse_vulkaninfo_views(REFERENCE_VULKANINFO));

        let chosen = select_primary(&devices).expect("a device");
        assert_eq!(chosen.key, "pci:0000:01:00.0");
        assert_eq!(
            chosen.memory_free_mb,
            Some(3767),
            "an unmerged Vulkan twin advertised 11745 MB free for a 4 GB card and won the \
             memory tiebreak against the NVML entry that measured it"
        );
    }

    #[test]
    fn selection_prefers_the_discrete_card_over_a_bigger_shared_igpu() {
        let devices = vec![
            DeviceInfo {
                llm_runtime: None,
                key: "pci:00:02.0".into(),
                backend: DeviceBackend::Vulkan,
                ordinal: 0,
                vendor: "Intel".into(),
                name: "Intel(R) Iris(R) Xe Graphics".into(),
                memory_total_mb: Some(11745),
                memory_free_mb: Some(4473),
                unified: true,
                discrete: false,
                memory_source: MemorySource::VkBudget,
            },
            DeviceInfo {
                llm_runtime: None,
                key: "pci:01:00.0".into(),
                backend: DeviceBackend::Vulkan,
                ordinal: 1,
                vendor: "NVIDIA".into(),
                name: "NVIDIA GeForce RTX 3050 Laptop GPU".into(),
                memory_total_mb: Some(4096),
                memory_free_mb: Some(3900),
                unified: false,
                discrete: true,
                memory_source: MemorySource::VkBudget,
            },
        ];

        let chosen = select_primary(&devices).expect("a device");
        assert_eq!(
            chosen.name, "NVIDIA GeForce RTX 3050 Laptop GPU",
            "measured on the reference laptop, the integrated device advertises 11.47 GiB \
             of shared system memory against the discrete card's 4.00 GiB, so ranking on \
             raw capacity picks the worse device"
        );

        let big_model = select_for(&devices, Some(8000), 32000).expect("a device");
        assert_eq!(
            big_model.name, "Intel(R) Iris(R) Xe Graphics",
            "but a model the discrete card cannot hold belongs on the unified pool that \
             can; discreteness is the tiebreak, never the key"
        );
    }

    #[test]
    fn vulkaninfo_type_before_name_real_world_order_parsed() {
        let out = "\
GPU0:
        apiVersion         = 1.3.277
        deviceType         = PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU
        deviceName         = Intel(R) Iris(R) Xe Graphics
";
        let found = parse_vulkaninfo(out);
        let gpu = found.first().expect("gpu");
        assert_eq!(gpu.name, "Intel(R) Iris(R) Xe Graphics");
    }

    #[test]
    fn vulkaninfo_multiple_llvmpipe_only_devices_rejected() {
        let out = "\
GPU0:
        deviceType         = PHYSICAL_DEVICE_TYPE_CPU
        deviceName         = llvmpipe (LLVM 17.0.6, 256 bits)
GPU1:
        deviceType         = PHYSICAL_DEVICE_TYPE_CPU
        deviceName         = llvmpipe (LLVM 17.0.6, 128 bits)
";
        assert!(parse_vulkaninfo(out).is_empty());
    }

    #[test]
    fn meminfo_parsed() {
        let out = "\
MemTotal:       32657196 kB
MemFree:         1116004 kB
MemAvailable:   24041680 kB
";
        let ram = parse_meminfo(out);
        assert_eq!(ram.total_mb, 31891);
        assert_eq!(ram.free_mb, 23478);
    }

    #[test]
    fn meminfo_garbage_gives_zeroes() {
        let ram = parse_meminfo("not meminfo at all");
        assert_eq!(ram.total_mb, 0);
        assert_eq!(ram.free_mb, 0);
    }

    #[test]
    fn cpuinfo_physical_cores_counted() {
        let mut cpuinfo = String::new();
        for cpu in 0..8 {
            cpuinfo.push_str(&format!(
                "processor\t: {cpu}\nphysical id\t: 0\ncore id\t\t: {}\n\n",
                cpu % 4
            ));
        }
        assert_eq!(parse_cpuinfo_physical_cores(&cpuinfo), Some(4));
    }

    #[test]
    fn cpuinfo_without_topology_gives_none() {
        assert_eq!(parse_cpuinfo_physical_cores("processor : 0\n"), None);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn hung_probe_times_out() {
        let result = run_probe_with_timeout("sleep", &["5"], Duration::from_millis(100)).await;
        assert!(result.is_none());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn fast_probe_within_timeout_succeeds() {
        let result = run_probe_with_timeout("echo", &["ok"], Duration::from_secs(5)).await;
        assert_eq!(result.as_deref().map(str::trim), Some("ok"));
    }

    #[test]
    fn a_drm_entry_becomes_an_identity_only_device() {
        let device =
            parse_drm_entry("card0", "0000:01:00.0", Some("0x10de\n")).expect("a pci card");
        assert_eq!(device.key, "pci:0000:01:00.0");
        assert_eq!(device.vendor, "NVIDIA");
        assert_eq!(device.ordinal, 0);
        assert_eq!(device.backend, DeviceBackend::Vulkan);
        assert!(crate::hardware::memory_is_unreadable(&device));
        assert_eq!(usable_memory_mb(&device), UNKNOWN_VRAM_FLOOR_MB);
        assert!(
            !device.discrete && !device.unified,
            "sysfs reports identity, not capability; guessing a hierarchy here would beat \
             the bus-id tiebreak with a coin flip"
        );
    }

    #[test]
    fn a_drm_entry_key_is_byte_identical_to_the_other_probes() {
        let sysfs = parse_drm_entry("card1", "0000:00:02.0", Some("0x8086")).expect("a pci card");
        let nvidia = parse_nvidia_smi("Intel, 1024, 512, 00000000:00:02.0\n").remove(0);
        assert_eq!(
            sysfs.key, nvidia.key,
            "a sysfs key that differs by one character grows a phantom device in the merge"
        );
    }

    #[test]
    fn drm_entries_that_are_not_pci_cards_are_skipped() {
        assert!(parse_drm_entry("card0-HDMI-A-1", "0000:01:00.0", None).is_none());
        assert!(parse_drm_entry("renderD128", "0000:01:00.0", None).is_none());
        assert!(parse_drm_entry("version", "0000:01:00.0", None).is_none());
        assert!(
            parse_drm_entry("card0", "virtio0", None).is_none(),
            "a virtio or platform device has no pci address and must not be keyed as if it did"
        );
    }

    #[test]
    fn an_unknown_vendor_id_still_yields_a_usable_device() {
        let device = parse_drm_entry("card3", "0000:04:00.0", Some("0xbeef")).expect("a card");
        assert_eq!(device.vendor, "Unknown");
        assert_eq!(device.ordinal, 3);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sysfs_enumerates_this_machines_pci_cards() {
        if !std::path::Path::new(SYSFS_DRM).is_dir() {
            return;
        }
        let found = sysfs_devices();
        if found.is_empty() {
            return;
        }
        for device in found {
            assert!(device.key.starts_with("pci:"), "{}", device.key);
            assert!(crate::hardware::memory_is_unreadable(&device));
        }
    }

    #[test]
    fn pci_vendor_ids_map_to_the_vendors_we_name() {
        assert_eq!(pci_vendor_name("0x10de"), "NVIDIA");
        assert_eq!(pci_vendor_name("0x1002"), "AMD");
        assert_eq!(pci_vendor_name("0x8086"), "Intel");
        assert_eq!(pci_vendor_name("0xbeef"), "Unknown");
    }

    #[tokio::test]
    async fn an_empty_enumeration_is_reprobed_not_cached() {
        let devices = super::device_inventory().await;
        if devices.is_empty() {
            assert!(
                super::DEVICE_INVENTORY.get().is_none(),
                "a process that starts before the driver or the container's device plugin \
                 attaches would be CPU-only for its whole lifetime with no error"
            );
        } else {
            assert!(super::DEVICE_INVENTORY.get().is_some());
        }
    }

    #[tokio::test]
    async fn detect_never_panics_and_returns_sane_values() {
        let dir = std::env::temp_dir();
        let profile = detect(&dir.join("models"), DirOrigin::Default, &dir.join("cache")).await;
        assert!(profile.cpu.cores_logical >= 1);
        assert!(profile.cpu.cores_physical >= 1);
        assert!(profile.disk.models_free_gb >= 0.0);
        let json = serde_json::to_value(&profile).expect("serialize");
        assert!(json.get("ram").is_some());
    }

    #[test]
    fn detect_disk_probes_existing_and_unborn_paths() {
        let dir = std::env::temp_dir();
        let disk = detect_disk(
            &dir,
            DirOrigin::Default,
            &dir.join("not-created-yet").join("cache"),
        );
        assert!(disk.models_free_gb > 0.0);
        assert!(disk.cache_free_gb > 0.0);
        assert_eq!(disk.models_free_gb, disk.cache_free_gb);
    }

    #[test]
    fn a_dangling_symlink_reports_nothing_rather_than_the_filesystem_it_sits_on() {
        let root = std::env::temp_dir().join(format!("txtcaster-dangling-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let link = root.join("models");
        let target = root.join("never-mounted");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        #[cfg(unix)]
        {
            let verdict = free_gb(&link, DirOrigin::Default);
            std::fs::remove_dir_all(&root).ok();
            assert_eq!(
                verdict, 0.0,
                "a symlink whose target is gone is how a relocated models dir looks when its \
                 drive is unmounted; walking past it reports the wrong filesystem"
            );
        }
        #[cfg(not(unix))]
        {
            let _ = (link, target);
            std::fs::remove_dir_all(&root).ok();
        }
    }

    #[test]
    fn a_configured_models_dir_that_is_not_there_reports_nothing_rather_than_another_drive() {
        let absent = std::env::temp_dir().join("txtcaster-absent-models-drive");
        std::fs::remove_dir_all(&absent).ok();

        let honest = detect_disk(&absent, DirOrigin::Configured, &std::env::temp_dir());
        assert_eq!(
            honest.models_free_gb, 0.0,
            "an unmounted drive must not borrow an ancestor's free space"
        );

        let borrowed = detect_disk(&absent, DirOrigin::Default, &std::env::temp_dir());
        assert!(
            borrowed.models_free_gb > 0.0,
            "a default dir that has not been created yet is on the ancestor's filesystem, \
             because nothing can be mounted on a path component that does not exist"
        );
    }
}
