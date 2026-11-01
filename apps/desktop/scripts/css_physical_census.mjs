#!/usr/bin/env node
// I18N-1 §0 physical-CSS census: direction-bound properties/utilities that break
// RTL mirroring. Reports file:line:match; `--check` exits 1 on any hit.
// Usage: node scripts/css_physical_census.mjs [--check] [--scope <regex on path>]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

// CSS files + inline style objects: physical longhands and offsets.
const CSS_PHYSICAL =
  /\b(margin|padding|border|inset|scroll-margin|scroll-padding)-(left|right)\s*:|text-align\s*:\s*(left|right)|float\s*:\s*(left|right)|\b(marginLeft|marginRight|paddingLeft|paddingRight|borderLeft|borderRight|textAlign:\s*["'](left|right))/g;
// Bare `left:`/`right:` keys: CSS offsets in .css files; in TS they are either
// inline-style offsets (page-coordinate overlays, intentionally physical) or
// DOMRect reads — reported as "review", never gated.
const OFFSET_KEY = /(?<![-\w.])(left|right)\s*:/g;
// Tailwind utilities (v4) with a physical axis; logical twins are ms/me/ps/pe/start/end/text-start/…
const TW_PHYSICAL =
  /(?<=^|[\s"'`{])(?:[a-z-]+:)*-?(?:m[lr]|p[lr]|scroll-m[lr]|scroll-p[lr])-[\w.\[\]/]+|(?<=^|[\s"'`{])(?:[a-z-]+:)*-?(?:left|right)-(?:\d|\[|auto|full|px|1\/)[\w.\[\]/]*|(?<=^|[\s"'`{])(?:[a-z-]+:)*(?:text-left|text-right|float-left|float-right|origin-left|origin-right|rounded-[lr](?:-[\w]+)?|border-[lr](?:-\d+)?|divide-x(?:-\d+)?)(?=$|[\s"'`}])/g;

export function listFiles(dir) {
  const out = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, e.name);
    if (e.isDirectory()) out.push(...listFiles(full));
    else if (/\.(tsx?|css)$/.test(e.name) && !/\.test\.tsx?$/.test(e.name))
      out.push(full);
  }
  return out.sort();
}

export function physicalHits(file, text = fs.readFileSync(file, "utf8")) {
  const hits = [];
  text.split("\n").forEach((lineText, i) => {
    if (/i18n-physical-ok/.test(lineText)) return; // justified, recorded in the note
    const isCss = file.endsWith(".css");
    for (const re of [CSS_PHYSICAL, TW_PHYSICAL]) {
      re.lastIndex = 0;
      for (const m of lineText.matchAll(re))
        hits.push({ file, line: i + 1, match: m[0].trim(), gated: true });
    }
    OFFSET_KEY.lastIndex = 0;
    for (const m of lineText.matchAll(OFFSET_KEY))
      hits.push({ file, line: i + 1, match: m[0].trim(), gated: isCss });
  });
  return hits;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const check = args.includes("--check");
  const scopeIx = args.indexOf("--scope");
  const scope = scopeIx >= 0 ? new RegExp(args[scopeIx + 1]) : null;
  const files = listFiles(path.join(ROOT, "src")).filter(
    (f) => !scope || scope.test(path.relative(ROOT, f).split(path.sep).join("/")),
  );
  const hits = files.flatMap((f) => physicalHits(f));
  for (const h of hits)
    console.log(
      `${path.relative(ROOT, h.file)}:${h.line}\t${h.gated ? "gated" : "review"}\t${h.match}`,
    );
  const gated = hits.filter((h) => h.gated);
  const perFile = new Set(gated.map((h) => h.file));
  console.error(
    `${gated.length} gated physical hits in ${perFile.size} files, ${hits.length - gated.length} offset keys to review (of ${files.length} scanned)`,
  );
  if (check && gated.length > 0) process.exit(1);
}
