import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  useSyncExternalStore,
} from "react";

import DeleteAction from "./DeleteAction";
import { F11_STRINGS } from "./f11Strings";
import { useI18n } from "./i18n";
import Button from "./ui/Button";
import SessionView from "./SessionView";
import {
  chaptersLabel,
  deleteSession,
  endReasonLine,
  ensureRetention,
  forgetScope,
  formatDuration,
  formatSessionDate,
  getDetailSnapshot,
  getHistorySnapshot,
  getRetentionSnapshot,
  historyFootLine,
  loadDetail,
  loadHistory,
  openTargetOf,
  showAllAvailable,
  subscribeHistory,
  visibleRows,
  type HistoryScope,
  INITIAL_RETENTION,
  type HistoryScopeState,
  type OpenTarget,
  type RetentionState,
  type SessionSummary,
} from "./sessionHistoryStore";
import { goToView } from "./views";

const LABEL =
  "text-label font-semibold tracking-[0.14em] text-ink-500 uppercase";
const LINK =
  "text-label text-ink-500 transition-colors duration-200 hover:text-ink-300";

interface ListProps {
  scope: HistoryScope;
  state: HistoryScopeState;
  showAll: boolean;
  onShowAll: () => void;
  onOpen: (sessionId: string) => void;
  onContinue: (sessionId: string) => void;
  onRetry: () => void;
  onForget: () => void | Promise<void>;
  notice?: string | null;
  continueDisabled?: boolean;
  nowMs?: number;
  timeZone?: string;
  /** F11-U6 §6.2: the retention setting as the store read it. */
  retention?: RetentionState;
}

function SessionRow({
  row,
  newest,
  onOpen,
  onContinue,
  continueDisabled,
  nowMs,
  timeZone,
}: {
  row: SessionSummary;
  newest: boolean;
  onOpen: (sessionId: string) => void;
  onContinue: (sessionId: string) => void;
  continueDisabled: boolean;
  nowMs: number;
  timeZone?: string;
}) {
  const reason = endReasonLine(row);
  const chapters = chaptersLabel(row);
  return (
    <li className="flex items-start gap-2 py-2">
      <button
        type="button"
        data-testid={`session-history-row-${row.id}`}
        onClick={() => onOpen(row.id)}
        className="group min-w-0 flex-1 text-start"
      >
        <span
          data-testid="session-history-date"
          className="block whitespace-nowrap text-body text-ink-100 transition-colors duration-200 group-hover:text-ember-300"
        >
          {formatSessionDate(row.started_at, nowMs, timeZone)}
        </span>
        <span
          data-testid="session-history-duration"
          className="block whitespace-nowrap text-secondary text-ink-400"
        >
          {formatDuration(row.duration_ms)}
          {chapters != null && ` · ${chapters}`}
        </span>
        {reason != null && (
          <span
            data-testid="session-history-reason"
            className="block text-secondary text-ink-400"
          >
            {reason}
          </span>
        )}
        {row.summary_excerpt != null && (
          <span
            data-testid="session-history-excerpt"
            className="block truncate text-secondary text-ink-300"
          >
            {row.summary_excerpt}
          </span>
        )}
      </button>
      {newest && (
        <Button
          variant="soft"
          size="sm"
          className="shrink-0"
          data-testid="session-history-continue"
          disabled={continueDisabled}
          onClick={() => onContinue(row.id)}
        >
          {F11_STRINGS.continueSession}
        </Button>
      )}
    </li>
  );
}

