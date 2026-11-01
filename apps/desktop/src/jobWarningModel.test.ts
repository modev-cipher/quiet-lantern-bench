import { NO_DEVICES, device, inventory } from "./deviceFixtures";
import { describe, expect, it } from "vitest";

import { i18nFor } from "./i18n";
import { stripIsolates } from "./i18n/format";

import type { CatalogModelEntry } from "./modelCatalogModel";
import { readSource } from "./tests/readSource";
import {
  activeJobModels,
  etaWarningLine,
  jobModelsInUse,
  redInlineCopy,
  resolveJobModels,
  resolveTaskModel,
  usableInstalledModels,
  visionBadgeModel,
  worstBadge,
  worstModel,
  redConfirmCopy,
  confirmToken,
  primaryAction,
  startGate,
} from "./jobWarningModel";

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

describe("resolveJobModels", () => {
  const catalog = [
    model(),
    model({ id: "lite-en", task: "tts", quality_score: 60 }),
    model({ id: "qwen-9b", task: "llm", quality_score: 80, badge: "yellow" }),
    model({ id: "qwen-4b", task: "llm", quality_score: 60 }),
  ];

  it("uses the default model per task when set", () => {
    const resolved = resolveJobModels(
      catalog,
      { tts: "lite-en", llm: "qwen-4b" },
      new Set(["kokoro-82m", "lite-en", "qwen-9b", "qwen-4b"]),
    );
    expect(resolved.tts?.id).toBe("lite-en");
    expect(resolved.llm?.id).toBe("qwen-4b");
  });

  it("falls back to the best installed model when no default is set", () => {
    const resolved = resolveJobModels(
      catalog,
      {},
      new Set(["lite-en", "qwen-4b"]),
    );
    expect(resolved.tts?.id).toBe("lite-en");
    expect(resolved.llm?.id).toBe("qwen-4b");
  });

  it("resolves null when nothing is installed for a task", () => {
    const resolved = resolveJobModels(catalog, {}, new Set());
    expect(resolved.tts).toBeNull();
    expect(resolved.llm).toBeNull();
  });

  it("ignores a default id missing from the catalog", () => {
    const resolved = resolveJobModels(
      catalog,
      { tts: "removed-model" },
      new Set(["kokoro-82m"]),
    );
    expect(resolved.tts?.id).toBe("kokoro-82m");
  });

  it("ignores a default that is not installed", () => {
    const resolved = resolveJobModels(
      catalog,
      { llm: "qwen-9b" },
      new Set(["qwen-4b"]),
    );
    expect(resolved.llm?.id).toBe("qwen-4b");
  });

  it("ignores a default pointing at another task's model", () => {
    const resolved = resolveJobModels(
      catalog,
      { llm: "kokoro-82m" },
      new Set(["kokoro-82m", "qwen-4b"]),
    );
    expect(resolved.llm?.id).toBe("qwen-4b");
  });

  it("ignores a deprecated default", () => {
    const withDeprecated = [
      ...catalog,
      model({ id: "old-llm", task: "llm", deprecated: true }),
    ];
    const resolved = resolveJobModels(
      withDeprecated,
      { llm: "old-llm" },
      new Set(["old-llm", "qwen-4b"]),
    );
    expect(resolved.llm?.id).toBe("qwen-4b");
  });

  it("prefers a fast-tier model over a higher-quality heavier one", () => {
    const resolved = resolveJobModels(
      catalog,
      {},
      new Set(["qwen-9b", "qwen-4b"]),
    );
    expect(resolved.llm?.id).toBe("qwen-4b");
  });

  it("never resolves deprecated models via fallback", () => {
    const withDeprecated = [
      ...catalog,
      model({
        id: "old-tts",
        task: "tts",
        quality_score: 99,
        deprecated: true,
      }),
    ];
    const resolved = resolveJobModels(
      withDeprecated,
      {},
      new Set(["old-tts", "kokoro-82m"]),
    );
    expect(resolved.tts?.id).toBe("kokoro-82m");
  });
});

