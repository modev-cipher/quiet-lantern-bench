import { describe, expect, it } from "vitest";

import {
  createAutoScrollApplier,
  DEADBAND_BOTTOM_FRACTION,
  DEADBAND_TOP_FRACTION,
  EYE_LINE_FRACTION,
  EYE_LINE_PADDING,
  figureScrollTop,
  INITIAL_AUTO_SCROLL,
  autoScroll,
  eyeLineScrollTop,
  isScrollKey,
  nearestScrollTop,
  topicScrollTop,
  type AutoScrollState,
} from "./readingPageScroll";

const SUPPRESSED: AutoScrollState = { suppressed: true, pageNo: 4 };
const ARMED: AutoScrollState = { suppressed: false, pageNo: 4 };

const WORD = { topPx: 1200, heightPx: 28 };
const VIEWPORT = 920;
const CONTENT = 1968;

describe("(a) a narration-driven page turn puts the word on the eye line", () => {
  it("emits eyeLine when the shown page changes while following", () => {
    const outcome = autoScroll(ARMED, {
      type: "shownPage",
      following: true,
      pageNo: 5,
    });
    expect(outcome.intent).toEqual({ kind: "eyeLine" });
    expect(outcome.state.pageNo).toBe(5);
  });

  it("emits nothing when the page did not actually change", () => {
    expect(
      autoScroll(ARMED, { type: "shownPage", following: true, pageNo: 4 })
        .intent,
    ).toEqual({ kind: "none" });
  });

  it("needs no narration-driven flag, because a manual flip is browsing by type", () => {
    const flipped = autoScroll(ARMED, {
      type: "shownPage",
      following: false,
      pageNo: 5,
    });
    expect(
      flipped.intent,
      "followModel's flip always returns browsing, so a page change seen while following " +
        "cannot have come from the user's hand",
    ).toEqual({ kind: "none" });
  });
});

describe("(b) within a page, following keeps the active word in view", () => {
  it("emits nearest on a word change while armed", () => {
    expect(autoScroll(ARMED, { type: "word", following: true }).intent).toEqual({
      kind: "nearest",
    });
  });
});

describe("(c) browsing owns the viewport absolutely", () => {
  it("emits nothing for any trigger while browsing", () => {
    for (const trigger of [
      { type: "shownPage" as const, following: false, pageNo: 9 },
      { type: "word" as const, following: false },
      { type: "manualScroll" as const, following: false },
    ]) {
      expect(autoScroll(ARMED, trigger).intent).toEqual({ kind: "none" });
    }
  });

  it("never suppresses on a browsing scroll, so returning to the narration still aims", () => {
    const scrolled = autoScroll(ARMED, {
      type: "manualScroll",
      following: false,
    });
    expect(scrolled.state.suppressed).toBe(false);
    expect(
      autoScroll(scrolled.state, {
        type: "shownPage",
        following: true,
        pageNo: 4,
      }).intent,
      "the shown page is unchanged, so returning onto the same page emits nothing",
    ).toEqual({ kind: "none" });
    expect(
      autoScroll(scrolled.state, {
        type: "shownPage",
        following: true,
        pageNo: 7,
      }).intent,
    ).toEqual({ kind: "eyeLine" });
  });
});

