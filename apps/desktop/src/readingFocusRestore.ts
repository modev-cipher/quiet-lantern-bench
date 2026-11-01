export const FOLLOW_ALONG_FALLBACK = '[data-testid="mini-player-follow-along"]';

export interface FocusTargetSource {
  querySelector: (selectors: string) => Element | null;
}

export interface FocusCandidate {
  isConnected: boolean;
  focus: () => void;
}

export function restoreTarget(
  previous: FocusCandidate | null,
  source: FocusTargetSource,
  fallbackSelector: string = FOLLOW_ALONG_FALLBACK,
): FocusCandidate | null {
  if (previous != null && previous.isConnected) return previous;
  const fallback = source.querySelector(fallbackSelector);
  if (fallback == null) return null;
  const candidate = fallback as unknown as FocusCandidate;
  if (typeof candidate.focus !== "function") return null;
  return candidate;
}

export function restoreFocus(
  previous: FocusCandidate | null,
  source: FocusTargetSource,
  fallbackSelector: string = FOLLOW_ALONG_FALLBACK,
): boolean {
  const target = restoreTarget(previous, source, fallbackSelector);
  if (target == null) return false;
  target.focus();
  return true;
}
