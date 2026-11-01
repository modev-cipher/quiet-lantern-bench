import { invoke } from "@tauri-apps/api/core";

import {
  createRatedEmitter,
  decideRated,
  type EmitterPolicy,
  type RatedEmitter,
} from "./diagnosticRatePolicy";

export type ReadingClockCondition = "focused" | "visible-unfocused" | "hidden";

export interface ReadingClockSample {
  condition: ReadingClockCondition;
  chapterIdx: number | null;
  mediaMs: number;
  tickHz: number;
  currentTimeDeltaMs: number;
  deltaMaxMs: number;
  zeroDeltaPct: number;
  resyncCount: number;
  offsetMs: number | null;
  visibilityState: string;
  focused: boolean;
}

export interface ReadingClockScope {
  userAgent: string;
  webkitVersion: string | null;
  platform: string;
  audioBackend: string | null;
  fixtureChapters: number;
  fixtureWords: number;
  fixtureShape: string;
}

const NOT_AVAILABLE = "n/a";

export const SUPPRESSED_HEARTBEAT_EVERY = 15;

export interface SuppressionState {
  lastEmittedMediaMs: number | null;
  suppressed: number;
}

export const INITIAL_SUPPRESSION: SuppressionState = {
  lastEmittedMediaMs: null,
  suppressed: 0,
};

export interface SuppressionDecision {
  emit: boolean;
  heartbeat: boolean;
  suppressedRun: number;
  next: SuppressionState;
}

const CLOCK_POLICY: EmitterPolicy = {
  minIntervalMs: Number.POSITIVE_INFINITY,
  heartbeatEvery: SUPPRESSED_HEARTBEAT_EVERY,
};

export function decideEmission(
  state: SuppressionState,
  sample: ReadingClockSample,
): SuppressionDecision {
  const rated = decideRated(
    CLOCK_POLICY,
    {
      lastKey:
        state.lastEmittedMediaMs == null
          ? null
          : String(state.lastEmittedMediaMs),
      lastEmitAtMs: 0,
      suppressed: state.suppressed,
    },
    String(sample.mediaMs),
    !(sample.tickHz > 0) || sample.zeroDeltaPct >= 100,
    0,
  );
  return {
    emit: rated.emit,
    heartbeat: rated.heartbeat,
    suppressedRun: rated.suppressedRun,
    next: {
      lastEmittedMediaMs: rated.emit
        ? sample.mediaMs
        : state.lastEmittedMediaMs,
      suppressed: rated.next.suppressed,
    },
  };
}

export function stamp(now: number): string {
  return new Date(now).toISOString();
}

function text(value: string | null | undefined): string {
  if (value == null) return NOT_AVAILABLE;
  const collapsed = value.trim().replace(/\s+/g, "_");
  return collapsed || NOT_AVAILABLE;
}

function number(value: number | null | undefined, decimals = 0): string {
  if (value == null || !Number.isFinite(value)) return NOT_AVAILABLE;
  return value.toFixed(decimals);
}

export function formatReadingClockSample(
  sample: ReadingClockSample,
  now: number = Date.now(),
): string {
  return [
    `at=${stamp(now)}`,
    `condition=${text(sample.condition)}`,
    `chapterIdx=${sample.chapterIdx == null ? NOT_AVAILABLE : number(sample.chapterIdx)}`,
    `mediaMs=${number(sample.mediaMs)}`,
    `tickHz=${number(sample.tickHz, 1)}`,
    `currentTimeDeltaMs=${number(sample.currentTimeDeltaMs, 1)}`,
    `deltaMaxMs=${number(sample.deltaMaxMs, 1)}`,
    `zeroDeltaPct=${number(sample.zeroDeltaPct)}`,
    `resyncCount=${number(sample.resyncCount)}`,
    `offsetMs=${number(sample.offsetMs)}`,
    `visibilityState=${text(sample.visibilityState)}`,
    `focused=${sample.focused ? "yes" : "no"}`,
  ].join(" ");
}

export function formatReadingClockScope(scope: ReadingClockScope): string {
  return [
    `platform=${text(scope.platform)}`,
    `webkit=${text(scope.webkitVersion)}`,
    `audioBackend=${text(scope.audioBackend)}`,
    `fixtureChapters=${number(scope.fixtureChapters)}`,
    `fixtureWords=${number(scope.fixtureWords)}`,
    `fixtureShape=${text(scope.fixtureShape)}`,
    `userAgent=${text(scope.userAgent)}`,
  ].join(" ");
}

export interface ReadingClockEnvironment {
  hidden: boolean;
  visibilityState: string;
  focused: boolean;
}

export function conditionFor(
  environment: ReadingClockEnvironment,
): ReadingClockCondition {
  if (environment.hidden) return "hidden";
  return environment.focused ? "focused" : "visible-unfocused";
}

