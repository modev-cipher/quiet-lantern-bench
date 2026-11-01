import { describe, expect, it } from "vitest";

import { bookAgentStateFrom, categoryLabel } from "./bookAgentModel";
import { t } from "./i18n";

describe("the Book composer's probe-backed states", () => {
  it("parses the three runtime answers and fails closed on garbage", () => {
    expect(bookAgentStateFrom({ state: "ready" })).toEqual({ kind: "ready" });
    expect(bookAgentStateFrom({ state: "unsupported" })).toEqual({
      kind: "unsupported",
    });
    const consent = bookAgentStateFrom({
      state: "consent_required",
      hosts: ["generativelanguage.googleapis.com"],
      data_categories: ["source_text", "voice_audio"],
      retention_url: "https://ai.google.dev/gemini-api/terms",
    });
    expect(consent.kind).toBe("consentRequired");
    for (const garbage of [null, "x", { state: "??" }, {}]) {
      expect(bookAgentStateFrom(garbage).kind).toBe("failed");
    }
  });

  it("consent copy says book text AND book metadata (A5 record)", () => {
    expect(categoryLabel("source_text")).toBe("Book text and book metadata");
    expect(categoryLabel("voice_audio")).toContain("microphone");
  });

  it("the whole-book scope discloses spoilers honestly (D9)", () => {
    expect(t("tutor.wholeBookDisclosure")).toContain("any part of this book");
    expect(t("tutor.wholeBookDisclosure")).toContain("haven't reached yet");
  });
});
