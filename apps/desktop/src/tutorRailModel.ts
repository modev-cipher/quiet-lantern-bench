// The resizable tutor rail: pure geometry + persistence for the companion
// sidebar's inline size. The drag handle in StudyCompanion feeds pointer
// positions through these functions; ReadingView owns the state (the
// companion itself is gate-banned from useState so Focus exit restores it).

export const TUTOR_RAIL_WIDTH_KEY = "txtcaster.reading.tutor-width";

/** Matches the top of the old `clamp(232px,22vw,288px)` resting width. */
export const RAIL_DEFAULT_PX = 288;
/** The conversation column stops being usable below this. */
export const RAIL_MIN_PX = 280;
/** The rail never crowds the page past this share of the viewport. */
export const RAIL_MAX_VIEWPORT_FRACTION = 0.55;
/** Arrow-key resize step when the handle is focused. */
export const RAIL_KEYBOARD_STEP_PX = 16;

export function railMaxPx(viewportWidth: number): number {
  // A tiny viewport must not invert the clamp range.
  return Math.max(
    RAIL_MIN_PX,
    Math.round(viewportWidth * RAIL_MAX_VIEWPORT_FRACTION),
  );
}

export function clampRailWidth(px: number, viewportWidth: number): number {
  if (!Number.isFinite(px)) return RAIL_DEFAULT_PX;
  return Math.min(
    railMaxPx(viewportWidth),
    Math.max(RAIL_MIN_PX, Math.round(px)),
  );
}

/**
 * Width implied by a pointer at `clientX` dragging the rail's inline-start
 * edge. The rail sits at the inline end, so in RTL the geometry mirrors:
 * the draggable edge is the rect's RIGHT side and growth points rightward.
 */
export function railWidthFromPointer(
  clientX: number,
  rect: { left: number; right: number },
  direction: "ltr" | "rtl",
): number {
  return direction === "rtl" ? clientX - rect.left : rect.right - clientX;
}

export function loadRailWidth(viewportWidth: number): number {
  try {
    const raw = localStorage.getItem(TUTOR_RAIL_WIDTH_KEY);
    if (raw == null) return clampRailWidth(RAIL_DEFAULT_PX, viewportWidth);
    return clampRailWidth(Number(raw), viewportWidth);
  } catch {
    return clampRailWidth(RAIL_DEFAULT_PX, viewportWidth);
  }
}

export function storeRailWidth(px: number): void {
  try {
    localStorage.setItem(TUTOR_RAIL_WIDTH_KEY, String(Math.round(px)));
  } catch {
    return;
  }
}

export function resetRailWidth(): void {
  try {
    localStorage.removeItem(TUTOR_RAIL_WIDTH_KEY);
  } catch {
    return;
  }
}
