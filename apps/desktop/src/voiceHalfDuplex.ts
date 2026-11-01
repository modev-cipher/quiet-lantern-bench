import { invoke } from "@tauri-apps/api/core";
import {
  assistantDeltaAgeMs,
  getActiveVoiceSnapshot,
  noteVoiceCommand,
  subscribeVoice,
} from "./voiceSessionStore";
import {
  getSnapshot as getPlaybackSnapshot,
  subscribe as subscribePlayback,
} from "./playbackStore";

const UNMUTE_TAIL_MS = 350;
const ASSISTANT_DELTA_HOLD_MS = 1200;
const QUIET_DELTA_STANDALONE_MS = 2000;

let armed = false;
let autoMuted = false;
// R2 defect 1: whether the active auto-mute used the HARD command
// (narration — the backend cannot see webview playback, so its idle-tail
// flush would leak narration echo) instead of the soft capture gate.
let autoHard = false;
// Kritias round finding 3: whether narration PLAYED at any point while the
// current auto-gate held. The upgrade below rides the mic_state IPC echo,
// so a narration burst that starts AND stops inside that echo window would
// otherwise release with a soft flush — flushing narration echo to the
// wire. The release decision reads this observed truth, not just the
// upgrade's success.
let narrationDuringGate = false;
let userMuted = false;
let talkOverHold = false;
let mutePending = false;
let unmuteTimer: ReturnType<typeof setTimeout> | null = null;
let quietStreakAt: number | null = null;

function clearUnmuteTimer(): void {
  if (unmuteTimer != null) {
    clearTimeout(unmuteTimer);
    unmuteTimer = null;
  }
}

function audioBusy(): boolean {
  const { session } = getActiveVoiceSnapshot();
  return session.audio === "speaking" || getPlaybackSnapshot().playing;
}

function deltaRecencyHolds(): boolean {
  const fresh = assistantDeltaAgeMs() < ASSISTANT_DELTA_HOLD_MS;
  if (audioBusy()) {
    quietStreakAt = null;
    return fresh;
  }
  if (!fresh) {
    quietStreakAt = null;
    return false;
  }
  if (quietStreakAt == null) quietStreakAt = Date.now();
  return Date.now() - quietStreakAt < QUIET_DELTA_STANDALONE_MS;
}

function silenceWanted(): boolean {
  const holds = deltaRecencyHolds();
  return audioBusy() || holds;
}

function silenceReason(): string {
  const { session } = getActiveVoiceSnapshot();
  if (session.audio === "speaking") return "assistant_audio";
  if (getPlaybackSnapshot().playing) return "narration";
  return "assistant_delta";
}

function armUnmuteTimer(): void {
  if (unmuteTimer != null) return;
  unmuteTimer = setTimeout(() => {
    unmuteTimer = null;
    const now = getActiveVoiceSnapshot().session;
    const stillLive = now.status === "live" || now.status === "renewing";
    if (!stillLive || userMuted || talkOverHold || !autoMuted) return;
    if (silenceWanted()) {
      armUnmuteTimer();
      return;
    }
    autoMuted = false;
    // R2 defect 1: the soft auto path releases the capture gate — the
    // backend flushes the idle-tail chunks it held, recovering the words
    // spoken in this timer's window. Hard auto-mutes (narration) and user
    // mutes keep the zero-capture commands. Kritias round finding 3: a gate
    // that ever OVERLAPPED narration releases hard too — its held ring may
    // carry narration echo the backend cannot distinguish.
    const wasHard = autoHard || narrationDuringGate;
    const releaseCommand = wasHard
      ? "voice_session_unmute"
      : "voice_session_ungate";
    autoHard = false;
    narrationDuringGate = false;
    noteVoiceCommand("auto_unmute", { reason: "audio_idle" });
    invoke(releaseCommand, { reason: "audio_idle" }).catch(() => {
      // Kritias round 3 finding 2: the runtime REFUSED the release — the
      // gate is still closed backend-side. Restore the truth this attempt
      // cleared (narration truth included) and retry; silence here would
      // leave the mic dead with the UI lying about it.
      autoMuted = true;
      autoHard = wasHard;
      narrationDuringGate = wasHard;
      noteVoiceCommand("auto_unmute_failed");
      armUnmuteTimer();
    });
  }, UNMUTE_TAIL_MS);
}

