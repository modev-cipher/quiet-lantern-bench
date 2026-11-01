import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { presentImportError, presentImportWarnings } from "../libraryModel";
import { CATALOGS } from "./catalog";

import { BANNED_PUBLIC_TOKENS } from "./bannedPublicTokens";
import { readSource } from "../tests/readSource";

function catalogValues(): Array<[string, string]> {
  const rows: Array<[string, string]> = [];
  for (const [locale, catalog] of Object.entries(CATALOGS)) {
    for (const [key, value] of Object.entries(catalog)) {
      if (typeof value === "string") rows.push([`${locale}:${key}`, value]);
      else
        for (const [form, text] of Object.entries(value))
          rows.push([`${locale}:${key}.${form}`, String(text)]);
    }
  }
  return rows;
}

describe("public copy names no engine or model (#31)", () => {
  it("keeps every catalog string in every locale clean", () => {
    const leaks = catalogValues().filter(([, text]) =>
      BANNED_PUBLIC_TOKENS.test(text),
    );
    expect(leaks.map(([key]) => key)).toEqual([]);
  });

  it("keeps the Library import presenters clean for every engine advice", () => {
    const engineLines = [
      "scanned_pages_unread: 3 of 3 page(s) are images with no text layer and were not read; install the 'Docling layout suite' model (docling-layout) to read them with OCR",
      "scanned_pages_unread: 3 of 9 page(s) are images with no text layer and were not read",
      "scanned_pdf_ocr: text extracted with OCR; accuracy may vary",
      "multi_column_upgraded: [2, 5]",
      "undecodable_pages: 2 of 18 page(s) show text whose fonts could not be decoded and were not read",
    ];
    for (const notice of presentImportWarnings(engineLines)) {
      expect(notice).not.toMatch(BANNED_PUBLIC_TOKENS);
    }
    for (const raw of [
      "ERR_SCANNED: scanned images",
      "ERR_DRM: encrypted",
      "ERR_OPEN: bad",
    ]) {
      expect(presentImportError(raw)).not.toMatch(BANNED_PUBLIC_TOKENS);
    }
  });
});

// Ruling #31.3 + #25 Q2 extended to the documentation of the public repo
// (BLACKOUT-SWEEP, 2026-09-09). Scope is the RENDERED/product docs only. The
// engineering set below is EXCLUDED by owner decision (release-checklist item
// "docs split before repo goes public"); listing it here pins the scope.
const REPO_ROOT = new URL("../../../../", import.meta.url);
const PUBLIC_COPY_DOCS = [
  "README.md",
  "docs/README.md",
  "docs/ONBOARDING_UX.md",
  "AGENTS.md",
  "docs/design/audit.md",
  "docs/design/direction.md",
  "docs/design/logging-engine-storage.md",
  "docs/design/logging-voice.md",
  "docs/design/provider-proof-ledger.md",
];
const EXCLUDED_DEVELOPER_SURFACES = [
  "CONTRIBUTING.md", // build-from-source recipe (OWNER-1: relocated here from README)
  "NOTICE", // legal attribution — must name licensed libraries (OWNER-3)
  "docs/ARCHITECTURE.md", // engineering specs — OWNER-2, not rewritten
  "docs/TECH_STACK.md",
  "docs/PIPELINE_SPEC.md",
  "docs/DATA_MODELS.md",
  "docs/MODEL_CATALOG.md",
  "docs/HARDWARE_PROFILER.md",
  "docs/TASKS.md",
  "docs/notes", // slice mechanism notes — OWNER-2
  "engine/tests/corpus/stress", // test-corpus ledgers — OWNER-2
  "catalog/catalog.json", // machine catalog; UI names come from modelNames.ts
];

/**
 * Inline code is scanned as prose unless it has the shape of a command, path,
 * glob or log literal (Kritias FIX-FIRST on #329: blanket inline-code removal
 * let a whole paragraph in backticks hide a banned name). Fenced blocks stay
 * exempt: they are copy-pasteable commands, never rendered copy.
 */