export interface ReadingClockWindow {
  chapterIdx: number | null;
  mediaMs: number;
  ticks: number;
  resyncCount: number;
  deltaSumMs: number;
  deltaMaxMs: number;
  zeroDeltaTicks: number;
  elapsedMs: number;
  offsetMs: number | null;
}

export function readingClockSample(
  environment: ReadingClockEnvironment,
  window: ReadingClockWindow,
): ReadingClockSample {
  return {
    condition: conditionFor(environment),
    chapterIdx: window.chapterIdx,
    mediaMs: window.mediaMs,
    tickHz: window.elapsedMs > 0 ? (window.ticks * 1000) / window.elapsedMs : Number.NaN,
    currentTimeDeltaMs: window.ticks > 0 ? window.deltaSumMs / window.ticks : Number.NaN,
    deltaMaxMs: window.deltaMaxMs,
    zeroDeltaPct: window.ticks > 0 ? (100 * window.zeroDeltaTicks) / window.ticks : Number.NaN,
    resyncCount: window.resyncCount,
    offsetMs: window.offsetMs,
    visibilityState: environment.visibilityState,
    focused: environment.focused,
  };
}

export function documentEnvironment(): ReadingClockEnvironment {
  return {
    hidden: document.hidden,
    visibilityState: document.visibilityState,
    focused: document.hasFocus(),
  };
}

export function sampleReadingClock(
  window: ReadingClockWindow,
): ReadingClockSample {
  return readingClockSample(documentEnvironment(), window);
}

export function readingClockScope(
  fixtureChapters: number,
  fixtureWords: number,
  fixtureShape: string,
): ReadingClockScope {
  const userAgent = navigator.userAgent;
  const webkit = /AppleWebKit\/([\d.]+)/.exec(userAgent);
  return {
    userAgent,
    webkitVersion: webkit == null ? null : webkit[1],
    platform: navigator.platform,
    audioBackend: null,
    fixtureChapters,
    fixtureWords,
    fixtureShape,
  };
}

export async function reportReadingClock(
  sample: ReadingClockSample,
  scope: ReadingClockScope,
  suppressedRun = 0,
): Promise<void> {
  const idle = suppressedRun > 0 ? ` idleWindows=${suppressedRun}` : "";
  try {
    await invoke("log_reading_clock", {
      line: `${formatReadingClockSample(sample)}${idle} ${formatReadingClockScope(scope)}`,
    });
  } catch {
    return;
  }
}

export type PlaybackEventName = "pause" | "error" | "exit";

export function formatPlaybackEvent(
  event: PlaybackEventName,
  mediaSeconds: number | null,
  now: number = Date.now(),
): string {
  const media =
    mediaSeconds == null || !Number.isFinite(mediaSeconds)
      ? NOT_AVAILABLE
      : Math.round(mediaSeconds * 1000).toString();
  return `media at=${stamp(now)} event=${event} mediaMs=${media}`;
}

const PLAYBACK_POLICY: EmitterPolicy = {
  minIntervalMs: 2_000,
  heartbeatEvery: 15,
};

interface PlaybackPayload {
  event: PlaybackEventName;
  mediaSeconds: number | null;
}

function mediaKey(mediaSeconds: number | null): string {
  if (mediaSeconds == null || !Number.isFinite(mediaSeconds)) {
    return NOT_AVAILABLE;
  }
  return Math.round(mediaSeconds * 1000).toString();
}

function playbackLine(payload: PlaybackPayload, suppressedRun: number): string {
  const base = formatPlaybackEvent(payload.event, payload.mediaSeconds);
  return suppressedRun > 0 ? `${base} suppressed=${suppressedRun}` : base;
}

async function sendDiagnosticLine(line: string): Promise<void> {
  try {
    await invoke("log_reading_clock", { line });
  } catch {
    return;
  }
}

function playbackEmitter(
  keyOf: (payload: PlaybackPayload) => string,
): RatedEmitter<PlaybackPayload> {
  return createRatedEmitter<PlaybackPayload>({
    policy: PLAYBACK_POLICY,
    keyOf,
    format: playbackLine,
    send: sendDiagnosticLine,
  });
}

const PLAYBACK_EMITTERS: Record<
  PlaybackEventName,
  RatedEmitter<PlaybackPayload>
> = {
  pause: playbackEmitter(() => "pause"),
  error: playbackEmitter(
    (payload) => `error@${mediaKey(payload.mediaSeconds)}`,
  ),
  exit: playbackEmitter((payload) => `exit@${mediaKey(payload.mediaSeconds)}`),
};

export function resetPlaybackEmitters(): void {
  for (const emitter of Object.values(PLAYBACK_EMITTERS)) {
    emitter.reset();
  }
}

export async function reportPlaybackEvent(
  event: PlaybackEventName,
  mediaSeconds: number | null,
): Promise<void> {
  await PLAYBACK_EMITTERS[event].report({ event, mediaSeconds });
}
