// The webview half of the VA6 voice playback bridge (Rust half:
// src-tauri/src/voice_playback.rs). Two directions:
//
// - Inbound effects: Rust resolves every voice tool into one absolute-state
//   effect ("be paused", "seek to exactly this position") delivered on the
//   "voice-playback-effect" event; `applyPlaybackEffect` maps each onto a
//   playbackStore action. Absolute state means a duplicate delivery is a
//   no-op — the once-only guarantee lives upstream in the dispatcher's
//   answer cache, this side just has to stay harmless.
// - Outbound reports: Rust needs to know where the player is (seek
//   arithmetic) and whether a playing surface exists at all
//   (PlaybackUnavailable). `surfaceReport` snapshots the store; the bridge
//   reports on meaningful changes and on a heartbeat while a track is
//   loaded — the heartbeat doubles as the liveness proof, because Rust
//   treats a report older than its TTL as a dead surface. When the track
//   unloads the bridge detaches explicitly.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import {
  dispatch,
  getSnapshot,
  subscribe,
  type PlaybackState,
} from "./playbackStore";

/** Matches SURFACE_TTL (4 s) on the Rust side with three beats of margin. */
export const REPORT_HEARTBEAT_MS = 1_000;

export type PlaybackEffect =
  | { op: "pause" }
  | { op: "resume" }
  | { op: "seek"; pos_ms: number; epoch: number }
  | { op: "speed"; speed: number };

export interface SurfaceReport {
  posMs: number;
  playing: boolean;
  speed: number;
}

export interface BridgeReport extends SurfaceReport {
  seekEpoch: number | null;
}

/** Maps one Rust effect onto the store action that realizes it. */
export function playbackEffectAction(effect: PlaybackEffect) {
  switch (effect.op) {
    case "pause":
      return { type: "pause" } as const;
    case "resume":
      return { type: "play" } as const;
    case "seek":
      return { type: "seekMs", ms: effect.pos_ms } as const;
    case "speed":
      return { type: "setRate", rate: effect.speed } as const;
    default:
      return null;
  }
}

export function applyPlaybackEffect(effect: PlaybackEffect): void {
  const action = playbackEffectAction(effect);
  if (action) dispatch(action);
}

/**
 * The player snapshot Rust needs, or null when no track is loaded (the
 * surface does not exist — Rust must see PlaybackUnavailable, not a frozen
 * position).
 */
export function surfaceReport(state: PlaybackState): SurfaceReport | null {
  if (!state.item) return null;
  const seconds = Number.isFinite(state.current) ? state.current : 0;
  return {
    posMs: Math.max(0, Math.round(seconds * 1000)),
    playing: state.playing,
    speed: state.rate,
  };
}

/**
 * Whether a store change is worth an immediate report (beyond the
 * heartbeat): surface identity, play/pause, rate, or a committed seek. Raw
 * timeupdate ticks are not — the heartbeat carries those.
 */
export function reportWorthy(
  prev: PlaybackState,
  next: PlaybackState,
): boolean {
  return (
    (prev.item === null) !== (next.item === null) ||
    prev.playing !== next.playing ||
    prev.rate !== next.rate ||
    prev.seekNonce !== next.seekNonce
  );
}

interface BridgeIo {
  report: (report: BridgeReport) => Promise<void>;
  detach: () => Promise<void>;
  onEffect: (handler: (effect: PlaybackEffect) => void) => Promise<() => void>;
  setInterval: (cb: () => void, ms: number) => ReturnType<typeof setInterval>;
  clearInterval: (id: ReturnType<typeof setInterval>) => void;
}

function tauriIo(): BridgeIo {
  return {
    report: (report) => invoke("voice_playback_report", { ...report }),
    detach: () => invoke("voice_playback_detach"),
    onEffect: (handler) =>
      listen<PlaybackEffect>("voice-playback-effect", (event) =>
        handler(event.payload),
      ),
    setInterval: (cb, ms) => setInterval(cb, ms),
    clearInterval: (id) => clearInterval(id),
  };
}

/**
 * Wires both directions against the live store and Tauri IPC. Returns a
 * cleanup that detaches the surface and stops listening.
 */
export function startVoicePlaybackBridge(io: BridgeIo = tauriIo()): () => void {
  let active = true;
  let last = getSnapshot();
  let attached = false;
  let seekEpoch: number | null = null;

  const push = () => {
    if (!active) return;
    const report = surfaceReport(getSnapshot());
    if (report) {
      attached = true;
      io.report({ ...report, seekEpoch }).catch(console.error);
    } else if (attached) {
      attached = false;
      io.detach().catch(console.error);
    }
  };

  const unsubscribe = subscribe(() => {
    const next = getSnapshot();
    const worthy = reportWorthy(last, next);
    last = next;
    if (worthy) push();
  });
  const heartbeat = io.setInterval(push, REPORT_HEARTBEAT_MS);
  const unlisten = io.onEffect((effect) => {
    if (!active) return;
    if (effect.op === "seek") seekEpoch = effect.epoch;
    applyPlaybackEffect(effect);
  });
  push();

  return () => {
    active = false;
    unsubscribe();
    io.clearInterval(heartbeat);
    unlisten.then((fn) => fn()).catch(console.error);
    if (attached) io.detach().catch(console.error);
  };
}
