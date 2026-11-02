import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { useI18n, type MessageDescriptor } from "./i18n";
import { CheckIcon, CloseIcon } from "./Icons";
import { useDialogFocus } from "./useDialogFocus";
import {
  formatAudioDate,
  formatAudioSize,
  type AudioShelfItem,
} from "./audioShelfModel";
import {
  applySnapshot,
  canShowQr,
  castRowNote,
  doneMessage,
  failed,
  firewallHintDue,
  initialState,
  keepSending,
  needsStopConfirm,
  pairAnother,
  plainError,
  progressPercent,
  qrExpiryDue,
  qrVisible,
  requestStop,
  safetyCode,
  sentCount,
  serving,
  starting,
  statusLabel,
  toggleSelected,
  type LanSendSnapshot,
  type LanSendStartResult,
  type LanSendState,
} from "./lanSendModel";
import { commandErrorText } from "./commandError";

export default function SendToPhoneModal({
  open: isOpen,
  audios,
  onClose,
}: {
  open: boolean;
  audios: AudioShelfItem[];
  onClose: () => void;
}) {
  const { locale, t, tx } = useI18n();
  const message = (value: MessageDescriptor) => tx(value.key, value.params);
  const rowNote = (cast: Parameters<typeof castRowNote>[0]) => {
    const note = castRowNote(cast);
    return note
      ? message(note)
      : tx("lan.progress", { percent: progressPercent(cast) });
  };
  const [state, setState] = useState<LanSendState>(() =>
    initialState(audios.map((audio) => audio.id)),
  );
  const [now, setNow] = useState(() => Date.now());
  const dialogRef = useRef<HTMLDivElement>(null);

  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    return () => {
      if (
        stateRef.current.step === "serving" ||
        stateRef.current.step === "done"
      ) {
        void invoke("lan_send_stop").catch(() => {});
      }
    };
  }, []);

  const idsKey = audios.map((audio) => audio.id).join(",");
  useEffect(() => {
    if (!isOpen) return;
    if (stateRef.current.step === "serving") return;
    setState(initialState(idsKey.length > 0 ? idsKey.split(",") : []));
  }, [isOpen, idsKey]);

  useEffect(() => {
    if (!isOpen) return;
    const unlisten = listen<LanSendSnapshot>("lan-send-progress", (event) => {
      setState((prev) => applySnapshot(prev, event.payload));
    });
    const tick = window.setInterval(() => {
      setNow(Date.now());
      invoke<LanSendSnapshot | null>("lan_send_status")
        .then((snapshot) => {
          if (snapshot != null) {
            setState((prev) => applySnapshot(prev, snapshot));
          }
        })
        .catch(() => {});
    }, 1_000);
    return () => {
      window.clearInterval(tick);
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, [isOpen]);

  const showQr = useCallback(async (exportIds: string[], repair: boolean) => {
    setState(starting);
    try {
      const result = await invoke<LanSendStartResult>("lan_send_start", {
        exportIds,
      });
      setState((prev) =>
        repair
          ? pairAnother(prev, result, Date.now())
          : serving(prev, result, Date.now()),
      );
    } catch (err) {
      setState((prev) => failed(prev, commandErrorText(err)));
    }
  }, []);

  // Secret TTL: the 1 s tick above advances `now`; when the QR on screen has
  // outlived the desktop's TTL, mint a fresh one through the same path as the
  // manual "New QR code" button. `starting` flips busy, so it fires once.
  useEffect(() => {
    if (!isOpen) return;
    if (qrExpiryDue(state, now)) {
      void showQr(state.selected, true);
    }
  }, [isOpen, state, now, showQr]);

  const stopAndClose = useCallback(() => {
    void invoke("lan_send_stop").catch(() => {});
    onClose();
  }, [onClose]);

  const requestClose = useCallback(() => {
    if (needsStopConfirm(state)) {
      setState(requestStop);
    } else {
      stopAndClose();
    }
  }, [state, stopAndClose]);

  useDialogFocus(dialogRef, isOpen, { onEscape: requestClose });

  if (!isOpen) return null;

  const snapshot = state.snapshot;
  const rows = snapshot?.casts ?? [];

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-ink-950/80 p-4 backdrop-blur-sm"
      data-testid="send-to-phone-modal"
      data-modal-layer=""
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("lan.title")}
        tabIndex={-1}
        className="animate-modal-in flex max-h-[85vh] w-full max-w-lg flex-col rounded-2xl border border-ink-700 bg-ink-950 shadow-overlay outline-none"
      >
        <div className="flex items-start justify-between gap-4 px-6 pt-6">
          <div className="flex flex-col gap-1">
            <p className="text-label font-semibold tracking-[0.16em] text-ember-400/90 uppercase">
              {t("lan.sameNetwork")}
            </p>
            <h2 className="font-serif text-title font-semibold tracking-tight text-ink-50">
              {t("lan.title")}
            </h2>
          </div>
          <button
            type="button"
            aria-label={t("lan.close")}
            data-testid="lan-send-close"
            onClick={requestClose}
            className="text-ink-500 transition-colors hover:text-ink-200"
          >
            <CloseIcon />
          </button>
        </div>

        <div className="flex-1 space-y-4 overflow-y-auto px-6 py-4">
          {state.step === "pick" && (
            <>
              <p className="text-body text-ink-400">{t("lan.pick")}</p>
              {audios.length === 0 && (
                <p className="text-body text-ink-400">{t("lan.empty")}</p>
              )}
              <ul className="space-y-2">
                {audios.map((audio) => (
                  <li key={audio.id}>
                    <label
                      className="flex cursor-pointer items-center gap-3 rounded-lg border border-ink-800 bg-ink-900 px-3 py-2"
                      data-testid={`lan-send-pick-${audio.id}`}
                    >
                      <input
                        type="checkbox"
                        checked={state.selected.includes(audio.id)}
                        onChange={() =>
                          setState((prev) => toggleSelected(prev, audio.id))
                        }
                        className="h-4 w-4 accent-ember-500"
                      />
                      <span className="min-w-0 flex-1">
                        <bdi className="block truncate text-body text-ink-100">
                          {audio.title}
                        </bdi>
                        <span className="block text-secondary text-ink-500">
                          {[
                            formatAudioSize(audio.bytes),
                            formatAudioDate(audio.createdAt, locale),
                          ]
                            .filter((part) => part !== "")
                            .join(" · ")}
                        </span>
                      </span>
                      {audio.stale && (
                        <span className="rounded-md bg-wash-yellow px-1.5 py-1 text-label leading-snug text-wash-yellow-ink">
                          {t("lan.stale")}
                        </span>
                      )}
                    </label>
                  </li>
                ))}
              </ul>
              {audios.some((audio) => audio.stale) && (
                <p className="text-secondary text-wash-yellow-ink">
                  {t("lan.staleHint")}
                </p>
              )}
            </>
          )}

          {state.step === "serving" && state.start != null && (
            <>
              {qrVisible(state) ? (
                <div className="flex flex-col items-center gap-4">
                  <div className="rounded-[2rem] border border-ink-700 bg-ink-900 p-3 pt-2 shadow-overlay">
                    <div className="mx-auto mb-2 h-1.5 w-12 rounded-full bg-ink-700" />
                    <div
                      data-testid="lan-send-qr"
                      style={{ width: "min(55vw, 250px)", minWidth: 190 }}
                      className="rounded-[1.25rem] bg-white p-3 [&_svg]:h-auto [&_svg]:w-full"
                      dangerouslySetInnerHTML={{ __html: state.start.qrSvg }}
                    />
                  </div>
                  <p className="max-w-sm text-center text-body text-ink-200">
                    {t("lan.scan")}
                  </p>
                  <p className="flex flex-wrap items-center justify-center gap-x-2 gap-y-1.5 text-center text-secondary text-ink-500">
                    <bdi dir="ltr" className="break-all">
                      {state.start.hosts.join(" · ")}
                    </bdi>
                    <span>
                      {tx("lan.safety", {
                        code: safetyCode(state.start.fingerprint),
                      })}
                    </span>
                  </p>
                </div>
              ) : (
                <div className="space-y-2" data-testid="lan-send-qr-consumed">
                  <p className="text-body text-ink-400">{t("lan.consumed")}</p>
                  <button
                    type="button"
                    data-testid="lan-send-new-qr"
                    onClick={() => void showQr(state.selected, true)}
                    className="rounded-lg border border-ink-800 px-3 py-2 text-body text-ink-200"
                  >
                    {t("lan.newQr")}
                  </button>
                </div>
              )}
              <p
                data-testid="lan-send-status"
                className="mx-auto flex w-fit items-center gap-2 rounded-md border border-ink-800 bg-ink-900 px-3.5 py-1.5 text-secondary text-ink-300"
              >
                <span
                  aria-hidden="true"
                  className="inline-block h-2 w-2 animate-pulse rounded-full bg-ember-500"
                />
                {message(statusLabel(snapshot))}
              </p>
              {snapshot?.pairLocked === true && (
                <div className="space-y-2" data-testid="lan-send-pair-locked">
                  <p className="text-body text-ember-300">{t("lan.locked")}</p>
                  <button
                    type="button"
                    onClick={() => void showQr(state.selected, true)}
                    className="rounded-lg border border-ink-800 px-3 py-2 text-body text-ink-200"
                  >
                    {t("lan.newQr")}
                  </button>
                </div>
              )}
              {firewallHintDue(state, now) && (
                <p
                  data-testid="lan-send-firewall-hint"
                  className="text-secondary text-ink-400"
                >
                  {t("lan.firewall")}
                </p>
              )}
              {rows.length > 0 && (
                <ul className="space-y-2">
                  {rows.map((cast) => (
                    <li
                      key={cast.castId}
                      data-testid={`lan-send-progress-${cast.castId}`}
                      className="space-y-1 rounded-lg border border-ink-800 bg-ink-900 px-3 py-2"
                    >
                      <div className="flex flex-wrap items-center justify-between gap-3">
                        <bdi className="min-w-0 flex-1 truncate text-body text-ink-100">
                          {cast.title}
                        </bdi>
                        <span className="min-w-0 max-w-full text-secondary break-words text-ink-400 [overflow-wrap:anywhere]">
                          {rowNote(cast)}
                        </span>
                      </div>
                      {!cast.missing && (
                        <div className="h-1.5 overflow-hidden rounded-full bg-ink-800">
                          <div
                            className="h-full rounded-full bg-ember-500 transition-all"
                            style={{ width: `${progressPercent(cast)}%` }}
                          />
                        </div>
                      )}
                    </li>
                  ))}
                </ul>
              )}
              {state.confirmingStop && (
                <div
                  className="space-y-2 rounded-lg border border-ember-500/30 bg-ember-500/[0.07] px-3 py-2"
                  data-testid="lan-send-stop-confirm"
                >
                  <p className="text-body text-ember-100">
                    {t("lan.stopConfirm")}
                  </p>
                  <div className="flex gap-2">
                    <button
                      type="button"
                      data-testid="lan-send-stop-confirmed"
                      onClick={stopAndClose}
                      className="rounded-lg bg-ink-50 px-3 py-2 text-body font-medium text-ink-950 hover:bg-ink-150"
                    >
                      {t("lan.stop")}
                    </button>
                    <button
                      type="button"
                      data-testid="lan-send-keep"
                      onClick={() => setState(keepSending)}
                      className="rounded-lg border border-ink-800 px-3 py-2 text-body text-ink-200"
                    >
                      {t("lan.keep")}
                    </button>
                  </div>
                </div>
              )}
            </>
          )}

          {state.step === "done" && (
            <>
              <div className="flex flex-col items-center gap-3 py-2 text-center">
                <span className="flex h-12 w-12 items-center justify-center rounded-full bg-wash-green text-wash-green-ink">
                  <CheckIcon className="h-6 w-6" />
                </span>
                <p
                  className="text-title font-serif font-semibold text-ink-50"
                  data-testid="lan-send-done"
                >
                  {message(doneMessage(sentCount(snapshot)))}
                </p>
              </div>
              {rows.length > 0 && (
                <ul className="space-y-1.5">
                  {rows.map((cast) => (
                    <li
                      key={cast.castId}
                      className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-ink-800 bg-ink-900 px-3.5 py-2.5"
                    >
                      <bdi className="min-w-0 flex-1 truncate text-body text-ink-100">
                        {cast.title}
                      </bdi>
                      <span className="min-w-0 max-w-full text-secondary break-words text-ink-400 [overflow-wrap:anywhere]">
                        {cast.missing ? null : (
                          <CheckIcon className="me-1.5 inline h-4 w-4 text-wash-green-ink" />
                        )}
                        {rowNote(cast)}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
              {state.reQr && state.start != null && (
                <>
                  <div
                    data-testid="lan-send-qr"
                    style={{ width: "min(60vw, 280px)", minWidth: 200 }}
                    className="mx-auto rounded-lg bg-white p-2 [&_svg]:h-auto [&_svg]:w-full"
                    dangerouslySetInnerHTML={{ __html: state.start.qrSvg }}
                  />
                  <p className="text-center text-body text-ink-200">
                    {t("lan.scan")}
                  </p>
                  <p className="break-words text-center text-secondary text-ink-500">
                    <bdi dir="ltr">{state.start.hosts.join(" · ")}</bdi> ·{" "}
                    {tx("lan.safety", {
                      code: safetyCode(state.start.fingerprint),
                    })}
                  </p>
                </>
              )}
            </>
          )}

          {state.step === "error" && state.error != null && (
            <p
              className="text-body text-ember-300"
              data-testid="lan-send-error"
            >
              {message(plainError(state.error))}
            </p>
          )}
        </div>

        <div className="flex flex-wrap justify-end gap-2 border-t border-ink-800 px-6 py-4">
          {state.step === "pick" && (
            <>
              <button
                type="button"
                onClick={stopAndClose}
                className="rounded-lg border border-ink-800 px-3 py-2 text-body text-ink-200"
              >
                {t("lan.cancel")}
              </button>
              <button
                type="button"
                data-testid="lan-send-show-qr"
                onClick={() => void showQr(state.selected, false)}
                disabled={!canShowQr(state)}
                className="rounded-lg bg-ink-50 px-3 py-2 text-body font-medium text-ink-950 transition-colors hover:bg-ink-150 disabled:opacity-50"
              >
                {state.busy ? t("lan.starting") : t("lan.showQr")}
              </button>
            </>
          )}
          {state.step === "serving" && (
            <button
              type="button"
              data-testid="lan-send-stop"
              onClick={requestClose}
              className="rounded-lg border border-ink-800 px-3 py-2 text-body text-ink-200"
            >
              {t("lan.stop")}
            </button>
          )}
          {state.step === "done" && (
            <>
              <button
                type="button"
                data-testid="lan-send-pair-another"
                onClick={() => void showQr(state.selected, true)}
                className="rounded-lg border border-ink-800 px-3 py-2 text-body text-ink-200"
              >
                {t("lan.pairAnother")}
              </button>
              <button
                type="button"
                onClick={stopAndClose}
                className="rounded-lg bg-ink-50 px-3 py-2 text-body font-medium text-ink-950 hover:bg-ink-150"
              >
                {t("lan.close")}
              </button>
            </>
          )}
          {state.step === "error" && (
            <button
              type="button"
              onClick={stopAndClose}
              className="rounded-lg bg-ink-50 px-3 py-2 text-body font-medium text-ink-950 hover:bg-ink-150"
            >
              {t("lan.close")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
