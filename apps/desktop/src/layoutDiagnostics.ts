import { invoke } from "@tauri-apps/api/core";

export const BREAKPOINT_REMS = [40, 48, 64, 80] as const;
export const BREAKPOINT_PX = [640, 768, 1024, 1280] as const;
export const WIDTH_PROBES_PX = [973, 974, 1024, 1280, 1362] as const;

const GRID_SELECTOR = '[data-testid="book-grid"]';
const GRID_WAIT_MS = 3000;

export interface MediaMatches {
  remRange: boolean[];
  remMinWidth: boolean[];
  pxRange: boolean[];
  pxMinWidth: boolean[];
}

export interface LayoutSample {
  innerWidth: number;
  clientWidth: number;
  containerWidth: number | null;
  devicePixelRatio: number;
  rootFontSizePx: string;
  breakpointLg: string;
  media: MediaMatches;
  widthProbes: boolean[];
  gridColumns: number | null;
  gridTemplate: string | null;
}

function listMatched<T extends number>(
  units: readonly T[],
  matches: boolean[],
  suffix: string,
): string {
  const active = units
    .filter((_, index) => matches[index])
    .map((unit) => `${unit}${suffix}`)
    .join(",");
  return active || "none";
}

export function firstUnmatchedProbe(matches: boolean[]): number | null {
  const index = matches.findIndex((matched) => !matched);
  return index < 0 ? null : WIDTH_PROBES_PX[index];
}

export function countGridColumns(template: string | null): number | null {
  if (template == null) return null;
  const trimmed = template.trim();
  if (!trimmed || trimmed === "none") return null;
  return trimmed.split(/\s+/).length;
}

export function formatLayoutSample(sample: LayoutSample): string {
  const columns =
    sample.gridColumns == null ? "n/a" : String(sample.gridColumns);
  const container =
    sample.containerWidth == null ? "n/a" : String(sample.containerWidth);
  const probeEdge = firstUnmatchedProbe(sample.widthProbes);
  return [
    `innerWidth=${sample.innerWidth}`,
    `clientWidth=${sample.clientWidth}`,
    `containerWidth=${container}`,
    `dpr=${sample.devicePixelRatio}`,
    `rootFontSize=${sample.rootFontSizePx}`,
    `breakpointLg=${sample.breakpointLg || "unset"}`,
    `remRange=[${listMatched(BREAKPOINT_REMS, sample.media.remRange, "rem")}]`,
    `remMin=[${listMatched(BREAKPOINT_REMS, sample.media.remMinWidth, "rem")}]`,
    `pxRange=[${listMatched(BREAKPOINT_PX, sample.media.pxRange, "px")}]`,
    `pxMin=[${listMatched(BREAKPOINT_PX, sample.media.pxMinWidth, "px")}]`,
    `firstUnmatchedProbe=${probeEdge == null ? "none" : `${probeEdge}px`}`,
    `gridColumns=${columns}`,
    `gridTemplate=${sample.gridTemplate ?? "n/a"}`,
  ].join(" ");
}

function matchAll(queries: string[]): boolean[] {
  return queries.map((query) => window.matchMedia(query).matches);
}

export function readLayoutSample(): LayoutSample {
  const root = document.documentElement;
  const grid = document.querySelector(GRID_SELECTOR);
  const gridTemplate =
    grid == null ? null : getComputedStyle(grid).gridTemplateColumns;
  const parent = grid?.parentElement ?? null;
  return {
    innerWidth: window.innerWidth,
    clientWidth: root.clientWidth,
    containerWidth: parent == null ? null : parent.clientWidth,
    devicePixelRatio: window.devicePixelRatio,
    rootFontSizePx: getComputedStyle(root).fontSize,
    breakpointLg: getComputedStyle(root)
      .getPropertyValue("--breakpoint-lg")
      .trim(),
    media: {
      remRange: matchAll(
        BREAKPOINT_REMS.map((rem) => `(width >= ${rem}rem)`),
      ),
      remMinWidth: matchAll(
        BREAKPOINT_REMS.map((rem) => `(min-width: ${rem}rem)`),
      ),
      pxRange: matchAll(BREAKPOINT_PX.map((px) => `(width >= ${px}px)`)),
      pxMinWidth: matchAll(BREAKPOINT_PX.map((px) => `(min-width: ${px}px)`)),
    },
    widthProbes: matchAll(
      WIDTH_PROBES_PX.map((px) => `(min-width: ${px}px)`),
    ),
    gridColumns: countGridColumns(gridTemplate),
    gridTemplate,
  };
}

export function waitForGrid(timeoutMs = GRID_WAIT_MS): Promise<void> {
  if (document.querySelector(GRID_SELECTOR) != null) return Promise.resolve();
  return new Promise((resolve) => {
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      observer.disconnect();
      clearTimeout(timer);
      resolve();
    };
    const observer = new MutationObserver(() => {
      if (document.querySelector(GRID_SELECTOR) != null) finish();
    });
    const timer = setTimeout(finish, timeoutMs);
    observer.observe(document.body, { childList: true, subtree: true });
  });
}

export async function reportLayout(label: string): Promise<void> {
  try {
    const line = `${label} ${formatLayoutSample(readLayoutSample())}`;
    await invoke("log_ui_layout", { line });
  } catch {
    return;
  }
}

export async function reportLayoutWhenReady(label: string): Promise<void> {
  await waitForGrid();
  await reportLayout(label);
}
