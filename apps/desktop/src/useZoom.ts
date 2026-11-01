import { useEffect } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";

export const ZOOM_MIN = 0.5;
export const ZOOM_MAX = 2.5;
export const ZOOM_STEP = 0.1;
const STORAGE_KEY = "txtcaster.zoom";

export function clampZoom(factor: number): number {
  const clamped = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, factor));
  return Math.round(clamped * 10) / 10;
}

export function loadZoom(): number {
  try {
    if (typeof localStorage === "undefined") return 1;
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw == null || raw.trim() === "") return 1;
    const parsed = Number(raw);
    return Number.isFinite(parsed) ? clampZoom(parsed) : 1;
  } catch {
    return 1;
  }
}

export function zoomFromKey(current: number, key: string): number | null {
  if (key === "+" || key === "=") return clampZoom(current + ZOOM_STEP);
  if (key === "-") return clampZoom(current - ZOOM_STEP);
  if (key === "0") return 1;
  return null;
}

export function zoomFromWheel(current: number, deltaY: number): number {
  return clampZoom(current + (deltaY < 0 ? ZOOM_STEP : -ZOOM_STEP));
}

export function useZoom(): void {
  useEffect(() => {
    let current = loadZoom();

    const apply = (factor: number) => {
      current = factor;
      try {
        localStorage.setItem(STORAGE_KEY, String(factor));
      } catch (error) {
        console.error("zoom persistence failed", error);
      }
      getCurrentWebview().setZoom(factor).catch(console.error);
    };

    if (current !== 1) apply(current);

    const onKeyDown = (event: KeyboardEvent) => {
      if (!event.ctrlKey || event.altKey) return;
      const next = zoomFromKey(current, event.key);
      if (next == null) return;
      event.preventDefault();
      apply(next);
    };

    const onWheel = (event: WheelEvent) => {
      if (!event.ctrlKey) return;
      event.preventDefault();
      apply(zoomFromWheel(current, event.deltaY));
    };

    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("wheel", onWheel);
    };
  }, []);
}
