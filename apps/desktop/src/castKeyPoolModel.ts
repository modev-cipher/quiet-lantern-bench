import { invoke } from "@tauri-apps/api/core";

import type { KeyState } from "./voiceKeysModel";

export type CastStage = "llm" | "tts";

export interface CastPoolEntry {
  baseUrl: string;
  provider: string;
  state: KeyState;
  keyPresent: boolean;
}

export interface CastKeyPool {
  llm: CastPoolEntry[];
  tts: CastPoolEntry[];
}

export function fetchCastKeyPool(): Promise<CastKeyPool> {
  return invoke<CastKeyPool>("list_cast_key_pool");
}

export function activeCloudChoices(
  pool: CastKeyPool,
  stage: CastStage,
): CastPoolEntry[] {
  return pool[stage].filter(
    (entry) => entry.keyPresent && entry.state === "active",
  );
}

export function stageHasCloudSide(pool: CastKeyPool, stage: CastStage): boolean {
  return activeCloudChoices(pool, stage).length > 0;
}
