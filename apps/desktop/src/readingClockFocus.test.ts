import { existsSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const FILES = [
  "readingClock.ts",
  "browserScheduler.ts",
  "readingHighlight.ts",
  "ReadingView.tsx",
  "ReadingScript.tsx",
  "ReadingFacsimile.tsx",
  "ReadingFallback.tsx",
  "ReadingHeader.tsx",
] as const;

const FORBIDDEN = [
  { pattern: /hasFocus\s*\(/, why: "reading document focus makes the clock stop when the window is merely unfocused" },
  { pattern: /document\.activeElement/, why: "the clock must not care which element is focused" },
  { pattern: /addEventListener\(\s*["']blur["']/, why: "a blur listener is a focus gate by another name" },
  { pattern: /addEventListener\(\s*["']focus["']/, why: "a focus listener is a focus gate by another name" },
  { pattern: /onBlur=/, why: "a blur handler in the reading surface would tie ticking to focus" },
] as const;

const FOCUS_EXEMPT = new Set(["ReadingView.tsx"]);

function sources(): Array<{ name: string; body: string }> {
  return FILES.filter((name) => existsSync(new URL(`./${name}`, import.meta.url))).map(
    (name) => ({
      name,
      body: readSource(new URL(`./${name}`, import.meta.url)),
    }),
  );
}

describe("the reading surface never gates on focus", () => {
  it("checks the files that would carry such a gate", () => {
    expect(sources().map((entry) => entry.name)).toEqual([...FILES]);
  });

  it("contains none of the focus predicates C2 claims we do not read", () => {
    for (const { name, body } of sources()) {
      for (const rule of FORBIDDEN) {
        if (
          FOCUS_EXEMPT.has(name) &&
          rule.pattern.source.includes("activeElement")
        ) {
          continue;
        }
        expect(rule.pattern.test(body), `${name}: ${rule.why}`).toBe(false);
      }
    }
  });

  it("allows the overlay exactly one use of activeElement, for restoring focus on close", () => {
    const view = sources().find((entry) => entry.name === "ReadingView.tsx");
    const uses = view?.body.match(/document\.activeElement/g) ?? [];

    expect(
      uses.length,
      "restoring focus when the overlay closes is an accessibility duty and is not a tick gate; more than one use means something else started reading focus",
    ).toBeLessThanOrEqual(1);
  });
});
