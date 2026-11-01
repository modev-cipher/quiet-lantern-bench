import type { ReadingChapterSummary, ReadingIndex } from "./readingModel";
import { useI18n } from "./i18n";

export const OUTLINE_OPEN_KEY = "txtcaster.reading.outline-open";

export function loadOutlineOpen(): boolean {
  try {
    return localStorage.getItem(OUTLINE_OPEN_KEY) !== "0";
  } catch {
    return true;
  }
}

export function storeOutlineOpen(open: boolean): void {
  try {
    localStorage.setItem(OUTLINE_OPEN_KEY, open ? "1" : "0");
  } catch {
    return;
  }
}

/** Params for `reader.castCoverage` when the cast covers only part of the
 * book (I18N-2: was a template literal the census could not see, so the AR
 * outline rendered English with an unisolated leading number). */
export function castCoverage(
  castChapters: number,
  bookChapters: number | null,
): { cast: number; count: number } | null {
  if (bookChapters == null || bookChapters <= castChapters) return null;
  // `count` drives the plural form (CLDR): 3 → few ("3 فصول"), 11 → many ("11 فصلًا").
  return { cast: castChapters, count: bookChapters };
}

function ReadingOutline({
  index,
  title,
  currentChapterId,
  bookChapterCount,
  folded,
  onSeek,
}: {
  index: ReadingIndex | null;
  title: string;
  currentChapterId: string | null;
  bookChapterCount: number | null;
  folded: boolean;
  onSeek: (chapter: ReadingChapterSummary) => void;
}) {
  const { t, tx } = useI18n();
  const coverage =
    index == null
      ? null
      : castCoverage(index.chapters.length, bookChapterCount);
  const chapters = index?.chapters ?? [];
  if (folded) {
    return (
      <nav
        data-testid="reading-outline"
        data-state="rail"
        aria-label={t("reader.chapters")}
        className="flex h-full w-14 shrink-0 flex-col border-e border-ink-800 bg-ink-950"
      >
        <ol className="flex min-h-0 flex-1 flex-col items-center gap-1 overflow-y-auto px-2 py-3">
          {chapters.map((chapter) => {
            const current = chapter.chapterId === currentChapterId;
            return (
              <li key={chapter.chapterId}>
                <button
                  type="button"
                  data-testid={`outline-chapter-${chapter.idx}`}
                  aria-current={current ? "true" : undefined}
                  aria-label={chapter.title}
                  title={chapter.title}
                  onClick={() => onSeek(chapter)}
                  className={`relative flex h-9 w-9 items-center justify-center rounded-md font-mono text-label transition-colors duration-200 active:scale-[0.96] ${
                    current
                      ? "bg-ink-850 font-semibold text-ember-400"
                      : "text-ink-400 hover:bg-ink-850 hover:text-ink-100"
                  }`}
                >
                  {current && (
                    <span className="absolute inset-y-2 -start-2 w-0.5 rounded-full bg-ember-500" />
                  )}
                  {chapter.idx + 1}
                </button>
              </li>
            );
          })}
        </ol>
      </nav>
    );
  }
  return (
    <nav
      data-testid="reading-outline"
      data-state="drawer"
      aria-label={t("reader.chapters")}
      className="flex h-full w-56 shrink-0 flex-col border-e border-ink-800 bg-ink-950"
    >
      <div className="border-b border-ink-800/70 px-5 pt-4 pb-3.5">
        <p className="font-serif text-heading leading-snug font-semibold text-ink-50">
          {title}
        </p>
        {index != null && (
          <p className="mt-1 font-mono text-label text-ink-400">
            {tx("reader.chaptersInCast", { count: index.chapters.length })}
          </p>
        )}
      </div>
      <ol className="min-h-0 flex-1 overflow-y-auto p-2.5">
        {chapters.map((chapter) => {
          const current = chapter.chapterId === currentChapterId;
          return (
            <li key={chapter.chapterId}>
              <button
                type="button"
                data-testid={`outline-chapter-${chapter.idx}`}
                aria-current={current ? "true" : undefined}
                onClick={() => onSeek(chapter)}
                className={`relative flex w-full items-baseline gap-2.5 rounded-lg px-3 py-2 text-start transition-colors duration-200 active:scale-[0.98] ${
                  current
                    ? "bg-ink-850 text-ink-50"
                    : "text-ink-300 hover:bg-ink-850 hover:text-ink-100"
                }`}
              >
                {current && (
                  <span className="absolute inset-y-2 start-0 w-0.5 rounded-full bg-ember-500" />
                )}
                <span className="w-5 shrink-0 font-mono text-label text-ink-500">
                  {chapter.idx + 1}
                </span>
                <span className="min-w-0 flex-1 truncate text-secondary font-medium">
                  {chapter.title}
                </span>
              </button>
            </li>
          );
        })}
      </ol>
      {coverage != null && (
        <p
          data-testid="outline-coverage"
          className="border-t border-ink-800/70 px-5 py-3 text-label leading-relaxed text-ink-500"
        >
          {tx("reader.castCoverage", coverage)}
        </p>
      )}
    </nav>
  );
}

export default ReadingOutline;
