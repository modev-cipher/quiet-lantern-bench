#!/usr/bin/env node
// I18N-1 gate C. EN: head vs baseline, any landmark moved/resized > tolerance is
// a regression (per-screen worse list). AR: each landmark present in EN must sit
// at the mirror position about the viewport centre within tolerance (or be
// listed as an accepted exception). Usage:
//   node scripts/i18n_geometry_diff.mjs en <baselineDir> <headDir> [tolPx=2]
//   node scripts/i18n_geometry_diff.mjs ar <enDir> <arDir> [tolPx=8]
import fs from "node:fs";
import path from "node:path";

const [mode, aDir, bDir, tolArg] = process.argv.slice(2);
const TOL = Number(tolArg ?? (mode === "ar" ? 8 : 2));
const load = (dir) =>
  Object.fromEntries(
    fs
      .readdirSync(dir)
      .filter((f) => f.endsWith(".json"))
      .map((f) => [
        f.replace(/\.json$/, ""),
        JSON.parse(fs.readFileSync(path.join(dir, f), "utf8")),
      ]),
  );
const byId = (shot) => {
  const m = new Map();
  for (const l of shot.landmarks) if (!m.has(l.testid)) m.set(l.testid, l);
  return m;
};

// Physical-by-design landmarks: page-coordinate overlays and media transport keep LTR geometry.
const AR_EXEMPT =
  /^(reading-facsimile|reading-page-overlay|agent-citation|reading-underline|reading-spotlight|player-seek)/;

const a = load(aDir),
  b = load(bDir);
let worse = 0;
for (const name of Object.keys(a).sort()) {
  if (!b[name]) {
    console.log(`${name}\tMISSING in ${bDir}`);
    worse++;
    continue;
  }
  const A = byId(a[name]),
    B = byId(b[name]);
  const W = a[name].viewport.width;
  const rows = [], info = [];
  for (const [id, la] of A) {
    const lb = B.get(id);
    if (!lb) {
      rows.push(`${id}\tmissing`);
      continue;
    }
    if (mode === "en") {
      const d = Math.max(
        Math.abs(la.x - lb.x),
        Math.abs(la.y - lb.y),
        Math.abs(la.w - lb.w),
        Math.abs(la.h - lb.h),
      );
      if (d > TOL)
        rows.push(
          `${id}\tΔ${d}px  en:${la.x},${la.y} ${la.w}×${la.h}  head:${lb.x},${lb.y} ${lb.w}×${lb.h}`,
        );
    } else {
      if (AR_EXEMPT.test(id)) continue;
      // Mirror = the box's anchored edge lands at the mirrored position: start edge
      // (EN left ↔ AR right) or end edge (EN right ↔ AR left). Translated text
      // changes widths and wrapped heights legitimately, so w/y are reported as info.
      const dxStart = Math.abs(W - la.x - (lb.x + lb.w));
      const dxEnd = Math.abs(W - (la.x + la.w) - lb.x);
      const dx = Math.min(dxStart, dxEnd);
      const dy = Math.abs(la.y - lb.y), dw = Math.abs(la.w - lb.w);
      const at = `en:${la.x},${la.y} ${la.w}×${la.h}  ar:${lb.x},${lb.y} ${lb.w}×${lb.h}`;
      if (dx > TOL) rows.push(`${id}\tmirror-Δ ${dx}px (start ${dxStart}, end ${dxEnd})  ${at}`);
      else if (dy > TOL || dw > TOL) info.push(`${id}\tinfo y:${dy} w:${dw}  ${at}`);
    }
  }
  const extra = [...B.keys()].filter((id) => !A.has(id));
  console.log(
    `## ${name}\t${A.size} landmarks\t${rows.length} worse${mode === "ar" ? `\t${info.length} info` : ""}${extra.length ? `\t+${extra.length} new` : ""}${mode === "ar" && b[name].htmlDir !== "rtl" ? "\tDIR NOT RTL" : ""}`,
  );
  for (const r of rows) console.log(`  ${r}`);
  if (process.env.GEOM_INFO) for (const r of info) console.log(`  ${r}`);
  worse += rows.length;
}
console.error(`${mode}: ${worse} worse rows, tolerance ${TOL}px`);
process.exit(worse ? 1 : 0);
