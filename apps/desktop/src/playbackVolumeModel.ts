/**
 * WebKitGTK trusts the audio server for a new stream's initial volume, and PipeWire
 * restores the last volume saved for the stream's role. A stream can therefore start
 * silent while the store says 100%. The store is the truth while a stream settles; after
 * that a change comes from the user's system mixer and is left alone.
 */
export const STREAM_SETTLE_MS = 3000;

export interface VolumeElement {
  volume: number;
  muted: boolean;
}

export function applyPlayerVolume(
  element: VolumeElement,
  target: { volume: number; fade: number; muted: boolean },
): void {
  const wanted = target.volume * target.fade;
  if (Math.abs(element.volume - wanted) > 0.005) element.volume = wanted;
  if (element.muted !== target.muted) element.muted = target.muted;
}

export function streamStillSettling(
  startedAtMs: number | null,
  nowMs: number,
): boolean {
  return startedAtMs != null && nowMs - startedAtMs < STREAM_SETTLE_MS;
}