describe("(d) a manual scroll is a viewport gesture, not a browse action", () => {
  it("cannot flip the mode, by type", () => {
    const outcome = autoScroll(ARMED, {
      type: "manualScroll",
      following: true,
    });
    expect(
      Object.keys(outcome.state).sort(),
      "the state carries no field a browse could travel in, so a scroll can never browse",
    ).toEqual(["pageNo", "suppressed"]);
  });

  it("suppresses within-page auto-scroll", () => {
    const outcome = autoScroll(ARMED, {
      type: "manualScroll",
      following: true,
    });
    expect(outcome.state.suppressed).toBe(true);
    expect(outcome.intent).toEqual({ kind: "none" });
    expect(
      autoScroll(outcome.state, { type: "word", following: true }).intent,
      "auto-scroll must not fight the reader's hand",
    ).toEqual({ kind: "none" });
  });

  it("stays suppressed across many word changes", () => {
    let state = SUPPRESSED;
    for (let tick = 0; tick < 50; tick += 1) {
      const outcome = autoScroll(state, { type: "word", following: true });
      expect(outcome.intent).toEqual({ kind: "none" });
      state = outcome.state;
    }
    expect(state.suppressed).toBe(true);
  });

  it("is re-established by the next narration-driven page turn, and only by it", () => {
    const turned = autoScroll(SUPPRESSED, {
      type: "shownPage",
      following: true,
      pageNo: 5,
    });
    expect(turned.state.suppressed).toBe(false);
    expect(turned.intent).toEqual({ kind: "eyeLine" });
    expect(
      autoScroll(turned.state, { type: "word", following: true }).intent,
    ).toEqual({ kind: "nearest" });
  });

  it("leaves a same-page seek suppressed, the disclosed residual", () => {
    expect(
      autoScroll(SUPPRESSED, { type: "word", following: true }).intent,
      "a seek that does not turn the page does not re-establish auto-scroll; bounded, and in " +
        "the direction that never moves the viewport under the reader",
    ).toEqual({ kind: "none" });
  });

  it("is idempotent, so a programmatic scroll echo cannot double-suppress", () => {
    const once = autoScroll(ARMED, { type: "manualScroll", following: true });
    const twice = autoScroll(once.state, {
      type: "manualScroll",
      following: true,
    });
    expect(twice.state).toBe(once.state);
  });
});

describe("the eye line is inside the ruled centre third", () => {
  it("sits between a third and two thirds down, biased upward", () => {
    expect(EYE_LINE_FRACTION).toBeGreaterThanOrEqual(1 / 3);
    expect(EYE_LINE_FRACTION).toBeLessThanOrEqual(2 / 3);
    expect(
      EYE_LINE_FRACTION,
      "a reader looks slightly above centre so the next lines are visible below the word",
    ).toBeLessThan(0.5);
  });

  it("places the word centre on the eye line", () => {
    const top = eyeLineScrollTop(WORD, VIEWPORT, 4000);
    expect(top).toBe(Math.round(1200 + 14 - 920 * EYE_LINE_FRACTION));
  });

  it("clamps at the top of the page", () => {
    expect(eyeLineScrollTop({ topPx: 10, heightPx: 28 }, VIEWPORT, 4000)).toBe(
      0,
    );
  });

  it("clamps at the bottom of the page", () => {
    expect(
      eyeLineScrollTop({ topPx: 1900, heightPx: 28 }, VIEWPORT, CONTENT),
    ).toBe(CONTENT - VIEWPORT);
  });

  it("refuses a word or viewport it cannot measure", () => {
    expect(eyeLineScrollTop({ topPx: Number.NaN, heightPx: 28 }, VIEWPORT, CONTENT)).toBeNull();
    expect(eyeLineScrollTop(WORD, 0, CONTENT)).toBeNull();
  });
});

describe("nearest scrolling moves as little as possible", () => {
  it("does not scroll a word already comfortably in view", () => {
    expect(nearestScrollTop({ topPx: 500, heightPx: 28 }, VIEWPORT, CONTENT, 300)).toBe(300);
  });

  it("scrolls up just enough for a word above the viewport", () => {
    expect(nearestScrollTop({ topPx: 320, heightPx: 28 }, VIEWPORT, CONTENT, 400)).toBe(272);
  });

  it("scrolls down just enough for a word below the viewport", () => {
    expect(nearestScrollTop({ topPx: 1000, heightPx: 28 }, VIEWPORT, CONTENT, 100)).toBe(
      1000 + 28 + 48 - VIEWPORT,
    );
  });

  it("aligns the top of a word taller than the viewport", () => {
    expect(nearestScrollTop({ topPx: 600, heightPx: 1000 }, VIEWPORT, 3000, 0)).toBe(552);
  });

  it("never scrolls past the end of the page", () => {
    expect(
      nearestScrollTop({ topPx: 1940, heightPx: 28 }, VIEWPORT, CONTENT, 0),
    ).toBe(CONTENT - VIEWPORT);
  });

  it("cannot scroll a page shorter than its viewport", () => {
    expect(nearestScrollTop(WORD, VIEWPORT, 500, 0)).toBe(0);
  });

  it("refuses inputs it cannot measure", () => {
    expect(nearestScrollTop(WORD, VIEWPORT, CONTENT, Number.NaN)).toBeNull();
    expect(nearestScrollTop({ topPx: 1, heightPx: Number.NaN }, VIEWPORT, CONTENT, 0)).toBeNull();
  });
});

