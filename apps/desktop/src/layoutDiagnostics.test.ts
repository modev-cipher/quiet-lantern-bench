import { describe, expect, it } from "vitest";

import {
  countGridColumns,
  firstUnmatchedProbe,
  formatLayoutSample,
  type LayoutSample,
} from "./layoutDiagnostics";

function sample(overrides: Partial<LayoutSample> = {}): LayoutSample {
  return {
    innerWidth: 1362,
    clientWidth: 1363,
    containerWidth: 1138,
    devicePixelRatio: 1.4,
    rootFontSizePx: "16px",
    breakpointLg: "1024px",
    media: {
      remRange: [true, true, false, false],
      remMinWidth: [true, true, true, true],
      pxRange: [true, true, true, true],
      pxMinWidth: [true, true, true, true],
    },
    widthProbes: [true, true, true, true, true],
    gridColumns: 5,
    gridTemplate: "198.973px 198.973px 198.973px 198.973px 198.973px",
    ...overrides,
  };
}

describe("countGridColumns", () => {
  it("counts the tracks in a computed template", () => {
    expect(countGridColumns("201px 201px 201px 201px 201px")).toBe(5);
    expect(countGridColumns("480px 480px 480px")).toBe(3);
  });

  it("returns null when there is no grid to measure", () => {
    expect(countGridColumns(null)).toBeNull();
    expect(countGridColumns("none")).toBeNull();
    expect(countGridColumns("   ")).toBeNull();
  });
});

describe("firstUnmatchedProbe", () => {
  it("names the width where matching stops", () => {
    expect(firstUnmatchedProbe([true, false, false, false, false])).toBe(974);
    expect(firstUnmatchedProbe([true, true, true, false, false])).toBe(1280);
  });

  it("returns null when every probe matches", () => {
    expect(firstUnmatchedProbe([true, true, true, true, true])).toBeNull();
  });
});

describe("formatLayoutSample", () => {
  it("reports the geometry needed to size a container-driven grid", () => {
    const line = formatLayoutSample(sample());

    expect(line).toContain("innerWidth=1362");
    expect(line).toContain("containerWidth=1138");
    expect(line).toContain("dpr=1.4");
    expect(line).toContain("rootFontSize=16px");
    expect(line).toContain("gridColumns=5");
    expect(line).toContain("breakpointLg=1024px");
  });

  it("separates a scaled rem basis from a scaled query viewport", () => {
    const scaledRem = formatLayoutSample(sample());

    expect(scaledRem).toContain("remRange=[40rem,48rem]");
    expect(scaledRem).toContain("pxRange=[640px,768px,1024px,1280px]");
    expect(scaledRem).toContain("firstUnmatchedProbe=none");

    const scaledViewport = formatLayoutSample(
      sample({
        media: {
          remRange: [true, true, false, false],
          remMinWidth: [true, true, false, false],
          pxRange: [true, true, false, false],
          pxMinWidth: [true, true, false, false],
        },
        widthProbes: [true, false, false, false, false],
      }),
    );

    expect(scaledViewport).toContain("pxRange=[640px,768px]");
    expect(scaledViewport).toContain("firstUnmatchedProbe=974px");
  });

  it("exposes a disagreement between range and min-width syntax", () => {
    const line = formatLayoutSample(sample());

    expect(line).toContain("remRange=[40rem,48rem]");
    expect(line).toContain("remMin=[40rem,48rem,64rem,80rem]");
  });

  it("says n/a rather than guessing when the grid is absent", () => {
    const line = formatLayoutSample(
      sample({ gridColumns: null, gridTemplate: null, containerWidth: null }),
    );

    expect(line).toContain("gridColumns=n/a");
    expect(line).toContain("gridTemplate=n/a");
    expect(line).toContain("containerWidth=n/a");
  });

  it("reports no matches without collapsing the field", () => {
    const line = formatLayoutSample(
      sample({
        media: {
          remRange: [false, false, false, false],
          remMinWidth: [false, false, false, false],
          pxRange: [false, false, false, false],
          pxMinWidth: [false, false, false, false],
        },
      }),
    );

    expect(line).toContain("remRange=[none]");
    expect(line).toContain("pxMin=[none]");
  });
});
