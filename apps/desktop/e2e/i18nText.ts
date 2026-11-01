// I18N-2: catalog-keyed expectations for e2e specs so a text assert is true in
// EVERY locale the suite runs in (I18N_E2E_LOCALE). Reads the JSON catalogs
// from disk (no TS JSON import in the Playwright loader); `{x}`/`{x|n}`/`{x|ltr}`/
// `{x|bdi}` islands are substituted plainly — React output carries no bidi
// controls, so textContent equals this.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const LOCALE = process.env.I18N_E2E_LOCALE === "ar" ? "ar" : "en";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const DIR = path.join(HERE, "..", "src", "i18n", "messages", LOCALE);

const catalog: Record<string, unknown> = Object.assign(
  {},
  ...fs
    .readdirSync(DIR)
    .filter((f) => f.endsWith(".json"))
    .map((f) => JSON.parse(fs.readFileSync(path.join(DIR, f), "utf8"))),
);

export const e2eLocale = LOCALE;

/** The visible text of a catalog message in the suite's locale. */
export function msg(
  key: string,
  params: Record<string, string | number> = {},
): string {
  const entry = catalog[key];
  // F11-U6 (additive): plural messages select their form by `count` the way
  // the app does (Intl.PluralRules for the suite's locale, `other` fallback).
  const raw =
    typeof entry === "object" && entry !== null
      ? ((entry as Record<string, string>)[
          new Intl.PluralRules(LOCALE).select(Number(params.count))
        ] ?? (entry as Record<string, string>).other)
      : entry;
  if (typeof raw !== "string")
    throw new Error(`${LOCALE} catalog has no plain key ${key}`);
  return raw.replace(/\{(\w+)(?:\|\w+)?\}/g, (_, name: string) => {
    const value = params[name];
    if (value === undefined) throw new Error(`${key}: missing param ${name}`);
    return typeof value === "number"
      ? new Intl.NumberFormat("en-US").format(value)
      : value;
  });
}

/**
 * U5 (additive): the i-th literal piece of a template, split on its
 * `{placeholders}` — for asserting copy whose islands carry runtime values.
 */
export function fragment(key: string, index: number): string {
  const raw = catalog[key];
  if (typeof raw !== "string")
    throw new Error(`${LOCALE} catalog has no plain key ${key}`);
  const pieces = raw.split(/\{[^}]+\}/);
  return pieces[index].trim();
}
