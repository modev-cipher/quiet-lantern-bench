import { NO_DEVICES, device, inventory } from "./deviceFixtures";
import { t } from "./i18n";
import { describe, expect, it } from "vitest";

import {
  canSetDefault,
  diskBlockMessage,
  downloadPct,
  downloadStatusLabel,
  fitBadge,
  UNREADABLE_VRAM_DETAIL,
  runsOnCpu,
  fitReason,
  formatBytes,
  groupByTask,
  installedSet,
  licenseTag,
  recommendModel,
  setupStatus,
  projectorMissing,
  projectorMissingSet,
  updateAvailable,
  updateLabel,
  updateSizeBytes,
  completeReadySet,
  textReadySet,
  visionReadySet,
  type BadgeKind,
  type CatalogModelEntry,
  type HardwareFitProfile,
  type ModelDownloadProgress,
  type ResourcePlan,
} from "./modelCatalogModel";

function model(overrides: Partial<CatalogModelEntry> = {}): CatalogModelEntry {
  return {
    id: "m1",
    task: "llm",
    display_name: "Model One",
    family: "test",
    quant: "Q4_K_M",
    format: "gguf",
    disk_bytes: 2_500_000_000,
    full_gpu_vram_mb: 3500,
    min_free_ram_mb: 4096,
    hybrid_capable: true,
    quality_score: 70,
    speed_score: 80,
    license: { spdx: "MIT", commercial_ok: true },
    badge: "green",
    manifest_sha256: "manifest-current",
    ...overrides,
  };
}

function plan(overrides: Partial<ResourcePlan> = {}): ResourcePlan {
  return {
    model_id: "m1",
    device: "cuda",
    n_gpu_layers: 28,
    n_layers_total: 40,
    kv_offload: true,
    ctx: 8192,
    batch: 512,
    tok_s_est: 21.5,
    badge: "yellow",
    ...overrides,
  };
}

function hw(overrides: Partial<HardwareFitProfile> = {}): HardwareFitProfile {
  return {
    ...inventory([device({ memoryFreeMb: 12_000 })]),
    ram: { freeMb: 16_384 },
    disk: { modelsFreeGb: 100 },
    ...overrides,
  };
}

describe("fitBadge", () => {
  it("maps each wire badge to its label", () => {
    const labels: Record<BadgeKind, string> = {
      green: "Fast",
      yellow: "Slower",
      orange: "Heavy",
      red: "Not advised",
    };
    for (const kind of Object.keys(labels) as BadgeKind[]) {
      const b = fitBadge(model({ badge: kind }));
      expect(b.kind).toBe(kind);
      expect(b.label).toBe(labels[kind]);
      expect(b.detail.length).toBeGreaterThan(0);
    }
  });

  it("shows the block split for yellow plans", () => {
    const b = fitBadge(
      model({ badge: "yellow", plan: plan({ n_gpu_layers: 28 }) }),
    );
    expect(b.detail).toContain("68");
    expect(b.detail).toContain("% on the GPU");
  });

  it("keeps generic copy when no plan is attached", () => {
    const b = fitBadge(model({ badge: "yellow", plan: null }));
    expect(b.detail).toBe(
      "Runs split across GPU + CPU — slower on this machine",
    );
  });

  it("converts slot counts to block counts at the boundaries", () => {
    const full = fitBadge(
      model({
        badge: "yellow",
        plan: plan({ n_gpu_layers: 41, n_layers_total: 40 }),
      }),
    );
    expect(full.detail).toBe(t("fit.yellow.detailPlan", { pct: 100 }));
    const outputOnly = fitBadge(
      model({ badge: "yellow", plan: plan({ n_gpu_layers: 1 }) }),
    );
    expect(outputOnly.detail).toBe(t("fit.yellow.detailPlan", { pct: 0 }));
  });
});

