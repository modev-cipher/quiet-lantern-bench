import { describe, expect, it } from "vitest";

import { enterTransform, type ThresholdRect } from "./thresholdModel";

const VIEWPORT = { width: 1920, height: 1080 };
const THUMB: ThresholdRect = { x: 100, y: 900, width: 44, height: 44 };

describe("the threshold maps the cover you touched onto the window", () => {
  it("places the clone exactly over the thumbnail before it grows", () => {
    expect(enterTransform(THUMB, VIEWPORT)).toBe(
      "translate(100px, 900px) scale(0.0229, 0.0407)",
    );
  });

  it("animates transform only, so it never costs a layout pass", () => {
    const transform = enterTransform(THUMB, VIEWPORT) ?? "";
    expect(transform).toMatch(/^translate\([^)]*\) scale\([^)]*\)$/);
    expect(transform).not.toMatch(/width|height|top|left/);
  });

  it("declines a source that is already the window, because there is no journey", () => {
    expect(enterTransform({ x: 0, y: 0, ...VIEWPORT }, VIEWPORT)).toBeNull();
    expect(
      enterTransform({ x: 0, y: 0, width: 1920, height: 40 }, VIEWPORT),
      "a source as wide as the window has nothing to grow into horizontally",
    ).toBeNull();
  });

  it("declines a source that cannot be measured", () => {
    expect(enterTransform({ x: 0, y: 0, width: 0, height: 44 }, VIEWPORT)).toBeNull();
    expect(
      enterTransform({ x: Number.NaN, y: 0, width: 44, height: 44 }, VIEWPORT),
    ).toBeNull();
    expect(
      enterTransform(THUMB, { width: 0, height: 0 }),
      "a viewport with no size would divide by zero into an infinite scale",
    ).toBeNull();
  });
});
