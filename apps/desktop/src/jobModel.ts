import { t, type MessageKey } from "./i18n";
import { storeTextForReader } from "./i18n/storeText";
import type { Scope } from "./jobFormModel";
import { reportAdvisoryNotice } from "./languageAdvisoryModel";

export type JobState =
  | "CREATED"
  | "QUEUED"
  | "INGESTING"
  | "LAYOUT"
  | "VISION"
  | "SCRIPTING"
  | "SYNTHESIZING"
  | "ALIGNING"
  | "RENDERING"
  | "DONE"
  | "PAUSED"
  | "CANCELLED"
  | "FAILED";

export interface JobSummary {
  id: string;
  bookId: string;
  scope: Scope;
  mode: string;
  voiceProfileId: string;
  outputFormat: string;
  state: JobState;
  checkpointJson?: string | null;
  overnight: boolean;
  etaSeconds: number | null;
  reportJson: string | null;
  outputPathsJson: string | null;
  createdAt: number;
  updatedAt: number;
}

export interface JobProgress {
  job_id: string;
  stage: JobState;
  pct: number;
  eta_seconds: number | null;
  current_item: string;
}

export interface JobStateUpdate {
  job_id: string;
  state: JobState;
  notice?: string;
}

export interface StageNotice {
  text: string;
  state: JobState;
}

export type StageNotices = Record<string, StageNotice>;

export function applyStateNotice(
  current: StageNotices,
  update: JobStateUpdate,
): StageNotices {
  const held = current[update.job_id];
  if (update.notice) {
    if (held && held.text === update.notice && held.state === update.state) {
      return current;
    }
    return {
      ...current,
      [update.job_id]: { text: update.notice, state: update.state },
    };
  }
  if (!held || held.state === update.state) return current;
  const next = { ...current };
  delete next[update.job_id];
  return next;
}

export const STAGE_LABELS = [
  "job.stage.ingest",
  "job.stage.layout",
  "job.stage.vision",
  "job.stage.script",
  "models.group.tts",
  "job.stage.sync",
  "job.stage.render",
] as const satisfies readonly MessageKey[];

export function stageIndex(state: JobState): number {
  switch (state) {
    case "INGESTING":
      return 0;
    case "LAYOUT":
      return 1;
    case "VISION":
      return 2;
    case "SCRIPTING":
      return 3;
    case "SYNTHESIZING":
      return 4;
    case "ALIGNING":
      return 5;
    case "RENDERING":
      return 6;
    case "DONE":
      return STAGE_LABELS.length;
    default:
      return -1;
  }
}

export type StepStatus = "done" | "active" | "pending";

const VALID_JOB_STATES: ReadonlySet<string> = new Set<JobState>([
  "CREATED",
  "QUEUED",
  "INGESTING",
  "LAYOUT",
  "VISION",
  "SCRIPTING",
  "SYNTHESIZING",
  "ALIGNING",
  "RENDERING",
  "DONE",
  "PAUSED",
  "CANCELLED",
  "FAILED",
]);

export function parseCheckpointStage(
  checkpointJson?: string | null,
): JobState | null {
  if (!checkpointJson) return null;
  try {
    const parsed: unknown = JSON.parse(checkpointJson);
    if (parsed && typeof parsed === "object") {
      const stage = (parsed as Record<string, unknown>).stage;
      if (typeof stage === "string" && VALID_JOB_STATES.has(stage)) {
        return stage as JobState;
      }
    }
  } catch {
    // ignore
  }
  return null;
}

export function railStage(job: JobSummary): JobState {
  return job.state === "PAUSED"
    ? (parseCheckpointStage(job.checkpointJson) ?? "QUEUED")
    : job.state;
}

export function stepStatus(step: number, state: JobState): StepStatus {
  const active = stageIndex(state);
  if (state === "DONE") return "done";
  if (step < active) return "done";
  if (step === active) return "active";
  return "pending";
}

export function isTerminal(state: JobState): boolean {
  return state === "DONE" || state === "CANCELLED" || state === "FAILED";
}

export function isRunning(state: JobState): boolean {
  return !isTerminal(state) && state !== "PAUSED" && state !== "QUEUED";
}

export function canDeleteJob(state: JobState): boolean {
  return isTerminal(state) || state === "PAUSED";
}

