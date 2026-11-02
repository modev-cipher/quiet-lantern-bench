import { describe, expect, it } from "vitest";

import { buildSetupChecklist } from "./setupChecklist";
import type { CatalogModelEntry } from "./modelCatalogModel";

function model(overrides: Partial<CatalogModelEntry> = {}): CatalogModelEntry {
  return {
    id: "m1",
    task: "tts",
    display_name: "Model",
    family: "test",
    quant: null,
    format: "gguf",
    disk_bytes: 1_000_000,
    min_free_ram_mb: 1024,
    hybrid_capable: false,
    quality_score: 50,
    speed_score: 50,
    license: { spdx: "MIT", commercial_ok: true },
    badge: "green",
    manifest_sha256: "manifest-current",
    ...overrides,
  };
}

const catalog = [
  model({ id: "kokoro", task: "tts" }),
  model({ id: "qwen", task: "llm" }),
];

describe("buildSetupChecklist", () => {
  it("derives capability items from the shared setupStatus source", () => {
    const { items } = buildSetupChecklist({
      models: catalog,
      installed: new Set(["kokoro"]),
      bookCount: 0,
      jobCount: 0,
    });
    const byId = Object.fromEntries(items.map((item) => [item.id, item]));
    expect(byId.voice.done).toBe(true);
    expect(byId.voice.optional).toBe(false);
    expect(byId.voice.target).toBe("models");
    expect(byId.llm.done).toBe(false);
    expect(byId.llm.optional).toBe(true);
    expect(Object.keys(byId).sort()).toEqual(["cast", "import", "llm", "voice"]);
  });

  it("adds import and cast items from library and job data", () => {
    const { items } = buildSetupChecklist({
      models: catalog,
      installed: new Set(),
      bookCount: 2,
      jobCount: 0,
    });
    const byId = Object.fromEntries(items.map((item) => [item.id, item]));
    expect(byId.import.done).toBe(true);
    expect(byId.import.target).toBe("library");
    expect(byId.cast.done).toBe(false);
    expect(byId.cast.target).toBe("jobs");
  });

  it("counts only required unfinished items as remaining", () => {
    const result = buildSetupChecklist({
      models: catalog,
      installed: new Set(["kokoro"]),
      bookCount: 1,
      jobCount: 0,
    });
    expect(result.remainingRequired).toBe(1);
    expect(result.allRequiredDone).toBe(false);
  });

  it("reports complete when every required item is done, ignoring optionals", () => {
    const result = buildSetupChecklist({
      models: catalog,
      installed: new Set(["kokoro"]),
      bookCount: 1,
      jobCount: 3,
    });
    expect(result.remainingRequired).toBe(0);
    expect(result.allRequiredDone).toBe(true);
  });

  it("omits capability items the catalog cannot provide", () => {
    const { items } = buildSetupChecklist({
      models: [model({ id: "kokoro", task: "tts" })],
      installed: new Set(),
      bookCount: 0,
      jobCount: 0,
    });
    const ids = items.map((item) => item.id);
    expect(ids).toContain("voice");
    expect(ids).not.toContain("llm");
  });

  it("asks a reader engine for no local models", () => {
    const list = buildSetupChecklist({
      models: catalog,
      installed: new Set(),
      bookCount: 0,
      jobCount: 0,
      readerEngine: true,
    });
    expect(list.items.map((item) => item.id)).toEqual(["import", "cast"]);
  });
});