describe("jobModelsInUse", () => {
  it("includes tts and llm when both resolve", () => {
    const tts = model();
    const llm = model({ id: "qwen-9b", task: "llm" });
    expect(jobModelsInUse({ tts, llm })).toEqual([tts, llm]);
  });

  it("includes only tts when no llm is installed", () => {
    const tts = model();
    expect(jobModelsInUse({ tts, llm: null })).toEqual([tts]);
  });

  it("is empty when nothing resolves", () => {
    expect(jobModelsInUse({ tts: null, llm: null })).toEqual([]);
  });
});

describe("activeJobModels", () => {
  const tts = model({ id: "kokoro-82m", task: "tts" });
  const llm = model({ id: "qwen-9b", task: "llm" });

  it("excludes llm in narrator mode regardless of scriptIsCloud", () => {
    expect(
      activeJobModels({
        models: [tts, llm],
        mode: "narrator",
        ttsIsCloud: false,
        scriptIsCloud: false,
      }),
    ).toEqual([tts]);
    expect(
      activeJobModels({
        models: [tts, llm],
        mode: "narrator",
        ttsIsCloud: false,
        scriptIsCloud: true,
      }),
    ).toEqual([tts]);
  });

  it("excludes tts when ttsIsCloud is true in narrator mode", () => {
    expect(
      activeJobModels({
        models: [tts, llm],
        mode: "narrator",
        ttsIsCloud: true,
        scriptIsCloud: false,
      }),
    ).toEqual([]);
  });

  it("includes both tts and llm in teacher mode with all local", () => {
    expect(
      activeJobModels({
        models: [tts, llm],
        mode: "teacher",
        ttsIsCloud: false,
        scriptIsCloud: false,
      }),
    ).toEqual([tts, llm]);
  });

  it("excludes llm in teacher mode when scriptIsCloud is true", () => {
    expect(
      activeJobModels({
        models: [tts, llm],
        mode: "teacher",
        ttsIsCloud: false,
        scriptIsCloud: true,
      }),
    ).toEqual([tts]);
  });

  it("excludes tts in teacher mode when ttsIsCloud is true", () => {
    expect(
      activeJobModels({
        models: [tts, llm],
        mode: "teacher",
        ttsIsCloud: true,
        scriptIsCloud: false,
      }),
    ).toEqual([llm]);
  });

  it("returns empty array when all models are cloud", () => {
    expect(
      activeJobModels({
        models: [tts, llm],
        mode: "teacher",
        ttsIsCloud: true,
        scriptIsCloud: true,
      }),
    ).toEqual([]);
  });
});

describe("worstBadge", () => {
  it("returns null for no models", () => {
    expect(worstBadge([])).toBeNull();
  });

  it("orders green < yellow < orange < red", () => {
    expect(worstBadge([model({ badge: "green" })])).toBe("green");
    expect(
      worstBadge([model({ badge: "green" }), model({ badge: "yellow" })]),
    ).toBe("yellow");
    expect(
      worstBadge([model({ badge: "orange" }), model({ badge: "yellow" })]),
    ).toBe("orange");
    expect(
      worstBadge([model({ badge: "red" }), model({ badge: "orange" })]),
    ).toBe("red");
  });
});

describe("worstModel", () => {
  it("returns the model carrying the worst badge", () => {
    const red = model({ id: "big-llm", badge: "red" });
    expect(worstModel([model({ badge: "yellow" }), red])?.id).toBe("big-llm");
  });

  it("returns null for no models", () => {
    expect(worstModel([])).toBeNull();
  });
});

