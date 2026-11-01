import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: () => Promise.resolve(null),
}));
vi.stubGlobal("document", {
  documentElement: {
    lang: "",
    dir: "",
    setAttribute() {},
    removeAttribute() {},
  },
});
vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem() {},
  removeItem() {},
});

import { setLocalePreference } from "./i18n/localeStore";
import { endReasonLineOf } from "./sessionHistoryStore";
import {
  INITIAL_VOICE_SESSION,
  noticeText,
  reduceVoiceSession,
} from "./voiceSessionModel";

afterEach(() => setLocalePreference("en"));

// Round-2 F3: copy captured at import (a module-level reason table; a notice
// string stored in reducer state) froze in English on an Arabic cold start
// and ignored a mid-session switch. Both now resolve at render/selector time.
describe("History reason lines and live notices follow the current locale", () => {
  it("a reason line read after switching to Arabic is Arabic", () => {
    expect(endReasonLineOf("unexpected")).toBe("Ended unexpectedly");
    setLocalePreference("ar");
    expect(endReasonLineOf("unexpected")).toBe("انتهت على نحو غير متوقّع");
    expect(endReasonLineOf("ended")).toBeNull();
  });

  it("a live notice produced in EN re-renders in Arabic after the switch", () => {
    const state = reduceVoiceSession(
      { ...INITIAL_VOICE_SESSION, status: "live" },
      { kind: "mic_state", state: "unavailable", detail: "" },
    );
    expect(state.noticeKey).toBe("voice.notice.noMic");
    expect(state.notice).toBeNull();
    expect(noticeText(state)).toBe("No microphone — type to ask instead.");
    setLocalePreference("ar");
    expect(noticeText(state)).toBe(
      "لا يوجد ميكروفون — اكتب سؤالك بدلًا من ذلك.",
    );
  });

  it("a raw backend message stays raw and clears the key", () => {
    const state = reduceVoiceSession(
      {
        ...INITIAL_VOICE_SESSION,
        status: "live",
        noticeKey: "voice.notice.noMic",
      },
      { kind: "failed", code: "send_text", message: "socket closed" },
    );
    expect(state.noticeKey).toBeNull();
    expect(noticeText(state)).toBe("send_text: socket closed");
  });
});
