import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi
  .fn<(cmd: string, args?: unknown) => Promise<unknown>>()
  .mockResolvedValue(null);
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));

import {
  reportUiCrash,
  resetUiCrashEmitter,
  uiCrashPayload,
} from "./uiCrashDiagnostics";

function lines(): string[] {
  return invoke.mock.calls
    .filter((call) => call[0] === "log_ui_error")
    .map((call) => (call[1] as { line: string }).line);
}

function crash(
  message: string,
  frame = "at render (app://assets/Main.js:88:17)",
) {
  const error = new TypeError(message);
  error.stack = `TypeError: ${message}\n    ${frame}\n    at commitRoot (app://assets/Main.js:12:3)`;
  return error;
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(Date.parse("2026-10-07T09:00:00.000Z"));
  invoke.mockClear();
  invoke.mockResolvedValue(null);
  resetUiCrashEmitter();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("the crash emitter is registered with the shared rate policy", () => {
  it("reports the first crash immediately", async () => {
    await reportUiCrash(crash("x is not a function"));

    expect(lines()).toHaveLength(1);
    expect(lines()[0]).toContain("TypeError: x is not a function");
    expect(
      lines()[0],
      "the backend digest is built from the stack, so the frames have to survive to it",
    ).toContain("at render (app://assets/Main.js:88:17)");
  });

  it("collapses a crash loop into a bounded number of lines", async () => {
    for (let step = 0; step < 40; step += 1) {
      await reportUiCrash(crash("x is not a function"));
      vi.advanceTimersByTime(40);
    }

    expect(
      lines().length,
      "a re-rendering crash loop must not write one line per render into the file users are \
told to email; the channel exists to carry one diagnosable crash",
    ).toBeLessThanOrEqual(6);
    expect(lines().length).toBeGreaterThan(0);
    expect(
      lines().some((line) => /suppressed=\d+/.test(line)),
      "a collapsed burst must stay countable, the same contract the playback and reading-clock \
emitters carry",
    ).toBe(true);
  });

  it("heartbeats a long loop instead of going silent forever", async () => {
    for (let step = 0; step < 60; step += 1) {
      await reportUiCrash(crash("x is not a function"));
      vi.advanceTimersByTime(40);
    }

    expect(lines().length).toBeGreaterThan(1);
    expect(lines().some((line) => line.includes("suppressed=15"))).toBe(true);
  });

  it("a different crash is reported even inside the window", async () => {
    await reportUiCrash(crash("x is not a function"));
    await reportUiCrash(crash("cannot read properties of undefined"));

    expect(lines()).toHaveLength(2);
    expect(lines()[1]).toContain("cannot read properties of undefined");
  });

  it("the same fault at a different site is a different crash", async () => {
    await reportUiCrash(crash("x is not a function"));
    await reportUiCrash(
      crash(
        "x is not a function",
        "at fetchJobs (app://assets/JobsPanel.js:410:9)",
      ),
    );

    expect(lines()).toHaveLength(2);
  });

  it("volatile numbers in the message do not make every occurrence unique", async () => {
    for (let step = 0; step < 10; step += 1) {
      await reportUiCrash(crash(`segment ${step * 137} failed to render`));
      vi.advanceTimersByTime(40);
    }

    expect(
      lines().length,
      "a key derived from the raw message defeats suppression entirely for any error that quotes \
an index, an id or a timestamp",
    ).toBe(1);
  });

  it("never sends the emitter's own suppression bookkeeping instead of the stack", async () => {
    for (let step = 0; step < 40; step += 1) {
      await reportUiCrash(crash("x is not a function"));
      vi.advanceTimersByTime(40);
    }
    vi.advanceTimersByTime(3_000);
    await reportUiCrash(crash("x is not a function"));

    const emitted = lines();
    const last = emitted[emitted.length - 1] ?? "";
    expect(last).toContain("at commitRoot (app://assets/Main.js:12:3)");
    expect(last).toMatch(/suppressed=\d+/);
  });

  it("a non-Error throw is still reported as its own text", async () => {
    await reportUiCrash("a bare string crash");

    expect(lines()).toEqual(["a bare string crash"]);
  });

  it("a failed report still rejects so the boundary can log it", async () => {
    invoke.mockRejectedValueOnce(new Error("ipc gone"));

    await expect(reportUiCrash(crash("x is not a function"))).rejects.toThrow(
      "ipc gone",
    );
  });
});

describe("uiCrashPayload", () => {
  it("names the error and takes its top frame", () => {
    const payload = uiCrashPayload(crash("boom"));
    expect(payload.name).toBe("TypeError");
    expect(payload.message).toBe("boom");
    expect(payload.frame).toBe("at render (app://assets/Main.js:88:17)");
    expect(payload.detail).toContain("at commitRoot");
  });

  it("falls back to n/a rather than inventing a frame", () => {
    const error = new RangeError("stackless");
    error.stack = undefined;
    const payload = uiCrashPayload(error);
    expect(payload.frame).toBe("n/a");
    expect(payload.detail).toBe("stackless");
  });

  it("carries a non-Error throw through unchanged", () => {
    const payload = uiCrashPayload({ nope: true });
    expect(payload.message).toBe("[object Object]");
    expect(payload.frame).toBe("n/a");
  });
});
