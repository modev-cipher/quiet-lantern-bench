import { describe, expect, it } from "vitest";

import type { AudioShelfItem } from "./audioShelfModel";
import {
  canFollowAlong,
  overlayState,
  type OverlayState,
} from "./readingOverlayModel";

function item(overrides: Partial<AudioShelfItem> = {}): AudioShelfItem {
  return {
    id: "export-1",
    bookId: "book-1",
    title: "A Book",
    path: "/exports/a.mp3",
    mode: "narrator",
    format: "mp3",
    bytes: 1024,
    stale: false,
    isScanned: false,
    jobId: "job-1",
    createdAt: 0,
    ...overrides,
  };
}

const STATES: OverlayState[] = ["none", "reading"];

describe("overlayState", () => {
  it("opens the Book from nothing and keeps it open on a repeat", () => {
    expect(overlayState("none", { type: "openReading" })).toBe("reading");
    expect(overlayState("reading", { type: "openReading" })).toBe("reading");
  });

  it("closes the Book", () => {
    expect(overlayState("reading", { type: "closeReading" })).toBe("none");
  });

  it("closes the Book when playback stops, because it has no item", () => {
    for (const state of STATES) {
      expect(overlayState(state, { type: "stop" })).toBe("none");
    }
  });

  it("is idempotent for a close that already happened", () => {
    expect(overlayState("none", { type: "closeReading" })).toBe("none");
    expect(overlayState("none", { type: "stop" })).toBe("none");
  });
});

describe("canFollowAlong", () => {
  it("allows an mp3 export that still has its job", () => {
    expect(canFollowAlong(item())).toBe(true);
  });

  it("refuses when there is nothing playing", () => {
    expect(canFollowAlong(null)).toBe(false);
  });

  it("refuses an export whose job is gone, because the words come from the job", () => {
    expect(canFollowAlong(item({ jobId: null }))).toBe(false);
  });

  it("refuses a stale export, which the backend will not serve either", () => {
    expect(
      canFollowAlong(item({ stale: true })),
      "get_media_url refuses a stale export, so an affordance here would lead nowhere",
    ).toBe(false);
  });

  it("refuses a format the chapter timeline was never built for", () => {
    expect(canFollowAlong(item({ format: "wav" }))).toBe(false);
    expect(canFollowAlong(item({ format: "MP3" }))).toBe(true);
  });
});
