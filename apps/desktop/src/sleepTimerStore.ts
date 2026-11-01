import { useSyncExternalStore } from "react";

import { SLEEP_OFF, type SleepTimer } from "./sleepTimerModel";

let timer: SleepTimer = SLEEP_OFF;
const listeners = new Set<() => void>();

export function getSleep(): SleepTimer {
  return timer;
}

export function setSleep(next: SleepTimer): void {
  if (next === timer) return;
  timer = next;
  for (const listener of listeners) listener();
}

export function subscribeSleep(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useSleepTimer(): SleepTimer {
  return useSyncExternalStore(subscribeSleep, getSleep);
}

export function resetSleepTimer(): void {
  setSleep(SLEEP_OFF);
}
