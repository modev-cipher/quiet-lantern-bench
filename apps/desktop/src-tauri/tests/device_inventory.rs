use txtcaster_lib::catalog::{self, CatalogEntry, ModelFormat, ModelTask};
use txtcaster_lib::hardware::device::{DeviceBackend, DevicePlan};
use txtcaster_lib::hardware::{
    budget_mb, plan, select_for, select_primary, usable_memory_mb, CpuInfo, DeviceInfo, DiskInfo,
    HardwareProfile, MemorySource, RamInfo, UNKNOWN_VRAM_FLOOR_MB,
};

fn device(
    key: &str,
    backend: DeviceBackend,
    name: &str,
    total: Option<u64>,
    free: Option<u64>,
    discrete: bool,
    unified: bool,
) -> DeviceInfo {
    DeviceInfo {
        llm_runtime: None,
        key: key.into(),
        backend,
        ordinal: 0,
        vendor: "Fixture".into(),
        name: name.into(),
        memory_total_mb: total,
        memory_free_mb: free,
        unified,
        discrete,
        memory_source: if free.is_some() {
            MemorySource::Nvml
        } else if total.is_some() {
            MemorySource::VkHeap
        } else {
            MemorySource::Unknown
        },
    }
}

fn apple_silicon() -> HardwareProfile {
    profile(
        vec![device(
            "metal:0",
            DeviceBackend::Metal,
            "Apple M3 Pro",
            Some(18_432),
            Some(14_000),
            false,
            true,
        )],
        18_432,
        14_000,
    )
}

fn twin_discrete() -> HardwareProfile {
    profile(
        vec![
            device(
                "pci:0000:01:00.0",
                DeviceBackend::Cuda,
                "RTX 4090",
                Some(24_564),
                Some(24_000),
                true,
                false,
            ),
            device(
                "pci:0000:41:00.0",
                DeviceBackend::Cuda,
                "RTX 4090",
                Some(24_564),
                Some(24_000),
                true,
                false,
            ),
        ],
        65_536,
        48_000,
    )
}

fn igpu_plus_dgpu() -> HardwareProfile {
    profile(
        vec![
            device(
                "pci:0000:00:02.0",
                DeviceBackend::Vulkan,
                "Intel Iris Xe",
                Some(11_745),
                Some(6_639),
                false,
                true,
            ),
            device(
                "pci:0000:01:00.0",
                DeviceBackend::Cuda,
                "RTX 3050 Laptop",
                Some(4_096),
                Some(3_767),
                true,
                false,
            ),
        ],
        15_660,
        9_000,
    )
}

fn headless() -> HardwareProfile {
    profile(Vec::new(), 15_660, 9_000)
}

fn unreadable_cuda() -> HardwareProfile {
    profile(
        vec![device(
            "pci:0000:03:00.0",
            DeviceBackend::Cuda,
            "Unnamed CUDA card",
            None,
            None,
            true,
            false,
        )],
        15_660,
        9_000,
    )
}

