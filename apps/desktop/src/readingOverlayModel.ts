import type { AudioShelfItem } from "./audioShelfModel";

export type OverlayState = "none" | "reading";

export type OverlayAction =
  { type: "openReading" } | { type: "closeReading" } | { type: "stop" };

export function overlayState(
  current: OverlayState,
  action: OverlayAction,
): OverlayState {
  switch (action.type) {
    case "openReading":
      return "reading";
    case "closeReading":
      return current === "reading" ? "none" : current;
    case "stop":
      return "none";
  }
}

export function canFollowAlong(item: AudioShelfItem | null): boolean {
  if (item == null) return false;
  if (item.jobId == null) return false;
  if (item.stale) return false;
  if (item.format.toLowerCase() !== "mp3") return false;
  return true;
}
