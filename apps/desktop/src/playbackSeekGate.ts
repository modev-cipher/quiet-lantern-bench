// WebKitGTK 2.50 on GStreamer 1.20 (the AppImage, and Ubuntu 22.04's own
// stack) wedges when currentTime is set while the element plays or while a
// play() is still starting: it sits at the target with readyState 2 and
// never advances. Measured 2026-11-02 with a plain page; WebKitGTK 2.54 and
// pausing first both play through. So every seek runs on a paused element,
// a seek during a start waits for "playing", and playback resumes once,
// after the seeks of a drag have settled.

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
  onPlay(): void;
  onPlaying(): void;
  onPause(): void;
  onSeeked(): void;
  /// Whether the gate, not the user, owns the element's paused state.
  holds(): boolean;
  reset(): void;
}

export function createSeekGate(
  audio: SeekableAudio,
  deps: SeekGateDeps,
): SeekGate {
  let holding = false;
  let starting = false;
  let deferred: number | null = null;
  let timer: number | null = null;

  const stopTimer = () => {
    if (timer == null) return;
    deps.clearTimer(timer);
    timer = null;
  };

  const seekPaused = (time: number) => {
    if (!audio.paused && audio.readyState >= HAVE_METADATA) {
      holding = true;
      audio.pause();
    }
    audio.currentTime = time;
  };

  const takeDeferred = (): number | null => {
    const time = deferred;
    deferred = null;
    return time;
  };

  const resumeWhenQuiet = () => {
    timer = null;
    if (!holding || audio.seeking) return;
    holding = false;
    if (audio.ended) {
      deps.onEnded();
      return;
    }
    if (!deps.shouldPlay()) return;
    starting = true;
    deps.resume().catch(() => {
      starting = false;
      const time = takeDeferred();
      if (time != null && audio.paused) audio.currentTime = time;
    });
  };

  return {
    seek(time) {
      stopTimer();
      if (starting) {
        deferred = time;
        return;
      }
      seekPaused(time);
    },
    onPlay() {
      starting = true;
    },
    onPlaying() {
      starting = false;
      const time = takeDeferred();
      if (time != null) seekPaused(time);
    },
    onPause() {
      if (!starting) return;
      starting = false;
      const time = takeDeferred();
      if (time != null) audio.currentTime = time;
    },
    onSeeked() {
      if (!holding || audio.seeking) return;
      stopTimer();
      timer = deps.setTimer(resumeWhenQuiet, SEEK_RESUME_QUIET_MS);
    },
    holds: () => holding,
    reset() {
      stopTimer();
      holding = false;
      starting = false;
      deferred = null;
    },
  };
}