describe("copy (ONBOARDING_UX §2 through the catalog)", () => {
  const culprit = model({ id: "qwen-9b", task: "llm", badge: "red" });

  it("formats the pre-start ETA line (formatEta already carries the ~)", () => {
    expect(stripIsolates(etaWarningLine("~2 h 40 min"))).toBe(
      "Estimated time for your selection: ~2 h 40 min.",
    );
  });

  it("red inline copy names the culprit by role and reports ram and vram as separate pools", () => {
    const copy = redInlineCopy(culprit, {
      ...inventory([device({ memoryFreeMb: 4_096 })]),
      ram: { freeMb: 8_192 },
    });
    expect(stripIsolates(copy)).toBe(
      "Teaching: no safe memory allocation fits (8.0 GB RAM + 4.0 GB VRAM available separately) — likely to fail",
    );
    // #31: never the vendor id or display_name.
    expect(copy).not.toContain("qwen");
    expect(copy).not.toContain("Kokoro");
  });

  it("red inline copy counts ram only when there is no gpu", () => {
    const copy = redInlineCopy(culprit, {
      ...NO_DEVICES,
      ram: { freeMb: 8_192 },
    });
    expect(stripIsolates(copy)).toBe(
      "Teaching: no safe memory allocation fits (8.0 GB RAM available) — likely to fail",
    );
  });

  it("red inline copy supports roleOverride for vision culprits", () => {
    const copy = redInlineCopy(
      culprit,
      {
        ...inventory([device({ memoryFreeMb: 4_096 })]),
        ram: { freeMb: 8_192 },
      },
      [culprit],
      "figures",
    );
    expect(stripIsolates(copy)).toBe(
      "Figure reading: no safe memory allocation fits (8.0 GB RAM + 4.0 GB VRAM available separately) — likely to fail",
    );
  });

  it("red inline copy degrades without hardware numbers and without a culprit", () => {
    expect(redInlineCopy(culprit, null)).toBe(
      "Teaching: no safe memory allocation fits — likely to fail",
    );
    expect(redInlineCopy(null, null)).toBe(
      "No safe memory allocation fits — likely to fail",
    );
  });

  it("confirm dialog copy matches the canonical spec string", () => {
    expect(redConfirmCopy().body).toBe(
      "Run anyway? This selection is expected to exceed available memory. " +
        "That rarely fails cleanly — the whole computer can slow down and stop " +
        "responding, even to Cancel. txtCaster checkpoints constantly, so if " +
        "you have to force a restart, the job resumes where it stopped.",
    );
    expect(redConfirmCopy().run).toBe("Run anyway");
    expect(redConfirmCopy().choose).toBe("Choose another");
  });

  it("confirm dialog copy states the swap consequence, not only the recovery promise", () => {
    expect(redConfirmCopy().body).toContain(
      "expected to exceed available memory",
    );
    expect(redConfirmCopy().body).toContain("stop responding");
  });

  it("the Arabic confirm body is Arabic, keeps the brand, and has no quota wording", () => {
    const ar = i18nFor("ar").t("fit.red.confirm.body");
    expect(ar).toMatch(/\p{Script=Arabic}/u);
    expect(ar).toContain("txtCaster");
    for (const key of [
      "fit.red.confirm.body",
      "fit.orange.detail",
      "job.queuedOvernight",
    ] as const) {
      for (const locale of ["en", "ar"] as const) {
        expect(i18nFor(locale).t(key)).not.toMatch(/quota|limit|حصة|حد أقصى/i);
      }
    }
  });
});

describe("visionBadgeModel", () => {
  const catalog = [
    model({
      id: "gemma-mllm",
      task: "llm",
      badge: "green",
      vision_badge: "red",
    }),
    model({ id: "text-only", task: "llm", badge: "green" }),
  ];

  it("substitutes the vision badge for the selected figure model", () => {
    const vision = visionBadgeModel(catalog, "gemma-mllm");
    expect(vision?.badge).toBe("red");
    expect(worstBadge([model(), vision!])).toBe("red");
  });

  it("falls back to the text badge when no vision badge is published", () => {
    expect(visionBadgeModel(catalog, "text-only")?.badge).toBe("green");
  });

  it("returns null when skipped or unknown", () => {
    expect(visionBadgeModel(catalog, null)).toBeNull();
    expect(visionBadgeModel(catalog, "missing")).toBeNull();
  });
});

describe("primaryAction (U5: 🟠 pre-selects overnight)", () => {
  it("is overnight only for an orange fit", () => {
    expect(primaryAction("orange")).toBe("overnight");
    for (const badge of ["green", "yellow", "red", null] as const) {
      expect(primaryAction(badge)).toBe("cast");
    }
  });
});

