import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";

import { PlayIcon } from "./Icons";
import SetupPill from "./SetupPill";
import { usePlayback } from "./usePlayback";
import TypographicCover from "./TypographicCover";
import { modeLabel, type AudioShelfItem } from "./audioShelfModel";
import { homeSelection, loadRecent, positionLine } from "./homeModel";
import type { ReadingIndex } from "./readingModel";
import type { ListenedMap } from "./listeningProgress";
import type { SetupChecklist } from "./setupChecklist";
import type { View } from "./views";
import { t, useI18n } from "./i18n";

function listenedLine(fraction: number | null): string {
  if (fraction == null || fraction <= 0) return t("study.notStarted");
  if (fraction >= 1) return t("study.finished");
  return t("study.listened", { pct: Math.round(fraction * 100) });
}

function ShelfContinue({
  shelf,
  listened,
  checklist,
  onNavigate,
  onOpenSession,
}: {
  shelf: AudioShelfItem[];
  listened: ListenedMap;
  checklist: SetupChecklist | null;
  onNavigate: (view: View) => void;
  onOpenSession: (
    item: AudioShelfItem,
    resumed: boolean,
    fraction: number | null,
  ) => void;
}) {
  const { t: tt } = useI18n();
  const [heroIndex, setHeroIndex] = useState<{
    jobId: string;
    index: ReadingIndex;
  } | null>(null);

  const selection = useMemo(
    () => homeSelection(shelf, loadRecent(), listened),
    [shelf, listened],
  );
  const hero = selection.hero;
  const heroJobId = selection.heroReadAlong ? (hero?.jobId ?? null) : null;

  useEffect(() => {
    if (heroJobId == null) return;
    let active = true;
    invoke<ReadingIndex>("get_reading_index", { jobId: heroJobId })
      .then((index) => {
        if (active) setHeroIndex({ jobId: heroJobId, index });
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [heroJobId]);

  const index =
    heroIndex != null && heroIndex.jobId === heroJobId ? heroIndex.index : null;

  const somethingLoaded = usePlayback((state) => state.item != null);
  const [layer, setLayer] = useState<HTMLElement | null>(null);
  useEffect(() => {
    setLayer(document.getElementById("main-overlay"));
  }, []);

  if (layer == null) return null;
  const showHero = hero != null && !somethingLoaded;
  if (!showHero && checklist == null) return null;

  return createPortal(
    <div
      className={`absolute end-6 flex flex-col items-end gap-2 transition-[bottom] duration-300 ${
        somethingLoaded ? "bottom-40 md:bottom-28" : "bottom-6"
      }`}
    >
      {checklist != null && (
        <SetupPill checklist={checklist} onNavigate={onNavigate} />
      )}
      {showHero && (
        <div
          data-testid="shelf-continue"
          className="resume-card pointer-events-auto"
        >
          <section
            data-testid="study-hero"
            aria-label={tt("study.continue")}
            className="shadow-overlay flex max-w-sm items-center gap-3 rounded-2xl border border-ink-800 bg-ink-900 py-2.5 ps-2.5 pe-3"
          >
            <div className="h-12 w-10 shrink-0 overflow-hidden rounded-md">
              <TypographicCover
                seed={hero.bookId || hero.id}
                title={hero.title}
                size="sm"
                progress={selection.heroListened}
              />
            </div>
            <div className="flex min-w-0 flex-1 flex-col">
              <p className="truncate text-secondary font-semibold text-ink-100">
                {hero.title}
              </p>
              <p className="truncate text-label text-ink-400">
                <span className="capitalize">{modeLabel(hero.mode)}</span> ·{" "}
                {(index != null
                  ? positionLine(index, selection.heroListened)
                  : null) ?? listenedLine(selection.heroListened)}
              </p>
            </div>
            <button
              type="button"
              data-testid="study-resume"
              aria-label={
                selection.heroReadAlong
                  ? tt("study.resume")
                  : tt("study.listen")
              }
              title={
                selection.heroReadAlong
                  ? tt("study.resume")
                  : tt("study.listen")
              }
              onClick={() => onOpenSession(hero, false, selection.heroListened)}
              className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-ember-500 text-on-accent transition-transform duration-200 hover:bg-ember-600 active:scale-[0.94]"
            >
              <PlayIcon className="ms-0.5 h-4 w-4" />
            </button>
          </section>
        </div>
      )}
    </div>,
    layer,
  );
}

export default ShelfContinue;
