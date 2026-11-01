// Pure model for the tutor's chat text: block structure, per-block text
// direction, and the streaming reveal step. No DOM, no React — the
// TutorRichText component renders what this returns.
//
// Why per-block direction (field session 2026-10-09): the tutor answers in
// Egyptian Arabic dense with English technical terms, and a numbered list
// item such as "1. Introduction: بنتعرف فيه على …" starts with a Latin
// word. `dir="auto"` (and <bdi>) pick the FIRST strong character, so that
// Arabic sentence rendered left-to-right with its words out of order. The
// direction here follows the words of the block instead.
//
// Markdown support is deliberately small and text-only (paragraphs, lists,
// headings, fenced code, **bold**, `code`): the model writes these, and
// everything renders as React text nodes — no HTML is ever interpreted.

export type TextDirection = "rtl" | "ltr";

export type TutorInline =
  | { kind: "text"; text: string }
  | { kind: "strong"; text: string }
  | { kind: "code"; text: string }
  /** `$…$` inline math: an LTR island, like code (review 2026-10-09, U4). */
  | { kind: "math"; text: string }
  /** A bare URL: an LTR island that may break anywhere (never a link). */
  | { kind: "url"; text: string };

export type TutorBlock =
  | {
      kind: "paragraph";
      dir: TextDirection;
      inlines: TutorInline[];
      /**
       * A later line of the same paragraph (review 2026-10-09, U3): each
       * line takes its own direction, so an English line inside an Arabic
       * paragraph is not aligned and ordered as Arabic.
       */
      continued?: boolean;
    }
  | { kind: "heading"; dir: TextDirection; inlines: TutorInline[] }
  | {
      kind: "item";
      dir: TextDirection;
      /** The marker as written ("1.", "2)") for ordered items; "•" otherwise. */
      marker: string;
      ordered: boolean;
      inlines: TutorInline[];
      /** Nesting depth from the line's indentation (0 = top level). */
      depth: number;
    }
  | { kind: "code"; text: string }
  /** A Markdown table, kept as written in an LTR block (U4). */
  | { kind: "table"; text: string };

const ARABIC_LETTER =
  /[\u0600-\u06FF\u0750-\u077F\u08A0-\u08FF\uFB50-\uFDFF\uFE70-\uFEFF]/u;
const LATIN_LETTER = /[A-Za-z\u00C0-\u024F]/u;
const WORD = /[\p{L}\p{M}]+/gu;

/**
 * The reading direction of one block of text, judged by its WORDS:
 * - only one script present → that script's direction;
 * - Arabic words at least as many as Latin ones → rtl (Arabic grammar
 *   carrying English terms: "الـ distributed system بيشتغل …");
 * - otherwise rtl only when the block OPENS in Arabic and Arabic still has
 *   a third of the words (an Arabic sentence about a titled book:
 *   "كتاب Distributed Systems لـ Maarten van Steen و");
 * - ltr otherwise, and for text with no letters at all.
 * Inline code never votes: `fetch()` inside Arabic prose is still Arabic.
 */
export function blockDirection(text: string): TextDirection {
  const prose = text
    .replace(/`[^`]*`/g, " ")
    .replace(/\$[^$\n]+\$/g, " ")
    .replace(URL_PATTERN, " ");
  let arabic = 0;
  let latin = 0;
  let first: TextDirection | null = null;
  for (const match of prose.matchAll(WORD)) {
    const word = match[0];
    if (ARABIC_LETTER.test(word)) {
      arabic += 1;
      first ??= "rtl";
    } else if (LATIN_LETTER.test(word)) {
      latin += 1;
      first ??= "ltr";
    }
  }
  if (arabic === 0) return "ltr";
  if (latin === 0) return "rtl";
  if (arabic >= latin) return "rtl";
  if (first === "rtl" && arabic * 2 >= latin) return "rtl";
  return "ltr";
}

const URL_PATTERN = /\bhttps?:\/\/[^\s<>()]+[^\s<>().,;:!?'"]/g;

/** Split plain text into text and URL inlines. */
function withUrls(text: string): TutorInline[] {
  const out: TutorInline[] = [];
  let at = 0;
  for (const match of text.matchAll(URL_PATTERN)) {
    const start = match.index ?? 0;
    if (start > at) out.push({ kind: "text", text: text.slice(at, start) });
    out.push({ kind: "url", text: match[0] });
    at = start + match[0].length;
  }
  if (at < text.length) out.push({ kind: "text", text: text.slice(at) });
  return out;
}

/**
 * Inline spans: **strong**, `code`, `$math$` and bare URLs; an unclosed
 * marker stays literal.
 */
export function parseInlines(text: string): TutorInline[] {
  const out: TutorInline[] = [];
  let plain = "";
  let at = 0;
  const flush = () => {
    if (plain !== "") out.push(...withUrls(plain));
    plain = "";
  };
  while (at < text.length) {
    if (text.startsWith("**", at)) {
      const close = text.indexOf("**", at + 2);
      if (close > at + 2) {
        flush();
        out.push({ kind: "strong", text: text.slice(at + 2, close) });
        at = close + 2;
        continue;
      }
    }
    if (text[at] === "$" && text[at + 1] !== "$") {
      const close = text.indexOf("$", at + 1);
      const body = close > at + 1 ? text.slice(at + 1, close) : "";
      // A price ("$5 and $10") is not math: math has no newline, does not
      // open on a space, and carries a symbol or a letter next to a digit.
      if (
        body !== "" &&
        !body.includes("\n") &&
        !/^\s|\s$/.test(body) &&
        /[=^_\\+*/<>]|[a-zA-Z]/.test(body)
      ) {
        flush();
        out.push({ kind: "math", text: body });
        at = close + 1;
        continue;
      }
    }
    if (text[at] === "`") {
      const close = text.indexOf("`", at + 1);
      if (close > at + 1) {
        flush();
        out.push({ kind: "code", text: text.slice(at + 1, close) });
        at = close + 1;
        continue;
      }
    }
    plain += text[at];
    at += 1;
  }
  flush();
  return out;
}

