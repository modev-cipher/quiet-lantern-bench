import { describe, expect, it } from "vitest";

import {
  MAX_SECONDS,
  canRecord,
  canSave,
  cloningEngineIds,
  cloningInstalled,
  defaultName,
  discardSample,
  durationLabel,
  failed,
  initialState,
  inputsUnavailable,
  nameError,
  sampleReady,
  sampleSummary,
  sampleWarning,
  selectInput,
  type CloneState,
  type VoiceSample,
} from "./cloneVoiceModel";
import type { VoiceProfileSummary } from "./jobFormModel";
import { t } from "./i18n";
import { stripIsolates } from "./i18n/format";

function profile(name: string): VoiceProfileSummary {
  return { id: name, name, engine: "chatterbox-0.5b", kind: "cloned" };
}

function sample(overrides: Partial<VoiceSample> = {}): VoiceSample {
  return {
    sampleId: "s1",
    seconds: 5,
    hitHardStop: false,
    source: "recording",
    ...overrides,
  };
}

function withSample(overrides: Partial<VoiceSample> = {}): CloneState {
  return sampleReady(initialState(), sample(overrides), "My voice");
}

describe("cloningInstalled", () => {
  it("is false until a cloning engine is actually installed", () => {
    expect(cloningInstalled([], ["chatterbox-0.5b"])).toBe(false);
    expect(cloningInstalled(["kokoro-82m"], ["chatterbox-0.5b"])).toBe(false);
    expect(
      cloningInstalled(["kokoro-82m", "chatterbox-0.5b"], ["chatterbox-0.5b"]),
    ).toBe(true);
  });

  it("accepts any catalog entry that advertises cloning, not just chatterbox", () => {
    expect(
      cloningInstalled(["heavy-tts"], ["chatterbox-0.5b", "heavy-tts"]),
    ).toBe(true);
  });
});

describe("cloningEngineIds", () => {
  const entry = (
    id: string,
    task: string,
    capabilities: string[],
    deprecated = false,
  ) =>
    ({
      id,
      task,
      capabilities,
      deprecated,
    }) as unknown as Parameters<typeof cloningEngineIds>[0][number];

  it("reads the capability from the catalog rather than hard-coding an id", () => {
    expect(
      cloningEngineIds([
        entry("kokoro-82m", "tts", ["multi_voice"]),
        entry("chatterbox-0.5b", "tts", ["voice_cloning"]),
        entry("heavy-tts", "tts", ["voice_cloning", "emotion_tags"]),
      ]),
    ).toEqual(["chatterbox-0.5b", "heavy-tts"]);
  });

  it("ignores non-tts entries and deprecated ones", () => {
    expect(
      cloningEngineIds([
        entry("some-llm", "llm", ["voice_cloning"]),
        entry("retired", "tts", ["voice_cloning"], true),
      ]),
    ).toEqual([]);
  });

  it("tolerates an entry with no capabilities array", () => {
    expect(
      cloningEngineIds([
        { id: "bare", task: "tts" } as unknown as Parameters<
          typeof cloningEngineIds
        >[0][number],
      ]),
    ).toEqual([]);
  });
});

describe("recording gates", () => {
  it("cannot record before a device is known", () => {
    expect(canRecord(initialState())).toBe(false);
  });

  it("can record once an input is offered", () => {
    const state = selectInput(initialState(), [
      { name: "Built-in", isDefault: true },
    ]);
    expect(canRecord(state)).toBe(true);
  });

  it("cannot record while something else is in flight", () => {
    const state = selectInput(initialState(), [
      { name: "Built-in", isDefault: true },
    ]);
    expect(canRecord({ ...state, busy: true })).toBe(false);
  });

  it("prefers the default device and falls back to the first", () => {
    expect(
      selectInput(initialState(), [
        { name: "USB mic", isDefault: false },
        { name: "Built-in", isDefault: true },
      ]).selectedInput,
    ).toBe("Built-in");
    expect(
      selectInput(initialState(), [{ name: "USB mic", isDefault: false }])
        .selectedInput,
    ).toBe("USB mic");
  });

  it("keeps the import route open when there is no microphone", () => {
    const state = inputsUnavailable(initialState(), "no microphone");
    expect(canRecord(state)).toBe(false);
    expect(state.inputsError).toBe("no microphone");
    expect(state.step).toBe("intro");
  });
});