fn profile(devices: Vec<DeviceInfo>, ram_total: u64, ram_free: u64) -> HardwareProfile {
    HardwareProfile {
        devices,
        ram: RamInfo {
            total_mb: ram_total,
            free_mb: ram_free,
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

fn amd_workstation() -> HardwareProfile {
    profile(
        vec![device(
            "pci:0000:03:00.0",
            DeviceBackend::Vulkan,
            "Radeon RX 7900 XTX",
            Some(24_560),
            Some(24_000),
            true,
            false,
        )],
        65_536,
        48_000,
    )
}

fn dual_vendor() -> HardwareProfile {
    profile(
        vec![
            device(
                "pci:0000:03:00.0",
                DeviceBackend::Vulkan,
                "Radeon RX 7900 XTX",
                Some(24_560),
                Some(24_000),
                true,
                false,
            ),
            device(
                "pci:0000:0a:00.0",
                DeviceBackend::Cuda,
                "RTX 3060",
                Some(12_288),
                Some(12_000),
                true,
                false,
            ),
        ],
        65_536,
        48_000,
    )
}

fn entry(id: &str, vram_mb: Option<u64>) -> CatalogEntry {
    let mut entry = catalog::bundled_catalog()
        .models
        .iter()
        .find(|m| m.task == ModelTask::Tts)
        .expect("a tts model in the bundled catalog")
        .clone();
    entry.id = id.into();
    entry.full_gpu_vram_mb = vram_mb;
    entry
}

#[test]
fn a_unified_device_is_budgeted_once_not_charged_to_both_pools() {
    let apple = apple_silicon();
    let metal = apple.primary().expect("the metal device");
    assert_eq!(usable_memory_mb(metal), 14_000);
    assert_eq!(
        budget_mb(metal, apple.ram.free_mb),
        14_000,
        "its memory is the RAM pool, so charging a VRAM gate and a RAM gate would reject a \
         model that actually fits"
    );

    let squeezed = budget_mb(metal, 6_000);
    assert_eq!(
        squeezed, 6_000,
        "when the system has less RAM free than the device claims, the smaller number wins"
    );
}

#[test]
fn a_metal_device_plans_on_the_gpu_rather_than_falling_back_to_cpu() {
    let apple = apple_silicon();
    let mut gguf = entry("some-llm", Some(1_000));
    gguf.format = ModelFormat::Gguf;

    let fit = plan::tts_fit(&gguf, &apple);
    assert!(fit.device.is_gpu());
    assert_eq!(
        fit.device,
        DevicePlan::gpu(DeviceBackend::Metal, 0).with_key(Some("metal:0".into())),
        "a backend the enum did not know about used to plan CPU-only on every Apple machine"
    );
}

#[test]
fn a_torch_model_on_apple_silicon_plans_cpu_until_the_engine_has_an_mps_path() {
    let apple = apple_silicon();
    let mut voice = entry("kokoro-82m", Some(1_000));
    voice.format = ModelFormat::Pytorch;

    assert_eq!(
        plan::tts_fit(&voice, &apple).device,
        DevicePlan::cpu(),
        "the engine maps only CUDA to a torch device, so planning Metal here would quote a \
         GPU estimate for a run that silently uses the CPU; R-J adds the MPS path"
    );
}

#[test]
fn two_identical_cards_are_told_apart_by_bus_id() {
    let twins = twin_discrete();
    assert_eq!(twins.devices.len(), 2);
    assert_ne!(twins.devices[0].key, twins.devices[1].key);
    assert_eq!(
        twins.devices[0].name, twins.devices[1].name,
        "the fixture exists because the names collide; only the bus ID separates them"
    );
    let chosen = select_primary(&twins.devices).expect("a device");
    assert_eq!(
        chosen.key, "pci:0000:01:00.0",
        "the tiebreak is the bus ID so the plan is reproducible run to run"
    );
}

#[test]
fn twin_cards_do_not_pool_their_memory_for_a_fit() {
    let twins = twin_discrete();
    let huge = select_for(&twins.devices, Some(40_000), twins.ram.free_mb).expect("a device");
    assert_eq!(
        budget_mb(huge, twins.ram.free_mb),
        24_000,
        "tensor_split ships off, so a model larger than one card is planned against one \
         card and takes the CPU ladder for the rest"
    );
}

#[test]
fn the_reference_laptop_prefers_the_discrete_card_until_the_model_outgrows_it() {
    let laptop = igpu_plus_dgpu();
    let small = laptop.select_for(Some(2_000)).expect("a device");
    assert_eq!(small.key, "pci:0000:01:00.0");

    let large = laptop.select_for(Some(8_000)).expect("a device");
    assert_eq!(
        large.key, "pci:0000:00:02.0",
        "a model the 4 GB card cannot hold belongs on the pool that can, even though that \
         pool is the slower integrated one"
    );
}

#[test]
fn a_headless_machine_plans_on_the_cpu_without_erroring() {
    let none = headless();
    assert!(none.primary().is_none());
    assert!(none.select_for(Some(1_000)).is_none());
    let fit = plan::tts_fit(&entry("voice", Some(1_000)), &none);
    assert_eq!(fit.device, DevicePlan::cpu());
    assert!(!fit.device.is_gpu());
}

#[test]
fn an_unreadable_device_is_usable_against_the_floor_not_excluded() {
    let unknown = unreadable_cuda();
    let device = unknown.primary().expect("the device is still selectable");
    assert_eq!(usable_memory_mb(device), UNKNOWN_VRAM_FLOOR_MB);
    assert_eq!(
        unknown
            .select_for(Some(1_500))
            .map(|device| device.key.as_str()),
        Some("pci:0000:03:00.0"),
        "a driver that will not report memory must not hard-block a selection"
    );
}

#[test]
fn the_fingerprint_changes_with_the_inventory_and_not_with_free_memory() {
    let laptop = igpu_plus_dgpu();
    let baseline = laptop.fingerprint();

    let mut busy = igpu_plus_dgpu();
    for device in busy.devices.iter_mut() {
        device.memory_free_mb = Some(64);
    }
    assert_eq!(
        busy.fingerprint(),
        baseline,
        "a busy machine is the same machine; re-benchmarking because a game is running is \
         churn, not invalidation"
    );

    let mut unplugged = igpu_plus_dgpu();
    unplugged.devices.retain(|device| !device.discrete);
    assert_ne!(unplugged.fingerprint(), baseline);

    assert_ne!(twin_discrete().fingerprint(), baseline);
    assert_ne!(headless().fingerprint(), baseline);
    assert_ne!(apple_silicon().fingerprint(), baseline);
}

#[test]
fn the_fingerprint_is_stable_across_probe_order() {
    let forward = igpu_plus_dgpu();
    let mut reversed = igpu_plus_dgpu();
    reversed.devices.reverse();
    assert_eq!(
        forward.fingerprint(),
        reversed.fingerprint(),
        "probes answer in whatever order they finish; an order-sensitive fingerprint would \
         re-benchmark at random"
    );
}

#[test]
fn a_torch_only_voice_will_not_be_planned_onto_a_vulkan_card() {
    let amd = amd_workstation();
    let mut voice = entry("kokoro-82m", Some(1_000));
    voice.format = ModelFormat::Pytorch;

    let fit = plan::tts_fit(&voice, &amd);
    assert_eq!(
        fit.device,
        DevicePlan::cpu(),
        "kokoro checks torch.cuda, so a 24 GB AMD card wins the ranking on capacity and \
         then clamps to CPU on every run while the ETA quotes a GPU real-time factor"
    );
}

#[test]
fn a_gguf_model_is_planned_onto_a_vulkan_card() {
    let amd = amd_workstation();
    let mut llm = entry("some-llm", Some(1_000));
    llm.format = ModelFormat::Gguf;

    let fit = plan::tts_fit(&llm, &amd);
    assert_eq!(
        fit.device.backend,
        DeviceBackend::Vulkan,
        "llama.cpp offloads on whatever backend its build supports, so excluding non-CUDA \
         devices for it would strand the reference machine's own Vulkan build on the CPU"
    );
}

#[test]
fn a_torch_only_voice_takes_the_cuda_card_beside_a_bigger_vulkan_one() {
    let laptop = igpu_plus_dgpu();
    let mut voice = entry("kokoro-82m", Some(1_000));
    voice.format = ModelFormat::Pytorch;

    let fit = plan::tts_fit(&voice, &laptop);
    assert_eq!(fit.device.backend, DeviceBackend::Cuda);
    assert_eq!(
        fit.device.key.as_deref(),
        Some("pci:0000:01:00.0"),
        "the capability filter must pick a usable device, not merely reject the ranking's \
         first answer"
    );
}

#[test]
fn a_torch_voice_falls_back_to_the_cuda_card_when_a_bigger_vulkan_card_outranks_it() {
    let workstation = dual_vendor();
    let mut voice = entry("kokoro-82m", Some(3_000));
    voice.format = ModelFormat::Pytorch;

    let fit = plan::tts_fit(&voice, &workstation);
    assert_eq!(
        fit.device.key.as_deref(),
        Some("pci:0000:0a:00.0"),
        "rejecting the ranking's winner is not the same as choosing among the devices the \
         runtime can use; an idle 12 GB CUDA card must not lose to a CPU fallback"
    );
}

#[test]
fn the_badge_and_the_plan_agree_about_a_runtime_that_cannot_use_the_card() {
    let amd = amd_workstation();
    let mut voice = entry("kokoro-82m", Some(3_000));
    voice.format = ModelFormat::Pytorch;

    let fit = plan::compute_fit(&voice, &amd, None);
    assert_eq!(
        fit.badge,
        plan::FitBadge::Orange,
        "the catalog badge is the surface the user reads; promising green while tts_fit \
         plans CPU is the optimistic estimate the capability rule exists to prevent"
    );
}

#[test]
fn the_plan_is_packed_against_the_device_it_names() {
    let laptop = igpu_plus_dgpu();
    let mut voice = entry("kokoro-82m", Some(5_000));
    voice.format = ModelFormat::Pytorch;

    let fit = plan::tts_fit(&voice, &laptop);
    assert_eq!(
        fit.device,
        DevicePlan::cpu(),
        "5 GB does not fit the 3767 MB CUDA card, and the 6639 MB iGPU is Vulkan, which \
         torch cannot use; planning the iGPU would size a batch against memory the run \
         never gets"
    );
}

#[test]
fn a_projector_reserve_shrinks_a_device_whose_free_memory_is_unknown() {
    let mut total_only = profile(
        vec![device(
            "pci:0000:01:00.0",
            DeviceBackend::Cuda,
            "RTX 3050",
            Some(4_096),
            None,
            true,
            false,
        )],
        16_384,
        12_000,
    );
    let before = usable_memory_mb(total_only.primary().expect("device"));
    txtcaster_lib::hardware::reserve_vram(&mut total_only, 1_585);
    let after = usable_memory_mb(total_only.primary().expect("device"));
    assert_eq!(
        after,
        before - 1_585,
        "the reserve is a no-op when memory_free_mb is None, so vision's projector \
         disappears from the budget and the card is planned as if it were empty"
    );
}

#[test]
fn a_projector_reserve_shrinks_a_device_with_no_readable_memory_at_all() {
    let mut unknown = unreadable_cuda();
    let before = usable_memory_mb(unknown.primary().expect("device"));
    assert_eq!(before, UNKNOWN_VRAM_FLOOR_MB);
    txtcaster_lib::hardware::reserve_vram(&mut unknown, 1_585);
    let after = usable_memory_mb(unknown.primary().expect("device"));
    assert!(
        after < before,
        "a floored device must still be charged for a reservation, or the floor becomes a \
         licence to over-plan"
    );
}

#[test]
fn vision_does_not_promise_the_gpu_a_projector_has_already_filled() {
    let total_only = profile(
        vec![device(
            "pci:0000:01:00.0",
            DeviceBackend::Cuda,
            "RTX 3050",
            Some(4_096),
            None,
            true,
            false,
        )],
        16_384,
        12_000,
    );
    let mut mllm = entry("mllm", Some(3_000));
    mllm.format = ModelFormat::Gguf;
    let mmproj_bytes = 945 * 1024 * 1024;

    let fit = plan::vision_fit(&mllm, &total_only, None, mmproj_bytes);
    assert_ne!(
        fit.badge,
        plan::FitBadge::Green,
        "4096 MB minus a 1585 MB projector cannot hold a 3000 MB model with headroom; a \
         green badge here is a planned OOM"
    );
}

#[test]
fn an_unreadable_device_never_badges_green() {
    let unknown = unreadable_cuda();
    let mut small = entry("tiny-voice", Some(1_000));
    small.format = ModelFormat::Pytorch;

    let fit = plan::compute_fit(&small, &unknown, None);
    assert_eq!(
        fit.badge,
        plan::FitBadge::Yellow,
        "1000 MB clears the 2048 MB floor, but the floor is a guess, not a measurement; \
         green promises a fit nobody measured (HARDWARE_PROFILER: 'the fit badge is yellow \
         with VRAM not reportable on this driver')"
    );
}

#[test]
fn unreadable_memory_alone_never_makes_a_badge_red() {
    let unknown = unreadable_cuda();
    let mut fits_the_floor = entry("tiny-voice", Some(1_000));
    fits_the_floor.format = ModelFormat::Pytorch;

    let fit = plan::compute_fit(&fits_the_floor, &unknown, None);
    assert_ne!(
        fit.badge,
        plan::FitBadge::Red,
        "a selection is never hard-blocked because a driver would not report memory"
    );
    assert_eq!(fit.badge, plan::FitBadge::Yellow);
}

#[test]
fn a_measured_impossibility_still_badges_red_on_an_unreadable_device() {
    let unknown = unreadable_cuda();
    let mut huge = entry("huge-voice", Some(48_000));
    huge.format = ModelFormat::Pytorch;
    huge.min_free_ram_mb = 512_000;

    assert_eq!(
        plan::compute_fit(&huge, &unknown, None).badge,
        plan::FitBadge::Red,
        "this red is a RAM verdict, not a consequence of unreadability; painting it yellow \
         would promise a run that cannot start"
    );
}

#[test]
fn a_device_with_readable_memory_still_badges_green_when_it_fits() {
    let laptop = igpu_plus_dgpu();
    let mut small = entry("tiny-voice", Some(1_000));
    small.format = ModelFormat::Pytorch;
    assert_eq!(
        plan::compute_fit(&small, &laptop, None).badge,
        plan::FitBadge::Green,
        "the yellow cap must apply only to unreadable devices, or every badge collapses"
    );
}

#[test]
fn the_unreadable_cap_never_promotes_a_worse_badge() {
    let unknown = unreadable_cuda();
    let mut hungry = entry("hungry", Some(48_000));
    hungry.format = ModelFormat::Pytorch;
    hungry.min_free_ram_mb = 1_024;

    let fit = plan::compute_fit(&hungry, &unknown, None);
    assert_eq!(
        fit.badge,
        plan::FitBadge::Orange,
        "the cap is a ceiling, not an assignment: promoting orange to yellow tells the \
         user a cpu-bound run is a fast one"
    );
}

#[test]
fn the_unreadable_cap_leaves_the_vision_region_timeout_in_place() {
    let unknown = unreadable_cuda();
    let mut mllm = entry("mllm", Some(48_000));
    mllm.format = ModelFormat::Gguf;
    mllm.min_free_ram_mb = 1_024;

    let badge =
        serde_json::to_value(plan::compute_fit(&mllm, &unknown, None).badge).expect("badge json");
    assert_ne!(
        badge, "yellow",
        "vision arms its per-region cpu timeout only on orange or red; promoting the badge \
         strips the timeout on exactly the unreadable-vram machine that needs it"
    );
}

#[test]
fn the_unreadable_cap_does_not_touch_a_plan_that_landed_on_the_cpu() {
    let unknown = unreadable_cuda();
    let mut torch_only = entry("voice", Some(1_000));
    torch_only.format = ModelFormat::Pytorch;

    let mut vulkan_box = unknown.clone();
    vulkan_box.devices[0].backend = DeviceBackend::Vulkan;
    let fit = plan::compute_fit(&torch_only, &vulkan_box, None);
    assert_ne!(
        fit.badge,
        plan::FitBadge::Yellow,
        "torch cannot use that device, so the plan is a cpu plan; an unreadable gpu the run \
         never touches must not colour the badge"
    );
}

#[test]
fn the_persisted_tts_badge_agrees_with_the_panel_on_an_unreadable_device() {
    let unknown = unreadable_cuda();
    let mut voice = entry("kokoro-82m", Some(1_000));
    voice.format = ModelFormat::Pytorch;

    let panel = plan::compute_fit(&voice, &unknown, None).badge;
    let persisted = plan::tts_fit(&voice, &unknown).badge;
    assert_eq!(
        persisted, panel,
        "the models panel reads compute_fit and the job reads tts_fit; the spec says never \
         green on an unreadable device, and one of these two was still saying green"
    );
    assert_eq!(persisted, plan::FitBadge::Yellow);
}

#[test]
fn the_persisted_align_badge_agrees_with_the_panel_on_an_unreadable_device() {
    let unknown = unreadable_cuda();
    let mut aligner = entry("whisperx-small", Some(1_000));
    aligner.format = ModelFormat::Ct2;

    assert_eq!(
        plan::align_fit(&aligner, &unknown).badge,
        plan::compute_fit(&aligner, &unknown, None).badge,
        "the models panel reads compute_fit and the job reads align_fit; if only one of them \
         knows align is cpu-int8 the panel advertises a device the run never touches"
    );
    assert_ne!(
        plan::align_fit(&aligner, &unknown).badge,
        plan::FitBadge::Yellow,
        "yellow is the unreadable-gpu colour, and ctranslate2 has no gpu path in this build, so \
         an unreadable gpu is as irrelevant here as it is to a torch-only entry -- same reason \
         as the_persisted_align_badge... sibling above"
    );
}