const ORDERED = /^(\s*)(\d{1,3}[.)])\s+(.*)$/;
const BULLET = /^(\s*)[-*•]\s+(.*)$/;
const TABLE_ROW = /^\s*\|.*\|\s*$/;

/** Indentation → nesting depth (two spaces or a tab per level, max 3). */
function depthOf(indent: string): number {
  const width = indent.replace(/\t/g, "  ").length;
  return Math.min(3, Math.floor(width / 2));
}
const HEADING = /^\s*#{1,4}\s+(.*)$/;
const FENCE = /^\s*```/;

/**
 * Split a tutor message into blocks. Lines inside a paragraph join with a
 * newline (the bubble keeps `white-space: pre-line`); a blank line, a list
 * item, a heading or a fence starts a new block. An unclosed fence (a reply
 * still streaming) runs to the end of the text.
 */
export function parseTutorBlocks(text: string): TutorBlock[] {
  const blocks: TutorBlock[] = [];
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  let paragraph: string[] = [];
  const flushParagraph = () => {
    const lines = paragraph.map((line) => line.trim()).filter((l) => l !== "");
    paragraph = [];
    lines.forEach((line, at) => {
      blocks.push({
        kind: "paragraph",
        dir: blockDirection(line),
        inlines: parseInlines(line),
        ...(at > 0 ? { continued: true } : {}),
      });
    });
  };
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (FENCE.test(line)) {
      flushParagraph();
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !FENCE.test(lines[i])) {
        code.push(lines[i]);
        i += 1;
      }
      blocks.push({ kind: "code", text: code.join("\n") });
      continue;
    }
    if (line.trim() === "") {
      flushParagraph();
      continue;
    }
    if (TABLE_ROW.test(line)) {
      flushParagraph();
      const rows: string[] = [];
      while (i < lines.length && TABLE_ROW.test(lines[i])) {
        rows.push(lines[i].trim());
        i += 1;
      }
      i -= 1;
      blocks.push({ kind: "table", text: rows.join("\n") });
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading != null) {
      flushParagraph();
      const body = heading[1].trim();
      blocks.push({
        kind: "heading",
        dir: blockDirection(body),
        inlines: parseInlines(body),
      });
      continue;
    }
    const ordered = ORDERED.exec(line);
    const bullet = ordered == null ? BULLET.exec(line) : null;
    if (ordered != null || bullet != null) {
      flushParagraph();
      const body = (ordered != null ? ordered[3] : bullet![2]).trim();
      blocks.push({
        kind: "item",
        dir: blockDirection(body),
        marker: ordered != null ? ordered[2] : "•",
        ordered: ordered != null,
        inlines: parseInlines(body),
        depth: depthOf((ordered ?? bullet)![1]),
      });
      continue;
    }
    paragraph.push(line);
  }
  flushParagraph();
  return blocks;
}

/** The direction a whole message opens with: its first block's, else ltr. */
export function messageDirection(text: string): TextDirection {
  for (const block of parseTutorBlocks(text)) {
    if (block.kind !== "code" && block.kind !== "table") return block.dir;
  }
  return "ltr";
}

/**
 * While a reply streams, hide a trailing markdown marker that has not
 * closed yet ("**bo", "`fetch") so the reader never sees raw asterisks
 * flash and vanish. Applied to the revealed prefix only.
 */
export function trimOpenMarkers(text: string): string {
  const strong = text.split("**").length - 1;
  let out = text;
  if (strong % 2 === 1) {
    const at = out.lastIndexOf("**");
    out = out.slice(0, at) + out.slice(at + 2);
  }
  const ticks = (out.match(/`/g) ?? []).length;
  if (ticks % 2 === 1 && !/```/.test(out)) {
    const at = out.lastIndexOf("`");
    out = out.slice(0, at) + out.slice(at + 1);
  }
  return out;
}

/**
 * One step of the streaming reveal: how many characters of `target` to
 * show after `elapsedMs`, starting from `shown`. The provider delivers a
 * reply in sentence-sized bursts; revealing each burst over a short glide
 * reads as writing instead of jumps. The rate scales with the backlog so
 * the reveal never falls more than about `CATCH_UP_MS` behind, and it
 * always lands on a word boundary (never half a word, never between an
 * Arabic letter and its marks).
 */
export const REVEAL_MIN_CHARS_PER_SECOND = 90;
export const CATCH_UP_MS = 450;

export function revealStep(
  target: string,
  shown: number,
  elapsedMs: number,
): number {
  if (shown >= target.length) return target.length;
  const backlog = target.length - shown;
  const rate = Math.max(
    REVEAL_MIN_CHARS_PER_SECOND,
    (backlog * 1000) / CATCH_UP_MS,
  );
  const raw = Math.min(
    target.length,
    shown + Math.max(1, Math.round((rate * elapsedMs) / 1000)),
  );
  if (raw >= target.length) return target.length;
  // Extend to the end of the word the raw cut fell into.
  const rest = target.slice(raw);
  const boundary = rest.search(/\s/);
  return boundary === -1 ? target.length : raw + boundary;
}