describe("the initial state auto-scrolls on the first page it is given", () => {
  it("treats the first shown page as a turn", () => {
    expect(
      autoScroll(INITIAL_AUTO_SCROLL, {
        type: "shownPage",
        following: true,
        pageNo: 1,
      }).intent,
    ).toEqual({ kind: "eyeLine" });
  });
});

interface FakeContainer {
  element: HTMLElement;
  tops: number[];
}

function fakeContainer(
  clientHeight: number,
  scrollHeight: number,
): FakeContainer {
  const tops: number[] = [];
  const element = {
    scrollTop: 0,
    clientHeight,
    scrollHeight,
    getBoundingClientRect: () => ({ top: 0, height: clientHeight }),
    scrollTo(options: { top: number }) {
      tops.push(options.top);
      element.scrollTop = options.top;
    },
  } as unknown as HTMLElement;
  return { element, tops };
}

function fakeWord(clientTop: number | null, height = 28): HTMLElement {
  return {
    hidden: clientTop == null,
    getBoundingClientRect: () => ({ top: clientTop ?? 0, height }),
  } as unknown as HTMLElement;
}

describe("the applier is the layer that actually scrolls", () => {
  it("puts the word on the eye line when the page turns", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    expect(
      container.tops,
      "the eye line is measured after the new page commits, never against the outgoing page's box",
    ).toEqual([]);
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    expect(container.tops).toEqual([846]);
  });

  it("shows a new page from the top when the ladder located no word", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    container.element.scrollTop = 500;
    applier.apply(
      { container: container.element, word: fakeWord(null) },
      { following: true, pageNo: 4, wordSignature: "none" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(null) });
    expect(container.tops).toEqual([0]);
  });

  it("does not scroll a word that is already in view", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    applier.apply(
      { container: container.element, word: fakeWord(1300 - 846) },
      { following: true, pageNo: 4, wordSignature: "b" },
    );
    expect(container.tops).toEqual([846]);
  });

  it("scrolls the minimum for a word that has left the viewport", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    applier.apply(
      { container: container.element, word: fakeWord(1800 - 846) },
      { following: true, pageNo: 4, wordSignature: "b" },
    );
    expect(container.tops).toEqual([846, 956]);
  });

  it("ignores a repeated word signature, so a 60Hz tick reads no layout", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    for (let tick = 0; tick < 30; tick += 1) {
      applier.apply(
        { container: container.element, word: fakeWord(1800 - 846) },
        { following: true, pageNo: 4, wordSignature: "a" },
      );
    }
    expect(container.tops).toEqual([846]);
  });

  it("never scrolls while browsing", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: false, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    applier.apply(
      { container: container.element, word: fakeWord(1800) },
      { following: false, pageNo: 5, wordSignature: "b" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1800) });
    expect(container.tops).toEqual([]);
  });

  it("stops following the word after a manual scroll, and resumes on the next turn", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    applier.manualScroll(true);
    applier.apply(
      { container: container.element, word: fakeWord(1800 - 846) },
      { following: true, pageNo: 4, wordSignature: "b" },
    );
    expect(container.tops).toEqual([846]);

    applier.apply(
      { container: container.element, word: fakeWord(300) },
      { following: true, pageNo: 5, wordSignature: "c" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(300) });
    expect(container.tops).toEqual([846, 846 + 300 + 14 - 368]);
  });

  it("re-arms on reset", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.manualScroll(true);
    applier.reset();
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    applier.apply(
      { container: container.element, word: fakeWord(1800 - 846) },
      { following: true, pageNo: 4, wordSignature: "b" },
    );
    expect(container.tops).toEqual([846, 956]);
  });

  it("survives a container that has not mounted", () => {
    const applier = createAutoScrollApplier();
    expect(() =>
      applier.apply(
        { container: null, word: null },
        { following: true, pageNo: 4, wordSignature: "a" },
      ),
    ).not.toThrow();
  });
});

