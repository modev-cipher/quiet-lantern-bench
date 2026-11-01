import { invoke } from "@tauri-apps/api/core";

import { anchoredSegmentCount } from "./readingPageModel";
import type { PageModeReason, ReadingMode } from "./readingPageModel";
import type { AnchorRung, GeometryOutcome, ReadingChapter } from "./readingModel";

export interface PageModeSample {
  mode: ReadingMode;
  reason: PageModeReason;
  chapterId: string | null;
  anchorGeometry: GeometryOutcome | null;
  segments: number;
  rungs: Record<AnchorRung, number>;
  anchored: number;
}

const RUNGS: AnchorRung[] = ["word", "line", "block", "page"];

export function emptyRungs(): Record<AnchorRung, number> {
  return { word: 0, line: 0, block: 0, page: 0 };
}

export function pageModeSample(
  mode: ReadingMode,
  reason: PageModeReason,
  chapter: ReadingChapter | null,
): PageModeSample {
  const rungs = emptyRungs();
  for (const segment of chapter?.segments ?? []) rungs[segment.anchor] += 1;
  return {
    mode,
    reason,
    chapterId: chapter?.chapterId ?? null,
    anchorGeometry: chapter?.anchorGeometry ?? null,
    segments: chapter?.segments.length ?? 0,
    rungs,
    anchored: anchoredSegmentCount(chapter?.segments ?? []),
  };
}

export function shouldEmitPageMode(
  previous: PageModeSample | null,
  next: PageModeSample,
): boolean {
  if (previous == null) return true;
  return (
    previous.mode !== next.mode ||
    previous.reason !== next.reason ||
    previous.chapterId !== next.chapterId ||
    previous.anchorGeometry !== next.anchorGeometry
  );
}

export function formatPageMode(sample: PageModeSample): string {
  const rungs = RUNGS.map((rung) => `${rung}=${sample.rungs[rung]}`).join(" ");
  const chapter =
    sample.chapterId == null ? "n/a" : sample.chapterId.slice(0, 8);
  return [
    `mode=${sample.mode}`,
    `reason=${sample.reason}`,
    `chapter=${chapter}`,
    `anchorGeometry=${sample.anchorGeometry ?? "n/a"}`,
    `segments=${sample.segments}`,
    rungs,
    `anchored=${sample.anchored}`,
  ].join(" ");
}

export async function reportPageMode(sample: PageModeSample): Promise<void> {
  try {
    await invoke("log_page_mode", { line: formatPageMode(sample) });
  } catch {
    return;
  }
}
