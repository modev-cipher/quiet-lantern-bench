import { afterEach, describe, expect, it } from "vitest";

import type { AudioShelfItem } from "./audioShelfModel";
import {
  dispatch,
  getSnapshot,
  playbackReducer,
  initialPlaybackState,
  resetPlaybackStore,
} from "./playbackStore";
import {
  applyPlaybackEffect,
  playbackEffectAction,
  REPORT_HEARTBEAT_MS,
  reportWorthy,
  startVoicePlaybackBridge,
  surfaceReport,
  type PlaybackEffect,
  type BridgeReport,
} from "./voicePlaybackBridge";

function item(overrides: Partial<AudioShelfItem> = {}): AudioShelfItem {
  return {
    id: "job-1",
    bookId: "book-1",
    title: "A Book",
    path: "/exports/a.mp3",
    mode: "narrator",
    format: "mp3",
    bytes: 1024,
    stale: false,
    isScanned: false,
    jobId: "job-1",
    createdAt: 1,
    ...overrides,
  };
}

afterEach(() => {
  resetPlaybackStore();
});

describe("playbackEffectAction maps Rust's absolute effects onto store actions", () => {
  it("pause / resume / seek / speed, one action each", () => {
    expect(playbackEffectAction({ op: "pause" })).toEqual({ type: "pause" });
    expect(playbackEffectAction({ op: "resume" })).toEqual({ type: "play" });
    expect(
      playbackEffectAction({ op: "seek", pos_ms: 41_000, epoch: 1 }),
    ).toEqual({
      type: "seekMs",
      ms: 41_000,
    });
    expect(playbackEffectAction({ op: "speed", speed: 1.5 })).toEqual({
      type: "setRate",
      rate: 1.5,
    });
  });

  it("an unknown op maps to nothing and applies nothing", () => {
    const unknown = { op: "explode" } as unknown as PlaybackEffect;
    expect(playbackEffectAction(unknown)).toBeNull();
    const before = getSnapshot();
    applyPlaybackEffect(unknown);
    expect(getSnapshot()).toBe(before);
  });
});

describe("duplicate delivery is a no-op — effects are absolute state, never toggles", () => {
  const observable = ({
    playing,
    current,
    rate,
    seekTo,
  }: typeof initialPlaybackState) => ({
    playing,
    current,
    rate,
    seekTo,
  });

  it("a second pause leaves the player exactly where the first put it", () => {
    dispatch({ type: "load", item: item() });
    applyPlaybackEffect({ op: "pause" });
    const once = observable(getSnapshot());
    applyPlaybackEffect({ op: "pause" });
    expect(observable(getSnapshot())).toEqual(once);
    expect(once.playing).toBe(false);
  });

  it("a second resume, likewise", () => {
    dispatch({ type: "load", item: item() });
    dispatch({ type: "pause" });
    applyPlaybackEffect({ op: "resume" });
    const once = observable(getSnapshot());
    applyPlaybackEffect({ op: "resume" });
    expect(observable(getSnapshot())).toEqual(once);
    expect(once.playing).toBe(true);
  });

  it("a re-delivered seek re-targets the same absolute position — never a relative jump", () => {
    dispatch({ type: "load", item: item() });
    dispatch({ type: "engineDuration", duration: 120 });
    applyPlaybackEffect({ op: "seek", pos_ms: 30_000, epoch: 1 });
    const once = observable(getSnapshot());
    applyPlaybackEffect({ op: "seek", pos_ms: 30_000, epoch: 1 });
    expect(observable(getSnapshot())).toEqual(once);
    expect(once.current).toBe(30);
  });

  it("a re-delivered speed holds the same absolute rate", () => {
    dispatch({ type: "load", item: item() });
    applyPlaybackEffect({ op: "speed", speed: 2 });
    applyPlaybackEffect({ op: "speed", speed: 2 });
    expect(getSnapshot().rate).toBe(2);
  });
});

describe("setRate is clamped so a bad payload cannot wedge the audio element", () => {
  const rateAfter = (rate: number) =>
    playbackReducer(
      { ...initialPlaybackState, item: item() },
      { type: "setRate", rate },
    ).rate;

  it("clamps to the dispatcher's [0.5, 3] bounds", () => {
    expect(rateAfter(0.1)).toBe(0.5);
    expect(rateAfter(99)).toBe(3);
    expect(rateAfter(1.25)).toBe(1.25);
  });

  it("refuses non-finite rates outright", () => {
    expect(rateAfter(Number.NaN)).toBe(1);
    expect(rateAfter(Number.POSITIVE_INFINITY)).toBe(1);
    expect(rateAfter(Number.NEGATIVE_INFINITY)).toBe(1);
  });
});

describe("surfaceReport", () => {
  it("is null with no loaded track — Rust must see PlaybackUnavailable, not a frozen player", () => {
    expect(surfaceReport(initialPlaybackState)).toBeNull();
  });

  it("reports the item clock in rounded milliseconds with playing and rate", () => {
    const state = {
      ...initialPlaybackState,
      item: item(),
      current: 12.3456,
      playing: true,
      rate: 1.5,
    };
    expect(surfaceReport(state)).toEqual({
      posMs: 12_346,
      playing: true,
      speed: 1.5,
    });
  });

  it("maps a non-finite clock to position zero rather than poisoning the seam", () => {
    const state = {
      ...initialPlaybackState,
      item: item(),
      current: Number.NaN,
    };
    expect(surfaceReport(state)?.posMs).toBe(0);
  });
});

