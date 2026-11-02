import { invoke } from "@tauri-apps/api/core";

import { F11_STRINGS } from "./f11Strings";
import { t } from "./i18n";
import { numberLocale } from "./i18n/localeModel";
import { getSnapshot } from "./i18n/localeStore";
import { commandErrorText } from "./commandError";

export interface SessionPosition {
  kind: "ms" | "offset" | "none" | string;
  value: number | null;
}

export const END_REASONS = [
  "ended",
  "unexpected",
  "expired",
  "connection_closed",
] as const;

export type EndReason = (typeof END_REASONS)[number];

export interface SessionLocation {
  chapter: number;
  page: number | null;
}

export interface SessionSummary {
  id: string;
  book_id: string | null;
  job_id: string | null;
  started_at: number;
  ended_at: number;
  end_reason: EndReason | string;
  duration_ms: number;
  turn_count: number;
  summary_excerpt: string | null;
  position: SessionPosition;
  chapters_touched?: number[];
  location?: SessionLocation | null;
  job_resolves: boolean;
  provider_changed: boolean;
}

export interface TranscriptTurn {
  turn_idx: number;
  role: "user" | "assistant" | string;
  text: string;
  created_at: number;
  voiceprint?: number[];
}

export interface SessionDetail {
  row: SessionSummary;
  turns: TranscriptTurn[];
  summary_text?: string | null;
}

export interface HistoryScope {
  bookId: string | null;
}

export type HistoryStatus = "idle" | "loading" | "ready" | "error";

export interface HistoryScopeState {
  status: HistoryStatus;
  rows: SessionSummary[];
  error: string | null;
}

export type DetailStatus = "loading" | "ready" | "error";

export interface DetailState {
  status: DetailStatus;
  detail: SessionDetail | null;
  error: string | null;
}

export const HISTORY_PREVIEW_ROWS = 5;

export const INITIAL_HISTORY_SCOPE: HistoryScopeState = {
  status: "idle",
  rows: [],
  error: null,
};

export function scopeKey(scope: HistoryScope): string {
  return scope.bookId == null ? "standalone" : `book:${scope.bookId}`;
}

export function visibleRows(
  rows: SessionSummary[],
  showAll: boolean,
): SessionSummary[] {
  return showAll ? rows : rows.slice(0, HISTORY_PREVIEW_ROWS);
}

export function showAllAvailable(rows: SessionSummary[]): boolean {
  return rows.length > HISTORY_PREVIEW_ROWS;
}

export function historyVisible(
  status: "idle" | "starting" | "live" | "renewing" | "ended",
  starting: boolean,
): boolean {
  if (starting) return false;
  return status === "idle" || status === "ended";
}

// Resolved at CALL time (round-2 F3): a module-level table captured the English
// getters at import, before initLocale ran — History labels froze in EN on an
// Arabic cold start and ignored a mid-session switch.
export function endReasonLineOf(reason: EndReason): string | null {
  switch (reason) {
    case "ended":
      return null;
    case "unexpected":
      return F11_STRINGS.reasonUnexpected;
    case "expired":
      return F11_STRINGS.reasonExpired;
    case "connection_closed":
      return F11_STRINGS.reasonConnectionClosed;
  }
}

function isEndReason(value: string): value is EndReason {
  return (END_REASONS as readonly string[]).includes(value);
}

export function endReasonLine(row: SessionSummary): string | null {
  return isEndReason(row.end_reason) ? endReasonLineOf(row.end_reason) : null;
}

export type OpenTarget =
  | { kind: "ms"; ms: number; notice: null }
  | { kind: "page"; page: number; notice: null }
  | { kind: "none"; notice: string | null };

export function openTargetOf(
  row: SessionSummary,
  readerJobId: string | null,
): OpenTarget {
  if (row.job_id == null) {
    const page = row.location?.page;
    return page != null && page > 0
      ? { kind: "page", page, notice: null }
      : { kind: "none", notice: null };
  }
  if (readerJobId == null || row.job_id !== readerJobId) {
    return { kind: "none", notice: F11_STRINGS.h9Notice };
  }
  if (row.position.kind !== "ms" || row.position.value == null) {
    return { kind: "none", notice: null };
  }
  return { kind: "ms", ms: Math.max(0, row.position.value), notice: null };
}

// I18N-2: dates follow the UI locale (Arabic month/weekday names, Latin digits
// per the `{x|n}` decision: `ar-u-nu-latn`); durations go through plural forms.
function dateLocale(): string {
  const locale = getSnapshot().locale;
  return locale === "en" ? "en-GB" : numberLocale(locale);
}

