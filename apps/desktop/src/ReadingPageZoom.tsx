import { PAGE_ZOOM_MAX, PAGE_ZOOM_MIN, type PageFit } from "./readingPageFit";
import { useI18n, type MessageKey } from "./i18n";

interface ReadingPageZoomProps {
  fit: PageFit;
  zoom: number;
  onFit: (fit: PageFit) => void;
  onZoom: (zoom: number) => void;
  onZoomIn: () => void;
  onZoomOut: () => void;
}

const PILL =
  "shrink-0 rounded-md border px-2.5 py-1 font-mono text-label tracking-wide uppercase transition-colors active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-40";
const IDLE = "border-ink-700 text-ink-300 hover:border-ember-500 hover:text-ember-300";
const ACTIVE = "border-ember-500 bg-ember-500/15 text-ember-200";

const STEP =
  "flex h-7 w-7 shrink-0 items-center justify-center rounded-full border border-ink-700 font-mono text-xs text-ink-300 transition-colors hover:border-ember-500 hover:text-ember-300 active:scale-[0.96] disabled:cursor-not-allowed disabled:opacity-40";

const FIT_KEY: Record<PageFit, MessageKey> = {
  width: "reader.fitWidth",
  page: "reader.fitPage",
};

function ReadingPageZoom({
  fit,
  zoom,
  onFit,
  onZoom,
  onZoomIn,
  onZoomOut,
}: ReadingPageZoomProps) {
  const { t } = useI18n();
  const percent = Math.round(zoom * 100);

  return (
    <div
      role="group"
      aria-label={t("reader.pageZoom")}
      data-testid="reading-page-zoom"
      data-fit={fit}
      data-zoom={zoom}
      className="flex items-center gap-1.5"
    >
      {(["width", "page"] as const).map((option) => (
        <button
          key={option}
          type="button"
          data-testid={`reading-fit-${option}`}
          aria-pressed={fit === option}
          onClick={() => onFit(option)}
          className={`${PILL} ${fit === option ? ACTIVE : IDLE}`}
        >
          {t(FIT_KEY[option])}
        </button>
      ))}
      <button
        type="button"
        data-testid="reading-zoom-out"
        aria-label={t("reader.zoomOut")}
        disabled={zoom <= PAGE_ZOOM_MIN}
        onClick={onZoomOut}
        className={STEP}
      >
        −
      </button>
      <button
        type="button"
        data-testid="reading-zoom-reset"
        aria-label={t("reader.resetZoom", { percent })}
        onClick={() => onZoom(1)}
        className="shrink-0 rounded-md px-1 font-mono text-label tracking-wide text-ink-400 tabular-nums uppercase transition-colors hover:text-ember-300"
      >
        {percent}%
      </button>
      <button
        type="button"
        data-testid="reading-zoom-in"
        aria-label={t("reader.zoomIn")}
        disabled={zoom >= PAGE_ZOOM_MAX}
        onClick={onZoomIn}
        className={STEP}
      >
        +
      </button>
    </div>
  );
}

export default ReadingPageZoom;