export function parseOutputPaths(raw: string | null): string[] {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (Array.isArray(parsed)) {
      return parsed.filter((item): item is string => typeof item === "string");
    }
  } catch {
    return [];
  }
  return [];
}

export function sanitizeAndRouteErrorMessage(raw: string): string {
  if (!raw) return t("job.failedGeneric");

  const lower = raw.toLowerCase();

  if (
    raw.includes("ERR_PROVIDER_KEY") ||
    lower.includes("no usable key") ||
    lower.includes("api key") ||
    lower.includes("configured key") ||
    lower.includes("keys configured") ||
    lower.includes("invalid key") ||
    lower.includes("unauthorized") ||
    lower.includes("forbidden") ||
    (lower.includes("key") && lower.includes("invalid")) ||
    (lower.includes("key") && lower.includes("cooldown"))
  ) {
    return t("job.provider.keyRejected");
  }

  if (
    raw.includes("ERR_PROVIDER_QUOTA") ||
    lower.includes("quota") ||
    lower.includes("rate-limited") ||
    lower.includes("rate limit") ||
    lower.includes("429") ||
    lower.includes("resource_exhausted") ||
    lower.includes("resource has been exhausted")
  ) {
    return t("job.provider.quota");
  }

  if (
    raw.includes("ERR_PROVIDER_TIMEOUT") ||
    lower.includes("timed out") ||
    lower.includes("timeout") ||
    lower.includes("deadline exceeded")
  ) {
    return t("job.provider.timeout");
  }

  if (
    raw.includes("ERR_PROVIDER_TRANSPORT") ||
    lower.includes("unreachable") ||
    lower.includes("connection refused") ||
    lower.includes("network error") ||
    lower.includes("failed to connect")
  ) {
    return t("job.provider.unreachable");
  }

  if (
    raw.includes("ERR_PROVIDER_MODEL") ||
    lower.includes("model not found") ||
    lower.includes("unknown model") ||
    lower.includes("does not exist")
  ) {
    return t("job.provider.unknownModel");
  }

  if (
    raw.includes("ERR_PROVIDER_MALFORMED") ||
    lower.includes("malformed") ||
    lower.includes("cannot serve the tts phase")
  ) {
    return t("job.provider.badShape");
  }

  if (
    raw.includes("ERR_PROVIDER_") ||
    lower.includes("generativelanguage.googleapis.com") ||
    lower.includes("api.openai.com") ||
    lower.includes("provider_staging")
  ) {
    return t("job.provider.error");
  }

  return storeTextForReader(raw);
}

export function failureMessage(reportJson: string | null): string {
  if (!reportJson) return t("job.failedGeneric");
  try {
    const parsed: unknown = JSON.parse(reportJson);
    if (parsed && typeof parsed === "object") {
      const record = parsed as Record<string, unknown>;
      if (
        record.provider_failure &&
        typeof record.provider_failure === "object"
      ) {
        const failure = providerFailure(reportJson);
        if (failure) {
          return providerFailureCopy(failure);
        }
      }
      if (typeof record.error === "string") {
        return sanitizeAndRouteErrorMessage(record.error);
      }
      if (typeof record.message === "string") {
        return sanitizeAndRouteErrorMessage(record.message);
      }
    }
  } catch {
    return sanitizeAndRouteErrorMessage(reportJson);
  }
  return sanitizeAndRouteErrorMessage(reportJson);
}

const QUALITY_FLAG_LABELS: Record<string, MessageKey> = {
  fallback_narrator: "job.flag.fallbackNarrator",
  code_block: "job.flag.codeBlock",
};

export function qualityNotices(reportJson: string | null): string[] {
  if (!reportJson) return [];
  let record: Record<string, unknown>;
  try {
    const parsed: unknown = JSON.parse(reportJson);
    if (!parsed || typeof parsed !== "object") return [];
    record = parsed as Record<string, unknown>;
  } catch {
    return [];
  }
  const total =
    typeof record.segment_count === "number" ? record.segment_count : 0;
  const notices: string[] = [];
  const counts = record.flag_counts;
  if (counts && typeof counts === "object") {
    for (const [flag, label] of Object.entries(QUALITY_FLAG_LABELS)) {
      const raw = (counts as Record<string, unknown>)[flag];
      if (typeof raw !== "number" || raw <= 0) continue;
      notices.push(
        total > 0
          ? t("job.flagOfTotal", { count: raw, total, label: t(label) })
          : t("job.flagCount", { count: raw, label: t(label) }),
      );
    }
  }
  const dropped = record.figure_markers_dropped;
  if (typeof dropped === "number" && dropped > 0) {
    notices.push(t("job.figureMarkersDropped", { count: dropped }));
  }
  notices.push(...languageAdvisoryNotices(record.language_advisory));
  notices.push(...verifyNotices(record.verify));
  return notices;
}

