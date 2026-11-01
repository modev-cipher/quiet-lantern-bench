import { describe, expect, it } from "vitest";

import {
  HARDWARE_REFRESH_INTERVAL_MS,
  subscribeHardwareProfile,
  type RefreshTargets,
} from "./hardwareRefresh";

interface FakeEnv {
  targets: RefreshTargets;
  fire: (type: "focus" | "visibilitychange") => void;
  tick: () => void;
  visibility: { state: string };
  intervals: { ms: number | null; cleared: boolean };
}

function fakeEnv(): FakeEnv {
  const listeners = new Map<string, () => void>();
  const visibility = { state: "visible" };
  const intervals: FakeEnv["intervals"] = { ms: null, cleared: false };
  let intervalCb: (() => void) | null = null;
  const targets: RefreshTargets = {
    window: {
      addEventListener: (type, cb) => listeners.set(type, cb),
      removeEventListener: (type) => listeners.delete(type),
    },
    document: {
      addEventListener: (type, cb) => listeners.set(type, cb),
      removeEventListener: (type) => listeners.delete(type),
      get visibilityState() {
        return visibility.state;
      },
    },
    setInterval: (cb, ms) => {
      intervalCb = cb;
      intervals.ms = ms;
      return 1 as unknown as ReturnType<typeof setInterval>;
    },
    clearInterval: () => {
      intervals.cleared = true;
    },
  };
  return {
    targets,
    fire: (type) => listeners.get(type)?.(),
    tick: () => intervalCb?.(),
    visibility,
    intervals,
  };
}

function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

describe("subscribeHardwareProfile", () => {
  it("fetches on subscribe, focus, visibility, and interval", async () => {
    const env = fakeEnv();
    let calls = 0;
    const received: number[] = [];
    const cleanup = subscribeHardwareProfile(
      () => Promise.resolve(++calls),
      (value) => received.push(value),
      env.targets,
    );
    await flush();
    env.fire("focus");
    await flush();
    env.fire("visibilitychange");
    await flush();
    env.tick();
    await flush();
    expect(received).toEqual([1, 2, 3, 4]);
    expect(env.intervals.ms).toBe(HARDWARE_REFRESH_INTERVAL_MS);
    cleanup();
  });

  it("skips refresh when the document is hidden", async () => {
    const env = fakeEnv();
    let calls = 0;
    subscribeHardwareProfile(
      () => Promise.resolve(++calls),
      () => {},
      env.targets,
    );
    await flush();
    env.visibility.state = "hidden";
    env.fire("visibilitychange");
    await flush();
    expect(calls).toBe(1);
  });

  it("drops stale responses that resolve after newer ones", async () => {
    const env = fakeEnv();
    const received: number[] = [];
    const pending: Array<(value: number) => void> = [];
    subscribeHardwareProfile(
      () =>
        new Promise<number>((resolve) => {
          pending.push(resolve);
        }),
      (value) => received.push(value),
      env.targets,
    );
    env.fire("focus");
    pending[1]?.(2);
    pending[0]?.(1);
    await flush();
    expect(received).toEqual([2]);
  });

  it("stops delivering after cleanup", async () => {
    const env = fakeEnv();
    const received: number[] = [];
    let resolveFetch: (value: number) => void = () => {};
    const cleanup = subscribeHardwareProfile(
      () =>
        new Promise<number>((resolve) => {
          resolveFetch = resolve;
        }),
      (value) => received.push(value),
      env.targets,
    );
    cleanup();
    resolveFetch(42);
    await flush();
    expect(received).toEqual([]);
    expect(env.intervals.cleared).toBe(true);
  });
});
