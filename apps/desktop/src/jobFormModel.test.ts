import { describe, expect, it } from "vitest";

import {
  buildEtaRequest,
  chapterMeta,
  chapterWords,
  formatEta,
  missingModels,
  pageRangeWords,
  resolveDefaultVoiceId,
  selectedChapterWords,
  validatePageRange,
  voiceFit,
  type Catalog,
  type ChapterSummary,
  castStartErrorMessage,
} from "./jobFormModel";

function chapter(overrides: Partial<ChapterSummary> = {}): ChapterSummary {
  return {
    id: "ch-1",
    idx: 1,
    title: "Intro",
    pageStart: 1,
    pageEnd: 10,
    isPseudo: false,
    wordCount: 4000,
    ...overrides,
  };
}

describe("chapterWords", () => {
  it("uses the word count when present", () => {
    expect(chapterWords(chapter())).toBe(4000);
  });

  it("falls back to pages when word count missing", () => {
    expect(chapterWords(chapter({ wordCount: null }))).toBe(2500);
  });
});

describe("selectedChapterWords", () => {
  it("sums only selected chapters", () => {
    const chapters = [
      chapter(),
      chapter({ id: "ch-2", wordCount: 1500 }),
      chapter({ id: "ch-3", wordCount: 900 }),
    ];
    expect(selectedChapterWords(chapters, new Set(["ch-1", "ch-3"]))).toBe(
      4900,
    );
  });

  it("returns zero for empty selection", () => {
    expect(selectedChapterWords([chapter()], new Set())).toBe(0);
  });
});

describe("pageRangeWords", () => {
  it("prorates chapters by page overlap", () => {
    const chapters = [
      chapter({ pageStart: 1, pageEnd: 10, wordCount: 1000 }),
      chapter({ id: "ch-2", pageStart: 11, pageEnd: 20, wordCount: 2000 }),
    ];
    expect(pageRangeWords(chapters, 6, 15)).toBe(1500);
  });

  it("falls back to page estimate when no chapters overlap", () => {
    const chapters = [chapter({ pageStart: 100, pageEnd: 120 })];
    expect(pageRangeWords(chapters, 1, 4)).toBe(1000);
  });

  it("returns zero for inverted ranges", () => {
    expect(pageRangeWords([chapter()], 10, 5)).toBe(0);
  });
});

describe("validatePageRange", () => {
  it("accepts a valid range", () => {
    expect(validatePageRange("3", "12", 100)).toEqual({
      ok: true,
      start: 3,
      end: 12,
    });
  });

  it("rejects non-numeric input", () => {
    const result = validatePageRange("a", "9", 100);
    expect(result.ok).toBe(false);
  });

  it("rejects a start below one", () => {
    const result = validatePageRange("0", "9", 100);
    expect(result.ok).toBe(false);
  });

  it("rejects an end beyond the book", () => {
    const result = validatePageRange("1", "101", 100);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toContain("100");
  });

  it("rejects inverted ranges", () => {
    expect(validatePageRange("9", "3", 100).ok).toBe(false);
  });

  it("skips the page-count check when unknown", () => {
    expect(validatePageRange("1", "9999", null).ok).toBe(true);
  });
});

describe("buildEtaRequest", () => {
  it("carries the models to estimate for", () => {
    const request = buildEtaRequest({
      mode: "narrator",
      words: 5000,
      voiceEngine: "kokoro-82m",
      llmModelId: "phi4-mini-q4km",
    });
    expect(request.voiceEngine).toBe("kokoro-82m");
    expect(request.llmModelId).toBe("phi4-mini-q4km");
    expect(request.words).toBe(5000);
  });

  it("defaults the llm model to null", () => {
    const request = buildEtaRequest({
      mode: "teacher",
      words: 5000,
      voiceEngine: "kokoro-82m",
    });
    expect(request.llmModelId).toBeNull();
  });

  it("asserts nothing about the hardware", () => {
    const request = buildEtaRequest({
      mode: "narrator",
      words: 100,
      voiceEngine: "kokoro-82m",
    });
    expect(Object.keys(request).sort()).toEqual([
      "llmModelId",
      "mode",
      "ttsWorkers",
      "voiceEngine",
      "words",
    ]);
  });
});