function languageAdvisoryNotices(advisory: unknown): string[] {
  if (!advisory || typeof advisory !== "object") return [];
  const record = advisory as Record<string, unknown>;
  if (typeof record.book !== "string") return [];
  const voiceSpeaks =
    typeof record.voice_speaks === "string" ? record.voice_speaks : null;
  return [reportAdvisoryNotice(record.book, voiceSpeaks)];
}

function verifyNotices(verify: unknown): string[] {
  if (!verify || typeof verify !== "object") return [];
  const record = verify as Record<string, unknown>;
  if (record.enabled !== true) return [];
  const counts = record.counts;
  if (!counts || typeof counts !== "object") return [];
  const read = (key: string): number => {
    const raw = (counts as Record<string, unknown>)[key];
    return typeof raw === "number" && raw > 0 ? raw : 0;
  };
  const failed = read("failed");
  const missing = read("missing");
  const unscoreable = read("unscoreable_ref");
  const unchecked = read("unchecked");
  const threshold =
    typeof record.threshold === "number" ? record.threshold : 0.15;
  const notices: string[] = [];
  if (unchecked > 0) {
    const scored = read("ok") + failed + missing + unscoreable;
    notices.push(
      t("job.accuracyUnchecked", {
        count: unchecked,
        total: unchecked + scored,
      }),
    );
  }
  if (failed > 0) {
    notices.push(
      t("job.accuracyDiff", {
        count: failed,
        pct: Math.round(threshold * 100),
      }),
    );
  }
  if (missing > 0) {
    notices.push(t("job.accuracyHeardNothing", { count: missing }));
  }
  if (unscoreable > 0) {
    notices.push(t("job.accuracyNoWords", { count: unscoreable }));
  }
  return notices;
}

