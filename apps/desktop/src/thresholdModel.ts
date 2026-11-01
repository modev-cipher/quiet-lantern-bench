export interface ThresholdRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface ThresholdSource {
  rect: ThresholdRect;
  seed: string;
  title: string;
}

export interface ThresholdViewport {
  width: number;
  height: number;
}

export const THRESHOLD_MS = 420;

export function enterTransform(
  source: ThresholdRect,
  viewport: ThresholdViewport,
): string | null {
  const numbers = [
    source.x,
    source.y,
    source.width,
    source.height,
    viewport.width,
    viewport.height,
  ];
  if (!numbers.every((value) => Number.isFinite(value))) return null;
  if (source.width <= 0 || source.height <= 0) return null;
  if (viewport.width <= 0 || viewport.height <= 0) return null;

  const scaleX = source.width / viewport.width;
  const scaleY = source.height / viewport.height;
  if (scaleX >= 1 || scaleY >= 1) return null;

  return `translate(${Math.round(source.x)}px, ${Math.round(source.y)}px) scale(${scaleX.toFixed(4)}, ${scaleY.toFixed(4)})`;
}

let pending: ThresholdSource | null = null;

export function armThreshold(
  element: Element | null,
  seed: string,
  title: string,
): void {
  if (element == null) {
    pending = null;
    return;
  }
  const box = element.getBoundingClientRect();
  pending = {
    rect: { x: box.x, y: box.y, width: box.width, height: box.height },
    seed,
    title,
  };
}

export function peekThreshold(): ThresholdSource | null {
  return pending;
}

export function resetThreshold(): void {
  pending = null;
}
