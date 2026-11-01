import { useMemo, useSyncExternalStore, type ReactNode } from "react";

import { CATALOGS, type MessageKey } from "./catalog";
import { formatMessage, formatNodes, type Params } from "./format";
import { directionForLocale, formatNumber, type Locale } from "./localeModel";
import { getSnapshot, subscribe } from "./localeStore";

export type { MessageKey } from "./catalog";
export {
  LOCALE_PREFERENCES,
  type Locale,
  type LocalePreference,
} from "./localeModel";
export {
  initLocale,
  setDirectionPreference,
  setLocalePreference,
} from "./localeStore";

export interface MessageDescriptor {
  key: MessageKey;
  params?: Params;
}

export interface I18n {
  locale: Locale;
  dir: "ltr" | "rtl";
  /** Plain string: attributes, titles, aria labels (islands as LRI…PDI). */
  t: (key: MessageKey, params?: Params) => string;
  /** React children: islands as <bdi dir="ltr" class="ltr-island">. */
  tx: (key: MessageKey, params?: Params) => ReactNode[];
  /** Metric numbers: Western digits in every locale. */
  n: (value: number, options?: Intl.NumberFormatOptions) => string;
}

export function i18nFor(locale: Locale): I18n {
  const catalog = CATALOGS[locale];
  return {
    locale,
    dir: directionForLocale(locale),
    t: (key, params) => formatMessage(locale, catalog[key], params),
    tx: (key, params) => formatNodes(locale, catalog[key], params),
    n: (value, options) => formatNumber(locale, value, options),
  };
}

/** Components: re-renders on locale change (instant switch, no reload). */
export function useI18n(): I18n {
  // getServerSnapshot: static renders (renderToStaticMarkup pins) read the same store.
  const { locale } = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return useMemo(() => i18nFor(locale), [locale]);
}

/** Models and stores: reads the current locale at call time. */
export function t(key: MessageKey, params?: Params): string {
  return i18nFor(getSnapshot().locale).t(key, params);
}