describe("diskBlockMessage", () => {
  it("returns null when disk is sufficient", () => {
    expect(diskBlockMessage(model(), hw())).toBeNull();
  });

  it("returns exact numbers when disk is insufficient", () => {
    const msg = diskBlockMessage(
      model({ disk_bytes: 6_600_000_000 }),
      hw({ disk: { modelsFreeGb: 4.1 } }),
    );
    expect(msg).toContain("6.6 GB");
    expect(msg).toContain("4.1 GB");
  });

  it("returns null when hardware is unknown", () => {
    expect(diskBlockMessage(model(), null)).toBeNull();
  });
});

describe("fitReason", () => {
  it("compares VRAM need against available GPU memory", () => {
    const reason = fitReason(
      model({ full_gpu_vram_mb: 7200 }),
      hw(inventory([device({ memoryFreeMb: 12_288 })])),
    );
    expect(reason).toBe("needs ~7.0 GB VRAM — you have 12.0 GB");
  });

  it("notes CPU fallback when there is no GPU", () => {
    const reason = fitReason(model({ full_gpu_vram_mb: 3500 }), hw(NO_DEVICES));
    expect(reason).toContain("no GPU detected");
  });

  it("falls back to RAM need for CPU-only models", () => {
    const reason = fitReason(
      model({ full_gpu_vram_mb: null, min_free_ram_mb: 2048 }),
      hw({ ram: { freeMb: 16_384 } }),
    );
    expect(reason).toBe("needs ~2.0 GB RAM — you have 16.0 GB free");
  });

  it("omits comparison when hardware is unknown", () => {
    const reason = fitReason(model({ full_gpu_vram_mb: null }), null);
    expect(reason).toContain("RAM");
    expect(reason).not.toContain("you have");
  });

  it("does not claim no-GPU for a VRAM model while hardware is still loading", () => {
    const reason = fitReason(model({ full_gpu_vram_mb: 3500 }), null);
    expect(reason).toBe("needs ~3.4 GB VRAM");
    expect(reason).not.toContain("no GPU detected");
  });

  it("says GPU memory is unknown when a GPU exists but free VRAM is not reported", () => {
    const reason = fitReason(
      model({ full_gpu_vram_mb: 3500 }),
      hw(inventory([device({ memoryFreeMb: null, memoryTotalMb: null })])),
    );
    expect(reason).toBe("needs ~3.4 GB VRAM — available GPU memory unknown");
  });
});

describe("formatBytes", () => {
  it("formats GB, MB and KB", () => {
    expect(formatBytes(2_500_000_000)).toBe("2.5 GB");
    expect(formatBytes(355_000_000)).toBe("355 MB");
    expect(formatBytes(63_000)).toBe("63 KB");
    expect(formatBytes(10)).toBe("1 KB");
  });
});

describe("groupByTask", () => {
  it("groups by canonical task order and sorts by quality", () => {
    const groups = groupByTask([
      model({ id: "llm-low", task: "llm", quality_score: 70 }),
      model({ id: "llm-high", task: "llm", quality_score: 92 }),
      model({ id: "tts-1", task: "tts", quality_score: 60 }),
    ]);
    expect(groups.map((g) => g.task)).toEqual(["tts", "llm"]);
    const llm = groups.find((g) => g.task === "llm")!;
    expect(llm.models.map((m) => m.id)).toEqual(["llm-high", "llm-low"]);
    expect(llm.label).toBe("Language models");
  });

  it("hides deprecated models and empty groups", () => {
    const groups = groupByTask([
      model({ id: "old", task: "llm", deprecated: true }),
      model({ id: "v1", task: "tts" }),
    ]);
    expect(groups.map((g) => g.task)).toEqual(["tts"]);
  });

  it("keeps a deprecated model visible while it is installed", () => {
    const groups = groupByTask(
      [
        model({ id: "old", task: "llm", deprecated: true }),
        model({ id: "gone", task: "llm", deprecated: true }),
        model({ id: "v1", task: "tts" }),
      ],
      new Set(["old"]),
    );
    expect(groups.map((g) => g.task)).toEqual(["tts", "llm"]);
    expect(groups[1]!.models.map((m) => m.id)).toEqual(["old"]);
  });

  it("keeps unknown tasks at the end", () => {
    const groups = groupByTask([
      model({ id: "x", task: "experimental" }),
      model({ id: "a", task: "align" }),
    ]);
    expect(groups.map((g) => g.task)).toEqual(["align", "experimental"]);
    expect(groups[1].label).toBe("experimental");
  });
});