describe("a manual scroll gesture is input, never the scroll event", () => {
  it("recognises the keys that scroll a container", () => {
    for (const key of ["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "]) {
      expect(isScrollKey(key)).toBe(true);
    }
  });

  it("ignores keys that do not scroll, so typing never suppresses auto-scroll", () => {
    for (const key of ["a", "Enter", "Escape", "Tab", "ArrowLeft", "ArrowRight", "Shift"]) {
      expect(isScrollKey(key)).toBe(false);
    }
  });
});

describe("a scrollbar drag is manual input, an auto-scroll's own echo is not", () => {
  it("does not suppress on the scroll event its own auto-scroll caused", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    expect(container.tops).toEqual([846]);

    applier.scrolled(container.element, true);

    applier.apply(
      { container: container.element, word: fakeWord(1800 - 846) },
      { following: true, pageNo: 4, wordSignature: "b" },
    );
    expect(
      container.tops,
      "an auto-scroll fires a scroll event too; reading that as the reader's hand would let the " +
        "first auto-scroll silently switch auto-scroll off for the rest of the page",
    ).toEqual([846, 956]);
  });

  it("suppresses on a scroll it did not command, which is the scrollbar path", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });

    container.element.scrollTop = 300;
    applier.scrolled(container.element, true);

    applier.apply(
      { container: container.element, word: fakeWord(1800 - 300) },
      { following: true, pageNo: 4, wordSignature: "b" },
    );
    expect(
      container.tops,
      "dragging the scrollbar thumb changes scrollTop without a wheel, touch or key event, so " +
        "the scroll event is the only signal that the reader moved the viewport",
    ).toEqual([846]);
  });

  it("never suppresses from a scroll while browsing", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.scrolled(container.element, false);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    expect(container.tops).toEqual([846]);
  });
});

describe("returning to the narration re-aims even when the page never changed", () => {
  it("re-arms and asks for the eye line, the variant a page-change trigger cannot see", () => {
    const outcome = autoScroll(SUPPRESSED, { type: "returnedToNarration" });
    expect(outcome.state.suppressed).toBe(false);
    expect(outcome.intent).toEqual({ kind: "eyeLine" });
  });

  it("recovers a viewport the reader scrolled away, browsed from, and came back to", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    applier.apply(
      { container: container.element, word: fakeWord(1200) },
      { following: true, pageNo: 4, wordSignature: "a" },
    );
    applier.commitPage({ container: container.element, word: fakeWord(1200) });
    expect(container.tops).toEqual([846]);

    container.element.scrollTop = 300;
    applier.scrolled(container.element, true);

    applier.apply(
      { container: container.element, word: fakeWord(1200 - 300) },
      { following: false, pageNo: 5, wordSignature: "a" },
    );
    applier.apply(
      { container: container.element, word: fakeWord(1200 - 300) },
      { following: false, pageNo: 4, wordSignature: "a" },
    );

    applier.returned();
    applier.commitPage({
      container: container.element,
      word: fakeWord(1200 - 300),
    });
    expect(
      container.tops,
      "browsing back onto the narration's own page means the shown page never changes, so a " +
        "page-change trigger cannot re-arm; without this the one affordance whose whole purpose " +
        "is re-aiming the viewport would do nothing visible and the highlight could stay offscreen",
    ).toEqual([846, 846]);

    applier.apply(
      { container: container.element, word: fakeWord(1800 - 846) },
      { following: true, pageNo: 4, wordSignature: "b" },
    );
    expect(
      container.tops,
      "and within-page following is armed again afterwards",
    ).toEqual([846, 846, 956]);
  });
});

