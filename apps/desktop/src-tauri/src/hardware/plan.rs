use serde::Serialize;

use crate::hardware::device::{DeviceBackend, DevicePlan, PLAN_REVISION};

use crate::catalog::gguf::GgufModelInfo;
use crate::catalog::{CatalogEntry, ModelFormat, Perf};
use crate::hardware::{HardwareProfile, MemorySource, RamInfo};

pub const VRAM_RESERVE_MB: u64 = 512;
pub const VRAM_HEADROOM: f64 = 0.8;
pub const RAM_HEADROOM: f64 = 0.8;
pub const HYBRID_MIN_FRACTION: f64 = 0.3;
pub const COMPUTE_SLACK_MB: u64 = 448;
pub const CTX_LADDER: [u64; 4] = [32_768, 16_384, 8_192, 4_096];
pub const DEFAULT_BATCH: u64 = 512;

pub fn compute_buffer_bytes(info: &GgufModelInfo, batch: u64) -> u64 {
    let logits = batch
        .saturating_mul(info.n_vocab)
        .saturating_mul(std::mem::size_of::<f32>() as u64);
    logits.saturating_add(COMPUTE_SLACK_MB * 1024 * 1024)
}

/// The host-RAM bytes a plan of a given shape occupies, broken out by term.
///
/// Every host-RAM fit decision in the tree goes through this type. Before it
/// existed the four decision sites each counted a different subset — the Orange
/// CPU fallback charged weights only, the no-GPU path charged weights and KV,
/// the hybrid path charged the compute buffer to VRAM even when it had planned a
/// CPU-resident plan, and `fallback_cpu_plan` charged nothing at all. Keeping the
/// arithmetic in one place is the point: the terms cannot drift apart again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamCharge {
    /// Model weights resident on the host.
    pub weights: u64,
    /// KV cache, when it is not offloaded to a device.
    pub kv: u64,
    /// llama.cpp compute buffer, when the plan runs on the host.
    pub compute: u64,
}

impl RamCharge {
    pub fn total(self) -> u64 {
        self.weights
            .saturating_add(self.kv)
            .saturating_add(self.compute)
    }
}

/// Host-RAM charge for a plan that keeps `gpu_weight_bytes` of weights on a
/// device.
///
/// `kv_on_gpu` and `compute_on_gpu` say which of the two non-weight terms the
/// device budget already carries. Both are false for any plan that ends up on
/// `DevicePlan::cpu()`, including the `n_gpu == 0` outcome of the hybrid
/// packer — that case emits a CPU plan, so its compute buffer is host memory
/// however it was budgeted upstream.
pub fn ram_charge(
    info: &GgufModelInfo,
    gpu_weight_bytes: u64,
    ctx: u64,
    batch: u64,
    kv_on_gpu: bool,
    compute_on_gpu: bool,
) -> RamCharge {
    RamCharge {
        weights: info.total_bytes().saturating_sub(gpu_weight_bytes),
        kv: if kv_on_gpu {
            0
        } else {
            info.kv_bytes_per_token.saturating_mul(ctx)
        },
        compute: if compute_on_gpu {
            0
        } else {
            compute_buffer_bytes(info, batch)
        },
    }
}

/// Host-RAM charge for a plan that runs entirely on the CPU. All three terms
/// are charged by definition.
pub fn cpu_ram_charge(info: &GgufModelInfo, ctx: u64, batch: u64) -> RamCharge {
    ram_charge(info, 0, ctx, batch, false, false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FitBadge {
    Green,
    Yellow,
    Orange,
    Red,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourcePlan {
    pub model_id: String,
    pub plan_revision: u64,
    pub device: DevicePlan,
    pub n_gpu_layers: u32,
    pub n_layers_total: u32,
    pub kv_offload: bool,
    pub ctx: u64,
    pub batch: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tok_s_est: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_ram_planned_mb: Option<u64>,
    pub badge: FitBadge,
}

#[derive(Debug, Clone)]
pub struct ModelFit {
    pub badge: FitBadge,
    pub plan: Option<ResourcePlan>,
}

pub fn compute_fit(
    entry: &CatalogEntry,
    profile: &HardwareProfile,
    gguf: Option<&GgufModelInfo>,
) -> ModelFit {
    let fit = match (entry.format, gguf) {
        (ModelFormat::Gguf, Some(info)) => gguf_fit(entry, profile, info),
        _ => heuristic_fit(entry, profile),
    };
    cap_unreadable_device(entry, profile, fit)
}

fn cap_unreadable_device(
    entry: &CatalogEntry,
    profile: &HardwareProfile,
    mut fit: ModelFit,
) -> ModelFit {
    let Some(device) = planned_gpu(entry, profile) else {
        return fit;
    };
    if !crate::hardware::memory_is_unreadable(device) {
        return fit;
    }
    fit.badge = demote_green(fit.badge);
    if let Some(plan) = fit.plan.as_mut() {
        plan.badge = demote_green(plan.badge);
    }
    fit
}

fn demote_green(badge: FitBadge) -> FitBadge {
    match badge {
        FitBadge::Green => FitBadge::Yellow,
        worse => worse,
    }
}

pub const CLIP_ENCODE_RESERVE_MB: u64 = 640;
pub const VISION_CTX_CAP: u64 = 8_192;

pub fn projector_reserve_mb(mmproj_bytes: u64) -> u64 {
    mmproj_bytes.div_ceil(1024 * 1024) + CLIP_ENCODE_RESERVE_MB
}

fn vision_entry(entry: &CatalogEntry) -> CatalogEntry {
    let mut entry = entry.clone();
    entry.ctx_max = Some(
        entry
            .ctx_max
            .map_or(VISION_CTX_CAP, |m| m.min(VISION_CTX_CAP)),
    );
    entry
}

pub fn vision_fit(
    entry: &CatalogEntry,
    profile: &HardwareProfile,
    gguf: Option<&GgufModelInfo>,
    mmproj_bytes: u64,
) -> ModelFit {
    let entry = vision_entry(entry);
    let mut vram_profile = profile.clone();
    let reserve_mb = projector_reserve_mb(mmproj_bytes);

    let capable = capable_device(&entry, profile);
    let measured_gpu_budget = profile
        .devices
        .iter()
        .filter(|d| d.backend.is_gpu() && d.memory_source != MemorySource::Unknown)
        .filter(|d| capable.map(|c| c.backend == d.backend).unwrap_or(true))
        .filter_map(|d| {
            if d.discrete {
                Some(crate::hardware::budget_mb(
                    d,
                    effective_free_mb(&profile.ram),
                ))
            } else if d.backend == DeviceBackend::Vulkan
                || d.memory_source == MemorySource::VkBudget
            {
                d.memory_free_mb
            } else {
                None
            }
        })
        .min();

    if let Some(limit) = measured_gpu_budget {
        for device in vram_profile.devices.iter_mut() {
            if capable.map(|c| c.key == device.key).unwrap_or(true) {
                if let Some(free) = device.memory_free_mb {
                    device.memory_free_mb = Some(free.min(limit));
                }
            }
        }
    }

    crate::hardware::reserve_vram(&mut vram_profile, reserve_mb);
    let fit = compute_fit(&entry, &vram_profile, gguf);
    if fit.plan.as_ref().is_some_and(|plan| !plan.device.is_gpu()) {
        // The packer landed on the host (a GPU too small for the projector
        // reserve packs zero slots), but this profile charged the projector
        // to VRAM only — the RAM gate that just accepted this plan never saw
        // the ~projector_reserve_mb the engine will load into host memory
        // (#163 review F1, a #154 escape). Re-gate through the CPU path,
        // whose profile subtracts the projector from the RAM the gate
        // evaluates: the same placement either fits honestly or refuses
        // loudly, and the recorded budget equals what the gate charged.
        return clamped_vision_cpu_fit(&entry, profile, gguf, mmproj_bytes);
    }

    if let Some(plan) = fit.plan.as_ref().filter(|p| p.device.is_gpu()) {
        let model_gpu_mb = if let Some(info) = gguf {
            let gpu_blocks = plan.n_gpu_layers.saturating_sub(1);
            let gpu_bytes: u64 = info
                .layer_bytes
                .iter()
                .rev()
                .take(gpu_blocks as usize)
                .sum();
            let scratch = compute_buffer_bytes(info, plan.batch);
            let kv_bytes = if plan.kv_offload {
                info.kv_bytes_per_token.saturating_mul(plan.ctx)
            } else {
                0
            };
            let bytes = gpu_bytes
                .saturating_add(info.output_bytes)
                .saturating_add(scratch)
                .saturating_add(kv_bytes);
            bytes.div_ceil(1024 * 1024)
        } else {
            entry.full_gpu_vram_mb.unwrap_or(0)
        };

        let total_vram_mb = reserve_mb.saturating_add(model_gpu_mb);
        let effective_budget = measured_gpu_budget.or_else(|| {
            capable_device(&entry, profile)
                .map(|d| crate::hardware::budget_mb(d, effective_free_mb(&profile.ram)))
        });

        if let Some(budget) = effective_budget {
            if total_vram_mb > budget {
                return clamped_vision_cpu_fit(&entry, profile, gguf, mmproj_bytes);
            }
        }
    }

    budget_projector_on_host(fit, reserve_mb)
}

pub fn vision_cpu_fit(
    entry: &CatalogEntry,
    profile: &HardwareProfile,
    gguf: Option<&GgufModelInfo>,
    mmproj_bytes: u64,
) -> ModelFit {
    let entry = vision_entry(entry);
    clamped_vision_cpu_fit(&entry, profile, gguf, mmproj_bytes)
}

/// Shared CPU-resident vision path: the projector is reserved out of the RAM
/// the gate sees (it loads into host memory on a CPU plan), and the devices
/// are zeroed so the packer cannot wander back onto a GPU the caller has
/// already ruled out. `entry` must already be ctx-clamped by [`vision_entry`].
fn clamped_vision_cpu_fit(
    entry: &CatalogEntry,
    profile: &HardwareProfile,
    gguf: Option<&GgufModelInfo>,
    mmproj_bytes: u64,
) -> ModelFit {
    let mut profile = profile.clone();
    crate::hardware::reserve_ram(&mut profile, projector_reserve_mb(mmproj_bytes));
    for device in profile.devices.iter_mut() {
        device.memory_free_mb = Some(0);
    }
    budget_projector_on_host(
        compute_fit(entry, &profile, gguf),
        projector_reserve_mb(mmproj_bytes),
    )
}

/// The projector reservation is taken out of the *profile* before the gate
/// runs, so the inner charge never contains it — but a CPU-resident vision
/// plan loads the projector into host RAM, and a recorded budget that omits
/// it would false-warn on every correct vision load. A GPU plan keeps the
/// projector on the device, where the VRAM reservation already carried it.
fn budget_projector_on_host(mut fit: ModelFit, reserve_mb: u64) -> ModelFit {
    if let Some(plan) = fit.plan.as_mut() {
        if !plan.device.is_gpu() {
            plan.host_ram_planned_mb = plan.host_ram_planned_mb.map(|mb| mb + reserve_mb);
        }
    }
    fit
}

fn gguf_fit(entry: &CatalogEntry, profile: &HardwareProfile, info: &GgufModelInfo) -> ModelFit {
    let chosen = capable_device(entry, profile);
    let vram_free =
        chosen.map(|device| crate::hardware::budget_mb(device, effective_free_mb(&profile.ram)));
    let gate_mb = ram_gate_mb(&profile.ram);
    let n_layers = info.n_layers;

    for &ctx in ladder(entry.ctx_max).iter() {
        if let Some(plan) = plan_for_ctx(entry, info, chosen, vram_free, gate_mb, ctx) {
            return ModelFit {
                badge: plan.badge,
                plan: Some(plan),
            };
        }
    }

    // Full-CPU fallback. This used to gate on `total_bytes` — weights only —
    // while the loop above gated the same plan on weights + KV. It was not a
    // second opinion; it was a bypass, and the only thing it could do was accept
    // a plan the gate had just refused.
    //
    // Counted correctly it can never accept, and that is a theorem rather than a
    // hope: `ladder()` is never empty so the loop always runs, its final
    // iteration uses exactly the ctx this branch picks, and a hybrid charge is
    // never larger than the full-CPU charge at the same ctx (weights are reduced
    // by the GPU-resident bytes, and KV and compute are each either the same term
    // or zero). So the loop falling through means the full-CPU charge is over the
    // gate too. Kept rather than deleted because it is harmless once correct, and
    // pinned by `cpu_fallback_never_accepts_what_the_ladder_refused` so that
    // reintroducing a weaker term here fails a test instead of a machine.
    let ctx = *ladder(entry.ctx_max).last().unwrap_or(&4_096);
    let charge = cpu_ram_charge(info, ctx, DEFAULT_BATCH).total();
    if fits_ram(charge, gate_mb) {
        let plan = cpu_plan(entry, n_layers, ctx, FitBadge::Orange, charge);
        return ModelFit {
            badge: FitBadge::Orange,
            plan: Some(plan),
        };
    }
    ModelFit {
        badge: FitBadge::Red,
        plan: None,
    }
}

fn plan_for_ctx(
    entry: &CatalogEntry,
    info: &GgufModelInfo,
    chosen: Option<&crate::hardware::DeviceInfo>,
    vram_free: Option<u64>,
    gate_mb: u64,
    ctx: u64,
) -> Option<ResourcePlan> {
    let n_layers = info.n_layers;

    let kv_bytes = info.kv_bytes_per_token.saturating_mul(ctx);

    let Some(vram_free) = vram_free else {
        // No capable device: weights, KV and the compute buffer are all host
        // memory. The compute buffer used to be missing here because `scratch`
        // is not computed until after this early return.
        let charge = cpu_ram_charge(info, ctx, DEFAULT_BATCH).total();
        return fits_ram(charge, gate_mb)
            .then(|| cpu_plan(entry, n_layers, ctx, FitBadge::Orange, charge));
    };

    let budget_mb = vram_free.saturating_sub(VRAM_RESERVE_MB);
    let budget = budget_mb.saturating_mul(1024 * 1024);
    let scratch = compute_buffer_bytes(info, DEFAULT_BATCH);

    let full_slots = n_layers.saturating_add(1);
    let mut kv_offload = true;
    let mut n_gpu = packed_layers(info, budget, kv_bytes.saturating_add(scratch));
    if n_gpu < full_slots {
        let retry = packed_layers(info, budget, scratch);
        if retry > n_gpu {
            n_gpu = retry;
            kv_offload = false;
        }
    }
    if n_gpu == 0 {
        kv_offload = false;
    }

    let gpu_blocks = n_gpu.saturating_sub(1);
    let gpu_bytes: u64 = info
        .layer_bytes
        .iter()
        .rev()
        .take(gpu_blocks as usize)
        .sum();
    // `scratch` was charged against the device budget at the packing step above,
    // which is right while layers are on the device. When the packer lands on
    // zero slots the emitted plan is `DevicePlan::cpu()`, so the compute buffer
    // is host memory and has to be charged here instead.
    let compute_on_gpu = n_gpu > 0;
    let charge = ram_charge(
        info,
        gpu_bytes,
        ctx,
        DEFAULT_BATCH,
        kv_offload,
        compute_on_gpu,
    );
    if !fits_ram(charge.total(), gate_mb) {
        return None;
    }

    let frac_gpu = if n_layers > 0 {
        f64::from(gpu_blocks) / f64::from(n_layers)
    } else {
        0.0
    };
    let badge = if n_gpu == full_slots && kv_offload {
        FitBadge::Green
    } else if frac_gpu >= HYBRID_MIN_FRACTION && entry.hybrid_capable {
        FitBadge::Yellow
    } else {
        FitBadge::Orange
    };
    let device = if n_gpu > 0 {
        device_plan(chosen)
    } else {
        DevicePlan::cpu()
    };
    let is_gpu = device.is_gpu();
    Some(ResourcePlan {
        model_id: entry.id.clone(),
        plan_revision: PLAN_REVISION,
        device,
        n_gpu_layers: if is_gpu { n_gpu } else { 0 },
        n_layers_total: n_layers,
        kv_offload: is_gpu && kv_offload,
        ctx,
        batch: DEFAULT_BATCH,
        tok_s_est: tok_s_est(entry.perf.as_ref(), frac_gpu),
        host_ram_planned_mb: Some(planned_mb(charge.total())),
        badge,
    })
}

/// The budget recorded for the residency check rounds *up*: the gate's own
/// rule is `floor` so the gate never rounds itself into memory it does not
/// have, but a recorded budget floored below the evaluated charge would make
/// an exactly-planned load appear to exceed its plan by a sub-MB truncation.
fn planned_mb(charge_bytes: u64) -> u64 {
    charge_bytes.div_ceil(1024 * 1024)
}

fn packed_layers(info: &GgufModelInfo, budget: u64, fixed: u64) -> u32 {
    let mut used = fixed.saturating_add(info.output_bytes);
    if used > budget {
        return 0;
    }
    let mut n = 1u32;
    for &bytes in info.layer_bytes.iter().rev() {
        let next = used.saturating_add(bytes);
        if next > budget {
            break;
        }
        used = next;
        n += 1;
    }
    n
}

fn cpu_plan(
    entry: &CatalogEntry,
    n_layers: u32,
    ctx: u64,
    badge: FitBadge,
    charge_bytes: u64,
) -> ResourcePlan {
    ResourcePlan {
        model_id: entry.id.clone(),
        plan_revision: PLAN_REVISION,
        device: DevicePlan::cpu(),
        n_gpu_layers: 0,
        n_layers_total: n_layers,
        kv_offload: false,
        ctx,
        batch: DEFAULT_BATCH,
        tok_s_est: tok_s_est(entry.perf.as_ref(), 0.0),
        host_ram_planned_mb: Some(planned_mb(charge_bytes)),
        badge,
    }
}

pub const TTS_BATCH_LADDER: [u64; 4] = [8, 4, 2, 1];
pub const DEFAULT_TTS_WORKERS: u64 = 2;
pub const DEFAULT_ALIGN_WORKERS: u64 = 2;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TtsPlan {
    pub model_id: String,
    pub plan_revision: u64,
    pub device: DevicePlan,
    pub tts_workers: u64,
    pub batch: u64,
    pub badge: FitBadge,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AlignPlan {
    pub model_id: String,
    pub plan_revision: u64,
    pub device: DevicePlan,
    pub align_workers: u64,
    pub badge: FitBadge,
}

pub(crate) fn device_plan(device: Option<&crate::hardware::DeviceInfo>) -> DevicePlan {
    match device {
        Some(device) => {
            DevicePlan::gpu(device.backend, device.ordinal).with_key(Some(device.key.clone()))
        }
        None => DevicePlan::cpu(),
    }
}

fn runtime_can_use(entry: &CatalogEntry, device: &crate::hardware::DeviceInfo) -> bool {
    match entry.format {
        ModelFormat::Gguf => device.backend.is_gpu() && device.llm_runtime != Some(false),
        ModelFormat::Ct2 => false,
        _ => device.backend == DeviceBackend::Cuda,
    }
}

pub(crate) fn capable_device<'a>(
    entry: &CatalogEntry,
    profile: &'a HardwareProfile,
) -> Option<&'a crate::hardware::DeviceInfo> {
    let usable: Vec<&crate::hardware::DeviceInfo> = profile
        .devices
        .iter()
        .filter(|device| runtime_can_use(entry, device))
        .collect();
    // llama.cpp offloads to an integrated GPU only when no discrete GPU is present
    // (llama_prepare_model_devices), so a GGUF plan must budget the card it will run on.
    let discrete_only =
        entry.format == ModelFormat::Gguf && usable.iter().any(|device| device.discrete);
    crate::hardware::select_among(
        usable
            .into_iter()
            .filter(|device| !discrete_only || device.discrete),
        entry.full_gpu_vram_mb.map(required_mb),
        // Selection and budgeting must agree about how much memory a device
        // has. `gguf_fit` computes the chosen device's budget from
        // `effective_free_mb`, so ranking candidates by the raw host figure
        // would let a unified device — whose "VRAM" *is* host memory, and so is
        // bounded by the cgroup — out-rank a discrete card that the cgroup does
        // not bound at all. Two answers to one question is the defect this task
        // is about; this is the fifth and last site that had one.
        effective_free_mb(&profile.ram),
    )
}

fn required_mb(need: u64) -> u64 {
    (need as f64 / VRAM_HEADROOM).ceil() as u64
}

fn planned_gpu<'a>(
    entry: &CatalogEntry,
    profile: &'a HardwareProfile,
) -> Option<&'a crate::hardware::DeviceInfo> {
    let need = entry.full_gpu_vram_mb?;
    let device = capable_device(entry, profile)?;
    let budget = crate::hardware::budget_mb(device, effective_free_mb(&profile.ram));
    ((budget as f64 * VRAM_HEADROOM) >= need as f64).then_some(device)
}

