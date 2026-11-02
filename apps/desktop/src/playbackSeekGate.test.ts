import { describe, expect, it } from "vitest";

import {
  createSeekGate,
  SEEK_RESUME_QUIET_MS,
  type SeekGate,
  type SeekableAudio,
} from "./playbackSeekGate";

// The stub wedges the way WebKitGTK 2.50 on GStreamer 1.20 (the AppImage and
// Ubuntu 22.04 stack) did when measured: a seek that lands while the element
// plays, or while a play() is still starting, leaves it at the target with
// readyState 2 and no further progress. Element events reach the gate the
// way PlaybackEngine forwards them.
class WedgingAudio implements SeekableAudio {
  paused = false;
  seeking = false;
  ended = false;
  readyState = 4;
  duration = 600;
  starting = false;
  wedged = false;
  ops: string[] = [];
  gate: SeekGate | null = null;
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
    this.starting = false;
    this.ops.push("pause");
    this.gate?.onPause();
  }

  play(): Promise<void> {
    this.paused = false;
    this.starting = true;
    this.ops.push("play");
    this.gate?.onPlay();
    return new Promise((resolve, reject) => {
      this.settle = resolve;
      this.fail = reject;
    });
  }

  finishSeek(): void {
    this.seeking = false;
    this.gate?.onSeeked();
  }

  startPlaying(): void {
    this.starting = false;
    this.gate?.onPlaying();
    this.settle?.();
  }

  refusePlay(): void {
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
  audio.gate = gate;
  const quiet = () => {
    const pending = [...timers.values()];
    timers.clear();
    for (const run of pending) run();
  };
  return { audio, gate, quiet, timers, events };
}

function last(ops: string[], back = 1): string | undefined {
  return ops[ops.length - back];
}

describe("seek gate", () => {
  it("pauses a playing element before seeking and resumes once the seek has settled", () => {
    const { audio, gate, quiet } = rig();
    gate.seek(120);
    expect(audio.ops).toEqual(["pause", "seek 120"]);
    expect(gate.holds()).toBe(true);
    audio.finishSeek();
    expect(audio.ops).not.toContain("play");
    quiet();
    expect(audio.ops).toEqual(["pause", "seek 120", "play"]);
    expect(gate.holds()).toBe(false);
    expect(audio.wedged).toBe(false);
  });

  it("holds through a whole drag and resumes once at its end", () => {
    const { audio, gate, quiet } = rig();
    for (const target of [100, 130, 160, 190, 220]) {
      gate.seek(target);
      audio.finishSeek();
    }
    quiet();
    expect(audio.ops.filter((op) => op === "pause")).toHaveLength(1);
    expect(audio.ops.filter((op) => op === "play")).toHaveLength(1);
    expect(last(audio.ops, 2)).toBe("seek 220");
    expect(audio.wedged).toBe(false);
  });

  it("does not resume while a later seek is still in flight", () => {
    const { audio, gate, quiet } = rig();
    gate.seek(100);
    gate.seek(200);
    audio.seeking = true;
    gate.onSeeked();
    quiet();
    expect(audio.ops).not.toContain("play");
  });

  it("defers a seek that arrives while its own resume is starting", () => {
    const { audio, gate, quiet } = rig();
    gate.seek(100);
    audio.finishSeek();
    quiet();
    gate.seek(300);
    expect(audio.currentTime).toBe(100);
    audio.startPlaying();
    expect(audio.ops).toEqual([
      "pause",
      "seek 100",
      "play",
      "pause",
      "seek 300",
    ]);
    expect(audio.wedged).toBe(false);
    audio.finishSeek();
    quiet();
    expect(last(audio.ops)).toBe("play");
  });

  it("defers a seek that arrives while any other play is starting", () => {
    const audio = new WedgingAudio();
    audio.paused = true;
    const { gate } = rig(audio);
    void audio.play();
    gate.seek(450);
    expect(audio.ops).toEqual(["play"]);
    audio.startPlaying();
    expect(audio.ops).toEqual(["play", "pause", "seek 450"]);
    expect(audio.wedged).toBe(false);
  });

  it("lands a deferred seek on a start that is paused instead", () => {
    const audio = new WedgingAudio();
    audio.paused = true;
    const { gate } = rig(audio, () => false);
    void audio.play();
    gate.seek(75);
    audio.pause();
    expect(audio.currentTime).toBe(75);
    expect(audio.wedged).toBe(false);
    expect(gate.holds()).toBe(false);
  });

  it("leaves an element the user paused paused", () => {
    const audio = new WedgingAudio();
    audio.paused = true;
    const { gate, quiet } = rig(audio, () => false);
    gate.seek(50);
    audio.finishSeek();
    quiet();
    expect(audio.ops).toEqual(["seek 50"]);
    expect(gate.holds()).toBe(false);
  });

  it("stays paused when the user pauses during a drag", () => {
    let playing = true;
    const { audio, gate, quiet } = rig(new WedgingAudio(), () => playing);
    gate.seek(80);
    playing = false;
    audio.finishSeek();
    quiet();
    expect(audio.ops).toEqual(["pause", "seek 80"]);
    expect(gate.holds()).toBe(false);
  });

  it("lands a deferred seek when the restart is refused", async () => {
    const { audio, gate, quiet } = rig();
    gate.seek(100);
    audio.finishSeek();
    quiet();
    gate.seek(250);
    audio.refusePlay();
    await Promise.resolve();
    await Promise.resolve();
    expect(audio.currentTime).toBe(250);
    expect(audio.wedged).toBe(false);
  });

  it("ends the track instead of restarting it when a drag lands on the end", () => {
    const { audio, gate, quiet, events } = rig();
    gate.seek(600);
    audio.finishSeek();
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
    const { audio, gate, timers } = rig();
    gate.seek(90);
    audio.finishSeek();
    gate.reset();
    expect(timers.size).toBe(0);
    expect(gate.holds()).toBe(false);
  });
});
