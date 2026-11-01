import type { BookSummary } from "./libraryModel";

export interface ExportSummary {
  id: string;
  bookId: string | null;
  jobId: string | null;
  path: string;
  bytes: number;
  format: string;
  mode: string;
  stale: boolean;
  createdAt: number;
  durationMs?: number | null;
  seekExact?: boolean | null;
}

export interface AudioShelfItem {
  id: string;
  bookId: string;
  title: string;
  path: string;
  mode: string;
  format: string;
  bytes: number;
  stale: boolean;
  isScanned: boolean;
  jobId: string | null;
  createdAt: number;
  durationMs?: number | null;
  seekExact?: boolean | null;
}

export function outputFormatOf(path: string, fallback: string): string {
  const match = /\.([a-z0-9]+)$/i.exec(path);
  return match ? match[1]!.toLowerCase() : fallback.toLowerCase();
}

export function isPlayable(format: string): boolean {
  return format === "mp3" || format === "wav" || format === "m4a";
}

export function canPlay(item: AudioShelfItem): boolean {
  return isPlayable(item.format) && !item.stale;
}

export function buildAudioShelf(
  exports: ExportSummary[],
  books: BookSummary[],
): AudioShelfItem[] {
  const byId = new Map(books.map((book) => [book.id, book]));
  return exports
    .map((row) => ({
      id: row.id,
      bookId: row.bookId ?? "",
      title:
        (row.bookId ? byId.get(row.bookId)?.title : undefined) ??
        audioFileName(row.path),
      path: row.path,
      mode: row.mode,
      format: outputFormatOf(row.path, row.format),
      bytes: row.bytes,
      stale: row.stale === true,
      isScanned:
        (row.bookId ? byId.get(row.bookId)?.isScanned : false) === true,
      jobId: row.jobId,
      createdAt: row.createdAt,
      durationMs: row.durationMs ?? null,
      seekExact: row.seekExact ?? null,
    }))
    .sort((a, b) => b.createdAt - a.createdAt);
}

export function durationParts(
  ms: number | null | undefined,
): { h: number; m: number } | null {
  if (ms == null || !Number.isFinite(ms) || ms <= 0) return null;
  const minutes = Math.max(1, Math.round(ms / 60_000));
  return { h: Math.floor(minutes / 60), m: minutes % 60 };
}

export function formatAudioSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "";
  const mb = bytes / (1024 * 1024);
  if (mb < 1) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
}

export function formatAudioDate(
  createdAt: number,
  locale: string = "en",
): string {
  if (!Number.isFinite(createdAt) || createdAt <= 0) return "";
  const date = new Date(createdAt);
  if (Number.isNaN(date.getTime())) return "";
  const intlLocale = locale === "ar" ? "ar-u-nu-latn" : "en-GB";
  return date.toLocaleDateString(intlLocale, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

export function audioFileName(path: string): string {
  const parts = path.split(/[/\\]/);
  return parts[parts.length - 1] ?? path;
}

const MODE_LABELS: Record<string, string> = {
  narrate: "narrator",
  teach: "teacher",
};

export function modeLabel(mode: string): string {
  return MODE_LABELS[mode] ?? mode;
}
