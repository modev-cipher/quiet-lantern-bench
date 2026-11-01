import { describe, expect, it } from "vitest";

import {
  parseDirectionPreference,
  resolveDirection,
  systemDirection,
} from "./directionModel";
import { readSource } from "./tests/readSource";

describe("direction preference parsing", () => {
  it("accepts only the two explicit directions, defaulting to system", () => {
    expect(parseDirectionPreference("ltr")).toBe("ltr");
    expect(parseDirectionPreference("rtl")).toBe("rtl");
    expect(parseDirectionPreference("auto")).toBe("system");
    expect(parseDirectionPreference(null)).toBe("system");
    expect(parseDirectionPreference("garbage")).toBe("system");
  });
});

describe("system direction from locale", () => {
  it("maps right-to-left languages by primary subtag", () => {
    expect(systemDirection("ar")).toBe("rtl");
    expect(systemDirection("ar-EG")).toBe("rtl");
    expect(systemDirection("he-IL")).toBe("rtl");
    expect(systemDirection("fa_IR")).toBe("rtl");
    expect(systemDirection("ur")).toBe("rtl");
    expect(systemDirection("ckb")).toBe("rtl");
  });

  it("defaults everything else to left-to-right, script-aware", () => {
    expect(systemDirection("en-US")).toBe("ltr");
    expect(systemDirection("fr")).toBe("ltr");
    expect(systemDirection("ku")).toBe("ltr");
    expect(systemDirection("")).toBe("ltr");
  });

  it("keeps the pre-paint script's language set in lockstep with the model", () => {
    const html = readSource(new URL("../index.html", import.meta.url));
    const inline = html.match(/var rtl = \[([^\]]+)\]/);
    expect(
      inline,
      "index.html lost its pre-paint direction script",
    ).not.toBeNull();
    const scriptSet = new Set(
      (inline as RegExpMatchArray)[1]!
        .split(",")
        .map((entry) => entry.trim().replace(/"/g, ""))
        .filter((entry) => entry !== ""),
    );
    for (const language of ["ar", "he", "fa", "ur", "ckb"]) {
      expect(scriptSet.has(language), `${language} missing pre-paint`).toBe(
        true,
      );
    }
    expect(scriptSet.has("ku")).toBe(false);
    for (const language of scriptSet) {
      expect(
        systemDirection(language),
        `${language} is pre-paint RTL but the model disagrees`,
      ).toBe("rtl");
    }
    expect(html).toContain('localStorage.getItem("txtcaster.direction")');
  });
});

describe("resolveDirection", () => {
  it("lets an explicit preference beat the locale", () => {
    expect(resolveDirection("rtl", "en-US")).toBe("rtl");
    expect(resolveDirection("ltr", "ar-EG")).toBe("ltr");
  });

  it("follows the locale on system", () => {
    expect(resolveDirection("system", "ar")).toBe("rtl");
    expect(resolveDirection("system", "en")).toBe("ltr");
  });
});
