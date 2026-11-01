import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: () => Promise.resolve(null),
}));
// The locale store mirrors the choice into <html lang dir> and localStorage;
// this test has no DOM, so give it the two objects it touches.
vi.stubGlobal("document", {
  documentElement: {
    lang: "",
    dir: "",
    setAttribute() {},
    removeAttribute() {},
  },
});
vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem() {},
  removeItem() {},
});

import { stripIsolates } from "./i18n/format";
import { setLocalePreference } from "./i18n/localeStore";
import { formatDuration, formatSessionDate } from "./sessionHistoryStore";

// I18N-2 §0 floor item 2 (torkiv.com AR capture of main): "min 19" and
// "Tue 1 Sept" in the Arabic Tutor pane — a hard-coded `min` suffix composed
// with a number, and dates formatted with a fixed en-GB locale. Durations now
// go through the catalog's plural forms with a number island; dates through
// Intl in the UI locale (Arabic names, Latin digits).
const ENGLISH_DATE_TOKEN =
  /\b(Mon|Tue|Wed|Thu|Fri|Sat|Sun|Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sept?|Oct|Nov|Dec)\b/;
const NOW = Date.UTC(2026, 8, 7, 12, 0);
const TZ = "UTC";

afterEach(() => setLocalePreference("en"));

describe("session history formatting follows the UI locale", () => {
  it("English is unchanged", () => {
    setLocalePreference("en");
    expect(stripIsolates(formatDuration(19 * 60_000))).toBe("19 min");
    expect(stripIsolates(formatDuration(72 * 60_000))).toBe("1 h 12 min");
    expect(formatSessionDate(Date.UTC(2026, 8, 1, 9, 0), NOW, TZ)).toMatch(
      /Tue 1 Sept?/,
    );
  });

  it("Arabic durations use plural forms with a Latin-digit island, never a bare 'min'", () => {
    setLocalePreference("ar");
    const nineteen = formatDuration(19 * 60_000);
    expect(nineteen).not.toMatch(/\bmin\b|\bh\b/);
    // Visible History text is the string form: it must carry no bidi controls.
    expect(nineteen).not.toMatch(/[\u2066-\u2069]/);
    expect(nineteen).toContain("19");
    expect(stripIsolates(nineteen)).toBe("19 دقيقة");
    expect(stripIsolates(formatDuration(3 * 60_000))).toBe("3 دقائق");
    expect(stripIsolates(formatDuration(60_000))).toBe("دقيقة واحدة");
    expect(stripIsolates(formatDuration(120 * 60_000))).toBe("ساعتان");
    expect(stripIsolates(formatDuration(72 * 60_000))).toBe(
      "ساعة واحدة و12 دقيقة",
    );
    expect(stripIsolates(formatDuration(20_000))).toBe("أقل من دقيقة");
  });

  it("Arabic dates carry no English weekday or month token and keep Western digits", () => {
    setLocalePreference("ar");
    const older = formatSessionDate(Date.UTC(2026, 8, 1, 9, 0), NOW, TZ);
    expect(older).not.toMatch(ENGLISH_DATE_TOKEN);
    expect(older).toMatch(/1/);
    expect(older).not.toMatch(/[٠-٩]/);
    const yesterday = formatSessionDate(NOW - 86_400_000, NOW, TZ);
    expect(yesterday).toBe("أمس، 12:00");
  });
});
