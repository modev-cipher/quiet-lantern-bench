import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";

import {
  aspectRatio,
  figureForSegment,
  figureViewport,
  filterWantedPages,
  pagesToFetch,
  type TravelDirection,
} from "./readingFacsimileModel";
import type {
  ReadingFigure,
  ReadingPage,
  ReadingSegment,
} from "./readingModel";
import { useI18n } from "./i18n";

interface ReadingFacsimileProps {
  segment: ReadingSegment | null;
  pageNo: number | null;
  direction: TravelDirection;
  pages: Map<number, ReadingPage>;
  figures: ReadingFigure[];
  lastPage: number;
  onNeedPages: (pages: number[]) => void;
  readyToken?: string;
  onPageReady?: (pageNo: number) => void;
  overlay?: React.ReactNode;
  hitLayer?: React.ReactNode;
}

function ReadingFacsimile({
  segment,
  pageNo,
  direction,
  pages,
  figures,
  lastPage,
  onNeedPages,
  onPageReady,
  readyToken,
  overlay,
  hitLayer,
}: ReadingFacsimileProps) {
  const { t } = useI18n();
  const imageRef = useRef<HTMLImageElement>(null);
  const page = pageNo == null ? null : (pages.get(pageNo) ?? null);
  const anchored = figureForSegment(segment, figures);
  const figure =
    anchored != null && anchored.pageNo === pageNo ? anchored : null;
  const viewport = figureViewport(page, figure);
  const ratio = aspectRatio(page);
  const [attempts, setAttempts] = useState<Record<string, number>>({});
  const asked = useRef<Set<number>>(new Set());
  const imagePath = page?.imagePath ?? null;
  const attempt = imagePath == null ? 0 : (attempts[imagePath] ?? 0);

  useEffect(() => {
    if (pageNo == null) return;
    const window_ = pagesToFetch(pageNo, lastPage, direction);
    for (const number of [...asked.current]) {
      if (!window_.includes(number)) asked.current.delete(number);
    }
    const wanted = filterWantedPages(window_, pages, asked.current);
    if (wanted.length === 0) return;
    for (const number of wanted) asked.current.add(number);
    onNeedPages(wanted);
  }, [direction, lastPage, onNeedPages, pageNo, pages]);

  const source = useMemo(() => {
    if (imagePath == null || attempt > 1) return null;
    const suffix = attempt === 1 ? "?retry=1" : "";
    return `${convertFileSrc(imagePath)}${suffix}`;
  }, [attempt, imagePath]);

  useEffect(() => {
    const image = imageRef.current;
    if (pageNo != null && image?.complete && image.naturalWidth > 0)
      onPageReady?.(pageNo);
  }, [pageNo, source, onPageReady, readyToken]);

  return (
    <div
      data-testid="reading-facsimile"
      data-page={pageNo ?? ""}
      data-figure={figure?.figureId ?? ""}
      data-zoomed={viewport == null ? "no" : "yes"}
      data-overlay={
        overlay == null ? "none" : viewport == null ? "on" : "zoomed-out"
      }
      data-attempt={attempt}
      className="relative w-full overflow-hidden rounded-xl bg-reading-page ring-1 ring-ink-800/70"
      style={ratio == null ? { aspectRatio: "3 / 4" } : { aspectRatio: ratio }}
    >
      {source == null ? (
        <div
          data-testid="reading-facsimile-placeholder"
          className="absolute inset-0 flex items-center justify-center font-mono text-label tracking-widest text-ink-500 uppercase"
        >
          {pageNo == null
            ? t("reader.noPage")
            : t("reader.pageAlt", { pageNo })}
        </div>
      ) : (
        <img
          ref={imageRef}
          data-testid="reading-facsimile-image"
          src={source}
          onLoad={() => {
            if (pageNo != null) onPageReady?.(pageNo);
          }}
          alt={t("reader.pageAlt", { pageNo: pageNo ?? "" })}
          className="absolute inset-0 h-full w-full object-contain transition-transform duration-300"
          style={
            viewport == null
              ? undefined
              : {
                  transform: `scale(${viewport.scale})`,
                  transformOrigin: `${viewport.originXPct}% ${viewport.originYPct}%`,
                }
          }
          onError={() => {
            if (imagePath == null) return;
            setAttempts((previous) => ({
              ...previous,
              [imagePath]: (previous[imagePath] ?? 0) + 1,
            }));
          }}
        />
      )}
      {viewport == null && hitLayer}
      {viewport == null && overlay}
      {figure && (
        <div
          data-testid="reading-figure-frame"
          className="pointer-events-none absolute inset-0 rounded-xl ring-2 ring-reading-frame/70"
        />
      )}
    </div>
  );
}

export default ReadingFacsimile;