describe("saving gates", () => {
  it("cannot save without a sample", () => {
    expect(canSave({ ...initialState(), name: "Alex" })).toBe(false);
  });

  it("cannot save without a name", () => {
    expect(canSave({ ...withSample(), name: "  " })).toBe(false);
  });

  it("can save once both are present", () => {
    expect(canSave(withSample())).toBe(true);
  });

  it("cannot save twice while the first save is running", () => {
    expect(canSave({ ...withSample(), busy: true })).toBe(false);
  });
});

describe("names", () => {
  it("suggests a free name", () => {
    expect(defaultName([])).toBe("My voice");
    expect(defaultName([profile("My voice")])).toBe("My voice 2");
    expect(defaultName([profile("My voice"), profile("My voice 2")])).toBe(
      "My voice 3",
    );
  });

  it("rejects an empty or duplicate name with copy a person can act on", () => {
    expect(nameError("  ", [])).toContain("Give this voice a name");
    expect(nameError("Alex", [profile("Alex")])).toContain(
      "already have a voice with that name",
    );
    expect(nameError(" Alex ", [profile("Alex")])).not.toBeNull();
    expect(nameError("Alex", [profile("Sam")])).toBeNull();
  });
});

describe("sample feedback", () => {
  it("reports where the sample came from and how long it is", () => {
    expect(stripIsolates(sampleSummary(sample({ seconds: 5.24 })))).toBe(
      "Recorded · 5.2s of speech",
    );
    expect(
      stripIsolates(sampleSummary(sample({ source: "import", seconds: 8 }))),
    ).toBe("Imported · 8.0s of speech");
  });

  it("says nothing when the sample is comfortably inside the window", () => {
    expect(sampleWarning(sample({ seconds: 6 }))).toBeNull();
  });

  it("warns when only the first slice will be used", () => {
    expect(
      stripIsolates(sampleWarning(sample({ seconds: MAX_SECONDS })) ?? ""),
    ).toContain(`first ${MAX_SECONDS} seconds`);
  });

  it("explains a recording that hit the hard stop", () => {
    expect(
      stripIsolates(sampleWarning(sample({ hitHardStop: true })) ?? ""),
    ).toContain("only the first minute was kept");
  });

  it("formats durations to one decimal", () => {
    expect(durationLabel(3)).toBe("3.0s");
    expect(durationLabel(11.96)).toBe("12.0s");
  });
});

describe("state transitions", () => {
  it("moves to review and adopts the suggested name only when empty", () => {
    const fresh = sampleReady(initialState(), sample(), "My voice");
    expect(fresh.step).toBe("review");
    expect(fresh.name).toBe("My voice");
    const named = sampleReady(
      { ...initialState(), name: "Alex" },
      sample(),
      "My voice",
    );
    expect(named.name).toBe("Alex");
  });

  it("clears a stale preview when a new sample arrives", () => {
    const state = { ...withSample(), previewPath: "/tmp/old.wav" };
    expect(
      sampleReady(state, sample({ sampleId: "s2" }), "My voice").previewPath,
    ).toBeNull();
  });

  it("discarding returns to the start with nothing left over", () => {
    const state = discardSample({
      ...withSample(),
      previewPath: "/tmp/p.wav",
      error: "boom",
    });
    expect(state.step).toBe("intro");
    expect(state.sample).toBeNull();
    expect(state.previewPath).toBeNull();
    expect(state.error).toBeNull();
  });

  it("a failure keeps the sample so the user can retry the save", () => {
    const state = failed(withSample(), "engine is not ready");
    expect(state.step).toBe("review");
    expect(state.sample).not.toBeNull();
    expect(state.busy).toBe(false);
    expect(state.error).toBe("engine is not ready");
  });

  it("a failure before any sample returns to the start", () => {
    const state = failed(initialState(), "no microphone");
    expect(state.step).toBe("intro");
    expect(state.sample).toBeNull();
  });
});

describe("disclosure copy", () => {
  it("tells the user their cloned audio is watermarked", () => {
    expect(t("voice.clone.watermarkNote")).toContain("watermark");
  });

  it("asks for consent before cloning someone", () => {
    expect(t("voice.clone.consentNote")).toContain("permission");
  });
});
