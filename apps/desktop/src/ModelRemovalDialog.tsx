import { useRef } from "react";
import { useDialogFocus } from "./useDialogFocus";
import { useI18n } from "./i18n";
import { AlertIcon, CloseIcon } from "./Icons";
import { formatBytes } from "./storageSettingsModel";

export default function ModelRemovalDialog({
  model,
  isDeleting,
  onClose,
  onConfirm,
  availability,
}: {
  model: { modelId: string; name: string; diskBytes: number } | null;
  isDeleting: boolean;
  onClose: () => void;
  onConfirm: () => void;
  availability?: "checking" | "unavailable";
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const { t, tx } = useI18n();

  useDialogFocus(dialogRef, Boolean(model), {
    initialFocusSelector: "[data-testid='cancel-delete-model-button']",
    onEscape: () => {
      if (!isDeleting) onClose();
    },
  });

  if (!model) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      data-testid="delete-model-modal"
      data-modal-layer=""
    >
      <button
        type="button"
        aria-label={t("settings.cancel")}
        onClick={isDeleting ? undefined : onClose}
        className="animate-fade-in absolute inset-0 cursor-default bg-ink-950/85 backdrop-blur-sm"
      />
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("settings.deleteModelModalTitle")}
        tabIndex={-1}
        className="animate-modal-in shadow-overlay relative flex max-h-[calc(100dvh-2rem)] w-full max-w-lg flex-col overflow-y-auto rounded-2xl border border-red-800/80 bg-ink-900 p-6 outline-none"
      >
        <div className="flex items-start justify-between gap-4">
          <div className="flex items-center gap-3">
            <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-red-900/30 text-red-400">
              <AlertIcon className="h-6 w-6" />
            </span>
            <div className="min-w-0">
              <h2 className="font-serif text-title font-semibold tracking-tight text-ink-50">
                {t("settings.deleteModelModalTitle")}
              </h2>
            </div>
          </div>
          <button
            type="button"
            aria-label={t("settings.cancel")}
            data-testid="delete-model-modal-close"
            disabled={isDeleting}
            onClick={onClose}
            className="rounded-lg p-1 text-ink-500 transition-colors hover:bg-ink-850 hover:text-ink-200 disabled:opacity-50"
          >
            <CloseIcon className="h-5 w-5" />
          </button>
        </div>

        <div className="mt-4 rounded-xl border border-red-900/40 bg-red-950/20 p-4 text-secondary text-ink-300 [overflow-wrap:anywhere]">
          <p>
            {tx("settings.deleteModelModalWarning", {
              name: model.name || model.modelId,
              size: formatBytes(model.diskBytes),
            })}
          </p>
        </div>

        {availability != null && (
          <p role="status" className="mt-3 text-secondary text-ink-400">
            {t(
              availability === "checking"
                ? "models.refreshing"
                : "models.removal.unavailable",
            )}
          </p>
        )}
        <div className="mt-6 flex items-center justify-end gap-3">
          <button
            type="button"
            data-testid="cancel-delete-model-button"
            disabled={isDeleting}
            onClick={onClose}
            className="rounded-lg border border-ink-700 px-4 py-2 text-secondary font-medium text-ink-200 transition-colors hover:border-ink-600 disabled:opacity-50"
          >
            {t("settings.deleteModelCancel")}
          </button>
          <button
            type="button"
            data-testid="confirm-delete-model-button"
            disabled={isDeleting || availability != null}
            onClick={onConfirm}
            className="rounded-lg bg-red-600 px-4 py-2 text-secondary font-medium text-white transition-colors hover:bg-red-500 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {isDeleting
              ? t("settings.deletingModel")
              : t("settings.deleteModelConfirm")}
          </button>
        </div>
      </div>
    </div>
  );
}
