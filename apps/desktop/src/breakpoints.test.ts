import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const CSS = readSource(new URL("./index.css", import.meta.url));

const TAILWIND_DEFAULT_REMS: Record<string, number> = {
  sm: 40,
  md: 48,
  lg: 64,
  xl: 80,
  "2xl": 96,
};

const ROOT_FONT_SIZE_PX = 16;

function declaredBreakpoint(name: string): string | null {
  const match = new RegExp(`--breakpoint-${name}:\\s*([^;]+);`).exec(CSS);
  return match == null ? null : match[1].trim();
}

describe("breakpoints are declared in pixels", () => {
  it("overrides every Tailwind default", () => {
    for (const name of Object.keys(TAILWIND_DEFAULT_REMS)) {
      expect(declaredBreakpoint(name), `--breakpoint-${name} is missing`).not.toBeNull();
    }
  });

  it("keeps each value identical to the rem default it replaces", () => {
    for (const [name, rems] of Object.entries(TAILWIND_DEFAULT_REMS)) {
      const declared = declaredBreakpoint(name);

      expect(declared).toMatch(/^\d+px$/);
      expect(
        Number.parseInt(declared as string, 10),
        `--breakpoint-${name} must equal ${rems}rem at a ${ROOT_FONT_SIZE_PX}px root`,
      ).toBe(rems * ROOT_FONT_SIZE_PX);
    }
  });

  it("declares no rem breakpoint, which WebKitGTK resolves against a scaled basis", () => {
    const remBreakpoint = /--breakpoint-[a-z0-9]+:\s*[\d.]+rem/.exec(CSS);

    expect(
      remBreakpoint,
      `rem breakpoints are resolved against 16px x devicePixelRatio under fractional ` +
        `scaling, so a rem value here silently changes which variants apply: ` +
        `${remBreakpoint?.[0] ?? ""}`,
    ).toBeNull();
  });
});
