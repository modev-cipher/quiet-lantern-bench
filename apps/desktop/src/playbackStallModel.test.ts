import { describe, expect, it } from "vitest";

import {
  nextStall,
  NO_STALL,
  STALL_LIMIT_TICKS,
  stalled,
} from "./playbackStallModel";

function run(times: number[]) {
  return times.reduce(nextStall, NO_STALL);
}

describe("playback stall", () => {
  it("is not a stall while the clock moves", () => {
    const moving = Array.from({ length: 60 }, (_, second) => second);
    expect(stalled(run(moving))).toBe(false);
  });

  it("is a stall after the clock holds still for the whole limit", () => {
    const held = Array.from({ length: STALL_LIMIT_TICKS + 1 }, () => 0);
    expect(stalled(run(held))).toBe(true);
  });

  it("does not call a shorter hold a stall", () => {
    const held = Array.from({ length: STALL_LIMIT_TICKS }, () => 42);
    expect(stalled(run(held))).toBe(false);
  });

  it("starts over when the clock moves again", () => {
    const hold = Array.from({ length: STALL_LIMIT_TICKS }, () => 42);
    expect(stalled(run([...hold, 43, 43]))).toBe(false);
  });
});
