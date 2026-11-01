#!/usr/bin/env node
// I18N-1 §0 string census: every user-visible string in src/**/*.tsx with
// file:line. Step 1 = JSX text nodes. Usage: node scripts/i18n_census.mjs [--json out]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SRC = path.join(ROOT, "src");

export const SURFACES = [
  ["Concepts (prototype)", /^(V2|v2r2|v2)/],
  [
    "Reader",
    /^(Reading|AudioPlayer|NowPlaying|MiniPlayer|PlaybackEngine|AgentCitation|reading|playback)/,
  ],
  ["Voice", /^(VoiceSession|CloneVoice|VoicePreview|voice|clone)/],
  [
    "Tutor/Ask",
    /^(BookAgent|StudyView|StudyCompanion|SessionView|study|tutor|f11|agent)/,
  ],
  ["History", /^(SessionHistory|sessionHistory)/],
  [
    "Modes/Wizard",
    /^(Wizard|JobModal|SetupChecklist|onboarding|jobForm|wizard)/,
  ],
  ["Jobs", /^(JobsPanel|job|castPhase)/],
  [
    "Settings",
    /^(Settings|storageSettings|learningPrivacy|reverseAnchor|theme|direction|teacherAnchor)/,
  ],
  [
    "Cast/Providers",
    /^(ProvidersPage|CastProvider|providers|providerGlyphs|cast)/,
  ],
  ["Models panel", /^(ModelsPanel|modelCatalog|setupChecklist)/],
  ["Activity tray", /^(ActivityTray|ShellTopBar)/],
  [
    "Library",
    /^(Library|BookMenu|TypographicCover|DeleteAction|library|audioShelf)/,
  ],
  ["Dialogs", /^(SendToPhone|ReviewPanel|lanSend|review|verify)/],
  ["Shell", /^(App|Icons|views|main)/],
];

export function surfaceOf(file) {
  const base = path.basename(file);
  for (const [name, re] of SURFACES) if (re.test(base)) return name;
  return "Other";
}

export function listSources(dir = SRC) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listSources(full));
    else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name))
      out.push(full);
  }
  return out.sort();
}

const HAS_LETTER = /\p{L}/u;
// Attributes whose value is read by users (screen readers, tooltips, inputs).
export const USER_ATTRS = new Set([
  "aria-label",
  "aria-description",
  "aria-valuetext",
  "aria-roledescription",
  "title",
  "placeholder",
  "alt",
  "label",
]);

function literalText(init, sf) {
  if (!init) return null;
  if (ts.isStringLiteral(init)) return init.text;
  if (ts.isJsxExpression(init) && init.expression) {
    const e = init.expression;
    if (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e))
      return e.text;
    if (ts.isTemplateExpression(e)) return e.getText(sf).replace(/\s+/g, " ");
    if (
      ts.isConditionalExpression(e) &&
      (ts.isStringLiteral(e.whenTrue) || ts.isStringLiteral(e.whenFalse))
    )
      return e.getText(sf).replace(/\s+/g, " ");
  }
  return null;
}

