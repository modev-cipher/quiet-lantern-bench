import { voiceMeta } from "./voiceModel";

export interface LanguageMismatch {
  book: string;
  voiceSpeaks: string | null;
}

const LANGUAGE_NAMES: Record<string, string> = {
  en: "English",
  es: "Spanish",
  fr: "French",
  de: "German",
  it: "Italian",
  pt: "Portuguese",
  hi: "Hindi",
  ja: "Japanese",
  zh: "Chinese",
  ko: "Korean",
  ar: "Arabic",
  he: "Hebrew",
};

export function languageName(code: string): string {
  return LANGUAGE_NAMES[code] ?? `"${code}"`;
}

export function normalizeLang(raw: string | null | undefined): string | null {
  if (raw == null) return null;
  const code = raw.trim().split(/[-_]/)[0]?.toLowerCase() ?? "";
  return code.length > 0 ? code : null;
}

export function languageAdvisory(
  bookLanguage: string | null | undefined,
  presetVoiceId: string | null | undefined,
  engineLanguages: readonly string[],
): LanguageMismatch | null {
  const book = normalizeLang(bookLanguage);
  if (book == null) return null;
  const meta = voiceMeta(presetVoiceId);
  if (meta != null) {
    if (meta.langCode === book) return null;
    return { book, voiceSpeaks: meta.langCode };
  }
  if (engineLanguages.length === 0) return null;
  if (engineLanguages.some((lang) => normalizeLang(lang) === book)) {
    return null;
  }
  return { book, voiceSpeaks: null };
}

export function advisoryCopy(mismatch: LanguageMismatch): string {
  const book = languageName(mismatch.book);
  if (mismatch.voiceSpeaks != null) {
    return `This book is in ${book}, but this voice speaks ${languageName(mismatch.voiceSpeaks)}. Pick a ${book} voice for a natural narration — or cast anyway.`;
  }
  return `This book is in ${book}, but this voice engine does not list ${book} among the languages it can speak. Pick a voice that speaks ${book} — or cast anyway.`;
}

export function reportAdvisoryNotice(
  book: string,
  voiceSpeaks: string | null,
): string {
  const bookName = languageName(book);
  if (voiceSpeaks != null) {
    return `This book is in ${bookName}, but the chosen voice speaks ${languageName(voiceSpeaks)} — check the narration.`;
  }
  return `This book is in ${bookName}, but the chosen voice engine does not list ${bookName} among the languages it can speak — check the narration.`;
}
