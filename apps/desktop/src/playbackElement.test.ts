import { afterEach, describe, expect, it } from "vitest";

import {
  audioTimeMs,
  getAudioElement,
  registerAudioElement,
} from "./playbackElement";

function fakeAudio(currentTime: number): HTMLAudioElement {
  return { currentTime } as HTMLAudioElement;
}

afterEach(() => {
  registerAudioElement(null);
});

describe("the playback element registry", () => {
  it("hands back the element the engine registered", () => {
    const audio = fakeAudio(0);
    registerAudioElement(audio);

    expect(getAudioElement()).toBe(audio);
  });

  it("holds nothing until an element is registered", () => {
    expect(getAudioElement()).toBeNull();
  });

  it("lets the last writer win, because one element is mounted at a time", () => {
    const first = fakeAudio(1);
    const second = fakeAudio(2);
    registerAudioElement(first);
    registerAudioElement(second);

    expect(getAudioElement()).toBe(second);
  });

  it("clears on unmount so a stale node cannot keep answering", () => {
    registerAudioElement(fakeAudio(3));
    registerAudioElement(null);

    expect(getAudioElement()).toBeNull();
  });

  it("reports the element's own clock in milliseconds", () => {
    registerAudioElement(fakeAudio(1.234));

    expect(audioTimeMs()).toBeCloseTo(1234, 6);
  });

  it("reports an absent element as not-a-number rather than as zero", () => {
    expect(Number.isNaN(audioTimeMs())).toBe(true);
  });

  it("keeps reading the live element rather than a snapshot", () => {
    const audio = fakeAudio(0);
    registerAudioElement(audio);
    audio.currentTime = 2.5;

    expect(audioTimeMs()).toBeCloseTo(2500, 6);
  });
});