pub fn planned_device(entry: &CatalogEntry, profile: &HardwareProfile) -> DevicePlan {
    device_plan(planned_gpu(entry, profile))
}

pub fn planned_on_unreadable_memory(entry: &CatalogEntry, profile: &HardwareProfile) -> bool {
    planned_gpu(entry, profile).is_some_and(crate::hardware::memory_is_unreadable)
}

fn cpu_badge(entry: &CatalogEntry, profile: &HardwareProfile) -> FitBadge {
    if entry.min_free_ram_mb <= profile.ram.free_mb {
        FitBadge::Orange
    } else {
        FitBadge::Red
    }
}

fn tts_batch(
    entry: &CatalogEntry,
    profile: &HardwareProfile,
    chosen: Option<&crate::hardware::DeviceInfo>,
) -> u64 {
    let Some(chosen) = chosen else {
        return TTS_BATCH_LADDER[2];
    };
    let Some(need) = entry.full_gpu_vram_mb else {
        return TTS_BATCH_LADDER[0];
    };
    let budget =
        crate::hardware::budget_mb(chosen, effective_free_mb(&profile.ram)) as f64 * VRAM_HEADROOM;
    for &batch in TTS_BATCH_LADDER.iter() {
        let headroom = need as f64 * (1.0 + (batch as f64 / TTS_BATCH_LADDER[0] as f64) * 0.25);
        if budget >= headroom {
            return batch;
        }
    }
    TTS_BATCH_LADDER[TTS_BATCH_LADDER.len() - 1]
}

pub fn tts_fit(entry: &CatalogEntry, profile: &HardwareProfile) -> TtsPlan {
    let chosen = planned_gpu(entry, profile);
    TtsPlan {
        model_id: entry.id.clone(),
        plan_revision: PLAN_REVISION,
        device: device_plan(chosen),
        tts_workers: DEFAULT_TTS_WORKERS,
        batch: tts_batch(entry, profile, chosen),
        badge: stage_badge(entry, profile, chosen),
    }
}

/// The badge a whole-model stage plan carries.
fn stage_badge(
    entry: &CatalogEntry,
    profile: &HardwareProfile,
    chosen: Option<&crate::hardware::DeviceInfo>,
) -> FitBadge {
    match chosen {
        Some(device) if crate::hardware::memory_is_unreadable(device) => FitBadge::Yellow,
        Some(_) => FitBadge::Green,
        None => cpu_badge(entry, profile),
    }
}

pub fn align_fit(entry: &CatalogEntry, profile: &HardwareProfile) -> AlignPlan {
    AlignPlan {
        model_id: entry.id.clone(),
        plan_revision: PLAN_REVISION,
        device: device_plan(None),
        align_workers: DEFAULT_ALIGN_WORKERS,
        badge: stage_badge(entry, profile, None),
    }
}