function dayParts(
  epochMs: number,
  timeZone: string | undefined,
): { year: number; month: number; day: number } {
  const parts = new Intl.DateTimeFormat(dateLocale(), {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).formatToParts(new Date(epochMs));
  const read = (type: string): number =>
    Number(parts.find((part) => part.type === type)?.value ?? "0");
  return { year: read("year"), month: read("month"), day: read("day") };
}

function dayOrdinal(parts: {
  year: number;
  month: number;
  day: number;
}): number {
  return Math.floor(
    Date.UTC(parts.year, parts.month - 1, parts.day) / 86_400_000,
  );
}

export function formatSessionDate(
  startedAtMs: number,
  nowMs: number,
  timeZone?: string,
): string {
  const started = dayParts(startedAtMs, timeZone);
  const today = dayParts(nowMs, timeZone);
  const distance = dayOrdinal(today) - dayOrdinal(started);
  const time = new Intl.DateTimeFormat(dateLocale(), {
    timeZone,
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).format(new Date(startedAtMs));
  if (distance === 0)
    return t("session.dayAt", { day: F11_STRINGS.today, time });
  if (distance === 1)
    return t("session.dayAt", { day: F11_STRINGS.yesterday, time });
  const sameYear = started.year === today.year;
  return new Intl.DateTimeFormat(dateLocale(), {
    timeZone,
    weekday: "short",
    day: "numeric",
    month: "short",
    ...(sameYear ? {} : { year: "numeric" }),
  }).format(new Date(startedAtMs));
}

export function formatDuration(durationMs: number): string {
  const minutes = Math.round(Math.max(0, durationMs) / 60_000);
  if (minutes < 1) return F11_STRINGS.underAMinute;
  if (minutes < 60) return t("session.durationMinutes", { count: minutes });
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  const hoursText = t("session.durationHours", { count: hours });
  return rest === 0
    ? hoursText
    : t("session.durationHoursMinutes", {
        hours: hoursText,
        minutes: t("session.durationMinutes", { count: rest }),
      });
}

export function sortedTurns(turns: TranscriptTurn[]): TranscriptTurn[] {
  return [...turns].sort((a, b) => a.turn_idx - b.turn_idx);
}

export function chaptersLabel(row: SessionSummary): string | null {
  const chapters = row.chapters_touched;
  if (chapters == null || chapters.length === 0) return null;
  const sorted = [...new Set(chapters)].sort((a, b) => a - b);
  const first = sorted[0];
  const last = sorted[sorted.length - 1];
  if (sorted.length === 1) return F11_STRINGS.chapterSingle(first);
  const contiguous = last - first === sorted.length - 1;
  return contiguous
    ? F11_STRINGS.chapterRange(first, last)
    : F11_STRINGS.chapterList(sorted);
}

const scopes = new Map<string, HistoryScopeState>();
const scopeTokens = new Map<string, number>();
const details = new Map<string, DetailState>();
const detailTokens = new Map<string, number>();
const listeners = new Set<() => void>();
let nextToken = 1;

function emit(): void {
  for (const listener of listeners) listener();
}

export function subscribeHistory(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function getHistorySnapshot(scope: HistoryScope): HistoryScopeState {
  return scopes.get(scopeKey(scope)) ?? INITIAL_HISTORY_SCOPE;
}

export function getDetailSnapshot(sessionId: string): DetailState | null {
  return details.get(sessionId) ?? null;
}

function setScope(key: string, next: HistoryScopeState): void {
  scopes.set(key, next);
  emit();
}

export function loadHistory(scope: HistoryScope): Promise<void> {
  const key = scopeKey(scope);
  const token = nextToken++;
  scopeTokens.set(key, token);
  const current = scopes.get(key) ?? INITIAL_HISTORY_SCOPE;
  setScope(key, { ...current, status: "loading", error: null });
  return invoke<SessionSummary[]>("voice_session_list", {
    bookId: scope.bookId,
  })
    .then((rows) => {
      if (scopeTokens.get(key) !== token) return;
      setScope(key, { status: "ready", rows, error: null });
    })
    .catch((error: unknown) => {
      if (scopeTokens.get(key) !== token) return;
      const stale = scopes.get(key) ?? INITIAL_HISTORY_SCOPE;
      setScope(key, { ...stale, status: "error", error: commandErrorText(error) });
    });
}

export function ensureHistory(scope: HistoryScope): void {
  if (getHistorySnapshot(scope).status === "idle") void loadHistory(scope);
}

export function invalidateHistory(scope: HistoryScope): void {
  const key = scopeKey(scope);
  scopeTokens.set(key, nextToken++);
  const current = scopes.get(key);
  if (current == null) return;
  setScope(key, { ...current, status: "idle" });
}

export function loadDetail(sessionId: string): Promise<void> {
  const token = nextToken++;
  detailTokens.set(sessionId, token);
  details.set(sessionId, { status: "loading", detail: null, error: null });
  emit();
  return invoke<SessionDetail>("voice_session_transcript", { sessionId })
    .then((detail) => {
      if (detailTokens.get(sessionId) !== token) return;
      details.set(sessionId, { status: "ready", detail, error: null });
      emit();
    })
    .catch((error: unknown) => {
      if (detailTokens.get(sessionId) !== token) return;
      details.set(sessionId, {
        status: "error",
        detail: null,
        error: commandErrorText(error),
      });
      emit();
    });
}

export function deleteSession(
  scope: HistoryScope,
  sessionId: string,
): Promise<void> {
  return invoke<void>("voice_session_delete", { sessionId }).then(() => {
    const key = scopeKey(scope);
    const current = scopes.get(key) ?? INITIAL_HISTORY_SCOPE;
    details.delete(sessionId);
    detailTokens.delete(sessionId);
    setScope(key, {
      ...current,
      rows: current.rows.filter((row) => row.id !== sessionId),
    });
    return loadHistory(scope);
  });
}

export function forgetScope(scope: HistoryScope): Promise<number> {
  return invoke<number>("voice_session_forget_scope", {
    bookId: scope.bookId,
  }).then((count) => {
    const key = scopeKey(scope);
    for (const row of scopes.get(key)?.rows ?? []) details.delete(row.id);
    setScope(key, { status: "ready", rows: [], error: null });
    void loadHistory(scope);
    return count;
  });
}

export function resetSessionHistoryStore(): void {
  scopes.clear();
  scopeTokens.clear();
  details.clear();
  detailTokens.clear();
  nextToken = 1;
  emit();
}

// F11-U6 retention (spec §6.1/§6.2, D-8 ruled (c)): the app setting
// `voice.transcript_retention` as the History surface and Settings read it.
// One store so the rail's H12/H13 line and the Settings radiogroup agree.
// "unreadable" = the runtime could not answer; the surface then makes no
// claim (no empty line, no retention line) — the privacy card's pattern.
export type RetentionValue = "keep" | "never";

export interface RetentionState {
  status: "idle" | "loading" | "ready" | "error";
  value: RetentionValue | null;
}

export const INITIAL_RETENTION: RetentionState = {
  status: "idle",
  value: null,
};

let retention: RetentionState = INITIAL_RETENTION;
let retentionToken = 0;

export function getRetentionSnapshot(): RetentionState {
  return retention;
}

function setRetentionState(next: RetentionState): void {
  retention = next;
  emit();
}

export function loadRetention(): Promise<void> {
  const token = ++retentionToken;
  setRetentionState({ ...retention, status: "loading" });
  return invoke<RetentionValue>("voice_retention_get")
    .then((value) => {
      if (retentionToken !== token) return;
      setRetentionState({ status: "ready", value });
    })
    .catch(() => {
      if (retentionToken !== token) return;
      setRetentionState({ status: "error", value: null });
    });
}

export function ensureRetention(): void {
  if (retention.status === "idle") void loadRetention();
}

export function setRetention(value: RetentionValue): Promise<void> {
  const token = ++retentionToken;
  return invoke<RetentionValue>("voice_retention_set", { value }).then(
    (stored) => {
      if (retentionToken !== token) return;
      setRetentionState({ status: "ready", value: stored });
    },
  );
}

/** Test seam: reset the retention store between cases. */
export function resetRetentionStore(): void {
  retentionToken += 1;
  retention = INITIAL_RETENTION;
}

// §6.2 table: what the list's foot says when the surface is ready.
// "empty"     → Keep and no rows: the existing empty line
// "retention" → Never: the retention line (alone, or beneath the rows — H13)
// "none"      → rows under Keep, or retention unreadable (no claim is made)
export type HistoryFootLine = "empty" | "retention" | "none";

export function historyFootLine(
  state: HistoryScopeState,
  retention: RetentionState,
): HistoryFootLine {
  if (state.status !== "ready") return "none";
  if (retention.status === "ready" && retention.value === "never")
    return "retention";
  if (retention.status === "error") return "none";
  return state.rows.length === 0 ? "empty" : "none";
}
