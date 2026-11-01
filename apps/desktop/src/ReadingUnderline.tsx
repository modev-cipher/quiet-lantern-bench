import { useI18n } from "./i18n";

interface ReadingUnderlineProps {
  underlineRef: React.RefObject<HTMLDivElement | null>;
}

function ReadingUnderline({ underlineRef }: ReadingUnderlineProps) {
  const { t } = useI18n();
  return (
    <div className="flex items-center gap-3">
      <span
        data-testid="reading-underline-label"
        className="shrink-0 font-mono text-label tracking-wide text-ink-400 uppercase"
      >
        {t("reader.sentenceProgress")}
      </span>
      <div
        ref={underlineRef}
        data-testid="reading-underline"
        className="h-0.5 min-w-0 flex-1 overflow-hidden rounded-full bg-reading-underline-track"
      >
        <div
          data-testid="reading-underline-fill"
          className={/* i18n-physical-ok: page-coordinate overlay */ "h-full w-full origin-left bg-reading-underline"}
          style={{ transform: "scaleX(var(--reading-fill, 0))" }}
        />
      </div>
    </div>
  );
}

export default ReadingUnderline;
