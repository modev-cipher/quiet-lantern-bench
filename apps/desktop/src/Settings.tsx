import ModelRemovalDialog from "./ModelRemovalDialog";
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";

import { formatCheckedAt, statusLineFor } from "./appUpdateModel";
import { useAppUpdate } from "./useAppUpdate";

import { CustomDropdown } from "./CustomDropdown";
import { openEngineSetup } from "./engineSetupStore";
import { goToView, openReportIssueModal } from "./views";
import { useDialogFocus } from "./useDialogFocus";
import {
  AlertIcon,
  CloseIcon,
  DownloadIcon,
  FolderIcon,
  HelpCircleIcon,
  InfoIcon,
  MicIcon,
  SunIcon,
  TargetIcon,
  TrashIcon,
} from "./Icons";
import type { BookSummary } from "./libraryModel";
import {
  getRetentionSnapshot,
  loadRetention,
  setRetention,
  subscribeHistory,
  type RetentionValue,
} from "./sessionHistoryStore";
import {
  consentRowLabel,
  reverseAnchorHostOf,
  sortedConsents,
  type ReverseAnchorConsent,
} from "./reverseAnchorModel";
import {
  loadSpotlightEnabled,
  storeSpotlightEnabled,
} from "./teacherAnchorModel";
import {
  personalizeLocked,
  personalizeNoteOf,
  preferenceTopicLabel,
  type LearningPreference,
  type LearningPrivacy,
} from "./learningPrivacyModel";
import { THEME_PREFERENCES, type ThemePreference } from "./themeModel";
import {
  DIRECTION_PREFERENCES,
  loadDirectionPreference,
  type DirectionPreference,
} from "./directionModel";
import { setThemePreference } from "./themeStore";
import { useTheme } from "./useTheme";
import {
  LOCALE_PREFERENCES,
  useI18n,
  type LocalePreference,
  type MessageKey,
} from "./i18n";
import { numberLocale } from "./i18n/localeModel";
import {
  getSnapshot as localeSnapshot,
  setDirectionPreference,
  setLocalePreference,
  subscribe as subscribeLocale,
} from "./i18n/localeStore";
import {
  cacheUsageMessage,
  capGbInputFor,
  capNoticeOf,
  capRequest,
  dirStatusOf,
  formatBytes,
  parseCapGb,
  pathTail,
  type CapMode,
  type ModelStorageInfo,
  type StorageBreakdown,
  type StorageDirView,
  type StorageSettings,
} from "./storageSettingsModel";
import { GlideGroup } from "./GlideThumb";
import Switch from "./Switch";

function SettingRow({
  label,
  value,
  hint,
  testId,
}: {
  label: string;
  value: string;
  hint?: string;
  testId?: string;
}) {
  return (
    <div className="settings-row flex items-start justify-between gap-6">
      <div className="min-w-0">
        <p className="text-body font-medium text-ink-100">{label}</p>
        {hint && <p className="mt-1 text-secondary text-ink-500">{hint}</p>}
      </div>
      <span
        data-testid={testId}
        dir="ltr"
        className="ltr-island max-w-[55%] shrink-0 truncate font-mono text-secondary text-ink-400"
        title={value}
      >
        {pathTail(value)}
      </span>
    </div>
  );
}

function DirectoryRow({
  label,
  dir,
  hint,
  testId,
}: {
  label: string;
  dir: StorageDirView;
  hint: string;
  testId: string;
}) {
  return (
    <SettingRow
      label={label}
      value={dir.path}
      hint={`${hint} ${dirStatusOf(dir)}`}
      testId={testId}
    />
  );
}

function CacheLimitRow({
  settings,
  onSaved,
}: {
  settings: StorageSettings;
  onSaved: (next: StorageSettings) => void;
}) {
  const [mode, setMode] = useState<CapMode>(() => settings.cacheCapSource);
  const [gbInput, setGbInput] = useState(() => capGbInputFor(settings));
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const { t, tx } = useI18n();

  useEffect(() => {
    setMode(settings.cacheCapSource);
    setGbInput(capGbInputFor(settings));
    setError(null);
  }, [settings]);

  const save = useCallback(() => {
    let gb = 0;
    if (mode === "custom") {
      const parsed = parseCapGb(gbInput);
      if ("error" in parsed) {
        setError(parsed.error);
        return;
      }
      gb = parsed.gb;
    }
    setError(null);
    setSaving(true);
    invoke<StorageSettings>("set_cache_cap_bytes", {
      capBytes: capRequest(mode, gb),
    })
      .then(onSaved)
      .catch((reason) => setError(String(reason)))
      .finally(() => setSaving(false));
  }, [gbInput, mode, onSaved]);

  return (
    <div className="settings-row">
      <div className="flex items-start justify-between gap-6">
        <div className="min-w-0">
          <p className="text-body font-medium text-ink-100">
            {t("settings.cacheLimit")}
          </p>
          <p className="row-hint text-secondary text-ink-500">
            {t("settings.cacheLimitHint")}
          </p>
        </div>
        <span
          data-testid="cache-cap-effective"
          className="shrink-0 font-mono text-secondary text-ink-400"
        >
          {tx(
            cacheUsageMessage(settings).key,
            cacheUsageMessage(settings).params,
          )}
        </span>
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <CustomDropdown
          testId="cache-cap-mode"
          label={t("settings.cacheLimit")}
          value={mode}
          onChange={(val) => setMode(val as CapMode)}
          options={[
            { value: "default", label: t("settings.capDefault") },
            { value: "custom", label: t("settings.capCustom") },
            { value: "unlimited", label: t("settings.capUnlimited") },
          ]}
        />
        {mode === "custom" && (
          <label className="flex items-center gap-1.5 text-secondary text-ink-400">
            <input
              data-testid="cache-cap-gb"
              aria-label={t("settings.cacheLimitGb")}
              value={gbInput}
              inputMode="decimal"
              onChange={(event) => setGbInput(event.target.value)}
              className="w-20 rounded-lg border border-ink-800 bg-ink-900 px-2 py-1.5 text-end font-mono text-ink-200"
            />
            {t("settings.gb")}
          </label>
        )}
        <button
          type="button"
          data-testid="cache-cap-save"
          disabled={saving}
          onClick={save}
          className="rounded-lg border border-ink-700 bg-ink-800/70 px-3 py-1.5 text-secondary font-medium text-ink-100 transition-colors duration-200 hover:bg-ink-800 active:scale-[0.98] disabled:opacity-50"
        >
          {saving ? t("settings.saving") : t("settings.save")}
        </button>
      </div>
      {capNoticeOf(settings) && (
        <p
          data-testid="cache-cap-notice"
          className="mt-2 text-secondary text-wash-yellow-ink"
        >
          {capNoticeOf(settings)}
        </p>
      )}
      {error && (
        <p
          data-testid="cache-cap-error"
          className="mt-2 text-secondary text-wash-red-ink"
        >
          {error}
        </p>
      )}
    </div>
  );
}

