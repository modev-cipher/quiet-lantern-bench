import { useI18n } from "./i18n";
import { ChevronIcon } from "./Icons";

export default function AppNavigationControls({
  back,
  forward,
  onBack,
  onForward,
}: {
  back: boolean;
  forward: boolean;
  onBack: () => void;
  onForward: () => void;
}) {
  const { t } = useI18n();
  const button =
    "flex h-9 w-9 shrink-0 items-center justify-center rounded-lg text-ink-300 hover:bg-ink-800 disabled:opacity-30 disabled:hover:bg-transparent";
  return (
    <nav
      aria-label={t("nav.history")}
      data-testid="app-navigation"
      className="flex shrink-0 items-center gap-1"
    >
      <button
        type="button"
        data-testid="app-back"
        aria-label={t("nav.back")}
        title={t("nav.back")}
        disabled={!back}
        onClick={onBack}
        className={button}
      >
        <ChevronIcon className="h-4 w-4 rotate-180 rtl:rotate-0" />
      </button>
      <button
        type="button"
        data-testid="app-forward"
        aria-label={t("nav.forward")}
        title={t("nav.forward")}
        disabled={!forward}
        onClick={onForward}
        className={button}
      >
        <ChevronIcon className="h-4 w-4 rtl:rotate-180" />
      </button>
    </nav>
  );
}