fn heuristic_fit(entry: &CatalogEntry, profile: &HardwareProfile) -> ModelFit {
    let vram_free = capable_device(entry, profile)
        .map(|device| crate::hardware::budget_mb(device, effective_free_mb(&profile.ram)))
        .unwrap_or(0);
    let ram_free = profile.ram.free_mb;
    let vram_budget = vram_free as f64 * VRAM_HEADROOM;

    let badge = match entry.full_gpu_vram_mb {
        Some(need) if vram_budget >= need as f64 => FitBadge::Green,
        Some(need) if entry.hybrid_capable && vram_budget >= need as f64 * HYBRID_MIN_FRACTION => {
            FitBadge::Yellow
        }
        Some(_) | None => {
            if entry.min_free_ram_mb <= ram_free {
                if entry.full_gpu_vram_mb.is_none() {
                    FitBadge::Green
                } else {
                    FitBadge::Orange
                }
            } else {
                FitBadge::Red
            }
        }
    };
    ModelFit { badge, plan: None }
}

pub const MIN_CTX: u64 = 512;

fn ladder(ctx_max: Option<u64>) -> Vec<u64> {
    let cap = ctx_max.unwrap_or(u64::MAX);
    let steps: Vec<u64> = CTX_LADDER.iter().copied().filter(|&c| c <= cap).collect();
    if steps.is_empty() {
        vec![cap.clamp(MIN_CTX, 4_096)]
    } else {
        steps
    }
}

/// Nominal capacity a model may claim, independent of what is free right now.
/// WARP `docs/ENGINE.md` §3 uses the same fraction for the same reason: a
/// budget that tracks only free memory over-plans on a machine that is briefly
/// idle and then loses the race to whatever wakes up next.
pub const CAPACITY_FRACTION: f64 = 0.75;

/// Host memory held back for everything that is not the model.
///
/// A named constant with a stated rationale in the same register as
/// `UNKNOWN_VRAM_FLOOR_MB`, deliberately **not** tuned to a machine. AGENTS §5.2
/// says budgets scale and magic numbers do not, and this is the exception that
/// has to be argued: a fraction of a small pool is not an operating system.
/// `RAM_HEADROOM` alone leaves 20 % of *free*, which on a tight machine is a few
/// hundred MB for the compositor, the webview and FFmpeg together.
///
/// Derived from a stated composition of the roles txtCaster needs resident to
/// stay responsive, rather than from measurements of one box:
///
/// | role | figure | basis |
/// |---|---|---|
/// | compositor + display server | 400 MB | 33 MB measured under Hyprland here; GNOME/KDE are the 300–600 MB population this must hold for, and the lean case is not the population |
/// | Tauri webview (WebKitGTK / WebView2) | 500 MB | browser-engine processes measured 297–838 MB on one host at one instant; an Electron analogue was 479 MB |
/// | FFmpeg transcode working set | 120 MB | transient, sized to a decode+encode pair |
///
/// The spread inside a single role — 297 to 838 MB for the browser engine, on
/// one machine at one instant — is the argument against fitting this constant to
/// any machine, including the one it was measured on.
///
/// What makes an imperfect constant tolerable here is an asymmetry, worth being
/// explicit about because it would not hold on a refusal path. `reserve_term` is
/// a third term of the same `min`, so it can only ever shrink the budget;
/// `gate_mb` clamps at zero; and a zero gate is rung 3, which emits the smallest
/// plan with 🔴 rather than refusing. A value that is too large costs
/// throughput. It cannot freeze a machine and it cannot make a model
/// unavailable.
pub const OS_RESERVE_MB: u64 = 1024;

/// Nominal capacity: the host's RAM, or the cgroup's limit when that is tighter.
///
/// The host figure is kept as the other half of the `min` rather than replaced,
/// so a limit larger than the machine cannot inflate the budget.
pub fn usable_ram_mb(ram: &RamInfo) -> u64 {
    ram.cgroup
        .limit_mb
        .map_or(ram.total_mb, |limit| limit.min(ram.total_mb))
}

/// Memory actually obtainable right now: `MemAvailable`, or the cgroup's
/// headroom when the hierarchy will admit less than the host has free.
pub fn effective_free_mb(ram: &RamInfo) -> u64 {
    ram.cgroup
        .headroom_mb
        .map_or(ram.free_mb, |headroom| headroom.min(ram.free_mb))
}

/// The host-RAM ceiling, in MB, that every emitted plan is charged against.
///
/// There is exactly one of these because there has to be: `fallback_cpu_plan`
/// lives outside this module and needs the same answer, and before it had one it
/// consulted no memory figure at all.
///
/// ```text
/// capacity_term = floor(0.75 * usable_ram())
/// pressure_term = floor(0.80 * effective_free())
/// reserve_term  = effective_free() - OS_RESERVE_MB
/// gate_mb       = max(0, min(capacity, pressure, reserve))
/// ```
///
/// Capacity bounds the ceiling, pressure bounds the instant, and both are kept.
/// WARP excludes the pressure terms on the grounds that "a budget resolved once
/// at open cannot track them", which is right for a run lasting hours;
/// txtCaster recomputes a plan per stage at stage entry, so at our cadence
/// pressure is a valid input and capacity alone would over-plan against a
/// machine that is already busy.
///
/// The reserve is a third term of the same `min` rather than a subtraction
/// applied afterwards. Subtracting it from an already-fractional budget would
/// charge it twice on a large machine while still not biting on a small one,
/// which is the opposite of what it is for.
///
/// `floor` at every step, and `u64` saturation supplies the `max(0, …)`: the
/// gate must never round itself upward into memory the machine does not have,
/// and a machine with less free memory than the reserve yields a zero budget,
/// which is rung 3 and never a refusal.
pub fn ram_gate_mb(ram: &RamInfo) -> u64 {
    let effective_free = effective_free_mb(ram);
    let capacity = (usable_ram_mb(ram) as f64 * CAPACITY_FRACTION).floor() as u64;
    let pressure = (effective_free as f64 * RAM_HEADROOM).floor() as u64;
    let reserve = effective_free.saturating_sub(OS_RESERVE_MB);
    capacity.min(pressure).min(reserve)
}

/// The single fit predicate: a whole [`RamCharge`] total against the ceiling
/// from [`ram_gate_mb`].
///
/// Takes the gate rather than a raw free-memory figure so that the headroom
/// fraction is applied in one place and cannot be applied twice, or forgotten,
/// at a call site.
pub fn fits_ram(cpu_bytes: u64, gate_mb: u64) -> bool {
    cpu_bytes <= gate_mb.saturating_mul(1024 * 1024)
}

