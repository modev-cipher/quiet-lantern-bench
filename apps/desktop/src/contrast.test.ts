import { readdirSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

/*
 * The WCAG gate over the theme's token definitions. Contrast is
 * deterministic math (WCAG 2.2, SC 1.4.3 Contrast (Minimum) and SC 1.4.11
 * Non-text Contrast): every fg/bg pairing the design system produces is
 * enumerated here and computed for BOTH palettes. Darken one ink token and
 * this file goes red before any eye has to notice.
 *
 * Tiers:
 *   BODY  4.5:1 — normal-size text (SC 1.4.3)
 *   UI    3.0:1 — large/semibold labels, icons, focus rings, meaningful
 *                 non-text boundaries (SC 1.4.3 large text, SC 1.4.11)
 */
const BODY = 4.5;
const UI = 3.0;

const CSS = readSource(new URL("./index.css", import.meta.url));

// The typographic covers keep their palette in both themes: they are objects
// (book covers), not surfaces, and were dark-ground/cream-fg from the start.
const THEME_INVARIANT_FAMILIES = ["paper"];

function block(open: string): string {
  const start = CSS.indexOf(open);
  expect(start, `${open} block is missing from index.css`).toBeGreaterThan(-1);
  return CSS.slice(start, CSS.indexOf("}", start));
}

function tokensIn(body: string): Map<string, string> {
  const tokens = new Map<string, string>();
  for (const match of body.matchAll(
    /--color-([a-z0-9-]+):\s*(#[0-9a-f]{6})/g,
  )) {
    tokens.set(match[1], match[2]);
  }
  return tokens;
}

const lightTokens = tokensIn(block("@theme"));
const darkOverrides = tokensIn(block(':root[data-theme="dark"]'));
const darkTokens = new Map([...lightTokens, ...darkOverrides]);

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

const SURFACES = ["ink-950", "ink-900", "ink-850", "ink-800", "surface"] as const;

interface Pairing {
  fg: string;
  bg: string;
  min: number;
  minPerTheme?: { light: number; dark: number };
  why: string;
}

const pairings: Pairing[] = [];

// Body text inks on every app surface they sit on.
for (const fg of [
  "ink-500",
  "ink-400",
  "ink-300",
  "ink-200",
  "ink-150",
  "ink-100",
  "ink-50",
]) {
  for (const bg of SURFACES) {
    pairings.push({ fg, bg, min: BODY, why: "text ink on app surface" });
  }
}

// Inverted primary chips: bg-ink-50/150 with text-ink-950.
pairings.push(
  { fg: "ink-950", bg: "ink-50", min: BODY, why: "inverted primary button" },
  { fg: "ink-950", bg: "ink-150", min: BODY, why: "inverted button hover" },
);

// Accent text. ember-400 doubles as the active-nav icon tint on ink-800
// chips (icon tier); as running text it only sits on canvas and cards.
for (const fg of ["ember-100", "ember-200", "ember-300"]) {
  for (const bg of SURFACES) {
    pairings.push({ fg, bg, min: BODY, why: "accent text on app surface" });
  }
}
for (const bg of ["ink-950", "ink-900"] as const) {
  pairings.push({ fg: "ember-400", bg, min: BODY, why: "accent text" });
}
for (const bg of ["ink-850", "ink-800"] as const) {
  pairings.push({ fg: "ember-400", bg, min: UI, why: "accent icon on chip" });
}

// Solid accent buttons carry ink-950 labels (semibold label tier).
for (const bg of ["ember-600", "ember-500", "ember-400"] as const) {
  pairings.push({ fg: "ink-950", bg, min: UI, why: "label on solid accent" });
}
for (const bg of ["ember-600", "ember-500"] as const) {
  pairings.push({
    fg: "on-accent",
    bg,
    min: BODY,
    why: "white label on a solid accent fill and its hover",
  });
}

// Washes: their own ink, and the same ink used as plain status text on cards.
for (const wash of ["green", "red", "blue", "yellow"] as const) {
  pairings.push({
    fg: `wash-${wash}-ink`,
    bg: `wash-${wash}`,
    min: BODY,
    why: "banner ink on its wash",
  });
  for (const bg of SURFACES) {
    pairings.push({
      fg: `wash-${wash}-ink`,
      bg,
      min: BODY,
      why: "wash ink as status text on app surface",
    });
  }
}

// Typographic covers (theme-invariant, verified once per theme anyway).
for (const tone of ["a", "b", "c", "d", "e"] as const) {
  pairings.push(
    {
      fg: "paper-fg",
      bg: `paper-tone-${tone}`,
      min: BODY,
      why: "cover title on tone",
    },
    {
      fg: "paper-fg",
      bg: `paper-base-${tone}`,
      min: BODY,
      why: "cover title on base",
    },
    {
      fg: "paper-fg-muted",
      bg: `paper-base-${tone}`,
      min: BODY,
      why: "cover subtitle on base",
    },
  );
}

// The reading surface — the follow-along-critical set.
pairings.push(
  {
    fg: "reading-highlight-fg",
    bg: "reading-highlight",
    min: BODY,
    why: "the spoken word on its highlight",
  },
  {
    // The moving highlight block against the page. In dark this carries the
    // follow-along; in light #167 differentiates by hue at 1.82 and the
    // floor only forbids regressing below that shipped value.
    fg: "reading-highlight",
    bg: "reading-page",
    min: UI,
    minPerTheme: { light: 1.8, dark: UI },
    why: "highlight block vs page",
  },
  {
    fg: "reading-underline",
    bg: "reading-underline-track",
    min: UI,
    why: "progress fill vs track",
  },
  {
    fg: "reading-underline",
    bg: "reading-page",
    min: UI,
    why: "progress bar vs page",
  },
  { fg: "reading-stale", bg: "reading-page", min: BODY, why: "stale advisory" },
  {
    fg: "reading-stale",
    bg: "ink-950",
    min: BODY,
    why: "stale advisory on canvas",
  },
  {
    fg: "reading-frame",
    bg: "reading-page",
    min: UI,
    why: "figure frame vs page",
  },
  { fg: "ink-100", bg: "reading-page", min: BODY, why: "script body on page" },
  {
    fg: "ink-50",
    bg: "reading-page",
    min: BODY,
    why: "script heading on page",
  },
  {
    // Successor to reading-fade (#195): the fallback sentence is the ONLY
    // rendering of its text — the reader must actually read it, so it is
    // quieter than body ink but still clears the large-text tier (its 24px
    // serif qualifies as large under SC 1.4.3).
    fg: "reading-fallback",
    bg: "reading-page",
    min: UI,
    why: "fallback sentence shown without word timings",
  },
);

// The listening surface (MiniPlayer / Now Playing) lives on the invariant
// dark-paper object family in both themes (owner ruling H15); its accents
// must clear their tiers against that ground.
pairings.push(
  {
    fg: "ember-500",
    bg: "paper-base-e",
    min: UI,
    why: "listening-surface accent fill and icons",
  },
  {
    fg: "ember-400",
    bg: "paper-base-e",
    min: UI,
    why: "listening-surface accent hover",
  },
  {
    fg: "reading-highlight",
    bg: "paper-base-e",
    min: BODY,
    why: "listening-surface accent text and player notices",
  },
);

// Focus rings must survive every surface (SC 1.4.11).
for (const bg of SURFACES) {
  pairings.push({ fg: "ink-300", bg, min: UI, why: "focus ring on surface" });
}

const themes = [
  ["light", lightTokens],
  ["dark", darkTokens],
] as const;

describe("every colour pairing the design system produces clears its WCAG tier", () => {
  for (const [name, tokens] of themes) {
    it(`in the ${name} theme`, () => {
      const failures: string[] = [];
      for (const { fg, bg, min, minPerTheme, why } of pairings) {
        const fgHex = tokens.get(fg);
        const bgHex = tokens.get(bg);
        expect(fgHex, `--color-${fg} is not defined`).toBeDefined();
        expect(bgHex, `--color-${bg} is not defined`).toBeDefined();
        const floor = minPerTheme?.[name] ?? min;
        const measured = ratio(fgHex as string, bgHex as string);
        if (measured < floor) {
          failures.push(
            `${fg} on ${bg} (${why}): ${measured.toFixed(2)} < ${floor}`,
          );
        }
      }
      expect(
        failures,
        `${failures.length} pairing(s) regressed below their tier:\n  ${failures.join("\n  ")}`,
      ).toEqual([]);
    });
  }

  it("checks a matrix large enough to mean something", () => {
    expect(pairings.length).toBeGreaterThanOrEqual(80);
  });
});

/*
 * #190: the gate-escape class. A token added to BOTH palettes (or an existing
 * token put to a new use) never used to touch this file — the light-only
 * guard, the ramp order and the pairing floor all ran green around it. The
 * assertion below ties usage to the matrix: every colour token the app
 * references must either participate in a verified pairing or carry an
 * explicit decorative exemption. Adding a token to the palettes and using it
 * without deciding which of those it is goes red here.
 */
const MATRIX_EXEMPT: Record<string, string> = {
  "ink-700":
    "hairline borders, kbd frames and scrollbar thumbs — sub-3:1 by design, never the sole carrier of meaning (SC 1.4.11 decorative)",
  "ink-600":
    "hover state of the same hairlines and scrollbar thumbs — decorative like ink-700",
};

// Utilities that put a token behind or into TEXT or meaningful graphics can
// never be decorative; an exempt token must not appear in them.
const TEXTUAL_UTILITIES = ["text", "fill", "stroke"] as const;

const TOKEN_USAGE =
  /\b(?:bg|text|border|ring|outline|decoration|fill|stroke|from|via|to|accent|caret|divide|shadow)-((?:ink|ember|wash|paper|reading)-[a-z0-9]+(?:-[a-z0-9]+)*)/g;

function usedTokens(): Map<string, Set<string>> {
  const used = new Map<string, Set<string>>();
  const record = (token: string, where: string) => {
    if (!used.has(token)) used.set(token, new Set());
    (used.get(token) as Set<string>).add(where);
  };
  const dir = new URL(".", import.meta.url);
  for (const name of readdirSync(dir)) {
    if (!/\.tsx?$/.test(name) || /\.test\.tsx?$/.test(name)) continue;
    if (name.endsWith(".d.ts")) continue;
    const body = readSource(new URL(`./${name}`, import.meta.url));
    for (const match of body.matchAll(TOKEN_USAGE)) {
      record(match[1], `${name}: ${match[0]}`);
    }
  }
  for (const match of CSS.matchAll(/var\(--color-([a-z0-9-]+)\)/g)) {
    record(match[1], `index.css: var(--color-${match[1]})`);
  }
  return used;
}

describe("every colour token the app references participates in the matrix", () => {
  const matrix = new Set(pairings.flatMap(({ fg, bg }) => [fg, bg]));
  const used = usedTokens();

  it("scans enough usage to mean something", () => {
    expect(used.size).toBeGreaterThanOrEqual(40);
  });

  it("finds each used token in a verified pairing or an annotated exemption", () => {
    const escapes = [...used.entries()]
      .filter(([token]) => !matrix.has(token) && !(token in MATRIX_EXEMPT))
      .map(([token, sites]) => `${token} (${[...sites][0]})`);
    expect(
      escapes,
      `these tokens are painted but never contrast-checked — add a pairing ` +
        `above or an annotated MATRIX_EXEMPT entry (#190):\n  ${escapes.join("\n  ")}`,
    ).toEqual([]);
  });

  it("keeps every exemption both real and decorative", () => {
    for (const token of Object.keys(MATRIX_EXEMPT)) {
      expect(
        lightTokens.has(token),
        `MATRIX_EXEMPT lists ${token}, which @theme does not declare`,
      ).toBe(true);
      expect(
        used.has(token),
        `MATRIX_EXEMPT lists ${token}, which nothing uses — drop the entry`,
      ).toBe(true);
      const textual = [...(used.get(token) ?? [])].filter((site) =>
        TEXTUAL_UTILITIES.some((utility) => site.includes(` ${utility}-`)),
      );
      expect(
        textual,
        `${token} is exempt as decorative but paints text or icons: ${textual.join(", ")}`,
      ).toEqual([]);
    }
  });
});

describe("the dark palette is complete", () => {
  it("overrides every non-invariant colour token @theme declares", () => {
    const missing = [...lightTokens.keys()].filter(
      (token) =>
        !darkOverrides.has(token) &&
        !THEME_INVARIANT_FAMILIES.includes(token.split("-")[0]),
    );
    expect(
      missing,
      `these tokens keep their light value in dark mode: ${missing.join(", ")}`,
    ).toEqual([]);
  });

  it("declares no token @theme does not know", () => {
    const stray = [...darkOverrides.keys()].filter(
      (token) => !lightTokens.has(token),
    );
    expect(
      stray,
      `dark-only tokens would emit no utility: ${stray.join(", ")}`,
    ).toEqual([]);
  });
});

describe("the ink ramp keeps its order", () => {
  const RAMP = [
    "ink-950",
    "ink-900",
    "ink-850",
    "ink-800",
    "ink-700",
    "ink-600",
    "ink-500",
    "ink-400",
    "ink-300",
    "ink-200",
    "ink-150",
    "ink-100",
    "ink-50",
  ];

  it("descends in luminance from canvas to heading in light", () => {
    // #167 ships ink-150 (#333333, neutral) and ink-100 (#2f3437, cool) as a
    // near-tie with distinct hue roles; the ramp may tie within a whisker but
    // must never visibly invert.
    const epsilon = 0.005;
    const values = RAMP.map((token) =>
      luminance(lightTokens.get(token) as string),
    );
    for (let i = 1; i < values.length; i += 1) {
      expect(
        values[i],
        `${RAMP[i]} should not be lighter than ${RAMP[i - 1]} in light`,
      ).toBeLessThan(values[i - 1] + epsilon);
    }
  });

  it("ascends in luminance from canvas to heading in dark", () => {
    const values = RAMP.map((token) =>
      luminance(darkTokens.get(token) as string),
    );
    for (let i = 1; i < values.length; i += 1) {
      expect(
        values[i],
        `${RAMP[i]} should be lighter than ${RAMP[i - 1]} in dark`,
      ).toBeGreaterThan(values[i - 1]);
    }
  });
});
