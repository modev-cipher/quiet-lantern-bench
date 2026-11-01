import type { ReadingClockScheduler } from "./readingClock";

export function browserScheduler(): ReadingClockScheduler {
  return {
    requestFrame: (callback) => requestAnimationFrame(callback),
    cancelFrame: (handle) => cancelAnimationFrame(handle),
    startInterval: (callback, ms) => window.setInterval(callback, ms),
    stopInterval: (handle) => window.clearInterval(handle),
    onVisibilityChange: (callback) => {
      document.addEventListener("visibilitychange", callback);
      return () => document.removeEventListener("visibilitychange", callback);
    },
    isHidden: () => document.hidden,
  };
}
