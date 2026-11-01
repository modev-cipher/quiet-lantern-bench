export const EYE_LINE_FRACTION = 0.4;
export const NEAREST_MARGIN_PX = 48;
export const EYE_LINE_PADDING = 24;

const SCROLL_KEYS = new Set([
  "ArrowUp",
  "ArrowDown",
  "PageUp",
  "PageDown",
  "Home",
  "End",
  " ",
]);

export function isScrollKey(key: string): boolean {
  return SCROLL_KEYS.has(key);
}

export type ScrollIntent =
  | { kind: "none" }
  | { kind: "eyeLine" }
  | { kind: "nearest" };

export const NO_SCROLL: ScrollIntent = { kind: "none" };

export interface AutoScrollState {
  suppressed: boolean;
  pageNo: number | null;
}

export const INITIAL_AUTO_SCROLL: AutoScrollState = {
  suppressed: false,
  pageNo: null,
};

export type AutoScrollTrigger =
  | { type: "shownPage"; following: boolean; pageNo: number | null }
  | { type: "returnedToNarration" }
  | { type: "word"; following: boolean }
  | { type: "manualScroll"; following: boolean };

export interface AutoScrollOutcome {
  state: AutoScrollState;
  intent: ScrollIntent;
}

export function autoScroll(
  state: AutoScrollState,
  trigger: AutoScrollTrigger,
): AutoScrollOutcome {
  switch (trigger.type) {
    case "shownPage": {
      if (trigger.pageNo === state.pageNo) {
        return { state, intent: NO_SCROLL };
      }
      if (!trigger.following || trigger.pageNo == null) {
        return {
          state: { ...state, pageNo: trigger.pageNo },
          intent: NO_SCROLL,
        };
      }
      return {
        state: { suppressed: false, pageNo: trigger.pageNo },
        intent: { kind: "eyeLine" },
      };
    }
    case "returnedToNarration":
      return {
        state: { ...state, suppressed: false },
        intent: { kind: "eyeLine" },
      };
    case "word": {
      if (!trigger.following || state.suppressed) {
        return { state, intent: NO_SCROLL };
      }
      return { state, intent: { kind: "nearest" } };
    }
    case "manualScroll": {
      if (!trigger.following || state.suppressed) {
        return { state, intent: NO_SCROLL };
      }
      return { state: { ...state, suppressed: true }, intent: NO_SCROLL };
    }
    default:
      return { state, intent: NO_SCROLL };
  }
}

function scrollRange(viewportHeightPx: number, contentHeightPx: number): number {
  if (!Number.isFinite(viewportHeightPx) || !Number.isFinite(contentHeightPx)) {
    return 0;
  }
  return Math.max(0, contentHeightPx - viewportHeightPx);
}

export interface WordBox {
  topPx: number;
  heightPx: number;
}

export function eyeLineScrollTop(
  word: WordBox,
  viewportHeightPx: number,
  contentHeightPx: number,
): number | null {
  if (!Number.isFinite(word.topPx) || !Number.isFinite(word.heightPx)) {
    return null;
  }
  if (!Number.isFinite(viewportHeightPx) || viewportHeightPx <= 0) return null;
  const centre = word.topPx + word.heightPx / 2;
  const target = centre - viewportHeightPx * EYE_LINE_FRACTION;
  const range = scrollRange(viewportHeightPx, contentHeightPx);
  return Math.max(0, Math.min(range, Math.round(target)));
}

export function nearestScrollTop(
  word: WordBox,
  viewportHeightPx: number,
  contentHeightPx: number,
  currentTop: number,
): number | null {
  if (!Number.isFinite(word.topPx) || !Number.isFinite(word.heightPx)) {
    return null;
  }
  if (!Number.isFinite(viewportHeightPx) || viewportHeightPx <= 0) return null;
  if (!Number.isFinite(currentTop)) return null;
  const range = scrollRange(viewportHeightPx, contentHeightPx);
  const clamp = (value: number) => Math.max(0, Math.min(range, value));
  const margin = Math.min(NEAREST_MARGIN_PX, viewportHeightPx / 4);
  const top = currentTop + margin;
  const bottom = currentTop + viewportHeightPx - margin;
  const wordBottom = word.topPx + word.heightPx;
  if (word.topPx >= top && wordBottom <= bottom) return clamp(currentTop);
  if (word.heightPx + margin * 2 > viewportHeightPx) {
    return clamp(Math.round(word.topPx - margin));
  }
  if (word.topPx < top) return clamp(Math.round(word.topPx - margin));
  return clamp(Math.round(wordBottom + margin - viewportHeightPx));
}

export const DEADBAND_TOP_FRACTION = 0.15;
export const DEADBAND_BOTTOM_FRACTION = 0.75;

export function topicScrollTop(
  blockTopPx: number,
  viewportHeightPx: number,
  contentHeightPx: number,
  currentTop: number,
): number | null {
  if (!Number.isFinite(blockTopPx)) return null;
  if (!Number.isFinite(viewportHeightPx) || viewportHeightPx <= 0) return null;
  if (!Number.isFinite(currentTop)) return null;
  const range = scrollRange(viewportHeightPx, contentHeightPx);
  const clamp = (value: number) => Math.max(0, Math.min(range, value));

  const deadbandTop = currentTop + viewportHeightPx * DEADBAND_TOP_FRACTION;
  const deadbandBottom = currentTop + viewportHeightPx * DEADBAND_BOTTOM_FRACTION;
  if (blockTopPx >= deadbandTop && blockTopPx <= deadbandBottom) {
    return clamp(currentTop);
  }

  const target = blockTopPx - viewportHeightPx * EYE_LINE_FRACTION;
  return clamp(Math.round(target));
}

