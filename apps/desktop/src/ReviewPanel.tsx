import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { contentLanguage } from "./contentLanguage";
import { useI18n, type MessageDescriptor } from "./i18n";
import { AlertIcon, CheckIcon } from "./Icons";
import type { JobSummary } from "./jobModel";
import {
  bandLabel,
  parseVerifyBlock,
  resynthesisBlockReason,
  reviewRows,
  scoredNothingNotice,
  uncheckedNotice,
  verifySummary,
  werLabel,
  type SegmentSummary,
} from "./verifyModel";
import { commandErrorText } from "./commandError";

function BandChip({
  label,
  tone,
}: {
  label: React.ReactNode;
  tone: "warn" | "muted";
}) {
  return (
    <span
      className={`shrink-0 rounded px-1.5 py-0.5 font-mono text-label tracking-wide uppercase ${
        tone === "warn"
          ? "bg-wash-yellow text-wash-yellow-ink"
          : "bg-ink-800 text-ink-400"
      }`}
    >
      {label}
    </span>
  );
}

function ReviewPanel({
  job,
  bookLanguage,
}: {
  job: JobSummary;
  bookLanguage?: string | null;
}) {
  const { t, tx } = useI18n();
  const [open, setOpen] = useState(false);
  const [segments, setSegments] = useState<SegmentSummary[]>([]);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<MessageDescriptor | null>(null);
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);
  const returnSegment = useRef<string | null>(null);
  const mutationPending = useRef(false);

  useEffect(() => {
    if (editing != null) {
      editorRef.current?.focus();
    } else if (returnSegment.current != null) {
      const button = document.getElementById(
        `review-edit-${job.id}-${returnSegment.current}`,
      );
      if (button instanceof HTMLButtonElement && !button.disabled)
        button.focus();
      else toggleRef.current?.focus();
      returnSegment.current = null;
    }
  }, [editing, job.id]);

  const block = parseVerifyBlock(job.reportJson);
  const rows = reviewRows(segments, block);
  const blockedReason = resynthesisBlockReason(job.state);
  const otherSentenceHint =
    busy != null
      ? t("review.rerecording")
      : editing != null
        ? t("review.singleEditHint")
        : null;

  const [loadState, setLoadState] = useState<
    "idle" | "loading" | "ready" | "error"
  >("idle");
  const latestLoad = useRef(0);
  const load = useCallback(() => {
    const request = ++latestLoad.current;
    setLoadState("loading");
    invoke<SegmentSummary[]>("list_reviewable_segments", { jobId: job.id })
      .then((items) => {
        if (request !== latestLoad.current) return;
        setLoadState("ready");
        setSegments(items);
        setError(null);
      })
      .catch((err) => {
        if (request !== latestLoad.current) return;
        setLoadState("error");
        setError({ key: "review.loadFailed", params: { reason: commandErrorText(err) } });
      });
  }, [job.id]);

  useEffect(() => {
    if (open) load();
  }, [open, load, job.state, job.updatedAt]);

  const send = useCallback(
    (segmentId: string, text: string | null) => {
      if (mutationPending.current || (editing != null && editing !== segmentId))
        return;
      mutationPending.current = true;
      setBusy(segmentId);
      setError(null);
      invoke("resynthesize_segment", { jobId: job.id, segmentId, text })
        .then(() => {
          setEditing((current) => (current === segmentId ? null : current));
          window.dispatchEvent(new Event("txtcaster:job-created"));
        })
        .catch((cause: unknown) =>
          setError({
            key: "review.rerecordFailed",
            params: { reason: commandErrorText(cause) },
          }),
        )
        .finally(() => {
          mutationPending.current = false;
          setBusy(null);
        });
    },
    [job.id, editing],
  );

  const summary = verifySummary(block);
  const unchecked = uncheckedNotice(block);
  const scoredNone = scoredNothingNotice(block);

  return (
    <section
      data-testid={`review-panel-${job.id}`}
      className="flex flex-col gap-2.5 border-t border-ink-800/50 pt-2.5"
    >
      <button
        type="button"
        ref={toggleRef}
        data-testid={`review-toggle-${job.id}`}
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
        className="flex items-center justify-between gap-3 text-start text-secondary text-ink-300 transition-colors hover:text-ink-100"
      >
        <span className="font-medium">{t("review.sentences")}</span>
        <span
          data-testid={`review-summary-${job.id}`}
          className={`rounded-md border px-2.5 py-0.5 text-label ${unchecked || scoredNone ? "border-wash-yellow-ink/30 bg-wash-yellow text-wash-yellow-ink" : "border-ink-800 text-ink-500"} first-letter:uppercase`}
        >
          {tx(summary.key, summary.params)}
        </span>
      </button>

      {open && (
        <div className="flex flex-col gap-2.5">
          {unchecked && (
            <p
              role="status"
              data-testid={`review-unchecked-${job.id}`}
              className="rounded-lg border border-wash-yellow-ink/20 bg-wash-yellow px-3.5 py-2.5 text-secondary text-wash-yellow-ink"
            >
              {tx(unchecked.key, unchecked.params)}
            </p>
          )}
          {scoredNone && (
            <p
              role="status"
              data-testid={`review-scored-nothing-${job.id}`}
              className="rounded-lg border border-wash-yellow-ink/20 bg-wash-yellow px-3.5 py-2.5 text-secondary text-wash-yellow-ink"
            >
              {tx(scoredNone.key, scoredNone.params)}
            </p>
          )}
          {blockedReason && (
            <p
              data-testid={`review-blocked-${job.id}`}
              className="rounded-lg border border-ink-800 bg-ink-900 px-3.5 py-2.5 text-secondary text-ink-300"
            >
              {tx(blockedReason.key, blockedReason.params)}
            </p>
          )}
          {loadState === "loading" && (
            <p role="status" className="text-secondary text-ink-400">
              {t("review.loading")}
            </p>
          )}
          {error && (
            <div className="flex flex-col gap-2">
              <p
                role="alert"
                data-testid={`review-error-${job.id}`}
                className="rounded-lg border border-wash-red-ink/20 bg-wash-red px-3.5 py-2.5 text-secondary text-wash-red-ink"
              >
                {tx(error.key, error.params)}
              </p>
              {loadState === "error" && (
                <button
                  type="button"
                  onClick={() => {
                    load();
                    toggleRef.current?.focus();
                  }}
                  className="self-start rounded-lg border border-ink-700 px-3 py-1.5 text-label text-ink-200"
                >
                  {t("review.retry")}
                </button>
              )}
            </div>
          )}
          {rows.length === 0 ? (
            unchecked || loadState !== "ready" ? null : (
              <p
                data-testid={`review-empty-${job.id}`}
                className="flex items-center gap-2 text-secondary text-ink-400"
              >
                <CheckIcon className="h-3.5 w-3.5 text-wash-green-ink" />
                {t("review.empty")}
              </p>
            )
          ) : (
            <ul className="flex flex-col gap-2">
              {rows.map((row) => {
                const isEditing = editing === row.segment.id;
                const working = busy === row.segment.id;
                return (
                  <li
                    key={row.segment.id}
                    data-testid={`review-row-${row.segment.id}`}
                    className="flex flex-col gap-2 rounded-xl border border-ink-800 bg-ink-950 px-3.5 py-3"
                  >
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-mono text-label text-ink-500">
                        #{row.segment.idx + 1}
                      </span>
                      {row.band ? (
                        <BandChip
                          label={tx(bandLabel(row.band).key)}
                          tone={row.band === "ok" ? "muted" : "warn"}
                        />
                      ) : (
                        <BandChip label={t("review.edited")} tone="muted" />
                      )}
                      <span
                        data-testid={`review-wer-${row.segment.id}`}
                        className="font-mono text-label text-ink-500"
                      >
                        {tx(werLabel(row.wer).key, werLabel(row.wer).params)}
                      </span>
                      {row.referenceWords != null && (
                        <span className="font-mono text-label text-ink-500">
                          {tx("review.words", { count: row.referenceWords })}
                        </span>
                      )}
                    </div>

                    {isEditing ? (
                      <div className="flex flex-col gap-1.5">
                        <label
                          htmlFor={`review-editor-${job.id}-${row.segment.id}`}
                          className="text-label font-medium text-ink-300"
                        >
                          {tx("review.editSentence", {
                            index: row.segment.idx + 1,
                          })}
                        </label>
                        {busy == null && (
                          <p
                            id={`review-hint-${job.id}-${row.segment.id}`}
                            className="text-label text-ink-400"
                          >
                            {t("review.singleEditHint")}
                          </p>
                        )}
                        <textarea
                          {...contentLanguage(bookLanguage)}
                          ref={editorRef}
                          id={`review-editor-${job.id}-${row.segment.id}`}
                          data-testid={`review-input-${row.segment.id}`}
                          value={draft}
                          aria-describedby={
                            busy == null
                              ? `review-hint-${job.id}-${row.segment.id}`
                              : undefined
                          }
                          readOnly={busy != null}
                          onChange={(event) => {
                            if (!mutationPending.current)
                              setDraft(event.target.value);
                          }}
                          onKeyDown={(event) => {
                            if (
                              event.key === "Escape" &&
                              !event.nativeEvent.isComposing
                            ) {
                              event.preventDefault();
                              event.stopPropagation();
                              if (!mutationPending.current) setEditing(null);
                            }
                          }}
                          rows={3}
                          className="w-full resize-y rounded-lg border border-ink-700 bg-ink-950 px-3 py-2 text-secondary text-ink-100 outline-none focus:border-ember-500"
                        />
                      </div>
                    ) : (
                      <p
                        {...contentLanguage(bookLanguage)}
                        data-testid={`review-text-${row.segment.id}`}
                        className="text-secondary leading-relaxed text-ink-200"
                      >
                        {row.segment.text}
                      </p>
                    )}

                    <div className="flex flex-wrap items-center justify-end gap-1.5">
                      {working && !isEditing && (
                        <span role="status" className="text-label text-ink-400">
                          {t("review.rerecording")}
                        </span>
                      )}
                      {isEditing ? (
                        <>
                          <button
                            type="button"
                            data-testid={`review-cancel-${row.segment.id}`}
                            disabled={busy != null}
                            onClick={() => {
                              if (!mutationPending.current) setEditing(null);
                            }}
                            className="rounded-lg px-3 py-1.5 text-label text-ink-400 transition-colors hover:bg-ink-800 hover:text-ink-200 disabled:cursor-not-allowed disabled:opacity-40"
                          >
                            {t("review.cancel")}
                          </button>
                          <button
                            type="button"
                            data-testid={`review-save-${row.segment.id}`}
                            disabled={
                              busy != null ||
                              blockedReason != null ||
                              draft.trim().length === 0 ||
                              draft.trim() === row.segment.text
                            }
                            onClick={() => send(row.segment.id, draft.trim())}
                            className="rounded-lg bg-ink-50 px-3 py-1.5 text-label font-semibold text-ink-950 transition-colors hover:bg-ink-150 disabled:cursor-not-allowed disabled:opacity-40"
                          >
                            {working
                              ? t("review.rerecording")
                              : t("review.saveAndRerecord")}
                          </button>
                        </>
                      ) : (
                        <>
                          {row.segment.edited && (
                            <button
                              type="button"
                              data-testid={`review-revert-${row.segment.id}`}
                              disabled={
                                busy != null ||
                                editing != null ||
                                blockedReason != null
                              }
                              title={
                                (blockedReason
                                  ? t(blockedReason.key, blockedReason.params)
                                  : null) ??
                                otherSentenceHint ??
                                t("review.restoreOriginal")
                              }
                              onClick={() => send(row.segment.id, null)}
                              className="rounded-lg px-3 py-1.5 text-label text-ink-300 transition-colors hover:bg-ink-800 hover:text-ink-100 disabled:cursor-not-allowed disabled:opacity-40"
                            >
                              {t("review.revert")}
                            </button>
                          )}
                          <button
                            type="button"
                            id={`review-edit-${job.id}-${row.segment.id}`}
                            data-testid={`review-edit-${row.segment.id}`}
                            disabled={
                              busy != null ||
                              editing != null ||
                              blockedReason != null
                            }
                            title={
                              (blockedReason
                                ? t(blockedReason.key, blockedReason.params)
                                : null) ??
                              otherSentenceHint ??
                              t("review.editHint")
                            }
                            onClick={() => {
                              returnSegment.current = row.segment.id;
                              setDraft(row.segment.text);
                              setEditing(row.segment.id);
                            }}
                            className="rounded-lg bg-ink-800 px-3 py-1.5 text-label font-medium text-ink-200 transition-colors hover:bg-ink-700 disabled:cursor-not-allowed disabled:opacity-40"
                          >
                            {t("review.edit")}
                          </button>
                        </>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
          {block?.enabled === false && (
            <p className="flex items-center gap-2 text-label text-ink-500">
              <AlertIcon className="h-3.5 w-3.5" />
              {t("review.checkOffHint")}
            </p>
          )}
        </div>
      )}
    </section>
  );
}

export default ReviewPanel;
