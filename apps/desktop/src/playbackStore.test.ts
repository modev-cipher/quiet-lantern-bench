import { afterEach, describe, expect, it, vi } from "vitest";

import {
  dispatch,
  getSnapshot,
  initialPlaybackState,
  playbackReducer,
  resetPlaybackStore,
  subscribe,
  type PlaybackState,
} from "./playbackStore";
import type { AudioShelfItem } from "./audioShelfModel";

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

function state(overrides: Partial<PlaybackState> = {}): PlaybackState {
  return { ...initialPlaybackState, ...overrides };
}

describe("playbackReducer", () => {
  it("loads a fresh item and starts playing from the top", () => {
    const next = playbackReducer(state({ rate: 1.5, muted: true }), {
      type: "load",
      item: item(),
    });
    expect(next.item?.id).toBe("job-1");
    expect(next.playing).toBe(true);
    expect(next.current).toBe(0);
    expect(next.rate).toBe(1.5);
    expect(next.muted).toBe(true);
  });

  it("re-loading the same item resumes without resetting time", () => {
    const playing = state({ item: item(), current: 42, playing: false });
    const next = playbackReducer(playing, { type: "load", item: item() });
    expect(next.current).toBe(42);
    expect(next.playing).toBe(true);
  });

  it("loading a different path of the same job reloads from the top", () => {
    const playing = state({ item: item(), current: 42, playing: false });
    const next = playbackReducer(playing, {
      type: "load",
      item: item({ path: "/exports/b.mp3" }),
    });
    expect(next.item?.path).toBe("/exports/b.mp3");
    expect(next.current).toBe(0);
    expect(next.playing).toBe(true);
  });

  it("toggles play only when an item is loaded", () => {
    expect(playbackReducer(state(), { type: "toggle" }).playing).toBe(false);
    const loaded = state({ item: item(), playing: false });
    expect(playbackReducer(loaded, { type: "toggle" }).playing).toBe(true);
  });

  it("seekFraction sets a seek target and bumps the nonce", () => {
    const loaded = state({ item: item(), duration: 100, current: 0 });
    const next = playbackReducer(loaded, {
      type: "seekFraction",
      fraction: 0.5,
    });
    expect(next.seekTo).toBe(50);
    expect(next.current).toBe(50);
    expect(next.seekNonce).toBe(1);
  });

  it("seekMs seeks to an absolute millisecond through the same nonce path", () => {
    const loaded = state({ item: item(), duration: 100, current: 0 });
    const next = playbackReducer(loaded, { type: "seekMs", ms: 890 });
    expect(next.seekTo).toBe(0.89);
    expect(next.current).toBe(0.89);
    expect(next.seekNonce).toBe(1);
  });

  it("seekMs clamps into the track", () => {
    const loaded = state({ item: item(), duration: 100 });
    expect(playbackReducer(loaded, { type: "seekMs", ms: -50 }).seekTo).toBe(0);
    expect(
      playbackReducer(loaded, { type: "seekMs", ms: 200_000 }).seekTo,
    ).toBe(100);
  });

  it("seekMs still seeks while the duration is unknown, where a fraction would lie", () => {
    const loaded = state({ item: item(), duration: Number.NaN });
    const next = playbackReducer(loaded, { type: "seekMs", ms: 890 });
    expect(
      next.seekTo,
      "seekTime collapses every fraction to 0 under a non-finite duration, so routing a tap through it would silently rewind to the top",
    ).toBe(0.89);
    expect(next.seekNonce).toBe(1);
  });

  it("seekMs refuses a non-finite target rather than corrupting the media element", () => {
    const loaded = state({ item: item(), duration: 100 });
    expect(playbackReducer(loaded, { type: "seekMs", ms: Number.NaN })).toBe(
      loaded,
    );
  });

  it("skip clamps within the track", () => {
    const loaded = state({ item: item(), duration: 100, current: 90 });
    expect(playbackReducer(loaded, { type: "skip", delta: 30 }).seekTo).toBe(
      100,
    );
    expect(playbackReducer(loaded, { type: "skip", delta: -100 }).seekTo).toBe(
      0,
    );
  });

  it("skip bumps the seek nonce, so it ends browsing like every deliberate re-aim", () => {
    const loaded = state({ item: item(), duration: 100, current: 30 });
    const next = playbackReducer(loaded, { type: "skip", delta: 30 });
    expect(
      next.seekNonce,
      "the follow machine exits browsing on any seek-nonce bump; a skip that bypassed the nonce would leave the surface browsing a page the user just navigated away from",
    ).toBe(1);
  });

  it("cycles rate and toggles mute", () => {
    expect(playbackReducer(state(), { type: "cycleRate" }).rate).toBe(1.25);
    expect(playbackReducer(state(), { type: "toggleMute" }).muted).toBe(true);
  });

  it("sets an absolute volume, clamped to the element's range", () => {
    expect(
      playbackReducer(state(), { type: "setVolume", volume: 0.4 }).volume,
    ).toBe(0.4);
    expect(
      playbackReducer(state(), { type: "setVolume", volume: 3 }).volume,
    ).toBe(1);
    expect(
      playbackReducer(state(), { type: "setVolume", volume: -1 }).volume,
    ).toBe(0);
    const before = state({ volume: 0.6 });
    expect(
      playbackReducer(before, { type: "setVolume", volume: Number.NaN }),
    ).toBe(before);
  });

  it("fades separately from the volume; stop and a new item reset it, a same-item reload keeps it", () => {
    const faded = playbackReducer(state({ item: item(), volume: 0.8 }), {
      type: "setFade",
      fade: 0.25,
    });
    expect(faded.fade).toBe(0.25);
    expect(faded.volume).toBe(0.8);
    expect(playbackReducer(faded, { type: "setFade", fade: 9 }).fade).toBe(1);
    expect(playbackReducer(faded, { type: "stop" }).fade).toBe(1);
    expect(
      playbackReducer(faded, {
        type: "load",
        item: item({ id: "job-2", path: "/exports/b.mp3" }),
      }).fade,
    ).toBe(1);
    expect(playbackReducer(faded, { type: "load", item: item() }).fade).toBe(
      0.25,
    );
  });

  it("keeps the volume across stop and a new load", () => {
    const loud = state({ item: item(), volume: 0.3 });
    expect(playbackReducer(loud, { type: "stop" }).volume).toBe(0.3);
    expect(playbackReducer(loud, { type: "load", item: item() }).volume).toBe(
      0.3,
    );
  });

  it("stop clears the item but keeps rate and mute preferences", () => {
    const loaded = state({ item: item(), current: 30, rate: 2, muted: true });
    const next = playbackReducer(loaded, { type: "stop" });
    expect(next.item).toBeNull();
    expect(next.current).toBe(0);
    expect(next.rate).toBe(2);
    expect(next.muted).toBe(true);
  });

  it("ended resets time and stops without clearing the item", () => {
    const loaded = state({ item: item(), current: 99, playing: true });
    const next = playbackReducer(loaded, { type: "engineEnded" });
    expect(next.playing).toBe(false);
    expect(next.current).toBe(0);
    expect(next.item?.id).toBe("job-1");
  });

  it("engine events update observed state", () => {
    const loaded = state({ item: item() });
    expect(
      playbackReducer(loaded, { type: "engineTime", current: 12 }).current,
    ).toBe(12);
    expect(
      playbackReducer(loaded, { type: "engineDuration", duration: 300 })
        .duration,
    ).toBe(300);
    expect(playbackReducer(loaded, { type: "engineError" }).error).toBe(true);
  });

  it("an engine error clears the buffering shimmer so the player never sticks", () => {
    const loaded = playbackReducer(state({ item: item() }), {
      type: "engineBuffering",
      buffering: true,
    });
    expect(loaded.buffering).toBe(true);
    const errored = playbackReducer(loaded, { type: "engineError" });
    expect(errored.error).toBe(true);
    expect(errored.buffering).toBe(false);
  });

  it("carries a refusal message so the player can say why it will not play", () => {
    const loaded = playbackReducer(initialPlaybackState, {
      type: "load",
      item: item(),
    });
    const refused = playbackReducer(loaded, {
      type: "engineError",
      message: "this audio no longer matches the book's chapters",
    });
    expect(refused.playing).toBe(false);
    expect(refused.errorMessage).toContain("no longer matches");
    expect(
      playbackReducer(loaded, { type: "engineError" }).errorMessage,
    ).toBeNull();
    const recovered = playbackReducer(refused, { type: "play" });
    expect(recovered.error).toBe(false);
    expect(
      recovered.errorMessage,
      "a retry must clear the message too, or the player keeps accusing a healed export",
    ).toBeNull();
  });
});

