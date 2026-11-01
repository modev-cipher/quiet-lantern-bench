import { describe, expect, it } from "vitest";

import {
  modelDisplayName,
  modelOutcome,
  modelTier,
  type NameableModel,
} from "./modelNames";

// #31: a catalog id such as `teaching-model-9b` or a vendor display name must
// never be what the user reads. Names are role + tier; phrases are outcomes.
const CATALOG: NameableModel[] = [
  { id: "teaching-model-9b", task: "llm", quality_score: 70 },
  { id: "teaching-model-27b", task: "llm", quality_score: 90 },
  { id: "teaching-model-2b", task: "llm", quality_score: 40 },
  {
    id: "teaching-model-old",
    task: "llm",
    quality_score: 99,
    deprecated: true,
  },
  { id: "figure-reader-4b", task: "layout", quality_score: 80 },
  { id: "studio-voice", task: "tts", quality_score: 60 },
];

const RAW = /teaching-model|figure-reader|studio-voice|\b\d+b\b/i;

describe("model display names (#31)", () => {
  it("never surface the catalog id or a parameter-count token", () => {
    for (const m of CATALOG) {
      expect(modelDisplayName(m, CATALOG)).not.toMatch(RAW);
      expect(modelOutcome(m)).not.toMatch(RAW);
    }
  });

  it("rank live peers of a role into Max / Pro+ / Pro; deprecated entries do not take a tier", () => {
    expect(modelDisplayName(CATALOG[1], CATALOG)).toBe("Teaching · Max");
    expect(modelDisplayName(CATALOG[0], CATALOG)).toBe("Teaching · Pro+");
    expect(modelDisplayName(CATALOG[2], CATALOG)).toBe("Teaching · Pro");
    expect(modelTier(CATALOG[3], CATALOG)).toBeNull();
  });

  it("a role with a single live entry is named by role alone", () => {
    expect(modelDisplayName(CATALOG[4], CATALOG)).toBe("Page layout");
    expect(modelDisplayName(CATALOG[5], CATALOG)).toBe("Narration voice");
    expect(modelDisplayName(CATALOG[5])).toBe("Narration voice");
  });

  it("supports roleOverride and deduplicates identical models in peer ranking", () => {
    // When an LLM model is used for figures, roleOverride names it Figure reading
    expect(modelDisplayName(CATALOG[0], [CATALOG[0]], "figures")).toBe("Figure reading");
    // When the same model is in the peer list multiple times, it does not rank against itself
    expect(modelDisplayName(CATALOG[0], [CATALOG[0], CATALOG[0]])).toBe("Teaching");
  });

  it("outcome phrases are outcome-first and carry no mechanism word", () => {
    expect(modelOutcome({ task: "llm" })).toBe(
      "Explains the book in its own words",
    );
    expect(modelDisplayName({ id: "x", task: "llm" }, undefined, "figures")).toMatch(
      /^Figure reading/,
    );
    for (const task of ["llm", "tts", "asr", "layout", "zzz"])
      expect(modelOutcome({ task })).not.toMatch(/\bmodel\b|\bLLM\b|\bVLM\b/i);
  });
});
