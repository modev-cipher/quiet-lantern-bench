import { describe, expect, it } from "vitest";

import {
  groupVoices,
  voiceLabel,
  voiceMeta,
  type VoiceProfileSummary,
} from "./voiceModel";

function profile(
  overrides: Partial<VoiceProfileSummary> = {},
): VoiceProfileSummary {
  return {
    id: "preset-kokoro-af-heart",
    name: "Heart · American English (female)",
    engine: "kokoro-82m",
    kind: "preset",
    presetVoiceId: "af_heart",
    ...overrides,
  };
}

describe("voiceMeta", () => {
  it("parses language, flag, gender, and display name", () => {
    expect(voiceMeta("af_heart")).toEqual({
      language: "American English",
      langCode: "en",
      flag: "🇺🇸",
      gender: "female",
      shortName: "Heart",
    });
    expect(voiceMeta("bm_daniel")).toMatchObject({
      language: "British English",
      flag: "🇬🇧",
      gender: "male",
      shortName: "Daniel",
    });
    expect(voiceMeta("zf_xiaobei")).toMatchObject({
      language: "Chinese",
      langCode: "zh",
      shortName: "Xiaobei",
    });
    expect(voiceMeta("jf_gongitsune")).toMatchObject({
      language: "Japanese",
      shortName: "Gongitsune",
    });
  });

  it("returns null for cloned or malformed ids", () => {
    expect(voiceMeta(null)).toBeNull();
    expect(voiceMeta(undefined)).toBeNull();
    expect(voiceMeta("")).toBeNull();
    expect(voiceMeta("heart")).toBeNull();
    expect(voiceMeta("xf_heart")).toBeNull();
    expect(voiceMeta("ax_heart")).toBeNull();
    expect(voiceMeta("af_")).toBeNull();
  });
});

describe("voiceLabel", () => {
  it("builds a labeled voice line", () => {
    expect(voiceLabel(profile())).toBe(
      "🇺🇸 Heart · American English (female)",
    );
  });

  it("falls back to the stored name for non-preset profiles", () => {
    expect(
      voiceLabel(
        profile({ name: "My cloned voice", presetVoiceId: null, kind: "cloned" }),
      ),
    ).toBe("My cloned voice");
  });
});

describe("groupVoices", () => {
  it("groups by language alphabetically with unparsed voices last", () => {
    const groups = groupVoices([
      profile({ id: "p1", presetVoiceId: "zf_xiaobei" }),
      profile({ id: "p2", presetVoiceId: "af_heart" }),
      profile({ id: "p3", presetVoiceId: "af_bella" }),
      profile({ id: "p4", presetVoiceId: null, name: "Cloned" }),
      profile({ id: "p5", presetVoiceId: "jm_kumo" }),
    ]);
    expect(groups.map((g) => g.language)).toEqual([
      "American English",
      "Chinese",
      "Japanese",
      "Other voices",
    ]);
    expect(groups[0]!.voices.map((v) => v.id)).toEqual(["p3", "p2"]);
    expect(groups[3]!.voices[0]!.id).toBe("p4");
  });

  it("returns empty for no profiles", () => {
    expect(groupVoices([])).toEqual([]);
  });
});