describe("recommendModel", () => {
  it("picks the highest-quality green-fit model, tie-broken by speed", () => {
    const rec = recommendModel([
      model({
        id: "g-fast",
        badge: "green",
        quality_score: 80,
        speed_score: 90,
      }),
      model({
        id: "g-best",
        badge: "green",
        quality_score: 92,
        speed_score: 40,
      }),
      model({ id: "y-top", badge: "yellow", quality_score: 97 }),
    ]);
    expect(rec?.model.id).toBe("g-best");
    expect(rec?.fallback).toBe(false);
  });

  it("tie-breaks equal quality by speed", () => {
    const rec = recommendModel([
      model({ id: "slow", badge: "green", quality_score: 80, speed_score: 30 }),
      model({ id: "fast", badge: "green", quality_score: 80, speed_score: 95 }),
    ]);
    expect(rec?.model.id).toBe("fast");
  });

  it("falls back to the best hybrid when nothing is green", () => {
    const rec = recommendModel([
      model({ id: "y-low", badge: "yellow", quality_score: 70 }),
      model({ id: "y-high", badge: "yellow", quality_score: 90 }),
      model({ id: "r", badge: "red", quality_score: 99 }),
    ]);
    expect(rec?.model.id).toBe("y-high");
    expect(rec?.fallback).toBe(true);
  });

  it("falls back to the best of a weak field when nothing runs well", () => {
    const rec = recommendModel([
      model({ id: "r-low", badge: "red", quality_score: 70 }),
      model({ id: "r-high", badge: "red", quality_score: 95 }),
    ]);
    expect(rec?.model.id).toBe("r-high");
    expect(rec?.fallback).toBe(true);
  });

  it("ignores deprecated models and returns null when empty", () => {
    expect(recommendModel([model({ deprecated: true })])).toBeNull();
    expect(recommendModel([])).toBeNull();
  });
});

describe("setupStatus", () => {
  const catalog = [
    model({ id: "kokoro", task: "tts" }),
    model({ id: "qwen", task: "llm" }),
  ];

  it("marks a capability ready when a model for its task is installed", () => {
    const status = setupStatus(catalog, new Set(["kokoro"]));
    const tts = status.find((s) => s.task === "tts")!;
    const llm = status.find((s) => s.task === "llm")!;
    expect(tts.ready).toBe(true);
    expect(tts.label).toBe("Narration voice");
    expect(llm.ready).toBe(false);
  });

  it("only lists capabilities the catalog can provide", () => {
    const status = setupStatus(
      [model({ id: "kokoro", task: "tts" })],
      new Set(),
    );
    expect(status.map((s) => s.task)).toEqual(["tts"]);
  });

  it("ignores deprecated models for task existence and readiness", () => {
    const status = setupStatus(
      [
        model({ id: "kokoro", task: "tts" }),
        model({ id: "old-llm", task: "llm", deprecated: true }),
      ],
      new Set(["old-llm"]),
    );
    expect(status.map((s) => s.task)).toEqual(["tts"]);
    expect(status.some((s) => s.task === "llm")).toBe(false);
  });

  it("counts an installed deprecated model as ready for a live task", () => {
    const status = setupStatus(
      [
        model({ id: "new-llm", task: "llm" }),
        model({ id: "old-llm", task: "llm", deprecated: true }),
      ],
      new Set(["old-llm"]),
    );
    expect(status.find((s) => s.task === "llm")?.ready).toBe(true);
  });
});

