import { describe, expect, it } from "vitest";

import { boxSignature, createPageOverlayApplier } from "./readingPageHighlight";
import { NO_OVERLAY } from "./readingPageModel";
import type { OverlayPlan } from "./readingPageModel";

interface FakeElement {
  hidden: boolean;
  style: Record<string, string>;
}

function element(): FakeElement {
  return { hidden: false, style: {} };
}

function targets(word: FakeElement, fallback: FakeElement) {
  return {
    word: word as unknown as HTMLElement,
    fallback: fallback as unknown as HTMLElement,
  };
}

const WORD: OverlayPlan = {
  kind: "word",
  boxes: [{ leftPct: 10, topPct: 20, widthPct: 5, heightPct: 2 }],
};

const SEGMENT: OverlayPlan = {
  kind: "segment",
  boxes: [{ leftPct: 10, topPct: 20, widthPct: 30, heightPct: 2 }],
};

describe("the page overlay applier", () => {
  it("places the word box and hides the fallback", () => {
    const word = element();
    const fallback = element();

    createPageOverlayApplier().apply(targets(word, fallback), WORD);

    expect(word.hidden).toBe(false);
    expect(word.style).toEqual({
      left: "10%",
      top: "20%",
      width: "5%",
      height: "2%",
    });
    expect(fallback.hidden).toBe(true);
  });

  it("shows the fallback and hides the word box on a token with no rect", () => {
    const word = element();
    const fallback = element();

    createPageOverlayApplier().apply(targets(word, fallback), SEGMENT);

    expect(
      word.hidden,
      "the last located word must not stay lit under a token that was never located",
    ).toBe(true);
    expect(fallback.hidden).toBe(false);
  });

  it("leaves the fallback's geometry alone, because React owns those boxes", () => {
    const word = element();
    const fallback = element();

    createPageOverlayApplier().apply(targets(word, fallback), {
      kind: "segment",
      boxes: [
        { leftPct: 10, topPct: 20, widthPct: 30, heightPct: 2 },
        { leftPct: 10, topPct: 24, widthPct: 18, heightPct: 2 },
      ],
    });

    expect(fallback.hidden).toBe(false);
    expect(
      fallback.style,
      "a line or block rung can carry several rects and they change once per segment, so they are React state; only the per-frame word box is written imperatively. Moving that geometry here without moving this test would make two owners for one box",
    ).toEqual({});
  });

  it("hides both when the ladder anchored nothing on this page", () => {
    const word = element();
    const fallback = element();

    createPageOverlayApplier().apply(targets(word, fallback), NO_OVERLAY);

    expect(word.hidden).toBe(true);
    expect(fallback.hidden).toBe(true);
  });

  it("writes nothing while the box has not moved, because this runs every frame", () => {
    const word = element();
    const fallback = element();
    const applier = createPageOverlayApplier();

    applier.apply(targets(word, fallback), WORD);
    word.style.left = "touched";
    applier.apply(targets(word, fallback), { ...WORD, boxes: [...WORD.boxes] });

    expect(word.style.left).toBe("touched");

    applier.apply(targets(word, fallback), SEGMENT);
    expect(word.hidden).toBe(true);
  });

  it("re-applies to freshly mounted targets even when the plan has not changed", () => {
    const applier = createPageOverlayApplier();
    const first = { word: element(), fallback: element() };

    applier.apply(targets(first.word, first.fallback), SEGMENT);
    expect(first.fallback.hidden).toBe(false);

    applier.apply({ word: null, fallback: null }, SEGMENT);

    const remounted = { word: element(), fallback: element() };
    remounted.fallback.hidden = true;
    remounted.word.hidden = true;
    applier.apply(targets(remounted.word, remounted.fallback), SEGMENT);

    expect(
      remounted.fallback.hidden,
      "a same-page figure zoom unmounts the overlay and React remounts it with hidden hard-coded, so a signature-only cache skips the write and every later segment-plan segment stays blank. React cannot heal it: the hidden prop never changes, so there is no reconciliation delta",
    ).toBe(false);
    expect(remounted.word.hidden).toBe(true);
  });

  it("re-applies after a reset, so a remount is not left with a stale box", () => {
    const word = element();
    const fallback = element();
    const applier = createPageOverlayApplier();

    applier.apply(targets(word, fallback), WORD);
    applier.reset();
    word.hidden = true;
    applier.apply(targets(word, fallback), WORD);

    expect(word.hidden).toBe(false);
  });

  it("survives targets that are not mounted", () => {
    expect(() =>
      createPageOverlayApplier().apply({ word: null, fallback: null }, WORD),
    ).not.toThrow();
  });

  it("distinguishes two different word boxes but not two segment plans", () => {
    expect(boxSignature(WORD)).not.toBe(
      boxSignature({ ...WORD, boxes: [{ ...WORD.boxes[0], leftPct: 11 }] }),
    );
    expect(boxSignature(SEGMENT)).toBe(boxSignature({ ...SEGMENT, boxes: [] }));
    expect(boxSignature({ kind: "word", boxes: [] })).toBe("none");
  });
});
