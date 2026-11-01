export type PageFit = "width" | "page";

export const FIT_STORAGE_KEY = "txtcaster.pageFit";
export const ZOOM_STORAGE_KEY = "txtcaster.pageZoom";

export const PAGE_ZOOM_MIN = 0.5;
export const PAGE_ZOOM_MAX = 2.5;
export const PAGE_ZOOM_STEP = 0.1;

export const READING_COLUMN_MAX_PX = 1500;
export const SURFACE_GUTTER_PX = 24;

export const DEFAULT_FIT: PageFit = "width";

export interface FitInput {
  containerWidth: number;
  containerHeight: number;
  ratio: number | null;
  fit: PageFit;
  zoom: number;
}

export function clampPageZoom(factor: number): number {
  if (!Number.isFinite(factor)) return 1;
  const clamped = Math.min(PAGE_ZOOM_MAX, Math.max(PAGE_ZOOM_MIN, factor));
  return Math.round(clamped * 100) / 100;
}

export function pageFitFrom(stored: string | null): PageFit {
  return stored === "page" ? "page" : DEFAULT_FIT;
}

export function loadPageFit(): PageFit {
  try {
    return pageFitFrom(localStorage.getItem(FIT_STORAGE_KEY));
  } catch {
    return DEFAULT_FIT;
  }
}

export function storePageFit(fit: PageFit): void {
  try {
    localStorage.setItem(FIT_STORAGE_KEY, fit);
  } catch {
    return;
  }
}

export function loadPageZoom(): number {
  try {
    const raw = localStorage.getItem(ZOOM_STORAGE_KEY);
    if (raw == null || raw.trim() === "") return 1;
    return clampPageZoom(Number(raw));
  } catch {
    return 1;
  }
}

export function storePageZoom(zoom: number): void {
  try {
    localStorage.setItem(ZOOM_STORAGE_KEY, String(zoom));
  } catch {
    return;
  }
}

export function zoomIn(current: number): number {
  return clampPageZoom(current + PAGE_ZOOM_STEP);
}

export function zoomOut(current: number): number {
  return clampPageZoom(current - PAGE_ZOOM_STEP);
}

export function fittedWidthPx(input: FitInput): number | null {
  const { containerWidth, containerHeight, ratio, fit, zoom } = input;
  if (ratio == null || !Number.isFinite(ratio) || ratio <= 0) return null;
  if (!Number.isFinite(containerWidth) || containerWidth <= 0) return null;
  const capped = Math.min(containerWidth, READING_COLUMN_MAX_PX);
  let base = capped;
  if (fit === "page") {
    if (!Number.isFinite(containerHeight) || containerHeight <= 0) return null;
    base = Math.min(capped, containerHeight * ratio);
  }
  const scaled = base * clampPageZoom(zoom);
  if (!Number.isFinite(scaled) || scaled <= 0) return null;
  return Math.floor(scaled);
}

export function scrollsVertically(input: FitInput): boolean {
  const width = fittedWidthPx(input);
  if (width == null) return false;
  const { containerHeight, ratio } = input;
  if (ratio == null || ratio <= 0) return false;
  if (!Number.isFinite(containerHeight) || containerHeight <= 0) return false;
  return width / ratio > containerHeight;
}
