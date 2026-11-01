import { describe, expect, it } from "vitest";

import {
  CUSTOM_PRESET_ID,
  OPENAI_COMPAT_PRESETS,
  presetById,
} from "./providerPresets";

// F2 structural pins: presets must never weaken the keyless-loopback-only
// consent law, and every prefilled URL must be a shape the backend's
// canonical-URL policy can accept (https for hosted, loopback for local).

describe("openai-compatible presets", () => {
  it("keyless is prefilled only for loopback base URLs", () => {
    for (const preset of OPENAI_COMPAT_PRESETS) {
      const loopback =
        preset.baseUrl.startsWith("http://localhost") ||
        preset.baseUrl.startsWith("http://127.0.0.1");
      if (preset.keyless) {
        expect(loopback, `${preset.id} prefills keyless off-loopback`).toBe(
          true,
        );
      }
      if (!loopback) {
        expect(
          preset.baseUrl.startsWith("https://"),
          `${preset.id} hosted URL must be https`,
        ).toBe(true);
      }
    }
  });

  it("loopback presets carry the local note (null link); hosted carry a data-handling link", () => {
    for (const preset of OPENAI_COMPAT_PRESETS) {
      if (preset.keyless) {
        expect(preset.dataHandlingUrl).toBeNull();
      } else {
        expect(preset.dataHandlingUrl).toMatch(/^https:\/\//);
      }
    }
  });

  it("ids are unique and never collide with the custom sentinel", () => {
    const ids = OPENAI_COMPAT_PRESETS.map((preset) => preset.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ids).not.toContain(CUSTOM_PRESET_ID);
  });

  it("offers api.openai.com itself as a first-class preset (F6 rider)", () => {
    const openai = presetById("openai");
    expect(openai?.baseUrl).toBe("https://api.openai.com/v1");
    expect(openai?.keyless).toBe(false);
    expect(openai?.dataHandlingUrl).toMatch(/^https:\/\//);
  });

  it("presetById resolves known ids and returns null otherwise", () => {
    expect(presetById("ollama")?.baseUrl).toBe("http://localhost:11434/v1");
    expect(presetById("openrouter")?.baseUrl).toBe(
      "https://openrouter.ai/api/v1",
    );
    expect(presetById(CUSTOM_PRESET_ID)).toBeNull();
    expect(presetById("nope")).toBeNull();
  });

  it("no preset URL ends with a trailing slash (the fetch appends /models)", () => {
    for (const preset of OPENAI_COMPAT_PRESETS) {
      expect(preset.baseUrl.endsWith("/"), preset.id).toBe(false);
    }
  });
});
