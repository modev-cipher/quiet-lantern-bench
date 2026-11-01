import { describe, expect, it } from "vitest";

import {
  STREAM_SETTLE_MS,
  applyPlayerVolume,
  streamStillSettling,
} from "./playbackVolumeModel";

describe("applyPlayerVolume", () => {
  it("overrides a restored silent stream with the store's volume", () => {
    const element = { volume: 0, muted: false };
    applyPlayerVolume(element, { volume: 1, fade: 1, muted: false });
    expect(element.volume).toBe(1);
  });

  it("includes the sleep fade and restores mute", () => {
    const element = { volume: 1, muted: true };
    applyPlayerVolume(element, { volume: 0.8, fade: 0.5, muted: false });
    expect(element.volume).toBeCloseTo(0.4);
    expect(element.muted).toBe(false);
  });

  it("does not write when already matching, so no volumechange loop starts", () => {
    let writes = 0;
    const element = {
      get volume() {
        return 0.5;
      },
      set volume(_value: number) {
        writes += 1;
      },
      muted: false,
    };
    applyPlayerVolume(element, { volume: 0.5, fade: 1, muted: false });
    expect(writes).toBe(0);
  });
});

describe("streamStillSettling", () => {
  it("leaves later mixer changes alone", () => {
    expect(streamStillSettling(null, 10)).toBe(false);
    expect(streamStillSettling(100, 100 + STREAM_SETTLE_MS - 1)).toBe(true);
    expect(streamStillSettling(100, 100 + STREAM_SETTLE_MS)).toBe(false);
  });
});
