import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const SOURCE = readSource(new URL("./Library.tsx", import.meta.url));
const CSS = readSource(new URL("./index.css", import.meta.url));
const SHELF_GRID = CSS.slice(
  CSS.indexOf(".shelf-grid {"),
  CSS.indexOf("\n}", CSS.indexOf(".shelf-grid {")),
);

const GRID_CLASSNAMES = [
  ...SOURCE.matchAll(/className="([^"]*\bgrid\b[^"]*)"/g),
].map((match) => match[1]);

const RESPONSIVE_VARIANT = /\b(sm|md|lg|xl|2xl):/;

describe("library grids are sized by their container, not by media queries", () => {
  it("finds both card grids", () => {
    expect(GRID_CLASSNAMES.length).toBeGreaterThanOrEqual(2);
  });

  it("uses auto-fill tracks so the column count follows the container width", () => {
    const fluid = GRID_CLASSNAMES.filter((name) =>
      name.includes("grid-cols-[repeat(auto-fill"),
    );

    expect(fluid).toHaveLength(3);
    for (const name of fluid) {
      expect(
        name,
        "the documented track (PIPELINE_SPEC §4, TASKS R-Y): 180px, so a card and its actions fit the default window",
      ).toContain("minmax(min(180px,100%),1fr)");
    }
  });

  it("sizes the book shelf by its container too: auto-fill tracks on the documented 180px pitch", () => {
    expect(SOURCE).toContain('className="shelf-grid"');
    expect(SHELF_GRID).toContain(
      "grid-template-columns: repeat(auto-fill, var(--shelf-col));",
    );
    expect(SHELF_GRID).toContain("column-gap: 14px;");
    expect(CSS).toContain("--shelf-col: 166px;");
    expect(SHELF_GRID).not.toContain("@media");
  });

  it("declares no responsive breakpoint variant on any grid", () => {
    for (const name of GRID_CLASSNAMES) {
      expect(
        RESPONSIVE_VARIANT.test(name),
        `grid className must not depend on a media query: "${name}"`,
      ).toBe(false);
    }
  });

  it("keeps a fixed grid-cols utility off the card grids", () => {
    for (const name of GRID_CLASSNAMES) {
      expect(/\bgrid-cols-\d/.test(name)).toBe(false);
    }
  });
});