export const CODE_LITERAL_SHAPES: readonly RegExp[] = [
  // one token carrying a path/glob/identifier mark: src-tauri/, *.gguf, --offline, engine::pdfium, a.b
  /^(?=\S+$)(?=.*(?:[/*_]|::|--|\.\w)).+$/,
  // command line with at least one flag: cargo test --lib, pnpm x build --config y.json
  /^[\w.-]+(?:\s+[\w@./*:=-]+)*\s+--?[\w-]+(?:=\S+)?(?:\s+[\w@./*:=-]+)*$/,
  // key/value log or config literal: target: "sidecar", level=debug
  /^[\w.-]+\s*[:=]\s*"?[\w.:-]+"?$/,
];
// Owner-mandated skill slugs: AGENTS.md sec.3.5 MUST name these skills, and
// some slugs embed a banned token (rust-best-practice). Exempt only an exact,
// fully backticked slug; never extend with banned shapes like gpt-4 — if
// sec.3.5 adds such a skill, pin its slug here in the same PR.
export const MANDATED_SKILL_SLUGS: readonly string[] = [
  "rust-best-practice",
  "tauri-v2",
];
export function isCodeLiteral(span: string): boolean {
  return (
    MANDATED_SKILL_SLUGS.includes(span) ||
    CODE_LITERAL_SHAPES.some((shape) => shape.test(span))
  );
}
/** Rendered prose: fenced blocks blanked; inline spans blanked only when they are code literals. */
export function proseOf(markdown: string): string {
  const blank = (m: string) => m.replace(/[^\n]/g, "");
  return markdown
    .replace(/```[\s\S]*?```/g, blank)
    .replace(/`([^`\n]*)`/g, (m, inner: string) =>
      isCodeLiteral(inner) ? blank(m) : inner,
    );
}

describe("inline-code boundary of the docs scan", () => {
  it("RED: a paragraph wrapped in backticks is still prose", () => {
    const para =
      "`The standard voice ships with Kokoro and reads every book aloud offline.`";
    expect(para.split(" ").length).toBeGreaterThanOrEqual(6);
    expect(proseOf(para)).toMatch(BANNED_PUBLIC_TOKENS);
  });
  it("RED: a bare banned name in backticks is still prose", () => {
    expect(proseOf("preset voice `Kokoro` is fast")).toMatch(
      BANNED_PUBLIC_TOKENS,
    );
    expect(proseOf("`Kokoro 82M`")).toMatch(BANNED_PUBLIC_TOKENS);
  });
  it("RED: banned slug lookalikes are not exempt by the skill allowlist", () => {
    expect(proseOf("style it with `gpt-4` today")).toMatch(
      BANNED_PUBLIC_TOKENS,
    );
    expect(proseOf("use `rust-best-practice-v2` here")).toMatch(
      BANNED_PUBLIC_TOKENS,
    );
  });
  it("GREEN: the exact mandated skill slug is exempt", () => {
    expect(proseOf("MUST invoke `rust-best-practice` first")).not.toMatch(
      BANNED_PUBLIC_TOKENS,
    );
  });
  it("GREEN: command, path, glob and log literals stay exempt", () => {
    const literals =
      "dir `src-tauri/` under `apps/desktop/src-tauri`, glob `*.gguf`, flag `--offline`, " +
      'id `target=engine::pdfium`, log `target: "sidecar"`, ' +
      "command `pnpm tauri build --config src-tauri/tauri.bundle.conf.json`";
    expect(proseOf(literals)).not.toMatch(BANNED_PUBLIC_TOKENS);
  });
});

describe("public repo docs name no engine, library, runtime or model (#31.3)", () => {
  it.each(PUBLIC_COPY_DOCS)("%s prose is clean", (rel) => {
    const lines = proseOf(readSource(new URL(rel, REPO_ROOT))).split("\n");
    const leaks = lines.flatMap((line, i) =>
      BANNED_PUBLIC_TOKENS.test(line)
        ? [`${rel}:${i + 1}: ${line.trim()}`]
        : [],
    );
    expect(leaks).toEqual([]);
  });

  it("pins the excluded developer surfaces so scope drift is visible", () => {
    for (const rel of EXCLUDED_DEVELOPER_SURFACES) {
      expect(existsSync(fileURLToPath(new URL(rel, REPO_ROOT))), rel).toBe(
        true,
      );
    }
    expect(
      PUBLIC_COPY_DOCS.filter((d) => EXCLUDED_DEVELOPER_SURFACES.includes(d)),
    ).toEqual([]);
  });
});