export function SessionHistoryList({
  scope,
  state,
  showAll,
  onShowAll,
  onOpen,
  onContinue,
  onRetry,
  onForget,
  notice = null,
  continueDisabled = false,
  nowMs,
  timeZone,
  retention = INITIAL_RETENTION,
}: ListProps) {
  useI18n(); // I18N-2: copy comes from F11 getters; re-render on a locale switch.
  const now = nowMs ?? Date.now();
  const book = scope.bookId != null;
  const hasRows = state.rows.length > 0;
  const foot = historyFootLine(state, retention);
  return (
    <section
      data-testid="session-history"
      className="flex min-h-0 flex-col gap-2"
    >
      <p className={LABEL}>{F11_STRINGS.pastSessions}</p>
      {state.status === "loading" && !hasRows && (
        <p
          data-testid="session-history-loading"
          className="text-secondary text-ink-400"
        >
          {F11_STRINGS.historyLoading}
        </p>
      )}
      {state.status === "error" && (
        <p
          role="status"
          data-testid="session-history-error"
          className="flex flex-wrap items-baseline gap-2 text-secondary text-reading-stale"
        >
          {F11_STRINGS.historyError}
          <button type="button" onClick={onRetry} className={LINK}>
            {F11_STRINGS.retry}
          </button>
        </p>
      )}
      {foot === "empty" && (
        <p
          data-testid="session-history-empty"
          className="text-secondary text-ink-400"
        >
          {book
            ? F11_STRINGS.historyEmptyBook
            : F11_STRINGS.historyEmptyStandalone}
        </p>
      )}
      {hasRows && (
        <ul className="flex min-h-0 flex-col divide-y divide-ink-800 overflow-y-auto">
          {visibleRows(state.rows, showAll).map((row, at) => (
            <SessionRow
              key={row.id}
              row={row}
              newest={at === 0}
              onOpen={onOpen}
              onContinue={onContinue}
              continueDisabled={continueDisabled}
              nowMs={now}
              timeZone={timeZone}
            />
          ))}
        </ul>
      )}
      {hasRows && !showAll && showAllAvailable(state.rows) && (
        <button
          type="button"
          data-testid="session-history-show-all"
          onClick={onShowAll}
          className={`self-start ${LINK}`}
        >
          {F11_STRINGS.showAll}
        </button>
      )}
      {foot === "retention" && (
        <p
          role="status"
          data-testid="session-history-retention"
          className="flex flex-wrap items-baseline gap-2 text-secondary text-reading-stale"
        >
          {F11_STRINGS.retentionNever}
          <button
            type="button"
            data-testid="session-history-retention-settings"
            onClick={() => goToView("settings")}
            className={LINK}
          >
            {F11_STRINGS.retentionChange}
          </button>
        </p>
      )}
      {notice != null && (
        <p role="status" className="text-secondary text-reading-stale">
          {notice}
        </p>
      )}
      {hasRows && (
        <div className="mt-1">
          <DeleteAction
            testId="history-forget"
            label={book ? F11_STRINGS.forgetBook : F11_STRINGS.forgetStandalone}
            question={
              book
                ? F11_STRINGS.forgetConfirmQuestion
                : F11_STRINGS.forgetStandaloneConfirmQuestion
            }
            confirmLabel={F11_STRINGS.forgetConfirmLabel}
            cancelLabel={F11_STRINGS.keep}
            onConfirm={() => void onForget()}
          />
        </div>
      )}
    </section>
  );
}

interface Props {
  bookId: string | null;
  jobId: string | null;
  onContinue: (sessionId: string) => void;
  onOpenTarget?: (target: OpenTarget) => void;
  continueDisabled?: boolean;
}

export default function SessionHistory({
  bookId,
  jobId,
  onContinue,
  onOpenTarget,
  continueDisabled = false,
}: Props) {
  useI18n(); // I18N-2: copy comes from F11 getters; re-render on a locale switch.
  const scope = useMemo<HistoryScope>(() => ({ bookId }), [bookId]);
  const state = useSyncExternalStore(
    subscribeHistory,
    () => getHistorySnapshot(scope),
    () => getHistorySnapshot(scope),
  );
  const retention = useSyncExternalStore(
    subscribeHistory,
    getRetentionSnapshot,
    getRetentionSnapshot,
  );
  const [showAll, setShowAll] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  useEffect(() => {
    void loadHistory(scope);
    ensureRetention();
    setShowAll(false);
    setOpenId(null);
    setNotice(null);
  }, [scope]);

  const detail = useSyncExternalStore(
    subscribeHistory,
    () => (openId == null ? null : getDetailSnapshot(openId)),
    () => (openId == null ? null : getDetailSnapshot(openId)),
  );

  const open = useCallback(
    (sessionId: string) => {
      const row = state.rows.find((entry) => entry.id === sessionId);
      if (row == null) return;
      setDeleteError(null);
      setOpenId(sessionId);
      void loadDetail(sessionId);
      onOpenTarget?.(openTargetOf(row, jobId));
    },
    [jobId, onOpenTarget, state.rows],
  );

  const forget = useCallback(() => {
    setNotice(null);
    return forgetScope(scope).then(
      () => undefined,
      () => setNotice(F11_STRINGS.forgetFailed),
    );
  }, [scope]);

  const openRow =
    openId == null
      ? null
      : (state.rows.find((entry) => entry.id === openId) ??
        detail?.detail?.row ??
        null);

  if (openId != null && openRow != null) {
    return (
      <SessionView
        row={openRow}
        detail={detail}
        notice={openTargetOf(openRow, jobId).notice}
        onBack={() => setOpenId(null)}
        onContinue={() => onContinue(openRow.id)}
        onDelete={() =>
          deleteSession(scope, openRow.id).then(
            () => setOpenId(null),
            () => setDeleteError(F11_STRINGS.deleteFailed),
          )
        }
        deleteError={deleteError}
        continueDisabled={continueDisabled}
      />
    );
  }

  return (
    <SessionHistoryList
      scope={scope}
      state={state}
      retention={retention}
      showAll={showAll}
      onShowAll={() => setShowAll(true)}
      onOpen={open}
      onContinue={onContinue}
      onRetry={() => void loadHistory(scope)}
      onForget={forget}
      notice={notice}
      continueDisabled={continueDisabled}
    />
  );
}
