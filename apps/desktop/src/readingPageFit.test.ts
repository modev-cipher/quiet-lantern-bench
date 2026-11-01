import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  DEFAULT_FIT,
  READING_COLUMN_MAX_PX,
  clampPageZoom,
  fittedWidthPx,
  loadPageFit,
  loadPageZoom,
  pageFitFrom,
  scrollsVertically,
  storePageFit,
  storePageZoom,
  zoomIn,
  zoomOut,
} from "./readingPageFit";

const DDIA_RATIO = 504 / 661.5;
const DDIA_WIDTH_PT = 504;
const PHYSICAL_SIZE_FLOOR_PX_PER_PT = 2.0;

const VIEWPORT = { containerWidth: 1920, containerHeight: 920 };

describe("the fit model targets the reading column, not the card", () => {
  it("fills the ruled reading column at a 1920 viewport", () => {
    expect(
      fittedWidthPx({ ...VIEWPORT, ratio: DDIA_RATIO, fit: "width", zoom: 1 }),
    ).toBe(READING_COLUMN_MAX_PX);
  });

  it("uses the container when it is narrower than the column", () => {
    expect(
      fittedWidthPx({
        containerWidth: 900,
        containerHeight: 920,
        ratio: DDIA_RATIO,
        fit: "width",
        zoom: 1,
      }),
    ).toBe(900);
  });

  it("defaults to width, because fit-page cannot reach a readable size on a 1080p seat", () => {
    expect(DEFAULT_FIT).toBe("width");

    const page = fittedWidthPx({
      ...VIEWPORT,
      ratio: DDIA_RATIO,
      fit: "page",
      zoom: 1,
    });
    const width = fittedWidthPx({
      ...VIEWPORT,
      ratio: DDIA_RATIO,
      fit: "width",
      zoom: 1,
    });

    expect(page).toBe(700);
    expect(
      (page ?? 0) / DDIA_WIDTH_PT,
      "fit-page renders the 504pt DDIA trim below its printed physical size, which is the " +
        "defect this item fixes at a larger size, so it must never be the default",
    ).toBeLessThan(PHYSICAL_SIZE_FLOOR_PX_PER_PT);
    expect(
      (width ?? 0) / DDIA_WIDTH_PT,
      "fit-width must clear printed physical size; 1500px over a 504pt trim is 2.98 px/pt, " +
        "which is also the scale-3.0 raster at 1:1",
    ).toBeGreaterThanOrEqual(PHYSICAL_SIZE_FLOOR_PX_PER_PT);
  });

  it("fit-page never exceeds the column even on a very tall viewport", () => {
    expect(
      fittedWidthPx({
        containerWidth: 1920,
        containerHeight: 4000,
        ratio: DDIA_RATIO,
        fit: "page",
        zoom: 1,
      }),
    ).toBe(READING_COLUMN_MAX_PX);
  });
});

describe("zoom is the user's override of the fit", () => {
  it("multiplies the fitted width and may exceed the column", () => {
    expect(
      fittedWidthPx({ ...VIEWPORT, ratio: DDIA_RATIO, fit: "width", zoom: 2 }),
    ).toBe(3000);
  });

  it("clamps to the 50-250% band", () => {
    expect(clampPageZoom(0.1)).toBe(0.5);
    expect(clampPageZoom(9)).toBe(2.5);
    expect(zoomOut(0.5)).toBe(0.5);
    expect(zoomIn(2.5)).toBe(2.5);
    expect(zoomIn(1)).toBe(1.1);
    expect(zoomOut(1)).toBe(0.9);
  });

  it("treats a non-finite zoom as 1 rather than collapsing the page", () => {
    expect(clampPageZoom(Number.NaN)).toBe(1);
    expect(clampPageZoom(Number.POSITIVE_INFINITY)).toBe(1);
    expect(
      fittedWidthPx({
        ...VIEWPORT,
        ratio: DDIA_RATIO,
        fit: "width",
        zoom: Number.NaN,
      }),
    ).toBe(READING_COLUMN_MAX_PX);
  });
});

describe("a page without measured dimensions claims nothing", () => {
  it("refuses a null, zero or negative ratio", () => {
    for (const ratio of [null, 0, -1, Number.NaN]) {
      expect(
        fittedWidthPx({ ...VIEWPORT, ratio, fit: "width", zoom: 1 }),
      ).toBeNull();
    }
  });

  it("refuses a container that has not been measured yet", () => {
    expect(
      fittedWidthPx({
        containerWidth: 0,
        containerHeight: 920,
        ratio: DDIA_RATIO,
        fit: "width",
        zoom: 1,
      }),
    ).toBeNull();
  });

  it("refuses fit-page without a measured height", () => {
    expect(
      fittedWidthPx({
        containerWidth: 1920,
        containerHeight: 0,
        ratio: DDIA_RATIO,
        fit: "page",
        zoom: 1,
      }),
    ).toBeNull();
  });
});

describe("the ruled default scrolls, and that is the point", () => {
  it("fit-width overflows a 1080p viewport vertically", () => {
    expect(
      scrollsVertically({
        ...VIEWPORT,
        ratio: DDIA_RATIO,
        fit: "width",
        zoom: 1,
      }),
    ).toBe(true);
  });

  it("fit-page does not overflow, which is why it is offered as a preset", () => {
    expect(
      scrollsVertically({
        ...VIEWPORT,
        ratio: DDIA_RATIO,
        fit: "page",
        zoom: 1,
      }),
    ).toBe(false);
  });

  it("fit-page never overflows its container at any viewport height", () => {
    for (let height = 400; height <= 1400; height += 7) {
      const width = fittedWidthPx({
        containerWidth: 1920,
        containerHeight: height,
        ratio: DDIA_RATIO,
        fit: "page",
        zoom: 1,
      });
      expect(width).not.toBeNull();
      expect(
        (width ?? 0) / DDIA_RATIO,
        `fit-page at a ${height}px viewport overflows it, which shows a scrollbar in the mode ` +
          "whose whole purpose is that the page fits",
      ).toBeLessThanOrEqual(height);
    }
  });
});

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
      return [...values.keys()][index] ?? null;
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

describe("the preference is per seat and degrades to the default", () => {
  beforeEach(() => {
    withStorage(fakeStorage());
  });

  afterEach(() => {
    withStorage(undefined);
  });

  it("survives a seat with no storage at all rather than failing to open", () => {
    withStorage(undefined);
    expect(loadPageFit()).toBe("width");
    expect(loadPageZoom()).toBe(1);
    expect(() => storePageFit("page")).not.toThrow();
    expect(() => storePageZoom(1.2)).not.toThrow();
  });

  it("reads only the exact literal as fit-page", () => {
    expect(pageFitFrom("page")).toBe("page");
    for (const stored of [null, "", "Page", "width", "fit-page", "1"]) {
      expect(pageFitFrom(stored)).toBe("width");
    }
  });

  it("round-trips a stored fit and zoom", () => {
    storePageFit("page");
    storePageZoom(1.4);
    expect(loadPageFit()).toBe("page");
    expect(loadPageZoom()).toBe(1.4);
  });

  it("degrades a corrupt stored zoom to 1", () => {
    localStorage.setItem("txtcaster.pageZoom", "wide please");
    expect(loadPageZoom()).toBe(1);
    localStorage.setItem("txtcaster.pageZoom", "");
    expect(loadPageZoom()).toBe(1);
  });

  it("clamps a stored zoom that is out of band", () => {
    localStorage.setItem("txtcaster.pageZoom", "99");
    expect(loadPageZoom()).toBe(2.5);
  });
});
