import { invoke } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";

import { useI18n } from "./i18n";

export interface StartupFailure {
  code:
    "alreadyRunning" | "databaseDamaged" | "diskFull" | "noAccess" | "unknown";
  detail: string;
  dataDir: string | null;
}

function StartupFailureScreen({ failure }: { failure: StartupFailure }) {
  const { t } = useI18n();
  return (
    <main
      data-testid="startup-failure"
      className="flex h-screen items-center justify-center bg-ink-950 p-6 text-ink-200"
    >
      <div className="w-full max-w-lg">
        <h1 className="font-serif text-title font-semibold tracking-tight text-ink-50">
          {t(`startup.title.${failure.code}`)}
        </h1>
        <p className="mt-3 text-body text-ink-300">
          {t(`startup.body.${failure.code}`)}
        </p>
        <details className="mt-4 text-secondary text-ink-400">
          <summary className="cursor-pointer">{t("startup.details")}</summary>
          <p
            dir="ltr"
            className="mt-2 font-mono text-label break-words select-text"
          >
            {failure.detail}
          </p>
          {failure.dataDir != null && (
            <p
              dir="ltr"
              className="mt-1 font-mono text-label break-words select-text"
            >
              {failure.dataDir}
            </p>
          )}
        </details>
        <div className="mt-6 flex flex-wrap gap-3">
          {failure.dataDir != null && (
            <button
              type="button"
              data-testid="startup-open-folder"
              onClick={() =>
                void revealItemInDir(failure.dataDir!).catch(console.error)
              }
              className="rounded-lg border border-ink-700 px-4 py-2 text-secondary font-medium text-ink-200 transition-colors hover:bg-ink-850"
            >
              {t("startup.openFolder")}
            </button>
          )}
          <button
            type="button"
            data-testid="startup-quit"
            onClick={() => void invoke("startup_quit").catch(console.error)}
            data-variant="solid"
            className="ui-btn"
          >
            {t("startup.quit")}
          </button>
        </div>
      </div>
    </main>
  );
}

export default StartupFailureScreen;
