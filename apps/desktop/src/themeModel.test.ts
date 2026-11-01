import { describe, expect, it } from "vitest";

import {
  parseCssColor,
  parsePreference,
  resolveTheme,
  THEME_MIRROR_KEY,
  THEME_PREFERENCES,
  WINDOW_CHROME_TOKENS,
  windowChrome,
} from "./themeModel";
import { readSource } from "./tests/readSource";

describe("parsePreference", () => {
  it("accepts the two explicit themes", () => {
    expect(parsePreference("light")).toBe("light");
    expect(parsePreference("dark")).toBe("dark");
  });

  it("treats everything else as system, because a corrupt mirror must not pin a theme", () => {
    for (const raw of [null, "", "system", "auto", "DARK", "0"]) {
      expect(parsePreference(raw)).toBe("system");
    }
  });
});

describe("resolveTheme", () => {
  it("lets an explicit preference override the OS", () => {
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", false)).toBe("dark");
  });

  it("follows the OS only under system", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
  });
});

describe("parseCssColor", () => {
  it("reads the rgb()/rgba() forms getComputedStyle serialises", () => {
    expect(parseCssColor("rgb(23, 21, 18)")).toEqual({
      red: 23,
      green: 21,
      blue: 18,
    });
    expect(parseCssColor("rgba(255, 255, 255, 0.5)")).toEqual({
      red: 255,
      green: 255,
      blue: 255,
    });
  });

  it("refuses anything it cannot prove is a colour", () => {
    for (const raw of ["", "transparent", "#171512", "rgb(999, 0, 0)"]) {
      expect(parseCssColor(raw), raw).toBeNull();
    }
  });
});

describe("windowChrome", () => {
  const LIGHT: Record<string, string> = {
    "--color-ink-900": "rgb(233, 230, 222)",
    "--color-ink-100": "rgb(36, 39, 44)",
    "--color-ember-500": "rgb(186, 85, 24)",
    "--color-ink-700": "rgb(201, 196, 184)",
  };

  it("frames the window in the top-bar surface, ink text and an ember focus border", () => {
    expect(windowChrome((token) => LIGHT[token] ?? "", "dark")).toEqual({
      caption: { red: 233, green: 230, blue: 222 },
      text: { red: 36, green: 39, blue: 44 },
      borderFocused: { red: 186, green: 85, blue: 24 },
      borderUnfocused: { red: 201, green: 196, blue: 184 },
      scheme: "dark",
    });
  });

  it("reads exactly the four identity tokens", () => {
    const asked: string[] = [];
    windowChrome((token) => {
      asked.push(token);
      return LIGHT[token] ?? "";
    }, "system");
    expect(asked.sort()).toEqual(Object.values(WINDOW_CHROME_TOKENS).sort());
    expect(Object.keys(LIGHT).sort()).toEqual(asked);
  });

  it("sends nothing rather than a partly guessed frame", () => {
    for (const missing of Object.keys(LIGHT)) {
      const read = (token: string) =>
        token === missing ? "" : (LIGHT[token] ?? "");
      expect(windowChrome(read, "light"), missing).toBeNull();
    }
  });
});

describe("the pre-paint script in index.html honours the same contract", () => {
  const html = readSource(new URL("../index.html", import.meta.url));

  it("reads the same mirror key the store writes", () => {
    expect(html).toContain(`localStorage.getItem("${THEME_MIRROR_KEY}")`);
  });

  it("falls back to the OS preference, not to a hardcoded theme", () => {
    expect(html).toContain("prefers-color-scheme: dark");
  });

  it("sets data-theme on the document element before the app boots", () => {
    expect(html).toMatch(/documentElement\.dataset\.theme/);
  });

  it("recognises exactly the preferences the model does", () => {
    for (const preference of THEME_PREFERENCES) {
      if (preference === "system") continue;
      expect(html).toContain(`"${preference}"`);
    }
  });
});
