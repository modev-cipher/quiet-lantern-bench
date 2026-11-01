import { describe, expect, it } from "vitest";

import { cloneErrorKey, failed, initialState } from "./cloneVoiceModel";
import { readSource } from "./tests/readSource";

// Round-2 F4 (#31): backend/native clone errors were rendered raw — they can
// carry model ids and stack names. Known classes map to outcome-first keys;
// unknown text renders the generic line; the DOM never sees the raw string.
const MODAL = readSource(new URL("./CloneVoiceModal.tsx", import.meta.url));

describe("clone errors reach the DOM only as catalog copy", () => {
  it("classifies the backend's own messages", () => {
    expect(
      cloneErrorKey(
        "That sample is only 1.4 seconds of speech. Cloning needs at least 3 seconds",
      ),
    ).toBe("voice.clone.err.tooShort");
    expect(cloneErrorKey("engine is not ready")).toBe(
      "voice.clone.err.engineNotReady",
    );
    expect(cloneErrorKey("No recording is running.")).toBe(
      "voice.clone.err.noRecording",
    );
    expect(cloneErrorKey("that recording is no longer available")).toBe(
      "voice.clone.err.sampleGone",
    );
  });
  it("a raw error naming a model id becomes the generic line, raw text kept off the UI", () => {
    const state = failed(
      initialState(),
      "chatterbox-0.5b: CUDA out of memory in torch",
    );
    expect(state.errorKey).toBe("voice.clone.err.generic");
    expect(state.error).toContain("chatterbox");
    expect(MODAL).not.toMatch(/\{state\.error\}|\{state\.inputsError\}/);
    expect(MODAL).toContain("t(state.errorKey ??");
    expect(MODAL).toContain("t(state.inputsErrorKey ??");
  });
});
