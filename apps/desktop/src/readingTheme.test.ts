import { readdirSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const CSS = readSource(new URL("./index.css", import.meta.url));

const READING_TOKENS = [
  "highlight",
  "highlight-fg",
  "underline",
  "underline-track",
  "fallback",
  "stale",
  "page",
  "frame",
] as const;

const READING_CORE = [
  "ReadingView.tsx",
  "ReadingScript.tsx",
  "ReadingFacsimile.tsx",
  "ReadingFallback.tsx",
  "ReadingHeader.tsx",
  "readingClock.ts",
  "readingHighlight.ts",
] as const;

const READING_FILE = /^[Rr]eading[A-Za-z]*\.tsx?$/;

const ALLOWED_COLOUR_FAMILIES = new Set([
  "ink",
  "ember",
  "paper",
  "reading",
  "transparent",
  "current",
  "inherit",
]);

const COLOUR_UTILITY =
  /\b(?:bg|text|border|ring|outline|decoration|fill|stroke|from|via|to|accent|caret|divide|shadow)-([a-z]+)-(\d{2,3})\b/g;

const TOKEN_REFERENCE =
  /\b(?:bg|text|border|ring|outline|decoration|fill|stroke|from|via|to|accent|caret|divide|shadow)-([a-z][a-z0-9]*(?:-[a-z0-9]+)*)/g;

const HEX_LITERAL = /#[0-9a-fA-F]{3,8}\b/;
const FUNCTIONAL_COLOUR = /(?<![a-zA-Z])(?:rgba?|hsla?|oklch|color-mix)\(/;

function themeBlock(): string {
  const start = CSS.indexOf("@theme");
  const end = CSS.indexOf("}", start);
  return CSS.slice(start, end);
}

function declaredTokens(): string[] {
  return [...themeBlock().matchAll(/--color-reading-([a-z-]+):/g)].map(
    (match) => match[1],
  );
}

function declaredColours(): Set<string> {
  return new Set(
    [...themeBlock().matchAll(/--color-([a-z0-9-]+):/g)].map(
      (match) => match[1],
    ),
  );
}

function projectFamilies(): Set<string> {
  return new Set(
    [...declaredColours()].map((token) => token.slice(0, token.indexOf("-"))),
  );
}

function undeclaredTokensIn(
  sources: Array<{ name: string; body: string }>,
): Array<{ name: string; utility: string; token: string }> {
  const declared = declaredColours();
  const families = projectFamilies();
  const found: Array<{ name: string; utility: string; token: string }> = [];

  for (const { name, body } of sources) {
    for (const match of body.matchAll(TOKEN_REFERENCE)) {
      const token = match[1];
      const family = token.slice(0, token.indexOf("-") + 1 || undefined);
      if (!families.has(family.replace(/-$/, ""))) continue;
      if (declared.has(token)) continue;
      found.push({ name, utility: match[0], token });
    }
  }
  return found;
}

// Flat scan of src/ only. If the tree ever gains subdirectories this must
// recurse, or new files leave the guard silently.
function projectSources(): Array<{ name: string; body: string }> {
  return readdirSync(new URL(".", import.meta.url))
    .filter(
      (name) =>
        /\.tsx?$/.test(name) &&
        !name.endsWith(".test.ts") &&
        !name.endsWith(".test.tsx") &&
        !name.endsWith(".d.ts"),
    )
    .sort()
    .map((name) => ({
      name,
      body: readSource(new URL(`./${name}`, import.meta.url)),
    }));
}

function readingFileNames(): string[] {
  return readdirSync(new URL(".", import.meta.url))
    .filter(
      (name) =>
        READING_FILE.test(name) &&
        !name.endsWith(".test.ts") &&
        !name.endsWith(".test.tsx"),
    )
    .sort();
}

function readingSources(): Array<{ name: string; body: string }> {
  return readingFileNames().map((name) => ({
    name,
    body: readSource(new URL(`./${name}`, import.meta.url)),
  }));
}

function referencedTokens(): Set<string> {
  const referenced = new Set<string>();
  for (const { body } of readingSources()) {
    for (const match of body.matchAll(/--color-reading-([a-z-]+)/g)) {
      referenced.add(match[1]);
    }
    for (const match of body.matchAll(
      /(?:bg|text|border|ring|outline|decoration|fill|stroke|shadow)-reading-([a-z][a-z-]*)/g,
    )) {
      referenced.add(match[1]);
    }
  }
  return referenced;
}

describe("reading theme tokens", () => {
  it("declares every token the reading surface is specified to use", () => {
    const declared = declaredTokens();

    for (const token of READING_TOKENS) {
      expect(declared, `--color-reading-${token} is missing from @theme`).toContain(
        token,
      );
    }
  });

  it("declares them inside @theme so Tailwind emits the utilities", () => {
    for (const token of READING_TOKENS) {
      expect(themeBlock()).toContain(`--color-reading-${token}:`);
    }
  });

  it("keeps the underline fill out of the theme, because it is per-tick state", () => {
    expect(
      themeBlock(),
      "--reading-fill is written from the clock every frame; a theme token is a constant",
    ).not.toContain("--reading-fill");
  });

  it("resolves every declared token to a literal from the ink or ember ramp", () => {
    for (const token of READING_TOKENS) {
      const value = new RegExp(`--color-reading-${token}:\\s*([^;]+);`).exec(CSS);

      expect(value, `--color-reading-${token} has no value`).not.toBeNull();
      expect(
        (value as RegExpExecArray)[1].trim(),
        "a var() reference here breaks Tailwind's opacity modifiers",
      ).toMatch(/^#[0-9a-f]{6}$/);
    }
  });
});

describe("reading components carry no colour of their own", () => {
  it("uses no hex or functional colour literal", () => {
    for (const { name, body } of readingSources()) {
      expect(body, `${name} hardcodes a colour instead of a token`).not.toMatch(
        HEX_LITERAL,
      );
      expect(body, `${name} hardcodes a colour instead of a token`).not.toMatch(
        FUNCTIONAL_COLOUR,
      );
    }
  });

  it("uses no Tailwind colour family outside the theme's own", () => {
    for (const { name, body } of readingSources()) {
      for (const match of body.matchAll(COLOUR_UTILITY)) {
        expect(
          ALLOWED_COLOUR_FAMILIES.has(match[1]),
          `${name} uses ${match[0]}, which is a Tailwind default palette colour`,
        ).toBe(true);
      }
    }
  });

  it("resolves every colour utility it writes to a token @theme declares", () => {
    for (const miss of undeclaredTokensIn(readingSources())) {
      expect(
        false,
        `${miss.name} writes ${miss.utility}, but --color-${miss.token} is not declared in @theme, so Tailwind emits no rule and the element silently keeps its inherited colour`,
      ).toBe(true);
    }
  });

  it("never writes a colour token from JavaScript", () => {
    for (const { name, body } of readingSources()) {
      expect(
        body,
        `${name} assigns a colour token at runtime; colour is theme state, fill is clock state`,
      ).not.toMatch(/setProperty\(\s*["'`]--color-reading-/);
    }
  });

  it("enumerates the reading surface instead of listing it, so a new file cannot escape", () => {
    const found = readingFileNames();

    for (const name of READING_CORE) {
      expect(
        found,
        `${name} is part of the reading surface and the scan no longer sees it`,
      ).toContain(name);
    }
    expect(
      found.length,
      "a glob that matches nothing makes every scan above vacuous",
    ).toBeGreaterThanOrEqual(READING_CORE.length);
  });

  it("references every token it declares", () => {
    const referenced = referencedTokens();

    for (const token of READING_TOKENS) {
      expect(
        referenced,
        `--color-reading-${token} is declared but nothing uses it`,
      ).toContain(token);
    }
  });
});

describe("the app's own colour families resolve", () => {
  it("declares every shade the whole app references from a family it owns", () => {
    const families = projectFamilies();
    const sources = projectSources();

    expect(
      sources.length,
      "a glob that matches nothing makes this scan vacuous",
    ).toBeGreaterThan(READING_CORE.length);
    expect(families, "the theme declares no ember ramp to check").toContain(
      "ember",
    );

    for (const miss of undeclaredTokensIn(sources)) {
      expect(
        false,
        `${miss.name} writes ${miss.utility}, and ${miss.token.split("-")[0]} is a family this theme owns, so Tailwind has no default to fall back on and emits nothing`,
      ).toBe(true);
    }
  });
});

const TAILWIND_SHADOW_STEPS = new Set([
  "2xs",
  "xs",
  "sm",
  "md",
  "lg",
  "xl",
  "2xl",
  "none",
  "inner",
  "initial",
]);

const TAILWIND_WIDTH_STEPS = new Set([
  "3xs",
  "2xs",
  "xs",
  "sm",
  "md",
  "lg",
  "xl",
  "2xl",
  "3xl",
  "4xl",
  "5xl",
  "6xl",
  "7xl",
  "none",
  "full",
  "min",
  "max",
  "fit",
  "prose",
]);

const NAMESPACES = [
  {
    theme: "shadow",
    utility: /\bshadow-([a-z][a-z0-9-]*)/g,
    builtIn: TAILWIND_SHADOW_STEPS,
  },
  {
    theme: "container",
    utility: /\bmax-w-([a-z][a-z0-9-]*)/g,
    builtIn: TAILWIND_WIDTH_STEPS,
  },
] as const;

function declaredIn(namespace: string): Set<string> {
  return new Set(
    [...themeBlock().matchAll(new RegExp(`--${namespace}-([a-z0-9-]+):`, "g"))].map(
      (match) => match[1],
    ),
  );
}

describe("the theme's own non-colour namespaces resolve too", () => {
  it("declares every custom shadow and measure the app writes", () => {
    const families = projectFamilies();
    const sources = projectSources();

    expect(sources.length).toBeGreaterThan(READING_CORE.length);

    for (const { theme, utility, builtIn } of NAMESPACES) {
      const declared = declaredIn(theme);
      let checked = 0;

      for (const { name, body } of sources) {
        for (const match of body.matchAll(utility)) {
          const token = match[1];
          if (builtIn.has(token)) continue;
          if (families.has(token.split("-")[0]!)) continue;
          checked += 1;

          expect(
            declared.has(token),
            `${name} writes ${match[0]}, but --${theme}-${token} is not declared in @theme, so the utility emits nothing and the surface silently loses it`,
          ).toBe(true);
        }
      }

      expect(
        checked,
        `no custom --${theme}-* utility was scanned, so this assertion proves nothing`,
      ).toBeGreaterThan(0);
    }
  });
});
