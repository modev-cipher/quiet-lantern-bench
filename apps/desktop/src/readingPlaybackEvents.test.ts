import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn().mockResolvedValue(null);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

import {
  reportPlaybackEvent,
  resetPlaybackEmitters,
} from "./readingClockDiagnostics";

function lines(): string[] {
  return invoke.mock.calls.map(
    (call) => (call[1] as { line: string }).line,
  );
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(Date.parse("2026-08-27T10:00:00.000Z"));
  invoke.mockClear();
  resetPlaybackEmitters?.();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("pause flood aggregation", () => {
  it("collapses a scrub burst of pauses into one line carrying the suppressed count", async () => {
    for (let step = 0; step < 12; step += 1) {
      await reportPlaybackEvent("pause", 10 + step * 0.25);
      vi.advanceTimersByTime(120);
    }
    const emitted = lines().filter((line) => line.includes("event=pause"));
    expect(
      emitted.length,
      "a 12-step scrub gesture must not write 12 pause lines — floods bury the only diagnostic channel the user has",
    ).toBe(1);
    vi.advanceTimersByTime(3_000);
    await reportPlaybackEvent("pause", 20);
    const after = lines().filter((line) => line.includes("event=pause"));
    expect(after.length).toBe(2);
    expect(
      after[1],
      "the line that ends a suppressed run must carry the run's size",
    ).toContain("suppressed=11");
  });

  it("emits a long stall's heartbeat instead of going silent forever", async () => {
    for (let step = 0; step < 16; step += 1) {
      await reportPlaybackEvent("pause", 10 + step * 0.25);
      vi.advanceTimersByTime(50);
    }
    const emitted = lines().filter((line) => line.includes("event=pause"));
    expect(emitted.length).toBe(2);
    expect(emitted[1]).toContain("suppressed=15");
  });
});

describe("error and exit visibility", () => {
  it("aggregates only exact repeats of an error and still reports the count", async () => {
    await reportPlaybackEvent("error", 5);
    await reportPlaybackEvent("error", 5);
    await reportPlaybackEvent("error", 5);
    vi.advanceTimersByTime(3_000);
    await reportPlaybackEvent("error", 5);
    const emitted = lines().filter((line) => line.includes("event=error"));
    expect(emitted.length).toBe(2);
    expect(emitted[1]).toContain("suppressed=2");
  });

  it("a different error inside the window emits immediately", async () => {
    await reportPlaybackEvent("error", 5);
    await reportPlaybackEvent("error", 9);
    const emitted = lines().filter((line) => line.includes("event=error"));
    expect(emitted.length).toBe(2);
  });

  it("an exit is never suppressed behind a pause burst", async () => {
    for (let step = 0; step < 8; step += 1) {
      await reportPlaybackEvent("pause", 10 + step * 0.25);
      vi.advanceTimersByTime(100);
    }
    await reportPlaybackEvent("exit", 12);
    const exits = lines().filter((line) => line.includes("event=exit"));
    expect(exits.length).toBe(1);
  });

  it("a mid-playback error still emits its by-design error-then-exit pair", async () => {
    await reportPlaybackEvent("error", 7);
    await reportPlaybackEvent("exit", 7);
    expect(lines().some((line) => line.includes("event=error"))).toBe(true);
    expect(lines().some((line) => line.includes("event=exit"))).toBe(true);
  });
});
