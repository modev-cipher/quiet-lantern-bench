import { nextRate, seekTime, skipTarget } from "./audioPlayerModel";
import type { AudioShelfItem } from "./audioShelfModel";

export interface PlaybackState {
  item: AudioShelfItem | null;
  playing: boolean;
  current: number;
  duration: number;
  buffering: boolean;
  rate: number;
  muted: boolean;
  volume: number;
  fade: number;
  error: boolean;
  errorMessage: string | null;
  seekNonce: number;
  seekTo: number | null;
}

export type PlaybackAction =
  /**
   * `autoplay` (default true) is the caller's play intent. A user pressing
   * Play/Study means playback and omits it; a Talk entry mounts the reader
   * around a live tutor and must be playback-neutral — `autoplay: false`
   * loads the item ready-but-paused, with no transient `playing: true`
   * state the engine could race into an audible start.
   */
  | { type: "load"; item: AudioShelfItem; autoplay?: boolean }
  | { type: "play" }
  | { type: "pause" }
  | { type: "toggle" }
  | { type: "seekFraction"; fraction: number }
  | { type: "seekMs"; ms: number }
  | { type: "skip"; delta: number }
  | { type: "cycleRate" }
  | { type: "setRate"; rate: number }
  | { type: "toggleMute" }
  | { type: "setVolume"; volume: number }
  | { type: "setFade"; fade: number }
  | { type: "stop" }
  | { type: "engineTime"; current: number }
  | { type: "engineDuration"; duration: number }
  | { type: "engineBuffering"; buffering: boolean }
  | { type: "enginePlaying"; playing: boolean }
  | { type: "engineEnded" }
  | { type: "engineError"; message?: string }
  | { type: "seekConsumed" };

export const initialPlaybackState: PlaybackState = {
  item: null,
  playing: false,
  current: 0,
  duration: Number.NaN,
  buffering: false,
  rate: 1,
  muted: false,
  volume: 1,
  fade: 1,
  error: false,
  errorMessage: null,
  seekNonce: 0,
  seekTo: null,
};

function withSeek(state: PlaybackState, target: number): PlaybackState {
  return {
    ...state,
    current: target,
    seekTo: target,
    seekNonce: state.seekNonce + 1,
  };
}

export function playbackReducer(
  state: PlaybackState,
  action: PlaybackAction,
): PlaybackState {
  switch (action.type) {
    case "load": {
      const autoplay = action.autoplay !== false;
      const sameItem =
        state.item?.id === action.item.id &&
        state.item?.path === action.item.path;
      if (sameItem) {
        // Neutral load of the already-loaded item leaves `playing` alone:
        // it neither issues a play nor pauses audio the user started.
        return {
          ...state,
          playing: autoplay ? true : state.playing,
          error: false,
          errorMessage: null,
        };
      }
      return {
        ...initialPlaybackState,
        item: action.item,
        rate: state.rate,
        muted: state.muted,
        volume: state.volume,
        playing: autoplay,
      };
    }
    case "play":
      return state.item
        ? { ...state, playing: true, error: false, errorMessage: null }
        : state;
    case "pause":
      return { ...state, playing: false };
    case "toggle":
      return state.item ? { ...state, playing: !state.playing } : state;
    case "seekFraction":
      return withSeek(state, seekTime(action.fraction, state.duration));
    case "seekMs":
      if (!Number.isFinite(action.ms)) return state;
      return withSeek(state, skipTarget(0, action.ms / 1000, state.duration));
    case "skip":
      return withSeek(
        state,
        skipTarget(state.current, action.delta, state.duration),
      );
    case "cycleRate":
      return { ...state, rate: nextRate(state.rate) };
    case "setRate":
      // Absolute rate for the voice-tool bridge (dispatcher-validated to
      // [0.5, 3.0] upstream); clamped again so a bad payload cannot wedge
      // the audio element.
      if (!Number.isFinite(action.rate)) return state;
      return { ...state, rate: Math.min(3, Math.max(0.5, action.rate)) };
    case "toggleMute":
      return { ...state, muted: !state.muted };
    case "setVolume":
      if (!Number.isFinite(action.volume)) return state;
      return { ...state, volume: Math.min(1, Math.max(0, action.volume)) };
    case "setFade":
      if (!Number.isFinite(action.fade)) return state;
      return { ...state, fade: Math.min(1, Math.max(0, action.fade)) };
    case "stop":
      return {
        ...initialPlaybackState,
        rate: state.rate,
        muted: state.muted,
        volume: state.volume,
      };
    case "engineTime":
      return { ...state, current: action.current };
    case "engineDuration":
      return { ...state, duration: action.duration };
    case "engineBuffering":
      return { ...state, buffering: action.buffering };
    case "enginePlaying":
      return { ...state, playing: action.playing };
    case "engineEnded":
      return { ...state, playing: false, current: 0 };
    case "engineError":
      return {
        ...state,
        playing: false,
        error: true,
        errorMessage: action.message ?? null,
        buffering: false,
      };
    case "seekConsumed":
      return { ...state, seekTo: null };
    default:
      return state;
  }
}

let state = initialPlaybackState;
const listeners = new Set<() => void>();

export function getSnapshot(): PlaybackState {
  return state;
}

export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function dispatch(action: PlaybackAction): void {
  const next = playbackReducer(state, action);
  if (next === state) return;
  state = next;
  for (const listener of listeners) listener();
}

export function resetPlaybackStore(): void {
  state = initialPlaybackState;
  for (const listener of listeners) listener();
}
