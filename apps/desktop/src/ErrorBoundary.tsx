import { Component, type ReactNode } from "react";

import { i18nFor, type I18n } from "./i18n";
import { getSnapshot as localeSnapshot } from "./i18n/localeStore";
import { reportUiCrash } from "./uiCrashDiagnostics";

type ErrorBoundaryState = { failed: boolean };

const FALLBACK_CRASHED =
  "The app hit an unexpected problem and this screen stopped. Your library and jobs are safe on disk.";
const FALLBACK_RELOAD = "Reload this screen";

function fallbackCopy(): Pick<I18n, "t"> {
  return {
    t: ((key: string) =>
      key === "shell.reloadApp" ? FALLBACK_RELOAD : FALLBACK_CRASHED) as I18n["t"],
  };
}

function copy(): Pick<I18n, "t"> {
  try {
    return i18nFor(localeSnapshot().locale);
  } catch {
    try {
      return i18nFor("en");
    } catch {
      return fallbackCopy();
    }
  }
}

export default class ErrorBoundary extends Component<
  { children: ReactNode },
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { failed: true };
  }

  componentDidCatch(error: unknown) {
    console.error("ui crashed", error);
    void reportUiCrash(error).catch(() => {
      console.error("ui crash report did not reach the backend", error);
    });
  }

  render() {
    if (!this.state.failed) {
      return this.props.children;
    }
    const { t } = copy();
    return (
      <div className="fixed inset-0 z-50 flex items-center justify-center p-6">
        <div className="paper-surface flex max-w-md flex-col items-center gap-5 rounded-2xl p-8 text-center">
          <p className="text-body text-paper-fg">{t("shell.recoveryCrashed")}</p>
          <button
            type="button"
            onClick={() => window.location.reload()}
            className="rounded-lg bg-ink-50 px-5 py-2.5 text-secondary font-semibold text-ink-950 transition-colors hover:bg-ink-150 active:scale-[0.98]"
          >
            {t("shell.reloadApp")}
          </button>
        </div>
      </div>
    );
  }
}
