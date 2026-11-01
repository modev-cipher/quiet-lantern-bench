import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { EngineStatus } from "./engineStatus";
import { engineSatisfies, type EngineNeed } from "./engineSetupModel";

export interface EngineSetupRequest {
  need: EngineNeed;
  onReady: (() => void) | null;
}

let request: EngineSetupRequest | null = null;
const listeners = new Set<() => void>();

function emit(): void {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useEngineSetupRequest(): EngineSetupRequest | null {
  return useSyncExternalStore(subscribe, () => request);
}

export function openEngineSetup(
  need: EngineNeed,
  onReady: (() => void) | null = null,
): void {
  request = { need, onReady };
  emit();
}

export function closeEngineSetup(): void {
  request = null;
  emit();
}

/** True when the work can go ahead. A failed status read never stands in the way. */
export async function ensureEngine(
  need: EngineNeed,
  onReady: () => void,
): Promise<boolean> {
  try {
    const [status, variant] = await Promise.all([
      invoke<EngineStatus>("engine_status"),
      invoke<string | null>("engine_active_variant"),
    ]);
    if (engineSatisfies(status, variant, need)) return true;
  } catch (error) {
    console.error(error);
    return true;
  }
  openEngineSetup(need, onReady);
  return false;
}
