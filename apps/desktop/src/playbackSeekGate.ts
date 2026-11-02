// WebKitGTK 2.50 on GStreamer 1.20 (the AppImage, and Ubuntu 22.04's own
// stack) wedges when currentTime is set while the element plays or while a
// play() is still starting: it sits at the target with readyState 2 and
// never advances. Measured 2026-11-02 with a plain page; WebKitGTK 2.54 and
// pausing first both play through. So every seek runs on a paused element,
// and playback resumes once, after the seeks of a drag have settled.

export const SEEK_RESUME_QUIET_MS = 250;

const HAVE_METADATA = 1;

export interface SeekableAudio {
  readonly paused: boolean;
  readonly seeking: boolean;
  readonly ended: boolean;
  readonly readyState: number;
  currentTime: number;
  pause(): void;
}

export interface SeekGateDeps {
  shouldPlay(): boolean;
  resume(): Promise<void>;
  onEnded(): void;
  setTimer(run: () => void, ms: number): number;
  clearTimer(id: number): void;
}

export interface SeekGate {
  seek(time: number): void;
  onSeeked(): void;
  onPlaying(): void;
  /// Whether the gate, not the user, owns the element's paused state.
  holds(): boolean;
  reset(): void;
}

type Phase = "idle" | "holding" | "resuming";

export function createSeekGate(
  audio: SeekableAudio,
  deps: SeekGateDeps,
): SeekGate {
  let phase: Phase = "idle";
  let deferred: number | null = null;
  let timer: number | null = null;

  const stopTimer = () => {
    if (timer == null) return;
    deps.clearTimer(timer);
    timer = null;
  };

  const seekPaused = (time: number) => {
    if (!audio.paused && audio.readyState >= HAVE_METADATA) {
      phase = "holding";
      audio.pause();
    }
    audio.currentTime = time;
  };

  const resumeWhenQuiet = () => {
    timer = null;
    if (phase !== "holding" || audio.seeking) return;
    if (audio.ended) {
      phase = "idle";
      deps.onEnded();
      return;
    }
    if (!deps.shouldPlay()) {
      phase = "idle";
      return;
    }
    phase = "resuming";
    deps.resume().catch(() => {
      if (phase !== "resuming") return;
      phase = "idle";
      if (deferred == null) return;
      const time = deferred;
      deferred = null;
      audio.currentTime = time;
    });
  };

  return {
    seek(time) {
      stopTimer();
      if (phase === "resuming") {
        deferred = time;
        return;
      }
      seekPaused(time);
    },
    onSeeked() {
      if (phase !== "holding" || audio.seeking) return;
      stopTimer();
      timer = deps.setTimer(resumeWhenQuiet, SEEK_RESUME_QUIET_MS);
    },
    onPlaying() {
      if (phase !== "resuming") return;
      phase = "idle";
      if (deferred == null) return;
      const time = deferred;
      deferred = null;
      seekPaused(time);
    },
    holds: () => phase !== "idle",
    reset() {
      stopTimer();
      phase = "idle";
      deferred = null;
    },
  };
}
