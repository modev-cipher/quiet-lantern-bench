import type { PluralForms } from "./format";
import type { Locale } from "./localeModel";
import arRecovery from "./messages/ar/recovery.json";
import enRecovery from "./messages/en/recovery.json";
import arJobs from "./messages/ar/jobs.json";
import arProviders from "./messages/ar/providers.json";
import arLibrary from "./messages/ar/library.json";
import arModels from "./messages/ar/models.json";
import arPlayer from "./messages/ar/player.json";
import arReader from "./messages/ar/reader.json";
import arSettings from "./messages/ar/settings.json";
import arShell from "./messages/ar/shell.json";
import arTutor from "./messages/ar/tutor.json";
import arVoice from "./messages/ar/voice.json";
import enJobs from "./messages/en/jobs.json";
import enProviders from "./messages/en/providers.json";
import enLibrary from "./messages/en/library.json";
import enModels from "./messages/en/models.json";
import enPlayer from "./messages/en/player.json";
import enReader from "./messages/en/reader.json";
import enSettings from "./messages/en/settings.json";
import enShell from "./messages/en/shell.json";
import enTutor from "./messages/en/tutor.json";
import enVoice from "./messages/en/voice.json";

// EN is the source of truth: its key set IS the type. A key missing from the
// Arabic catalog is a TYPE error here (Catalog is exact over MessageKey); a
// key the Arabic catalog has but EN lacks is caught by scripts/i18n_catalog_lint.mjs.
export const en = {
  ...enRecovery,
  ...enProviders,
  ...enReader,
  ...enPlayer,
  ...enShell,
  ...enSettings,
  ...enModels,
  ...enVoice,
  ...enTutor,
  ...enJobs,
  ...enLibrary,
};

export type MessageKey = keyof typeof en;

export type Catalog = {
  [K in MessageKey]: (typeof en)[K] extends string ? string : PluralForms;
};

export const ar: Catalog = {
  ...arRecovery,
  ...arProviders,
  ...arReader,
  ...arPlayer,
  ...arShell,
  ...arSettings,
  ...arModels,
  ...arVoice,
  ...arTutor,
  ...arJobs,
  ...arLibrary,
};

export const CATALOGS: Record<Locale, Catalog> = { en, ar };