describe("R-BQ — a dangling eye line cannot outlive the suppression that followed it", () => {
  it("refuses a pending eye line while the reader is holding the viewport", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    const word = () => ({ container: container.element, word: fakeWord(1200) });

    applier.apply(word(), { following: true, pageNo: 4, wordSignature: "a" });
    applier.commitPage(word());
    applier.apply(word(), { following: false, pageNo: 5, wordSignature: "a" });

    applier.returned();
    applier.commitPage(word());

    applier.apply(word(), { following: true, pageNo: 4, wordSignature: "a" });

    applier.manualScroll(true);
    container.element.scrollTop = 1500;
    const before = container.tops.length;

    applier.commitPage(word());

    expect(
      container.tops.length,
      "a resize or a fit click fired commitPage against a stale pending eye line and yanked the viewport out of the reader's hand",
    ).toBe(before);
  });

  it("still honours the eye line on a genuine narration turn, because the turn clears suppression first", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    const word = () => ({ container: container.element, word: fakeWord(1200) });

    applier.apply(word(), { following: true, pageNo: 4, wordSignature: "a" });
    applier.commitPage(word());
    applier.manualScroll(true);

    applier.apply(word(), { following: true, pageNo: 5, wordSignature: "b" });
    const before = container.tops.length;
    applier.commitPage(word());

    expect(
      container.tops.length,
      "suppression must not survive a narration-driven turn, or following silently stops working",
    ).toBe(before + 1);
  });
});

describe("coarse segments anchor to fallback envelope on page turn", () => {
  function fakeEnvelope(clientTop: number | null, height = 100): HTMLElement {
    const child = {
      getBoundingClientRect: () => ({ top: clientTop ?? 0, height }),
    } as unknown as HTMLElement;
    return {
      hidden: clientTop == null,
      firstElementChild: child,
      getBoundingClientRect: () => ({ top: 0, height: 1000 }),
    } as unknown as HTMLElement;
  }

  it("anchors to envelope bbox with EYE_LINE_PADDING and includes container.scrollTop", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    container.element.scrollTop = 100;
    const envelope = fakeEnvelope(400, 80);

    applier.apply(
      { container: container.element, word: null, envelope },
      { following: true, pageNo: 2, wordSignature: "seg-1" },
    );
    applier.commitPage({ container: container.element, word: null, envelope });

    const expectedTop = 400 - 0 + 100 - EYE_LINE_PADDING;
    expect(container.tops).toEqual([expectedTop]);
  });

  it("falls back to top 0 if envelope is hidden or missing", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    container.element.scrollTop = 500;
    const hiddenEnvelope = fakeEnvelope(null);

    applier.apply(
      { container: container.element, word: null, envelope: hiddenEnvelope },
      { following: true, pageNo: 2, wordSignature: "seg-2" },
    );
    applier.commitPage({ container: container.element, word: null, envelope: hiddenEnvelope });
    expect(container.tops).toEqual([0]);
  });

  it("anchors to standalone envelope element without children", () => {
    const applier = createAutoScrollApplier();
    const container = fakeContainer(920, 1968);
    const standalone = {
      hidden: false,
      firstElementChild: null,
      getBoundingClientRect: () => ({ top: 300, height: 50 }),
    } as unknown as HTMLElement;

    applier.apply(
      { container: container.element, word: null, envelope: standalone },
      { following: true, pageNo: 3, wordSignature: "seg-3" },
    );
    applier.commitPage({ container: container.element, word: null, envelope: standalone });
    expect(container.tops).toEqual([300 - EYE_LINE_PADDING]);
  });
});

