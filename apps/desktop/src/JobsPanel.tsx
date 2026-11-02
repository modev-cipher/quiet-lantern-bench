import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { revealItemInDir } from "@tauri-apps/plugin-opener";

import { engineRefusal } from "./engineSetupModel";
import { openEngineSetup } from "./engineSetupStore";
import { useI18n } from "./i18n";
import { castScopeLabel, castScopeOf } from "./castScope";
import type { BookSummary } from "./libraryModel";
import {
  buildAudioShelf,
  canPlay,
  formatAudioDate,
  modeLabel,
  type AudioShelfItem,
  type ExportSummary,
} from "./audioShelfModel";
import { dispatch } from "./playbackStore";
import {
  AlertIcon,
  CheckIcon,
  ChevronIcon,
  CloseIcon,
  DownloadIcon,
  FolderIcon,
  JobsIcon,
  PauseIcon,
  PlayIcon,
} from "./Icons";
import DeleteAction from "./DeleteAction";
import { runJobControl } from "./jobsPanelControl";
import ReviewPanel from "./ReviewPanel";
import { staleExportNotice } from "./verifyModel";
import { goToAudioShelf, goToModels, goToSendToPhone, goToView } from "./views";
import { providerDisplayName } from "./i18n/providerNames";
import type { StudioJobs } from "./ActivityTray";
import {
  activeStageProgress,
  canDeleteJob,
  checkpointProgress,
  deleteGateOf,
  castsLayout,
  displayOrder,
  formatCountdown,
  parsePausedReason,
  type PausedReason,
  queuePosition,
  queuePositionLabel,
  type JobDeletePreview,
  failedStage,
  failureMessage,
  providerFailure,
  providerFailureCopy,
  recoveryHistory,
  recoveryLine,
  isRunning,
  applyStateNotice,
  coversHeldNotice,
  executionNotices,
  isTerminal,
  jobBadges,
  missingModelFromFailure,
  parseOutputPaths,
  qualityNotices,
  queuedNotice,
  progressBarPercent,
  railStage,
  stageEtaLabel,
  stageIndex,
  stageLabel,
  stepStatus,
  STAGE_LABELS,
  type JobProgress,
  type JobStateUpdate,
  type JobSummary,
  type StageNotices,
} from "./jobModel";
import { commandErrorText } from "./commandError";

