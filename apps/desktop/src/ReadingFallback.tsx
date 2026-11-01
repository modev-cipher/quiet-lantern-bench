import { contentLanguage } from "./contentLanguage";
import type { ReadingSegment } from "./readingModel";
import { useI18n } from "./i18n";

interface ReadingFallbackProps {
  segment: ReadingSegment;
  language?: string | null;
  onRepair: (() => void) | null;
}

function ReadingFallback({
  segment,
  language,
  onRepair,
}: ReadingFallbackProps) {
  const { t } = useI18n();
  const stale = segment.provenance === "stale";

  return (
    <div className="flex flex-col gap-3">
      <p
        {...contentLanguage(language)}
        data-testid="reading-fallback-text"
        data-provenance={segment.provenance}
        className="font-serif text-2xl leading-relaxed tracking-tight text-reading-fallback"
      >
        {segment.text}
      </p>
      <div className="flex items-center gap-3">
        <p
          data-testid="reading-fallback-notice"
          className="text-label leading-snug text-reading-stale"
        >
          {stale ? t("reader.staleTimings") : t("reader.neverAligned")}
        </p>
        {stale && onRepair && (
          <button
            type="button"
            data-testid="reading-fallback-repair"
            onClick={onRepair}
            className="shrink-0 rounded-md border border-ink-700 px-3 py-1 font-mono text-label tracking-wide text-ink-300 uppercase transition-colors hover:border-ember-500 hover:text-ember-300 active:scale-[0.98]"
          >
            {t("reader.reviewChapter")}
          </button>
        )}
      </div>
    </div>
  );
}

export default ReadingFallback;