describe("Teacher mode topic and figure viewport synchronization", () => {
  const VIEWPORT = 800;
  const CONTENT = 2400;

  describe("topicScrollTop deadband behavior", () => {
    it("returns null on non-finite or invalid geometry", () => {
      expect(topicScrollTop(NaN, VIEWPORT, CONTENT, 100)).toBeNull();
      expect(topicScrollTop(200, 0, CONTENT, 100)).toBeNull();
      expect(topicScrollTop(200, VIEWPORT, CONTENT, NaN)).toBeNull();
    });

    it("keeps viewport stationary when block top is comfortably within the 15%-75% deadband", () => {
      const currentTop = 500;
      const deadbandTop = currentTop + VIEWPORT * DEADBAND_TOP_FRACTION;
      const deadbandBottom = currentTop + VIEWPORT * DEADBAND_BOTTOM_FRACTION;
      const insideDeadband = (deadbandTop + deadbandBottom) / 2;
      expect(topicScrollTop(insideDeadband, VIEWPORT, CONTENT, currentTop)).toBe(
        currentTop,
      );

      // exactly at the boundary limits
      expect(topicScrollTop(deadbandTop, VIEWPORT, CONTENT, currentTop)).toBe(
        currentTop,
      );
      expect(topicScrollTop(deadbandBottom, VIEWPORT, CONTENT, currentTop)).toBe(
        currentTop,
      );
    });

    it("scrolls block top to eye-line (40%) when block top is above the deadband (< 15%)", () => {
      const currentTop = 500;
      // block top is at 400 (above 620)
      // target = 400 - 800 * 0.40 = 400 - 320 = 80
      expect(topicScrollTop(400, VIEWPORT, CONTENT, currentTop)).toBe(80);
    });

    it("scrolls block top to eye-line (40%) when block top is below the deadband (> 75%)", () => {
      const currentTop = 500;
      // block top is at 1200 (below 1100)
      // target = 1200 - 800 * 0.40 = 1200 - 320 = 880
      expect(topicScrollTop(1200, VIEWPORT, CONTENT, currentTop)).toBe(880);
    });

    it("clamps target scroll position to valid content range", () => {
      // block near the very top: 50 - 320 = -270 -> clamped to 0
      expect(topicScrollTop(50, VIEWPORT, CONTENT, 500)).toBe(0);

      // block near the very bottom: 2300 - 320 = 1980 -> clamped to 2400 - 800 = 1600
      expect(topicScrollTop(2300, VIEWPORT, CONTENT, 500)).toBe(1600);
    });
  });

  describe("figureScrollTop centering behavior", () => {
    it("returns null on non-finite or invalid inputs", () => {
      expect(figureScrollTop(NaN, VIEWPORT, CONTENT)).toBeNull();
      expect(figureScrollTop(400, 0, CONTENT)).toBeNull();
    });

    it("centers the figure vertically in the viewport", () => {
      // figureCenterPx = 1000, viewport = 800
      // target = 1000 - 800 / 2 = 600
      expect(figureScrollTop(1000, VIEWPORT, CONTENT)).toBe(600);
    });

    it("clamps figure scroll position to scroll boundaries", () => {
      // figure near top: 200 - 400 = -200 -> clamped to 0
      expect(figureScrollTop(200, VIEWPORT, CONTENT)).toBe(0);

      // figure near bottom: 2200 - 400 = 1800 -> clamped to 1600
      expect(figureScrollTop(2200, VIEWPORT, CONTENT)).toBe(1600);
    });
  });

  describe("AutoScrollApplier.syncTeacher smooth alignment", () => {
    it("smoothly scrolls container when following is true and targetTop is valid", () => {
      const applier = createAutoScrollApplier();
      const container = fakeContainer(VIEWPORT, CONTENT);
      container.element.scrollTop = 100;

      applier.syncTeacher(container.element, 500, true);
      expect(container.tops).toEqual([500]);
    });

    it("does not scroll when following is false or targetTop is null", () => {
      const applier = createAutoScrollApplier();
      const container = fakeContainer(VIEWPORT, CONTENT);

      applier.syncTeacher(container.element, 500, false);
      expect(container.tops).toEqual([]);

      applier.syncTeacher(container.element, null, true);
      expect(container.tops).toEqual([]);
    });

    it("does not scroll if container is already at target position", () => {
      const applier = createAutoScrollApplier();
      const container = fakeContainer(VIEWPORT, CONTENT);
      container.element.scrollTop = 500;

      applier.syncTeacher(container.element, 500, true);
      expect(container.tops).toEqual([]);
    });
  });
});