function StageRail({
  job,
  failedStep = null,
}: {
  job: JobSummary;
  failedStep?: number | null;
}) {
  const { t } = useI18n();
  return (
    <div className="flex items-center gap-1.5">
      {STAGE_LABELS.map((label, step) => {
        const failed = failedStep === step;
        const status = failed
          ? "failed"
          : failedStep != null && step > failedStep
            ? "pending"
            : failedStep != null
              ? "done"
              : stepStatus(step, railStage(job));
        return (
          <div
            key={label}
            data-testid={`job-step-${job.id}-${step}`}
            data-status={status}
            className="flex flex-1 flex-col gap-1.5"
          >
            <div
              className={`h-1.5 rounded-full transition-colors duration-300 ${
                status === "failed"
                  ? "bg-wash-red-ink"
                  : status === "done"
                    ? "bg-ink-500"
                    : status === "active"
                      ? "bg-ember-500"
                      : "bg-ink-800"
              }`}
            />
            <span
              className={`text-label font-medium tracking-wide ${
                status === "failed"
                  ? "text-wash-red-ink"
                  : status === "active"
                    ? "text-ember-400"
                    : status === "done"
                      ? "text-ink-400"
                      : "text-ink-500"
              }`}
            >
              {t(label)}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function ControlButton({
  label,
  text,
  testId,
  onClick,
  children,
}: {
  label: string;
  text: string;
  testId: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      data-testid={testId}
      onClick={onClick}
      className="flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-secondary font-medium text-ink-400 transition-colors duration-200 hover:bg-ink-800 hover:text-ink-200 active:scale-[0.96]"
    >
      {children}
      {text}
    </button>
  );
}

export function RateLimitPauseCard({
  job,
  reason,
  onControl,
}: {
  job: JobSummary;
  reason: PausedReason;
  onControl?: (command: string, jobId: string) => void | Promise<void>;
}) {
  const { t } = useI18n();
  const [now, setNow] = useState(() => Date.now());
  const [controlError, setControlError] = useState<string | null>(null);
  const runControl = async (command: string) => {
    if (!onControl) return;
    setControlError(null);
    try {
      await onControl(command, job.id);
    } catch (raw) {
      setControlError(commandErrorText(raw));
    }
  };

  useEffect(() => {
    if (reason.class !== "per_minute") return;
    const timer = window.setInterval(() => {
      setNow(Date.now());
    }, 1000);
    return () => {
      window.clearInterval(timer);
    };
  }, [reason.class]);

  const providerName = providerDisplayName(reason.provider);
  const progress = checkpointProgress(job);

  if (reason.class === "per_minute") {
    const remainingMs = Math.max(0, (reason.retry_at_epoch_ms ?? 0) - now);
    const countdown = formatCountdown(remainingMs);
    const message = t("job.rateLimitedPerMinute", {
      provider: providerName,
      countdown,
      attempt: reason.attempt ?? 1,
    });
    return (
      <>
        <div
          role="status"
          data-testid={`job-rate-limit-card-${job.id}`}
          className="flex items-start gap-2.5 rounded-xl border border-wash-yellow-ink/20 bg-wash-yellow px-4 py-3.5 text-secondary"
        >
          <span className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center text-wash-yellow-ink">
            <AlertIcon className="h-4 w-4" />
          </span>
          <div className="min-w-0 flex-1 space-y-1.5">
            <div className="flex items-center justify-between gap-2">
              <p className="font-semibold text-wash-yellow-ink">
                {t("job.pauseShortBreak")}
              </p>
              {progress && (
                <span className="text-xs text-wash-yellow-ink/80 font-mono">
                  {t("job.pauseProgressSaved", {
                    done: progress.done,
                    total: progress.total,
                  })}
                </span>
              )}
            </div>
            <p
              data-testid={`job-rate-limit-message-${job.id}`}
              className="font-medium text-wash-yellow-ink text-sm"
            >
              {message}
            </p>
            <p className="text-xs text-wash-yellow-ink/90">
              {t("job.pauseNoActionNeeded", { countdown })}
            </p>
            <div className="flex flex-wrap items-center gap-2 pt-1">
              <button
                type="button"
                data-testid={`job-rate-limit-resume-${job.id}`}
                onClick={() => void runControl("resume_job")}
                className="flex items-center gap-1.5 rounded-lg bg-wash-yellow-ink/15 px-3 py-1.5 text-xs font-semibold text-wash-yellow-ink hover:bg-wash-yellow-ink/25 active:scale-[0.98]"
              >
                {t("job.pauseResumeNow")}
              </button>
              {!job.overnight && (
                <button
                  type="button"
                  data-testid={`job-rate-limit-overnight-${job.id}`}
                  onClick={() => void runControl("switch_job_to_overnight")}
                  className="flex items-center gap-1.5 rounded-lg border border-wash-yellow-ink/30 px-3 py-1.5 text-xs font-semibold text-wash-yellow-ink hover:bg-wash-yellow-ink/20 active:scale-[0.98]"
                >
                  {t("job.switchToOvernight")}
                </button>
              )}
            </div>
          </div>
        </div>
        {controlError != null && (
          <p
            role="alert"
            data-testid={`job-rate-limit-error-${job.id}`}
            className="break-words text-label text-wash-red-ink"
          >
            {controlError}
          </p>
        )}
      </>
    );
  }

  const message = t("job.rateLimitedDaily", {
    provider: providerName,
  });

  return (
    <div
      role="alert"
      data-testid={`job-rate-limit-card-${job.id}`}
      className="flex items-start gap-2.5 rounded-xl border border-wash-red-ink/20 bg-wash-red px-4 py-3.5 text-secondary"
    >
      <span className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center text-wash-red-ink">
        <AlertIcon className="h-4 w-4" />
      </span>
      <div className="min-w-0 flex-1 space-y-2">
        <div className="flex items-center justify-between gap-2">
          <p className="font-semibold text-wash-red-ink">
            {t("job.pauseWaitingProvider")}
          </p>
          {progress && (
            <span className="text-xs text-wash-red-ink/80 font-mono">
              {t("job.pauseProgressSaved", {
                done: progress.done,
                total: progress.total,
              })}
            </span>
          )}
        </div>
        <p
          data-testid={`job-rate-limit-message-${job.id}`}
          className="font-medium text-wash-red-ink text-sm"
        >
          {message}
        </p>
        <div className="flex flex-wrap items-center gap-2 pt-1">
          <button
            type="button"
            data-testid={`job-rate-limit-try-now-${job.id}`}
            onClick={() => void runControl("resume_job")}
            className="flex items-center gap-1.5 rounded-lg bg-wash-red-ink/15 px-3 py-1.5 text-xs font-semibold text-wash-red-ink hover:bg-wash-red-ink/25 active:scale-[0.98]"
          >
            {t("job.pauseTryNow")}
          </button>
          <button
            type="button"
            data-testid={`job-rate-limit-settings-${job.id}`}
            onClick={() => goToView("settings")}
            className="flex items-center gap-2 rounded-lg bg-ember-500 px-3.5 py-2 text-secondary font-semibold text-on-accent transition-colors hover:bg-ember-600 active:scale-[0.98]"
          >
            {t("job.openSettings")}
          </button>
          <button
            type="button"
            data-testid={`job-rate-limit-cancel-${job.id}`}
            onClick={() => void runControl("cancel_job")}
            className="flex items-center gap-2 rounded-lg border border-wash-red-ink/30 px-3.5 py-2 text-secondary font-medium text-wash-red-ink transition-colors hover:bg-wash-red-ink/10 active:scale-[0.98]"
          >
            {t("job.cancel")}
          </button>
        </div>
      </div>
      {controlError != null && (
        <p
          role="alert"
          data-testid={`job-rate-limit-error-${job.id}`}
          className="break-words text-label text-wash-red-ink"
        >
          {controlError}
        </p>
      )}
    </div>
  );
}

export function JobRow({
  job,
  bookTitle,
  bookLanguage,
  progress,
  index,
  staleExports,
  queueNotice,
  queueLabel,
  playableItem,
  onControl,
}: {
  job: JobSummary;
  bookTitle: string;
  bookLanguage?: string | null;
  progress: JobProgress | null;
  index: number;
  staleExports: number;
  queueNotice: string | null;
  queueLabel: string | null;
  playableItem: AudioShelfItem | null;
  onControl: (command: string, jobId: string) => void | Promise<void>;
}) {
  const { t, tx } = useI18n();
  const [controlError, setControlError] = useState<string | null>(null);
  const runControl = async (command: string) => {
    setControlError(null);
    try {
      await onControl(command, job.id);
    } catch (raw) {
      setControlError(commandErrorText(raw));
    }
  };
  const showRail = !isTerminal(job.state) && job.state !== "QUEUED";
  const stageProgress = activeStageProgress(progress, job.state);
  const checkpoint = !stageProgress ? checkpointProgress(job) : null;
  const pct = stageProgress
    ? progressBarPercent(stageProgress)
    : checkpoint && checkpoint.total > 0
      ? Math.min(100, Math.max(0, (checkpoint.done / checkpoint.total) * 100))
      : 0;
  const currentItemLabel =
    stageProgress?.current_item ??
    (checkpoint && checkpoint.total > 0
      ? `${checkpoint.done}/${checkpoint.total} segments`
      : t("job.stageStarting"));
  const outputs = parseOutputPaths(job.outputPathsJson);
  const notices = qualityNotices(job.reportJson);
  const execution = executionNotices(job.reportJson, job.state);
  const failedAt =
    job.state === "FAILED"
      ? failedStage(job.reportJson, progress?.stage ?? null)
      : null;
  const failedStep = failedAt ? stageIndex(failedAt) : null;
  const failedLabel = failedAt ? stageLabel(failedAt) : null;
  const failText = job.state === "FAILED" ? failureMessage(job.reportJson) : "";
  const missingModel =
    job.state === "FAILED" ? missingModelFromFailure(failText) : null;
  const cloudFailure =
    job.state === "FAILED" ? providerFailure(job.reportJson) : null;
  const pausedReason = job.state === "PAUSED" ? parsePausedReason(job) : null;
  const recoveredLine = isTerminal(job.state)
    ? recoveryLine(recoveryHistory(job.reportJson))
    : null;
  const [confirmingLocal, setConfirmingLocal] = useState(false);
  const [recoveryError, setRecoveryError] = useState<string | null>(null);
  const retryJob = async (switchToLocal: boolean) => {
    setRecoveryError(null);
    try {
      await invoke("retry_failed_job", { jobId: job.id, switchToLocal });
      setConfirmingLocal(false);
    } catch (raw) {
      // Backend refusals (blocker 7) are shown, never swallowed.
      setConfirmingLocal(false);
      const need = engineRefusal(String(raw));
      if (need != null) {
        openEngineSetup(need, () => void retryJob(switchToLocal));
        return;
      }
      setRecoveryError(commandErrorText(raw));
    }
  };
  const staleNotice =
    job.state === "DONE" ? staleExportNotice(staleExports) : null;
  // F11-U6 J7: the runtime says what a delete would take (sessions, live);
  // the model decides; this row only renders. Re-asked whenever the row
  // becomes deletable or the panel refreshes, so a session that started
  // since is seen before the click.
  const deletable = canDeleteJob(job.state);
  const [deletePreview, setDeletePreview] = useState<JobDeletePreview | null>(
    null,
  );
  useEffect(() => {
    if (!deletable) return;
    let active = true;
    invoke<JobDeletePreview>("job_delete_preview", { jobId: job.id })
      .then((preview) => {
        if (active) setDeletePreview(preview);
      })
      .catch((raw) => {
        if (active) setControlError(commandErrorText(raw));
      });
    return () => {
      active = false;
    };
  }, [deletable, job.id, job.updatedAt]);
  const deleteGate = deleteGateOf(deletable ? deletePreview : null);

  return (
    <article
      data-testid={`job-row-${job.id}`}
      style={{ "--stagger": index } as React.CSSProperties}
      className="animate-rise-in flex flex-col gap-4 rounded-2xl border border-ink-800 bg-ink-900 p-6"
    >
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2.5">
          <h2 className="truncate font-serif text-title font-semibold text-ink-50">
            {bookTitle}
          </h2>
          {jobBadges(job).map((badge) => (
            <span
              key={badge}
              className={`shrink-0 rounded px-2 py-0.5 font-mono text-label tracking-wide uppercase ${
                badge === "overnight"
                  ? "bg-ember-500/15 text-ember-300"
                  : "bg-ink-800 text-ink-400"
              }`}
            >
              {badge}
            </span>
          ))}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {isRunning(job.state) && (
            <ControlButton
              label={t("job.pauseCast")}
              text={t("player.pause")}
              testId={`pause-job-${job.id}`}
              onClick={() => void runControl("pause_job")}
            >
              <PauseIcon className="h-4 w-4" />
            </ControlButton>
          )}
          {job.state === "PAUSED" && (
            <ControlButton
              label={t("job.resumeCast")}
              text={t("study.resume")}
              testId={`resume-job-${job.id}`}
              onClick={() => void runControl("resume_job")}
            >
              <PlayIcon className="h-4 w-4" />
            </ControlButton>
          )}
          {!isTerminal(job.state) && (
            <ControlButton
              label={t("job.cancelCast")}
              text={t("library.cancel")}
              testId={`cancel-job-${job.id}`}
              onClick={() => void runControl("cancel_job")}
            >
              <CloseIcon className="h-4 w-4" />
            </ControlButton>
          )}
        </div>
      </div>
      {controlError != null && (
        <p
          role="alert"
          data-testid={`job-control-error-${job.id}`}
          className="break-words text-label text-wash-red-ink"
        >
          {controlError}
        </p>
      )}

      {showRail && <StageRail job={job} />}

      {failedStep != null && <StageRail job={job} failedStep={failedStep} />}

      {showRail && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center justify-between gap-4 text-secondary">
            <span
              data-testid={`job-current-item-${job.id}`}
              className="truncate text-ink-400 first-letter:uppercase"
            >
              {currentItemLabel}
            </span>
            <span className="shrink-0 font-mono text-ink-500 tabular-nums">
              {job.state === "PAUSED"
                ? t("job.paused")
                : stageEtaLabel(stageProgress?.eta_seconds ?? null)}
            </span>
          </div>
          <div className="shimmer-bar h-1.5 overflow-hidden rounded-full bg-ink-800">
            <div
              data-testid={`job-progress-bar-${job.id}`}
              className="h-full rounded-full bg-ember-500 transition-[width] duration-300"
              style={{ width: `${pct}%` }}
            />
          </div>
          <p
            data-testid={`job-checkpoint-note-${job.id}`}
            className="text-label text-ink-500"
          >
            {job.state === "PAUSED"
              ? t("job.checkpointPaused")
              : t("job.checkpointRunning")}
          </p>
        </div>
      )}

      {pausedReason != null && (
        <RateLimitPauseCard
          job={job}
          reason={pausedReason}
          onControl={onControl}
        />
      )}

      {queueNotice && (
        <p
          data-testid={`job-queued-${job.id}`}
          className="text-secondary text-ember-300/90"
        >
          {queueNotice}
        </p>
      )}
      {queueLabel != null && (
        <p
          data-testid={`job-queue-position-${job.id}`}
          className="text-secondary text-ink-400"
        >
          {queueLabel}
        </p>
      )}

      {recoveredLine != null && (
        <p
          data-testid={`job-recovered-${job.id}`}
          className="text-secondary text-ink-400"
        >
          {recoveredLine}
        </p>
      )}

      {execution.length > 0 && (
        <ul
          data-testid={`job-execution-${job.id}`}
          className="space-y-1 text-secondary text-wash-yellow-ink"
        >
          {execution.map((notice) => (
            <li key={notice}>{notice}</li>
          ))}
        </ul>
      )}

      {job.state === "DONE" && (
        <div className="flex items-start gap-2.5 rounded-xl border border-ink-800 bg-ink-950/40 px-4 py-3.5 text-secondary">
          <span className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-full bg-wash-green-ink/15 text-wash-green-ink">
            <CheckIcon className="h-3 w-3" />
          </span>
          <div className="min-w-0 flex-1 space-y-1">
            <p className="text-body font-semibold text-ink-100">
              {t("job.complete")}
            </p>
            {outputs.map((path) => (
              <div key={path} className="flex items-center gap-2">
                <p
                  data-testid={`job-output-${job.id}`}
                  data-stale={staleNotice ? "true" : "false"}
                  className={`min-w-0 flex-1 truncate font-mono ${
                    staleNotice ? "text-ink-500 line-through" : "text-ink-300"
                  }`}
                >
                  {path}
                </p>
                <button
                  type="button"
                  data-testid={`job-reveal-${job.id}`}
                  aria-label={
                    staleNotice ? t("job.revealStale") : t("job.reveal")
                  }
                  title={
                    staleNotice ? t("job.revealStaleTitle") : t("job.reveal")
                  }
                  onClick={() => void revealItemInDir(path).catch(() => {})}
                  className="shrink-0 rounded-md p-1 text-ink-500 transition-colors hover:bg-ink-800 hover:text-ink-200 active:scale-[0.96]"
                >
                  <FolderIcon className="h-4 w-4" />
                </button>
              </div>
            ))}
            {!staleNotice && (
              <div className="flex flex-wrap items-center gap-2 pt-1.5">
                {playableItem && (
                  <button
                    type="button"
                    data-testid={`job-play-${job.id}`}
                    onClick={() =>
                      dispatch({ type: "load", item: playableItem })
                    }
                    className="flex items-center gap-2 rounded-lg bg-ember-500 px-3.5 py-2 text-secondary font-semibold text-on-accent transition-colors hover:bg-ember-600 active:scale-[0.98]"
                  >
                    <PlayIcon className="h-4 w-4" />
                    {t("job.playIt")}
                  </button>
                )}
                {playableItem && (
                  <button
                    type="button"
                    data-testid={`job-send-${job.id}`}
                    onClick={goToSendToPhone}
                    className="flex items-center gap-2 rounded-lg border border-ink-700 px-3.5 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-850 hover:text-ink-50 active:scale-[0.98]"
                  >
                    {t("job.sendToPhone")}
                  </button>
                )}
                <button
                  type="button"
                  data-testid={`job-open-shelf-${job.id}`}
                  onClick={goToAudioShelf}
                  className="flex items-center gap-2 rounded-lg border border-ink-700 px-3.5 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-850 hover:text-ink-50 active:scale-[0.98]"
                >
                  {t("job.openShelf")}
                </button>
              </div>
            )}
            {notices.length > 0 && (
              <ul
                data-testid={`job-notices-${job.id}`}
                className="space-y-0.5 pt-1 text-wash-yellow-ink"
              >
                {notices.map((notice) => (
                  <li key={notice}>{notice}</li>
                ))}
              </ul>
            )}
          </div>
        </div>
      )}

      {job.state === "FAILED" && (
        <div
          role="alert"
          data-testid={`job-error-${job.id}`}
          className="flex items-start gap-2.5 rounded-xl border border-wash-red-ink/20 bg-wash-red px-4 py-3.5 text-secondary"
        >
          <span className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center text-wash-red-ink">
            <AlertIcon className="h-4 w-4" />
          </span>
          <div className="min-w-0 flex-1 space-y-1.5">
            <p className="font-medium text-wash-red-ink">
              {missingModel
                ? t("job.missingModel")
                : failedLabel
                  ? t("job.failedDuring", { stage: failedLabel })
                  : t("job.failed")}
            </p>
            {cloudFailure == null && (
              <div className="mt-1 space-y-2">
                <p
                  data-testid={`job-error-message-${job.id}`}
                  className="break-words text-wash-red-ink/90"
                >
                  {missingModel
                    ? t("job.needsModel", { name: missingModel })
                    : failText}
                </p>
                {recoveryError != null && (
                  <p
                    data-testid={`job-recovery-error-${job.id}`}
                    className="text-label text-wash-red-ink"
                  >
                    {recoveryError}
                  </p>
                )}
                {missingModel ? (
                  <button
                    type="button"
                    data-testid={`job-get-model-${job.id}`}
                    onClick={goToModels}
                    className="flex items-center gap-2 rounded-lg bg-ember-500 px-3.5 py-2 text-secondary font-semibold text-on-accent transition-colors hover:bg-ember-600 active:scale-[0.98]"
                  >
                    <DownloadIcon className="h-4 w-4" />
                    {t("job.installInModels")}
                  </button>
                ) : (
                  <div className="flex flex-wrap gap-2 pt-1">
                    <button
                      type="button"
                      data-testid={`job-retry-${job.id}`}
                      onClick={() => void retryJob(false)}
                      className="rounded-lg bg-ink-50 px-3.5 py-2 text-secondary font-semibold text-ink-950 transition-colors hover:bg-ink-150 active:scale-[0.98]"
                    >
                      {t("job.retry")}
                    </button>
                  </div>
                )}
              </div>
            )}
            {cloudFailure != null && (
              <div
                data-testid={`job-provider-recovery-${job.id}`}
                className="mt-1 space-y-2"
              >
                <p
                  data-testid={`job-error-message-${job.id}`}
                  className="text-wash-red-ink/90"
                >
                  {providerFailureCopy(cloudFailure)}
                </p>
                {recoveryError != null && (
                  <p
                    data-testid={`job-recovery-error-${job.id}`}
                    className="text-label text-wash-red-ink"
                  >
                    {recoveryError}
                  </p>
                )}
                {confirmingLocal ? (
                  <div
                    data-testid={`job-local-confirm-${job.id}`}
                    className="space-y-2 rounded-lg border border-ink-800 bg-ink-950 p-3"
                  >
                    <p className="text-secondary text-ink-200">
                      {t("job.switchLocalQuestion")}
                    </p>
                    <div className="flex gap-2">
                      <button
                        type="button"
                        data-testid={`job-retry-local-confirm-${job.id}`}
                        onClick={() => void retryJob(true)}
                        className="rounded-lg bg-ink-50 px-3.5 py-2 text-secondary font-semibold text-ink-950 transition-colors hover:bg-ink-150 active:scale-[0.98]"
                      >
                        {t("job.switchAndRetry")}
                      </button>
                      <button
                        type="button"
                        onClick={() => setConfirmingLocal(false)}
                        className="rounded-lg border border-ink-700 px-3.5 py-2 text-secondary text-ink-300 transition-colors hover:border-ink-600"
                      >
                        {t("job.keepAsIs")}
                      </button>
                    </div>
                  </div>
                ) : (
                  <div className="flex flex-wrap gap-2">
                    {cloudFailure.retryable && (
                      <button
                        type="button"
                        data-testid={`job-retry-provider-${job.id}`}
                        onClick={() => void retryJob(false)}
                        className="rounded-lg bg-ink-50 px-3.5 py-2 text-secondary font-semibold text-ink-950 transition-colors hover:bg-ink-150 active:scale-[0.98]"
                      >
                        {t("job.retrySameProvider")}
                      </button>
                    )}
                    <button
                      type="button"
                      data-testid={`job-retry-local-${job.id}`}
                      onClick={() => setConfirmingLocal(true)}
                      className="rounded-lg border border-ink-700 bg-ink-800 px-3.5 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-700 active:scale-[0.98]"
                    >
                      {t("job.switchLocal")}
                    </button>
                    <button
                      type="button"
                      data-testid={`job-recast-${job.id}`}
                      onClick={() => goToView("library")}
                      className="rounded-lg border border-ink-700 bg-ink-800 px-3.5 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-700 active:scale-[0.98]"
                    >
                      {t("job.newCastOtherProvider")}
                    </button>
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      )}

      {staleNotice && (
        <div
          role="status"
          data-testid={`job-stale-${job.id}`}
          className="flex items-start gap-2.5 rounded-xl border border-wash-yellow-ink/20 bg-wash-yellow px-4 py-3.5 text-secondary"
        >
          <span className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center text-wash-yellow-ink">
            <AlertIcon className="h-4 w-4" />
          </span>
          <div className="min-w-0 flex-1 space-y-1.5">
            <p className="text-wash-yellow-ink">{tx(staleNotice.key, staleNotice.params)}</p>
            <button
              type="button"
              data-testid={`job-rerender-${job.id}`}
              onClick={() => void runControl("rerender_job")}
              className="flex items-center gap-2 rounded-lg bg-ember-500 px-3.5 py-2 text-secondary font-semibold text-on-accent transition-colors hover:bg-ember-600 active:scale-[0.98]"
            >
              {t("job.rerender")}
            </button>
          </div>
        </div>
      )}

      {job.state !== "CREATED" && <ReviewPanel job={job} bookLanguage={bookLanguage} />}

      {job.state === "CANCELLED" && (
        <p className="text-secondary text-ink-500">{t("job.cancelled")}</p>
      )}

      {deletable && (
        <div
          className="flex justify-end border-t border-ink-800/50 pt-2.5"
          data-testid={`job-delete-gate-${job.id}`}
          data-gate={deleteGate.kind}
        >
          <DeleteAction
            testId={`job-${job.id}`}
            title={deleteGate.kind === "pending" ? undefined : deleteGate.title}
            question={
              deleteGate.kind === "confirm" ? deleteGate.question : undefined
            }
            confirmLabel={
              deleteGate.kind === "confirm"
                ? deleteGate.confirmLabel
                : undefined
            }
            cancelLabel={
              deleteGate.kind === "confirm" ? deleteGate.cancelLabel : undefined
            }
            requireConfirm={deleteGate.kind === "confirm"}
            disabled={
              deleteGate.kind === "pending" || deleteGate.kind === "refused"
            }
            onConfirm={() => void runControl("delete_job")}
          />
        </div>
      )}
    </article>
  );
}

function HistoryRow({
  job,
  bookTitle,
  item,
  playable,
  open,
  onToggle,
}: {
  job: JobSummary;
  bookTitle: string;
  item: AudioShelfItem | null;
  playable: AudioShelfItem | null;
  open: boolean;
  onToggle: () => void;
}) {
  const { t, locale } = useI18n();
  const scope = item == null ? null : castScopeOf(item.path);
  const tone =
    job.state === "DONE"
      ? "bg-wash-green-ink"
      : job.state === "FAILED"
        ? "bg-wash-red-ink"
        : "bg-wash-yellow-ink";
  return (
    <div
      data-testid={`job-history-${job.id}`}
      className="group flex items-center gap-3 rounded-xl border border-transparent px-3 py-2 transition-colors duration-200 hover:border-ink-800 hover:bg-ink-900"
    >
      <span aria-hidden="true" className={`h-2 w-2 shrink-0 rounded-full ${tone}`} />
      <button
        type="button"
        data-testid={`job-expand-${job.id}`}
        aria-expanded={open}
        onClick={onToggle}
        className="flex min-w-0 flex-1 items-baseline gap-2.5 text-start"
      >
        <span className="truncate text-body font-semibold text-ink-100">
          {bookTitle}
        </span>
        {scope != null && (
          <span className="shrink-0 text-secondary text-ink-300">
            {castScopeLabel(scope)}
          </span>
        )}
        <span className="truncate text-label text-ink-500">
          {[modeLabel(job.mode), formatAudioDate(job.updatedAt, locale)]
            .filter(Boolean)
            .join(" · ")}
        </span>
      </button>
      {playable != null && (
        <button
          type="button"
          data-testid={`job-history-play-${job.id}`}
          onClick={() => dispatch({ type: "load", item: playable })}
          className="flex shrink-0 items-center gap-1.5 rounded-lg px-2.5 py-1 text-secondary font-medium text-ember-400 transition-colors duration-200 hover:bg-ink-850 hover:text-ember-300"
        >
          <PlayIcon className="h-3.5 w-3.5" />
          {t("job.play")}
        </button>
      )}
      <button
        type="button"
        aria-label={open ? t("job.hideDetails") : t("job.showDetails")}
        aria-expanded={open}
        onClick={onToggle}
        className="shrink-0 rounded-md p-1 text-ink-500 transition-colors duration-200 hover:bg-ink-850 hover:text-ink-200"
      >
        <ChevronIcon
          className={`h-4 w-4 transition-transform duration-300 ${open ? "rotate-90" : ""}`}
        />
      </button>
    </div>
  );
}

function JobsSkeleton() {
  return (
    <div
      aria-hidden="true"
      data-testid="jobs-skeleton"
      className="flex flex-col gap-3"
    >
      {[0, 1].map((row) => (
        <div
          key={row}
          className="flex flex-col gap-4 rounded-2xl border border-ink-800 bg-ink-900 p-6"
        >
          <div className="flex items-center justify-between">
            <div className="shimmer-bar h-4 w-48 rounded bg-ink-800/70" />
            <div className="shimmer-bar h-4 w-16 rounded bg-ink-800/50" />
          </div>
          <div className="shimmer-bar h-1.5 rounded-full bg-ink-800/60" />
          <div className="shimmer-bar h-3 w-64 rounded bg-ink-800/50" />
        </div>
      ))}
    </div>
  );
}

export interface JobsPanelProps {
  studio?: StudioJobs;
}

function JobsPanel({ studio }: JobsPanelProps) {
  const { t } = useI18n();
  const [jobs, setJobs] = useState<JobSummary[] | null>(null);
  const [books, setBooks] = useState<BookSummary[]>([]);
  const [localProgress, setLocalProgress] = useState<Record<string, JobProgress>>({});
  const progress = studio?.progressByJob ?? localProgress;
  const [exports, setExports] = useState<ExportSummary[]>([]);
  const [staleByJob, setStaleByJob] = useState<Record<string, number>>({});
  const [notices, setNotices] = useState<StageNotices>({});
  const [loadError, setLoadError] = useState<string | null>(null);
  const [openOverride, setOpenOverride] = useState<Record<string, boolean>>({});

  const refresh = useCallback(() => {
    setLoadError(null);
    invoke<JobSummary[]>("list_jobs")
      .then(setJobs)
      .catch((err) => setLoadError(commandErrorText(err)));
    invoke<BookSummary[]>("list_books").then(setBooks).catch(console.error);
    invoke<ExportSummary[]>("list_exports")
      .then((rows) => {
        setExports(rows);
        const counts: Record<string, number> = {};
        for (const row of rows) {
          if (!row.stale || !row.jobId) continue;
          counts[row.jobId] = (counts[row.jobId] ?? 0) + 1;
        }
        setStaleByJob(counts);
      })
      .catch(console.error);
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (studio?.progressByJob) return;
    let active = true;
    const unlistenProgress = listen<JobProgress>("job-progress", (event) => {
      if (!active) return;
      setLocalProgress((current) => ({
        ...current,
        [event.payload.job_id]: event.payload,
      }));
    });
    return () => {
      active = false;
      unlistenProgress.then((fn) => fn()).catch(console.error);
    };
  }, [studio?.progressByJob]);

  useEffect(() => {
    let active = true;
    const unlistenState = listen<JobStateUpdate>("job-state", (event) => {
      if (!active) return;
      setJobs((current) =>
        (current ?? []).map((job) =>
          job.id === event.payload.job_id
            ? { ...job, state: event.payload.state }
            : job,
        ),
      );
      setNotices((current) => applyStateNotice(current, event.payload));
      if (isTerminal(event.payload.state)) refresh();
    });
    return () => {
      active = false;
      unlistenState.then((fn) => fn()).catch(console.error);
    };
  }, [refresh]);

  useEffect(() => {
    const onCreated = () => refresh();
    window.addEventListener("txtcaster:job-created", onCreated);
    return () => window.removeEventListener("txtcaster:job-created", onCreated);
  }, [refresh]);

  const control = useCallback(
    (command: string, jobId: string) =>
      runJobControl(command, jobId, jobs ?? []).then(refresh),
    [jobs, refresh],
  );

  const bookTitle = useCallback(
    (bookId: string) =>
      books.find((book) => book.id === bookId)?.title ?? t("job.unknownBook"),
    [books, t],
  );

  const ordered = displayOrder(jobs ?? []);
  const activeCount = ordered.filter((job) => !isTerminal(job.state)).length;
  const shelf = buildAudioShelf(exports, books);
  const playableFor = (jobId: string) =>
    shelf.find(
      (item) => item.jobId === jobId && !item.stale && canPlay(item),
    ) ?? null;

  const layout = castsLayout(ordered);
  const renderJob = (job: JobSummary, index: number) => (
            <div key="detail" className="flex flex-col gap-2">
              {notices[job.id]?.text &&
              !coversHeldNotice(
                executionNotices(job.reportJson, job.state),
                notices[job.id]?.text,
              ) ? (
                <div
                  role="status"
                  aria-live="polite"
                  data-testid={`job-notice-${job.id}`}
                  className="rounded-xl border border-ember-500/30 bg-ember-500/[0.07] px-4 py-2.5 text-secondary text-ember-200"
                >
                  {notices[job.id]?.text}
                </div>
              ) : null}
              <JobRow
                job={job}
                index={index}
                bookTitle={bookTitle(job.bookId)}
                bookLanguage={books.find((book) => book.id === job.bookId)?.language}
                progress={progress[job.id] ?? null}
                staleExports={staleByJob[job.id] ?? 0}
                queueNotice={queuedNotice(job, jobs ?? [])}
                queueLabel={queuePositionLabel(queuePosition(job, jobs ?? []))}
                playableItem={job.state === "DONE" ? playableFor(job.id) : null}
                onControl={control}
              />
            </div>
  );

  const castItems = (): React.ReactNode[] => {
    const items: React.ReactNode[] = [];
    if (layout.now.length > 0) {
      items.push(
        <h2
          key="heading-now"
          data-testid="jobs-now"
          className="text-label font-semibold tracking-[0.12em] text-ink-500 uppercase"
        >
          {t("job.now")}
        </h2>,
      );
    }
    layout.now.forEach((job, index) => {
      items.push(
        <div key={job.id} className="flex flex-col gap-2">
          {renderJob(job, index)}
        </div>,
      );
    });
    if (layout.history.length > 0) {
      items.push(
        <h2
          key="heading-history"
          data-testid="jobs-history"
          className={`text-label font-semibold tracking-[0.12em] text-ink-500 uppercase ${
            layout.now.length > 0 ? "mt-5" : ""
          }`}
        >
          {t("job.history")}
        </h2>,
      );
    }
    layout.history.forEach(({ job, openByDefault }, index) => {
      const open = openOverride[job.id] ?? openByDefault;
      items.push(
        <div key={job.id} className="flex flex-col gap-2">
          <HistoryRow
            key="summary"
            job={job}
            bookTitle={bookTitle(job.bookId)}
            item={shelf.find((entry) => entry.jobId === job.id) ?? null}
            playable={job.state === "DONE" ? playableFor(job.id) : null}
            open={open}
            onToggle={() =>
              setOpenOverride((current) => ({
                ...current,
                [job.id]: !open,
              }))
            }
          />
          {open && renderJob(job, layout.now.length + index)}
        </div>,
      );
    });
    return items;
  };

  return (
    <section
      data-testid="jobs-panel"
      className="mx-auto flex w-full max-w-panel flex-col gap-7 px-10 py-10"
    >
      <header className="flex items-end justify-between gap-4">
        <div className="flex flex-col gap-1.5">
          <h1 className="font-serif text-display font-semibold tracking-tight text-ink-50">
            {t("job.title")}
          </h1>
          <p className="text-body text-ink-400">
            {jobs == null
              ? t("job.loadingQueue")
              : ordered.length === 0
                ? t("job.queueEmpty")
                : t("job.activeTotal", {
                    active: activeCount,
                    total: ordered.length,
                  })}
          </p>
        </div>
      </header>

      {loadError ? (
        <div
          role="alert"
          className="flex items-center justify-between gap-4 rounded-xl border border-wash-red-ink/20 bg-wash-red px-5 py-4 text-body text-wash-red-ink"
        >
          <span>{loadError}</span>
          <button
            type="button"
            onClick={refresh}
            className="rounded border border-wash-red-ink/30 px-3 py-1 text-secondary font-medium hover:bg-wash-red-ink/10"
          >
            {t("session.retry")}
          </button>
        </div>
      ) : jobs == null ? (
        <JobsSkeleton />
      ) : ordered.length === 0 ? (
        <div className="flex min-h-[300px] flex-1 flex-col items-center justify-center gap-4 rounded-xl border border-dashed border-ink-700 bg-ink-900 py-16 text-center">
          <span className="flex h-16 w-16 items-center justify-center rounded-2xl bg-ink-800/70 text-ink-400">
            <JobsIcon className="h-7 w-7" />
          </span>
          <div className="flex flex-col gap-1.5">
            <p className="font-serif text-title font-semibold text-ink-100">
              {t("job.noCastsYet")}
            </p>
            <p className="text-body text-ink-500">{t("job.emptyHint")}</p>
          </div>
          <button
            type="button"
            data-testid="jobs-empty-to-library"
            onClick={() => goToView("library")}
            className="flex items-center gap-2 rounded-xl bg-ink-50 px-5 py-2.5 text-body font-semibold text-ink-950 transition-colors duration-200 hover:bg-ink-150 active:scale-[0.98]"
          >
            {t("job.browseBooks")}
          </button>
        </div>
      ) : (
        <div className="flex flex-col gap-3">{castItems()}</div>
      )}
    </section>
  );
}

export default JobsPanel;
