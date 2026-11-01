import { useSyncExternalStore } from "react";

import { listenedFraction } from "./listeningProgress";
import { getSnapshot, subscribe, type PlaybackState } from "./playbackStore";

export function usePlayback<T>(selector: (state: PlaybackState) => T): T {
  return useSyncExternalStore(subscribe, () => selector(getSnapshot()));
}

export type CardPlayback = "idle" | "active" | "playing";

export function cardListened(
  state: PlaybackState,
  id: string,
  path: string,
): number | null {
  if (state.item?.id !== id || state.item?.path !== path) return null;
  return listenedFraction(state.current, state.duration);
}

export function cardPlayback(
  state: PlaybackState,
  id: string,
  path: string,
): CardPlayback {
  if (state.item?.id !== id || state.item?.path !== path) return "idle";
  return state.playing ? "playing" : "active";
}
