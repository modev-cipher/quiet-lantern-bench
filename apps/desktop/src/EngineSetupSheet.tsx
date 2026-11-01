import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import {
  LITE_VARIANT,
  fullVariantBody,
  installErrorKey,
  installOptions,
  progressFraction,
  type EngineInstallInfo,
  type InstallEvent,
  type InstallProgress,
  type VariantOffer,
} from "./engineSetupModel";
import { closeEngineSetup, useEngineSetupRequest } from "./engineSetupStore";
import { useI18n } from "./i18n";
import { formatBytes } from "./modelCatalogModel";
import { useDialogFocus } from "./useDialogFocus";

function EngineSetupSheet() {
  const request = useEngineSetupRequest();
  const { t } = useI18n();
  const dialogRef = useRef<HTMLDivElement>(null);
  const [info, setInfo] = useState<EngineInstallInfo | null>(null);
  const [progress, setProgress] = useState<InstallProgress | null>(null);
  const [errorKey, setErrorKey] = useState<ReturnType<
    typeof installErrorKey
  > | null>(null);
  const [fullVariant, setFullVariant] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const onReady = useRef<(() => void) | null>(null);
  onReady.current = request?.onReady ?? onReady.current;
  const open = request != null;

  const load = useCallback(async () => {
    setLoading(true);
    setErrorKey(null);
    try {
      const next = await invoke<EngineInstallInfo>("engine_install_info");
      setInfo(next);
      setFullVariant((current) => current ?? next.recommended.variant);
      if (next.offerError != null)
        setErrorKey(installErrorKey(next.offerError));
    } catch (raw) {
      setErrorKey(installErrorKey(String(raw)));
    } finally {
      setLoading(false);
    }
  }, []);

  const installing = useRef(false);
  installing.current = progress != null;
  useEffect(() => {
    if (open && !installing.current) void load();
  }, [open, load]);

  useEffect(() => {
    const unlisten = listen<InstallEvent>("engine-install", (event) => {
      const payload = event.payload;
      if (payload.phase === "installed") {
        setProgress(null);
        const resume = onReady.current;
        onReady.current = null;
        closeEngineSetup();
        resume?.();
      } else if (payload.phase === "failed") {
        setProgress(null);
        if (payload.code !== "ERR_ENGINE_CANCELLED")
          setErrorKey(installErrorKey(payload.code));
      } else {
        setProgress(payload);
      }
    });
    return () => {
      unlisten.then((stop) => stop()).catch(console.error);
    };
  }, []);

  const dismiss = useCallback(() => {
    if (progress == null) onReady.current = null;
    closeEngineSetup();
  }, [progress]);

  useDialogFocus(dialogRef, open, { onEscape: dismiss });

  const install = useCallback(async (variant: string) => {
    setErrorKey(null);
    try {
      setProgress({ phase: "verifying" });
      await invoke("install_engine", { variant });
    } catch (raw) {
      setProgress(null);
      setErrorKey(installErrorKey(String(raw)));
    }
  }, []);

  const cancel = useCallback(() => {
    invoke<boolean>("cancel_engine_install").catch(console.error);
  }, []);

  const options = useMemo(() => {
    if (info == null || request == null) return [];
    return installOptions(
      info,
      request.need,
      fullVariant ?? info.recommended.variant,
    );
  }, [info, request, fullVariant]);

  if (request == null) return null;

  const needsUpdate =
    info?.installed != null && info.installed.engineApi !== info.requiredApi;
  const body = needsUpdate
    ? t("engine.setup.bodyUpdate")
    : request.need === "documents"
      ? t("engine.setup.bodyDocuments")
      : t("engine.setup.bodyLocalAi");
  const alternateFull =
    info?.offered.some((offer) => offer.variant === "cuda13") === true &&
    info.offered.some((offer) => offer.variant === "cpu")
      ? (fullVariant ?? info.recommended.variant) === "cpu"
        ? "cuda13"
        : "cpu"
      : null;

  const optionBody = (offer: VariantOffer): string => {
    if (offer.variant === LITE_VARIANT) return t("engine.setup.liteBody");
    const choice = info!.recommended;
    return t(fullVariantBody(choice, offer.variant), {
      gpu: choice.gpuName ?? "",
    });
  };

  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center p-4"
      data-testid="engine-setup-sheet"
      data-modal-layer=""
    >
      <button
        type="button"
        aria-label={t("engine.setup.close")}
        onClick={dismiss}
        className="animate-fade-in absolute inset-0 cursor-default bg-ink-950/85 backdrop-blur-sm"
      />
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("engine.setup.title")}
        tabIndex={-1}
        className="animate-modal-in shadow-overlay relative flex max-h-full w-full max-w-lg flex-col overflow-y-auto rounded-2xl border border-ink-800 bg-ink-900 p-6"
      >
        <h2 className="font-serif text-title font-semibold tracking-tight text-ink-50">
          {t("engine.setup.title")}
        </h2>
        <p className="mt-2 text-secondary text-ink-400">{body}</p>

        {progress != null ? (
          <div className="mt-5" data-testid="engine-setup-progress">
            <p className="text-secondary text-ink-200" aria-live="polite">
              {progress.phase === "downloading"
                ? t("engine.setup.phase.downloading", {
                    done: formatBytes(progress.bytesDone),
                    total: formatBytes(progress.bytesTotal),
                  })
                : progress.phase === "extracting"
                  ? t("engine.setup.phase.extracting", {
                      files: progress.filesDone,
                    })
                  : t(`engine.setup.phase.${progress.phase}`)}
            </p>
            <div
              role="progressbar"
              aria-label={t("engine.setup.progressLabel")}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={
                progressFraction(progress) == null
                  ? undefined
                  : Math.round(progressFraction(progress)! * 100)
              }
              className="mt-2 h-1.5 overflow-hidden rounded-full bg-ink-800"
            >
              <div
                className="h-full rounded-full bg-ember-500 transition-[width] duration-300"
                style={{ width: `${(progressFraction(progress) ?? 1) * 100}%` }}
              />
            </div>
            <div className="mt-5 flex justify-end gap-3">
              <button
                type="button"
                data-testid="engine-setup-hide"
                onClick={dismiss}
                className="rounded-lg border border-ink-700 px-4 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-850"
              >
                {t("engine.setup.hide")}
              </button>
              {progress.phase !== "starting" && (
                <button
                  type="button"
                  data-testid="engine-setup-cancel"
                  onClick={cancel}
                  className="rounded-lg border border-ink-700 px-4 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-850"
                >
                  {t("engine.setup.cancel")}
                </button>
              )}
            </div>
          </div>
        ) : (
          <>
            {loading && (
              <p
                className="mt-5 text-secondary text-ink-400"
                data-testid="engine-setup-loading"
              >
                {t("engine.setup.loading")}
              </p>
            )}
            {errorKey != null && (
              <p
                role="alert"
                data-testid="engine-setup-error"
                className="mt-4 rounded-xl border border-red-900/40 bg-red-950/20 p-3 text-secondary text-ink-200"
              >
                {t(errorKey)}
              </p>
            )}
            {!loading && options.length > 0 && (
              <ul className="mt-5 flex flex-col gap-3">
                {options.map((offer) => {
                  const lacksSpace =
                    info?.spaceAvailableBytes != null &&
                    info.spaceAvailableBytes < offer.spaceNeededBytes;
                  return (
                    <li
                      key={offer.variant}
                      data-testid={`engine-option-${offer.variant}`}
                      className="rounded-xl border border-ink-800 p-4"
                    >
                      <p className="text-body font-medium text-ink-100">
                        {offer.variant === LITE_VARIANT
                          ? t("engine.setup.liteTitle")
                          : t("engine.setup.fullTitle")}
                      </p>
                      <p className="mt-1 text-secondary text-ink-400">
                        {optionBody(offer)}
                      </p>
                      <p className="mt-1 text-label text-ink-500">
                        {t("engine.setup.sizes", {
                          download: formatBytes(offer.downloadBytes),
                          needed: formatBytes(offer.spaceNeededBytes),
                        })}
                      </p>
                      {lacksSpace && (
                        <p className="mt-1 text-label text-red-400">
                          {t("engine.setup.notEnoughSpace", {
                            available: formatBytes(info!.spaceAvailableBytes!),
                          })}
                        </p>
                      )}
                      <div className="mt-3 flex flex-wrap items-center gap-3">
                        <button
                          type="button"
                          data-testid={`engine-install-${offer.variant}`}
                          disabled={lacksSpace}
                          onClick={() => void install(offer.variant)}
                          data-variant="solid"
                          className="ui-btn disabled:opacity-50"
                        >
                          {t("engine.setup.install")}
                        </button>
                        {offer.variant !== LITE_VARIANT &&
                          alternateFull != null && (
                            <button
                              type="button"
                              data-testid="engine-switch-build"
                              onClick={() => setFullVariant(alternateFull)}
                              className="text-secondary text-ink-400 underline-offset-2 hover:text-ink-200 hover:underline"
                            >
                              {alternateFull === "cpu"
                                ? t("engine.setup.switchCpu")
                                : t("engine.setup.switchGpu")}
                            </button>
                          )}
                      </div>
                    </li>
                  );
                })}
              </ul>
            )}
            <div className="mt-5 flex justify-end gap-3">
              {errorKey != null && (
                <button
                  type="button"
                  data-testid="engine-setup-retry"
                  onClick={() => void load()}
                  className="rounded-lg border border-ink-700 px-4 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-850"
                >
                  {t("engine.setup.retry")}
                </button>
              )}
              <button
                type="button"
                data-testid="engine-setup-not-now"
                onClick={dismiss}
                className="rounded-lg border border-ink-700 px-4 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-850"
              >
                {t("engine.setup.notNow")}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export default EngineSetupSheet;