function readReport(reportJson: string | null): Record<string, unknown> | null {
  if (!reportJson) return null;
  try {
    const parsed: unknown = JSON.parse(reportJson);
    if (!parsed || typeof parsed !== "object") return null;
    return parsed as Record<string, unknown>;
  } catch {
    return null;
  }
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function asCount(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0
    ? value
    : 0;
}

export function executionNotices(
  reportJson: string | null,
  state: JobState,
): string[] {
  const record = readReport(reportJson);
  if (!record) return [];
  const notices: string[] = [];

  const revisions = asCount(record.plan_revisions);
  if (revisions > 0) {
    const log = Array.isArray(record.recovery_log) ? record.recovery_log : [];
    const last = asRecord(log[log.length - 1]);
    const action = typeof last?.action === "string" ? last.action : null;
    notices.push(
      action
        ? t("job.reducedHardwareAction", { count: revisions, action })
        : t("job.reducedHardware", { count: revisions }),
    );
  }

  const contention = asRecord(record.contention);
  if (!contention) return notices;

  const waited = asCount(contention.waited_s);
  const retries = asCount(contention.retries);
  const paused = state === "PAUSED" ? asRecord(contention.paused) : null;

  if (paused) {
    const stage =
      typeof paused.stage === "string"
        ? stageLabel(paused.stage as JobState)
        : null;
    const attempts = asCount(paused.resume_attempts);
    const cap = asCount(paused.max_pauses);
    const attempt =
      cap > 0 ? t("job.pauseOf", { attempts, cap }) : String(attempts);
    notices.push(
      t("job.pausedContention", {
        stage: stage ?? t("job.thisJob"),
        waited,
        count: retries,
        attempt,
      }),
    );
    return notices;
  }

  if (retries > 0) {
    notices.push(
      `Waited ${waited}s for the engine over ${retries} ${
        retries === 1 ? "retry" : "retries"
      }.`,
    );
  }
  return notices;
}

export function coversHeldNotice(
  execution: string[],
  held: string | undefined,
): boolean {
  if (!held) return false;
  // The pause notice is catalog text, so the recognisable prefix is whatever
  // the current locale puts before the stage name: render it with a sentinel
  // in the stage slot and keep the part before it.
  const prefix = t("job.pausedContention", {
    stage: "\u0000",
    waited: 0,
    count: 2,
    attempt: "",
  }).split("\u0000")[0]!;
  return execution.some((line) => line.startsWith(prefix));
}

const PIPELINE_STATES = new Set<JobState>([
  "INGESTING",
  "LAYOUT",
  "VISION",
  "SCRIPTING",
  "SYNTHESIZING",
  "ALIGNING",
  "RENDERING",
]);

export function stageLabel(state: JobState): string | null {
  const index = stageIndex(state);
  if (index < 0 || index >= STAGE_LABELS.length) return null;
  return t(STAGE_LABELS[index]!);
}

export function failedStage(
  reportJson: string | null,
  fallbackStage: JobState | null,
): JobState | null {
  if (reportJson) {
    try {
      const parsed: unknown = JSON.parse(reportJson);
      if (parsed && typeof parsed === "object") {
        const record = parsed as Record<string, unknown>;
        const raw = record.failed_stage ?? record.stage;
        if (typeof raw === "string") {
          const candidate = raw.toUpperCase() as JobState;
          if (PIPELINE_STATES.has(candidate)) return candidate;
        }
      }
    } catch {
      return fallbackStage && PIPELINE_STATES.has(fallbackStage)
        ? fallbackStage
        : null;
    }
  }
  if (fallbackStage && PIPELINE_STATES.has(fallbackStage)) {
    return fallbackStage;
  }
  return null;
}

export function missingModelFromFailure(message: string): string | null {
  const match = /model ['"]?([\w.-]+)['"]? is not installed/i.exec(message);
  return match ? match[1]! : null;
}

export function stageEtaLabel(etaSeconds: number | null): string {
  if (etaSeconds == null || etaSeconds < 0) return "";
  const minutes = Math.round(etaSeconds / 60);
  if (minutes < 1) return t("job.etaUnderMinute");
  return t("job.etaMinutes", { n: minutes });
}

export function activeStageProgress(
  progress: JobProgress | null,
  state: JobState,
): JobProgress | null {
  if (!progress) return null;
  if (!PIPELINE_STATES.has(state)) return progress;
  return progress.stage === state ? progress : null;
}

export function progressBarPercent(progress: JobProgress | null): number {
  if (!progress) return 0;
  if (progress.stage === "DONE") return 100;
  return Math.min(100, Math.max(0, progress.pct * 100));
}

export const OVERNIGHT_QUEUE_NOTICE = () => t("job.queuedOvernight");
export const WAITING_QUEUE_NOTICE = () => t("job.waitingQueue");
export const STARTING_QUEUE_NOTICE = () => t("job.waitingStart");

export function queuedNotice(
  job: JobSummary,
  jobs: readonly JobSummary[],
): string | null {
  if (job.state !== "QUEUED") return null;
  if (job.overnight) return OVERNIGHT_QUEUE_NOTICE();
  const busy = jobs.some(
    (other) => other.id !== job.id && isRunning(other.state),
  );
  return busy ? WAITING_QUEUE_NOTICE() : STARTING_QUEUE_NOTICE();
}

export function jobBadges(job: JobSummary): string[] {
  const badges = [job.mode, job.outputFormat];
  if (job.overnight) badges.push(t("job.overnightBadge"));
  return badges;
}

// Typed provider-failure projection (Hermes MF10 → the S-4 recovery
// surface): what failed, whether retrying the SAME binding is sensible,
// and how long the provider asked us to wait.
export interface ProviderFailure {
  class: string;
  retryable: boolean;
  retryAfterSecs: number | null;
  detail: string;
}

export function providerFailure(
  reportJson: string | null,
): ProviderFailure | null {
  const report = readReport(reportJson);
  const raw = report?.provider_failure;
  if (raw == null || typeof raw !== "object") return null;
  const record = raw as Record<string, unknown>;
  if (typeof record.class !== "string") return null;
  return {
    class: record.class,
    retryable: record.retryable === true,
    retryAfterSecs:
      typeof record.retry_after_secs === "number"
        ? record.retry_after_secs
        : null,
    detail: typeof record.detail === "string" ? record.detail : "",
  };
}

export function providerFailureCopy(failure: ProviderFailure): string {
  switch (failure.class) {
    case "auth":
      return t("job.provider.keyRejected");
    case "quota":
      return failure.retryAfterSecs != null
        ? t("job.provider.rateLimited", { n: failure.retryAfterSecs })
        : t("job.provider.quota");
    case "transport":
      return t("job.provider.unreachable");
    case "timeout":
      return t("job.provider.timeout");
    case "model_not_found":
      return t("job.provider.unknownModel");
    case "malformed":
      return t("job.provider.badShape");
    default:
      return t("job.provider.error");
  }
}

// The visible recovery history (§5.5: recovery is reportable, not
// write-only): read from the FINAL report's provider-owned keys.
export interface RecoveryEvent {
  action: string;
  planRev: number;
}

export function recoveryHistory(reportJson: string | null): RecoveryEvent[] {
  const report = readReport(reportJson);
  const raw = report?.provider_recovery_log;
  if (!Array.isArray(raw)) return [];
  return raw.flatMap((entry) => {
    if (entry == null || typeof entry !== "object") return [];
    const record = entry as Record<string, unknown>;
    if (typeof record.action !== "string") return [];
    return [
      {
        action: record.action,
        planRev: typeof record.plan_rev === "number" ? record.plan_rev : 0,
      },
    ];
  });
}

export function recoveryLine(events: RecoveryEvent[]): string | null {
  if (events.length === 0) return null;
  const last = events[events.length - 1]!;
  const what =
    last.action === "switch_to_local"
      ? t("job.recoveredLocal")
      : t("job.recoveredSame");
  const attempt =
    events.length > 1 ? t("job.attempt", { n: events.length }) : "";
  return t("job.recovered", { what, attempt });
}

// F11-U6 (spec §3 J7, §7.4 D8–D10): the delete-cast gate. The runtime's
// `job_delete_preview` says how many tutor sessions the FK cascade would take
// and whether one of them is live; this decides what the affordance does. The
// component only renders the decision. `null` = the preview has not answered
// yet: the affordance waits rather than deleting on a guess.
export interface JobDeletePreview {
  session_count: number;
  live: boolean;
}

export type DeleteGate =
  | { kind: "pending" }
  | { kind: "immediate"; title: string }
  | {
      kind: "confirm";
      title: string;
      question: string;
      confirmLabel: string;
      cancelLabel: string;
    }
  | { kind: "refused"; title: string };

export function deleteGateOf(preview: JobDeletePreview | null): DeleteGate {
  if (preview == null) return { kind: "pending" };
  if (preview.live) return { kind: "refused", title: t("job.deleteLiveTitle") };
  if (preview.session_count <= 0)
    return { kind: "immediate", title: t("job.deleteTitle") };
  const count = preview.session_count;
  return {
    kind: "confirm",
    title: t("job.deleteTitle"),
    question: t("job.deleteQuestion", { count }),
    confirmLabel: t("job.deleteConfirm", { count }),
    cancelLabel: t("job.deleteKeep"),
  };
}

// F11-U6 queue order: QUEUED casts run in creation order — the order the
// worker restores after a restart (`resume_on_launch` reads `render_job`
// ORDER BY created_at). Position 0 = the next cast to start; `null` for any
// cast that is not waiting in the queue. Ties on `createdAt` fall back to id
// so two rows never claim the same position.
// The queue key, shared with the store: `render_job … ORDER BY created_at, id`
// (`storage/job.rs list_jobs_in_states`, which both `list_jobs` and the relaunch
// read). Ties on the millisecond stamp are broken by id on both sides, so the
// panel can never show an order the worker will not restore.
export function compareByQueueKey(a: JobSummary, b: JobSummary): number {
  if (a.createdAt !== b.createdAt) return a.createdAt - b.createdAt;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

/** The panel's order: newest first — the queue key reversed. */
export function castsLayout(ordered: readonly JobSummary[]): {
  now: JobSummary[];
  history: { job: JobSummary; openByDefault: boolean }[];
} {
  const now = ordered.filter((job) => !isTerminal(job.state));
  let newestDoneSeen = false;
  const history = ordered
    .filter((job) => isTerminal(job.state))
    .map((job) => {
      if (job.state !== "DONE") return { job, openByDefault: true };
      const openByDefault = !newestDoneSeen;
      newestDoneSeen = true;
      return { job, openByDefault };
    });
  return { now, history };
}

export function displayOrder(jobs: readonly JobSummary[]): JobSummary[] {
  return [...jobs].sort((a, b) => compareByQueueKey(b, a));
}

export function queuePosition(
  job: JobSummary,
  jobs: readonly JobSummary[],
): number | null {
  if (job.state !== "QUEUED") return null;
  return jobs.filter(
    (other) =>
      other.id !== job.id &&
      other.state === "QUEUED" &&
      compareByQueueKey(other, job) < 0,
  ).length;
}

export function queuePositionLabel(position: number | null): string | null {
  if (position == null) return null;
  if (position === 0) return t("job.queueNext");
  return t("job.queueAhead", { count: position });
}

export type RateLimitClass = "per_minute" | "daily";

export interface PausedReason {
  kind: string;
  class: RateLimitClass;
  retry_at_epoch_ms?: number | null;
  provider: string;
  segment_idx?: number;
  last_error_message?: string;
  attempt?: number;
}

function validatePausedReason(val: unknown): PausedReason | null {
  if (!val || typeof val !== "object") return null;
  const r = val as Record<string, unknown>;
  if (typeof r.kind !== "string" || typeof r.provider !== "string") return null;
  if (r.class !== "per_minute" && r.class !== "daily") return null;
  return {
    kind: r.kind,
    class: r.class,
    retry_at_epoch_ms:
      typeof r.retry_at_epoch_ms === "number" ? r.retry_at_epoch_ms : null,
    provider: r.provider,
    segment_idx: typeof r.segment_idx === "number" ? r.segment_idx : 0,
    last_error_message:
      typeof r.last_error_message === "string" ? r.last_error_message : "",
    attempt: typeof r.attempt === "number" ? r.attempt : 1,
  };
}

export function parsePausedReason(job: JobSummary): PausedReason | null {
  if (job.checkpointJson) {
    try {
      const parsed: unknown = JSON.parse(job.checkpointJson);
      if (parsed && typeof parsed === "object") {
        const reason = (parsed as Record<string, unknown>).paused_reason;
        const valid = validatePausedReason(reason);
        if (valid) return valid;
      }
    } catch {
      // ignore
    }
  }
  if (job.reportJson) {
    try {
      const parsed: unknown = JSON.parse(job.reportJson);
      if (parsed && typeof parsed === "object") {
        const reason = (parsed as Record<string, unknown>).paused_reason;
        const valid = validatePausedReason(reason);
        if (valid) return valid;
      }
    } catch {
      // ignore
    }
  }
  return null;
}

export function formatCountdown(remainingMs: number): string {
  if (!Number.isFinite(remainingMs) || remainingMs <= 0) {
    return "00:00";
  }
  const totalSecs = Math.ceil(remainingMs / 1000);
  const mins = Math.floor(totalSecs / 60);
  const secs = totalSecs % 60;
  return `${String(mins).padStart(2, "0")}:${String(secs).padStart(2, "0")}`;
}

export function checkpointProgress(
  job: JobSummary,
): { done: number; total: number } | null {
  if (job.checkpointJson) {
    try {
      const parsed: unknown = JSON.parse(job.checkpointJson);
      if (parsed && typeof parsed === "object") {
        const record = parsed as Record<string, unknown>;
        const done =
          typeof record.done_segments === "number"
            ? record.done_segments
            : null;
        const total =
          typeof record.total_segments === "number"
            ? record.total_segments
            : null;
        if (done != null && total != null && total > 0) {
          return { done, total };
        }
      }
    } catch {
      // ignore
    }
  }
  if (job.reportJson) {
    try {
      const parsed: unknown = JSON.parse(job.reportJson);
      if (parsed && typeof parsed === "object") {
        const record = parsed as Record<string, unknown>;
        const done =
          typeof record.done_segments === "number"
            ? record.done_segments
            : null;
        const total =
          typeof record.total_segments === "number"
            ? record.total_segments
            : typeof record.segment_count === "number"
              ? record.segment_count
              : null;
        if (done != null && total != null && total > 0) {
          return { done, total };
        }
      }
    } catch {
      // ignore
    }
  }
  return null;
}
