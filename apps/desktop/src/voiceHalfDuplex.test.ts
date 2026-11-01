import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

const invokeMock = vi.fn((command: string) => {
  void command;
  return Promise.resolve();
});
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string) => invokeMock(command),
}));

import {
  ensureHalfDuplexGate,
  resetHalfDuplexGate,
  userMute,
  userUnmute,
} from "./voiceHalfDuplex";
import {
  dispatchVoiceEvent,
  bindVoicePanelBook,
  resetVoiceSessionStore,
} from "./voiceSessionStore";
import {
  dispatch as dispatchPlayback,
  resetPlaybackStore,
} from "./playbackStore";

// The wire shape is the D3 envelope (SessionUiEnvelope in voice_session.rs).
const TEST_SESSION = "01920000-0000-7000-8000-000000000001";
function wireEvent(event: unknown): void {
  dispatchVoiceEvent({ session_id: TEST_SESSION, event });
}

function goLiveListening(): void {
  wireEvent({ kind: "lifecycle", state: "connected" });
  wireEvent({ kind: "mic_state", state: "listening", detail: "" });
}

function calls(): unknown[] {
  return invokeMock.mock.calls.map((call) => call[0]);
}

beforeEach(() => {
  vi.useFakeTimers();
  invokeMock.mockClear();
  resetVoiceSessionStore();
  resetPlaybackStore();
  resetHalfDuplexGate();
  ensureHalfDuplexGate();
});

