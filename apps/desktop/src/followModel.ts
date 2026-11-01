import type { TravelDirection } from "./readingFacsimileModel";

export type FollowState =
  | { mode: "following" }
  | { mode: "browsing"; pageNo: number };

export const FOLLOWING: FollowState = { mode: "following" };

export type FollowAction =
  | { type: "flip"; delta: number }
  | { type: "returnToNarration" }
  | { type: "seeked" }
  | { type: "chapterChanged" }
  | { type: "readingModeChanged" }
  | { type: "narrationPage"; pageNo: number | null };

export interface FollowContext {
  narrationPage: number | null;
  lastPage: number;
}

function clampPage(pageNo: number, lastPage: number): number {
  const floored = Math.max(1, pageNo);
  if (Number.isFinite(lastPage) && lastPage >= 1) {
    return Math.min(floored, lastPage);
  }
  return floored;
}

export function shownPage(
  state: FollowState,
  narrationPage: number | null,
): number | null {
  return state.mode === "browsing" ? state.pageNo : narrationPage;
}

export function seekNonceAdvanced(previous: number, next: number): boolean {
  return next > previous;
}

export function travelDirectionAfter(
  action: FollowAction,
  accepted: boolean,
  previous: TravelDirection,
): TravelDirection {
  if (!accepted || action.type !== "flip") return previous;
  return action.delta < 0 ? -1 : 1;
}

export function followReducer(
  state: FollowState,
  action: FollowAction,
  context: FollowContext,
): FollowState {
  switch (action.type) {
    case "flip": {
      if (!Number.isInteger(action.delta)) return state;
      const base =
        state.mode === "browsing" ? state.pageNo : context.narrationPage;
      if (base == null) return state;
      const target = base + action.delta;
      if (!Number.isFinite(target)) return state;
      return {
        mode: "browsing",
        pageNo: clampPage(target, context.lastPage),
      };
    }
    case "returnToNarration":
      return state.mode === "following" ? state : FOLLOWING;
    case "seeked":
      return state.mode === "following" ? state : FOLLOWING;
    case "chapterChanged":
      return state;
    case "readingModeChanged":
      return state;
    case "narrationPage":
      return state;
    default:
      return state;
  }
}