fn tok_s_est(perf: Option<&Perf>, frac_gpu: f64) -> Option<f64> {
    let perf = perf?;
    let gpu = perf.tok_s_gpu;
    let cpu = perf.tok_s_cpu;
    match (gpu, cpu) {
        (Some(g), Some(c)) if frac_gpu > 0.0 && frac_gpu < 1.0 => {
            Some(1.0 / (frac_gpu / g + (1.0 - frac_gpu) / c))
        }
        (Some(g), _) if frac_gpu >= 1.0 => Some(g),
        (_, Some(c)) if frac_gpu <= 0.0 => Some(c),
        (Some(g), None) => Some(g * frac_gpu),
        (None, Some(c)) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::gguf::GgufModelInfo;
    use crate::catalog::{
        CatalogEntry, Download, DownloadFile, License, ModelFormat, ModelSource, ModelTask, Perf,
    };
    use crate::hardware::{CpuInfo, DeviceInfo, DiskInfo, HardwareProfile, RamInfo};

    const MB: u64 = 1024 * 1024;

    fn entry(id: &str, format: ModelFormat) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            task: ModelTask::Llm,
            display_name: id.to_string(),
            family: "test".to_string(),
            quant: Some("Q4_K_M".to_string()),
            format,
            disk_bytes: 1000,
            full_gpu_vram_mb: Some(3584),
            min_free_ram_mb: 4096,
            hybrid_capable: true,
            ctx_max: Some(131_072),
            quality_score: 80,
            speed_score: 70,
            capabilities: vec![],
            languages: vec![],
            license: License {
                spdx: "Apache-2.0".to_string(),
                commercial_ok: true,
            },
            download: Download {
                files: vec![DownloadFile {
                    url: "https://example.com/m.gguf".to_string(),
                    sha256: "a".repeat(64),
                    dest_path: "m.gguf".to_string(),
                    size_bytes: 1000,
                }],
            },
            source: ModelSource::Curated,
            evidence: vec![],
            deprecated: false,
            replacement: None,
            perf: Some(Perf {
                tok_s_gpu: Some(60.0),
                tok_s_cpu: Some(10.0),
                rtf_gpu: None,
                rtf_cpu: None,
            }),
        }
    }

    pub(super) fn profile(vram_free_mb: Option<u64>, ram_free_mb: u64) -> HardwareProfile {
        HardwareProfile {
            devices: vram_free_mb
                .map(|v| crate::hardware::test_device(Some(v)))
                .into_iter()
                .collect(),
            ram: RamInfo {
                total_mb: ram_free_mb * 2,
                free_mb: ram_free_mb,
                cgroup: Default::default(),
            },
            cpu: CpuInfo {
                cores_physical: 8,
                cores_logical: 16,
                avx2: true,
                avx512: false,
            },
            disk: DiskInfo {
                models_free_gb: 100.0,
                cache_free_gb: 100.0,
            },
        }
    }

    pub(super) fn gguf(
        n_layers: u32,
        layer_mb: u64,
        non_layer_mb: u64,
        kv_per_tok: u64,
        output_mb: u64,
    ) -> GgufModelInfo {
        GgufModelInfo {
            n_layers,
            layer_bytes: vec![layer_mb * MB; n_layers as usize],
            non_layer_bytes: non_layer_mb * MB,
            output_bytes: output_mb * MB,
            kv_bytes_per_token: kv_per_tok,
            n_vocab: 0,
        }
    }

    #[test]
    fn reserve_memory_shrinks_vram_and_ram_and_saturates() {
        let mut p = profile(Some(3000), 8_192);
        crate::hardware::reserve_memory(&mut p, 945);
        assert_eq!(p.primary().unwrap().memory_free_mb, Some(2055));
        assert_eq!(p.ram.free_mb, 7_247);
        crate::hardware::reserve_memory(&mut p, 100_000);
        assert_eq!(p.primary().unwrap().memory_free_mb, Some(0));
        assert_eq!(p.ram.free_mb, 0);
        p.devices.clear();
        crate::hardware::reserve_memory(&mut p, 500);
        assert!(p.devices.is_empty());
    }

    #[test]
    fn a_cpu_plan_carries_the_charge_the_gate_evaluated() {
        let info = gguf(10, 100, 50, 0, 100);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(None, 8_192),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(!plan.device.is_gpu());
        assert_eq!(
            cpu_ram_charge(&info, plan.ctx, plan.batch).total(),
            1_498 * MB,
            "weights 1050 (10x100 layers + 50 non-layer) + compute 448; the \
             fixture must stay hand-derivable"
        );
        assert_eq!(plan.host_ram_planned_mb, Some(1_498));
    }

    #[test]
    fn a_fully_offloaded_plan_budgets_the_host_only_for_what_stays_on_it() {
        let info = gguf(10, 100, 50, 0, 100);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(2_240), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert_eq!(plan.n_gpu_layers, 11, "full offload, so the premise holds");
        assert_eq!(
            plan.host_ram_planned_mb,
            Some(50),
            "only the 50 MB of non-layer weights stay on the host"
        );
    }

    #[test]
    fn a_vision_cpu_plan_budgets_the_projector_on_the_host() {
        let info = gguf(10, 100, 50, 0, 100);
        let mmproj = 990_372_672u64;
        let plan = vision_cpu_fit(
            &entry("mllm", ModelFormat::Gguf),
            &profile(None, 8_192),
            Some(&info),
            mmproj,
        )
        .plan
        .expect("plan");
        assert!(!plan.device.is_gpu());
        assert_eq!(projector_reserve_mb(mmproj), 945 + CLIP_ENCODE_RESERVE_MB);
        assert_eq!(
            plan.host_ram_planned_mb,
            Some(1_498 + 945 + CLIP_ENCODE_RESERVE_MB)
        );
    }

    #[test]
    fn a_vision_gpu_plan_keeps_the_projector_out_of_the_host_budget() {
        let info = gguf(10, 100, 50, 0, 100);
        let plan = vision_fit(
            &entry("mllm", ModelFormat::Gguf),
            &profile(Some(16_000), 8_192),
            Some(&info),
            990_372_672,
        )
        .plan
        .expect("plan");
        assert!(plan.device.is_gpu());
        assert_eq!(plan.host_ram_planned_mb, Some(50));
    }

    #[test]
    fn mmproj_reservation_reduces_gpu_slots() {
        let info = gguf(10, 100, 50, 0, 100);
        let entry = entry("mllm", ModelFormat::Gguf);
        let full = compute_fit(&entry, &profile(Some(2_240), 16_384), Some(&info))
            .plan
            .expect("unreserved plan");
        let mut reserved_profile = profile(Some(2_240), 16_384);
        crate::hardware::reserve_memory(&mut reserved_profile, 945);
        let reserved = compute_fit(&entry, &reserved_profile, Some(&info))
            .plan
            .expect("reserved plan");
        assert_eq!(full.n_gpu_layers, 11);
        assert!(reserved.n_gpu_layers < full.n_gpu_layers);
        assert!(reserved.n_gpu_layers > 0);
    }

    #[test]
    fn mmproj_reservation_gates_cpu_ram_fit() {
        let info = gguf(10, 100, 50, 0, 100);
        let entry = entry("mllm", ModelFormat::Gguf);
        // Free RAM raised 1500 -> 2048 -> 2560 across R-AM's two gate changes.
        // The fixture's baseline is "a model that fits before the projector is
        // reserved" and the charge it must clear is 1050 MiB of weights plus a
        // 448 MiB compute buffer = 1498 MiB. 1500 MB free stopped clearing it
        // when the compute buffer entered the charge (1200 MiB gate); 2048 MB
        // stopped clearing it when the absolute OS reserve entered the gate
        // (1024 MiB gate, since 2048 - OS_RESERVE_MB binds below 0.8 x free).
        // 2560 MB gives a 1536 MiB gate. The intent is unchanged; only the
        // headroom needed to express it moved. Both flips are pinned:
        // `the_compute_buffer_flips_a_previously_fitting_cpu_model_to_red` and
        // `the_os_reserve_binds_below_five_gigabytes_free`.
        let unreserved = compute_fit(&entry, &profile(None, 2_560), Some(&info));
        assert!(!unreserved.plan.expect("cpu plan").device.is_gpu());
        let mut reserved_profile = profile(None, 2_560);
        crate::hardware::reserve_memory(&mut reserved_profile, 945);
        let reserved = compute_fit(&entry, &reserved_profile, Some(&info));
        assert_eq!(reserved.badge, FitBadge::Red);
        assert!(reserved.plan.is_none());
    }

    /// Box 5 — the one badge flip this unit causes, asserted rather than absorbed.
    ///
    /// The property is "weights alone clear the gate, the whole charge does not",
    /// which is what the old arithmetic got wrong. The free figure that expresses
    /// it moved from 1500 to 2400 MB when the absolute OS reserve entered the
    /// gate: at 1500 MB free the gate is now 476 MiB, below even the weights, so
    /// that input can no longer show the *old* acceptance — it is Red twice over.
    /// 2400 MB free gives a 1376 MiB gate, between the 1050 MiB of weights and
    /// the 1498 MiB whole charge, so the flip is still the compute buffer's.
    /// llama.cpp wants that ~448 MiB buffer and the old arithmetic never charged
    /// it; accepting the load anyway is the freeze.
    #[test]
    fn the_compute_buffer_flips_a_previously_fitting_cpu_model_to_red() {
        let info = gguf(10, 100, 50, 0, 100);
        let entry = entry("mllm", ModelFormat::Gguf);

        let weights = info.total_bytes();
        let compute = compute_buffer_bytes(&info, DEFAULT_BATCH);
        assert_eq!(weights, 1_050 * MB);
        assert_eq!(
            compute,
            COMPUTE_SLACK_MB * MB,
            "n_vocab is 0 in this fixture"
        );
        assert!(
            fits_ram(weights, gate(2_400)),
            "the old weights-only arithmetic accepted this"
        );
        assert!(
            !fits_ram(weights + compute, gate(2_400)),
            "the whole charge does not fit, which is why the badge moves"
        );

        let fit = compute_fit(&entry, &profile(None, 2_400), Some(&info));
        assert_eq!(fit.badge, FitBadge::Red);
        assert!(fit.plan.is_none());
    }

    #[test]
    fn vision_fit_reserves_the_projector_from_vram_only() {
        let info = gguf(10, 100, 50, 0, 100);
        let entry = entry("mllm", ModelFormat::Gguf);
        let p = profile(Some(4_096), 16_384);
        let mmproj = 990_372_672u64;

        let baseline_ram = p.ram.free_mb;
        let fit = vision_fit(&entry, &p, Some(&info), mmproj);
        assert!(fit.plan.is_some());
        assert_eq!(
            p.ram.free_mb, baseline_ram,
            "the caller's profile must not be mutated"
        );

        let mut vram_only = p.clone();
        crate::hardware::reserve_vram(&mut vram_only, projector_reserve_mb(mmproj));
        assert_eq!(
            vram_only.ram.free_mb, baseline_ram,
            "a gpu alternative hosts the projector in vram, so ram must be untouched"
        );
    }

    #[test]
    fn a_vision_fit_that_lands_on_the_host_is_gated_and_budgeted_like_the_cpu_path() {
        // F1's mechanism pin: when the GPU is too small for the projector
        // reserve (zero slots packed), vision_fit's outcome must be the CPU
        // path's outcome — same gate (RAM minus projector), same recorded
        // budget. Two things make this pin actually bite, both measured on
        // the sabotaged (re-gate disabled) build: the fixture is KV-BEARING
        // (kv-free charges are ctx-independent, so both gates pick identical
        // plans and the pin holds vacuously), and the machine is CHOSEN so
        // the projector reserve straddles a ctx ladder rung — the full gate
        // admits ctx 8192 while the projector-honest gate only admits 4096.
        // On machines where both gates admit the same rung, the discrete
        // ladder swallows the difference and equality cannot distinguish the
        // two gatings. The selection loop derives that machine from the
        // planner's own gate, so a gate-term change retunes rather than
        // silently vacates the pin.
        let info = gguf(10, 100, 50, 524_288, 100); // kv: 0.5 MB/token
        let entry = entry("mllm", ModelFormat::Gguf);
        let mmproj = 990_372_672u64;
        let reserve = projector_reserve_mb(mmproj);
        let charge_at = |ctx: u64| cpu_ram_charge(&info, ctx, DEFAULT_BATCH).total();
        let ram_free = [
            6_144u64, 7_168, 8_192, 9_216, 10_240, 12_288, 14_336, 16_384,
        ]
        .into_iter()
        .find(|&ram| {
            let full = gate(ram);
            let honest = full.saturating_sub(reserve);
            fits_ram(charge_at(8_192), full)      // full gate admits the top rung
                    && !fits_ram(charge_at(8_192), honest) // honest gate refuses it
                    && fits_ram(charge_at(4_096), honest) // but admits the next rung
        })
        .expect("a machine where the projector reserve straddles a ladder rung must exist");
        let p = profile(Some(1_000), ram_free); // GPU present, smaller than the 1585 MB reserve

        let via_vision = vision_fit(&entry, &p, Some(&info), mmproj);
        let via_cpu = vision_cpu_fit(&entry, &p, Some(&info), mmproj);
        let vision_plan = via_vision.plan.expect("host placement fits this machine");
        let cpu_plan = via_cpu.plan.expect("cpu path fits this machine");
        assert!(
            !vision_plan.device.is_gpu(),
            "premise: the packer landed on the host"
        );
        assert_eq!(via_vision.badge, via_cpu.badge);
        assert_eq!(vision_plan.n_gpu_layers, cpu_plan.n_gpu_layers);
        assert_eq!(
            vision_plan.ctx, cpu_plan.ctx,
            "a larger ctx here means the plan was gated against RAM the projector will consume"
        );
        assert_eq!(vision_plan.batch, cpu_plan.batch);
        assert_eq!(
            vision_plan.host_ram_planned_mb, cpu_plan.host_ram_planned_mb,
            "the recorded budget must be the CPU path's: gate-evaluated charge plus the projector"
        );
        let charge = cpu_ram_charge(&info, vision_plan.ctx, vision_plan.batch).total();
        assert_eq!(
            vision_plan.host_ram_planned_mb,
            Some(planned_mb(charge) + projector_reserve_mb(mmproj)),
            "budget = what the gate charged + the reserve the gate's profile subtracted"
        );
    }

    #[test]
    fn a_vision_fit_cpu_outcome_cannot_outspend_the_gate_the_projector_shrank() {
        // F1's escape, closed: on origin/main this machine got an Orange CPU
        // plan whose true host consumption exceeded the RAM gate by the whole
        // projector reserve — badge Orange, no red dialog, and the recorded
        // consumption-sized budget guaranteed the residency warn stayed
        // silent on exactly this path. Honest gating must refuse it.
        let info = gguf(10, 100, 50, 0, 100);
        let entry = entry("mllm", ModelFormat::Gguf);
        let mmproj = 990_372_672u64; // reserve = 945 + 640 = 1585 MB
        let ram_free = 4_096u64;

        let g = gate(ram_free);
        assert!(
            fits_ram(1_498 * MB, g),
            "precondition: the naked CPU charge clears the full gate ({g} MB) — the pre-fix acceptance"
        );
        assert!(
            !fits_ram(1_498 * MB, g.saturating_sub(projector_reserve_mb(mmproj))),
            "precondition: it cannot clear the gate once the projector is charged"
        );

        let fit = vision_fit(&entry, &profile(Some(1_000), ram_free), Some(&info), mmproj);
        assert_eq!(
            fit.badge,
            FitBadge::Red,
            "the projector-honest gate refuses"
        );
        assert!(fit.plan.is_none());
    }

    #[test]
    fn a_sub_mebibyte_charge_rounds_its_recorded_budget_up_at_the_planner_site() {
        // F2: every other plan.rs fixture charge is MB-aligned, so flooring
        // planned_mb survived the whole suite. One deliberately unaligned
        // charge (kv_bytes_per_token indivisible by MB) pins the rounding
        // direction at the site that carries most plans.
        let info = gguf(10, 100, 50, 7, 100); // kv = ctx * 7 bytes: never MB-aligned
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(None, 8_192),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(!plan.device.is_gpu());
        let charge = cpu_ram_charge(&info, plan.ctx, plan.batch).total();
        assert_ne!(
            charge % MB,
            0,
            "precondition: the fixture is not MB-aligned"
        );
        assert_eq!(
            plan.host_ram_planned_mb,
            Some(charge / MB + 1),
            "a floored budget would sit below the evaluated charge and false-warn an exactly-planned load"
        );
    }

    #[test]
    fn vision_cpu_fit_reserves_the_projector_from_ram_only() {
        let info = gguf(10, 100, 50, 0, 100);
        let entry = entry("mllm", ModelFormat::Gguf);
        let p = profile(Some(4_096), 16_384);
        let mmproj = 990_372_672u64;

        let plan = vision_cpu_fit(&entry, &p, Some(&info), mmproj)
            .plan
            .expect("cpu alternative");
        assert!(!plan.device.is_gpu());
        assert_eq!(plan.n_gpu_layers, 0);

        let mut ram_only = p.clone();
        crate::hardware::reserve_ram(&mut ram_only, projector_reserve_mb(mmproj));
        assert_eq!(
            ram_only.primary().unwrap().memory_free_mb,
            Some(4_096),
            "a cpu alternative hosts the projector in ram, so vram must be untouched"
        );
    }

    #[test]
    fn single_pool_reservation_clears_a_false_red() {
        let mut info = gguf(42, 59, 2_260, 107_520, 360);
        info.n_vocab = 262_144;
        let mut entry = entry("gemma4-e4b-q4km", ModelFormat::Gguf);
        entry.ctx_max = Some(32_768);
        let mmproj = 990_372_672u64;
        let p = profile(Some(3_641), 7_000);

        let mut both_pools = p.clone();
        crate::hardware::reserve_memory(&mut both_pools, projector_reserve_mb(mmproj));
        let double_counted = compute_fit(&vision_entry(&entry), &both_pools, Some(&info));
        assert_eq!(
            double_counted.badge,
            FitBadge::Red,
            "charging the projector to vram and ram at once rejects a placement that fits"
        );
        assert!(double_counted.plan.is_none());

        let fit = vision_fit(&entry, &p, Some(&info), mmproj);
        let plan = fit
            .plan
            .expect("single-pool reservation must find a placement");
        assert_eq!(fit.badge, FitBadge::Orange);
        assert!(plan.n_gpu_layers > 0);
        assert!(
            plan.device.is_gpu(),
            "device drives projector placement in the engine, so a gpu plan is what moves the clip encode off the cpu"
        );
    }

    #[test]
    fn single_pool_reservation_flips_the_boundary_case_to_yellow() {
        let mut info = gguf(42, 59, 2_260, 107_520, 360);
        info.n_vocab = 262_144;
        let mut entry = entry("gemma4-e4b-q4km", ModelFormat::Gguf);
        entry.ctx_max = Some(32_768);
        let mmproj = 990_372_672u64;
        let p = profile(Some(4_184), 7_000);

        let mut both_pools = p.clone();
        crate::hardware::reserve_memory(&mut both_pools, projector_reserve_mb(mmproj));
        assert_eq!(
            compute_fit(&vision_entry(&entry), &both_pools, Some(&info)).badge,
            FitBadge::Red
        );

        let fit = vision_fit(&entry, &p, Some(&info), mmproj);
        assert_eq!(
            fit.badge,
            FitBadge::Yellow,
            "HARDWARE_PROFILER §4 acceptance"
        );
        assert!(fit.plan.expect("plan").n_gpu_layers >= 14);
    }

    #[test]
    fn vision_fit_caps_ctx_and_reserves_projector() {
        let info = gguf(10, 100, 50, 0, 100);
        let entry = entry("mllm", ModelFormat::Gguf);
        let p = profile(Some(2_048), 16_384);
        let text = compute_fit(&entry, &p, Some(&info))
            .plan
            .expect("text plan");
        let vision = vision_fit(&entry, &p, Some(&info), 990_372_672)
            .plan
            .expect("vision plan");
        assert!(vision.ctx <= VISION_CTX_CAP);
        assert!(vision.n_gpu_layers < text.n_gpu_layers);
    }

    #[test]
    fn vision_fit_clamps_to_cpu_when_total_vram_exceeds_measured_budget() {
        let mut info = gguf(42, 59, 2_260, 107_520, 360);
        info.n_vocab = 262_144;
        let mut entry = entry("gemma4-e4b-q4km", ModelFormat::Gguf);
        entry.ctx_max = Some(32_768);
        let mmproj = 990_372_672u64;

        let mut dual_gpu_profile = profile(None, 16_384);
        dual_gpu_profile.devices = vec![
            DeviceInfo {
                llm_runtime: None,
                key: "pci:00:02.0".into(),
                backend: DeviceBackend::Vulkan,
                ordinal: 0,
                vendor: "Intel".into(),
                name: "Intel(R) Iris(R) Xe Graphics".into(),
                memory_total_mb: Some(11_745),
                memory_free_mb: Some(4_473),
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
                memory_total_mb: Some(4_096),
                memory_free_mb: Some(2_261),
                unified: false,
                discrete: true,
                memory_source: MemorySource::VkBudget,
            },
        ];

        let fit = vision_fit(&entry, &dual_gpu_profile, Some(&info), mmproj);
        let plan = fit.plan.expect("clamped cpu plan fits host memory");
        assert!(
            !plan.device.is_gpu(),
            "when total vram exceeds the measured discrete/vulkan budget (2261 MB), plan must clamp to cpu"
        );
        assert_eq!(plan.n_gpu_layers, 0);
    }

    #[test]
    fn full_gpu_fit_is_green() {
        let info = gguf(32, 60, 200, 16_384, 100);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(8_192), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert_eq!(fit.badge, FitBadge::Green);
        assert_eq!(plan.n_gpu_layers, 33);
        assert_eq!(plan.n_layers_total, 32);
        assert!(plan.kv_offload);
        assert!(plan.device.is_gpu());
        assert_eq!(plan.tok_s_est, Some(60.0));
    }

    #[test]
    fn hybrid_fit_is_yellow_with_partial_layers() {
        let info = gguf(40, 160, 400, 65_536, 100);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(4_096), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert_eq!(fit.badge, FitBadge::Yellow);
        assert!(plan.n_gpu_layers > 0);
        assert!(plan.n_gpu_layers < 40);
        assert!(
            f64::from(plan.n_gpu_layers) >= HYBRID_MIN_FRACTION * 40.0,
            "got {} layers",
            plan.n_gpu_layers
        );
        let est = plan.tok_s_est.expect("tok_s_est");
        assert!(est > 10.0 && est < 60.0);
    }

    #[test]
    fn kv_offload_retry_recovers_layers() {
        let info = gguf(10, 100, 0, 262_144, 0);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(1_600), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(!plan.kv_offload);
        assert!(f64::from(plan.n_gpu_layers) >= HYBRID_MIN_FRACTION * 10.0);
    }

    #[test]
    fn no_gpu_is_orange_cpu_plan() {
        let info = gguf(32, 60, 200, 16_384, 100);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(None, 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert_eq!(fit.badge, FitBadge::Orange);
        assert!(!plan.device.is_gpu());
        assert_eq!(plan.n_gpu_layers, 0);
        assert!(!plan.kv_offload);
        assert_eq!(plan.tok_s_est, Some(10.0));
    }

    #[test]
    fn over_memory_is_red_without_plan() {
        let info = gguf(64, 500, 1_000, 65_536, 200);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(2_048), 4_096),
            Some(&info),
        );
        assert_eq!(fit.badge, FitBadge::Red);
        assert!(fit.plan.is_none());
    }

    #[test]
    fn ctx_steps_down_until_plan_fits() {
        let info = gguf(16, 100, 100, 2 * MB, 50);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(8_192), 32_768),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(
            plan.ctx < 32_768,
            "ctx {} should have stepped down",
            plan.ctx
        );
        assert!(plan.ctx >= 4_096);
    }

    #[test]
    fn ctx_capped_by_model_ctx_max() {
        let mut e = entry("m", ModelFormat::Gguf);
        e.ctx_max = Some(8_192);
        let info = gguf(8, 10, 10, 1_024, 10);
        let fit = compute_fit(&e, &profile(Some(8_192), 16_384), Some(&info));
        assert_eq!(fit.plan.expect("plan").ctx, 8_192);
    }

    #[test]
    fn small_vram_headroom_reserved() {
        let info = gguf(4, 100, 50, 0, 50);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(600), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert_eq!(plan.n_gpu_layers, 0);
        assert!(!plan.device.is_gpu());
    }

    #[test]
    fn heuristic_green_when_vram_covers_full_need() {
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(8_192), 16_384),
            None,
        );
        assert_eq!(fit.badge, FitBadge::Green);
        assert!(fit.plan.is_none());
    }

    #[test]
    fn heuristic_yellow_for_hybrid_partial_vram() {
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(2_048), 16_384),
            None,
        );
        assert_eq!(fit.badge, FitBadge::Yellow);
    }

    #[test]
    fn heuristic_orange_when_cpu_ram_sufficient() {
        let mut e = entry("m", ModelFormat::Gguf);
        e.hybrid_capable = false;
        let fit = compute_fit(&e, &profile(Some(512), 16_384), None);
        assert_eq!(fit.badge, FitBadge::Orange);
    }

    #[test]
    fn heuristic_red_when_memory_exceeded() {
        let mut e = entry("m", ModelFormat::Gguf);
        e.hybrid_capable = false;
        e.min_free_ram_mb = 65_536;
        let fit = compute_fit(&e, &profile(Some(512), 4_096), None);
        assert_eq!(fit.badge, FitBadge::Red);
    }

    #[test]
    fn heuristic_red_when_requirement_sits_between_the_pools() {
        for format in [ModelFormat::Pytorch, ModelFormat::Gguf] {
            let mut e = entry("m", format);
            e.hybrid_capable = false;
            e.full_gpu_vram_mb = Some(8_192);
            e.min_free_ram_mb = 8_192;
            let fit = compute_fit(&e, &profile(Some(6_144), 6_144), None);
            assert_eq!(
                fit.badge,
                FitBadge::Red,
                "{format:?}: 8192 MB fits neither pool alone (ram 6144, vram 6144); the pools are never summed"
            );
            assert!(fit.plan.is_none());
        }
    }

    #[test]
    fn heuristic_cpu_only_model_green_on_adequate_ram() {
        let mut e = entry("m", ModelFormat::Pytorch);
        e.full_gpu_vram_mb = None;
        e.min_free_ram_mb = 2_048;
        let fit = compute_fit(&e, &profile(None, 8_192), None);
        assert_eq!(fit.badge, FitBadge::Green);
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn table_driven_model_hw_matrix() {
        let cases: Vec<(&str, GgufModelInfo, Option<u64>, u64, u32, FitBadge)> = vec![
            (
                "full-fit",
                gguf(32, 60, 200, 16_384, 100),
                Some(8_192),
                16_384,
                33,
                FitBadge::Green,
            ),
            (
                "hybrid",
                gguf(40, 160, 400, 65_536, 100),
                Some(4_096),
                16_384,
                19,
                FitBadge::Yellow,
            ),
            (
                "cpu-only-hw",
                gguf(32, 60, 200, 16_384, 100),
                None,
                16_384,
                0,
                FitBadge::Orange,
            ),
            (
                "tiny-vram",
                gguf(4, 100, 50, 0, 50),
                Some(600),
                16_384,
                0,
                FitBadge::Orange,
            ),
        ];
        for (name, info, vram, ram, expected_layers, expected_badge) in cases {
            let fit = compute_fit(
                &entry(name, ModelFormat::Gguf),
                &profile(vram, ram),
                Some(&info),
            );
            assert_eq!(fit.badge, expected_badge, "{name}: badge");
            let plan = fit.plan.expect("plan");
            assert_eq!(plan.n_gpu_layers, expected_layers, "{name}: n_gpu_layers");
        }
        let red = compute_fit(
            &entry("red", ModelFormat::Gguf),
            &profile(Some(2_048), 4_096),
            Some(&gguf(64, 500, 1_000, 65_536, 200)),
        );
        assert_eq!(red.badge, FitBadge::Red);
        assert!(red.plan.is_none());
    }

    #[test]
    fn small_vram_prefers_partial_offload_over_cpu() {
        let info = gguf(42, 105, 600, 32_768, 360);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(3_058), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(plan.device.is_gpu(), "should offload, not fall back to cpu");
        assert!(
            plan.n_gpu_layers >= 10,
            "expected a meaningful split, got {}",
            plan.n_gpu_layers
        );
        assert!(plan.n_gpu_layers < 42);
    }

    #[test]
    fn partial_offload_below_hybrid_floor_is_orange_but_on_gpu() {
        let info = gguf(40, 300, 500, 16_384, 100);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(2_500), 32_768),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(plan.device.is_gpu());
        assert!(plan.n_gpu_layers > 0);
        assert!(f64::from(plan.n_gpu_layers) < HYBRID_MIN_FRACTION * 40.0);
        assert_eq!(fit.badge, FitBadge::Orange);
    }

    #[test]
    fn positive_offload_packs_output_slot_then_trailing_blocks() {
        let mut layer_bytes = vec![10 * MB; 10];
        layer_bytes.extend(vec![500 * MB; 10]);
        let info = GgufModelInfo {
            n_layers: 20,
            layer_bytes,
            non_layer_bytes: 100 * MB,
            output_bytes: 100 * MB,
            kv_bytes_per_token: 0,
            n_vocab: 0,
        };
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(2_192), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(plan.device.is_gpu());
        assert_eq!(
            plan.n_gpu_layers, 3,
            "budget fits output slot + 2 trailing 500MB blocks; prefix packing would claim far more"
        );
    }

    #[test]
    fn compute_buffer_is_dominated_by_the_logits_term() {
        let mut info = gguf(42, 60, 2_000, 1_024, 300);
        info.n_vocab = 262_144;
        let bytes = compute_buffer_bytes(&info, 512);
        assert_eq!(bytes / MB, 512 + COMPUTE_SLACK_MB);
        assert_eq!(
            compute_buffer_bytes(&info, 256) / MB,
            256 + COMPUTE_SLACK_MB
        );
    }

    #[test]
    fn compute_buffer_covers_every_measured_sample() {
        let mut info = gguf(42, 60, 2_000, 1_024, 300);
        info.n_vocab = 262_144;
        let measured: [(u64, u64); 6] = [
            (512, 573),
            (512, 525),
            (512, 893),
            (256, 510),
            (256, 561),
            (128, 351),
        ];
        for (batch, observed_mb) in measured {
            let reserved_mb = compute_buffer_bytes(&info, batch) / MB;
            assert!(
                reserved_mb >= observed_mb,
                "batch {batch}: reserved {reserved_mb} MiB < measured {observed_mb} MiB"
            );
        }
    }

    #[test]
    fn a_large_vocab_model_no_longer_plans_an_unloadable_full_offload() {
        let mut info = gguf(42, 59, 2_260, 107_520, 360);
        info.n_vocab = 262_144;
        let fit = compute_fit(
            &entry("gemma4-e4b-q4km", ModelFormat::Gguf),
            &profile(Some(3_766), 15_660),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(
            plan.n_gpu_layers < 43,
            "must not plan full offload it cannot load, got {}",
            plan.n_gpu_layers
        );
        assert!(plan.n_gpu_layers > 12, "must beat the old 12-block hybrid");
        assert_eq!(
            plan.ctx, 32_768,
            "ctx must be retained: kv lives in host RAM"
        );
        assert!(!plan.kv_offload);
    }

    #[test]
    fn full_offload_needs_layer_count_plus_output_slot() {
        let info = gguf(42, 60, 2_000, 1_024, 300);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(3_812), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert_eq!(plan.n_gpu_layers, 43);
        assert!(plan.kv_offload);
        assert_eq!(fit.badge, FitBadge::Green);
        assert_eq!(plan.tok_s_est, Some(60.0));
    }

    #[test]
    fn one_block_short_of_full_offload_stays_partial() {
        let info = gguf(42, 60, 2_000, 1_024, 300);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(3_752), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert_eq!(plan.n_gpu_layers, 42);
        assert!(plan.kv_offload);
        assert_eq!(fit.badge, FitBadge::Yellow);
    }

    #[test]
    fn zero_slot_cpu_plan_counts_kv_in_ram_check() {
        let info = gguf(10, 100, 500, 64 * 1024, 50);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(600), 4_096),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(!plan.device.is_gpu());
        assert_eq!(plan.n_gpu_layers, 0);
        assert!(!plan.kv_offload);
        assert_eq!(
            plan.ctx, 16_384,
            "32K ctx KV no longer fits free RAM once counted"
        );
    }

    #[test]
    fn output_slot_too_big_for_budget_yields_cpu_plan() {
        let info = gguf(42, 60, 2_000, 1_024, 300);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(512), 16_384),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert!(!plan.device.is_gpu());
        assert_eq!(plan.n_gpu_layers, 0);
        assert_eq!(fit.badge, FitBadge::Orange);
    }

    #[test]
    fn plan_serializes_to_engine_contract() {
        let info = gguf(32, 60, 200, 16_384, 100);
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(8_192), 16_384),
            Some(&info),
        );
        let value = serde_json::to_value(fit.plan.expect("plan")).unwrap();
        for key in [
            "model_id",
            "device",
            "n_gpu_layers",
            "n_layers_total",
            "kv_offload",
            "ctx",
            "batch",
            "badge",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
    }

    // ---------------------------------------------------------------------
    // R-AM — every host-RAM decision site charges the whole plan.
    //
    // Arithmetic these fixtures are built on, so the numbers below can be read
    // without running them:
    //   gate(ram_free_mb) = ram_free_mb * RAM_HEADROOM = ram_free_mb * 0.8
    //   compute           = 448 MiB flat, because `gguf()` sets n_vocab = 0
    //                       so the logits term vanishes and only COMPUTE_SLACK_MB
    //                       remains
    //   weights           = non_layer_bytes + sum(layer_bytes)   (output_bytes is
    //                       a packing term, not part of total_bytes())
    // Every fixture pins `ctx_max` to 4096 so `ladder()` yields exactly one rung
    // and the assertion is about the charge, not about where the ladder stopped.
    // ---------------------------------------------------------------------

    fn ctx_capped(id: &str, ctx_max: u64) -> CatalogEntry {
        let mut e = entry(id, ModelFormat::Gguf);
        e.ctx_max = Some(ctx_max);
        e
    }

    /// A profile whose host RAM is `total`/`free` and whose cgroup says
    /// `limit`/`headroom` (both in MB, `None` for "no limit").
    pub(super) fn cgroup_profile(
        total_mb: u64,
        free_mb: u64,
        limit_mb: Option<u64>,
        headroom_mb: Option<u64>,
    ) -> HardwareProfile {
        let mut profile = profile(None, free_mb);
        profile.ram = RamInfo {
            total_mb,
            free_mb,
            cgroup: crate::hardware::cgroup::CgroupMemory {
                limit_mb,
                headroom_mb,
            },
        };
        profile
    }

    // -----------------------------------------------------------------
    // R-AM — the three-term gate, and the cgroup behind it.
    // -----------------------------------------------------------------

    /// The headline case, in the shape WARP records: a big host, a small
    /// cgroup. Before this the planner read `MemTotal`/`MemAvailable` and
    /// planned against the host, and the kernel — which charges the cgroup —
    /// kills the process rather than degrading, so no ladder rung softens it.
    #[test]
    fn a_cgroup_limited_host_plans_to_the_limit_not_to_the_hosts_ram() {
        // 3050 MiB of weights, no KV, 448 MiB compute buffer = 3498 MiB.
        let info = gguf(10, 300, 50, 0, 100);
        let entry = ctx_capped("m", 4_096);
        assert_eq!(
            cpu_ram_charge(&info, 4_096, DEFAULT_BATCH).total(),
            3_498 * MB
        );

        // The host alone: 64 GB total, 32 GB free. Nothing is tight.
        let host = cgroup_profile(65_536, 32_768, None, None);
        assert_eq!(ram_gate_mb(&host.ram), 26_214);
        assert!(compute_fit(&entry, &host, Some(&info)).plan.is_some());

        // The same host inside a 4 GB cgroup that has already used 512 MB.
        let limited = cgroup_profile(65_536, 32_768, Some(4_096), Some(3_584));
        assert_eq!(
            ram_gate_mb(&limited.ram),
            2_560,
            "min(0.75*4096=3072, 0.80*3584=2867, 3584-1024=2560)"
        );
        let fit = compute_fit(&entry, &limited, Some(&info));
        assert_eq!(fit.badge, FitBadge::Red);
        assert!(
            fit.plan.is_none(),
            "3498 MiB does not fit a 2560 MiB gate, whatever the host says"
        );
    }

    /// `usable_ram()` is a `min`, so a limit above the machine's own RAM cannot
    /// inflate the budget. A container given more than the box has is not a box
    /// with more memory.
    #[test]
    fn a_limit_larger_than_the_machine_cannot_inflate_the_budget() {
        let roomy = cgroup_profile(16_384, 16_384, Some(1_048_576), Some(1_048_576));
        let unlimited = cgroup_profile(16_384, 16_384, None, None);
        assert_eq!(ram_gate_mb(&roomy.ram), ram_gate_mb(&unlimited.ram));
        assert_eq!(ram_gate_mb(&roomy.ram), 12_288, "0.75 * 16384, capacity");
    }

    /// Which term binds, across the range. This is the test that would catch a
    /// refactor collapsing the three terms into two, or applying the reserve as
    /// a subtraction after the fraction instead of as a third term of the same
    /// `min` — which would charge it twice on a large machine while still not
    /// biting on a small one.
    /// One row of the gate table: the machine, what the cgroup says about it,
    /// the gate that should come out, and which of the three terms produced it.
    type GateCase = (u64, u64, Option<u64>, Option<u64>, u64, &'static str);

    #[test]
    fn each_of_the_three_terms_binds_somewhere() {
        // (total_mb, free_mb, cgroup limit, cgroup headroom, gate, binding term)
        let cases: &[GateCase] = &[
            // Mostly-idle machine: 0.75 of capacity is under 0.80 of free.
            (8_192, 8_000, None, None, 6_144, "capacity"),
            // Busy machine with room to spare: pressure binds.
            (65_536, 32_768, None, None, 26_214, "pressure"),
            // Small machine: the absolute reserve binds.
            (8_192, 4_096, None, None, 3_072, "reserve"),
            // Nothing left after the reserve: clamped at zero, never negative.
            (8_192, 1_024, None, None, 0, "reserve, clamped"),
            (8_192, 512, None, None, 0, "reserve, clamped"),
            // The cgroup feeds both halves: limit -> capacity, headroom -> the
            // other two.
            (
                65_536,
                65_536,
                Some(8_192),
                Some(8_192),
                6_144,
                "capacity, cgroup",
            ),
            (
                65_536,
                65_536,
                Some(65_536),
                Some(4_096),
                3_072,
                "reserve, cgroup",
            ),
        ];
        for &(total, free, limit, headroom, want, why) in cases {
            let profile = cgroup_profile(total, free, limit, headroom);
            assert_eq!(
                ram_gate_mb(&profile.ram),
                want,
                "total {total} free {free} limit {limit:?} headroom {headroom:?} ({why})"
            );
        }
    }

    /// The crossover the absolute reserve exists for, asserted rather than
    /// asserted-about: it is the binding term exactly below
    /// `OS_RESERVE_MB / (1 - RAM_HEADROOM)`. Above that a fraction is enough;
    /// below it, 20 % of a small pool is not an operating system.
    #[test]
    fn the_os_reserve_binds_below_five_gigabytes_free() {
        let crossover = (OS_RESERVE_MB as f64 / (1.0 - RAM_HEADROOM)).round() as u64;
        assert_eq!(crossover, 5_120);

        // Just below: reserve wins. Just above: pressure wins. Capacity is kept
        // out of it by giving the machine plenty of nominal RAM.
        let tight = cgroup_profile(65_536, 5_119, None, None);
        assert_eq!(ram_gate_mb(&tight.ram), 5_119 - OS_RESERVE_MB);
        let roomy = cgroup_profile(65_536, 5_121, None, None);
        assert_eq!(ram_gate_mb(&roomy.ram), (5_121.0 * RAM_HEADROOM) as u64);
        assert!(ram_gate_mb(&roomy.ram) < 5_121 - OS_RESERVE_MB);
    }

    /// A zero gate is rung 3, not a refusal. ARCHITECTURE §6.1 invariant 1
    /// forbids a memory figure making a model unavailable, so the reserve must
    /// be unable to turn into one however small the machine is.
    #[test]
    fn a_machine_below_the_reserve_still_gets_a_plan_from_the_fallback() {
        let starved = cgroup_profile(8_192, 512, None, None);
        assert_eq!(ram_gate_mb(&starved.ram), 0);

        let info = gguf(10, 100, 50, 0, 100);
        let entry = ctx_capped("m", 4_096);
        let fit = compute_fit(&entry, &starved, Some(&info));
        assert_eq!(fit.badge, FitBadge::Red);
        assert!(fit.plan.is_none(), "the planner has nothing that fits");

        // But the emitted JSON is still a runnable plan, because the fallback
        // emits at rung 3 rather than refusing.
        let json = crate::orchestrator::llm::fallback_cpu_plan(
            "m",
            Some(&info),
            ram_gate_mb(&starved.ram),
        );
        assert_eq!(json["badge"], "red");
        assert_eq!(json["device"]["backend"], "cpu");
        assert!(json["ctx"].as_u64().expect("ctx") >= 512);
    }

    /// Device *selection* reads the cgroup too, not only device budgeting.
    ///
    /// `capable_device` was the fifth `budget_mb` path and the one left on the
    /// raw host figure when the other four were converted. It is not in the
    /// freeze class — `gguf_fit` recomputes the chosen device's budget from
    /// `effective_free_mb`, so an over-ranked device still gets a correctly
    /// bounded plan — but it is the same defect shape: two answers to "how much
    /// memory does this device have".
    ///
    /// The consequence is real and this fixture is the one that shows it. A
    /// unified device's memory *is* host memory, so a cgroup bounds it; a
    /// discrete card's VRAM is not, so the cgroup does not touch it. Inside a
    /// 4 GB cgroup a 16 GB unified device therefore offers less usable memory
    /// than a 4 GB discrete card, and the discrete card is the correct pick.
    /// Ranking on `ram.free_mb` picks the unified one.
    #[test]
    fn an_integrated_gpu_carries_a_gguf_model_when_it_is_the_only_gpu() {
        let mut entry = ctx_capped("m", 4_096);
        entry.full_gpu_vram_mb = Some(3_000);
        let mut integrated = crate::hardware::test_device(Some(8_192));
        integrated.key = "integrated:0".into();
        integrated.unified = true;
        integrated.discrete = false;
        let mut host = cgroup_profile(65_536, 32_768, None, None);
        host.devices = vec![integrated];
        assert_eq!(
            capable_device(&entry, &host).expect("a device").key,
            "integrated:0",
            "with no discrete card llama.cpp uses the integrated GPU, and so does the plan"
        );
    }

    #[test]
    fn a_gpu_the_engine_cannot_reach_carries_no_gguf_model() {
        let mut entry = ctx_capped("m", 4_096);
        entry.full_gpu_vram_mb = Some(3_000);
        let mut card = crate::hardware::test_device(Some(8_192));
        card.llm_runtime = Some(false);
        let mut host = cgroup_profile(65_536, 32_768, None, None);
        host.devices = vec![card];
        assert!(
            capable_device(&entry, &host).is_none(),
            "a card llama.cpp did not report would run the model on the CPU anyway"
        );
        entry.format = ModelFormat::Pytorch;
        assert!(
            capable_device(&entry, &host).is_some(),
            "torch reaches the card through CUDA whatever llama.cpp reported"
        );
        host.devices[0].llm_runtime = None;
        entry.format = ModelFormat::Gguf;
        assert!(
            capable_device(&entry, &host).is_some(),
            "before any engine has reported, the plan keeps the card"
        );
    }

    #[test]
    fn selection_prefers_a_discrete_card_the_cgroup_cannot_shrink() {
        let mut entry = ctx_capped("m", 4_096);
        entry.full_gpu_vram_mb = Some(8_000);
        assert_eq!(
            required_mb(8_000),
            10_000,
            "premise: neither device is a full fit"
        );

        let unified = {
            let mut d = crate::hardware::test_device(Some(16_384));
            d.key = "unified:0".into();
            d.unified = true;
            d.discrete = false;
            d
        };
        let discrete = {
            let mut d = crate::hardware::test_device(Some(4_096));
            d.key = "discrete:0".into();
            d.unified = false;
            d.discrete = true;
            d
        };

        // Unlimited: the unified device has more memory, but llama.cpp runs a GGUF model on
        // the discrete card whenever one exists, so that card is what the plan must budget.
        let mut host = cgroup_profile(65_536, 32_768, None, None);
        host.devices = vec![unified.clone(), discrete.clone()];
        assert_eq!(
            capable_device(&entry, &host).expect("a device").key,
            "discrete:0",
            "an integrated GPU beside a discrete card is never where llama.cpp offloads"
        );

        // Inside a 4 GB cgroup with 3584 MB of headroom, the unified device can
        // reach only 3584 MB of its 16 GB, which is less than the discrete
        // card's own 4096 MB that the cgroup does not bound.
        let mut limited = cgroup_profile(65_536, 32_768, Some(4_096), Some(3_584));
        limited.devices = vec![unified, discrete];
        assert_eq!(effective_free_mb(&limited.ram), 3_584);
        assert_eq!(
            capable_device(&entry, &limited).expect("a device").key,
            "discrete:0",
            "the cgroup shrinks the unified device's pool but not the card's"
        );
    }

    /// `fingerprint()` keys the `hw_benchmark` cache, and a benchmark measuring
    /// tok/s is a CPU property. Entering a cgroup must not invalidate it, so the
    /// limit lands beside the host figures rather than replacing them.
    #[test]
    fn a_cgroup_limit_does_not_change_the_hardware_fingerprint() {
        let host = cgroup_profile(65_536, 32_768, None, None);
        let limited = cgroup_profile(65_536, 32_768, Some(4_096), Some(3_584));
        assert_eq!(host.fingerprint(), limited.fingerprint());
        assert_ne!(
            ram_gate_mb(&host.ram),
            ram_gate_mb(&limited.ram),
            "premise: the gate must differ, or this asserts nothing"
        );
    }

    /// The gate for a machine with `ram_free_mb` free, derived from the same
    /// `profile()` the planner is handed. Deliberately not a literal: a fixture
    /// that hard-codes its own ceiling stops testing the planner's the moment
    /// `ram_gate_mb` gains a term.
    pub(super) fn gate(ram_free_mb: u64) -> u64 {
        ram_gate_mb(&profile(None, ram_free_mb).ram)
    }

    /// Site A, the Orange CPU fallback at the weights-only gate.
    ///
    /// The fixture is chosen so the ladder rung *refuses* and the fallback
    /// *accepts* on `origin/main`: weights alone clear the gate, weights + KV do
    /// not. That is the bypass, and it emits a plan needing 3996 MiB on a machine
    /// whose gate is 3276 MiB — 720 MiB of overcommit, which is the freeze.
    #[test]
    fn cpu_fallback_refuses_a_model_whose_kv_cache_does_not_fit() {
        // weights 2048 MiB, kv 1500 MiB @ ctx 4096, compute 448 MiB.
        let info = gguf(10, 100, 1_048, 384_000, 50);
        let entry = ctx_capped("m", 4_096);
        let ram_free = 4_096;

        // The premise, asserted rather than asserted-about: this is a model that
        // fits on weights and does not fit once the KV cache is counted.
        let gate = gate(ram_free) * MB;
        assert_eq!(info.total_bytes(), 2_048 * MB);
        assert!(
            info.total_bytes() <= gate,
            "premise: weights alone must clear the gate, else the fixture proves nothing"
        );
        assert!(
            info.total_bytes() + info.kv_bytes_per_token * 4_096 > gate,
            "premise: weights + KV must not clear the gate"
        );

        let fit = compute_fit(&entry, &profile(None, ram_free), Some(&info));
        assert_eq!(
            fit.badge,
            FitBadge::Red,
            "the weights-only gate used to emit an Orange full-CPU plan here"
        );
        assert!(
            fit.plan.is_none(),
            "no plan may be emitted when the whole charge is over the gate"
        );
    }

    /// Site B, the no-device early return. This is the site the audit called the
    /// *correct* one, and it is still incomplete: it counts weights + KV and
    /// omits the compute buffer. The fixture clears the gate on weights + KV and
    /// fails only once the compute buffer is added, so it passes on `origin/main`
    /// and refuses here — proof that the fix is larger than "call what the
    /// reference site already does".
    #[test]
    fn the_gate_charges_the_compute_buffer_the_reference_site_omitted() {
        // weights 2000 MiB, kv 1000 MiB @ ctx 4096, compute 448 MiB.
        let info = gguf(10, 100, 1_000, 256_000, 50);
        let entry = ctx_capped("m", 4_096);
        let ram_free = 4_096;

        let gate = gate(ram_free) * MB;
        let weights_plus_kv = info.total_bytes() + info.kv_bytes_per_token * 4_096;
        assert!(
            weights_plus_kv <= gate,
            "premise: the reference site's own arithmetic must accept this model"
        );
        assert!(
            weights_plus_kv + compute_buffer_bytes(&info, DEFAULT_BATCH) > gate,
            "premise: adding the compute buffer must be what tips it over"
        );

        let fit = compute_fit(&entry, &profile(None, ram_free), Some(&info));
        assert_eq!(fit.badge, FitBadge::Red);
        assert!(fit.plan.is_none());
    }

    /// The theorem the fallback's comment states: once counted correctly, site A
    /// can never accept a plan the ladder refused, because a full-CPU charge is
    /// never smaller than the hybrid charge at the same ctx.
    ///
    /// Expressed as the property that actually matters — *every emitted plan has
    /// passed the ceiling check* — over a grid rather than a single point, so a
    /// fixture tuned to one rung cannot make it pass vacuously.
    #[test]
    fn cpu_fallback_never_accepts_what_the_ladder_refused() {
        let mut emitted = 0;
        let mut refused = 0;
        for ram_free in [1_024u64, 2_048, 4_096, 8_192, 16_384] {
            for kv_per_tok in [0u64, 16_384, 128_000, 384_000] {
                for (layers, layer_mb, non_layer_mb) in
                    [(10u32, 100u64, 200u64), (32, 60, 500), (8, 400, 1_000)]
                {
                    for ctx_max in [4_096u64, 8_192, 32_768] {
                        let info = gguf(layers, layer_mb, non_layer_mb, kv_per_tok, 50);
                        let entry = ctx_capped("m", ctx_max);
                        // No device, so both site A and site B are in play and
                        // the whole charge is host memory.
                        let fit = compute_fit(&entry, &profile(None, ram_free), Some(&info));
                        match fit.plan {
                            Some(plan) => {
                                emitted += 1;
                                let charge = cpu_ram_charge(&info, plan.ctx, plan.batch).total();
                                assert!(
                                    fits_ram(charge, gate(ram_free)),
                                    "emitted a plan charging {charge} B against a \
                                     {ram_free} MB gate (ctx {}, kv/tok {kv_per_tok}, \
                                     {layers}x{layer_mb}MB + {non_layer_mb}MB)",
                                    plan.ctx,
                                );
                            }
                            None => {
                                refused += 1;
                                assert_eq!(fit.badge, FitBadge::Red);
                            }
                        }
                    }
                }
            }
        }
        // Guard against the grid degenerating to all-refuse or all-accept, which
        // would make the assertion above true for the wrong reason.
        assert!(emitted > 0 && refused > 0, "grid covered only one outcome");
    }

    /// The reference site's behaviour on a GPU-resident plan is unchanged: the
    /// compute buffer was charged to the device budget at the packing step and
    /// must not be charged a second time against host RAM.
    ///
    /// Tightened deliberately — host RAM is set so that the plan fits on its real
    /// charge (1000 MiB of non-layer weights against a 1024 MiB gate) and would
    /// vanish if the compute buffer were also charged (1448 MiB). So this fails
    /// if the fix over-reaches. Free RAM moved 1500 -> 2048 when the absolute OS
    /// reserve entered the gate; the device is not unified, so `budget_mb`
    /// ignores host RAM and the VRAM side of this fixture is untouched.
    #[test]
    fn a_gpu_resident_plan_does_not_charge_the_compute_buffer_to_host_ram() {
        let info = gguf(10, 100, 1_000, 0, 50);
        let entry = ctx_capped("m", 4_096);
        let fit = compute_fit(&entry, &profile(Some(4_096), 2_048), Some(&info));

        let plan = fit.plan.expect("full offload must still plan");
        assert_eq!(plan.n_gpu_layers, 11, "layers + output slot all on device");
        assert!(plan.kv_offload);
        assert_eq!(fit.badge, FitBadge::Green);

        let charge = ram_charge(&info, 10 * MB * 100, 4_096, DEFAULT_BATCH, true, true);
        assert_eq!(charge.kv, 0);
        assert_eq!(charge.compute, 0);
        assert_eq!(charge.total(), 1_000 * MB, "only non-layer weights on host");
        assert!(
            !fits_ram(
                charge.total() + compute_buffer_bytes(&info, DEFAULT_BATCH),
                gate(2_048)
            ),
            "premise: double-charging the compute buffer would have refused this plan"
        );
    }

    /// The zero-slot case is the one place a *device* is present and the emitted
    /// plan is nonetheless CPU-resident, so the compute buffer moves back onto
    /// the host. Pins that the charge follows the plan rather than the packing.
    #[test]
    fn a_zero_slot_plan_charges_the_compute_buffer_to_the_host() {
        let info = gguf(10, 100, 500, 64 * 1024, 50);
        // 600 MB of VRAM minus the 512 MB reserve leaves 88 MB — far below the
        // output slot, so the packer lands on zero and emits DevicePlan::cpu().
        let fit = compute_fit(
            &entry("m", ModelFormat::Gguf),
            &profile(Some(600), 4_096),
            Some(&info),
        );
        let plan = fit.plan.expect("plan");
        assert_eq!(plan.n_gpu_layers, 0);
        assert!(!plan.device.is_gpu());

        let charge = cpu_ram_charge(&info, plan.ctx, plan.batch);
        assert_eq!(charge.compute, compute_buffer_bytes(&info, DEFAULT_BATCH));
        assert!(fits_ram(charge.total(), gate(4_096)));
    }

    /// Site C's compute buffer, pinned by the *decision* instead of the helper.
    ///
    /// `a_zero_slot_plan_charges_the_compute_buffer_to_the_host` above asserts
    /// that `cpu_ram_charge` returns a compute term and that the emitted plan
    /// fits. Both of those hold no matter what `plan_for_ctx` does with
    /// `compute_on_gpu`: the first is a property of the helper, and the second
    /// picks a fixture the gate accepts either way. Reverting
    /// `let compute_on_gpu = n_gpu > 0;` to `true` therefore leaves the entire
    /// suite green — verified by sabotage during review of this PR.
    ///
    /// This fixture is the discriminating one. The gate is placed strictly
    /// between the charge without the compute buffer and the charge with it, so
    /// the packer's zero-slot CPU plan is emitted **iff** the compute buffer is
    /// wrongly left on the device budget. That is the freeze R-AM's site C is
    /// about: a `DevicePlan::cpu()` whose 448 MiB scratch was charged only
    /// against VRAM the plan never touches.
    #[test]
    fn a_zero_slot_plan_is_refused_by_its_compute_buffer_alone() {
        // 1000 MiB of weights, no KV (so the ctx rung cannot be what decides),
        // 448 MiB compute buffer because `gguf()` sets n_vocab = 0.
        let info = gguf(10, 100, 0, 0, 50);
        let entry = ctx_capped("m", 4_096);

        // 600 MB of VRAM less the 512 MB reserve leaves 88 MB, far below the
        // 448 MiB scratch plus the 50 MiB output slot, so the packer lands on
        // zero slots and `plan_for_ctx` emits `DevicePlan::cpu()`.
        let profile = profile(Some(600), 2_200);
        let gate = gate(2_200);
        assert_eq!(gate, 1_176, "min(0.75*4400, 0.80*2200, 2200-1024)");

        let weights = info.total_bytes();
        let compute = compute_buffer_bytes(&info, DEFAULT_BATCH);
        assert_eq!(weights, 1_000 * MB);
        assert_eq!(compute, COMPUTE_SLACK_MB * MB);
        assert!(
            fits_ram(weights, gate),
            "premise: charged without the compute buffer this plan is accepted"
        );
        assert!(
            !fits_ram(weights + compute, gate),
            "premise: the compute buffer alone is what puts the charge over"
        );

        let fit = compute_fit(&entry, &profile, Some(&info));
        assert_eq!(
            fit.badge,
            FitBadge::Red,
            "a CPU-resident plan whose compute buffer does not fit must not be emitted"
        );
        assert!(
            fit.plan.is_none(),
            "the zero-slot plan runs on the host, so its scratch is host memory"
        );
    }

    /// The unified-memory half of the cgroup fix, which nothing else pins.
    ///
    /// `budget_mb` clamps a device's memory by host RAM **only** when the device
    /// is `unified`, and `hardware::test_device` is `unified: false`, so every
    /// other fixture in this module is blind to which RAM figure is threaded
    /// into it. Swapping `effective_free_mb(&profile.ram)` back to
    /// `profile.ram.free_mb` in `gguf_fit` left the whole suite green —
    /// verified by sabotage during review of this PR.
    ///
    /// On a unified device the "VRAM" *is* host memory, so the raw figure lets
    /// the device budget escape the cgroup limit entirely. Without this the
    /// cgroup fix is a no-op on exactly the machines where the two pools are
    /// one, which is the argument the PR gives for the divergence and the thing
    /// no test was holding.
    #[test]
    fn a_unified_device_budgets_against_the_cgroup_not_the_host() {
        // 3050 MiB of weights, no KV, 448 MiB compute buffer.
        let info = gguf(10, 300, 50, 0, 100);
        let entry = ctx_capped("m", 4_096);

        let with_unified_device = |profile: &mut HardwareProfile| {
            let mut device = crate::hardware::test_device(Some(16_384));
            device.unified = true;
            device.discrete = false;
            profile.devices = vec![device];
        };

        // Unlimited: the unified device may budget against the host's 32 GB
        // free, so all 10 layers plus the output slot pack and the plan is
        // fully device-resident.
        let mut host = cgroup_profile(65_536, 32_768, None, None);
        with_unified_device(&mut host);
        let roomy = compute_fit(&entry, &host, Some(&info)).plan.expect("plan");
        assert_eq!(
            roomy.n_gpu_layers,
            info.n_layers + 1,
            "premise: unlimited, the whole model packs onto the unified device"
        );
        assert_eq!(roomy.badge, FitBadge::Green);

        // The same host and the same device inside a 4 GB cgroup that has
        // already used 512 MB. `effective_free_mb` is 3584, so the unified
        // clamp cuts the device budget to 3584 - 512 = 3072 MB and only 8 of
        // the 10 layers pack. Reading `ram.free_mb` instead would leave the
        // device budgeting against 16 GB and pack all of them, planning 3050
        // MiB of weights into a cgroup that will admit 3584.
        let mut limited = cgroup_profile(65_536, 32_768, Some(4_096), Some(3_584));
        with_unified_device(&mut limited);
        let capped = compute_fit(&entry, &limited, Some(&info))
            .plan
            .expect("the gate shrinks the plan, it never refuses one");
        assert_eq!(
            capped.n_gpu_layers, 9,
            "the cgroup headroom, not the device's own 16 GB, bounds the packer"
        );
        assert_eq!(
            capped.badge,
            FitBadge::Yellow,
            "a partially offloaded plan, which is the shrink the limit forces"
        );
    }
}

