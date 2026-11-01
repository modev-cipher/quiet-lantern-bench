import { describe, expect, it } from "vitest";

import { CATALOGS } from "./i18n/catalog";
import { readSource } from "./tests/readSource";

// Round-2 F5: the voice meta line ("Heart speaks American English · female voice")
// composed English words around a translated language name, and the e2e
// assert had dropped the gender half. The line is one catalog message per
// gender in both locales; the spec asserts the whole rendered line.
const VOICE_SELECTOR = readSource(
  new URL("./VoiceSelector.tsx", import.meta.url),
);

describe("voice meta line is catalog copy with the gender inside", () => {
  it("VoiceSelector renders voice.meta.female/male, never literal gender words", () => {
    expect(VOICE_SELECTOR).not.toMatch(/"female voice"|"male voice"|speaks\{" "\}/);
    expect(VOICE_SELECTOR).toContain('"voice.meta.female"');
    expect(VOICE_SELECTOR).toContain('"voice.meta.male"');
  });
  it("both locales carry both genders with the three params", () => {
    for (const locale of ["en", "ar"] as const)
      for (const key of ["voice.meta.female", "voice.meta.male"] as const) {
        const form = CATALOGS[locale][key] as string;
        for (const p of ["{flag}", "{name}", "{language}"])
          expect(form, `${locale} ${key}`).toContain(p);
      }
    expect(CATALOGS.en["voice.meta.female"]).toContain("female voice");
    expect(CATALOGS.en["voice.meta.male"]).toContain("male voice");
    expect(CATALOGS.ar["voice.meta.female"]).not.toBe(
      CATALOGS.ar["voice.meta.male"],
    );
  });
});
