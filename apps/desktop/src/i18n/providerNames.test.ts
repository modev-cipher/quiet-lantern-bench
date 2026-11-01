import { describe, expect, it } from "vitest";

import { CATALOGS } from "./catalog";
import { PROVIDER_DISPLAY_NAMES, providerDisplayName } from "./providerNames";

// I18N-2 R-H: provider names are a display layer for the user's own
// connections (#25 Q3) and never leak into outcome copy (#31).
describe("providerNames", () => {
  it("names every BYOK provider id in English, identically for both locales", () => {
    for (const id of ["gemini", "deepgram", "cartesia", "openai_compat"]) {
      expect(PROVIDER_DISPLAY_NAMES[id]).toBeTruthy();
      expect(providerDisplayName(id)).toBe(PROVIDER_DISPLAY_NAMES[id]);
    }
    expect(providerDisplayName("gemini")).toBe("Google Gemini");
  });

  it("falls back to the id for a provider the map does not know", () => {
    expect(providerDisplayName("acme_voice")).toBe("acme_voice");
  });

  it("no Voice or Tutor catalog value names a provider or model (#31)", () => {
    const vendor =
      /\b(gemini|google|openai|deepgram|cartesia|chatterbox|kokoro|whisper)\b/i;
    for (const locale of ["en", "ar"] as const) {
      const catalog = CATALOGS[locale] as Record<string, unknown>;
      for (const [key, value] of Object.entries(catalog)) {
        if (!/^(voice|tutor|study|companion|session)\./.test(key)) continue;
        const forms =
          typeof value === "string"
            ? [value]
            : Object.values(value as Record<string, string>);
        for (const form of forms)
          expect(form, `${locale} ${key}`).not.toMatch(vendor);
      }
    }
  });
});
