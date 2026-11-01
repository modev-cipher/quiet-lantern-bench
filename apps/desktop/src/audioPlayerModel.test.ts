import { describe, expect, it } from "vitest";

import {
  formatRate,
  formatTime,
  nextRate,
  progressPercent,
  seekFraction,
  seekTime,
  skipTarget,
} from "./audioPlayerModel";

describe("formatTime", () => {
  it("formats mm:ss under an hour", () => {
    expect(formatTime(0)).toBe("0:00");
    expect(formatTime(9)).toBe("0:09");
    expect(formatTime(754)).toBe("12:34");
  });

  it("formats h:mm:ss at or above an hour", () => {
    expect(formatTime(3600)).toBe("1:00:00");
    expect(formatTime(3922)).toBe("1:05:22");
  });

  it("returns a placeholder for unknown durations", () => {
    expect(formatTime(Number.NaN)).toBe("—");
    expect(formatTime(Number.POSITIVE_INFINITY)).toBe("—");
    expect(formatTime(-1)).toBe("—");
  });
});

describe("progressPercent", () => {
  it("computes a clamped percentage", () => {
    expect(progressPercent(30, 120)).toBe(25);
    expect(progressPercent(200, 100)).toBe(100);
    expect(progressPercent(-5, 100)).toBe(0);
  });

  it("returns zero when duration is unknown", () => {
    expect(progressPercent(10, 0)).toBe(0);
    expect(progressPercent(10, Number.NaN)).toBe(0);
  });
});

describe("seekFraction", () => {
  it("maps a pointer x within the rail to 0..1", () => {
    expect(seekFraction(50, 0, 100)).toBe(0.5);
    expect(seekFraction(0, 0, 100)).toBe(0);
    expect(seekFraction(100, 0, 100)).toBe(1);
  });

  it("RTL measures from the right edge, so the visual start is still 0", () => {
    expect(seekFraction(100, 0, 100, "rtl")).toBe(0);
    expect(seekFraction(75, 0, 100, "rtl")).toBe(0.25);
    expect(seekFraction(0, 0, 100, "rtl")).toBe(1);
    expect(seekFraction(140, 0, 100, "rtl")).toBe(0);
    expect(seekFraction(50, 0, 100, "ltr")).toBe(0.5);
  });

  it("clamps out-of-bounds pointers", () => {
    expect(seekFraction(-20, 0, 100)).toBe(0);
    expect(seekFraction(140, 0, 100)).toBe(1);
  });

  it("returns zero for a zero-width rail", () => {
    expect(seekFraction(10, 0, 0)).toBe(0);
  });
});

describe("seekTime", () => {
  it("converts a fraction to a clamped time", () => {
    expect(seekTime(0.5, 120)).toBe(60);
    expect(seekTime(1.5, 120)).toBe(120);
    expect(seekTime(0.25, 0)).toBe(0);
  });
});

describe("skipTarget", () => {
  it("advances and rewinds within bounds", () => {
    expect(skipTarget(30, 30, 120)).toBe(60);
    expect(skipTarget(10, -15, 120)).toBe(0);
    expect(skipTarget(110, 30, 120)).toBe(120);
  });

  it("allows unbounded forward skip when duration is unknown", () => {
    expect(skipTarget(50, 30, Number.NaN)).toBe(80);
  });
});

describe("nextRate / formatRate", () => {
  it("cycles through the rate list and wraps", () => {
    expect(nextRate(1)).toBe(1.25);
    expect(nextRate(1.25)).toBe(1.5);
    expect(nextRate(1.5)).toBe(2);
    expect(nextRate(2)).toBe(0.75);
    expect(nextRate(0.75)).toBe(1);
  });

  it("falls back to the first rate for an unknown rate", () => {
    expect(nextRate(3)).toBe(1);
  });

  it("formats a rate label", () => {
    expect(formatRate(1.25)).toBe("1.25×");
  });
});
