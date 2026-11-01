export const TUTOR_DOCK_KEY = "txtcaster.reading.tutor-dock";

export const FLOAT_MARGIN_PX = 16;
export const FLOAT_MAX_WIDTH_PX = 400;
const HEADER_CLEARANCE_PX = 72;
const PLAYER_CLEARANCE_PX = 176;
const FLOAT_MAX_HEIGHT_PX = 560;
const FLOAT_MIN_HEIGHT_PX = 160;

export type TutorDock =
  | { mode: "docked" }
  | { mode: "floating"; x: number; y: number; folded: boolean };

type Viewport = { width: number; height: number };

export function floatHeight(viewportHeight: number): number {
  return Math.max(
    FLOAT_MIN_HEIGHT_PX,
    Math.min(
      FLOAT_MAX_HEIGHT_PX,
      viewportHeight -
        HEADER_CLEARANCE_PX -
        PLAYER_CLEARANCE_PX -
        FLOAT_MARGIN_PX,
    ),
  );
}

export function clampFloat(
  pos: { x: number; y: number },
  width: number,
  height: number,
  viewport: Viewport,
): { x: number; y: number } {
  const maxX = Math.max(
    FLOAT_MARGIN_PX,
    viewport.width - width - FLOAT_MARGIN_PX,
  );
  const maxY = Math.max(
    FLOAT_MARGIN_PX,
    viewport.height - height - FLOAT_MARGIN_PX,
  );
  return {
    x: Math.round(Math.min(maxX, Math.max(FLOAT_MARGIN_PX, pos.x))),
    y: Math.round(Math.min(maxY, Math.max(FLOAT_MARGIN_PX, pos.y))),
  };
}

export function defaultFloat(
  viewport: Viewport,
  width: number,
  direction: "ltr" | "rtl",
): Extract<TutorDock, { mode: "floating" }> {
  return {
    mode: "floating",
    x:
      direction === "rtl"
        ? FLOAT_MARGIN_PX
        : viewport.width - width - FLOAT_MARGIN_PX,
    y: HEADER_CLEARANCE_PX,
    folded: false,
  };
}

export function loadDock(): TutorDock {
  try {
    const raw = localStorage.getItem(TUTOR_DOCK_KEY);
    if (raw == null) return { mode: "docked" };
    const value: unknown = JSON.parse(raw);
    if (
      typeof value === "object" &&
      value !== null &&
      "mode" in value &&
      value.mode === "floating" &&
      "x" in value &&
      "y" in value &&
      "folded" in value &&
      typeof value.x === "number" &&
      Number.isFinite(value.x) &&
      typeof value.y === "number" &&
      Number.isFinite(value.y) &&
      typeof value.folded === "boolean"
    ) {
      return { mode: "floating", x: value.x, y: value.y, folded: value.folded };
    }
    return { mode: "docked" };
  } catch {
    return { mode: "docked" };
  }
}

export function storeDock(dock: TutorDock): void {
  try {
    localStorage.setItem(TUTOR_DOCK_KEY, JSON.stringify(dock));
  } catch {
    return;
  }
}