describe("downloadPct", () => {
  function progress(
    overrides: Partial<ModelDownloadProgress> = {},
  ): ModelDownloadProgress {
    return {
      modelId: "m1",
      fileIndex: 0,
      filesTotal: 1,
      bytesDone: 0,
      bytesTotal: 100,
      state: "downloading",
      ...overrides,
    };
  }

  it("reports single-file progress", () => {
    expect(downloadPct(progress({ bytesDone: 50 }))).toBe(50);
  });

  it("weights multi-file progress by file index", () => {
    expect(
      downloadPct(
        progress({
          fileIndex: 1,
          filesTotal: 2,
          bytesDone: 50,
          bytesTotal: 100,
        }),
      ),
    ).toBe(75);
  });

  it("clamps to 100 and handles zero totals", () => {
    expect(downloadPct(progress({ bytesDone: 200 }))).toBe(100);
    expect(downloadPct(progress({ filesTotal: 0 }))).toBe(0);
    expect(downloadPct(progress({ bytesTotal: 0 }))).toBe(0);
  });
});

describe("downloadStatusLabel", () => {
  it("labels each state", () => {
    const base: ModelDownloadProgress = {
      modelId: "m1",
      fileIndex: 0,
      filesTotal: 1,
      bytesDone: 0,
      bytesTotal: 1,
      state: "downloading",
    };
    expect(downloadStatusLabel(base)).toBe("Downloading");
    expect(downloadStatusLabel({ ...base, filesTotal: 3, fileIndex: 1 })).toBe(
      "Downloading file 2/3",
    );
    expect(downloadStatusLabel({ ...base, state: "verifying" })).toBe(
      "Verifying…",
    );
    expect(
      downloadStatusLabel({
        ...base,
        state: "verifying",
        filesTotal: 15,
        fileIndex: 4,
      }),
    ).toBe("Verifying file 5/15…");
    expect(downloadStatusLabel({ ...base, state: "done" })).toBe("Finishing…");
  });
});

describe("installedSet / licenseTag / canSetDefault", () => {
  it("builds an id set from installs", () => {
    expect(
      installedSet([{ modelId: "a", diskBytes: 1, sha256: "x" }]).has("a"),
    ).toBe(true);
  });

  it("tags non-commercial licenses only", () => {
    expect(licenseTag(model())).toBeNull();
    expect(
      licenseTag(
        model({ license: { spdx: "CC-BY-NC-4.0", commercial_ok: false } }),
      ),
    ).toBe("non-commercial");
  });

  it("allows defaults only for pipeline tasks", () => {
    expect(canSetDefault("llm")).toBe(true);
    expect(canSetDefault("tts")).toBe(true);
    expect(canSetDefault("layout")).toBe(false);
  });
});

