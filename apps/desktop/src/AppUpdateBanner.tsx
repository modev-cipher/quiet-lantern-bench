import { bannerFor } from "./appUpdateModel";
import { useI18n } from "./i18n";
import { useAppUpdate } from "./useAppUpdate";

const primary =
  "shrink-0 rounded-lg bg-ink-50 px-3 py-1.5 text-secondary font-semibold text-ink-950 hover:bg-white";
const quiet =
  "shrink-0 rounded-lg border border-ink-800 px-3 py-1.5 text-secondary text-ink-400 hover:text-ink-100";

/** Non-modal update notice under the top bar. Renders nothing unless the
 *  updater has something actionable (UPD-1 contract rows f, g, i). */
export function AppUpdateBanner() {
  const { t } = useI18n();
  const { snapshot, install, cancel, dismiss, openRelease } = useAppUpdate();
  const banner = bannerFor(snapshot);
  if (!banner) return null;

  return (
    <div
      role="status"
      aria-label={t("update.bannerLabel")}
      data-testid="app-update-banner"
      className="flex items-center gap-4 border-b border-ink-800/80 bg-ink-900 px-6 py-2.5 text-body text-ink-200"
    >
      {banner.kind === "available" && (
        <>
          <p className="min-w-0 flex-1 truncate">
            <span className="font-medium text-ink-50">
              {banner.notes
                ? t("update.readyWithNotes", {
                    version: banner.version,
                    notes: banner.notes,
                  })
                : t("update.readyVersion", { version: banner.version })}
            </span>
          </p>
          <button
            type="button"
            data-testid="app-update-install"
            onClick={banner.action === "restart" ? install : openRelease}
            className={primary}
          >
            {t(banner.action === "restart" ? "update.install" : "update.get")}
          </button>
          <button
            type="button"
            data-testid="app-update-later"
            onClick={dismiss}
            className={quiet}
          >
            {t("update.later")}
          </button>
        </>
      )}
      {banner.kind === "progress" && (
        <>
          <p
            data-testid="app-update-progress"
            className="min-w-0 flex-1 truncate"
          >
            {banner.phase === "downloading"
              ? t("update.downloading", { percent: banner.percent ?? 0 })
              : banner.phase === "verifying"
                ? t("update.verifying")
                : t("update.installing")}
          </p>
          <div
            aria-hidden="true"
            className="h-1 w-32 shrink-0 overflow-hidden rounded bg-ink-800"
          >
            <div
              className="h-1 rounded bg-ink-50 transition-[width]"
              style={{ width: `${banner.percent ?? 100}%` }}
            />
          </div>
          {banner.phase === "downloading" && (
            <button
              type="button"
              data-testid="app-update-cancel"
              onClick={cancel}
              className={quiet}
            >
              {t("update.cancel")}
            </button>
          )}
        </>
      )}
      {banner.kind === "failed" && (
        <>
          <p data-testid="app-update-failed" className="min-w-0 flex-1">
            <span className="text-ink-50">
              {t("update.failed", { version: banner.currentVersion })}
            </span>{" "}
            <span className="text-ink-400">{t("update.failedHint")}</span>
          </p>
          <button
            type="button"
            data-testid="app-update-dismiss"
            onClick={dismiss}
            className={quiet}
          >
            {t("update.dismiss")}
          </button>
        </>
      )}
    </div>
  );
}
