import { t, type MessageKey } from "./i18n";
// The "Personalize Learning Style & Tone" toggle (owner ruling D-2): the
// settings-surface half of the voice tutor's learning-profile privacy
// control. The Rust side owns the behavior (profile mode is decided once at
// session start); this model owns what the card has to decide:
//
// - Never-persist wins: while transcript retention is "never" the tutor runs
//   memory-only regardless of the toggle, so the toggle renders DISABLED at
//   its stored position with an explanatory note — flipping it there would
//   promise a change that cannot happen.
// - Off is "stop using", not "destroy": saved preferences stay on disk and
//   come back when the toggle does. The hint says so, since a privacy toggle
//   that silently deletes — or one the user believes deletes — both break
//   trust.

export interface LearningPrivacy {
  personalizeEnabled: boolean;
  neverPersist: boolean;
}

// Copy lives in the catalog (settings.personalize*, settings.neverPersistNote,
// settings.privacyUnreadableNote). The exported note is the English form for
// the model pins; personalizeNoteOf reads the current locale at call time.
export const NEVER_PERSIST_NOTE = t("settings.neverPersistNote");
/** Under never-persist the toggle is inert — memory-only always wins. */
export function personalizeLocked(privacy: LearningPrivacy): boolean {
  return privacy.neverPersist;
}

/** The note under the toggle, or null when the toggle is live. */
export function personalizeNoteOf(privacy: LearningPrivacy): string | null {
  return privacy.neverPersist ? t("settings.neverPersistNote") : null;
}

export interface LearningPreference {
  key: string;
  value: string;
  updatedAt: number;
}

const TOPIC_KEYS = new Map<string, MessageKey>([
  ["language", "settings.memoryTopicLanguage"],
  ["pace", "settings.memoryTopicPace"],
  ["style", "settings.memoryTopicStyle"],
  ["level", "settings.memoryTopicLevel"],
  ["goal", "settings.memoryTopicGoal"],
]);

export function preferenceTopicLabel(key: string): string {
  const catalogKey = TOPIC_KEYS.get(key.trim().toLowerCase());
  return catalogKey == null ? key : t(catalogKey);
}
