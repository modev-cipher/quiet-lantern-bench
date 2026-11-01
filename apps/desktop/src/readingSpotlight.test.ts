import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import ReadingSpotlight from "./ReadingSpotlight";
import type { SpotlightPlan } from "./teacherAnchorModel";
import { readSource } from "./tests/readSource";

/*
 * Structure pins for the §11.1 spotlight (the #167 lesson: pin structure,
 * not colour values — the contrast matrix owns the values). The
 * owner-approved style is exactly two ingredients: a subtle outline, and a
 * very light dim of the surrounding content — for REGION anchors only.
 * Section anchors outline the heading block with NO surrounding dim, and a
 * live tutor session has ZERO text highlighting, ever.
 */

const plan = (shape: SpotlightPlan["shape"]): SpotlightPlan => ({
  shape,
  box: { leftPct: 10, topPct: 20, widthPct: 30, heightPct: 40 },
});

const render = (spotlight: SpotlightPlan | null): string =>
  renderToStaticMarkup(createElement(ReadingSpotlight, { plan: spotlight }));

const count = (markup: string, needle: string): number =>
  markup.split(needle).length - 1;

describe("the spotlight's §11.1 structure", () => {
  it("a region anchor gets an outline plus a four-rect surrounding dim", () => {
    const markup = render(plan("region"));
    expect(count(markup, 'data-testid="reading-spotlight-dim"')).toBe(4);
    expect(count(markup, 'data-testid="reading-spotlight-outline"')).toBe(1);
    expect(markup).toContain('data-shape="region"');
  });

  it("a section (block) anchor gets the outline only — no surrounding dim", () => {
    const markup = render(plan("block"));
    expect(count(markup, 'data-testid="reading-spotlight-dim"')).toBe(0);
    expect(count(markup, 'data-testid="reading-spotlight-outline"')).toBe(1);
    expect(markup).toContain('data-shape="block"');
  });

  it("no plan renders nothing at all", () => {
    expect(render(null)).toBe("");
  });

  it("the spotlight never paints text-highlight vocabulary — a tutor explains, it does not read", () => {
    for (const shape of ["region", "block"] as const) {
      const markup = render(plan(shape));
      expect(markup).not.toMatch(/reading-highlight/);
      expect(markup).not.toMatch(/\bdecoration-/);
    }
  });

  it("does not intercept the user's pointer — sync never fights the scroll", () => {
    expect(render(plan("region"))).toContain("pointer-events-none");
  });
});

/*
 * Teacher-mode source files must stay free of the reading/narration mode's
 * highlighting machinery (§11.1 hard rule: two visual languages, never
 * conflated). WordTimestamps are ignored, not rendered, in tutor sessions.
 */
describe("teacher-mode files never touch the follow-along highlight", () => {
  for (const name of ["ReadingSpotlight.tsx", "teacherAnchorModel.ts"]) {
    it(`${name} references no highlight token or word timing`, () => {
      const body = readSource(new URL(`./${name}`, import.meta.url));
      expect(body).not.toMatch(/reading-highlight/);
      expect(body).not.toMatch(/WordTimestamps|wordTimestamps/);
    });
  }
});

/*
 * The dim-floor pin (§11.1 "very light" dim, by measurement). The veil is
 * the page's own colour at a source-declared alpha; compositing it over any
 * content c gives fg' = a·page + (1−a)·c against the (unchanged) page
 * ground. The worst case — maximal-contrast content, pure black on the
 * light page / pure white on the dark page — must stay comfortably
 * readable through the veil, which is what makes the dim an emphasis and
 * not a curtain. Floors are the shipped measurements (3.73:1-outline's
 * sibling numbers from the U3 commit): 6.82:1 light, 7.83:1 dark — both
 * far above the 4.5:1 body tier.
 */
const CSS = readSource(new URL("./index.css", import.meta.url));

function block(open: string): string {
  const start = CSS.indexOf(open);
  expect(start, `${open} block is missing from index.css`).toBeGreaterThan(-1);
  return CSS.slice(start, CSS.indexOf("}", start));
}

function tokenIn(body: string, token: string): string {
  const match = body.match(new RegExp(`--color-${token}:\\s*(#[0-9a-f]{6})`));
  expect(match, `--color-${token} is not declared`).not.toBeNull();
  return (match as RegExpMatchArray)[1];
}

function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((at) => {
    const channel = parseInt(hex.slice(at, at + 2), 16) / 255;
    return channel <= 0.04045
      ? channel / 12.92
      : ((channel + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function ratio(fg: string, bg: string): number {
  const [hi, lo] = [luminance(fg), luminance(bg)].sort((a, b) => b - a);
  return (hi + 0.05) / (lo + 0.05);
}

function composite(alpha: number, over: string, under: string): string {
  const px = (hex: string) =>
    [1, 3, 5].map((at) => parseInt(hex.slice(at, at + 2), 16));
  const [o, u] = [px(over), px(under)];
  return `#${o
    .map((v, i) =>
      Math.round(alpha * v + (1 - alpha) * u[i])
        .toString(16)
        .padStart(2, "0"),
    )
    .join("")}`;
}

describe("the dim veil stays very light in both themes", () => {
  const source = readSource(new URL("./ReadingSpotlight.tsx", import.meta.url));
  const veil = source.match(/bg-reading-page\/(\d+)/);

  it("dims with the page's own colour at a declared alpha", () => {
    expect(veil, "the dim must be a reading-page alpha veil").not.toBeNull();
  });

  const alpha = veil ? Number(veil[1]) / 100 : Number.NaN;

  const themes = [
    { name: "light", body: block("@theme"), extreme: "#000000", floor: 6.63 },
    {
      name: "dark",
      body: block(':root[data-theme="dark"]'),
      extreme: "#ffffff",
      floor: 7.83,
    },
  ];

  for (const { name, body, extreme, floor } of themes) {
    it(`in ${name}: maximal-contrast content keeps ≥${floor}:1 through the veil`, () => {
      const page = tokenIn(body, "reading-page");
      const veiled = composite(alpha, page, extreme);
      expect(ratio(veiled, page)).toBeGreaterThanOrEqual(floor);
    });
  }
});
