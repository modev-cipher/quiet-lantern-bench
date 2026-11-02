import { t, type MessageKey } from "./i18n";
import { storeTextForReader } from "./i18n/storeText";
import { sanitizeAndRouteErrorMessage } from "./jobModel";

const KNOWN_REFUSALS: Record<string, MessageKey> = {
  ERR_JOB_ACTIVE: "error.jobActive",
  ERR_EXTRACTION_ACTIVE: "error.extractionActive",
  ERR_NO_BOOK: "error.noBook",
};

export function commandErrorText(raw: unknown): string {
  const text = String(raw).replace(/^Error:\s*/, "");
  const code = /^(ERR_[A-Z0-9_]+)/.exec(text)?.[1];
  if (code != null && KNOWN_REFUSALS[code] != null) {
    return t(KNOWN_REFUSALS[code]);
  }
  if (text.includes("ERR_PROVIDER")) return sanitizeAndRouteErrorMessage(text);
  return storeTextForReader(text);
}
