import { describe, expect, it } from "vitest";

import {
  cloudArgs,
  consentDataCategoryCopy,
  consentPhaseCopy,
  consentTitleCopy,
  costLine,
  initialProviderSelection,
  LOOPBACK_FACT,
  parseConsentRequired,
} from "./castProviderModel";
import { t } from "./i18n";

describe("castProviderModel", () => {
  it("every fresh form starts Local/Local — the T4 law has no other constructor", () => {
    expect(initialProviderSelection()).toEqual({
      script: { kind: "local" },
      tts: { kind: "local" },
    });
    // Duplication resets by construction: duplicating a job builds a fresh
    // form, and a fresh form has exactly one initial state.
    expect(initialProviderSelection()).toEqual(initialProviderSelection());
  });

  it("the loopback fact names this computer and never cost copy (A18/T14)", () => {
    expect(LOOPBACK_FACT()).toContain("this computer");
    expect(LOOPBACK_FACT()).not.toMatch(/free/i);
  });

  it("cost truth is three-valued and never renders zero or Free (T12)", () => {
    expect(
      costLine({
        state: "known",
        usd_low: 1.2,
        usd_high: 1.56,
        currency: "USD",
        as_of: "2026-08-28",
        basis: "b",
      }),
    ).toContain("~$1.20–~$1.56 USD (rates as of 2026-08-28");
    expect(
      costLine({
        state: "stale",
        usd_low: 0.04,
        usd_high: 0.04,
        currency: "USD",
        as_of: "2025-01-01",
        basis: "b",
      }),
    ).toContain("may be out of date");
    expect(
      costLine({
        state: "unknown",
        message: "Cost unavailable from this app - check your provider",
      }),
    ).toBe("Cost unavailable from this app - check your provider");
    expect(costLine(null)).toBe("Estimating…");
    // A tiny real amount never renders as a zero (T12) — the formatter is
    // the enforcement point, so the pin is on its output.
    const tiny = costLine({
      state: "known",
      usd_low: 0.0004,
      usd_high: 0.0004,
      currency: "USD",
      as_of: "2026-08-28",
      basis: "b",
    });
    expect(tiny).toContain("under $0.01");
    expect(tiny).not.toMatch(/\$0\.0{2,}/);
    for (const line of [
      costLine({
        state: "unknown",
        message: "Cost unavailable from this app - check your provider",
      }),
      LOOPBACK_FACT(),
    ]) {
      expect(line).not.toMatch(/\$0|free/i);
    }
  });

  it("the consent refusal parses structurally and anything else stays an error", () => {
    const payload = [
      {
        phase: "script",
        connectionId: "c",
        connectionName: "Work Gemini",
        provider: "gemini",
        hosts: ["generativelanguage.googleapis.com"],
        dataCategories: ["source_text"],
        retentionUrl: "https://ai.google.dev/gemini-api/terms",
      },
    ];
    const parsed = parseConsentRequired(
      `CONSENT_REQUIRED:${JSON.stringify(payload)}`,
    );
    expect(parsed?.[0]?.hosts).toEqual(["generativelanguage.googleapis.com"]);
    expect(parseConsentRequired("connection 'x' is disabled")).toBeNull();
    expect(parseConsentRequired("CONSENT_REQUIRED:not json")).toBeNull();
  });

  it("consent copy names data categories in plain words", () => {
    expect(consentDataCategoryCopy("source_text")).toContain("source text");
    expect(consentDataCategoryCopy("script_text")).toContain("script text");
  });

  it("names the figure images a vision phase sends, never the raw category", () => {
    expect(consentDataCategoryCopy("figure_crops")).toBe(
      t("cast.data.figureCrops"),
    );
    expect(consentDataCategoryCopy("figure_crops")).not.toContain(
      "figure_crops",
    );
  });

  it("labels each consent phase by what it does", () => {
    expect(consentPhaseCopy("script")).toBe(t("cast.phaseScripting"));
    expect(consentPhaseCopy("vision")).toBe(t("cast.phaseFigures"));
    expect(consentPhaseCopy("tts")).toBe(t("cast.phaseAudio"));
  });

  it("the sheet's title says whether text, figure images, or both leave the device", () => {
    const script = { dataCategories: ["source_text"] };
    const vision = { dataCategories: ["figure_crops"] };
    expect(consentTitleCopy([script])).toBe(t("cast.consentTitle"));
    expect(consentTitleCopy([vision])).toBe(t("cast.consentTitleImages"));
    expect(consentTitleCopy([script, vision])).toBe(
      t("cast.consentTitleTextAndImages"),
    );
  });

  it("submission args carry cloud selections only when explicitly chosen", () => {
    expect(cloudArgs(initialProviderSelection())).toEqual({});
    expect(
      cloudArgs({
        script: {
          kind: "cloud",
          connectionId: "c-remote",
          model: "gemini-2.5-flash",
        },
        tts: { kind: "local" },
      }),
    ).toEqual({
      scriptCloud: {
        connectionId: "c-remote",
        model: "gemini-2.5-flash",
        effort: "low",
      },
    });
    expect(
      cloudArgs({
        script: {
          kind: "cloud",
          connectionId: "c-remote",
          model: "gemini-2.5-flash",
          effort: "high",
        },
        tts: { kind: "local" },
      }).scriptCloud?.effort,
    ).toBe("high");
    expect(
      cloudArgs({
        script: { kind: "local" },
        tts: { kind: "cloud", voiceProfileId: "vp-1" },
      }),
    ).toEqual({ ttsCloud: { voiceProfileId: "vp-1" } });
  });
});
