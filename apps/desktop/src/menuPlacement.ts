const GAP = 6;
const EDGE = 8;

interface Rect {
  top: number;
  bottom: number;
  left: number;
  right: number;
}

export function menuPlacement(
  trigger: Rect,
  menu: { width: number; height: number },
  viewport: { width: number; height: number },
  direction: "ltr" | "rtl",
): { top: number; left: number; side: "below" | "above" } {
  const fitsBelow =
    trigger.bottom + GAP + menu.height <= viewport.height - EDGE;
  const fitsAbove = trigger.top - GAP - menu.height >= EDGE;
  const side = fitsBelow || !fitsAbove ? "below" : "above";
  const top =
    side === "below" ? trigger.bottom + GAP : trigger.top - GAP - menu.height;
  const preferred =
    direction === "rtl" ? trigger.left : trigger.right - menu.width;
  const left = Math.min(
    Math.max(EDGE, preferred),
    viewport.width - EDGE - menu.width,
  );
  return { top: Math.round(top), left: Math.round(left), side };
}
