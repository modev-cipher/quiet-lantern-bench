export const LISTENED_KEY = "txtcaster.listened";

export const MAX_TRACKED = 200;

export type ListenedMap = Record<string, number>;

export function parseListened(raw: string | null): ListenedMap {
  if (raw == null) return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return {};
  }
  if (parsed == null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return {};
  }
  const map: ListenedMap = {};
  for (const [id, value] of Object.entries(parsed as Record<string, unknown>)) {
    if (typeof value !== "number" || !Number.isFinite(value)) continue;
    if (value <= 0 || value > 1) continue;
    map[id] = value;
  }
  return map;
}

export function serialiseListened(map: ListenedMap): string {
  return JSON.stringify(map);
}

export function listenedFraction(
  currentSec: number,
  durationSec: number,
): number | null {
  if (!Number.isFinite(currentSec) || !Number.isFinite(durationSec)) return null;
  if (durationSec <= 0 || currentSec < 0) return null;
  return Math.min(1, currentSec / durationSec);
}

export function withListen(
  map: ListenedMap,
  id: string,
  fraction: number | null,
): ListenedMap {
  if (
    id === "" ||
    fraction == null ||
    !Number.isFinite(fraction) ||
    fraction <= 0 ||
    fraction > 1
  ) {
    return map;
  }
  if (map[id] === fraction) return map;
  const next: ListenedMap = { ...map, [id]: fraction };
  const ids = Object.keys(next);
  if (ids.length <= MAX_TRACKED) return next;
  for (const stale of ids.slice(0, ids.length - MAX_TRACKED)) {
    delete next[stale];
  }
  return next;
}

export function listenedOf(map: ListenedMap, id: string): number | null {
  const value = map[id];
  return typeof value === "number" ? value : null;
}

export const PERSIST_STEP = 0.005;

export function loadListened(): ListenedMap {
  try {
    return parseListened(localStorage.getItem(LISTENED_KEY));
  } catch {
    return {};
  }
}

export function saveListen(id: string, fraction: number | null): void {
  if (fraction == null) return;
  try {
    const current = loadListened();
    const next = withListen(current, id, fraction);
    if (next === current) return;
    localStorage.setItem(LISTENED_KEY, serialiseListened(next));
  } catch {
    return;
  }
}
