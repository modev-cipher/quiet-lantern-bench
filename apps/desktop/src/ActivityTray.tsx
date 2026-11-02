import { t as tStatic, useI18n } from "./i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { AudioIcon, ChevronIcon, CloseIcon } from "./Icons";
import { presentStatus, type EngineStatus } from "./engineStatus";
import {
  STAGE_LABELS,
  formatCountdown,
  isRunning,
  isTerminal,
  parsePausedReason,
  progressBarPercent,
  stageIndex,
  type JobProgress,
  type JobState,
  type JobSummary,
} from "./jobModel";
import type { BookSummary } from "./libraryModel";
import { providerDisplayName } from "./i18n/providerNames";
import { goToView } from "./views";
import { useDialogFocus } from "./useDialogFocus";
import { commandErrorText } from "./commandError";

export interface StudioJobs {
  jobs: JobSummary[] | null;
  progress: JobProgress | null;
  progressByJob?: Record<string, JobProgress>;
}

export function useStudioJobs(): StudioJobs {
  const [jobs, setJobs] = useState<JobSummary[] | null>(null);
  const [progress, setProgress] = useState<JobProgress | null>(null);
  const [progressByJob, setProgressByJob] = useState<
    Record<string, JobProgress>
  >({});

  const refresh = useCallback(() => {
    invoke<JobSummary[]>("list_jobs")
      .then(setJobs)
      .catch(() => setJobs((known) => known ?? []));
  }, []);

  useEffect(() => {
    refresh();
    window.addEventListener("txtcaster:job-created", refresh);
    let active = true;
    const unlistenProgress = listen<JobProgress>("job-progress", (event) => {
      if (active) {
        setProgress(event.payload);
        setProgressByJob((current) => ({
          ...current,
          [event.payload.job_id]: event.payload,
        }));
      }
    });
    const unlistenState = listen<{ job_id: string; state: JobState }>(
      "job-state",
      (event) => {
        if (!active) return;
        const { job_id, state } = event.payload;
        setJobs(
          (known) =>
            known?.map((job) =>
              job.id === job_id ? { ...job, state } : job,
            ) ?? known,
        );
        if (isTerminal(state)) {
          setProgress((current) =>
            current?.job_id === job_id ? null : current,
          );
          setProgressByJob((current) => {
            if (!(job_id in current)) return current;
            const next = { ...current };
            delete next[job_id];
            return next;
          });
        }
      },
    );
    return () => {
      active = false;
      window.removeEventListener("txtcaster:job-created", refresh);
      unlistenProgress.then((fn) => fn()).catch(console.error);
      unlistenState.then((fn) => fn()).catch(console.error);
    };
  }, [refresh]);

  return { jobs, progress, progressByJob };
}

export function runningJob(jobs: JobSummary[] | null): JobSummary | null {
  return jobs?.find((job) => isRunning(job.state)) ?? null;
}

export function runningPct(
  job: JobSummary | null,
  progress: JobProgress | null,
): number | null {
  if (job == null || progress == null || progress.job_id !== job.id)
    return null;
  return progressBarPercent(progress);
}

function stageLabelOf(state: JobState, progress: JobProgress | null): string {
  const stage = progress?.stage ?? state;
  const index = stageIndex(stage);
  return index >= 0 && index < STAGE_LABELS.length
    ? tStatic(STAGE_LABELS[index]!)
    : tStatic("activity.starting");
}