export function jsxTextStrings(file, text = fs.readFileSync(file, "utf8")) {
  const sf = ts.createSourceFile(
    file,
    text,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  const rows = [];
  const line = (node) =>
    sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1;
  const visit = (node) => {
    if (ts.isJsxText(node)) {
      const value = node.getText(sf).replace(/\s+/g, " ").trim();
      if (HAS_LETTER.test(value))
        rows.push({ kind: "jsx", file, line: line(node), text: value });
    } else if (
      ts.isJsxAttribute(node) &&
      USER_ATTRS.has(node.name.getText(sf))
    ) {
      const value = literalText(node.initializer, sf);
      if (value != null && HAS_LETTER.test(value))
        rows.push({
          kind: "attr",
          file,
          line: line(node),
          text: value,
          attr: node.name.getText(sf),
        });
    } else if (
      ts.isJsxExpression(node) &&
      node.expression &&
      node.parent &&
      ts.isJsxElement(node.parent)
    ) {
      const e = node.expression;
      if (
        (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e)) &&
        HAS_LETTER.test(e.text)
      )
        rows.push({ kind: "jsx", file, line: line(node), text: e.text });
      // I18N-2: `{cond ? "Resume" : "Listen"}` — one-word branches escaped both
      // passes (the literal pass wants ≥ 2 words); gate every string branch,
      // nested conditionals included.
      const branches = (x) =>
        ts.isConditionalExpression(x)
          ? [...branches(x.whenTrue), ...branches(x.whenFalse)]
          : ts.isStringLiteral(x) && HAS_LETTER.test(x.text)
            ? [x.text]
            : [];
      if (ts.isConditionalExpression(e))
        for (const text of branches(e))
          rows.push({ kind: "jsx", file, line: line(node), text });
      // I18N-2 R-K: a template child whose fixed parts read like a sentence is
      // hard-coded copy too (gated like JSX text).
      if (ts.isTemplateExpression(e)) {
        const fixed = [
          e.head.text,
          ...e.templateSpans.map((s) => s.literal.text),
        ]
          .join(" ")
          .replace(/\s+/g, " ")
          .trim();
        if (fixed.split(" ").length >= 4 && COPY_SHAPE.test(fixed))
          rows.push({
            kind: "jsx",
            file,
            line: line(node),
            text: e.getText(sf).replace(/\s+/g, " "),
          });
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return rows;
}

// Step 4 (#31): runtime identifiers rendered raw — a JSX text child or user attribute whose
// expression names an id/path/engine/model variable with no display-name mapping around it.
const RAW_ID =
  /(^|[^\w])(id|Id|modelId|voiceId|providerId|connectionId|engine|model|path|file|filename|url|hostname)\b/;
const MAPPED =
  /displayName|display_name|label|Label|title|name\b|t\(|tx\(|pathTail|formatRate|Name\(/;
export function rawIdStrings(file, text = fs.readFileSync(file, "utf8")) {
  const sf = ts.createSourceFile(
    file,
    text,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  const rows = [];
  const line = (node) =>
    sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1;
  const consider = (expr, where) => {
    const code = expr.getText(sf).replace(/\s+/g, " ");
    if (/<[A-Za-z]/.test(code)) return; // a conditional block of JSX, not a value
    if (RAW_ID.test(code) && !MAPPED.test(code))
      rows.push({
        kind: "rawid",
        file,
        line: line(expr),
        text: `${where}: {${code.slice(0, 90)}}`,
      });
  };
  const visit = (node) => {
    if (ts.isJsxExpression(node) && node.expression && node.parent) {
      if (ts.isJsxElement(node.parent) || ts.isJsxFragment(node.parent))
        consider(node.expression, "text");
      else if (
        ts.isJsxAttribute(node.parent) &&
        USER_ATTRS.has(node.parent.name.getText(sf))
      )
        consider(node.expression, node.parent.name.getText(sf));
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return rows;
}

// Step 3: copy hidden in plain string literals (label maps, hints, toasts,
// error copy). Heuristic: ≥2 words starting with a letter, outside excluded
// contexts (invoke/testid/className/keys/imports/t() calls). Kind "literal".
const EXCLUDED_PROPS = new Set([
  "className",
  "id",
  "key",
  "type",
  "role",
  "href",
  "src",
  "value",
  "name",
  "testId",
  "testid",
  "kind",
  "mode",
  "family",
  "engine",
  "format",
]);
const COPY_SHAPE = /^\p{L}[^\n]*\s\p{L}/u;
// Utility-class strings ("flex-col gap-1", "md:ml-0 hover:x") and CSS selectors are not copy.
const UTILITY_TOKEN =
  /^-?(?:[a-z-]+:)*[a-z0-9]+(?:-[a-z0-9.\[\]/%!'#]+)+$|^(?:[a-z-]+:)+[^\s]+$|^[a-z]+$/;
const looksLikeClasses = (text) =>
  text.split(/\s+/).every((tok) => UTILITY_TOKEN.test(tok)) && /-|:/.test(text);
const looksLikeSelector = (text) => /^[a-z]+(,\s*[a-z\[\]'=]+)+$/.test(text);

export function literalStrings(file, text = fs.readFileSync(file, "utf8")) {
  const sf = ts.createSourceFile(
    file,
    text,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  const rows = [];
  const line = (node) =>
    sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1;
  const excluded = (node) => {
    const p = node.parent;
    if (!p) return true;
    if (
      ts.isImportDeclaration(p) ||
      ts.isJsxAttribute(p) ||
      ts.isJsxExpression(p)
    )
      return true;
    if (ts.isPropertyAssignment(p) && EXCLUDED_PROPS.has(p.name.getText(sf)))
      return true;
    if (ts.isCallExpression(p)) {
      const callee = p.expression.getText(sf);
      if (
        /^(invoke|t|tx|listen|emit|console\.\w+|require|localStorage\.\w+|sessionStorage\.\w+|querySelector|getAttribute|setAttribute|describe|it|test|expect)$/.test(
          callee,
        )
      )
        return true;
    }
    if (
      ts.isBinaryExpression(p) &&
      p.operatorToken.kind === ts.SyntaxKind.EqualsEqualsEqualsToken
    )
      return true;
    if (ts.isCaseClause(p) || ts.isTypeNode(p) || ts.isLiteralTypeNode(p))
      return true;
    return false;
  };
  const visit = (node) => {
    // I18N-2: a template with substitutions is copy too when its fixed parts
    // read like a sentence (`${n} of ${m} chapters are in this cast…` reached
    // the AR DOM in English because this pass only saw plain literals).
    if (ts.isTemplateExpression(node) && !excluded(node)) {
      const fixed = [
        node.head.text,
        ...node.templateSpans.map((s) => s.literal.text),
      ]
        .join(" ")
        .replace(/\s+/g, " ")
        .trim();
      if (fixed.split(" ").length >= 4 && COPY_SHAPE.test(fixed))
        rows.push({
          kind: "literal",
          file,
          line: line(node),
          text: node.getText(sf).replace(/\s+/g, " "),
        });
    }
    if (
      (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) &&
      COPY_SHAPE.test(node.text) &&
      !looksLikeClasses(node.text) &&
      !looksLikeSelector(node.text) &&
      !excluded(node)
    )
      rows.push({
        kind: "literal",
        file,
        line: line(node),
        text: node.text.replace(/\s+/g, " "),
      });
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return rows;
}

// --check --scope <regex>: hard-coded JSX text / user attributes in scope files fail the lint.
// Exempt: technical terms and units (stay English by law), symbols/punctuation-only, brand.
const POLICY = JSON.parse(
  fs.readFileSync(path.join(ROOT, "src/i18n/technical-terms.json"), "utf8"),
);
const EXEMPT = new Set(POLICY.terms.map((t) => t.toLowerCase()));
export function exempt(text) {
  const words = text.split(/[\s·—–\-,.;:()/]+/).filter((w) => /\p{L}/u.test(w));
  if (text.trim().length <= 1) return true; // a lone glyph (brand mark) is not copy
  return (
    words.length === 0 ||
    words.every((w) => EXEMPT.has(w.toLowerCase()) || /^v?\d/.test(w))
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const check = args.includes("--check");
  const scopeIx = args.indexOf("--scope");
  const scope = scopeIx >= 0 ? new RegExp(args[scopeIx + 1]) : null;
  const files = listSources().filter(
    (f) => !scope || scope.test(path.relative(ROOT, f).split(path.sep).join("/")),
  );
  if (check) {
    const hard = files
      .flatMap((f) => jsxTextStrings(f))
      .filter((r) => !exempt(r.text));
    for (const r of hard)
      console.log(
        `${path.relative(ROOT, r.file)}:${r.line}\thard-coded ${r.kind}\t${r.text}`,
      );
    console.error(
      `${hard.length} hard-coded user-visible strings in ${files.length} scope files`,
    );
    process.exit(hard.length ? 1 : 0);
  }
  const rows = files
    .flatMap((f) => [
      ...jsxTextStrings(f),
      ...literalStrings(f),
      ...rawIdStrings(f),
    ])
    .sort((a, b) =>
      a.file === b.file ? a.line - b.line : a.file < b.file ? -1 : 1,
    );
  for (const r of rows)
    console.log(
      `${path.relative(ROOT, r.file)}:${r.line}\t${surfaceOf(r.file)}\t${r.kind}${r.attr ? ":" + r.attr : ""}\t${r.text}`,
    );
  const kinds = {};
  for (const r of rows) kinds[r.kind] = (kinds[r.kind] ?? 0) + 1;
  console.error(`${rows.length} strings ${JSON.stringify(kinds)}`);
}
