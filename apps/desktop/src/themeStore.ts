import { invoke } from "@tauri-apps/api/core";

import {
  parseCssColor,
  parsePreference,
  resolveTheme,
  THEME_MIRROR_KEY,
  windowChrome,
  type ResolvedTheme,
  type ThemePreference,
} from "./themeModel";

export interface ThemeState {
  preference: ThemePreference;
  resolved: ResolvedTheme;
}

let state: ThemeState = { preference: "system", resolved: "light" };
const listeners = new Set<() => void>();

export function getSnapshot(): ThemeState {
  return state;
}

export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function systemPrefersDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function pushWindowBackground(): void {
  const background = parseCssColor(
    getComputedStyle(document.body).backgroundColor,
  );
  if (background == null) return;
  void invoke("set_window_background", { ...background }).catch(() => {});
}

function readToken(token: string): string {
  const declared = getComputedStyle(document.documentElement)
    .getPropertyValue(token)
    .trim();
  if (declared === "") return "";
  const probe = document.createElement("span");
  probe.hidden = true;
  probe.style.color = `var(${token})`;
  document.body.appendChild(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
}

function pushWindowChrome(preference: ThemePreference): void {
  const palette = windowChrome(readToken, preference);
  if (palette == null) return;
  void invoke("set_window_chrome", { palette }).catch(() => {});
}

function applyPreference(preference: ThemePreference): void {
  const resolved = resolveTheme(preference, systemPrefersDark());
  document.documentElement.dataset.theme = resolved;
  try {
    localStorage.setItem(THEME_MIRROR_KEY, preference);
  } catch {
    // the mirror only speeds up first paint; losing it costs nothing
  }
  state = { preference, resolved };
  for (const listener of listeners) listener();
  pushWindowBackground();
  pushWindowChrome(preference);
}

export function setThemePreference(preference: ThemePreference): void {
  applyPreference(preference);
  void invoke("set_theme_preference", { preference }).catch(console.error);
}

function reveal(): void {
  void invoke("reveal_main_window").catch(() => {});
}

export function initTheme(): () => void {
  let mirrored: string | null = null;
  try {
    mirrored = localStorage.getItem(THEME_MIRROR_KEY);
  } catch {
    mirrored = null;
  }
  applyPreference(parsePreference(mirrored));
  invoke<string>("get_theme_preference")
    .then((stored) => {
      const preference = parsePreference(stored);
      if (preference !== state.preference) applyPreference(preference);
      reveal();
    })
    .catch((error: unknown) => {
      console.error(error);
      reveal();
    });
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  const onSystemChange = () => {
    if (state.preference === "system") applyPreference("system");
  };
  media.addEventListener("change", onSystemChange);
  return () => media.removeEventListener("change", onSystemChange);
}
