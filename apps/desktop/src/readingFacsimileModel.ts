import type { ReadingFigure, ReadingPage, ReadingSegment } from "./readingModel";

export const FIGURE_MARGIN = 0.12;
export const MAX_FIGURE_ZOOM = 6;

export interface FigureViewport {
  scale: number;
  originXPct: number;
  originYPct: number;
}

export interface NormalisedRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export function framedFigureId(segment: ReadingSegment | null): string | null {
  return segment?.figureId ?? null;
}

export type TravelDirection = 1 | -1;

export function pagesToFetch(
  pageNo: number,
  lastPage: number,
  direction: TravelDirection,
): number[] {
  if (!Number.isFinite(pageNo) || pageNo < 1) return [];
  const known = Number.isFinite(lastPage) && lastPage >= 1;
  const first = known ? Math.min(pageNo, lastPage) : pageNo;
  const pages = [first];
  if (direction === -1) {
    if (first - 1 >= 1) pages.push(first - 1);
    return pages;
  }
  if (!known || first + 1 <= lastPage) pages.push(first + 1);
  return pages;
}

export function filterWantedPages(
  window_: number[],
  pages: Map<number, ReadingPage>,
  asked: Set<number>,
): number[] {
  return window_.filter((number) => {
    if (asked.has(number)) return false;
    const known = pages.get(number);
    if (known == null) return true;
    return known.imagePath == null;
  });
}

export function aspectRatio(page: ReadingPage | null): number | null {
  if (page == null) return null;
  const { widthPt, heightPt } = page;
  if (widthPt == null || heightPt == null) return null;
  if (widthPt <= 0 || heightPt <= 0) return null;
  return widthPt / heightPt;
}

export function normalisedRect(
  page: ReadingPage,
  figure: ReadingFigure,
): NormalisedRect | null {
  const { widthPt, heightPt } = page;
  if (widthPt == null || heightPt == null) return null;
  if (widthPt <= 0 || heightPt <= 0) return null;
  if (figure.bbox == null) return null;
  const [x0, y0, x1, y1] = figure.bbox;
  const left = Math.min(x0, x1) / widthPt;
  const top = Math.min(y0, y1) / heightPt;
  const width = Math.abs(x1 - x0) / widthPt;
  const height = Math.abs(y1 - y0) / heightPt;
  if (width <= 0 || height <= 0) return null;
  if (left < 0 || top < 0 || left + width > 1.0001 || top + height > 1.0001) {
    return null;
  }
  return { left, top, width, height };
}

export function figureViewport(
  page: ReadingPage | null,
  figure: ReadingFigure | null,
): FigureViewport | null {
  if (page == null || figure == null) return null;
  const rect = normalisedRect(page, figure);
  if (rect == null) return null;
  const widest = Math.max(rect.width, rect.height);
  const fitted = 1 / (widest * (1 + FIGURE_MARGIN));
  const scale = Math.min(MAX_FIGURE_ZOOM, Math.max(1, fitted));
  const half = 0.5 / scale;
  const clamp = (value: number) => Math.min(1 - half, Math.max(half, value));
  return {
    scale,
    originXPct: clamp(rect.left + rect.width / 2) * 100,
    originYPct: clamp(rect.top + rect.height / 2) * 100,
  };
}

export function figureForSegment(
  segment: ReadingSegment | null,
  figures: ReadingFigure[],
): ReadingFigure | null {
  const figureId = framedFigureId(segment);
  if (figureId == null) return null;
  return figures.find((figure) => figure.figureId === figureId) ?? null;
}

export function pageToShow(
  segment: ReadingSegment | null,
  figures: ReadingFigure[],
): number | null {
  if (segment == null) return null;
  const figure = figureForSegment(segment, figures);
  return figure?.pageNo ?? segment.pageNo;
}

export interface FigurePageHold {
  pageNo: number;
  remainingSegments: number;
}

export const MAX_FIGURE_PAGE_HOLD = 4;

export function updateFigureHold(
  currentHold: FigurePageHold | null,
  activeSegment: ReadingSegment | null,
  figures: ReadingFigure[],
  isContiguous: boolean,
  isSegmentTransition: boolean,
): { hold: FigurePageHold | null; pageNo: number | null } {
  if (activeSegment == null) {
    return { hold: null, pageNo: null };
  }
  const naturalPage = pageToShow(activeSegment, figures);
  if (!isContiguous) {
    const figure = figureForSegment(activeSegment, figures);
    const newHold =
      figure != null
        ? { pageNo: figure.pageNo, remainingSegments: MAX_FIGURE_PAGE_HOLD }
        : null;
    return { hold: newHold, pageNo: naturalPage };
  }

  const figure = figureForSegment(activeSegment, figures);
  if (figure != null) {
    return {
      hold: { pageNo: figure.pageNo, remainingSegments: MAX_FIGURE_PAGE_HOLD },
      pageNo: figure.pageNo,
    };
  }

  if (currentHold != null) {
    const remaining = isSegmentTransition
      ? currentHold.remainingSegments - 1
      : currentHold.remainingSegments;
    if (naturalPage != null && naturalPage > currentHold.pageNo) {
      return { hold: null, pageNo: naturalPage };
    }
    if (remaining >= 0) {
      return {
        hold:
          remaining > 0
            ? { pageNo: currentHold.pageNo, remainingSegments: remaining }
            : null,
        pageNo: currentHold.pageNo,
      };
    }
  }

  return { hold: null, pageNo: naturalPage };
}