describe("formatEta", () => {
  it("formats sub-minute estimates", () => {
    expect(formatEta(20)).toBe("< 1 min");
  });

  it("formats minutes", () => {
    expect(formatEta(1440)).toBe("~24 min");
  });

  it("formats whole hours", () => {
    expect(formatEta(7200)).toBe("~2 h");
  });

  it("formats hours with minutes", () => {
    expect(formatEta(5400)).toBe("~1 h 30 min");
  });

  it("returns a dash for invalid totals", () => {
    expect(formatEta(0)).toBe("—");
    expect(formatEta(Number.NaN)).toBe("—");
  });
});

describe("chapterMeta", () => {
  it("shows words and page range", () => {
    expect(chapterMeta(chapter())).toBe("4,000 words · pp. 1–10");
  });

  it("shows a single page without a range", () => {
    expect(chapterMeta(chapter({ pageStart: 5, pageEnd: 5 }))).toBe(
      "4,000 words · p. 5",
    );
  });

  it("omits words when unknown", () => {
    expect(chapterMeta(chapter({ wordCount: null }))).toBe("pp. 1–10");
  });

  it("formats metadata for Arabic locale with Western digits", () => {
    expect(chapterMeta(chapter(), "ar")).toBe("4,000 كلمة · ص. 1–10");
    expect(chapterMeta(chapter({ pageStart: 5, pageEnd: 5 }), "ar")).toBe(
      "4,000 كلمة · ص. 5",
    );
    expect(chapterMeta(chapter({ wordCount: null }), "ar")).toBe("ص. 1–10");
  });
});

describe("voiceFit", () => {
  const withBadge: Catalog = {
    models: [{ id: "kokoro-82m", task: "tts", badge: "green" }],
  };
  const noBadge: Catalog = {
    models: [{ id: "kokoro-82m", task: "tts" }],
  };

  it("returns a labelled badge for a matching engine", () => {
    expect(voiceFit(withBadge, "kokoro-82m")).toEqual({
      kind: "green",
      label: "Fast",
    });
  });

  it("returns null when no badge is present", () => {
    expect(voiceFit(noBadge, "kokoro-82m")).toBeNull();
  });

  it("returns null for an unknown engine or missing catalog", () => {
    expect(voiceFit(withBadge, "missing")).toBeNull();
    expect(voiceFit(null, "kokoro-82m")).toBeNull();
  });
});

describe("missingModels", () => {
  const withNames: Catalog = {
    models: [
      { id: "kokoro-82m", task: "tts", display_name: "Kokoro 82M" },
      { id: "qwen-9b", task: "llm", display_name: "Qwen 9B" },
    ],
  };

  it("flags the voice model when it is not installed", () => {
    const result = missingModels({
      mode: "narrator",
      voiceEngine: "kokoro-82m",
      catalog: withNames,
      installedIds: new Set(),
    });
    expect(result).toHaveLength(1);
    expect(result[0]!.modelId).toBe("kokoro-82m");
    expect(result[0]!.message).toContain("Narration voice");
    expect(result[0]!.message).not.toContain("Kokoro");
  });

  it("returns nothing when the voice is installed in narrator mode", () => {
    expect(
      missingModels({
        mode: "narrator",
        voiceEngine: "kokoro-82m",
        catalog: withNames,
        installedIds: new Set(["kokoro-82m"]),
      }),
    ).toEqual([]);
  });

  it("adds an LLM warning for teacher mode when none is installed", () => {
    const result = missingModels({
      mode: "teacher",
      voiceEngine: "kokoro-82m",
      catalog: withNames,
      installedIds: new Set(["kokoro-82m"]),
    });
    expect(result).toHaveLength(1);
    expect(result[0]!.message).toContain("Teacher mode");
  });

  it("clears the LLM warning once any llm model is installed", () => {
    expect(
      missingModels({
        mode: "teacher",
        voiceEngine: "kokoro-82m",
        catalog: withNames,
        installedIds: new Set(["kokoro-82m", "qwen-9b"]),
      }),
    ).toEqual([]);
  });

  it("does not require local voice model when ttsIsCloud is true", () => {
    expect(
      missingModels({
        mode: "narrator",
        voiceEngine: null,
        catalog: withNames,
        installedIds: new Set(),
        ttsIsCloud: true,
      }),
    ).toEqual([]);
  });

  it("does not require local LLM when scriptIsCloud is true", () => {
    expect(
      missingModels({
        mode: "teacher",
        voiceEngine: "kokoro-82m",
        catalog: withNames,
        installedIds: new Set(["kokoro-82m"]),
        scriptIsCloud: true,
      }),
    ).toEqual([]);
  });
});