function ActivityTray({
  studio,
  engine,
  onOpenProduction,
  onClose,
  elevated = false,
  returnFocusTestId = "nav-jobs",
  anchor = "rail",
}: {
  elevated?: boolean;
  anchor?: "rail" | "corner";
  returnFocusTestId?: string;
  studio: StudioJobs;
  engine: EngineStatus | null;
  onOpenProduction: () => void;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [books, setBooks] = useState<BookSummary[]>([]);
  const panelRef = useRef<HTMLElement>(null);

  const dismiss = useCallback(() => {
    onClose();
    document
      .querySelector<HTMLElement>(`[data-testid="${returnFocusTestId}"]`)
      ?.focus();
  }, [onClose, returnFocusTestId]);

  useDialogFocus(panelRef, true, { onEscape: dismiss });

  useEffect(() => {
    let active = true;
    invoke<BookSummary[]>("list_books")
      .then((rows) => {
        if (active) setBooks(rows);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, []);

  const titleOf = (job: JobSummary) =>
    books.find((book) => book.id === job.bookId)?.title ??
    t("cast.fallbackTitle");
  const badge = presentStatus(engine);
  const jobs = studio.jobs ?? [];
  const active = runningJob(studio.jobs);
  const pct = runningPct(active, studio.progress);
  const queued = jobs.filter((job) => job.state === "QUEUED");
  const paused = jobs.filter((job) => job.state === "PAUSED");
  const hasPerMinute = paused.some(
    (job) => parsePausedReason(job)?.class === "per_minute",
  );
  const [now, setNow] = useState(() => Date.now());
  const [cancelError, setCancelError] = useState<{
    jobId: string;
    message: string;
  } | null>(null);
  useEffect(() => {
    if (!hasPerMinute) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [hasPerMinute]);
  const done = jobs
    .filter((job) => job.state === "DONE")
    .sort((a, b) => b.updatedAt - a.updatedAt)
    .slice(0, 1);
  const quiet =
    active == null &&
    queued.length === 0 &&
    paused.length === 0 &&
    done.length === 0;

  return (
    <div data-modal-layer="" className="contents">
      <button
        type="button"
        aria-label={t("activity.close")}
        tabIndex={-1}
        onClick={dismiss}
        className={`fixed inset-0 cursor-default ${elevated ? "z-[55]" : "z-30"}`}
      />
      <section
        ref={panelRef}
        tabIndex={-1}
        id="activity-tray"
        role="dialog"
        aria-modal="true"
        data-testid="activity-tray"
        aria-label={t("activity.title")}
        className={`animate-modal-in shadow-overlay absolute w-[380px] ${anchor === "rail" ? "start-[4.5rem] top-54 max-h-[calc(100vh-14.5rem)] overflow-y-auto" : "end-4 top-14"} rounded-2xl border border-ink-800 bg-ink-900 outline-none ${
          elevated ? "z-[56]" : "z-40"
        }`}
      >
        <header className="flex items-center justify-between gap-3 border-b border-ink-800 px-5 py-3">
          <h2 className="text-body font-semibold text-ink-50">
            {t("activity.title")}
          </h2>
          <span className="flex items-center gap-2 text-label text-ink-400">
            <span className={`h-2 w-2 rounded-full ${badge.dotClass}`} />
            {badge.label}
          </span>
          <button
            type="button"
            data-testid="close-activity"
            aria-label={t("activity.close")}
            onClick={dismiss}
            className="rounded-lg p-1 text-ink-400 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
          >
            <CloseIcon className="h-4 w-4" />
          </button>
        </header>
        {quiet && (
          <p className="px-5 py-4 text-secondary text-ink-400">
            {t("activity.nothing")}
          </p>
        )}
        {active != null && (
          <div className="border-b border-ink-800/70 px-5 py-3.5">
            <div className="flex items-baseline justify-between gap-3">
              <p className="min-w-0 truncate text-body font-medium text-ink-100">
                {titleOf(active)}
              </p>
              {pct != null && (
                <p className="shrink-0 font-mono text-label text-ink-400">
                  {Math.round(pct)}%
                </p>
              )}
            </div>
            <p className="mt-0.5 text-secondary text-ink-300">
              {stageLabelOf(active.state, studio.progress)}
              {active.overnight ? ` · ${t("job.overnightBadge")}` : ""}
            </p>
            <span className="mt-2.5 block h-1 rounded-full bg-ink-800">
              <span
                className="block h-1 rounded-full bg-ember-500"
                style={{ width: `${Math.round(pct ?? 0)}%` }}
              />
            </span>
          </div>
        )}
        {[...paused, ...queued].map((job) => {
          const reason = job.state === "PAUSED" ? parsePausedReason(job) : null;
          return (
            <div key={job.id} className="border-b border-ink-800/70 px-5 py-3">
              <p className="truncate text-body font-medium text-ink-200">
                {titleOf(job)}
              </p>
              {reason?.class === "per_minute" ? (
                <p
                  data-testid={`tray-rate-limit-${job.id}`}
                  className="mt-0.5 text-secondary text-wash-yellow-ink"
                >
                  {t("job.rateLimitedPerMinute", {
                    provider: providerDisplayName(reason.provider),
                    countdown: formatCountdown(
                      Math.max(0, (reason.retry_at_epoch_ms ?? 0) - now),
                    ),
                    attempt: reason.attempt ?? 1,
                  })}
                </p>
              ) : reason?.class === "daily" ? (
                <div
                  data-testid={`tray-rate-limit-${job.id}`}
                  className="mt-1 space-y-2"
                >
                  <p className="text-secondary text-wash-red-ink">
                    {t("job.rateLimitedDaily", {
                      provider: providerDisplayName(reason.provider),
                    })}
                  </p>
                  <div className="flex items-center gap-2 pt-0.5">
                    <button
                      type="button"
                      data-testid={`tray-settings-${job.id}`}
                      onClick={() => {
                        dismiss();
                        goToView("settings");
                      }}
                      className="rounded-lg bg-ink-800 px-2.5 py-1 text-label font-semibold text-ink-100 transition-colors hover:bg-ink-700 active:scale-[0.98]"
                    >
                      {t("job.openSettings")}
                    </button>
                    <button
                      type="button"
                      data-testid={`tray-cancel-${job.id}`}
                      onClick={() => {
                        setCancelError(null);
                        void invoke("cancel_job", { jobId: job.id }).catch(
                          (raw) =>
                            setCancelError({
                              jobId: job.id,
                              message: commandErrorText(raw),
                            }),
                        );
                      }}
                      className="rounded-lg border border-wash-red-ink/30 px-2.5 py-1 text-label font-medium text-wash-red-ink transition-colors hover:bg-wash-red-ink/10 active:scale-[0.98]"
                    >
                      {t("job.cancel")}
                    </button>
                  </div>
                  {cancelError?.jobId === job.id && (
                    <p
                      role="alert"
                      data-testid={`tray-cancel-error-${job.id}`}
                      className="mt-1 text-label text-wash-red-ink"
                    >
                      {cancelError.message}
                    </p>
                  )}
                </div>
              ) : (
                <p className="mt-0.5 text-secondary text-ink-400">
                  {job.state === "PAUSED"
                    ? t("activity.paused")
                    : t("job.waitingQueue")}
                </p>
              )}
            </div>
          );
        })}
        {done.map((job) => (
          <div
            key={job.id}
            className="flex items-center gap-3 border-b border-ink-800/70 px-5 py-3"
          >
            <div className="min-w-0 flex-1">
              <p className="truncate text-body font-medium text-ink-100">
                {titleOf(job)}
              </p>
              <p className="mt-0.5 text-secondary text-ink-300">
                {t("activity.newAudio")}
              </p>
            </div>
            <button
              type="button"
              onClick={() => goToView("audio")}
              className="flex items-center gap-1.5 rounded-lg bg-ink-800 px-2.5 py-1.5 text-label font-semibold text-ink-100 transition-colors duration-200 hover:bg-ink-700 active:scale-[0.98]"
            >
              <AudioIcon className="h-4 w-4" /> {t("study.listen")}
            </button>
          </div>
        ))}
        <footer className="flex items-center gap-3 px-5 py-3">
          <p className="min-w-0 flex-1 text-label text-ink-400">
            {t("activity.oneAtATime")}
          </p>
          <button
            type="button"
            data-testid="open-production"
            onClick={onOpenProduction}
            className="flex items-center gap-1 rounded-lg px-2 py-1 text-label font-medium text-ink-300 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
          >
            {t("activity.history")} <ChevronIcon className="h-3.5 w-3.5" />
          </button>
        </footer>
      </section>
    </div>
  );
}

export default ActivityTray;
