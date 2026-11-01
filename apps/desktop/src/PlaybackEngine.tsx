import { useEffect, useRef } from "react";

import {
  PERSIST_STEP,
  listenedFraction,
  saveListen,
} from "./listeningProgress";
import { noteRecent } from "./homeModel";
import { forgetMediaUrl, getMediaUrl } from "./mediaUrl";
import { reportPlaybackEvent } from "./readingClockDiagnostics";
import { registerAudioElement } from "./playbackElement";
import { applyPlayerVolume, streamStillSettling } from "./playbackVolumeModel";
import {
  createSourceSwitcher,
  reconcileAction,
  sourceKey,
  type SourceSwitcher,
} from "./playbackSourceSwitch";
import { dispatch, getSnapshot } from "./playbackStore";
import { usePlayback } from "./usePlayback";

function PlaybackEngine() {
  const audioRef = useRef<HTMLAudioElement>(null);
  const switcher = useRef<SourceSwitcher | null>(null);
  const lastListened = useRef(-1);
  const listenedKey = useRef<string | null>(null);
  const streamStartedAt = useRef<number | null>(null);
  const item = usePlayback((s) => s.item);
  const playing = usePlayback((s) => s.playing);
  const seekTo = usePlayback((s) => s.seekTo);
  const seekNonce = usePlayback((s) => s.seekNonce);
  const rate = usePlayback((s) => s.rate);
  const muted = usePlayback((s) => s.muted);
  const volume = usePlayback((s) => s.volume);
  const fade = usePlayback((s) => s.fade);

  useEffect(() => {
    registerAudioElement(audioRef.current);
    return () => registerAudioElement(null);
  }, []);

  const itemId = item?.id ?? null;

  useEffect(() => {
    if (itemId != null) noteRecent(itemId);
  }, [itemId]);

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    if (switcher.current == null) {
      switcher.current = createSourceSwitcher(audio, {
        resolveUrl: getMediaUrl,
        forgetUrl: forgetMediaUrl,
        shouldAutoplay: () => getSnapshot().playing,
        onEngineError: (message) => dispatch({ type: "engineError", message }),
      });
    }
    if (item != null) {
      const key = sourceKey(item);
      if (listenedKey.current !== key) {
        listenedKey.current = key;
        lastListened.current = -1;
      }
    } else {
      listenedKey.current = null;
      lastListened.current = -1;
    }
    switcher.current.sync(item);
  }, [item]);

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio || !item) return;
    // Custody guard (F9 storm, R2-2): while a switch is mid-flight the
    // switcher owns the element; reconciling against raw element state here
    // called play() on a source-less element (WebKitGTK keeps a stale
    // currentSrc after detach) and the event/store feedback became a
    // permanent play/pause oscillation — see reconcileAction.
    const engine = switcher.current;
    const action = reconcileAction(
      playing,
      audio.paused,
      engine?.installedKey() ?? null,
      engine?.wantsSource() ?? false,
    );
    if (action === "play") {
      // play() rejections are async: capture the install GENERATION this
      // attempt plays against, and report only while no re-install has
      // happened since. Without the capture, a stale rejection (e.g. the
      // next switch's detach aborting this play) lands after the next
      // install and marks a healthy source as errored — and path or key
      // identity cannot express this (A1 -> B -> A2 re-installs the same
      // path).
      const attempt = engine?.installGeneration() ?? -1;
      void audio.play().catch(() => {
        if (switcher.current?.playAttemptStillCurrent(attempt)) {
          dispatch({ type: "engineError" });
        }
      });
    } else if (action === "pause") {
      audio.pause();
    } else if (action === "retry" && engine != null) {
      // A resolution failed earlier (nothing installed, nothing in flight)
      // and the user asked to play again: re-drive the switch so the URL
      // is re-requested instead of leaving a dead "playing" UI.
      engine.sync(item);
    }
  }, [playing, item]);

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio || seekTo === null) return;
    audio.currentTime = seekTo;
    dispatch({ type: "seekConsumed" });
  }, [seekNonce, seekTo]);

  useEffect(() => {
    const audio = audioRef.current;
    if (audio) audio.playbackRate = rate;
  }, [rate, item]);

  useEffect(() => {
    const audio = audioRef.current;
    if (audio) audio.muted = muted;
  }, [muted, item]);

  useEffect(() => {
    const audio = audioRef.current;
    if (audio) audio.volume = volume * fade;
  }, [volume, fade, item]);

  const installed = () => switcher.current?.installedKey() ?? null;

  const settleVolume = (element: HTMLAudioElement) => {
    if (streamStillSettling(streamStartedAt.current, Date.now())) {
      applyPlayerVolume(element, getSnapshot());
    }
  };

  return (
    <audio
      ref={audioRef}
      preload="auto"
      className="hidden"
      onLoadedMetadata={(event) =>
        dispatch({
          type: "engineDuration",
          duration: event.currentTarget.duration,
        })
      }
      onDurationChange={(event) =>
        dispatch({
          type: "engineDuration",
          duration: event.currentTarget.duration,
        })
      }
      onTimeUpdate={(event) => {
        const element = event.currentTarget;
        settleVolume(element);
        // Same custody rule as onPlay/onPause: detached-element churn must
        // not clobber the store's position mid-switch.
        if (installed() == null) return;
        dispatch({ type: "engineTime", current: element.currentTime });
        if (item == null || installed() !== sourceKey(item)) return;
        const fraction = listenedFraction(
          element.currentTime,
          element.duration,
        );
        if (fraction == null) return;
        if (Math.abs(fraction - lastListened.current) < PERSIST_STEP) return;
        lastListened.current = fraction;
        saveListen(item.id, fraction);
      }}
      onPlay={() => {
        // Mid-switch play events come from engine churn on a detached
        // element (the F9 storm's true=side of the store flip-flop), never
        // from a source the user can hear; only installed sources speak
        // for engine state.
        if (installed() == null) return;
        dispatch({ type: "enginePlaying", playing: true });
      }}
      onPause={(event) => {
        // A pause with no installed source is the switch detach (the old
        // pipeline being released) or churn before the new source attaches
        // — never the user; reporting it as engine state would clobber the
        // play intent the switch carries. It still reaches diagnostics.
        // Custody alone is the test: WebKitGTK keeps a stale currentSrc
        // after detach, so element state cannot supplement it (that stale
        // guard is what let the R2-2 storm through). With a source
        // attached, installedKey() is set in the same tick as src, so real
        // element pauses always pass this guard.
        if (installed() == null) {
          reportPlaybackEvent("pause", event.currentTarget.currentTime);
          return;
        }
        dispatch({ type: "enginePlaying", playing: false });
        reportPlaybackEvent("pause", event.currentTarget.currentTime);
        if (item != null && installed() === sourceKey(item)) {
          saveListen(
            item.id,
            listenedFraction(
              event.currentTarget.currentTime,
              event.currentTarget.duration,
            ),
          );
        }
      }}
      onWaiting={() => dispatch({ type: "engineBuffering", buffering: true })}
      onVolumeChange={(event) => settleVolume(event.currentTarget)}
      onPlaying={(event) => {
        streamStartedAt.current = Date.now();
        settleVolume(event.currentTarget);
        dispatch({ type: "engineBuffering", buffering: false });
      }}
      onCanPlay={() => dispatch({ type: "engineBuffering", buffering: false })}
      onEnded={() => {
        dispatch({ type: "engineEnded" });
        if (item != null && installed() === sourceKey(item)) {
          saveListen(item.id, 1);
        }
      }}
      onError={(event) => {
        reportPlaybackEvent("error", event.currentTarget.currentTime);
        if (switcher.current?.wantsSource()) dispatch({ type: "engineError" });
      }}
    />
  );
}

export default PlaybackEngine;