describe("reportWorthy", () => {
  const base = { ...initialPlaybackState, item: item() };

  it("surface identity, play state, rate and committed seeks are worth an immediate report", () => {
    expect(reportWorthy(initialPlaybackState, base)).toBe(true);
    expect(reportWorthy(base, initialPlaybackState)).toBe(true);
    expect(reportWorthy(base, { ...base, playing: true })).toBe(true);
    expect(reportWorthy(base, { ...base, rate: 2 })).toBe(true);
    expect(reportWorthy(base, { ...base, seekNonce: 1 })).toBe(true);
  });

  it("raw timeupdate ticks are not — the heartbeat carries those", () => {
    expect(reportWorthy(base, { ...base, current: 55.5 })).toBe(false);
    expect(
      reportWorthy(base, { ...base, duration: 300, buffering: true }),
    ).toBe(false);
  });
});

interface FakeIo {
  reports: BridgeReport[];
  detaches: number;
  handler: ((effect: PlaybackEffect) => void) | null;
  unlistened: number;
  tick: (() => void) | null;
  cleared: boolean;
}

function fakeIo() {
  const io: FakeIo = {
    reports: [],
    detaches: 0,
    handler: null,
    unlistened: 0,
    tick: null,
    cleared: false,
  };
  const bridgeIo = {
    report: (report: BridgeReport) => {
      io.reports.push(report);
      return Promise.resolve();
    },
    detach: () => {
      io.detaches += 1;
      return Promise.resolve();
    },
    onEffect: (handler: (effect: PlaybackEffect) => void) => {
      io.handler = handler;
      return Promise.resolve(() => {
        io.unlistened += 1;
      });
    },
    setInterval: (cb: () => void, ms: number) => {
      expect(ms).toBe(REPORT_HEARTBEAT_MS);
      io.tick = cb;
      return 0 as unknown as ReturnType<typeof setInterval>;
    },
    clearInterval: () => {
      io.cleared = true;
    },
  };
  return { io, bridgeIo };
}

describe("startVoicePlaybackBridge lifecycle", () => {
  it("reports immediately on worthy changes, never on bare time ticks", () => {
    const { io, bridgeIo } = fakeIo();
    const stop = startVoicePlaybackBridge(bridgeIo);
    expect(io.reports).toEqual([]);

    dispatch({ type: "load", item: item() });
    expect(io.reports).toHaveLength(1);
    expect(io.reports[0]).toEqual({
      posMs: 0,
      playing: true,
      speed: 1,
      seekEpoch: null,
    });

    dispatch({ type: "engineTime", current: 7 });
    expect(io.reports).toHaveLength(1);

    dispatch({ type: "pause" });
    expect(io.reports).toHaveLength(2);
    expect(io.reports[1].playing).toBe(false);
    stop();
  });

  it("the heartbeat is the liveness proof: each tick re-reports while a track is loaded", () => {
    const { io, bridgeIo } = fakeIo();
    const stop = startVoicePlaybackBridge(bridgeIo);
    dispatch({ type: "load", item: item() });
    dispatch({ type: "engineTime", current: 2.5 });
    const before = io.reports.length;
    io.tick?.();
    expect(io.reports).toHaveLength(before + 1);
    expect(io.reports[io.reports.length - 1]).toEqual({
      posMs: 2_500,
      playing: true,
      speed: 1,
      seekEpoch: null,
    });
    stop();
  });

  it("detaches when the track unloads, and only after having attached", () => {
    const { io, bridgeIo } = fakeIo();
    const stop = startVoicePlaybackBridge(bridgeIo);
    io.tick?.();
    expect(io.detaches).toBe(0);

    dispatch({ type: "load", item: item() });
    dispatch({ type: "stop" });
    expect(io.detaches).toBe(1);
    stop();
  });

  it("a report after a seek carries that seek's epoch, so Rust can tell it saw the seek", () => {
    const { io, bridgeIo } = fakeIo();
    const stop = startVoicePlaybackBridge(bridgeIo);
    dispatch({ type: "load", item: item() });
    io.handler?.({ op: "seek", pos_ms: 30_000, epoch: 7 });
    const afterSeek = io.reports[io.reports.length - 1];
    expect(afterSeek).toMatchObject({ posMs: 30_000, seekEpoch: 7 });
    io.tick?.();
    expect(io.reports[io.reports.length - 1].seekEpoch).toBe(7);
    stop();
  });

  it("delivers inbound effects into the store while active", () => {
    const { io, bridgeIo } = fakeIo();
    const stop = startVoicePlaybackBridge(bridgeIo);
    dispatch({ type: "load", item: item() });
    io.handler?.({ op: "pause" });
    expect(getSnapshot().playing).toBe(false);
    stop();
  });

  it("cleanup stops the heartbeat, unsubscribes, detaches and drops late effects", async () => {
    const { io, bridgeIo } = fakeIo();
    const stop = startVoicePlaybackBridge(bridgeIo);
    dispatch({ type: "load", item: item() });
    const reported = io.reports.length;

    stop();
    await Promise.resolve();
    expect(io.cleared).toBe(true);
    expect(io.unlistened).toBe(1);
    expect(io.detaches).toBe(1);

    io.handler?.({ op: "pause" });
    expect(getSnapshot().playing).toBe(true);
    dispatch({ type: "pause" });
    expect(io.reports).toHaveLength(reported);
  });
});
