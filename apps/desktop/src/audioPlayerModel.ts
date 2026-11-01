export const PLAYBACK_RATES = [1, 1.25, 1.5, 2, 0.75] as const;

export function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  const total = Math.floor(seconds);
  const hrs = Math.floor(total / 3600);
  const mins = Math.floor((total % 3600) / 60);
  const secs = total % 60;
  const pad = (value: number) => value.toString().padStart(2, "0");
  if (hrs > 0) return `${hrs}:${pad(mins)}:${pad(secs)}`;
  return `${mins}:${pad(secs)}`;
}

export function progressPercent(current: number, duration: number): number {
  if (!Number.isFinite(duration) || duration <= 0) return 0;
  const ratio = current / duration;
  return Math.min(100, Math.max(0, ratio * 100));
}

export function seekFraction(
  clientX: number,
  left: number,
  width: number,
  direction: "ltr" | "rtl" = "ltr",
): number {
  if (width <= 0) return 0;
  const ratio =
    direction === "rtl"
      ? (left + width - clientX) / width
      : (clientX - left) / width;
  return Math.min(1, Math.max(0, ratio));
}

export function seekTime(fraction: number, duration: number): number {
  if (!Number.isFinite(duration) || duration <= 0) return 0;
  return Math.min(duration, Math.max(0, fraction * duration));
}

export function skipTarget(
  current: number,
  delta: number,
  duration: number,
): number {
  const target = current + delta;
  if (target < 0) return 0;
  if (Number.isFinite(duration) && duration > 0 && target > duration) {
    return duration;
  }
  return target;
}

export function nextRate(current: number): number {
  const index = PLAYBACK_RATES.indexOf(
    current as (typeof PLAYBACK_RATES)[number],
  );
  const next = PLAYBACK_RATES[(index + 1) % PLAYBACK_RATES.length];
  return next ?? 1;
}

export function formatRate(rate: number): string {
  return `${rate}×`;
}
