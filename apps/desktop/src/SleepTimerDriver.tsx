import { useEffect } from "react";

import { tickSleep } from "./sleepTimerModel";
import { getSleep, setSleep } from "./sleepTimerStore";
import { dispatch, getSnapshot } from "./playbackStore";

const TICK_MS = 250;

function SleepTimerDriver() {
  useEffect(() => {
    let last = Date.now();
    let prevMs = getSnapshot().current * 1000;
    const id = window.setInterval(() => {
      const now = Date.now();
      const elapsedMs = now - last;
      last = now;
      const playback = getSnapshot();
      const currentMs = playback.current * 1000;
      const timer = getSleep();
      if (playback.item == null && timer.kind !== "off") {
        setSleep({ kind: "off" });
      }
      const step = tickSleep(getSleep(), {
        elapsedMs,
        playing: playback.playing,
        currentMs,
        prevMs,
      });
      prevMs = currentMs;
      setSleep(step.timer);
      if (step.expire) {
        dispatch({ type: "pause" });
        dispatch({ type: "setFade", fade: 1 });
        return;
      }
      if (Math.abs(playback.fade - step.fade) > 0.005) {
        dispatch({ type: "setFade", fade: step.fade });
      }
    }, TICK_MS);
    return () => window.clearInterval(id);
  }, []);
  return null;
}

export default SleepTimerDriver;