#[cfg(test)]
mod stage_plan_tests {
    use super::tests::gguf;
    use super::tests::profile;
    use super::*;
    use crate::catalog::bundled_catalog;

    fn entry(id: &str) -> CatalogEntry {
        bundled_catalog()
            .models
            .iter()
            .find(|e| e.id == id)
            .expect("catalog entry")
            .clone()
    }

    #[test]
    fn user_shaped_4gb_profile_keeps_kokoro_green() {
        let fit = compute_fit(&entry("kokoro-82m"), &profile(Some(3689), 7088), None);
        assert_eq!(
            fit.badge,
            FitBadge::Green,
            "rtx 3050 4gb with 3689mb free budgets 2951mb against a 2560mb need: an orange \
             kokoro on this shape means the profiler lost the cuda device, not that the \
             voice is heavy"
        );
    }

    #[test]
    fn user_shaped_4gb_profile_rates_uninstalled_teacher_llm_yellow() {
        let fit = compute_fit(&entry("gemma4-e4b-q4km"), &profile(Some(3689), 7088), None);
        assert_eq!(
            fit.badge,
            FitBadge::Yellow,
            "5500mb need against a 2951mb budget clears the 0.3 hybrid floor: partial, \
             not cpu-bound"
        );
    }

    #[test]
    fn user_shaped_4gb_profile_packs_installed_teacher_llm_partially_on_cuda() {
        let mut info = gguf(42, 106, 200, 107_520, 60);
        info.n_vocab = 262_144;
        let fit = compute_fit(
            &entry("gemma4-e4b-q4km"),
            &profile(Some(3689), 7088),
            Some(&info),
        );
        let plan = fit.plan.expect("installed weights always plan");
        assert_eq!(fit.badge, FitBadge::Yellow);
        assert!(
            plan.device.is_gpu(),
            "a yellow teacher llm must still carry its gpu layers, or the strip copy \
             calling it cpu-bound is false: {plan:?}"
        );
        assert!(
            plan.n_gpu_layers > 0 && plan.n_gpu_layers <= plan.n_layers_total,
            "partial means partial: {plan:?}"
        );
    }