describe("resolveDefaultVoiceId", () => {
  const profile = (
    id: string,
    engine: string,
  ): import("./jobFormModel").VoiceProfileSummary => ({
    id,
    name: id,
    engine,
    kind: "preset",
    presetVoiceId: null,
  });
  const ghostKokoro = profile("preset-kokoro-af-heart", "kokoro-82m");
  const lite = profile(
    "preset-lite-tts",
    "lite-tts",
  );

  it("prefers the previous choice when it is listed and its engine is installed", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["kokoro-82m", "lite-tts"]),
        "preset-lite-tts",
      ),
    ).toBe("preset-lite-tts");
  });

  it("ignores a previous choice whose engine is no longer installed", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["lite-tts"]),
        "preset-kokoro-af-heart",
      ),
    ).toBe("preset-lite-tts");
  });

  it("ignores a previous choice that no longer exists", () => {
    expect(
      resolveDefaultVoiceId(
        [lite],
        new Set(["lite-tts"]),
        "gone",
      ),
    ).toBe("preset-lite-tts");
  });

  it("skips profiles of uninstalled models even when they are listed first", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["lite-tts"]),
        null,
      ),
    ).toBe("preset-lite-tts");
  });

  it("falls back to the first listed profile when nothing is installed", () => {
    expect(resolveDefaultVoiceId([ghostKokoro, lite], new Set(), null)).toBe(
      "preset-kokoro-af-heart",
    );
  });

  it("returns null when no profiles exist", () => {
    expect(resolveDefaultVoiceId([], new Set(["kokoro-82m"]), null)).toBeNull();
  });

  it("prefers an installed voice that speaks the book language", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["kokoro-82m", "lite-tts"]),
        null,
        (voice) => voice.id === lite.id,
      ),
    ).toBe("preset-lite-tts");
  });

  it("still lets the previous choice win even when it cannot speak the book language", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["kokoro-82m", "lite-tts"]),
        "preset-kokoro-af-heart",
        (voice) => voice.id === lite.id,
      ),
    ).toBe("preset-kokoro-af-heart");
  });

  it("falls back to any installed voice when none speaks the book language", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["kokoro-82m", "lite-tts"]),
        null,
        () => false,
      ),
    ).toBe("preset-kokoro-af-heart");
  });

  it("prefers the voice whose engine fits this machine best", () => {
    const fitRank = (engine: string): number =>
      engine === "lite-tts" ? 0 : 2;
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["kokoro-82m", "lite-tts"]),
        null,
        undefined,
        fitRank,
      ),
    ).toBe("preset-lite-tts");
    expect(
      resolveDefaultVoiceId(
        [lite, ghostKokoro],
        new Set(["kokoro-82m", "lite-tts"]),
        null,
        undefined,
        (engine) => (engine === "kokoro-82m" ? 0 : 2),
      ),
    ).toBe("preset-kokoro-af-heart");
  });

  it("keeps an unknown fit behind a known fit", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["kokoro-82m", "lite-tts"]),
        null,
        undefined,
        (engine) => (engine === "lite-tts" ? 1 : null),
      ),
    ).toBe("preset-lite-tts");
  });

  it("never picks an uninstalled voice just because it speaks the language", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["lite-tts"]),
        null,
        (voice) => voice.id === ghostKokoro.id,
      ),
    ).toBe("preset-lite-tts");
  });

  it("prefers the user-configured preferredEngine when installed", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["kokoro-82m", "lite-tts"]),
        null,
        undefined,
        undefined,
        "lite-tts",
      ),
    ).toBe("preset-lite-tts");
  });

  it("respects speaksBook within the preferredEngine", () => {
    const studioEn = profile("preset-studio-en", "studio-tts");
    const studioAr = profile("preset-studio-ar", "studio-tts");
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, studioEn, studioAr],
        new Set(["kokoro-82m", "studio-tts"]),
        null,
        (v) => v.id === "preset-studio-ar",
        undefined,
        "studio-tts",
      ),
    ).toBe("preset-studio-ar");
  });

  it("respects lastChoiceId when it matches the preferredEngine", () => {
    const studio1 = profile("preset-studio-1", "studio-tts");
    const studio2 = profile("preset-studio-2", "studio-tts");
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, studio1, studio2],
        new Set(["kokoro-82m", "studio-tts"]),
        "preset-studio-2",
        undefined,
        undefined,
        "studio-tts",
      ),
    ).toBe("preset-studio-2");
  });

  it("falls back gracefully if preferredEngine is not installed", () => {
    expect(
      resolveDefaultVoiceId(
        [ghostKokoro, lite],
        new Set(["kokoro-82m"]),
        null,
        undefined,
        undefined,
        "lite-tts",
      ),
    ).toBe("preset-kokoro-af-heart");
  });
});

