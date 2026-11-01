import { invoke } from "@tauri-apps/api/core";

import type { FollowAction, FollowState } from "./followModel";
import type { TapOutcome } from "./readingPageModel";

export function describeFollowState(state: FollowState): string {
  return state.mode === "browsing" ? `browsing:${state.pageNo}` : "following";
}

export interface FollowTransition {
  trigger: FollowAction["type"];
  from: FollowState;
  to: FollowState;
}

export function followTransitionLine(
  transition: FollowTransition,
): string | null {
  const from = describeFollowState(transition.from);
  const to = describeFollowState(transition.to);
  if (from === to) return null;
  const seek = transition.trigger === "seeked" ? "yes" : "no";
  return `state=${transition.to.mode} trigger=${transition.trigger} from=${from} to=${to} seek=${seek}`;
}

function rounded(value: number): number {
  return Math.round(value * 10) / 10;
}

export function tapLine(outcome: TapOutcome): string {
  const at =
    outcome.xPt == null || outcome.yPt == null
      ? "unmapped"
      : `${rounded(outcome.xPt)},${rounded(outcome.yPt)}`;
  const page = outcome.pageNo == null ? "none" : outcome.pageNo;
  const hit =
    outcome.hit == null ? "none" : `word seekMs=${outcome.hit.startMs}`;
  return `${at} page=${page} hit=${hit}`;
}

export async function reportFollowState(line: string): Promise<void> {
  try {
    await invoke("log_follow_state", { line });
  } catch {
    return;
  }
}

export async function reportReadingTap(line: string): Promise<void> {
  try {
    await invoke("log_reading_tap", { line });
  } catch {
    return;
  }
}
