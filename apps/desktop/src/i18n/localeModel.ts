// I18N-1 locale resolution (owner law #25 Q3): default = host OS locale;
// explicit Settings toggle (System | English | العربية) wins; the preference
// persists (Tauri settings table, mirrored to localStorage for first paint).
//
// Resolution order, pinned by localeModel.test.ts:
//   1. explicit preference "en" | "ar"
//   2. host OS locale (Tauri `host_locale` command; LC_ALL/LC_MESSAGES/LANG
//      on Linux, user default locale on Windows/macOS)
//   3. the webview's navigator.language (browser harness / command missing)
//   4. "en"
export const LOCALES = ["en", "ar"] as const;
export type Locale = (typeof LOCALES)[number];

export const LOCALE_PREFERENCES = ["system", "en", "ar"] as const;
export type LocalePreference = (typeof LOCALE_PREFERENCES)[number];

/** localStorage mirror of the persisted preference (first paint, harness). */
export const LOCALE_KEY = "txtcaster.locale";

export function parseLocalePreference(raw: string | null): LocalePreference {
  return raw === "en" || raw === "ar" ? raw : "system";
}

/** BCP-47 / POSIX tag → supported UI locale, or null when unsupported. */
export function localeFromTag(tag: string | null | undefined): Locale | null {
  if (!tag) return null;
  const language =
    tag
      .trim()
      .split(/[-_.@]/)[0]
      ?.toLowerCase() ?? "";
  return language === "ar" ? "ar" : language === "en" ? "en" : null;
}

export interface LocaleSources {
  hostLocale: string | null;
  navigatorLanguage: string | null;
}

export function resolveLocale(
  preference: LocalePreference,
  sources: LocaleSources,
): Locale {
  if (preference !== "system") return preference;
  return (
    localeFromTag(sources.hostLocale) ??
    localeFromTag(sources.navigatorLanguage) ??
    "en"
  );
}

/** The document direction a UI locale implies before any override. */
export function directionForLocale(locale: Locale): "ltr" | "rtl" {
  return locale === "ar" ? "rtl" : "ltr";
}

/** Arabic has six CLDR plural categories; pinned in localeModel.test.ts. */
export type PluralCategory = Intl.LDMLPluralRule;

export function pluralCategory(locale: Locale, count: number): PluralCategory {
  return new Intl.PluralRules(locale).select(count);
}

/**
 * Numbers inside UI copy are metrics (pages, percent, GB): Western digits,
 * LTR, in every locale — `nu-latn` keeps Arabic from switching to ٠١٢.
 */
export function numberLocale(locale: Locale): string {
  return locale === "ar" ? "ar-u-nu-latn" : "en";
}

export function formatNumber(
  locale: Locale,
  value: number,
  options?: Intl.NumberFormatOptions,
): string {
  return new Intl.NumberFormat(numberLocale(locale), options).format(value);
}