    #[test]
    fn tts_plan_uses_gpu_when_vram_fits() {
        let plan = tts_fit(&entry("kokoro-82m"), &profile(Some(8192), 16384));
        assert_eq!(plan.model_id, "kokoro-82m");
        assert!(plan.device.is_gpu());
        assert_eq!(plan.tts_workers, DEFAULT_TTS_WORKERS);
        assert_eq!(plan.badge, FitBadge::Green);
        assert!(TTS_BATCH_LADDER.contains(&plan.batch));
    }

    #[test]
    fn tts_plan_falls_back_to_cpu_without_gpu() {
        let plan = tts_fit(&entry("kokoro-82m"), &profile(None, 16384));
        assert!(!plan.device.is_gpu());
        assert_eq!(plan.badge, FitBadge::Orange);
    }

    #[test]
    fn tts_plan_scales_batch_down_under_vram_pressure() {
        let roomy = tts_fit(&entry("kokoro-82m"), &profile(Some(16384), 16384));
        let tight = tts_fit(&entry("kokoro-82m"), &profile(Some(3400), 16384));
        assert!(tight.batch <= roomy.batch);
    }

    #[test]
    fn tts_plan_is_red_when_neither_pool_fits() {
        let plan = tts_fit(&entry("chatterbox-0.5b"), &profile(None, 512));
        assert!(!plan.device.is_gpu());
        assert_eq!(plan.badge, FitBadge::Red);
    }

