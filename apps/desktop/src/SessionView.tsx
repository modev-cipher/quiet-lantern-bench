import DeleteAction from "./DeleteAction";
import {
  endReasonLine,
  formatDuration,
  formatSessionDate,
  sortedTurns,
  type DetailState,
  type SessionSummary,
} from "./sessionHistoryStore";
import { useI18n } from "./i18n";
import TutorRichText from "./TutorRichText";
import Voiceprint from "./ui/Voiceprint";
import { voiceprintFrom } from "./voiceSessionModel";

const LABEL =
  "text-label font-semibold tracking-[0.14em] text-ink-500 uppercase";
const LINK =
  "text-label text-ink-500 transition-colors duration-200 hover:text-ink-300";
const BUBBLE_USER =
  "tutor-bubble ms-auto max-w-[92%] rounded-xl bg-ink-850 px-3 py-2 text-secondary leading-snug text-ink-100";
const BUBBLE_TUTOR =
  "tutor-bubble me-auto max-w-[92%] rounded-xl bg-ink-800 px-3 py-2 text-secondary leading-snug text-ink-50";

interface Props {
  row: SessionSummary;
  detail: DetailState | null;
  notice: string | null;
  onBack: () => void;
  onContinue: () => void;
  onDelete: () => void | Promise<void>;
  deleteError: string | null;
  continueDisabled?: boolean;
  nowMs?: number;
  timeZone?: string;
}

export default function SessionView({
  row,
  detail,
  notice,
  onBack,
  onContinue,
  onDelete,
  deleteError,
  continueDisabled = false,
  nowMs,
  timeZone,
}: Props) {
  const { t } = useI18n();
  const reason = endReasonLine(row);
  const turns = sortedTurns(detail?.detail?.turns ?? []);
  const summary = detail?.detail?.summary_text ?? row.summary_excerpt;
  return (
    <section
      data-testid="session-view"
      className="flex min-h-0 flex-1 flex-col gap-3"
    >
      <button
        type="button"
        data-testid="session-view-back"
        onClick={onBack}
        className={`self-start ${LINK}`}
      >
        ‹ {t("session.back")}
      </button>
      <header className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <h3 className="font-serif text-title font-semibold text-ink-100">
            {formatSessionDate(row.started_at, nowMs ?? Date.now(), timeZone)}
          </h3>
          <p className="text-secondary text-ink-400">
            {formatDuration(row.duration_ms)}
            {reason != null && ` · ${reason}`}
          </p>
          {notice != null && (
            <p
              role="status"
              data-testid="session-view-notice"
              className="mt-1 text-secondary text-reading-stale"
            >
              {notice}
            </p>
          )}
        </div>
        <button
          type="button"
          data-testid="session-view-continue"
          disabled={continueDisabled}
          onClick={onContinue}
          className="shrink-0 rounded-lg bg-ink-50 px-3 py-1.5 text-secondary font-semibold text-ink-950 transition-transform duration-200 active:scale-[0.98] disabled:opacity-50"
        >
          {t("session.continue")}
        </button>
      </header>
      {summary != null && (
        <div>
          <p className={LABEL}>{t("session.summary")}</p>
          <p
            data-testid="session-view-summary"
            className="mt-1 text-body text-ink-300"
          >
            {summary}
          </p>
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-col gap-1.5">
        <p className={LABEL}>{t("session.transcript")}</p>
        {detail?.status === "loading" && (
          <p className="text-secondary text-ink-400">
            {t("session.transcriptLoading")}
          </p>
        )}
        {detail?.status === "error" && (
          <p role="status" className="text-secondary text-reading-stale">
            {t("session.transcriptError")}
          </p>
        )}
        {turns.length > 0 && (
          <ul
            role="log"
            data-testid="voice-session-transcript"
            className="flex min-h-0 flex-1 flex-col gap-2.5 overflow-y-auto"
          >
            {turns.map((turn) => {
              const print =
                turn.role === "user" ? voiceprintFrom(turn.voiceprint) : null;
              return (
                <li
                  key={turn.turn_idx}
                  className={turn.role === "user" ? BUBBLE_USER : BUBBLE_TUTOR}
                >
                  <span className="sr-only">
                    {turn.role === "user" ? t("tutor.you") : t("tutor.tutor")}
                    :{" "}
                  </span>
                  {print != null && (
                    <span className="mb-1 flex items-center text-label text-ink-500">
                      <Voiceprint bars={print} label={t("voice.spoken")} />
                    </span>
                  )}
                  <TutorRichText text={turn.text} />
                </li>
              );
            })}
          </ul>
        )}
      </div>
      <div className="flex flex-col gap-1">
        <DeleteAction
          testId={`session-view-${row.id}`}
          label={t("session.deleteSession")}
          question={t("session.deleteConfirmQuestion")}
          confirmLabel={t("session.deleteConfirmLabel")}
          cancelLabel={t("session.keep")}
          onConfirm={() => void onDelete()}
        />
        {deleteError != null && (
          <p role="status" className="text-secondary text-reading-stale">
            {deleteError}
          </p>
        )}
      </div>
    </section>
  );
}
