import { describe, expect, it } from "vitest";

import {
  castFigureModelId,
  figureLine,
  figureModel,
  figureOverride,
  figureState,
} from "./figureModel";
import type { CatalogModelEntry } from "./modelCatalogModel";

function llm(
  id: string,
  quality: number,
  overrides: Partial<CatalogModelEntry> = {},
): CatalogModelEntry {
  return {
    id,
    task: "llm",
    display_name: id,
    quality_score: quality,
    badge: "green",
    capabilities: ["teacher_mode", "figure_explanation"],
    ...overrides,
  } as CatalogModelEntry;
}

const seeing = llm("gemma4-e2b-q4km", 70, { projector_bytes: 672_423_616 });
const better = llm("qwen3.5-9b-instruct-q4km", 92);
const blind = llm("phi4-mini-q4km", 74, {
  capabilities: ["teacher_mode"],
  deprecated: true,
});
const models = [seeing, better, blind];

const base = {
  explainFigures: true,
  models,
  visionReadyIds: new Set([seeing.id, better.id, blind.id]),
  projectorMissingIds: new Set<string>(),
  cloudScript: null,
};

describe("castFigureModelId mirrors the native cast-model resolution", () => {
  it("uses the teacher's chosen model verbatim", () => {
    expect(
      castFigureModelId({
        models,
        installedIds: new Set([seeing.id, better.id]),
        llmOverrideId: seeing.id,
      }),
    ).toBe(seeing.id);
  });

  it("otherwise picks the best live installed llm", () => {
    expect(
      castFigureModelId({
        models,
        installedIds: new Set([seeing.id, better.id, blind.id]),
        llmOverrideId: null,
      }),
    ).toBe(better.id);
  });

  it("falls back to a deprecated install only when it is the only one", () => {
    expect(
      castFigureModelId({
        models,
        installedIds: new Set([blind.id]),
        llmOverrideId: null,
      }),
    ).toBe(blind.id);
    expect(
      castFigureModelId({
        models,
        installedIds: new Set(),
        llmOverrideId: null,
      }),
    ).toBeNull();
  });
});

describe("figureState", () => {
  it("is off when the person turns figure explanation off", () => {
    expect(
      figureState({ ...base, explainFigures: false, castModelId: seeing.id }),
    ).toEqual({ kind: "off" });
  });

  it("the cast's own model explains figures when it can read pictures", () => {
    const state = figureState({ ...base, castModelId: seeing.id });
    expect(state).toEqual({ kind: "local", model: seeing });
    expect(figureModel(state)).toBe(seeing);
  });

  it("a model that cannot read pictures never borrows another", () => {
    expect(figureState({ ...base, castModelId: blind.id })).toEqual({
      kind: "unavailable",
    });
  });

  it("a missing image reader is named, not hidden", () => {
    const state = figureState({
      ...base,
      castModelId: seeing.id,
      projectorMissingIds: new Set([seeing.id]),
    });
    expect(state).toEqual({ kind: "projectorMissing", model: seeing });
    expect(figureLine(state, models)).toContain("672 MB");
  });

  it("a cloud script model explains figures only when its provider reads images", () => {
    const cloud = { connectionName: "My Gemini", model: "gemini-3.7-flash" };
    expect(
      figureState({
        ...base,
        castModelId: seeing.id,
        cloudScript: { ...cloud, readsImages: true },
      }),
    ).toEqual({ kind: "cloud", ...cloud });
    expect(
      figureState({
        ...base,
        castModelId: seeing.id,
        cloudScript: { ...cloud, readsImages: false },
      }),
    ).toEqual({ kind: "unavailable" });
  });

  it("an unverified cloud model keeps its identity without claiming image support", () => {
    const cloud = { connectionName: "Custom", model: "unreviewed-model" };
    expect(
      figureState({
        ...base,
        castModelId: null,
        cloudScript: { ...cloud, readsImages: "unknown" },
      }),
    ).toEqual({ kind: "cloudUnknown", ...cloud });
  });
});

describe("figureOverride", () => {
  it("records only an explicit opt-out", () => {
    expect(figureOverride(true)).toEqual({});
    expect(figureOverride(false)).toEqual({ explain_figures: false });
  });
});