describe("playback store singleton", () => {
  afterEach(() => resetPlaybackStore());

  it("notifies subscribers on state change", () => {
    const listener = vi.fn();
    const unsub = subscribe(listener);
    dispatch({ type: "load", item: item() });
    expect(listener).toHaveBeenCalledTimes(1);
    expect(getSnapshot().item?.id).toBe("job-1");
    unsub();
    dispatch({ type: "pause" });
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it("keeps state continuous: opening does not reset an already-loaded item", () => {
    dispatch({ type: "load", item: item() });
    dispatch({ type: "engineTime", current: 55 });
    dispatch({ type: "load", item: item() });
    expect(getSnapshot().current).toBe(55);
  });

  it("close/stop clears the current item", () => {
    dispatch({ type: "load", item: item() });
    dispatch({ type: "stop" });
    expect(getSnapshot().item).toBeNull();
  });
});

describe("selector stability", () => {
  it("engineTime ticks preserve item identity and selector fields", () => {
    const loaded = playbackReducer(state(), { type: "load", item: item() });
    const ticked = playbackReducer(loaded, {
      type: "engineTime",
      current: 12.5,
    });
    expect(ticked.item).toBe(loaded.item);
    expect(ticked.playing).toBe(loaded.playing);
    expect(ticked.rate).toBe(loaded.rate);
    expect(ticked.muted).toBe(loaded.muted);
    expect(ticked.current).toBe(12.5);
  });

  it("cardPlayback maps ownership and playing to primitives", async () => {
    const { cardPlayback } = await import("./usePlayback");
    const playing = playbackReducer(
      playbackReducer(state(), { type: "load", item: item() }),
      { type: "enginePlaying", playing: true },
    );
    expect(cardPlayback(playing, "job-1", "/exports/a.mp3")).toBe("playing");
    expect(cardPlayback(playing, "job-1", "/other.mp3")).toBe("idle");
    const paused = playbackReducer(playing, { type: "pause" });
    expect(cardPlayback(paused, "job-1", "/exports/a.mp3")).toBe("active");
    expect(cardPlayback(state(), "job-1", "/exports/a.mp3")).toBe("idle");
  });
});