export function figureScrollTop(
  figureCenterPx: number,
  viewportHeightPx: number,
  contentHeightPx: number,
): number | null {
  if (!Number.isFinite(figureCenterPx)) return null;
  if (!Number.isFinite(viewportHeightPx) || viewportHeightPx <= 0) return null;
  const range = scrollRange(viewportHeightPx, contentHeightPx);
  const clamp = (value: number) => Math.max(0, Math.min(range, value));

  const target = figureCenterPx - viewportHeightPx / 2;
  return clamp(Math.round(target));
}

export interface AutoScrollTargets {
  container: HTMLElement | null;
  word: HTMLElement | null;
  envelope?: HTMLElement | null;
}

export interface AutoScrollInput {
  following: boolean;
  pageNo: number | null;
  wordSignature: string;
}

export interface AutoScrollApplier {
  apply: (targets: AutoScrollTargets, input: AutoScrollInput) => void;
  syncTeacher: (
    container: HTMLElement | null,
    targetTop: number | null,
    following: boolean,
  ) => void;
  commitPage: (targets: AutoScrollTargets) => void;
  returned: () => void;
  scrolled: (container: HTMLElement | null, following: boolean) => void;
  manualScroll: (following: boolean) => void;
  reset: () => void;
}

export function wordBoxIn(
  container: HTMLElement,
  word: HTMLElement | null,
): WordBox | null {
  if (word == null || word.hidden) return null;
  const box = word.getBoundingClientRect();
  if (box.height <= 0) return null;
  const frame = container.getBoundingClientRect();
  return {
    topPx: box.top - frame.top + container.scrollTop,
    heightPx: box.height,
  };
}

export function createAutoScrollApplier(): AutoScrollApplier {
  let state = INITIAL_AUTO_SCROLL;
  let signature: string | null = null;
  let pendingEyeLine = false;
  let expectedTop: number | null = null;
  let programmaticUntilMs = 0;

  const commit = (
    container: HTMLElement,
    top: number,
    smooth: boolean = false,
  ) => {
    if (Math.abs(container.scrollTop - top) < 1) {
      expectedTop = null;
      return;
    }
    expectedTop = top;
    if (smooth) {
      programmaticUntilMs = Date.now() + 500;
      container.scrollTo({ top, behavior: "smooth" });
    } else {
      container.scrollTo({ top, behavior: "auto" });
    }
  };

  const eyeLine = (targets: AutoScrollTargets) => {
    const container = targets.container;
    if (container == null) return;
    const word = wordBoxIn(container, targets.word);
    if (word == null) {
      const envelope = targets.envelope;
      if (envelope != null && !envelope.hidden) {
        const targetEl =
          (envelope.firstElementChild as HTMLElement | null) ?? envelope;
        const box = targetEl.getBoundingClientRect();
        if (box.height > 0) {
          const frame = container.getBoundingClientRect();
          const targetTop =
            box.top - frame.top + container.scrollTop - EYE_LINE_PADDING;
          const range = scrollRange(
            container.clientHeight,
            container.scrollHeight,
          );
          const top = Math.max(0, Math.min(range, Math.round(targetTop)));
          commit(container, top);
          return;
        }
      }
      commit(container, 0);
      return;
    }
    const top = eyeLineScrollTop(
      word,
      container.clientHeight,
      container.scrollHeight,
    );
    if (top != null) commit(container, top);
  };

  return {
    apply(targets, input) {
      const turn = autoScroll(state, {
        type: "shownPage",
        following: input.following,
        pageNo: input.pageNo,
      });
      state = turn.state;
      if (turn.intent.kind === "eyeLine") {
        signature = input.wordSignature;
        pendingEyeLine = true;
        return;
      }
      if (input.wordSignature === signature) return;
      signature = input.wordSignature;
      const word = autoScroll(state, {
        type: "word",
        following: input.following,
      });
      state = word.state;
      const container = targets.container;
      if (word.intent.kind !== "nearest" || container == null) return;
      const box = wordBoxIn(container, targets.word);
      if (box == null) return;
      const top = nearestScrollTop(
        box,
        container.clientHeight,
        container.scrollHeight,
        container.scrollTop,
      );
      if (top != null) commit(container, top);
    },
    syncTeacher(container, targetTop, following) {
      if (
        container == null ||
        targetTop == null ||
        !following ||
        state.suppressed
      ) {
        return;
      }
      commit(container, targetTop, true);
    },
    returned() {
      const outcome = autoScroll(state, { type: "returnedToNarration" });
      state = outcome.state;
      pendingEyeLine = outcome.intent.kind === "eyeLine";
    },
    commitPage(targets) {
      if (!pendingEyeLine) return;
      pendingEyeLine = false;
      if (state.suppressed) return;
      eyeLine(targets);
    },
    scrolled(container, following) {
      if (Date.now() < programmaticUntilMs) {
        return;
      }
      if (
        container != null &&
        expectedTop != null &&
        Math.abs(container.scrollTop - expectedTop) <= 2
      ) {
        expectedTop = null;
        return;
      }
      expectedTop = null;
      state = autoScroll(state, { type: "manualScroll", following }).state;
    },
    manualScroll(following) {
      expectedTop = null;
      programmaticUntilMs = 0;
      state = autoScroll(state, { type: "manualScroll", following }).state;
    },
    reset() {
      state = INITIAL_AUTO_SCROLL;
      signature = null;
      pendingEyeLine = false;
      expectedTop = null;
      programmaticUntilMs = 0;
    },
  };
}
