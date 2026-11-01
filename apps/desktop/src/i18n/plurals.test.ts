import { describe, expect, it } from "vitest";

import { CATALOGS, type MessageKey } from "./catalog";
import {
  formatMessage,
  selectForm,
  stripIsolates,
  type PluralForms,
} from "./format";
import { pluralCategory, type Locale } from "./localeModel";

// Round-1 C2-1: "2 من 3 فصلًا" — the footer key was a plain string with
// the noun frozen in the `many` form, outside the plural mechanism, and
// selectForm falls back to `other` silently when a category form is missing.
// Law: every plural key carries a form for EVERY CLDR category its locale
// can select for n ∈ {0,1,2,3,10,11,100} — no fallback, ever.
const COUNTS = [0, 1, 2, 3, 10, 11, 100];

function pluralKeys(locale: Locale): MessageKey[] {
  return (Object.keys(CATALOGS[locale]) as MessageKey[]).filter(
    (k) => typeof CATALOGS[locale][k] !== "string",
  );
}

describe("plural forms cover every CLDR category the locale selects", () => {
  for (const locale of ["en", "ar"] as const) {
    it(`${locale}: ${pluralKeys(locale).length} plural keys × ${COUNTS.length} counts`, () => {
      expect(pluralKeys(locale).length).toBeGreaterThan(0);
      for (const key of pluralKeys(locale)) {
        const message = CATALOGS[locale][key] as PluralForms;
        for (const count of COUNTS) {
          const category = pluralCategory(locale, count);
          expect(
            message[category],
            `${locale} ${key} n=${count} → ${category}`,
          ).toBeTypeOf("string");
          expect(selectForm(locale, message, { count })).toBe(
            message[category],
          );
        }
      }
    });
  }

  it("Arabic selects zero/one/two/few/many/other for 0/1/2/3/11/100", () => {
    expect(COUNTS.map((n) => pluralCategory("ar", n))).toEqual([
      "zero",
      "one",
      "two",
      "few",
      "few",
      "many",
      "other",
    ]);
  });

  it("the outline footer says 3 فصول (few), 11 فصلًا (many), فصلين (two), 100 فصل (other)", () => {
    const line = (count: number) =>
      stripIsolates(
        formatMessage("ar", CATALOGS.ar["reader.castCoverage"], {
          cast: 2,
          count,
        }),
      );
    expect(line(3)).toContain("2 من 3 فصول");
    expect(line(10)).toContain("10 فصول");
    expect(line(11)).toContain("11 فصلًا");
    expect(line(2)).toContain("من فصلين");
    expect(line(100)).toContain("100 فصل ");
    expect(line(3)).not.toMatch(/3 فصلًا/);
  });

  it("History durations: 2 minutes is the dual", () => {
    const m = (n: number) =>
      stripIsolates(
        formatMessage("ar", CATALOGS.ar["session.durationMinutes"], {
          count: n,
        }),
      );
    expect(m(2)).toBe("دقيقتان");
    expect(m(3)).toBe("3 دقائق");
    expect(m(11)).toBe("11 دقيقة");
    expect(
      stripIsolates(
        formatMessage("ar", CATALOGS.ar["session.durationHours"], { count: 2 }),
      ),
    ).toBe("ساعتان");
  });
});
