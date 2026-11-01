import { existsSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { readSource } from "./tests/readSource";

const CLOCK = readSource(new URL("./readingClock.ts", import.meta.url));

const CLOCK_CALL_SITES = ["ReadingView.tsx", "useReadingClock.ts"] as const;


function callSites(): Array<{ name: string; body: string }> {
  return CLOCK_CALL_SITES.filter((name) =>
    existsSync(new URL(`./${name}`, import.meta.url)),
  ).map((name) => ({
    name,
    body: readSource(new URL(`./${name}`, import.meta.url)),
  }));
}

describe("the reading clock reads the audio element, not the store", () => {
  it("does not import the 4Hz store or its hook", () => {
    expect(
      CLOCK,
      "playbackStore.current is fed by onTimeUpdate at about 4Hz, which cannot meet a 120ms budget",
    ).not.toMatch(/from\s+["']\.\/playbackStore["']/);
    expect(CLOCK).not.toMatch(/from\s+["']\.\/usePlayback["']/);
  });

  it("does not reach the registry itself, so its time source stays injected", () => {
    expect(
      CLOCK,
      "an injected getTimeMs is what makes the clock testable without a DOM",
    ).not.toMatch(/from\s+["']\.\/playbackElement["']/);
  });

  it("names no timeupdate listener of its own", () => {
    expect(CLOCK).not.toContain("timeupdate");
    expect(CLOCK).not.toContain("onTimeUpdate");
  });

  it("keeps every call site off the store's current time", () => {
    for (const { name, body } of callSites()) {
      expect(
        body,
        `${name} must feed the clock from the audio element, not from playbackStore.current`,
      ).not.toMatch(/getSnapshot\(\)\s*\.\s*current\b/);
      expect(body).not.toMatch(/usePlayback\([^)]*\.\s*current\b/);
      expect(body).not.toMatch(/\bstate\s*\.\s*current\b/);
      expect(body).not.toContain("timeupdate");
    }
  });

  it("feeds the clock the audio element's own time", () => {
    const sites = callSites();

    expect(
      sites.length,
      "the guard is only meaningful once something constructs the clock",
    ).toBeGreaterThan(0);
    for (const { name, body } of sites) {
      expect(
        body,
        `${name} must pass audioTimeMs as the clock's time source`,
      ).toMatch(/getTimeMs:\s*audioTimeMs/);
    }
  });
});