describe("confirmToken + startGate (U5: 🔴 is a real gate; F2: the confirm is bound and spent)", () => {
  const culprit = model({ id: "qwen-9b", task: "llm", badge: "red" });
  const voice = model({ id: "kokoro-82m", task: "tts", badge: "green" });
  const set = [voice, culprit];
  const base = { ready: true, models: set, culprit, token: null };

  it("mints a token only for a red fit, bound to the model set and its badges", () => {
    expect(confirmToken("red", set)).toBe("red:kokoro-82m=green,qwen-9b=red");
    expect(confirmToken("red", [culprit, voice])).toBe(
      confirmToken("red", set),
    );
    for (const badge of ["green", "yellow", "orange", null] as const) {
      expect(confirmToken(badge, set)).toBeNull();
    }
    // Another set, or the same set in another fit state, is another token.
    expect(confirmToken("red", [culprit])).not.toBe(confirmToken("red", set));
    expect(
      confirmToken("red", [voice, { ...culprit, badge: "orange" }]),
    ).not.toBe(confirmToken("red", set));
  });

  it("is not-ready while the fit is still being computed", () => {
    expect(startGate({ ...base, badge: null, ready: false })).toEqual({
      kind: "not-ready",
    });
    expect(startGate({ ...base, badge: "red", ready: false })).toEqual({
      kind: "not-ready",
    });
  });

  it("lets green, yellow and orange start without a confirm", () => {
    for (const badge of ["green", "yellow", "orange"] as const) {
      expect(startGate({ ...base, badge })).toEqual({ kind: "ok" });
    }
  });

  it("requires a confirm for red and names the culprit", () => {
    expect(startGate({ ...base, badge: "red" })).toEqual({
      kind: "confirm-required",
      culpritId: "qwen-9b",
    });
  });

  it("accepts only the token minted for the current fit", () => {
    const token = confirmToken("red", set);
    expect(startGate({ ...base, badge: "red", token })).toEqual({ kind: "ok" });
    // The set changed (figure model dropped) after the confirm: re-ask.
    expect(
      startGate({ ...base, badge: "red", models: [culprit], token }),
    ).toEqual({ kind: "confirm-required", culpritId: "qwen-9b" });
    // A token from a previous red state of a different composition: re-ask.
    expect(
      startGate({ ...base, badge: "red", token: "red:qwen-9b=red" }),
    ).toEqual({ kind: "confirm-required", culpritId: "qwen-9b" });
  });

  it("still gates red when no culprit could be named", () => {
    expect(startGate({ ...base, badge: "red", culprit: null })).toEqual({
      kind: "confirm-required",
      culpritId: null,
    });
  });

  it("allows 0-model all-cloud jobs to start when ready", () => {
    expect(
      startGate({
        ready: true,
        models: [],
        badge: null,
        culprit: null,
        token: null,
      }),
    ).toEqual({ kind: "ok" });
  });

  it("blocks 0-model jobs when not-ready", () => {
    expect(
      startGate({
        ready: false,
        models: [],
        badge: null,
        culprit: null,
        token: null,
      }),
    ).toEqual({ kind: "not-ready" });
  });
});

describe("overnight copy (approved sentences)", () => {
  const HINT = {
    en: "Queue overnight is pre-selected. We’ll try to keep your computer awake while rendering.",
    ar: "«سجّله ليلًا» مُحدَّد مسبقًا. سنحاول منع السكون أثناء إنشاء التسجيل.",
  } as const;
  const QUEUED = {
    en: "Queued overnight. We’ll try to keep your computer awake while rendering.",
    ar: "في انتظار التسجيل الليلي. سنحاول منع السكون أثناء إنشاء التسجيل.",
  } as const;
  const OVERPROMISE =
    /stays awake|until (the|this) cast is done|overnight batch|يبقى جهازك مستيقظًا|حتى ينتهي/u;
  for (const locale of ["en", "ar"] as const) {
    it(`${locale}: the orange hint says the lock is only attempted while rendering`, () => {
      expect(stripIsolates(i18nFor(locale).t("fit.orange.overnightHint"))).toBe(
        HINT[locale],
      );
    });
    it(`${locale}: the queued notice makes no awake guarantee and names no batch`, () => {
      expect(stripIsolates(i18nFor(locale).t("job.queuedOvernight"))).toBe(
        QUEUED[locale],
      );
    });
    it(`${locale}: neither string overpromises`, () => {
      for (const key of [
        "fit.orange.overnightHint",
        "job.queuedOvernight",
      ] as const) {
        expect(i18nFor(locale).t(key)).not.toMatch(OVERPROMISE);
      }
    });
  }
});

