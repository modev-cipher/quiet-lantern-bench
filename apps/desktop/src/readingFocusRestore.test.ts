import { describe, expect, it, vi } from "vitest";

import {
  FOLLOW_ALONG_FALLBACK,
  restoreFocus,
  restoreTarget,
} from "./readingFocusRestore";

function candidate(isConnected: boolean) {
  return { isConnected, focus: vi.fn() };
}

function source(match: unknown) {
  return {
    querySelector: vi.fn((selectors: string) =>
      selectors === FOLLOW_ALONG_FALLBACK ? (match as Element | null) : null,
    ),
  };
}

describe("restoreTarget", () => {
  it("keeps the opener when it is still in the document", () => {
    const previous = candidate(true);
    const doc = source(candidate(true));
    expect(restoreTarget(previous, doc)).toBe(previous);
    expect(doc.querySelector).not.toHaveBeenCalled();
  });

  it("falls back to the mini player when the opener unmounted with the overlay", () => {
    const survivor = candidate(true);
    const doc = source(survivor);
    expect(restoreTarget(candidate(false), doc)).toBe(survivor);
  });

  it("returns nothing rather than a target that cannot take focus", () => {
    expect(restoreTarget(candidate(false), source({}))).toBeNull();
    expect(restoreTarget(candidate(false), source(null))).toBeNull();
  });

  it("focuses the fallback so closing from Now Playing never lands on the body", () => {
    const survivor = candidate(true);
    expect(restoreFocus(candidate(false), source(survivor))).toBe(true);
    expect(survivor.focus).toHaveBeenCalledTimes(1);
  });

  it("reports failure when nothing can be focused", () => {
    expect(restoreFocus(null, source(null))).toBe(false);
  });
});
