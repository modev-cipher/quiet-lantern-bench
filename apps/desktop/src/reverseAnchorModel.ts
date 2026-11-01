// The reverse-anchor consent gate, webview half (VOICE_AGENT §11.1, bound by
// the §5 per-first-use-per-book floor). Tapping/selecting an element during a
// live tutor session sends reading-position information ("the user is
// looking at X") to the provider — so the send path asks once per (book,
// endpoint), names the destination host, and is revocable in Settings. The
// enforcement itself is Rust-side (KeyManager::check_reverse_anchor_consent,
// the gate the session runtime must pass); this module carries the shared
// types, the interstitial copy, and the pure helpers the consent surface
// renders from. Without consent, taps send nothing — the tutor simply
// doesn't know.

export interface ReverseAnchorConsent {
  bookId: string;
  baseUrl: string;
  recordedAtMs: number;
}

export interface UrlCheck {
  canonicalUrl: string;
  consented: boolean;
}

export function needsReverseAnchorConsent(check: UrlCheck | null): boolean {
  return check === null || !check.consented;
}

export function reverseAnchorHostOf(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
}

/** The first-use interstitial copy: destination host + book, asked once. */
export function reverseAnchorConsentCopy(
  hostname: string,
  bookTitle: string,
): string {
  return (
    `Tapping a figure or passage tells the tutor where you are looking in ` +
    `"${bookTitle}", which sends that reading position to ${hostname}. ` +
    `Nothing is sent for taps until you allow it. This is asked once for ` +
    `this book, and you can revoke it in Settings at any time.`
  );
}

/** Settings-row label: prefer the book's title, fall back to its id. */
export function consentRowLabel(
  consent: ReverseAnchorConsent,
  titlesById: Map<string, string>,
): string {
  return titlesById.get(consent.bookId) ?? consent.bookId;
}

/** Stable order for the revocation list: title, then endpoint. */
export function sortedConsents(
  consents: ReverseAnchorConsent[],
  titlesById: Map<string, string>,
): ReverseAnchorConsent[] {
  return [...consents].sort((a, b) => {
    const byTitle = consentRowLabel(a, titlesById).localeCompare(
      consentRowLabel(b, titlesById),
    );
    return byTitle !== 0 ? byTitle : a.baseUrl.localeCompare(b.baseUrl);
  });
}
