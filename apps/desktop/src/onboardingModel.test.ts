import { NO_DEVICES, device, inventory } from "./deviceFixtures";
import { describe, expect, it } from "vitest";

import type { CatalogModelEntry } from "./modelCatalogModel";
import {
  combinedDiskBlock,
  combinedDiskBytes,
  defaultChoices,
  diskLine,
  emptySelection,
  gpuLine,
  pickBookPath,
  pickLlm,
  queuedModels,
  ramCpuLine,
  reasonLine,
  recommendModel,
  selectedModelIds,
  taskModels,
  toggleTts,
  WIZARD_STEPS,
  type WizardHardware,
} from "./onboardingModel";

function model(overrides: Partial<CatalogModelEntry> = {}): CatalogModelEntry {
  return {
    id: "kokoro-82m",
    task: "tts",
    display_name: "Kokoro 82M",
    family: "kokoro",
    quant: null,
    format: "pytorch",
    disk_bytes: 355_000_000,
    full_gpu_vram_mb: 1200,
    min_free_ram_mb: 2048,
    hybrid_capable: false,
    quality_score: 78,
    speed_score: 95,
    license: { spdx: "Apache-2.0", commercial_ok: true },
    badge: "green",
    manifest_sha256: "manifest-current",
    ...overrides,
  };
}

function hardware(overrides: Partial<WizardHardware> = {}): WizardHardware {
  return {
    ...inventory([
      device({
        vendor: "NVIDIA",
        name: "RTX 3060",
        memoryTotalMb: 12_288,
        memoryFreeMb: 12_000,
      }),
    ]),
    ram: { totalMb: 32_768, freeMb: 16_384 },
    cpu: { coresPhysical: 8, coresLogical: 16 },
    disk: { modelsFreeGb: 100 },
    ...overrides,
  };
}

describe("recommendModel", () => {
  it("picks the green model with the highest quality score", () => {
    const rec = recommendModel(
      [
        model({ id: "a", quality_score: 70, badge: "green" }),
        model({ id: "b", quality_score: 90, badge: "green" }),
        model({ id: "c", quality_score: 95, badge: "yellow" }),
      ],
      "tts",
    );
    expect(rec?.model.id).toBe("b");
    expect(rec?.fallback).toBe(false);
  });

  it("breaks quality ties by speed score", () => {
    const rec = recommendModel(
      [
        model({ id: "slow", quality_score: 80, speed_score: 40 }),
        model({ id: "fast", quality_score: 80, speed_score: 90 }),
      ],
      "tts",
    );
    expect(rec?.model.id).toBe("fast");
  });

  it("falls back to the best yellow when nothing is green", () => {
    const rec = recommendModel(
      [
        model({ id: "a", quality_score: 95, badge: "red" }),
        model({ id: "b", quality_score: 80, badge: "yellow" }),
        model({ id: "c", quality_score: 60, badge: "yellow" }),
      ],
      "tts",
    );
    expect(rec?.model.id).toBe("b");
    expect(rec?.fallback).toBe(true);
  });

  it("still recommends something when everything is red", () => {
    const rec = recommendModel(
      [model({ id: "a", quality_score: 95, badge: "red" })],
      "tts",
    );
    expect(rec?.model.id).toBe("a");
    expect(rec?.fallback).toBe(true);
  });

  it("ignores deprecated models and other tasks", () => {
    const rec = recommendModel(
      [
        model({ id: "old", quality_score: 99, deprecated: true }),
        model({ id: "llm", task: "llm", quality_score: 99 }),
        model({ id: "live", quality_score: 50 }),
      ],
      "tts",
    );
    expect(rec?.model.id).toBe("live");
  });

  it("returns null when the task has no models", () => {
    expect(recommendModel([], "tts")).toBeNull();
  });

  it("never recommends non-commercial models", () => {
    const rec = recommendModel(
      [
        model({
          id: "studio-tts",
          quality_score: 99,
          license: { spdx: "LicenseRef-NonCommercial", commercial_ok: false },
        }),
        model({ id: "kokoro-82m", quality_score: 70 }),
      ],
      "tts",
    );
    expect(rec?.model.id).toBe("kokoro-82m");
  });
});

