// RIG-ONLY (never ship): F9 track-switch stutter drill under the REAL-STACK
// PROOF LAW. Drives the real playbackStore -> PlaybackEngine -> <audio> ->
// WebKitGTK -> GStreamer -> media server chain; no mocks anywhere. Gated by
// VITE_STUTTER_DRILL=1 and only ever imported from a dev build.
//
// Protocol (mirrors the #258 manual drill, measured this time):
//   phase "switch": N rapid track switches across real export files.
//   phase "soak":  let the last track play PLAY_MS, sampling the element
//                  every SAMPLE_MS: currentTime, buffered end, readyState,
//                  paused, networkState. Stutter metric = wall-clock vs
//                  currentTime advance (a smooth playback advances ~1:1).
// All samples POST to the rig collector on 127.0.0.1:9777.

import { dispatch, getSnapshot } from "./playbackStore";
import { getAudioElement } from "./playbackElement";
import type { AudioShelfItem } from "./audioShelfModel";

const COLLECTOR = "http://127.0.0.1:9777/log";

function log(kind: string, data: Record<string, unknown>): void {
  const line = JSON.stringify({ t: Date.now(), kind, ...data });
  void fetch(COLLECTOR, { method: "POST", body: line, keepalive: true }).catch(
    () => {},
  );
}

function item(id: string, path: string): AudioShelfItem {
  return {
    id,
    bookId: "drill-book",
    title: id,
    path,
    mode: "narration",
    format: path.endsWith(".wav") ? "wav" : "mp3",
    bytes: 0,
    stale: false,
    isScanned: false,
    jobId: null,
    createdAt: Date.now(),
  };
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

let tapped: HTMLAudioElement | null = null;
const EVENTS = [
  "loadstart",
  "emptied",
  "abort",
  "loadedmetadata",
  "canplay",
  "play",
  "playing",
  "pause",
  "seeking",
  "seeked",
  "ratechange",
  "durationchange",
  "ended",
  "error",
  "stalled",
  "suspend",
  "waiting",
];

function tapElement(): void {
  const audio = getAudioElement();
  if (audio == null || audio === tapped) return;
  tapped = audio;
  let n = 0;
  for (const name of EVENTS) {
    audio.addEventListener(name, () => {
      n += 1;
      if (n > 2000 && n % 100 !== 0) return;
      const snap = getSnapshot();
      log("ev", {
        n,
        storeP: snap.playing,
        storeE: snap.error,
        ev: name,
        ct: Number(audio.currentTime.toFixed(3)),
        rate: audio.playbackRate,
        paused: audio.paused,
        seekingP: audio.seeking,
        ready: audio.readyState,
        net: audio.networkState,
        err: audio.error?.code ?? null,
        dur: Number.isFinite(audio.duration)
          ? Number(audio.duration.toFixed(3))
          : null,
        src: audio.currentSrc.slice(-24),
      });
    });
  }
  log("tap-installed", {});
}

interface DrillConfig {
  tracks: { id: string; path: string }[];
  switches: number;
  switchGapMs: number;
  soakMs: number;
  sampleMs: number;
  label: string;
}

async function sample(
  label: string,
  phase: string,
  ms: number,
  everyMs: number,
) {
  const start = performance.now();
  let lastCt = -1;
  let lastWall = start;
  while (performance.now() - start < ms) {
    const audio = getAudioElement();
    const snap = getSnapshot();
    if (audio) {
      const wall = performance.now();
      const ct = audio.currentTime;
      const bufEnd =
        audio.buffered.length > 0
          ? audio.buffered.end(audio.buffered.length - 1)
          : -1;
      // advance ratio over the sample window: 1.0 = smooth, <1 = stalling
      const ratio =
        lastCt >= 0 && wall > lastWall
          ? (ct - lastCt) / ((wall - lastWall) / 1000)
          : null;
      log("sample", {
        label,
        phase,
        ct: Number(ct.toFixed(3)),
        ratio: ratio == null ? null : Number(ratio.toFixed(3)),
        bufEnd: Number(bufEnd.toFixed(3)),
        ranges: audio.buffered.length,
        ready: audio.readyState,
        net: audio.networkState,
        paused: audio.paused,
        rate: audio.playbackRate,
        seekingP: audio.seeking,
        err: audio.error?.code ?? null,
        dur: Number.isFinite(audio.duration)
          ? Number(audio.duration.toFixed(3))
          : null,
        src: audio.currentSrc.slice(-24),
        storePlaying: snap.playing,
        storeBuffering: snap.buffering,
        storeError: snap.error,
        storeItem: snap.item?.id ?? null,
      });
      lastCt = ct;
      lastWall = wall;
    }
    await sleep(everyMs);
  }
}

export async function runDrill(cfg: DrillConfig): Promise<void> {
  tapElement();
  const items = cfg.tracks.map((t) => item(t.id, t.path));
  if (items.length === 0) {
    log("drill-refused", { reason: "no tracks armed" });
    return;
  }
  log("drill-start", {
    label: cfg.label,
    cfg: { ...cfg, tracks: cfg.tracks.length },
  });
  // warmup: play track 0 for 3s so we have a known-good baseline
  dispatch({ type: "load", item: items[0]! });
  await sample(cfg.label, "baseline", 3000, cfg.sampleMs);
  for (let i = 1; i <= cfg.switches; i++) {
    const next = items[i % items.length]!;
    log("switch", { label: cfg.label, n: i, to: next.id });
    dispatch({ type: "load", item: next });
    await sleep(cfg.switchGapMs);
  }
  log("soak-begin", { label: cfg.label, on: getSnapshot().item?.id ?? null });
  await sample(cfg.label, "soak", cfg.soakMs, cfg.sampleMs);
  log("drill-end", { label: cfg.label });
}

declare global {
  interface Window {
    __stutterDrill: (cfg?: Partial<DrillConfig>) => Promise<void>;
  }
}

export function installDrill(): void {
  const defaults: DrillConfig = {
    tracks: [],
    switches: 20,
    switchGapMs: 400,
    soakMs: 30000,
    sampleMs: 500,
    label: "drill",
  };
  window.__stutterDrill = (cfg) => runDrill({ ...defaults, ...cfg });
  // Track list + run commands arrive from the rig collector, so the drill
  // needs no hardcoded paths and can be re-armed from the shell.
  let lastRun = 0;
  let running = false;
  const poll = async () => {
    tapElement();
    try {
      if (defaults.tracks.length === 0) {
        const tracks = (await (
          await fetch("http://127.0.0.1:9777/tracks")
        ).json()) as { id: string; path: string }[];
        defaults.tracks = tracks;
        log("drill-armed", { tracks: tracks.length });
      }
      const cmd = (await (await fetch("http://127.0.0.1:9777/cmd")).json()) as {
        run?: number;
      } & Partial<DrillConfig>;
      if (cmd.run != null && cmd.run > lastRun && !running) {
        lastRun = cmd.run;
        running = true;
        const rest = { ...cmd };
        delete rest.run;
        delete rest.tracks;
        await runDrill({ ...defaults, ...rest });
        running = false;
      }
    } catch {
      /* collector not up yet */
    }
    setTimeout(poll, 2000);
  };
  void poll();
}
