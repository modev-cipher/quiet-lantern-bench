// I18N-2 display-name layer for the user's OWN provider connections (BYOK).
//
// Ruling #25 Q3: provider names stay English in every locale — they are the
// names of services the user chose and pays for, shown where the user manages
// that connection (Providers page, key rows). Ruling #31: a provider or model
// name never appears in OUTCOME copy (what the tutor does, what to do next);
// catalogs are linted against these tokens (technical-terms.json,
// stackNamesForbidden). Copy that must name the user's provider renders the
// VALUE through providerDisplayName(), never a hard-coded vendor string.
export const PROVIDER_DISPLAY_NAMES: Readonly<Record<string, string>> = {
  gemini: "Google Gemini",
  google: "Google Gemini",
  deepgram: "Deepgram",
  cartesia: "Cartesia",
  openai_compat: "OpenAI-compatible (custom URL)",
};

/** The display name of a BYOK provider id; unknown ids fall back to the id
 * itself so a new backend provider is never rendered as "undefined". */
export function providerDisplayName(id: string): string {
  return PROVIDER_DISPLAY_NAMES[id] ?? id;
}
