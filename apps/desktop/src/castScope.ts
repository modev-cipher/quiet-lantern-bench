import { t } from "./i18n";

export type CastScope =
  | { kind: "chapters"; list: string; single: boolean }
  | { kind: "pages"; start: number; end: number };

const SEPARATOR = " - ";

export function castScopeOf(path: string): CastScope | null {
  const file = path.split(/[\\/]/).pop() ?? "";
  const stem = file.replace(/\.[^.]+$/, "");
  const at = stem.lastIndexOf(SEPARATOR);
  if (at < 0) return null;
  const label = stem.slice(at + SEPARATOR.length);

  const pages = /^Pages (\d+)-(\d+)$/.exec(label);
  if (pages) {
    return { kind: "pages", start: Number(pages[1]), end: Number(pages[2]) };
  }

  const chapters = /^Ch (.+)$/.exec(label);
  if (!chapters || /^(none|unknown)$/.test(chapters[1])) return null;
  const raw = chapters[1];
  return {
    kind: "chapters",
    list: raw.replace(/(\d)-(\d)/g, "$1–$2"),
    single: /^\d+$/.test(raw),
  };
}

export function castScopeLabel(scope: CastScope): string {
  if (scope.kind === "pages") {
    return t("cast.scope.pages", { start: scope.start, end: scope.end });
  }
  return scope.single
    ? t("cast.scope.chapter", { list: scope.list })
    : t("cast.scope.chapters", { list: scope.list });
}