describe("updateAvailable / updateLabel", () => {
  const install = {
    modelId: "m1",
    diskBytes: 2_000_000_000,
    sha256: "manifest-old",
  };

  it("reports an update when the install manifest is stale", () => {
    expect(updateAvailable(model(), [install])).toBe(true);
  });

  it("reports no update when manifests match", () => {
    expect(
      updateAvailable(model(), [{ ...install, sha256: "manifest-current" }]),
    ).toBe(false);
  });

  it("reports no update for models that are not installed", () => {
    expect(updateAvailable(model(), [])).toBe(false);
  });

  it("sizes the update as the difference from the installed bytes", () => {
    expect(updateSizeBytes(model(), [install])).toBe(500_000_000);
    expect(updateLabel(model(), [install])).toBe("Update (500 MB)");
  });

  it("labels a plain update when no extra bytes are needed", () => {
    expect(
      updateLabel(model(), [{ ...install, diskBytes: 3_000_000_000 }]),
    ).toBe("Update");
  });

  describe("pre-projector install of a vision-capable LLM", () => {
    const qwen = model({
      id: "qwen3.5-9b-instruct-q4km",
      disk_bytes: 6_600_000_000,
      projector_bytes: 900_000_000,
      capabilities: ["teacher_mode", "figure_explanation"],
    });
    const stale = {
      modelId: qwen.id,
      diskBytes: 5_700_000_000,
      sha256: "manifest-old",
      missingFiles: ["mmproj-F16.gguf"],
    };

    it("offers the projector as an incremental download sized to the projector alone", () => {
      expect(updateAvailable(qwen, [stale])).toBe(true);
      expect(updateSizeBytes(qwen, [stale])).toBe(900_000_000);
      expect(updateLabel(qwen, [stale])).toBe(
        "Add figure explanation (900 MB)",
      );
    });

    it("still offers the projector when the manifest already matches", () => {
      const matching = { ...stale, sha256: "manifest-current" };
      expect(updateAvailable(qwen, [matching])).toBe(true);
      expect(updateLabel(qwen, [matching])).toBe(
        "Add figure explanation (900 MB)",
      );
    });

    it("falls back to the manifest diff when weights are missing too", () => {
      const gutted = {
        ...stale,
        missingFiles: ["weights.gguf", "mmproj-F16.gguf"],
      };
      expect(updateLabel(qwen, [gutted])).toBe("Update (900 MB)");
    });

    it("offers nothing once every file is present and the manifest matches", () => {
      expect(
        updateAvailable(qwen, [
          { ...stale, sha256: "manifest-current", missingFiles: [] },
        ]),
      ).toBe(false);
    });
  });
});

describe("fitBadge on an unreadable device", () => {
  it("names the driver instead of promising a split", () => {
    const badge = fitBadge(model({ badge: "yellow", memory_unreadable: true }));
    expect(badge.kind).toBe("yellow");
    expect(badge.detail).toBe(UNREADABLE_VRAM_DETAIL());
  });

  it("still describes the split when the memory is known", () => {
    const badge = fitBadge(
      model({ badge: "yellow", plan: plan({ n_gpu_layers: 28 }) }),
    );
    expect(badge.detail).toContain("% on the GPU");
  });

  it("is unchanged when no hardware is known yet", () => {
    expect(fitBadge(model({ badge: "yellow" })).detail).toContain(
      "Runs split across GPU + CPU",
    );
  });
});

describe("the unreadable copy follows the model, not the primary device", () => {
  it("stays silent for a model the backend planned on a readable card", () => {
    const badge = fitBadge(
      model({
        badge: "yellow",
        plan: plan({ n_gpu_layers: 28 }),
        memory_unreadable: false,
      }),
    );
    expect(badge.detail).not.toBe(UNREADABLE_VRAM_DETAIL());
  });

  it("names the driver for a model the backend planned on the unreadable card", () => {
    const badge = fitBadge(model({ badge: "yellow", memory_unreadable: true }));
    expect(badge.detail).toBe(UNREADABLE_VRAM_DETAIL());
  });

  it("asks the backend rather than the primary device, which can differ per model", () => {
    const onDualVendorBox = model({
      badge: "yellow",
      plan: plan({ n_gpu_layers: 28 }),
      memory_unreadable: false,
    });
    expect(fitBadge(onDualVendorBox).detail).toContain("% on the GPU");
  });
});

