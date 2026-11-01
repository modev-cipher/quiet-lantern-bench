import { describe, expect, it } from "vitest";

import { menuPlacement } from "./menuPlacement";

const viewport = { width: 1280, height: 800 };
const menu = { width: 220, height: 200 };

describe("menuPlacement", () => {
  it("opens below the trigger, aligned to its end edge, when there is room", () => {
    const trigger = { top: 300, bottom: 328, left: 600, right: 628 };
    expect(menuPlacement(trigger, menu, viewport, "ltr")).toEqual({
      top: 334,
      left: 408,
      side: "below",
    });
  });

  it("flips above the trigger near the bottom of the window", () => {
    const trigger = { top: 700, bottom: 728, left: 600, right: 628 };
    expect(menuPlacement(trigger, menu, viewport, "ltr")).toMatchObject({
      top: 494,
      side: "above",
    });
  });

  it("aligns to the trigger's start edge in right-to-left layouts", () => {
    const trigger = { top: 300, bottom: 328, left: 600, right: 628 };
    expect(menuPlacement(trigger, menu, viewport, "rtl").left).toBe(600);
  });

  it("never leaves the window horizontally", () => {
    const nearStart = { top: 300, bottom: 328, left: 10, right: 38 };
    expect(menuPlacement(nearStart, menu, viewport, "ltr").left).toBe(8);
    const nearEnd = { top: 300, bottom: 328, left: 1260, right: 1288 };
    expect(menuPlacement(nearEnd, menu, viewport, "rtl").left).toBe(1052);
  });
});
