import { describe, expect, it } from "vitest";

import {
  advisoryCopy,
  languageAdvisory,
  languageName,
  normalizeLang,
  reportAdvisoryNotice,
} from "./languageAdvisoryModel";

const KOKORO = ["en", "ja", "zh"];
const ENGLISH_ONLY = ["en"];

describe("languageAdvisory", () => {
  it("stays silent when the book language is unknown", () => {
    expect(languageAdvisory(null, "af_heart", ENGLISH_ONLY)).toBeNull();
    expect(languageAdvisory(undefined, null, ENGLISH_ONLY)).toBeNull();
    expect(languageAdvisory("  ", null, ENGLISH_ONLY)).toBeNull();
  });

  it("stays silent when the preset voice speaks the book language", () => {
    expect(languageAdvisory("zh", "zf_xiaobei", KOKORO)).toBeNull();
    expect(languageAdvisory("ja", "jf_alpha", KOKORO)).toBeNull();
    expect(languageAdvisory("en", "af_heart", KOKORO)).toBeNull();
    expect(languageAdvisory("en", "bf_alice", KOKORO)).toBeNull();
  });

  it("lets the preset voice language outrank the engine list", () => {
    expect(languageAdvisory("zh", "af_heart", KOKORO)).toEqual({
      book: "zh",
      voiceSpeaks: "en",
    });
  });

  it("falls back to the engine list for voices without a preset language", () => {
    expect(languageAdvisory("zh", null, ENGLISH_ONLY)).toEqual({
      book: "zh",
      voiceSpeaks: null,
    });
    expect(languageAdvisory("ar", null, ENGLISH_ONLY)).toEqual({
      book: "ar",
      voiceSpeaks: null,
    });
    expect(languageAdvisory("he", null, ENGLISH_ONLY)).toEqual({
      book: "he",
      voiceSpeaks: null,
    });
    expect(languageAdvisory("ko", null, ENGLISH_ONLY)).toEqual({
      book: "ko",
      voiceSpeaks: null,
    });
    expect(languageAdvisory("en", null, ENGLISH_ONLY)).toBeNull();
    expect(languageAdvisory("zh", null, ["en", "zh"])).toBeNull();
  });

  it("treats an unparseable preset id like an engine-level voice", () => {
    expect(languageAdvisory("zh", "en_US-lessac-medium", ENGLISH_ONLY)).toEqual(
      { book: "zh", voiceSpeaks: null },
    );
  });

  it("never accuses an engine that declares no languages", () => {
    expect(languageAdvisory("zh", null, [])).toBeNull();
  });

  it("ignores region subtags and case on both sides", () => {
    expect(languageAdvisory("EN-us", null, ENGLISH_ONLY)).toBeNull();
    expect(languageAdvisory("zh_CN", null, ["ZH-Hans"])).toBeNull();
  });
});

describe("normalizeLang", () => {
  it("keeps only the lowercase primary subtag", () => {
    expect(normalizeLang("EN-us")).toBe("en");
    expect(normalizeLang("zh_CN")).toBe("zh");
    expect(normalizeLang("pt")).toBe("pt");
    expect(normalizeLang("")).toBeNull();
    expect(normalizeLang("  ")).toBeNull();
    expect(normalizeLang(null)).toBeNull();
  });
});

describe("advisory copy", () => {
  it("names both languages when the voice itself mismatches", () => {
    const copy = advisoryCopy({ book: "zh", voiceSpeaks: "en" });
    expect(copy).toContain("Chinese");
    expect(copy).toContain("English");
    expect(copy).toContain("cast anyway");
  });

  it("names the voice engine when only the engine list mismatches", () => {
    const copy = advisoryCopy({ book: "ar", voiceSpeaks: null });
    expect(copy).toContain("Arabic");
    expect(copy).toContain("voice engine");
    expect(copy).toContain("cast anyway");
  });

  it("quotes unknown language codes instead of hiding them", () => {
    expect(languageName("xx")).toBe('"xx"');
  });

  it("builds a completion-report notice for both shapes", () => {
    expect(reportAdvisoryNotice("zh", "en")).toContain("Chinese");
    expect(reportAdvisoryNotice("zh", "en")).toContain("English");
    expect(reportAdvisoryNotice("he", null)).toContain("Hebrew");
    expect(reportAdvisoryNotice("he", null)).toContain("voice engine");
  });
});