describe("voice-less form state", () => {
  it("voiceFit returns null without an engine", () => {
    const catalog: Catalog = {
      models: [{ id: "kokoro-82m", task: "tts", badge: "green" }],
    };
    expect(voiceFit(catalog, null)).toBeNull();
  });

  it("missingModels explains that no narration voice is installed", () => {
    const result = missingModels({
      mode: "narrator",
      voiceEngine: null,
      catalog: null,
      installedIds: new Set(),
    });
    expect(result).toHaveLength(1);
    expect(result[0]!.modelId).toBe("");
    expect(result[0]!.message).toContain("No narration voice is installed yet");
  });
});

describe("buildCloudAwareEtaRequest (Hermes R2 blocker 8)", () => {
  it("cloud TTS means no combined local number — never a local-shaped lie", async () => {
    const { buildCloudAwareEtaRequest } = await import("./jobFormModel");
    expect(
      buildCloudAwareEtaRequest({
        mode: "teacher",
        words: 1200,
        voiceEngine: "kokoro-82m",
        scriptIsCloud: false,
        ttsIsCloud: true,
      }),
    ).toBeNull();
    expect(
      buildCloudAwareEtaRequest({
        mode: "teacher",
        words: 1200,
        voiceEngine: null,
        scriptIsCloud: true,
        ttsIsCloud: true,
      }),
    ).toBeNull();
  });

  it("cloud Script drops the local Teacher pass from the estimate", async () => {
    const { buildCloudAwareEtaRequest } = await import("./jobFormModel");
    const request = buildCloudAwareEtaRequest({
      mode: "teacher",
      words: 1200,
      voiceEngine: "kokoro-82m",
      scriptIsCloud: true,
      ttsIsCloud: false,
    });
    expect(request?.mode).toBe("narrator");
    expect(request?.llmModelId).toBeNull();
  });

  it("all-local requests are unchanged", async () => {
    const { buildCloudAwareEtaRequest } = await import("./jobFormModel");
    const request = buildCloudAwareEtaRequest({
      mode: "teacher",
      words: 1200,
      voiceEngine: "kokoro-82m",
      scriptIsCloud: false,
      ttsIsCloud: false,
    });
    expect(request?.mode).toBe("teacher");
    expect(request?.voiceEngine).toBe("kokoro-82m");
  });
});

describe("castStartErrorMessage", () => {
  it("names the missing encoder instead of echoing the backend refusal", () => {
    const message = castStartErrorMessage(
      "invalid request: ERR_FFMPEG_MISSING: the audio encoder could not be started (ffmpeg could not be started: no bundled copy and none on PATH (set TXTCASTER_FFMPEG to override))",
    );
    expect(message).toContain("audio encoder");
    expect(message).not.toContain("ERR_");
    expect(message).not.toContain("TXTCASTER_FFMPEG");
  });

  it("keeps the reason for every other refusal", () => {
    expect(castStartErrorMessage("application is shutting down")).toContain(
      "application is shutting down",
    );
  });
});