const DIRECTION_KEYS: Record<DirectionPreference, MessageKey> = {
  system: "settings.directionSystem",
  ltr: "settings.directionLtr",
  rtl: "settings.directionRtl",
};

const THEME_KEYS: Record<ThemePreference, MessageKey> = {
  system: "settings.themeSystem",
  light: "settings.themeLight",
  dark: "settings.themeDark",
};

const LANGUAGE_KEYS: Record<LocalePreference, MessageKey> = {
  system: "settings.languageSystem",
  en: "settings.languageEnglish",
  ar: "settings.languageArabic",
};

// `label` is the stable testid suffix (on/off); the visible text is translated.

function AppearanceCard() {
  const theme = useTheme();
  const { t } = useI18n();
  const localeState = useSyncExternalStore(subscribeLocale, localeSnapshot);
  const [direction, setDirection] = useState<DirectionPreference>(
    loadDirectionPreference,
  );

  const onDirection = (preference: DirectionPreference) => {
    setDirectionPreference(preference);
    setDirection(preference);
  };
  const [spotlight, setSpotlight] = useState(loadSpotlightEnabled);

  const onSpotlight = (enabled: boolean) => {
    setSpotlight(enabled);
    storeSpotlightEnabled(enabled);
  };

  return (
    <div
      data-testid="appearance-settings"
      className="settings-group"
    >
      <div className="settings-group-head">
        <span className="settings-group-icon">
          <SunIcon className="h-5 w-5" />
        </span>
        <h2 className="text-label font-semibold tracking-[0.12em] text-ink-400 uppercase">
          {t("settings.appearance")}
        </h2>
      </div>
      <div className="settings-row flex items-start justify-between gap-6">
        <div className="min-w-0">
          <p className="text-body font-medium text-ink-100">
            {t("settings.theme")}
          </p>
          <p className="row-hint text-secondary text-ink-500">
            {t("settings.themeHint")}
          </p>
        </div>
        <GlideGroup
          thumbClassName="rounded-md bg-ink-50"
          role="radiogroup"
          aria-label={t("settings.theme")}
          className="flex shrink-0 gap-1 rounded-lg border border-ink-800 bg-ink-900 p-1"
        >
          {THEME_PREFERENCES.map((preference) => {
            const active = theme.preference === preference;
            return (
              <button
                key={preference}
                type="button"
                role="radio"
                aria-checked={active}
                data-testid={`theme-choice-${preference}`}
                onClick={() => setThemePreference(preference)}
                className={`relative rounded-md px-3 py-1.5 text-secondary font-medium transition-colors duration-200 ${
                  active
                    ? "text-ink-950"
                    : "text-ink-400 hover:bg-ink-800/70 hover:text-ink-200"
                }`}
              >
                {t(THEME_KEYS[preference])}
              </button>
            );
          })}
        </GlideGroup>
      </div>
      <div className="settings-row flex items-start justify-between gap-6">
        <div className="min-w-0">
          <p className="text-body font-medium text-ink-100">
            {t("settings.language")}
          </p>
          <p className="row-hint text-secondary text-ink-500">
            {t("settings.languageHint")}
          </p>
        </div>
        <GlideGroup
          thumbClassName="rounded-md bg-ink-50"
          role="radiogroup"
          aria-label={t("settings.language")}
          data-testid="language-settings"
          className="flex shrink-0 gap-1 rounded-lg border border-ink-800 bg-ink-900 p-1"
        >
          {LOCALE_PREFERENCES.map((preference) => {
            const active = localeState.preference === preference;
            return (
              <button
                key={preference}
                type="button"
                role="radio"
                aria-checked={active}
                lang={preference === "system" ? undefined : preference}
                data-testid={`language-choice-${preference}`}
                onClick={() => setLocalePreference(preference)}
                className={`relative rounded-md px-3 py-1.5 text-secondary font-medium transition-colors duration-200 ${
                  active
                    ? "text-ink-950"
                    : "text-ink-400 hover:bg-ink-800/70 hover:text-ink-200"
                }`}
              >
                {t(LANGUAGE_KEYS[preference])}
              </button>
            );
          })}
        </GlideGroup>
      </div>
      <div className="settings-row flex items-start justify-between gap-6">
        <div className="min-w-0">
          <p className="text-body font-medium text-ink-100">
            {t("settings.direction")}
          </p>
          <p className="row-hint text-secondary text-ink-500">
            {t("settings.directionHint")}
          </p>
        </div>
        <GlideGroup
          thumbClassName="rounded-md bg-ink-50"
          role="radiogroup"
          aria-label={t("settings.direction")}
          className="flex shrink-0 gap-1 rounded-lg border border-ink-800 bg-ink-900 p-1"
        >
          {DIRECTION_PREFERENCES.map((preference) => {
            const active = direction === preference;
            return (
              <button
                key={preference}
                type="button"
                role="radio"
                aria-checked={active}
                data-testid={`direction-choice-${preference}`}
                onClick={() => onDirection(preference)}
                className={`relative rounded-md px-3 py-1.5 text-secondary font-medium transition-colors duration-200 ${
                  active
                    ? "text-ink-950"
                    : "text-ink-400 hover:bg-ink-800/70 hover:text-ink-200"
                }`}
              >
                {t(DIRECTION_KEYS[preference])}
              </button>
            );
          })}
        </GlideGroup>
      </div>
      <div className="settings-row flex items-start justify-between gap-6">
        <div className="min-w-0">
          <p className="text-body font-medium text-ink-100">
            {t("settings.spotlight")}
          </p>
          <p className="row-hint text-secondary text-ink-500">
            {t("settings.spotlightHint")}
          </p>
        </div>
        <Switch
          checked={spotlight}
          onChange={onSpotlight}
          label={t("settings.spotlight")}
          testId="spotlight-switch"
        />
      </div>
    </div>
  );
}