describe("resolveTaskModel", () => {
  const catalog = [
    model({ id: "qwen-pro", task: "llm", quality_score: 70 }),
    model({ id: "qwen-max", task: "llm", quality_score: 90 }),
    model({ id: "qwen-old", task: "llm", quality_score: 95, deprecated: true }),
    model({ id: "align-a", task: "align", quality_score: 50 }),
  ];
  const installed = new Set(["qwen-pro", "qwen-max", "qwen-old", "align-a"]);

  it("picks the higher-quality installed model on a badge tie, not catalog order", () => {
    expect(resolveTaskModel(catalog, "llm", {}, installed)?.id).toBe(
      "qwen-max",
    );
  });

  it("ignores a default that is not installed or is deprecated", () => {
    expect(
      resolveTaskModel(catalog, "llm", { llm: "qwen-gone" }, installed)?.id,
    ).toBe("qwen-max");
    expect(
      resolveTaskModel(catalog, "llm", { llm: "qwen-old" }, installed)?.id,
    ).toBe("qwen-max");
    expect(
      resolveTaskModel(catalog, "llm", { llm: "qwen-pro" }, installed)?.id,
    ).toBe("qwen-pro");
  });

  it("falls back to an installed deprecated model when no live one is installed", () => {
    const onlyOld = new Set(["qwen-old"]);
    expect(resolveTaskModel(catalog, "llm", {}, onlyOld)?.id).toBe("qwen-old");
    expect(
      resolveTaskModel(catalog, "llm", { llm: "qwen-old" }, onlyOld)?.id,
    ).toBe("qwen-old");
  });

  it("covers tasks beyond the cast's tts and llm", () => {
    expect(resolveTaskModel(catalog, "align", {}, installed)?.id).toBe(
      "align-a",
    );
    expect(resolveTaskModel(catalog, "align", {}, new Set())).toBeNull();
  });
});

describe("the Models setup cards name the model a cast would use", () => {
  it("resolve through the cast dialog's rule", () => {
    const panel = readSource(new URL("./ModelsPanel.tsx", import.meta.url));
    expect(panel).toContain("resolveTaskModel(");
    expect(panel).not.toMatch(/inTask\.find\(\(model\) => installed\.has/);
  });
});

describe("usableInstalledModels", () => {
  const catalog = [
    model({ id: "live-a", task: "llm", quality_score: 70 }),
    model({ id: "old-b", task: "llm", quality_score: 95, deprecated: true }),
  ];

  it("lists live installs and hides deprecated ones while a live one exists", () => {
    expect(
      usableInstalledModels(catalog, "llm", new Set(["live-a", "old-b"])).map(
        (m) => m.id,
      ),
    ).toEqual(["live-a"]);
  });

  it("keeps a deprecated install usable when it is all the person has", () => {
    expect(
      usableInstalledModels(catalog, "llm", new Set(["old-b"])).map(
        (m) => m.id,
      ),
    ).toEqual(["old-b"]);
  });
});

describe("a deprecated-only teaching model still reaches the red confirmation", () => {
  it("resolves the red deprecated install so its badge gates the start", () => {
    const tts = model({ id: "tts-green", task: "tts", badge: "green" });
    const phi = model({
      id: "phi-old",
      task: "llm",
      badge: "red",
      deprecated: true,
    });
    const resolved = resolveJobModels(
      [tts, phi],
      {},
      new Set(["tts-green", "phi-old"]),
    );
    expect(resolved.llm?.id).toBe("phi-old");
    expect(worstBadge(jobModelsInUse(resolved))).toBe("red");
  });
});
