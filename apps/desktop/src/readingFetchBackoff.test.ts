import { describe, expect, it } from "vitest";

import { FetchBackoff } from "./readingFetchBackoff";

describe("FetchBackoff", () => {
  it("lets a never-failed chapter through", () => {
    expect(new FetchBackoff().canTry("ch-1", 0)).toBe(true);
  });

  it("holds a failed chapter back instead of retrying it every frame", () => {
    const backoff = new FetchBackoff();
    backoff.failed("ch-1", 1_000);
    expect(backoff.canTry("ch-1", 1_016)).toBe(false);
    expect(backoff.canTry("ch-1", 1_999)).toBe(false);
    expect(backoff.canTry("ch-1", 2_000)).toBe(true);
    expect(backoff.canTry("ch-2", 1_016)).toBe(true);
  });

  it("doubles the wait per consecutive failure up to a ceiling", () => {
    const backoff = new FetchBackoff();
    const waits: number[] = [];
    let now = 0;
    for (let i = 0; i < 8; i += 1) {
      backoff.failed("ch-1", now);
      let wait = 0;
      while (!backoff.canTry("ch-1", now + wait)) wait += 1_000;
      waits.push(wait);
      now += wait;
    }
    expect(waits).toEqual([1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000]);
  });

  it("forgets a chapter once it loads", () => {
    const backoff = new FetchBackoff();
    backoff.failed("ch-1", 0);
    backoff.failed("ch-1", 1_000);
    backoff.succeeded("ch-1");
    expect(backoff.canTry("ch-1", 1_001)).toBe(true);
    backoff.failed("ch-1", 1_001);
    expect(backoff.canTry("ch-1", 2_001)).toBe(true);
  });
});