const SUPPORT_URL = "https://www.patreon.com/c/txtcaster/membership";

// The D-2 privacy surface: whether the voice tutor's learning profile
// persists across sessions. Under never-persist the toggle renders disabled
// at its stored position with a note — memory-only always wins there, and a
// live-looking switch would promise a change that cannot happen. An
// unreadable backend keeps the card visible with the memory-only note: the
// backend falls to the safe direction in that state, and a privacy surface
// that silently vanishes tells the user nothing.
const RETENTION_CHOICES = [
  { value: "keep", key: "settings.retentionKeep" },
  { value: "never", key: "settings.retentionNever" },
] as const;

function LearningPrivacyCard() {
  const { t } = useI18n();
  const [privacy, setPrivacy] = useState<LearningPrivacy | null>(null);
  const [unreadable, setUnreadable] = useState(false);
  // F11-U6 §6.1 (D-8 ruled (c)): the transcript-retention choice. Read from
  // the shared store so the History surface's line and this row agree; the
  // runtime writes the explicit `keep` default on first read.
  const retention = useSyncExternalStore(
    subscribeHistory,
    getRetentionSnapshot,
    getRetentionSnapshot,
  );

  const readPrivacy = useCallback(() => {
    invoke<LearningPrivacy>("get_learning_privacy")
      .then(setPrivacy)
      .catch((reason) => {
        console.error(reason);
        setUnreadable(true);
      });
  }, []);

  useEffect(() => {
    readPrivacy();
    void loadRetention();
  }, [readPrivacy]);

  const onRetention = (value: RetentionValue) => {
    if (retention.status !== "ready" || retention.value === value) return;
    // Never locks Personalize from the next session on; re-read so the
    // toggle's locked state follows the stored value, not a local guess.
    setRetention(value).then(readPrivacy).catch(console.error);
  };

  if (privacy == null && !unreadable) return null;

  const locked = privacy == null || personalizeLocked(privacy);
  const note =
    privacy == null
      ? t("settings.privacyUnreadableNote")
      : personalizeNoteOf(privacy);

  const onChoice = (enabled: boolean) => {
    if (locked || privacy == null || enabled === privacy.personalizeEnabled)
      return;
    invoke<LearningPrivacy>("set_personalize_learning", { enabled })
      .then(setPrivacy)
      .catch(console.error);
  };

  return (
    <div
      data-testid="learning-privacy-settings"
      className="settings-group"
    >
      <div className="settings-group-head">
        <span className="settings-group-icon">
          <MicIcon className="h-5 w-5" />
        </span>
        <h2 className="text-label font-semibold tracking-[0.12em] text-ink-400 uppercase">
          {t("settings.voiceTutor")}
        </h2>
      </div>
      {retention.status === "ready" && retention.value != null && (
        <div
          className="settings-row"
          data-testid="retention-settings"
        >
          <div className="flex items-start justify-between gap-6">
            <div className="min-w-0">
              <p className="text-body font-medium text-ink-100">
                {t("settings.retentionLabel")}
              </p>
              <p className="row-hint text-secondary text-ink-500">
                {t("settings.retentionHint")}
              </p>
            </div>
            <GlideGroup
              thumbClassName="rounded-md bg-ink-50"
              role="radiogroup"
              aria-label={t("settings.retentionLabel")}
              className="flex shrink-0 gap-1 rounded-lg border border-ink-800 bg-ink-900 p-1"
            >
              {RETENTION_CHOICES.map(({ value, key }) => {
                const active = retention.value === value;
                return (
                  <button
                    key={value}
                    type="button"
                    role="radio"
                    aria-checked={active}
                    data-testid={`retention-choice-${value}`}
                    onClick={() => onRetention(value)}
                    className={`relative rounded-md px-3 py-1.5 text-secondary font-medium transition-colors duration-200 ${
                      active
                        ? "text-ink-950"
                        : "text-ink-400 hover:bg-ink-800/70 hover:text-ink-200"
                    }`}
                  >
                    {t(key)}
                  </button>
                );
              })}
            </GlideGroup>
          </div>
        </div>
      )}
      <div className="settings-row">
        <div className="flex items-start justify-between gap-6">
          <div className="min-w-0">
            <p className="text-body font-medium text-ink-100">
              {t("settings.personalizeLabel")}
            </p>
            <p className="row-hint text-secondary text-ink-500">
              {t("settings.personalizeHint")}
            </p>
          </div>
          {privacy != null && (
            <Switch
              checked={privacy.personalizeEnabled}
              onChange={onChoice}
              label={t("settings.personalizeLabel")}
              disabled={locked}
              testId="personalize-switch"
            />
          )}
        </div>
        {note && (
          <p
            data-testid="personalize-note"
            className="mt-2 text-secondary text-wash-yellow-ink"
          >
            {note}
          </p>
        )}
      </div>
      <LearnerMemoryRow />
    </div>
  );
}

