import { describe, expect, it } from "vitest";

import {
  MAX_TRACKED,
  listenedFraction,
  listenedOf,
  parseListened,
  serialiseListened,
  withListen,
} from "./listeningProgress";

describe("a corrupt store degrades to no progress, never to a wrong rule", () => {
  it("survives absent, malformed and hostile values", () => {
    expect(parseListened(null)).toEqual({});
    expect(parseListened("not json")).toEqual({});
    expect(parseListened("[1,2,3]")).toEqual({});
    expect(parseListened("null")).toEqual({});
    expect(
      parseListened('{"a":"half","b":null,"c":true}'),
      "a non-number would render as NaN% of a track",
    ).toEqual({});
  });

  it("drops fractions outside the track, because a rule cannot be 140% full", () => {
    expect(parseListened('{"a":1.4,"b":-0.2,"c":0,"d":0.5}')).toEqual({
      d: 0.5,
    });
  });

  it("round-trips what it accepts", () => {
    const map = { a: 0.25, b: 1 };
    expect(parseListened(serialiseListened(map))).toEqual(map);
  });
});

describe("the fraction is only computed from numbers that can make one", () => {
  it("refuses a duration the element has not reported yet", () => {
    expect(listenedFraction(12, Number.NaN)).toBeNull();
    expect(listenedFraction(12, 0)).toBeNull();
    expect(listenedFraction(Number.NaN, 100)).toBeNull();
    expect(listenedFraction(-1, 100)).toBeNull();
  });

  it("clamps a position past the end to a full rule", () => {
    expect(listenedFraction(101, 100)).toBe(1);
    expect(listenedFraction(25, 100)).toBe(0.25);
  });
});

describe("the store stays bounded, because a library has no ceiling", () => {
  it("keeps the newest entries and forgets the oldest", () => {
    let map = {};
    for (let i = 0; i < MAX_TRACKED + 10; i += 1) {
      map = withListen(map, `id-${i}`, 0.5);
    }
    const ids = Object.keys(map);
    expect(ids.length).toBe(MAX_TRACKED);
    expect(listenedOf(map, "id-0")).toBeNull();
    expect(listenedOf(map, `id-${MAX_TRACKED + 9}`)).toBe(0.5);
  });

  it("does not grow on a repeated identical write", () => {
    const first = withListen({}, "a", 0.5);
    expect(withListen(first, "a", 0.5)).toBe(first);
  });

  it("ignores a write it cannot represent", () => {
    const map = { a: 0.5 };
    expect(withListen(map, "a", null)).toBe(map);
    expect(withListen(map, "", 0.5)).toBe(map);
    expect(withListen(map, "b", 0)).toBe(map);
  });
});

describe("the store refuses a fraction it could not read back", () => {
  it("rejects non-finite and out-of-range writes at the door", () => {
    const map = { a: 0.5 };
    expect(withListen(map, "b", Number.NaN)).toBe(map);
    expect(withListen(map, "b", Number.POSITIVE_INFINITY)).toBe(map);
    expect(
      withListen(map, "b", 1.4),
      "parseListened drops >1 on the next load, so storing it makes the rule disappear later instead of now",
    ).toBe(map);
    expect(withListen(map, "b", 1)).toEqual({ a: 0.5, b: 1 });
  });
});
