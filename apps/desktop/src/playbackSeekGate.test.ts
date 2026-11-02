import { describe, expect, it } from "vitest";

import {
  createSeekGate,
  SEEK_RESUME_QUIET_MS,
  type SeekableAudio,
} from "./playbackSeekGate";

// The stub wedges the way WebKitGTK 2.50 on GStreamer 1.20 (the AppImage and
// Ubuntu 22.04 stack) did when measured: a seek that lands while the element
// plays, or while a play() is still starting, leaves it at the target with
// readyState 2 and no further progress.
class WedgingAudio implements SeekableAudio {
  paused = false;
  seeking = false;
  ended = false;
  readyState = 4;
  duration = 600;
  starting = false;
  wedged = false;
  ops: string[] = [];
  private time = 0;
  private settle: (() => void) | null = null;
  private fail: ((cause: unknown) => void) | null = null;

  get currentTime(): number {
    return this.time;
  }

  set currentTime(value: number) {
    if (!this.paused || this.starting) this.wedged = true;
    this.time = value;
    this.ended = value >= this.duration;
    this.seeking = true;
    this.ops.push(`seek ${value}`);
  }

  pause(): void {
    this.paused = true;
    this.ops.push("pause");
  }

  play(): Promise<void> {
    this.paused = false;
    this.starting = true;
    this.ops.push("play");
    return new Promise((resolve, reject) => {
      this.settle = resolve;
      this.fail = reject;
    });
  }

  finishSeek(): void {
    this.seeking = false;
  }

  startPlaying(): void {
    this.starting = false;
    this.settle?.();
  }

  rejectPlay(): void {
    this.starting = false;
    this.paused = true;
    this.fail?.(new DOMException("blocked", "NotAllowedError"));
  }
}

function rig(audio = new WedgingAudio(), playing = () => true) {
  const timers = new Map<number, () => void>();
  let next = 1;
  const events: string[] = [];
  const gate = createSeekGate(audio, {
    shouldPlay: playing,
    resume: () => audio.play(),
    onEnded: () => events.push("ended"),
    setTimer: (run, ms) => {
      expect(ms).toBe(SEEK_RESUME_QUIET_MS);
      timers.set(next, run);
      return next++;
    },
    clearTimer: (id) => timers.delete(id),
  });
  const quiet = () => {
    const pending = [...timers.values()];
    timers.clear();
    for (const run of pending) run();
  };
  const seeked = () => {
    audio.finishSeek();
    gate.onSeeked();
  };
  return { audio, gate, quiet, seeked, timers, events };
}

describe("seek gate", () => {
  it("pauses a playing element before seeking and resumes once the seek has settled", () => {
    const { audio, gate, quiet, seeked } = rig();
    gate.seek(120);
    expect(audio.ops).toEqual(["pause", "seek 120"]);
    expect(gate.holds()).toBe(true);
    seeked();
    expect(audio.ops).not.toContain("play");
    quiet();
    expect(audio.ops).toEqual(["pause", "seek 120", "play"]);
    audio.startPlaying();
    gate.onPlaying();
    expect(gate.holds()).toBe(false);
    expect(audio.wedged).toBe(false);
  });

  it("holds through a whole drag and resumes once at its end", () => {
    const { audio, gate, quiet, seeked } = rig();
    for (const target of [100, 130, 160, 190, 220]) {
      gate.seek(target);
      seeked();
    }
    quiet();
    expect(audio.ops.filter((op) => op === "pause")).toHaveLength(1);
    expect(audio.ops.filter((op) => op === "play")).toHaveLength(1);
    expect(audio.ops[audio.ops.length - 2]).toBe("seek 220");
    expect(audio.wedged).toBe(false);
  });

  it("does not resume while a later seek is still in flight", () => {
    const { audio, gate, quiet } = rig();
    gate.seek(100);
    gate.seek(200);
    audio.finishSeek();
    audio.seeking = true;
    gate.onSeeked();
    quiet();
    expect(audio.ops).not.toContain("play");
  });

  it("defers a seek that arrives while playback is restarting until it plays", () => {
    const { audio, gate, quiet, seeked } = rig();
    gate.seek(100);
    seeked();
    quiet();
    gate.seek(300);
    expect(audio.currentTime).toBe(100);
    audio.startPlaying();
    gate.onPlaying();
    expect(audio.ops).toEqual([
      "pause",
      "seek 100",
      "play",
      "pause",
      "seek 300",
    ]);
    expect(audio.wedged).toBe(false);
    seeked();
    quiet();
    expect(audio.ops[audio.ops.length - 1]).toBe("play");
  });

  it("leaves an element the user paused paused", () => {
    const audio = new WedgingAudio();
    audio.paused = true;
    const { gate, quiet, seeked } = rig(audio, () => false);
    gate.seek(50);
    seeked();
    quiet();
    expect(audio.ops).toEqual(["seek 50"]);
    expect(gate.holds()).toBe(false);
  });

  it("stays paused when the user pauses during a drag", () => {
    let playing = true;
    const { audio, gate, quiet, seeked } = rig(
      new WedgingAudio(),
      () => playing,
    );
    gate.seek(80);
    playing = false;
    seeked();
    quiet();
    expect(audio.ops).toEqual(["pause", "seek 80"]);
    expect(gate.holds()).toBe(false);
  });

  it("lands a deferred seek when the restart is refused", async () => {
    const { audio, gate, quiet, seeked } = rig();
    gate.seek(100);
    seeked();
    quiet();
    gate.seek(250);
    audio.rejectPlay();
    await Promise.resolve();
    await Promise.resolve();
    expect(gate.holds()).toBe(false);
    expect(audio.currentTime).toBe(250);
    expect(audio.wedged).toBe(false);
  });

  it("ends the track instead of restarting it when a drag lands on the end", () => {
    const { audio, gate, quiet, seeked, events } = rig();
    gate.seek(600);
    seeked();
    quiet();
    expect(audio.ops).not.toContain("play");
    expect(events).toEqual(["ended"]);
    expect(gate.holds()).toBe(false);
  });

  it("seeks directly before metadata, where no seek can complete", () => {
    const audio = new WedgingAudio();
    audio.readyState = 0;
    const { gate } = rig(audio);
    gate.seek(40);
    expect(audio.ops).toEqual(["seek 40"]);
    expect(gate.holds()).toBe(false);
  });

  it("forgets a hold when the source changes", () => {
    const { gate, timers, seeked } = rig();
    gate.seek(90);
    seeked();
    gate.reset();
    expect(timers.size).toBe(0);
    expect(gate.holds()).toBe(false);
  });
});
