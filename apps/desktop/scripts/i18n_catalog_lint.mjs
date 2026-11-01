#!/usr/bin/env node
// I18N-1 catalog lint (pins in docs/notes/i18n-1-bilingual.md §2):
//  1. key parity EN↔AR in both directions (TS catches missing AR keys; this catches extras)
//  2. placeholder parity: every {name|mode} in EN appears in every AR form and vice versa
//  3. technical terms stay English: no forbidden Arabic rendering (technical-terms.json)
//  4. #31: no stack/library names in either catalog
//  5. no empty strings; plural forms carry `other`
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const MSG = path.join(ROOT, "src/i18n/messages");
const policy = JSON.parse(
  fs.readFileSync(path.join(ROOT, "src/i18n/technical-terms.json"), "utf8"),
);

function load(locale) {
  const out = {};
  for (const f of fs.readdirSync(path.join(MSG, locale)).sort()) {
    if (!f.endsWith(".json")) continue;
    for (const [k, v] of Object.entries(
      JSON.parse(fs.readFileSync(path.join(MSG, locale, f), "utf8")),
    )) {
      if (k in out)
        throw new Error(`duplicate key ${k} across ${locale} files`);
      out[k] = v;
    }
  }
  return out;
}

const forms = (m) => (typeof m === "string" ? [m] : Object.values(m));
const placeholders = (s) =>
  new Set([...s.matchAll(/\{(\w+)(?:\|\w+)?\}/g)].map((x) => x[1]));
const same = (a, b) => a.size === b.size && [...a].every((x) => b.has(x));

export function lint(en, ar) {
  const problems = [];
  for (const k of Object.keys(en))
    if (!(k in ar)) problems.push(`missing in ar: ${k}`);
  for (const k of Object.keys(ar))
    if (!(k in en)) problems.push(`extra in ar (not in en): ${k}`);
  for (const [k, m] of Object.entries(en)) {
    const enPh = placeholders(forms(m).join(" "));
    for (const f of forms(ar[k] ?? {})) {
      if (f.trim() === "") problems.push(`empty ar form: ${k}`);
      const arPh = placeholders(f);
      // plural forms may drop {count} for zero/one/two; nothing else may differ
      const dropped = [...enPh].filter((p) => !arPh.has(p) && p !== "count");
      const invented = [...arPh].filter((p) => !enPh.has(p));
      if (dropped.length || invented.length)
        problems.push(
          `placeholder mismatch ${k}: en ${[...enPh]} ar ${[...arPh]}`,
        );
    }
    if (typeof m !== "string" && !("other" in m))
      problems.push(`en plural without other: ${k}`);
    if (ar[k] && typeof ar[k] !== "string" && !("other" in ar[k]))
      problems.push(`ar plural without other: ${k}`);
    for (const f of forms(m))
      if (f.trim() === "") problems.push(`empty en form: ${k}`);
  }
  for (const [k, m] of Object.entries(ar)) {
    for (const f of forms(m))
      for (const [term, renderings] of Object.entries(
        policy.arabicRenderingsForbidden,
      ))
        for (const r of renderings)
          if (f.includes(r))
            problems.push(
              `ar ${k}: "${r}" renders ${term}, which stays English`,
            );
  }
  for (const [locale, cat] of [
    ["en", en],
    ["ar", ar],
  ])
    for (const [k, m] of Object.entries(cat))
      for (const f of forms(m))
        for (const name of policy.stackNamesForbidden)
          if (
            new RegExp(
              `(^|[^\\p{L}])${name.replace(".", "\\.")}([^\\p{L}]|$)`,
              "u",
            ).test(f)
          )
            problems.push(
              `${locale} ${k}: stack name "${name}" in UI copy (#31)`,
            );
  return problems;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const problems = lint(load("en"), load("ar"));
  for (const p of problems) console.log(p);
  console.error(
    `${problems.length} catalog problems (${Object.keys(load("en")).length} keys)`,
  );
  process.exit(problems.length ? 1 : 0);
}
