// Approved overnight sentences, pinned per locale so the DOM asserts are discriminating (not catalog-keyed):
// the lock is only attempted while rendering, and acquisition can fail — no "stays awake" promise.
export const OVERNIGHT_HINT: Record<"en" | "ar", string> = {
  en: "Queue overnight is pre-selected. We’ll try to keep your computer awake while rendering.",
  ar: "«سجّله ليلًا» مُحدَّد مسبقًا. سنحاول منع السكون أثناء إنشاء التسجيل.",
};
export const QUEUED_OVERNIGHT: Record<"en" | "ar", string> = {
  en: "Queued overnight. We’ll try to keep your computer awake while rendering.",
  ar: "في انتظار التسجيل الليلي. سنحاول منع السكون أثناء إنشاء التسجيل.",
};
/** Wording the approved copy must never regress to: an unconditional awake guarantee or a batch. */
export const OVERPROMISE =
  /stays awake|until (the|this) cast is done|overnight batch|يبقى جهازك مستيقظًا|حتى ينتهي/u;
