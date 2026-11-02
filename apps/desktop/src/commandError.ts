import { t, type MessageKey } from "./i18n";
import { storeTextForReader } from "./i18n/storeText";
import { sanitizeAndRouteErrorMessage } from "./jobModel";

const KNOWN_REFUSALS: Record<string, MessageKey> = {
  ERR_JOB_ACTIVE: "error.jobActive",
  ERR_EXTRACTION_ACTIVE: "error.extractionActive",
  ERR_NO_BOOK: "error.noBook",
};

const KNOWN_SENTENCES: ReadonlyArray<readonly [string, MessageKey]> = [
  ["wrong passphrase", "error.wrongPassphrase"],
  ["Give this voice a name", "error.nameTheVoice"],
  ["Enter the provider's voice id", "error.enterVoiceId"],
  ["Built-in voices cannot be deleted", "error.builtInVoice"],
  ["Another book import is already in progress", "error.importActive"],
  ["Cannot uninstall engine while render jobs are active", "error.engineInUse"],
  ["Cannot purge audio cache while render jobs are active", "error.audioInUse"],
];

export function commandErrorText(raw: unknown): string {
  const text = String(raw).replace(/^Error:\s*/, "");
  const sentence = KNOWN_SENTENCES.find(([words]) => text.includes(words));
  if (sentence != null) return t(sentence[1]);
  const code = /^(ERR_[A-Z0-9_]+)/.exec(text)?.[1];
  if (code != null && KNOWN_REFUSALS[code] != null) {
    return t(KNOWN_REFUSALS[code]);
  }
  if (text.includes("ERR_PROVIDER")) return sanitizeAndRouteErrorMessage(text);
  return storeTextForReader(text);
}