describe("reasonLine", () => {
  it("shows the VRAM comparison for GPU machines", () => {
    expect(model().full_gpu_vram_mb).toBe(1200);
    expect(reasonLine(model(), hardware(), false)).toBe(
      "needs ~1.2 GB VRAM — you have 12 GB ✓",
    );
  });

  it("shows the RAM comparison when there is no GPU", () => {
    expect(reasonLine(model(), hardware(NO_DEVICES), false)).toBe(
      "needs ~2 GB RAM on CPU — you have 32 GB ✓",
    );
  });

  it("uses the plain-speak copy for fallback recommendations", () => {
    expect(reasonLine(model(), hardware(), true)).toContain(
      "no model that runs fast",
    );
  });
});

describe("taskModels", () => {
  it("sorts by quality then speed within a tier, and drops deprecated", () => {
    const models = taskModels(
      [
        model({ id: "a", quality_score: 60 }),
        model({ id: "b", quality_score: 90 }),
        model({ id: "dead", quality_score: 99, deprecated: true }),
      ],
      "tts",
    );
    expect(models.map((m) => m.id)).toEqual(["b", "a"]);
  });
});

describe("selection", () => {
  it("toggles TTS multi-select on and off", () => {
    let selection = emptySelection();
    selection = toggleTts(selection, "kokoro-82m");
    selection = toggleTts(selection, "chatterbox");
    expect(selection.tts).toEqual(["kokoro-82m", "chatterbox"]);
    selection = toggleTts(selection, "kokoro-82m");
    expect(selection.tts).toEqual(["chatterbox"]);
  });

  it("collects all selected model ids", () => {
    const selection = {
      tts: ["kokoro-82m"],
      llm: "phi4-mini",
    };
    expect(selectedModelIds(selection)).toEqual(["kokoro-82m", "phi4-mini"]);
  });

  it("queues only selected, not-yet-installed models", () => {
    const models = [
      model({ id: "kokoro-82m" }),
      model({ id: "phi4-mini", task: "llm" }),
      model({ id: "other" }),
    ];
    const queued = queuedModels(
      models,
      { tts: ["kokoro-82m"], llm: "phi4-mini" },
      new Set(["kokoro-82m"]),
    );
    expect(queued.map((m) => m.id)).toEqual(["phi4-mini"]);
  });
});

describe("download queue", () => {
  it("sums combined disk bytes", () => {
    expect(
      combinedDiskBytes([
        model({ disk_bytes: 1_000_000_000 }),
        model({ disk_bytes: 500_000_000 }),
      ]),
    ).toBe(1_500_000_000);
  });

  it("blocks with exact numbers when disk is too small", () => {
    const message = combinedDiskBlock(
      [model({ disk_bytes: 6_600_000_000 })],
      hardware({ disk: { modelsFreeGb: 4.1 } }),
    );
    expect(message).toBe(
      "Not enough disk space — needs 6.6 GB, you have 4.1 GB free",
    );
  });

  it("does not block when disk is sufficient", () => {
    expect(combinedDiskBlock([model()], hardware())).toBeNull();
  });
});

describe("defaultChoices", () => {
  it("sets the best selected TTS plus the llm choice", () => {
    const models = [
      model({ id: "low", quality_score: 60 }),
      model({ id: "high", quality_score: 90 }),
      model({ id: "brain", task: "llm" }),
    ];
    const choices = defaultChoices(models, {
      tts: ["low", "high"],
      llm: "brain",
    });
    expect(choices).toEqual([
      { task: "tts", modelId: "high" },
      { task: "llm", modelId: "brain" },
    ]);
  });

  it("returns nothing for an all-skip selection", () => {
    expect(defaultChoices([model()], emptySelection())).toEqual([]);
  });
});

describe("pickLlm", () => {
  it("records the picked llm and nothing else", () => {
    expect(
      pickLlm({ tts: ["kokoro-82m"], llm: null }, "gemma4-e4b-q4km"),
    ).toEqual({
      tts: ["kokoro-82m"],
      llm: "gemma4-e4b-q4km",
    });
    expect(pickLlm({ tts: [], llm: "gemma4-e4b-q4km" }, null)).toEqual({
      tts: [],
      llm: null,
    });
  });
});

