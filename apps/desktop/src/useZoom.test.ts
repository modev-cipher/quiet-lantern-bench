import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  clampZoom,
  loadZoom,
  zoomFromKey,
  zoomFromWheel,
  ZOOM_MAX,
  ZOOM_MIN,
} from "./useZoom";

function fakeStorage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() {
      return values.size;
    },
    clear() {
      values.clear();
    },
    getItem(key: string) {
      return values.get(key) ?? null;
    },
    key(index: number) {
      return Array.from(values.keys())[index] ?? null;
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

describe("clampZoom", () => {
  it("clamps to the allowed range", () => {
    expect(clampZoom(0.1)).toBe(ZOOM_MIN);
    expect(clampZoom(9)).toBe(ZOOM_MAX);
    expect(clampZoom(1.25)).toBe(1.3);
  });

  it("rounds to one decimal to avoid float drift", () => {
    expect(clampZoom(1.1 + 0.1)).toBe(1.2);
  });
});

describe("zoomFromKey", () => {
  it("steps up on plus and equals", () => {
    expect(zoomFromKey(1, "+")).toBe(1.1);
    expect(zoomFromKey(1, "=")).toBe(1.1);
  });

  it("steps down on minus", () => {
    expect(zoomFromKey(1, "-")).toBe(0.9);
  });

  it("resets on zero", () => {
    expect(zoomFromKey(2, "0")).toBe(1);
  });

  it("ignores other keys", () => {
    expect(zoomFromKey(1, "a")).toBeNull();
    expect(zoomFromKey(1, "ArrowUp")).toBeNull();
  });

  it("saturates at the bounds", () => {
    expect(zoomFromKey(ZOOM_MAX, "+")).toBe(ZOOM_MAX);
    expect(zoomFromKey(ZOOM_MIN, "-")).toBe(ZOOM_MIN);
  });
});

describe("zoomFromWheel", () => {
  it("zooms in on scroll up and out on scroll down", () => {
    expect(zoomFromWheel(1, -120)).toBe(1.1);
    expect(zoomFromWheel(1, 120)).toBe(0.9);
  });
});

describe("loadZoom", () => {
  beforeEach(() => {
    withStorage(fakeStorage());
  });

  afterEach(() => {
    withStorage(undefined);
  });

  it("survives no storage at all by falling back to 1", () => {
    withStorage(undefined);
    expect(loadZoom()).toBe(1);
  });

  it("returns default 1 when storage is absent or corrupt", () => {
    localStorage.removeItem("txtcaster.zoom");
    expect(loadZoom()).toBe(1);

    localStorage.setItem("txtcaster.zoom", "not-a-number");
    expect(loadZoom()).toBe(1);
  });

  it("returns default 1 on blank string rather than clamping 0 to ZOOM_MIN", () => {
    localStorage.setItem("txtcaster.zoom", "");
    expect(loadZoom()).toBe(1);

    localStorage.setItem("txtcaster.zoom", "   ");
    expect(loadZoom()).toBe(1);
  });

  it("loads and clamps valid numeric values", () => {
    localStorage.setItem("txtcaster.zoom", "1.4");
    expect(loadZoom()).toBe(1.4);

    localStorage.setItem("txtcaster.zoom", "0.1");
    expect(loadZoom()).toBe(ZOOM_MIN);

    localStorage.setItem("txtcaster.zoom", "9");
    expect(loadZoom()).toBe(ZOOM_MAX);
  });
});