describe("voiceHalfDuplex", () => {
  it("changing the displayed scope sends no microphone commands and preserves user mute", async () => {
    bindVoicePanelBook(null);
    goLiveListening();
    await userMute();
    wireEvent({ kind: "mic_state", state: "muted", detail: "user" });
    invokeMock.mockClear();
    bindVoicePanelBook("another-book");
    bindVoicePanelBook(null);
    vi.advanceTimersByTime(1500);
    expect(calls()).toEqual([]);
  });
  it("soft-gates when the tutor starts speaking and releases after the tail", () => {
    goLiveListening();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    // R2 defect 1: the AUTO path uses the soft gate — the backend holds
    // capture and flushes the release window's words; the hard mute pair
    // stays reserved for user privacy mutes and narration.
    expect(calls()).toContain("voice_session_gate");
    expect(calls()).not.toContain("voice_session_mute");
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    expect(calls()).not.toContain("voice_session_ungate");
    vi.advanceTimersByTime(400);
    expect(calls()).toContain("voice_session_ungate");
    expect(calls()).not.toContain("voice_session_unmute");
  });

  it("narration keeps the HARD mute (the backend cannot see webview playback)", () => {
    goLiveListening();
    dispatchPlayback({ type: "enginePlaying", playing: true });
    expect(calls()).toContain("voice_session_mute");
    expect(calls()).not.toContain("voice_session_gate");
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    dispatchPlayback({ type: "enginePlaying", playing: false });
    vi.advanceTimersByTime(400);
    expect(calls()).toContain("voice_session_unmute");
    expect(calls()).not.toContain("voice_session_ungate");
  });

  it("a narration burst inside the mic-state echo window still releases HARD", () => {
    goLiveListening();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    expect(calls()).toContain("voice_session_gate");
    // Narration starts AND stops before the gate's mic_state echo arrives —
    // the upgrade path never fires, but the release must still be HARD:
    // the held ring may carry narration echo (Kritias round finding 3).
    dispatchPlayback({ type: "enginePlaying", playing: true });
    dispatchPlayback({ type: "enginePlaying", playing: false });
    expect(calls()).not.toContain("voice_session_mute");
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    vi.advanceTimersByTime(400);
    expect(calls()).toContain("voice_session_unmute");
    expect(calls()).not.toContain("voice_session_ungate");
  });

  it("a REJECTED userMute never erases narration truth (Kritias round-3 pin)", async () => {
    goLiveListening();
    // A soft gate that overlapped narration: narration truth is armed.
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    dispatchPlayback({ type: "enginePlaying", playing: true });
    dispatchPlayback({ type: "enginePlaying", playing: false });
    // The user tries to mute; the runtime refuses; the rollback must
    // restore EVERYTHING it cleared — narration truth included.
    invokeMock.mockRejectedValueOnce(new Error("runtime refused"));
    await expect(userMute()).rejects.toThrow("runtime refused");
    invokeMock.mockClear();
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    vi.advanceTimersByTime(400);
    // The release must be HARD: its ring may carry narration echo. Before
    // the fix, the rollback left narrationDuringGate cleared and this
    // released via the soft flush.
    expect(calls()).toContain("voice_session_unmute");
    expect(calls()).not.toContain("voice_session_ungate");
  });

  it("a refused release restores truth (narration included) and retries", async () => {
    goLiveListening();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    dispatchPlayback({ type: "enginePlaying", playing: true });
    dispatchPlayback({ type: "enginePlaying", playing: false });
    // The first release attempt is REFUSED by the runtime.
    invokeMock.mockRejectedValueOnce(new Error("mailbox refused"));
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    vi.advanceTimersByTime(400);
    expect(calls()).toContain("voice_session_unmute");
    invokeMock.mockClear();
    // Let the rejection propagate, then the re-armed timer retries — still
    // HARD: narration truth survived the failed attempt.
    await Promise.resolve();
    await Promise.resolve();
    vi.advanceTimersByTime(400);
    expect(calls()).toContain("voice_session_unmute");
    expect(calls()).not.toContain("voice_session_ungate");
  });

  it("narration starting under a soft gate upgrades it to a hard mute", () => {
    goLiveListening();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    expect(calls()).toContain("voice_session_gate");
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    // Narration begins while the soft gate holds: the held ring would flush
    // narration echo the backend cannot see — upgrade to the hard mute.
    dispatchPlayback({ type: "enginePlaying", playing: true });
    expect(calls()).toContain("voice_session_mute");
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    dispatchPlayback({ type: "enginePlaying", playing: false });
    vi.advanceTimersByTime(400);
    // The upgraded gate releases with the hard unmute — never a flush.
    expect(calls()).toContain("voice_session_unmute");
    expect(calls()).not.toContain("voice_session_ungate");
  });

  it("does not auto-unmute past a user-pressed Mute", async () => {
    goLiveListening();
    await userMute();
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    vi.advanceTimersByTime(1000);
    expect(calls()).not.toContain("voice_session_unmute");
    expect(calls()).not.toContain("voice_session_ungate");
  });

  it("a user unmute during tutor speech holds the mic open until quiet", async () => {
    goLiveListening();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    invokeMock.mockClear();
    await userUnmute();
    wireEvent({ kind: "mic_state", state: "listening", detail: "" });
    expect(calls()).not.toContain("voice_session_gate");
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    expect(calls()).toContain("voice_session_gate");
  });

  it("holds the pending unmute when audio resumes before the tail elapses", () => {
    goLiveListening();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    vi.advanceTimersByTime(1000);
    expect(calls()).not.toContain("voice_session_ungate");
  });

  it("gates on the first assistant delta, before the speaking edge arrives", () => {
    goLiveListening();
    wireEvent({ kind: "transcript", role: "assistant", text: "The" });
    expect(calls()).toContain("voice_session_gate");
  });

  it("requests the gate once per edge, not once per event", () => {
    goLiveListening();
    wireEvent({ kind: "transcript", role: "assistant", text: "The" });
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    expect(
      calls().filter((name) => name === "voice_session_gate"),
    ).toHaveLength(1);
  });

  it("stays muted across a between-segments audio gap while deltas are fresh", () => {
    goLiveListening();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    wireEvent({ kind: "transcript", role: "assistant", text: "part" });
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    vi.advanceTimersByTime(400);
    expect(calls()).not.toContain("voice_session_ungate");
    vi.advanceTimersByTime(2000);
    expect(calls()).toContain("voice_session_ungate");
  });

  it("a text-only delta trickle cannot latch the mute (owner session 01a04442)", () => {
    goLiveListening();
    wireEvent({ kind: "transcript", role: "assistant", text: "slow" });
    expect(calls()).toContain("voice_session_gate");
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    for (let at = 0; at < 120; at += 1) {
      vi.advanceTimersByTime(800);
      wireEvent({ kind: "transcript", role: "assistant", text: "drip" });
    }
    vi.advanceTimersByTime(3000);
    expect(calls()).toContain("voice_session_ungate");
  });

  it("the standalone quiet window still bridges the turn-start delta-before-audio gap", () => {
    goLiveListening();
    wireEvent({ kind: "transcript", role: "assistant", text: "The" });
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    vi.advanceTimersByTime(400);
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    vi.advanceTimersByTime(1000);
    expect(calls()).not.toContain("voice_session_ungate");
  });

  it("does nothing outside a live session", () => {
    dispatchPlayback({ type: "enginePlaying", playing: true });
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    expect(calls()).toEqual([]);
  });

  it("a refused user mute rolls back and leaves auto-mute armed", async () => {
    goLiveListening();
    invokeMock.mockRejectedValueOnce(new Error("runtime refused"));
    await expect(userMute()).rejects.toThrow("runtime refused");
    invokeMock.mockClear();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    expect(calls()).toContain("voice_session_gate");
  });

  it("a refused user unmute restores the user-muted latch", async () => {
    goLiveListening();
    await userMute();
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    invokeMock.mockRejectedValueOnce(new Error("runtime refused"));
    await expect(userUnmute()).rejects.toThrow("runtime refused");
    invokeMock.mockClear();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    vi.advanceTimersByTime(1000);
    expect(calls()).not.toContain("voice_session_unmute");
    expect(calls()).not.toContain("voice_session_ungate");
  });

  it("a session end cancels the pending unmute", () => {
    goLiveListening();
    wireEvent({ kind: "audio_state", state: "speaking", detail: "" });
    wireEvent({ kind: "mic_state", state: "muted", detail: "" });
    wireEvent({ kind: "audio_state", state: "idle", detail: "" });
    wireEvent({ kind: "ended", reason: "stopped" });
    vi.advanceTimersByTime(1000);
    expect(calls()).not.toContain("voice_session_ungate");
    expect(calls()).not.toContain("voice_session_unmute");
  });
});
