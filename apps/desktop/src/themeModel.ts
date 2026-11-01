export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

// Mirrored to localStorage so the inline script in index.html can set
// data-theme before first paint; the database row is the source of truth.
export const THEME_MIRROR_KEY = "txtcaster.themePreference";

export const THEME_PREFERENCES: readonly ThemePreference[] = [
  "system",
  "light",
  "dark",
];

export function parsePreference(value: string | null): ThemePreference {
  return value === "light" || value === "dark" ? value : "system";
}

export function resolveTheme(
  preference: ThemePreference,
  systemPrefersDark: boolean,
): ResolvedTheme {
  if (preference === "system") return systemPrefersDark ? "dark" : "light";
  return preference;
}

export interface WindowBackground {
  red: number;
  green: number;
  blue: number;
}

// getComputedStyle serialises the body background as rgb()/rgba(); the native
// window wants channel integers. Anything else means the canvas token is
// missing, and the native background is better left alone than guessed.
export function parseCssColor(value: string): WindowBackground | null {
  const match = /^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/.exec(value);
  if (match == null) return null;
  const [red, green, blue] = [match[1], match[2], match[3]].map(Number);
  if ([red, green, blue].some((channel) => channel > 255)) return null;
  return { red, green, blue };
}

export const WINDOW_CHROME_TOKENS = {
  caption: "--color-ink-900",
  text: "--color-ink-100",
  borderFocused: "--color-ember-500",
  borderUnfocused: "--color-ink-700",
} as const;

type WindowChromeRole = keyof typeof WINDOW_CHROME_TOKENS;

export type WindowChrome = Record<WindowChromeRole, WindowBackground> & {
  scheme: ThemePreference;
};

export function windowChrome(
  read: (token: string) => string,
  scheme: ThemePreference,
): WindowChrome | null {
  const color = (role: WindowChromeRole) =>
    parseCssColor(read(WINDOW_CHROME_TOKENS[role]));
  const caption = color("caption");
  const text = color("text");
  const borderFocused = color("borderFocused");
  const borderUnfocused = color("borderUnfocused");
  if (
    caption == null ||
    text == null ||
    borderFocused == null ||
    borderUnfocused == null
  ) {
    return null;
  }
  return { caption, text, borderFocused, borderUnfocused, scheme };
}
