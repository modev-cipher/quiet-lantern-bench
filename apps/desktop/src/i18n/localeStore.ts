import { invoke } from "@tauri-apps/api/core";

import {
  applyDirection,
  loadDirectionPreference,
  resolveDirection,
  storeDirectionPreference,
  type DirectionPreference,
} from "../directionModel";
import {
  LOCALE_KEY,
  directionForLocale,
  parseLocalePreference,
  resolveLocale,
  type Locale,
  type LocalePreference,
} from "./localeModel";

// Mirrors themeStore: a module store read through useSyncExternalStore.
// Switching is instant (no reload): every subscriber re-renders with the new
// catalog, and <html lang dir> is rewritten at the root.
export interface LocaleState {
  preference: LocalePreference;
  locale: Locale;
  hostLocale: string | null;
}

let state: LocaleState = {
  preference: "system",
  locale: "en",
  hostLocale: null,
};
const listeners = new Set<() => void>();
let preferenceRevision = 0;
let initializationRevision = 0;

export function getSnapshot(): LocaleState {
  return state;
}

export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function navigatorLanguage(): string | null {
  return typeof navigator === "undefined" ? null : (navigator.language ?? null);
}

/** Direction = the UI locale's direction unless the explicit direction preference overrides it. */
export function applyResolvedDirection(
  directionPreference = loadDirectionPreference(),
): void {
  const fromLocale = directionForLocale(state.locale);
  applyDirection(
    directionPreference === "system"
      ? fromLocale
      : resolveDirection(directionPreference, state.locale),
  );
}

function applyPreference(
  preference: LocalePreference,
  hostLocale: string | null,
): void {
  const locale = resolveLocale(preference, {
    hostLocale,
    navigatorLanguage: navigatorLanguage(),
  });
  state = { preference, locale, hostLocale };
  document.documentElement.lang = locale;
  applyResolvedDirection();
  try {
    if (preference === "system") localStorage.removeItem(LOCALE_KEY);
    else localStorage.setItem(LOCALE_KEY, preference);
  } catch {
    // the mirror only speeds up first paint
  }
  for (const listener of listeners) listener();
}

export function setLocalePreference(preference: LocalePreference): void {
  ++preferenceRevision;
  applyPreference(preference, state.hostLocale);
  void invoke("set_locale_preference", { preference }).catch(console.error);
}

export function setDirectionPreference(preference: DirectionPreference): void {
  storeDirectionPreference(preference);
  applyResolvedDirection(preference);
}

/** First paint from the mirror, then the persisted preference + host locale from the backend. */
export function initLocale(): void {
  const initialization = ++initializationRevision;
  const choiceAtStart = preferenceRevision;
  let mirrored: string | null = null;
  try {
    mirrored = localStorage.getItem(LOCALE_KEY);
  } catch {
    mirrored = null;
  }
  applyPreference(parseLocalePreference(mirrored), null);
  const host = invoke<string | null>("host_locale").catch(() => null);
  const stored = invoke<string>("get_locale_preference").catch(() => mirrored);
  void Promise.all([host, stored]).then(([hostLocale, preference]) => {
    if (initialization !== initializationRevision) return;
    applyPreference(
      preferenceRevision === choiceAtStart
        ? parseLocalePreference(preference)
        : state.preference,
      hostLocale,
    );
  });
}