describe("scan card lines", () => {
  it("formats the GPU line", () => {
    expect(gpuLine(hardware())).toBe("RTX 3060 · 12 GB VRAM");
  });

  it("names the card's capacity, not the memory free at this instant", () => {
    expect(gpuLine(hardware())).toContain("12 GB VRAM");
  });

  it("says how many other devices the machine has", () => {
    const hw = {
      ...hardware(),
      ...inventory([
        device({ name: "RTX 3060", memoryTotalMb: 12_288 }),
        device({ key: "pci:0000:00:02.0", name: "Iris Xe", discrete: false }),
      ]),
    };
    expect(gpuLine(hw)).toBe("RTX 3060 · 12 GB VRAM (+1 more)");
  });

  it("says shared memory is unreportable on a unified device", () => {
    const hw = {
      ...hardware(),
      ...inventory([
        device({
          name: "Radeon 780M",
          memoryTotalMb: null,
          memoryFreeMb: null,
          unified: true,
          discrete: false,
        }),
      ]),
    };
    expect(gpuLine(hw)).toBe(
      "Radeon 780M · shared memory not reportable on this driver",
    );
  });

  it("says VRAM is unreportable rather than inventing a number", () => {
    const hw = {
      ...hardware(),
      ...inventory([
        device({ name: "RX 6700", memoryTotalMb: null, memoryFreeMb: null }),
      ]),
    };
    expect(gpuLine(hw)).toBe("RX 6700 · VRAM not reportable on this driver");
  });

  it("calls a unified device's pool shared memory, not VRAM", () => {
    const hw = {
      ...hardware(),
      ...inventory([
        device({
          name: "Apple M3 Pro",
          memoryTotalMb: 18_432,
          unified: true,
          discrete: false,
        }),
      ]),
    };
    expect(gpuLine(hw)).toBe("Apple M3 Pro · 18 GB shared memory");
  });

  it("says CPU-only when there is no GPU", () => {
    expect(gpuLine(hardware(NO_DEVICES))).toBe(
      "No GPU detected — everything runs on CPU",
    );
  });

  it("formats RAM, CPU, and disk lines", () => {
    expect(ramCpuLine(hardware())).toBe("32 GB · CPU 8 cores");
    expect(diskLine(hardware())).toBe("100 GB free for models");
  });
});

describe("the wizard leads with what this machine can actually run", () => {
  it("orders by fit before quality, so the advice is not fifth in the list", () => {
    const heavy = model({ id: "heavy-tts", badge: "red", quality_score: 95 });
    const fits = model({ id: "kokoro-82m", badge: "green", quality_score: 60 });
    const slow = model({
      id: "studio-tts",
      badge: "orange",
      quality_score: 80,
    });

    const ordered = taskModels([heavy, slow, fits], "tts").map((m) => m.id);

    expect(
      ordered,
      "a list that opens with 'likely to fail' reads as a warning, not a choice",
    ).toEqual(["kokoro-82m", "studio-tts", "heavy-tts"]);
  });

  it("still lists every model, because nothing is ever hidden or disabled", () => {
    const heavy = model({ id: "heavy-tts", badge: "red", quality_score: 95 });
    const fits = model({ id: "kokoro-82m", badge: "green", quality_score: 60 });

    expect(taskModels([heavy, fits], "tts")).toHaveLength(2);
  });
});

describe("pickBookPath", () => {
  it("takes the first PDF or EPUB from what was dropped, whatever the case", () => {
    expect(pickBookPath(["/a/notes.txt", "/a/Book.PDF", "/a/b.epub"])).toBe(
      "/a/Book.PDF",
    );
    const windowsPath = String.raw`C:\Books\novel.epub`;
    expect(pickBookPath([windowsPath])).toBe(windowsPath);
  });

  it("says nothing usable was dropped rather than guessing", () => {
    expect(pickBookPath([])).toBeNull();
    expect(pickBookPath(["/a/cover.png", "/a/pdf"])).toBeNull();
  });
});

describe("the three onboarding steps", () => {
  it("walks welcome, voice, then the first book", () => {
    expect([...WIZARD_STEPS]).toEqual(["welcome", "voice", "book"]);
  });
});
