import { createElement, type ReactNode } from "react";

import {
  formatNumber,
  pluralCategory,
  type Locale,
  type PluralCategory,
} from "./localeModel";

// Message grammar (pinned in format.test.ts):
//   "{name}"      interpolate as text
//   "{name|ltr}"  LTR island: technical token, path, provider/model name — keeps its glyph order in RTL
//   "{name|n}"    number: Western digits, locale grouping, rendered as an LTR island
//   "{name|bdi}"  data of unknown script (book title, author): first-strong isolate
//   plural forms  { one: "…{count}…", other: "…" } selected by Intl.PluralRules(locale)
export type PluralForms = Partial<Record<PluralCategory, string>> & {
  other: string;
};
export type Message = string | PluralForms;
export type Params = Record<string, string | number>;

const TOKEN = /\{(\w+)(?:\|(ltr|n|bdi))?\}/g;

export function selectForm(
  locale: Locale,
  message: Message,
  params?: Params,
): string {
  if (typeof message === "string") return message;
  const count = Number(params?.count ?? 0);
  return message[pluralCategory(locale, count)] ?? message.other;
}

type Piece = { kind: "text" | "island" | "bdi"; text: string };

export function pieces(
  locale: Locale,
  message: Message,
  params: Params = {},
): Piece[] {
  const template = selectForm(locale, message, params);
  const out: Piece[] = [];
  let last = 0;
  for (const m of template.matchAll(TOKEN)) {
    if (m.index! > last)
      out.push({ kind: "text", text: template.slice(last, m.index) });
    const [, name, mode] = m;
    const raw = params[name];
    const text =
      raw == null
        ? `{${name}}`
        : mode === "n" && typeof raw === "number"
          ? formatNumber(locale, raw)
          : String(raw);
    out.push({ kind: mode === "bdi" ? "bdi" : mode ? "island" : "text", text });
    last = m.index! + m[0].length;
  }
  if (last < template.length)
    out.push({ kind: "text", text: template.slice(last) });
  return out;
}

const LRI = "\u2066"; // LEFT-TO-RIGHT ISOLATE
const FSI = "\u2068"; // FIRST STRONG ISOLATE
const PDI = "\u2069"; // POP DIRECTIONAL ISOLATE

/**
 * Plain string (attributes, title, aria-label, model copy): islands are wrapped
 * in Unicode isolates — LRI…PDI for `{x|ltr}` / `{x|n}`, FSI…PDI for `{x|bdi}` —
 * so a path, a token or a number inside Arabic keeps its glyph order wherever
 * the string is read (screen reader, tooltip, clipboard). Visible copy with
 * islands should use formatNodes, which isolates with <bdi> and carries NO
 * control characters in its text nodes.
 */
export function formatMessage(
  locale: Locale,
  message: Message,
  params?: Params,
): string {
  return pieces(locale, message, params)
    .map((p) =>
      p.kind === "island"
        ? `${LRI}${p.text}${PDI}`
        : p.kind === "bdi"
          ? `${FSI}${p.text}${PDI}`
          : p.text,
    )
    .join("");
}

/** Strip the isolate controls formatMessage adds (tests, plain-text sinks). */
export function stripIsolates(text: string): string {
  return text.replace(/[\u2066-\u2069]/g, "");
}

/** React children: islands become <bdi dir="ltr" class="ltr-island">, so glyph order and bounding boxes stay LTR. */
export function formatNodes(
  locale: Locale,
  message: Message,
  params?: Params,
): ReactNode[] {
  return pieces(locale, message, params).map((p, i) =>
    p.kind === "island"
      ? createElement(
          "bdi",
          { key: i, dir: "ltr", className: "ltr-island" },
          p.text,
        )
      : p.kind === "bdi"
        ? createElement("bdi", { key: i }, p.text)
        : p.text,
  );
}
