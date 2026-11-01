import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  FLOAT_MARGIN_PX,
  TUTOR_DOCK_KEY,
  clampFloat,
  defaultFloat,
  floatHeight,
  loadDock,
  storeDock,
} from "./tutorDockModel";

function fakeStorage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() {
      return values.size;
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (i) => [...values.keys()][i] ?? null,
    removeItem: (key) => void values.delete(key),
    setItem: (key, value) => void values.set(key, String(value)),
  };
}

const VIEW = { width: 1440, height: 900 };

beforeEach(() => vi.stubGlobal("localStorage", fakeStorage()));
afterEach(() => vi.unstubAllGlobals());

describe("floatHeight", () => {
  it("leaves room above and below and never collapses", () => {
    expect(floatHeight(900)).toBe(560);
    expect(floatHeight(700)).toBe(700 - 72 - 176 - FLOAT_MARGIN_PX);
    expect(floatHeight(200)).toBeGreaterThanOrEqual(160);
  });
});

describe("defaultFloat", () => {
  it("LTR: rests at the top-right under the header, clear of the player", () => {
    const f = defaultFloat(VIEW, 360, "ltr");
    expect(f.x).toBe(1440 - 360 - FLOAT_MARGIN_PX);
    expect(f.y).toBe(72);
    expect(f.y + floatHeight(900)).toBeLessThanOrEqual(900 - 176);
    expect(f.folded).toBe(false);
  });

  it("RTL mirrors: rests at the top-left", () => {
    expect(defaultFloat(VIEW, 360, "rtl").x).toBe(FLOAT_MARGIN_PX);
  });
});

describe("clampFloat keeps the window reachable", () => {
  it("pulls a window dragged past any edge back inside the margin", () => {
    expect(clampFloat({ x: -400, y: -50 }, 360, 560, VIEW)).toEqual({
      x: FLOAT_MARGIN_PX,
      y: FLOAT_MARGIN_PX,
    });
    expect(clampFloat({ x: 5000, y: 5000 }, 360, 560, VIEW)).toEqual({
      x: 1440 - 360 - FLOAT_MARGIN_PX,
      y: 900 - 560 - FLOAT_MARGIN_PX,
    });
  });

  it("leaves an in-bounds position untouched and rounds it", () => {
    expect(clampFloat({ x: 300.4, y: 120.6 }, 360, 560, VIEW)).toEqual({
      x: 300,
      y: 121,
    });
  });
});

describe("persistence", () => {
  it("defaults to docked when nothing or garbage is stored", () => {
    expect(loadDock()).toEqual({ mode: "docked" });
    localStorage.setItem(TUTOR_DOCK_KEY, "{not json");
    expect(loadDock()).toEqual({ mode: "docked" });
    localStorage.setItem(TUTOR_DOCK_KEY, JSON.stringify({ mode: "floating" }));
    expect(loadDock()).toEqual({ mode: "docked" });
  });

  it("round-trips a floating window with its place and fold", () => {
    const dock = { mode: "floating" as const, x: 40, y: 80, folded: true };
    storeDock(dock);
    expect(loadDock()).toEqual(dock);
    storeDock({ mode: "docked" });
    expect(loadDock()).toEqual({ mode: "docked" });
  });

  it("survives storage that throws", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
    });
    expect(loadDock()).toEqual({ mode: "docked" });
    expect(() => storeDock({ mode: "docked" })).not.toThrow();
  });
});
