import { afterEach, describe, expect, it, vi } from "vitest";
import { contentLanguage } from "./contentLanguage";

afterEach(() => vi.unstubAllGlobals());

describe("content language metadata", () => {
  it.each([
    [" ar-EG ", "ar-EG", "rtl"],
    ["eng", "en", "ltr"],
    ["ara", "ar", "rtl"],
    ["az-Arab", "az-Arab", "rtl"],
    ["az-Latn", "az-Latn", "ltr"],
    ["zh-Hant", "zh-Hant", "ltr"],
    ["xyz", "xyz", "auto"],
    ["auto", "", "auto"],
    ["und", "", "auto"],
    ["mixed", "", "auto"],
    ["en_ar", "", "auto"],
    [null, "", "auto"],
  ])(
    "validates %s without guessing the book's language",
    (input, lang, dir) => {
      expect(contentLanguage(input)).toEqual({ lang, dir });
    },
  );

  it("uses script-aware fallbacks on webviews without locale text information", () => {
    class LocaleWithoutDirection extends Intl.Locale {}
    Object.defineProperties(LocaleWithoutDirection.prototype, {
      getTextInfo: { value: undefined },
      textInfo: { value: undefined },
    });
    vi.stubGlobal(
      "Intl",
      new Proxy(Intl, {
        get(target, key) {
          return key === "Locale"
            ? LocaleWithoutDirection
            : Reflect.get(target, key);
        },
      }),
    );
    expect(contentLanguage("az-Arab")).toEqual({ lang: "az-Arab", dir: "rtl" });
    expect(contentLanguage("az-Latn")).toEqual({ lang: "az-Latn", dir: "ltr" });
    expect(contentLanguage("th")).toEqual({ lang: "th", dir: "auto" });
  });
});