function LearnerMemoryRow() {
  const { t } = useI18n();
  const [entries, setEntries] = useState<LearningPreference[] | null>(null);

  useEffect(() => {
    invoke<LearningPreference[]>("list_learning_preferences")
      .then(setEntries)
      .catch(console.error);
  }, []);

  const forget = (key: string) => {
    invoke("forget_learning_preference", { key })
      .then(() =>
        setEntries((current) =>
          current == null
            ? current
            : current.filter((entry) => entry.key !== key),
        ),
      )
      .catch(console.error);
  };

  if (entries == null) return null;

  return (
    <div className="settings-row" data-testid="learner-memory">
      <p className="text-body font-medium text-ink-100">
        {t("settings.memoryLabel")}
      </p>
      <p className="row-hint text-secondary text-ink-500">
        {t("settings.memoryHint")}
      </p>
      {entries.length === 0 ? (
        <p
          data-testid="learner-memory-empty"
          className="mt-3 text-secondary text-ink-500"
        >
          {t("settings.memoryEmpty")}
        </p>
      ) : (
        <ul className="mt-3 space-y-3">
          {entries.map((entry) => {
            const topic = preferenceTopicLabel(entry.key);
            return (
              <li
                key={entry.key}
                data-testid="learner-memory-entry"
                className="flex items-start justify-between gap-6"
              >
                <div className="min-w-0">
                  <p className="text-secondary text-ink-500">{topic}</p>
                  <p dir="auto" className="text-body break-words text-ink-100">
                    {entry.value}
                  </p>
                </div>
                <button
                  type="button"
                  data-testid="learner-memory-forget"
                  aria-label={t("settings.memoryForgetLabel", { topic })}
                  onClick={() => forget(entry.key)}
                  className="shrink-0 rounded-lg border border-ink-800 px-3 py-1.5 text-secondary text-ink-400 transition-colors duration-200 hover:border-wash-red-ink/60 hover:text-wash-red-ink"
                >
                  {t("settings.memoryForget")}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

// The §11.1 revocation surface: every recorded per-book reverse-anchor
// consent, revocable here. Recording happens at first tap during a live
// tutor session; this card only lists and revokes.
function ReverseAnchorCard() {
  const { t } = useI18n();
  const [consents, setConsents] = useState<ReverseAnchorConsent[]>([]);
  const [titles, setTitles] = useState<Map<string, string>>(new Map());

  useEffect(() => {
    invoke<ReverseAnchorConsent[]>("list_reverse_anchor_consents")
      .then(setConsents)
      .catch(console.error);
    invoke<BookSummary[]>("list_books")
      .then((books) =>
        setTitles(new Map(books.map((book) => [book.id, book.title]))),
      )
      .catch(console.error);
  }, []);

  const revoke = (consent: ReverseAnchorConsent) => {
    invoke("revoke_reverse_anchor_consent", {
      bookId: consent.bookId,
      url: consent.baseUrl,
    })
      .then(() =>
        setConsents((known) =>
          known.filter(
            (entry) =>
              entry.bookId !== consent.bookId ||
              entry.baseUrl !== consent.baseUrl,
          ),
        ),
      )
      .catch(console.error);
  };

  if (consents.length === 0) return null;

  return (
    <div
      data-testid="reverse-anchor-settings"
      className="settings-group"
    >
      <div className="settings-group-head">
        <span className="settings-group-icon">
          <TargetIcon className="h-5 w-5" />
        </span>
        <h2 className="text-label font-semibold tracking-[0.12em] text-ink-400 uppercase">
          {t("settings.tutorPosition")}
        </h2>
      </div>
      <p className="settings-group-hint text-secondary text-ink-500">
        {t("settings.tutorPositionHint")}
      </p>
      {sortedConsents(consents, titles).map((consent) => (
        <div
          key={`${consent.bookId}\u241F${consent.baseUrl}`}
          className="settings-row flex items-start justify-between gap-6"
        >
          <div className="min-w-0">
            <p className="truncate text-body font-medium text-ink-100">
              {consentRowLabel(consent, titles)}
            </p>
            <p className="mt-1 font-mono text-secondary text-ink-500">
              {reverseAnchorHostOf(consent.baseUrl)}
            </p>
          </div>
          <button
            type="button"
            data-testid="reverse-anchor-revoke"
            onClick={() => revoke(consent)}
            className="shrink-0 rounded-lg border border-ink-800 px-3 py-1.5 text-secondary text-ink-400 transition-colors duration-200 hover:border-wash-red-ink/60 hover:text-wash-red-ink"
          >
            {t("settings.revoke")}
          </button>
        </div>
      ))}
    </div>
  );
}

function UpdatesCard() {
  const { t, locale } = useI18n();
  const { snapshot, checkNow } = useAppUpdate();
  const line = statusLineFor(snapshot);
  const text =
    line.kind === "unavailable"
      ? t("settings.updatesUnavailable")
      : line.kind === "checking"
        ? t("settings.checking")
        : line.kind === "never"
          ? t("settings.neverChecked")
          : line.kind === "offline"
            ? t("settings.updatesOffline")
            : t("settings.lastChecked", {
                time: formatCheckedAt(line.at, numberLocale(locale)),
              });
  const detail =
    line.kind !== "checked"
      ? null
      : line.detail === "up_to_date"
        ? t("settings.upToDate")
        : line.detail === "offline"
          ? t("settings.updatesOffline")
          : line.detail === "available" && line.version
            ? t("settings.updateAvailable", { version: line.version })
            : null;
  return (
    <div
      data-testid="update-settings"
      className="settings-group"
    >
      <div className="settings-group-head">
        <span className="settings-group-icon">
          <DownloadIcon className="h-5 w-5" />
        </span>
        <h2 className="text-label font-semibold tracking-[0.12em] text-ink-400 uppercase">
          {t("settings.updates")}
        </h2>
      </div>
      <p className="settings-group-hint text-secondary text-ink-500">
        {t("settings.updatesHint")}
      </p>
      <div className="settings-row flex items-center justify-between gap-6">
        <div className="min-w-0">
          <p
            data-testid="update-status-line"
            className="text-body text-ink-100"
          >
            {text}
          </p>
          {detail && (
            <p
              data-testid="update-status-detail"
              className="mt-1 text-secondary text-ink-500"
            >
              {detail}
            </p>
          )}
        </div>
        {line.kind !== "unavailable" && (
          <button
            type="button"
            data-testid="update-check-now"
            disabled={line.kind === "checking"}
            onClick={checkNow}
            className="shrink-0 rounded-lg border border-ink-800 px-3 py-1.5 text-secondary text-ink-400 hover:text-ink-100 disabled:opacity-50"
          >
            {t("settings.checkNow")}
          </button>
        )}
      </div>
    </div>
  );
}

function Settings({
  onOpenReportIssue,
}: {
  onOpenReportIssue?: () => void;
} = {}) {
  const { t } = useI18n();
  const [version, setVersion] = useState("");
  const [storage, setStorage] = useState<StorageSettings | null>(null);
  const [breakdown, setBreakdown] = useState<StorageBreakdown | null>(null);
  const [isUninstallingEngine, setIsUninstallingEngine] = useState(false);
  const [isPurgingCache, setIsPurgingCache] = useState(false);
  const [deletingModelId, setDeletingModelId] = useState<string | null>(null);
  const [modelPendingDelete, setModelPendingDelete] = useState<ModelStorageInfo | null>(null);
  const [isUninstallEngineModalOpen, setIsUninstallEngineModalOpen] = useState(false);
  const [purgeModalOpen, setPurgeModalOpen] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  const loadSettings = useCallback(() => {
    setLoadError(null);
    invoke<string>("app_version").then(setVersion).catch((err) => setLoadError(String(err)));
    invoke<StorageSettings>("get_storage_settings")
      .then(setStorage)
      .catch((err) => setLoadError(String(err)));
    invoke<StorageBreakdown>("get_storage_breakdown")
      .then(setBreakdown)
      .catch((err) => console.error("Failed to load storage breakdown", err));
  }, []);

  const handleUninstallEngine = async () => {
    setIsUninstallingEngine(true);
    try {
      await invoke("uninstall_engine");
      const nextBreakdown = await invoke<StorageBreakdown>("get_storage_breakdown");
      setBreakdown(nextBreakdown);
    } catch (err) {
      console.error("Failed to uninstall engine", err);
    } finally {
      setIsUninstallingEngine(false);
    }
  };

  const handleConfirmUninstallEngine = async () => {
    await handleUninstallEngine();
    setIsUninstallEngineModalOpen(false);
  };

  const handlePurgeCache = async () => {
    setIsPurgingCache(true);
    try {
      await invoke<number>("purge_audio_cache");
      const nextBreakdown = await invoke<StorageBreakdown>("get_storage_breakdown");
      setBreakdown(nextBreakdown);
      const nextStorage = await invoke<StorageSettings>("get_storage_settings");
      setStorage(nextStorage);
    } catch (err) {
      console.error("Failed to purge audio cache", err);
    } finally {
      setIsPurgingCache(false);
    }
  };

  const handleDeleteModel = async (modelId: string) => {
    setDeletingModelId(modelId);
    try {
      await invoke("delete_model", { modelId });
      const nextBreakdown = await invoke<StorageBreakdown>("get_storage_breakdown");
      setBreakdown(nextBreakdown);
      const nextStorage = await invoke<StorageSettings>("get_storage_settings");
      setStorage(nextStorage);
    } catch (err) {
      console.error("Failed to delete model", err);
    } finally {
      setDeletingModelId(null);
    }
  };

  const handleConfirmDeleteModel = async () => {
    if (!modelPendingDelete) return;
    const modelId = modelPendingDelete.modelId;
    await handleDeleteModel(modelId);
    setModelPendingDelete(null);
  };

  useEffect(() => {
    loadSettings();
  }, [loadSettings]);

  return (
    <section
      data-testid="settings-panel"
      className="flex w-full flex-col gap-8"
    >
      <header className="flex flex-col gap-1.5">
        <h2 className="font-serif text-title font-semibold tracking-tight text-ink-50">
          {t("nav.general")}
        </h2>
        <p className="max-w-xl text-body text-ink-400">{t("settings.intro")}</p>
      </header>

      {loadError && (
        <div
          role="alert"
          className="flex items-center justify-between gap-4 rounded-xl border border-wash-red-ink/20 bg-wash-red px-5 py-4 text-body text-wash-red-ink"
        >
          <span>{loadError}</span>
          <button
            type="button"
            onClick={loadSettings}
            className="rounded border border-wash-red-ink/30 px-3 py-1 text-secondary font-medium hover:bg-wash-red-ink/10"
          >
            {t("session.retry")}
          </button>
        </div>
      )}

      <AppearanceCard />

      <LearningPrivacyCard />

      <ReverseAnchorCard />

      {loadError == null && storage == null && (
        <div
          aria-hidden="true"
          data-testid="settings-skeleton"
          className="settings-group"
        >
          <div className="mb-6 flex items-center gap-2.5">
            <div className="shimmer-bar h-8 w-8 rounded-lg bg-ink-800/70" />
            <div className="shimmer-bar h-4 w-24 rounded bg-ink-800/70" />
          </div>
          {[0, 1, 2].map((row) => (
            <div
              key={row}
              className="settings-row flex items-start justify-between gap-6"
            >
              <div className="flex flex-col gap-2">
                <div className="shimmer-bar h-4 w-28 rounded bg-ink-800/70" />
                <div className="shimmer-bar h-3 w-56 rounded bg-ink-800/50" />
              </div>
              <div className="shimmer-bar h-3 w-32 rounded bg-ink-800/50" />
            </div>
          ))}
        </div>
      )}
      {storage && (
        <div
          data-testid="storage-settings"
          className="settings-group"
        >
          <div className="settings-group-head justify-between">
            <div className="flex items-center gap-2.5">
              <span className="settings-group-icon">
                <FolderIcon className="h-5 w-5" />
              </span>
              <h2 className="text-label font-semibold tracking-[0.12em] text-ink-400 uppercase">
                {t("settings.storage")}
              </h2>
            </div>
            {breakdown && (
              <span className="font-mono text-secondary text-ink-400">
                {t("settings.storageTotal")}: {formatBytes(breakdown.totalBytes)}
              </span>
            )}
          </div>
          <DirectoryRow
            label={t("settings.models")}
            dir={storage.models}
            hint={t("settings.modelsHint")}
            testId="storage-models-dir"
          />
          <DirectoryRow
            label={t("settings.exportedAudio")}
            dir={storage.exports}
            hint={t("settings.exportedAudioHint")}
            testId="storage-exports-dir"
          />
          <SettingRow
            label={t("settings.cache")}
            value={storage.cacheDir}
            hint={t("settings.cacheHint")}
            testId="storage-cache-dir"
          />
          <CacheLimitRow settings={storage} onSaved={setStorage} />

          {breakdown && (
            <div className="mt-6 border-t border-ink-800/60 pt-5">
              <div className="mb-3 flex items-center justify-between">
                <h3 className="text-caption font-semibold tracking-wide text-ink-300 uppercase">
                  {t("settings.storageBreakdown")}
                </h3>
              </div>

              {/* Engine row */}
              {(() => {
                const isEngineInstalled =
                  breakdown.engine.status === "installed" ||
                  breakdown.engine.status === "ready" ||
                  breakdown.engine.diskBytes > 0;
                return (
                  <div
                    data-testid="storage-breakdown-engine"
                    className="flex items-center justify-between gap-4 border-b border-ink-800/40 py-3"
                  >
                    <div className="min-w-0">
                      <p className="text-body font-medium text-ink-100">{t("settings.engineStorage")}</p>
                      <p className="text-secondary text-ink-500">
                        {isEngineInstalled
                          ? t("settings.engineStorageInstalled", { size: formatBytes(breakdown.engine.diskBytes) })
                          : t("settings.engineStorageNotInstalled")}
                      </p>
                    </div>
                    <button
                      type="button"
                      data-testid="engine-setup-button"
                      onClick={() =>
                        openEngineSetup(
                          isEngineInstalled ? "localAi" : "documents",
                          () => {
                            invoke<StorageBreakdown>("get_storage_breakdown")
                              .then(setBreakdown)
                              .catch(console.error);
                          },
                        )
                      }
                      className="shrink-0 rounded-lg border border-ink-700 px-3 py-1.5 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-850"
                    >
                      {isEngineInstalled
                        ? t("engine.setup.settingsChange")
                        : t("engine.setup.settingsMissing")}
                    </button>
                    {isEngineInstalled && (
                      <button
                        type="button"
                        data-testid="uninstall-engine-button"
                        disabled={isUninstallingEngine}
                        onClick={() => setIsUninstallEngineModalOpen(true)}
                        className="shrink-0 rounded-lg border border-red-900/50 bg-red-950/20 px-3 py-1.5 text-secondary font-medium text-red-300 transition-colors hover:bg-red-950/40 disabled:opacity-50"
                      >
                        {isUninstallingEngine ? t("settings.uninstallingEngine") : t("settings.uninstallEngine")}
                      </button>
                    )}
                  </div>
                );
              })()}

              {/* Cache row */}
              <div
                data-testid="storage-breakdown-cache"
                className="flex items-center justify-between gap-4 border-b border-ink-800/40 py-3"
              >
                <div className="min-w-0">
                  <p className="text-body font-medium text-ink-100">{t("settings.cacheStorage")}</p>
                  <p className="text-secondary text-ink-500">
                    {formatBytes(breakdown.cache.diskBytes)}
                    {breakdown.cache.activeJobLocks > 0 && (
                      <span> {t("settings.activeLocksNote", { count: String(breakdown.cache.activeJobLocks) })}</span>
                    )}
                  </p>
                </div>
                <button
                  type="button"
                  data-testid="purge-cache-button"
                  disabled={
                    isPurgingCache ||
                    breakdown.cache.diskBytes === 0 ||
                    breakdown.cache.activeJobLocks > 0
                  }
                  onClick={handlePurgeCache}
                  className="shrink-0 rounded-lg border border-ink-700 px-3 py-1.5 text-secondary font-medium text-ink-200 transition-colors hover:border-ink-600 disabled:opacity-50"
                >
                  {isPurgingCache ? t("settings.purgingCache") : t("settings.purgeCache")}
                </button>
              </div>

              {/* Books & Exports */}
              <div
                data-testid="storage-breakdown-books"
                className="flex items-center justify-between gap-4 border-b border-ink-800/40 py-3"
              >
                <div className="min-w-0">
                  <p className="text-body font-medium text-ink-100">{t("settings.booksStorage")}</p>
                  <p className="text-secondary text-ink-500">
                    {t("settings.booksStorageValue", {
                      count: String(breakdown.books.count),
                      size: formatBytes(breakdown.books.diskBytes),
                    })}
                  </p>
                </div>
              </div>

              <div
                data-testid="storage-breakdown-exports"
                className="flex items-center justify-between gap-4 border-b border-ink-800/40 py-3"
              >
                <div className="min-w-0">
                  <p className="text-body font-medium text-ink-100">{t("settings.exportsStorage")}</p>
                  <p className="text-secondary text-ink-500">
                    {t("settings.exportsStorageValue", {
                      count: String(breakdown.exports.count),
                      size: formatBytes(breakdown.exports.diskBytes),
                    })}
                  </p>
                </div>
              </div>

              {/* Models list */}
              <div data-testid="storage-breakdown-models" className="py-3">
                <p className="mb-2 text-body font-medium text-ink-100">{t("settings.installedModels")}</p>
                {breakdown.models.length === 0 ? (
                  <p className="text-secondary text-ink-500">{t("settings.noModelsInstalled")}</p>
                ) : (
                  <div className="flex flex-col gap-2">
                    {breakdown.models.map((m) => (
                      <div
                        key={m.modelId}
                        className="flex items-center justify-between rounded-lg border border-ink-800/40 bg-ink-950/40 px-3 py-2"
                      >
                        <div className="min-w-0">
                          <p className="truncate text-secondary font-medium text-ink-200">{m.name || m.modelId}</p>
                          <p className="text-caption text-ink-500">{formatBytes(m.diskBytes)} · {m.task}</p>
                        </div>
                        <button
                          type="button"
                          data-testid={`delete-model-${m.modelId}`}
                          disabled={deletingModelId === m.modelId}
                          onClick={() => setModelPendingDelete(m)}
                          className="shrink-0 rounded p-1.5 text-ink-500 transition-colors hover:text-red-400 disabled:opacity-50"
                          title={t("settings.deleteModel")}
                        >
                          <TrashIcon className="h-4 w-4" />
                        </button>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      )}

      {/* S-5: exactly ONE key surface exists — Settings → Providers.
          Stored accounts, slots, labels, vault state, and consents are the
          same VA3 data; nothing migrated because nothing moved (A28). */}
      <div
        data-testid="providers-pointer-card"
        className="flex items-center justify-between gap-4 rounded-xl border border-ink-800/80 bg-ink-900 p-6"
      >
        <div className="min-w-0">
          <h2 className="text-label font-semibold tracking-[0.12em] text-ink-400 uppercase">
            {t("settings.providersTitle")}
          </h2>
          <p className="mt-1 text-secondary text-ink-400">
            {t("settings.providersHint")}
          </p>
        </div>
        <button
          type="button"
          data-testid="open-providers"
          onClick={() => goToView("providers")}
          className="shrink-0 rounded-lg border border-ink-700 px-3.5 py-2 text-secondary font-medium text-ink-200 transition-colors hover:border-ink-600"
        >
          {t("settings.openProviders")}
        </button>
      </div>

      <UpdatesCard />

      <div
        data-testid="report-issue-settings-card"
        className="flex items-center justify-between gap-4 rounded-xl border border-ink-800/80 bg-ink-900 p-6"
      >
        <div className="min-w-0">
          <div className="mb-1 flex items-center gap-2">
            <span className="flex h-7 w-7 items-center justify-center rounded-lg bg-ink-800 text-ink-300">
              <HelpCircleIcon className="h-4 w-4" />
            </span>
            <h2 className="text-label font-semibold tracking-[0.12em] text-ink-400 uppercase">
              {t("settings.reportIssue")}
            </h2>
          </div>
          <p className="text-secondary text-ink-400">
            {t("settings.reportIssueHint")}
          </p>
        </div>
        <button
          type="button"
          data-testid="open-report-issue"
          onClick={onOpenReportIssue ?? openReportIssueModal}
          className="shrink-0 rounded-lg border border-ink-700 px-3.5 py-2 text-secondary font-medium text-ink-200 transition-colors hover:border-ink-600 hover:text-ink-100"
        >
          {t("settings.openReportIssue")}
        </button>
      </div>

      <div className="settings-group">
        <div className="settings-group-head">
          <span className="settings-group-icon">
            <InfoIcon className="h-5 w-5" />
          </span>
          <h2 className="text-label font-semibold tracking-[0.12em] text-ink-400 uppercase">
            {t("settings.about")}
          </h2>
        </div>
        <SettingRow
          label={t("settings.version")}
          value={version ? t("settings.versionValue", { version }) : "—"}
          hint={t("settings.versionHint")}
        />
        <div className="settings-row flex items-start justify-between gap-6">
          <div className="min-w-0">
            <p className="text-body font-medium text-ink-100">
              {t("settings.support")}
            </p>
            <p className="mt-1 text-secondary text-ink-500">
              {t("settings.supportHint")}
            </p>
          </div>
          <button
            type="button"
            data-testid="settings-support"
            onClick={() => void openUrl(SUPPORT_URL).catch(console.error)}
            className="shrink-0 rounded-lg border border-ink-700 px-3.5 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-800"
          >
            {t("settings.supportOpen")}
          </button>
        </div>
        <SettingRow
          label={t("settings.privacy")}
          value={t("settings.privacyValue")}
          hint={t("settings.privacyHint")}
        />
        <SettingRow
          label={t("settings.license")}
          value="Proprietary"
          hint={t("settings.licenseHint")}
        />
      </div>

      <DangerZoneCard onOpenPurgeModal={() => setPurgeModalOpen(true)} />

      <p className="text-center text-secondary text-ink-500">
        {t("settings.moreControls")}
      </p>

      <UninstallConfirmModal
        open={purgeModalOpen}
        onClose={() => setPurgeModalOpen(false)}
      />

      <ModelRemovalDialog
        model={modelPendingDelete}
        isDeleting={deletingModelId !== null && deletingModelId === modelPendingDelete?.modelId}
        onClose={() => setModelPendingDelete(null)}
        onConfirm={handleConfirmDeleteModel}
      />

      <UninstallEngineConfirmModal
        open={isUninstallEngineModalOpen}
        isUninstalling={isUninstallingEngine}
        engineBytes={breakdown?.engine.diskBytes ?? 0}
        onClose={() => setIsUninstallEngineModalOpen(false)}
        onConfirm={handleConfirmUninstallEngine}
      />
    </section>
  );
}

function DangerZoneCard({ onOpenPurgeModal }: { onOpenPurgeModal: () => void }) {
  const { t } = useI18n();
  return (
    <div
      data-testid="danger-zone"
      className="rounded-xl border border-red-900/60 bg-red-950/15 p-6"
    >
      <div className="mb-3 flex items-center gap-2.5">
        <span className="flex h-9 w-9 items-center justify-center rounded-lg bg-red-900/40 text-red-300">
          <AlertIcon className="h-5 w-5" />
        </span>
        <h2 className="text-body font-semibold tracking-wide text-red-300 uppercase">
          {t("settings.dangerZone")}
        </h2>
      </div>
      <div className="flex flex-col justify-between gap-4 sm:flex-row sm:items-center">
        <div className="min-w-0">
          <p className="text-body font-medium text-ink-100">
            {t("settings.dangerZoneTitle")}
          </p>
          <p className="mt-1 text-secondary text-ink-400">
            {t("settings.dangerZoneHint")}
          </p>
        </div>
        <button
          type="button"
          data-testid="open-danger-purge"
          onClick={onOpenPurgeModal}
          className="shrink-0 rounded-lg border border-red-700/60 bg-red-950/40 px-3.5 py-2 text-secondary font-medium text-red-300 transition-colors hover:bg-red-900/50 hover:text-red-100"
        >
          {t("settings.uninstallApp")}
        </button>
      </div>
    </div>
  );
}

function UninstallConfirmModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const { t } = useI18n();
  const [confirmText, setConfirmText] = useState("");
  const [isPurging, setIsPurging] = useState(false);
  const [purgeError, setPurgeError] = useState<string | null>(null);

  useDialogFocus(dialogRef, open, {
    onEscape: () => {
      if (!isPurging) onClose();
    },
  });

  useEffect(() => {
    if (!open) {
      setConfirmText("");
      setPurgeError(null);
      return;
    }
  }, [open]);

  if (!open) return null;

  const isConfirmed = confirmText.trim() === "UNINSTALL";

  const handleExecute = async () => {
    if (!isConfirmed || isPurging) return;
    setIsPurging(true);
    setPurgeError(null);
    try {
      await invoke("danger_self_purge", { confirmation: confirmText.trim() });
    } catch (err) {
      setPurgeError(String(err));
      setIsPurging(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      data-testid="uninstall-modal"
      data-modal-layer=""
    >
      <button
        type="button"
        aria-label={t("settings.cancel")}
        onClick={isPurging ? undefined : onClose}
        className="animate-fade-in absolute inset-0 cursor-default bg-ink-950/85 backdrop-blur-sm"
      />
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("settings.uninstallModalTitle")}
        tabIndex={-1}
        className="animate-modal-in shadow-overlay relative flex w-full max-w-lg flex-col overflow-hidden rounded-2xl border border-red-800/80 bg-ink-900 p-6 outline-none"
      >
        <div className="flex items-start justify-between gap-4">
          <div className="flex items-center gap-3">
            <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-red-900/30 text-red-400">
              <AlertIcon className="h-6 w-6" />
            </span>
            <div className="min-w-0">
              <h2 className="font-serif text-title font-semibold tracking-tight text-ink-50">
                {t("settings.uninstallModalTitle")}
              </h2>
              <p className="mt-0.5 text-secondary text-red-400">
                {t("settings.uninstallModalSubtitle")}
              </p>
            </div>
          </div>
          <button
            type="button"
            aria-label={t("settings.cancel")}
            data-testid="uninstall-modal-close"
            disabled={isPurging}
            onClick={onClose}
            className="rounded-lg p-1 text-ink-500 transition-colors hover:bg-ink-850 hover:text-ink-200 disabled:opacity-50"
          >
            <CloseIcon className="h-5 w-5" />
          </button>
        </div>

        <div className="mt-4 rounded-xl border border-red-900/40 bg-red-950/20 p-4 text-secondary text-ink-300">
          <p>{t("settings.uninstallModalWarning")}</p>
        </div>

        <div className="mt-4 flex flex-col gap-2">
          <label
            htmlFor="uninstall-confirm-input"
            className="text-secondary font-medium text-ink-200"
          >
            {t("settings.uninstallConfirmPrompt", { keyword: "UNINSTALL" })}
          </label>
          <input
            id="uninstall-confirm-input"
            type="text"
            data-testid="uninstall-confirm-input"
            disabled={isPurging}
            value={confirmText}
            onChange={(e) => setConfirmText(e.target.value)}
            placeholder={t("settings.uninstallConfirmPlaceholder")}
            className="rounded-lg border border-ink-700 bg-ink-950 px-3.5 py-2 font-mono text-body text-ink-100 placeholder:text-ink-500 focus:border-red-500 focus:outline-none"
          />
        </div>

        {purgeError && (
          <p className="mt-3 text-secondary font-medium text-red-400">
            {purgeError}
          </p>
        )}

        <div className="mt-6 flex items-center justify-end gap-3">
          <button
            type="button"
            disabled={isPurging}
            onClick={onClose}
            className="rounded-lg border border-ink-700 px-4 py-2 text-secondary font-medium text-ink-200 transition-colors hover:border-ink-600 disabled:opacity-50"
          >
            {t("settings.cancel")}
          </button>
          <button
            type="button"
            data-testid="confirm-danger-purge"
            disabled={!isConfirmed || isPurging}
            onClick={handleExecute}
            className="rounded-lg bg-red-600 px-4 py-2 text-secondary font-medium text-white transition-colors hover:bg-red-500 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {isPurging ? t("settings.uninstalling") : t("settings.uninstallExecute")}
          </button>
        </div>
      </div>
    </div>
  );
}

function UninstallEngineConfirmModal({
  open,
  isUninstalling,
  engineBytes,
  onClose,
  onConfirm,
}: {
  open: boolean;
  isUninstalling: boolean;
  engineBytes: number;
  onClose: () => void;
  onConfirm: () => void;
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const { t } = useI18n();

  useDialogFocus(dialogRef, open, {
    onEscape: () => {
      if (!isUninstalling) onClose();
    },
  });

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      data-testid="uninstall-engine-modal"
      data-modal-layer=""
    >
      <button
        type="button"
        aria-label={t("settings.cancel")}
        onClick={isUninstalling ? undefined : onClose}
        className="animate-fade-in absolute inset-0 cursor-default bg-ink-950/85 backdrop-blur-sm"
      />
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("settings.uninstallEngineModalTitle")}
        tabIndex={-1}
        className="animate-modal-in shadow-overlay relative flex w-full max-w-lg flex-col overflow-hidden rounded-2xl border border-red-800/80 bg-ink-900 p-6 outline-none"
      >
        <div className="flex items-start justify-between gap-4">
          <div className="flex items-center gap-3">
            <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-red-900/30 text-red-400">
              <AlertIcon className="h-6 w-6" />
            </span>
            <div className="min-w-0">
              <h2 className="font-serif text-title font-semibold tracking-tight text-ink-50">
                {t("settings.uninstallEngineModalTitle")}
              </h2>
            </div>
          </div>
          <button
            type="button"
            aria-label={t("settings.cancel")}
            data-testid="uninstall-engine-modal-close"
            disabled={isUninstalling}
            onClick={onClose}
            className="rounded-lg p-1 text-ink-500 transition-colors hover:bg-ink-850 hover:text-ink-200 disabled:opacity-50"
          >
            <CloseIcon className="h-5 w-5" />
          </button>
        </div>

        <div className="mt-4 rounded-xl border border-red-900/40 bg-red-950/20 p-4 text-secondary text-ink-300">
          <p>
            {t("settings.uninstallEngineModalWarning", {
              size: formatBytes(engineBytes),
            })}
          </p>
        </div>

        <div className="mt-6 flex items-center justify-end gap-3">
          <button
            type="button"
            data-testid="cancel-uninstall-engine-button"
            disabled={isUninstalling}
            onClick={onClose}
            className="rounded-lg border border-ink-700 px-4 py-2 text-secondary font-medium text-ink-200 transition-colors hover:border-ink-600 disabled:opacity-50"
          >
            {t("settings.uninstallEngineCancel")}
          </button>
          <button
            type="button"
            data-testid="confirm-uninstall-engine-button"
            disabled={isUninstalling}
            onClick={onConfirm}
            className="rounded-lg bg-red-600 px-4 py-2 text-secondary font-medium text-white transition-colors hover:bg-red-500 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {isUninstalling ? t("settings.uninstallingEngine") : t("settings.uninstallEngineConfirm")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default Settings;
