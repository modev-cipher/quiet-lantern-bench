import { ChevronIcon } from "./Icons";
import type { ReadingChapterSummary, TimelineOrigin } from "./readingModel";
import type { ReadingMode } from "./readingPageModel";
import { useI18n } from "./i18n";
import { GlideGroup } from "./GlideThumb";

const ORIGIN_LABEL: Record<TimelineOrigin, string> = {
  measured: "measured",
  dynamic: "assembled",
  unverified: "unverified",
  legacy: "legacy",
};

const ORIGIN_HINT: Record<TimelineOrigin, string> = {
  measured: "chapter starts were measured from the encoded audio",
  dynamic:
    "one whole-book encode; starts follow the assembled layout and were not separately measured",
  unverified: "some chapter measurements are missing, so starts are unverified",
  legacy: "this export predates chapter measurement",
};

interface ReadingHeaderProps {
  title: string;
  chapter: ReadingChapterSummary | null;
  origin: TimelineOrigin | null;
  mode: ReadingMode;
  onSelectMode: (next: ReadingMode) => void;
  onClose: () => void;
  compact?: boolean;
  controls?: React.ReactNode;
  /**
   * False on the job-independent BookSurface mount (F8 D2): with no
   * narration there is no script surface, so the Script/Page toggle is
   * hidden rather than offering a dead switch.
   */
  scriptAvailable?: boolean;
  utilities?: React.ReactNode;
}

const SEGMENT_ACTIVE =
  "relative rounded-md px-3 py-1 font-mono text-label tracking-wide text-ember-200 uppercase";
const SEGMENT_QUIET =
  "relative rounded-md px-3 py-1 font-mono text-label tracking-wide text-ink-300 uppercase transition-colors hover:text-ember-300 active:scale-[0.98]";

function ReadingHeader({
  title,
  chapter,
  origin,
  mode,
  onSelectMode,
  onClose,
  compact = false,
  controls,
  scriptAvailable = true,
  utilities,
}: ReadingHeaderProps) {
  const { t } = useI18n();
  return (
    <header
      data-testid="reading-header"
      data-compact={compact ? "yes" : "no"}
      className={`@container flex w-full min-w-0 flex-wrap gap-x-4 gap-y-2 ${
        compact ? "items-center" : "items-start"
      }`}
    >
      <button
        type="button"
        data-testid="reading-close"
        aria-label={t("reader.closeFollowAlongEsc")}
        title={t("reader.closeEsc")}
        onClick={onClose}
        className="flex h-9 shrink-0 items-center gap-1 rounded-md ps-2 pe-3 text-secondary font-semibold text-ember-300 transition-colors hover:bg-ink-800 hover:text-ember-200 active:scale-[0.96]"
      >
        <ChevronIcon className="h-4 w-4 rotate-180" />
        {t("nav.shelf")}
      </button>
      <div
        className={`flex min-w-0 flex-1 gap-2 overflow-hidden ${
          compact ? "items-baseline" : "flex-col gap-1"
        }`}
      >
        <h2
          className={`truncate font-serif font-semibold text-ink-50 ${
            compact ? "shrink-0 max-w-[40%] text-body" : "text-title"
          }`}
        >
          {title}
        </h2>
        <div className="flex min-w-0 items-center gap-2">
          {chapter && (
            <span
              data-testid="reading-chapter"
              className="min-w-0 truncate font-mono text-label tracking-wide text-ink-400 uppercase"
            >
              {chapter.title}
            </span>
          )}
          {origin && import.meta.env.DEV && (
            <span
              data-testid="reading-origin"
              title={ORIGIN_HINT[origin]}
              className="shrink-0 rounded bg-ink-800 px-1.5 py-0.5 font-mono text-label tracking-wide text-ink-400 uppercase @max-[60rem]:hidden"
            >
              {ORIGIN_LABEL[origin]}
            </span>
          )}
        </div>
      </div>
      {controls && (
        <div
          data-testid="reading-header-controls"
          className="flex shrink-0 items-center @max-[40rem]:order-last @max-[40rem]:basis-full @max-[40rem]:justify-end"
        >
          {controls}
        </div>
      )}
      {scriptAvailable && (
        <GlideGroup
          thumbClassName="rounded-md bg-ember-500/15"
          role="group"
          aria-label={t("reader.surface")}
          className="flex shrink-0 items-center gap-0.5 rounded-lg border border-ink-700 p-0.5"
        >
          <button
            type="button"
            data-testid="reading-mode-script"
            aria-pressed={mode === "script"}
            title={t("reader.scriptHint")}
            onClick={() => onSelectMode("script")}
            className={mode === "script" ? SEGMENT_ACTIVE : SEGMENT_QUIET}
          >
            {t("reader.script")}
          </button>
          <button
            type="button"
            data-testid="reading-mode-toggle"
            aria-pressed={mode === "page"}
            aria-label={t("reader.page")}
            title={t("reader.pageHint")}
            onClick={() => onSelectMode("page")}
            className={mode === "page" ? SEGMENT_ACTIVE : SEGMENT_QUIET}
          >
            {t("reader.page")}
          </button>
        </GlideGroup>
      )}
      {utilities && (
        <div
          data-testid="reading-utilities"
          className="flex shrink-0 items-center gap-1"
        >
          {utilities}
        </div>
      )}
    </header>
  );
}

export default ReadingHeader;
