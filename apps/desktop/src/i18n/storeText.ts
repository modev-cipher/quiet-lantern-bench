import { t } from "./index";
import { getSnapshot } from "./localeStore";

const ARABIC_LETTER = /[\u0600-\u06FF]/;

export function storeTextForReader(text: string): string {
  const words = text.replace(/ERR_[A-Z0-9_]+:\s*/g, "").trim();
  if (getSnapshot().locale === "en" || ARABIC_LETTER.test(words)) return words;
  console.warn(text);
  return t("error.unexplained");
}
