import { describe, expect, it } from "vitest";

import { effortLabel, phaseSummaryLines } from "./castPhaseSummary";
import type { CastProviderOptions } from "./castProviderModel";
import type { CatalogModelEntry } from "./modelCatalogModel";

const gemma = {
  id: "gemma-3-4b",
  task: "llm",
  display_name: "Gemma 3 4B",
  quality_score: 5,
  badge: "green",
} as CatalogModelEntry;

const kokoro = {
  id: "kokoro-82m",
  task: "tts",
  display_name: "Kokoro",
  quality_score: 5,
  badge: "green",
} as CatalogModelEntry;

const options: CastProviderOptions = {
  scriptConnections: [
    {
      connectionId: "c-1",
      name: "My Gemini",
      provider: "gemini",
      defaultModel: "gemini-3.7-flash",
      loopback: false,
    },
  ],
  ttsVoices: [
    {
      voiceProfileId: "vp-1",
      name: "Luna",
      voiceId: "luna",
      connectionId: "c-2",
      connectionName: "My Cartesia",
      provider: "cartesia",
      model: "sonic-2",
      loopback: false,
    },
  ],
};

describe("phaseSummaryLines", () => {
  it("renders the ruled zero-knowledge defaults: local models, plain words, the outcome alongside — never the id (#31)", () => {
    const lines = phaseSummaryLines({
      mode: "teacher",
      selection: { script: { kind: "local" }, tts: { kind: "local" } },
      options,
      resolved: { llm: gemma, tts: kokoro },
      figureState: { kind: "local", model: gemma },
      catalogModels: [gemma, kokoro],
      voiceName: "Sarah",
      voiceEngine: "kokoro-82m",
    });
    expect(lines.map((line) => line.phase)).toEqual([
      "script",
      "figures",
      "tts",
    ]);
    expect(lines[0]!.title).toBe("On this computer — Teaching");
    expect(lines[0]!.expert).toBe("Explains the book in its own words");
    expect(lines[1]!.title).toBe("On this computer — Teaching");
    for (const line of lines) {
      expect(`${line.title} ${line.expert ?? ""}`).not.toMatch(/gemma|kokoro|\b\d+b\b/i);
    }
    expect(lines[2]!.title).toBe("Sarah — on this computer");
    expect(lines[2]!.expert).toBe("Narrates in a natural voice");
  });

  it("narrator mode says the text is read as written, never a model claim", () => {
    const lines = phaseSummaryLines({
      mode: "narrator",
      selection: { script: { kind: "local" }, tts: { kind: "local" } },
      options: null,
      resolved: { llm: null, tts: null },
      figureState: { kind: "off" },
      catalogModels: [],
      voiceName: null,
      voiceEngine: null,
    });
    expect(lines[0]!.title).toBe(
      "Reads the text as written — on this computer",
    );
    expect(lines[1]!.title).toBe("Mentioned briefly, not explained");
    expect(lines[1]!.expert).toBeNull();
    expect(lines[2]!.title).toBe("On this computer — pick a voice below");
  });

  it("cloud selections name the connection, the LOW-default effort, and the exact model id", () => {
    const lines = phaseSummaryLines({
      mode: "teacher",
      selection: {
        script: { kind: "cloud", connectionId: "c-1", model: "gemini-3.7-flash" },
        tts: { kind: "cloud", voiceProfileId: "vp-1" },
      },
      options,
      resolved: { llm: gemma, tts: kokoro },
      figureState: { kind: "off" },
      catalogModels: [gemma, kokoro],
      voiceName: "Sarah",
      voiceEngine: "kokoro-82m",
    });
    expect(lines[0]!.title).toBe("My Gemini · Quick (recommended) effort");
    expect(lines[0]!.expert).toBe("gemini-3.7-flash");
    expect(lines[2]!.title).toBe("Luna — via My Cartesia");
    expect(lines[2]!.expert).toBe("sonic-2 · luna");
  });

  it("a raised effort override renders by its honest name", () => {
    expect(effortLabel(undefined)).toBe("Quick (recommended)");
    expect(effortLabel("high")).toBe("Deep");
    const lines = phaseSummaryLines({
      mode: "teacher",
      selection: {
        script: {
          kind: "cloud",
          connectionId: "c-1",
          model: "gemini-3.7-flash",
          effort: "medium",
        },
        tts: { kind: "local" },
      },
      options,
      resolved: { llm: gemma, tts: kokoro },
      figureState: { kind: "off" },
      catalogModels: [],
      voiceName: "Sarah",
      voiceEngine: "kokoro-82m",
    });
    expect(lines[0]!.title).toBe("My Gemini · Balanced effort");
  });

  it("renders Cloud Vision connection and model when cloud figure is chosen", () => {
    const lines = phaseSummaryLines({
      mode: "narrator",
      selection: { script: { kind: "local" }, tts: { kind: "local" } },
      options,
      resolved: { llm: null, tts: kokoro },
      figureState: {
        kind: "cloud",
        model: "gemini-2.5-flash",
        connectionName: "My Gemini",
      },
      catalogModels: [kokoro],
      voiceName: "Sarah",
      voiceEngine: "kokoro-82m",
    });
    expect(lines[1]!.phase).toBe("figures");
    expect(lines[1]!.title).toBe("Cloud Vision · My Gemini (gemini-2.5-flash)");
    expect(lines[1]!.expert).toBeNull();
  });

  it("a cast model that cannot read pictures says figures get a simple note", () => {
    const lines = phaseSummaryLines({
      mode: "teacher",
      selection: { script: { kind: "local" }, tts: { kind: "local" } },
      options,
      resolved: { llm: gemma, tts: kokoro },
      figureState: { kind: "unavailable" },
      catalogModels: [gemma, kokoro],
      voiceName: "Sarah",
      voiceEngine: "kokoro-82m",
    });
    expect(lines[1]!.title).toBe(
      "The chosen model can’t read pictures, so figures get a simple note",
    );
    expect(lines[1]!.expert).toBeNull();
  });
});