function evaluate(): void {
  const { session } = getActiveVoiceSnapshot();
  const live = session.status === "live" || session.status === "renewing";
  if (!live) {
    clearUnmuteTimer();
    autoMuted = false;
    autoHard = false;
    narrationDuringGate = false;
    talkOverHold = false;
    mutePending = false;
    return;
  }
  if (session.mic === "muted") mutePending = false;
  if (autoMuted && getPlaybackSnapshot().playing) narrationDuringGate = true;
  const wanted = silenceWanted();
  if (!wanted) talkOverHold = false;
  if (userMuted || talkOverHold) {
    clearUnmuteTimer();
    return;
  }
  if (wanted && session.mic === "listening" && !mutePending) {
    clearUnmuteTimer();
    autoMuted = true;
    mutePending = true;
    const reason = silenceReason();
    const hard = reason === "narration";
    autoHard = hard;
    narrationDuringGate = hard;
    noteVoiceCommand("auto_mute", { reason });
    invoke(hard ? "voice_session_mute" : "voice_session_gate", {
      reason,
    }).catch(() => {
      autoMuted = false;
      autoHard = false;
      mutePending = false;
    });
    return;
  }
  // Narration starting UNDER a soft gate upgrades it to a hard mute: the
  // held ring would otherwise flush narration echo the backend cannot see.
  if (
    autoMuted &&
    !autoHard &&
    session.mic === "muted" &&
    getPlaybackSnapshot().playing
  ) {
    autoHard = true;
    noteVoiceCommand("auto_mute_upgrade", { reason: "narration" });
    invoke("voice_session_mute", { reason: "narration" }).catch(() => {
      autoHard = false;
    });
    return;
  }
  if (!audioBusy() && autoMuted && session.mic === "muted") {
    armUnmuteTimer();
  }
}

export function ensureHalfDuplexGate(): void {
  if (armed) return;
  armed = true;
  subscribeVoice(evaluate);
  subscribePlayback(evaluate);
}

export function userMute(): Promise<void> {
  const previousAutoMuted = autoMuted;
  // Kritias round 3 finding 1: narration truth may only be cleared by the
  // RELEASE DECISION itself — a rejected mute's rollback must restore it,
  // or the eventual soft release would flush narration echo.
  const previousAutoHard = autoHard;
  const previousNarrationDuringGate = narrationDuringGate;
  userMuted = true;
  autoMuted = false;
  autoHard = false;
  narrationDuringGate = false;
  talkOverHold = false;
  clearUnmuteTimer();
  noteVoiceCommand("mute", { reason: "user" });
  return invoke<void>("voice_session_mute", { reason: "user" }).catch(
    (error: unknown) => {
      userMuted = false;
      autoMuted = previousAutoMuted;
      autoHard = previousAutoHard;
      narrationDuringGate = previousNarrationDuringGate;
      noteVoiceCommand("mute_failed");
      evaluate();
      throw error;
    },
  );
}

export function userUnmute(): Promise<void> {
  const previousUserMuted = userMuted;
  const previousAutoMuted = autoMuted;
  const previousTalkOverHold = talkOverHold;
  userMuted = false;
  autoMuted = false;
  clearUnmuteTimer();
  talkOverHold = silenceWanted();
  noteVoiceCommand("unmute", { reason: "user" });
  return invoke<void>("voice_session_unmute", { reason: "user" }).catch(
    (error: unknown) => {
      userMuted = previousUserMuted;
      autoMuted = previousAutoMuted;
      talkOverHold = previousTalkOverHold;
      noteVoiceCommand("unmute_failed");
      evaluate();
      throw error;
    },
  );
}

export function resetHalfDuplexGate(): void {
  clearUnmuteTimer();
  autoMuted = false;
  autoHard = false;
  narrationDuringGate = false;
  userMuted = false;
  talkOverHold = false;
  quietStreakAt = null;
}
