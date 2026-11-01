import { describe, expect, it } from "vitest";

import {
  activeCloudChoices,
  stageHasCloudSide,
  type CastKeyPool,
  type CastPoolEntry,
} from "./castKeyPoolModel";

function poolEntry(overrides: Partial<CastPoolEntry> = {}): CastPoolEntry {
  return {
    baseUrl: "https://generativelanguage.googleapis.com",
    provider: "gemini",
    state: "active",
    keyPresent: true,
    ...overrides,
  };
}

function poolWith(overrides: Partial<CastKeyPool> = {}): CastKeyPool {
  return { llm: [], tts: [], ...overrides };
}

describe("cast-stage cloud choices (ONBOARDING_UX §3 per-stage selector pool)", () => {
  it("offers only keys whose validation state is active", () => {
    const pool = poolWith({
      llm: [
        poolEntry(),
        poolEntry({ baseUrl: "https://a", state: "invalid" }),
        poolEntry({ baseUrl: "https://b", state: "out_of_quota" }),
        poolEntry({ baseUrl: "https://c", state: "unchecked" }),
      ],
    });
    expect(activeCloudChoices(pool, "llm").map((entry) => entry.baseUrl)).toEqual([
      "https://generativelanguage.googleapis.com",
    ]);
  });

  it("excludes entries whose key bytes are not loadable right now", () => {
    const pool = poolWith({
      tts: [poolEntry({ provider: "cartesia", keyPresent: false })],
    });
    expect(
      stageHasCloudSide(pool, "tts"),
      "a locked vault leaves metadata visible but no usable key; the selector must not \
       render a Cloud side it cannot start a cast with",
    ).toBe(false);
  });

  it("reports per-stage availability so the selector renders only where both sides exist", () => {
    const pool = poolWith({ llm: [poolEntry()] });
    expect(stageHasCloudSide(pool, "llm")).toBe(true);
    expect(stageHasCloudSide(pool, "tts")).toBe(false);
  });
});
