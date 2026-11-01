import { describe, expect, it } from "vitest";

import type { AudioShelfItem } from "./audioShelfModel";
import {
  bookAvailability,
  NO_AUDIO,
  partitionOrphans,
} from "./libraryAvailability";

function item(overrides: Partial<AudioShelfItem>): AudioShelfItem {
  return {
    id: "e1",
    bookId: "b1",
    title: "Fixture",
    path: "/exports/f.mp3",
    mode: "narrate",
    format: "mp3",
    bytes: 1,
    stale: false,
    isScanned: false,
    jobId: "j1",
    createdAt: 10,
    ...overrides,
  };
}

describe("bookAvailability", () => {
  it("stays silent for a book with no audio", () => {
    expect(bookAvailability("b1", [])).toEqual(NO_AUDIO);
    expect(bookAvailability("b1", [item({ bookId: "b2" })])).toEqual(NO_AUDIO);
  });

  it("offers the newest playable export for Study", () => {
    const shelf = [
      item({ id: "old", createdAt: 5 }),
      item({ id: "new", createdAt: 9 }),
    ];
    const availability = bookAvailability("b1", shelf);
    expect(availability.playable?.id).toBe("new");
    expect(availability.tone).toBe("ready");
    expect(availability.line).toBe("2 casts ready");
  });

  it("says stale when every export is stale, and offers no playback", () => {
    const availability = bookAvailability("b1", [item({ stale: true })]);
    expect(availability.line).toBe("Audio stale — re-render available");
    expect(availability.tone).toBe("stale");
    expect(availability.playable).toBeNull();
  });

  it("counts stale exports next to ready ones", () => {
    const shelf = [item({ id: "ok" }), item({ id: "bad", stale: true })];
    expect(bookAvailability("b1", shelf).line).toBe("Audio ready · 1 stale");
  });

  it("never claims ready for unplayable formats", () => {
    const availability = bookAvailability("b1", [item({ format: "ogg" })]);
    expect(availability.tone).toBe("stale");
    expect(availability.playable).toBeNull();
  });
});

describe("partitionOrphans", () => {
  it("splits exports whose book link is gone", () => {
    const shelf = [item({ id: "a" }), item({ id: "b", bookId: "" })];
    const { owned, orphans } = partitionOrphans(shelf);
    expect(owned.map((entry) => entry.id)).toEqual(["a"]);
    expect(orphans.map((entry) => entry.id)).toEqual(["b"]);
  });
});
