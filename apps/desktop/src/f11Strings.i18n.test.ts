import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: () => Promise.resolve(null),
}));
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

import { F11_STRINGS } from "./f11Strings";
import { stripIsolates } from "./i18n/format";
import { setLocalePreference } from "./i18n/localeStore";

afterEach(() => setLocalePreference("en"));

// Round-2 F1: "الفصول 3–4" with bare numbers renders as 4–3 in an RTL run. The
// range is ONE LTR island (U+2066 … U+2069) — the History rail renders the
// string form, so the isolate is what reaches the DOM.
describe("chapter range is a single LTR island", () => {
  it("Arabic keeps first–last order through the isolate", () => {
    setLocalePreference("ar");
    const range = F11_STRINGS.chapterRange(3, 4);
    expect(range).toContain("⁦3–4⁩");
    expect(range.indexOf("⁦")).toBeLessThan(range.indexOf("3"));
    expect(stripIsolates(range)).toBe("الفصول 3–4");
  });
  it("English is unchanged apart from the isolate", () => {
    expect(stripIsolates(F11_STRINGS.chapterRange(3, 4))).toBe("Ch. 3–4");
    expect(F11_STRINGS.chapterRange(3, 4)).toContain("⁦3–4⁩");
  });
});
