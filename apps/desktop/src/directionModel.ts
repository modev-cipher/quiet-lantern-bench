export const DIRECTION_KEY = "txtcaster.direction";

export const DIRECTION_PREFERENCES = ["system", "ltr", "rtl"] as const;

export type DirectionPreference = (typeof DIRECTION_PREFERENCES)[number];

export type Direction = "ltr" | "rtl";

const RTL_LANGUAGES = new Set([
  "ar",
  "he",
  "fa",
  "ur",
  "ps",
  "sd",
  "ug",
  "yi",
  "dv",
  "ckb",
]);

export function parseDirectionPreference(
  raw: string | null,
): DirectionPreference {
  return raw === "ltr" || raw === "rtl" ? raw : "system";
}

export function systemDirection(locale: string): Direction {
  const language = locale.split(/[-_]/)[0]?.toLowerCase() ?? "";
  return RTL_LANGUAGES.has(language) ? "rtl" : "ltr";
}

export function resolveDirection(
  preference: DirectionPreference,
  locale: string,
): Direction {
  if (preference === "ltr" || preference === "rtl") return preference;
  return systemDirection(locale);
}

export function loadDirectionPreference(): DirectionPreference {
  try {
    return parseDirectionPreference(localStorage.getItem(DIRECTION_KEY));
  } catch {
    return "system";
  }
}

export function storeDirectionPreference(
  preference: DirectionPreference,
): void {
  try {
    if (preference === "system") localStorage.removeItem(DIRECTION_KEY);
    else localStorage.setItem(DIRECTION_KEY, preference);
  } catch {
    return;
  }
}

export function applyDirection(direction: Direction): void {
  document.documentElement.setAttribute("dir", direction);
}

export function initDirection(): void {
  applyDirection(
    resolveDirection(loadDirectionPreference(), navigator.language ?? "en"),
  );
}

export function setDirectionPreference(preference: DirectionPreference): void {
  storeDirectionPreference(preference);
  applyDirection(resolveDirection(preference, navigator.language ?? "en"));
}
