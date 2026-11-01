import { describe, expect, it } from "vitest";

import {
  directionForLocale,
  formatNumber,
  localeFromTag,
  parseLocalePreference,
  pluralCategory,
  resolveLocale,
} from "./localeModel";

describe("locale resolution order (#25 Q3: host locale by default, toggle wins)", () => {
  const host = { hostLocale: "ar-EG", navigatorLanguage: "en-US" };

  it("an explicit preference beats every environment signal", () => {
    expect(resolveLocale("en", host)).toBe("en");
    expect(
      resolveLocale("ar", { hostLocale: "en-US", navigatorLanguage: "en-US" }),
    ).toBe("ar");
  });

  it("system follows the host OS locale before the webview language", () => {
    expect(resolveLocale("system", host)).toBe("ar");
    expect(
      resolveLocale("system", {
        hostLocale: "en_US.UTF-8",
        navigatorLanguage: "ar",
      }),
    ).toBe("en");
  });

  it("system falls back to navigator.language when the host answer is missing or unsupported", () => {
    expect(
      resolveLocale("system", { hostLocale: null, navigatorLanguage: "ar-SA" }),
    ).toBe("ar");
    expect(
      resolveLocale("system", { hostLocale: "fr-FR", navigatorLanguage: "ar" }),
    ).toBe("ar");
  });

  it("with nothing usable the UI is English", () => {
    expect(
      resolveLocale("system", { hostLocale: null, navigatorLanguage: null }),
    ).toBe("en");
    expect(
      resolveLocale("system", { hostLocale: "de-DE", navigatorLanguage: "fr" }),
    ).toBe("en");
  });

  it("parses POSIX and BCP-47 shapes, rejects the rest", () => {
    expect(localeFromTag("ar_EG.UTF-8")).toBe("ar");
    expect(localeFromTag("AR")).toBe("ar");
    expect(localeFromTag("en-GB")).toBe("en");
    expect(localeFromTag("arb")).toBeNull();
    expect(localeFromTag("")).toBeNull();
  });

  it("stored preference parsing never yields an unsupported locale", () => {
    expect(parseLocalePreference("ar")).toBe("ar");
    expect(parseLocalePreference("fr")).toBe("system");
    expect(parseLocalePreference(null)).toBe("system");
  });
});

describe("direction, plurals and digits", () => {
  it("Arabic is RTL, English LTR", () => {
    expect(directionForLocale("ar")).toBe("rtl");
    expect(directionForLocale("en")).toBe("ltr");
  });

  it("Arabic exposes all six CLDR plural categories; English two", () => {
    const ar = [0, 1, 2, 3, 11, 100].map((n) => pluralCategory("ar", n));
    expect(ar).toEqual(["zero", "one", "two", "few", "many", "other"]);
    expect([1, 2].map((n) => pluralCategory("en", n))).toEqual([
      "one",
      "other",
    ]);
  });

  it("metrics keep Western digits in Arabic (nu-latn) and locale grouping", () => {
    expect(formatNumber("ar", 1234)).toBe("1,234");
    // Measured: ICU wraps the Arabic percent in LRM marks (U+200E) — the digits stay Western.
    const percent = formatNumber("ar", 0.5, { style: "percent" });
    expect(percent.replace(/\u200e/g, "")).toBe("50%");
    expect(percent).not.toMatch(/[٠-٩]/u);
    expect(formatNumber("en", 1234)).toBe("1,234");
  });
});