    #[test]
    fn tts_plan_never_carries_gguf_fields() {
        let plan = tts_fit(&entry("kokoro-82m"), &profile(Some(8192), 16384));
        let json = serde_json::to_value(&plan).expect("serialize");
        assert!(json.get("n_gpu_layers").is_none());
        assert!(json.get("ctx").is_none());
        assert!(json.get("tts_workers").is_some());
    }

    #[test]
    fn align_plan_shape_matches_spec() {
        let plan = align_fit(&entry("whisperx-small-int8"), &profile(None, 16384));
        let json = serde_json::to_value(&plan).expect("serialize");
        assert_eq!(json["model_id"], "whisperx-small-int8");
        assert_eq!(json["device"]["backend"], "cpu");
        assert_eq!(json["align_workers"], DEFAULT_ALIGN_WORKERS);
        assert!(json.get("batch").is_none());
        assert!(json.get("tts_workers").is_none());
    }

    #[test]
    fn align_declares_the_cpu_even_when_a_gpu_would_fit() {
        let plan = align_fit(&entry("whisperx-small-int8"), &profile(Some(8192), 16384));
        assert!(
            !plan.device.is_gpu(),
            "align runs the int8 build it was quantised for on the cpu (PIPELINE_SPEC section 7). \
             ctranslate2 4.8.1 dlopens libcublas.so.12 while the gpu group installs cu13, so a \
             gpu align plan selects a path that cannot load, and RTF_align ~= 0.07 on cpu leaves \
             nothing worth chasing: {plan:?}"
        );
        assert_eq!(plan.align_workers, DEFAULT_ALIGN_WORKERS);
    }

    #[test]
    fn stage_plans_are_independent_of_the_script_plan() {
        let tts = tts_fit(&entry("kokoro-82m"), &profile(Some(8192), 16384));
        let align = align_fit(&entry("whisperx-small-int8"), &profile(Some(8192), 16384));
        assert_ne!(tts.model_id, align.model_id);
        assert_eq!(tts.model_id, "kokoro-82m");
    }

    #[test]
    fn ladder_floors_degenerate_ctx_at_min_ctx() {
        assert_eq!(ladder(Some(0)), vec![MIN_CTX]);
        assert_eq!(ladder(Some(256)), vec![MIN_CTX]);
        assert_eq!(ladder(Some(1024)), vec![1024]);
        assert_eq!(ladder(Some(4096)), vec![4096]);
        assert_eq!(ladder(Some(8192)), vec![8192, 4096]);
    }
}
