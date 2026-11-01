import { ChevronIcon } from "./Icons";
import { useI18n } from "./i18n";
interface ReadingPageControlsProps {
  browsing: boolean;
  onReturn: () => void;
  /** Overrides the resume affordance text (teacher-mode anchoring, §11.1). */
  resumeLabel?: string;
}

const EDGE_CLASS =
  "reading-edge pointer-events-auto absolute top-1/2 flex h-16 w-10 -translate-y-1/2 items-center justify-center rounded-full text-ink-500 transition-[opacity,background-color,color] duration-200 hover:bg-ink-800/70 hover:text-ink-100 active:scale-[0.96] disabled:pointer-events-none";

export function ReadingPageEdges({
  disabled,
  onFlip,
}: {
  disabled: boolean;
  onFlip: (delta: 1 | -1) => void;
}) {
  const { t } = useI18n();
  return (
    <div
      data-testid="reading-page-edges"
      className="pointer-events-none absolute inset-0 z-10"
    >
      <button
        type="button"
        data-testid="reading-flip-back"
        aria-label={t("reader.previousPage")}
        title={t("reader.previousPage")}
        disabled={disabled}
        onClick={() => onFlip(-1)}
        className={`${EDGE_CLASS} start-2`}
      >
        <ChevronIcon className="h-5 w-5 rotate-180" />
      </button>
      <button
        type="button"
        data-testid="reading-flip-forward"
        aria-label={t("reader.nextPage")}
        title={t("reader.nextPage")}
        disabled={disabled}
        onClick={() => onFlip(1)}
        className={`${EDGE_CLASS} end-4`}
      >
        <ChevronIcon className="h-5 w-5" />
      </button>
    </div>
  );
}

function ReadingPageControls({
  browsing,
  onReturn,
  resumeLabel,
}: ReadingPageControlsProps) {
  const { t } = useI18n();
  return (
    <div
      data-testid="reading-page-controls"
      data-browsing={browsing ? "yes" : "no"}
      className="flex shrink-0 items-center justify-center empty:hidden"
    >
      {browsing && (
        <button
          type="button"
          data-testid="reading-return"
          onClick={onReturn}
          className="animate-fade-in shrink-0 rounded-full border border-ember-500/60 bg-ember-500/15 px-3.5 py-1.5 text-label font-semibold text-ember-200 transition-colors duration-200 hover:bg-ember-500/25 active:scale-[0.98]"
        >
          {resumeLabel ?? t("reader.backToNarration")}
        </button>
      )}
    </div>
  );
}

export default ReadingPageControls;