describe("install readiness from the missing-files probe", () => {
  const complete = { modelId: "m1", diskBytes: 1, sha256: "manifest-current" };
  const preProjector = {
    modelId: "qwen3.5-9b-instruct-q4km",
    diskBytes: 1,
    sha256: "manifest-old",
    missingFiles: ["mmproj-F16.gguf"],
  };
  const gutted = {
    modelId: "m2",
    diskBytes: 1,
    sha256: "manifest-current",
    missingFiles: ["weights.gguf", "mmproj-F16.gguf"],
  };

  it("treats an install without a probe field as ready", () => {
    expect(visionReadySet([complete]).has("m1")).toBe(true);
    expect(projectorMissing(complete)).toBe(false);
  });

  it("excludes a pre-projector install from the ready set", () => {
    const ready = visionReadySet([complete, preProjector]);
    expect(ready.has("qwen3.5-9b-instruct-q4km")).toBe(false);
    expect(ready.has("m1")).toBe(true);
  });

  it("flags only installs whose missing files include the projector", () => {
    expect(projectorMissing(preProjector)).toBe(true);
    expect(projectorMissing(gutted)).toBe(true);
    expect([...projectorMissingSet([complete, preProjector, gutted])]).toEqual([
      "qwen3.5-9b-instruct-q4km",
      "m2",
    ]);
  });

  it("keeps any install with missing files out of the ready set", () => {
    expect(visionReadySet([gutted]).size).toBe(0);
  });

  it("keeps a text-ready install usable while the projector is missing", () => {
    const ready = textReadySet([complete, preProjector, gutted]);
    expect(ready.has("m1")).toBe(true);
    expect(ready.has("qwen3.5-9b-instruct-q4km")).toBe(true);
    expect(ready.has("m2")).toBe(false);
  });

  it("keeps a voice engine usable only when every file is present", () => {
    const ready = completeReadySet([complete, preProjector, gutted]);
    expect(ready.has("m1")).toBe(true);
    expect(ready.has("qwen3.5-9b-instruct-q4km")).toBe(false);
    expect(ready.has("m2")).toBe(false);
  });
});

describe("fitBadge on a GPU-less machine (F3)", () => {
  const noGpu = NO_DEVICES;
  const gpu = inventory([device({ memoryFreeMb: 8000 })]);

  it("never advertises GPU execution when the machine has no GPU (plan absent)", () => {
    const b = fitBadge(model({ badge: "green", plan: null }), noGpu);
    expect(b.detail).toBe(
      "Runs fully on your CPU — light enough for this machine",
    );
    expect(b.detail).not.toContain("GPU");
  });

  it("keeps the GPU wording when a GPU is present or the plan names one", () => {
    expect(fitBadge(model({ badge: "green", plan: null }), gpu).detail).toBe(
      "Runs fully on your GPU",
    );
    expect(
      fitBadge(
        model({ badge: "green", plan: plan({ n_gpu_layers: 40 }) }),
        noGpu,
      ).detail,
    ).toBe("Runs fully on your GPU");
  });

  it("uses CPU wording when the plan itself names the cpu backend", () => {
    const cpuPlan = {
      ...plan({ n_gpu_layers: 0 }),
      device: { backend: "cpu" },
    } as never;
    expect(
      fitBadge(model({ badge: "green", plan: cpuPlan }), gpu).detail,
    ).not.toContain("GPU");
  });

  it("stays neutral when the hardware is unknown and the plan is absent", () => {
    const b = fitBadge(model({ badge: "green", plan: null }));
    expect(b.detail).toBe("Runs fully on this machine");
    expect(runsOnCpu(null, undefined)).toBeNull();
  });
});

describe("sizes that the catalog left out", () => {
  it("formatBytes never prints NaN: an unknown size reads as a dash", () => {
    expect(formatBytes(Number.NaN)).toBe("—");
    expect(formatBytes(undefined as unknown as number)).toBe("—");
    expect(formatBytes(-1)).toBe("—");
    expect(formatBytes(355_000_000)).toBe("355 MB");
  });

  it("fitReason says nothing rather than half a sentence when the RAM need is unknown", () => {
    const missing = model({
      full_gpu_vram_mb: undefined,
      min_free_ram_mb: undefined as unknown as number,
    });
    expect(fitReason(missing, hw(NO_DEVICES))).toBe("");
    expect(fitReason(missing, null)).toBe("");
    expect(
      fitReason(model({ full_gpu_vram_mb: Number.NaN }), hw(NO_DEVICES)),
    ).not.toContain("NaN");
  });
});
