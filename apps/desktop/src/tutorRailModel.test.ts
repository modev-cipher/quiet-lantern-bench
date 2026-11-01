import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  RAIL_DEFAULT_PX,
  RAIL_MIN_PX,
  TUTOR_RAIL_WIDTH_KEY,
  clampRailWidth,
  loadRailWidth,
  railMaxPx,
  railWidthFromPointer,
  resetRailWidth,
  storeRailWidth,
} from "./tutorRailModel";

function fakeStorage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() {
      return values.size;
    },
    clear() {
      values.clear();
    },
    key(index: number) {
      return [...values.keys()][index] ?? null;
    },
    getItem(key: string) {
      return values.get(key) ?? null;
    },
    removeItem(key: string) {
      values.delete(key);
    },
    setItem(key: string, next: string) {
      values.set(key, next);
    },
  };
}

function withStorage(storage: Storage | undefined): void {
  Object.defineProperty(globalThis, "localStorage", {
    value: storage,
    configurable: true,
    writable: true,
  });
}

beforeEach(() => {
  withStorage(fakeStorage());
});

afterEach(() => {
  withStorage(undefined);
});

describe("clampRailWidth", () => {
  it("holds the floor and the viewport-share ceiling", () => {
    expect(clampRailWidth(100, 1920)).toBe(RAIL_MIN_PX);
    expect(clampRailWidth(5000, 1920)).toBe(Math.round(1920 * 0.55));
    expect(clampRailWidth(400, 1920)).toBe(400);
  });

  it("never inverts the range on a tiny viewport", () => {
    // 55% of 320px is under the 280px floor — the floor wins both ends.
    expect(railMaxPx(320)).toBe(RAIL_MIN_PX);
    expect(clampRailWidth(500, 320)).toBe(RAIL_MIN_PX);
  });

  it("rounds and rejects non-finite input", () => {
    expect(clampRailWidth(300.6, 1920)).toBe(301);
    expect(clampRailWidth(Number.NaN, 1920)).toBe(RAIL_DEFAULT_PX);
    expect(clampRailWidth(Number.POSITIVE_INFINITY, 1920)).toBe(
      RAIL_DEFAULT_PX,
    );
  });
});

describe("railWidthFromPointer", () => {
  const rect = { left: 100, right: 400 };

  it("LTR: the rail docks right, so the width is the pointer's distance from the right edge", () => {
    expect(railWidthFromPointer(80, rect, "ltr")).toBe(320);
  });

  it("RTL mirrors: the rail docks left and grows toward the end of the screen", () => {
    expect(railWidthFromPointer(420, rect, "rtl")).toBe(320);
  });
});

describe("persistence", () => {
  it("round-trips through localStorage under the txtcaster key", () => {
    storeRailWidth(342);
    expect(localStorage.getItem(TUTOR_RAIL_WIDTH_KEY)).toBe("342");
    expect(loadRailWidth(1920)).toBe(342);
  });

  it("defaults when nothing is stored and clamps a stale value on load", () => {
    expect(loadRailWidth(1920)).toBe(RAIL_DEFAULT_PX);
    storeRailWidth(2000);
    // the window shrank since the value was stored
    expect(loadRailWidth(800)).toBe(Math.round(800 * 0.55));
  });

  it("ignores garbage and reset removes the key", () => {
    localStorage.setItem(TUTOR_RAIL_WIDTH_KEY, "not-a-number");
    expect(loadRailWidth(1920)).toBe(RAIL_DEFAULT_PX);
    storeRailWidth(300);
    resetRailWidth();
    expect(localStorage.getItem(TUTOR_RAIL_WIDTH_KEY)).toBeNull();
    expect(loadRailWidth(1920)).toBe(RAIL_DEFAULT_PX);
  });
});
