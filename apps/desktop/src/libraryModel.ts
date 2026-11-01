import { t } from "./i18n";

export interface BookSummary {
  format?: string;
  id: string;
  title: string;
  author: string | null;
  pageCount: number | null;
  chapterCount: number;
  language: string | null;
  isScanned: boolean;
  coverPath: string | null;
  createdAt: number;
  hasPseudoChapters: boolean;
  layoutAnalysed: boolean;
  reflowed: boolean;
  blocksStale: boolean;
}

export interface ImportResult {
  book: BookSummary;
  warnings: string[];
}

export interface ImportProgress {
  type: "progress" | "done";
  page?: number;
  total?: number;
  stage?: string;
}

export interface ActiveImport {
  importId: string;
  filename: string;
  page: number;
  total: number;
  stage: string;
}

// Books imported before placeholder metadata was filtered at ingest can still
// carry a PDF producer's literal "Title"/"Author" strings; the original
// filename was not persisted for them, so display falls back to t("library.untitled").
function isPlaceholder(value: string, field: "title" | "author"): boolean {
  return value.trim().toLowerCase() === field;
}

export function bookDisplayTitle(book: BookSummary): string {
  return isPlaceholder(book.title, "title") ? t("library.untitled") : book.title;
}

export function bookDisplayAuthor(book: BookSummary): string | null {
  if (book.author == null || isPlaceholder(book.author, "author")) return null;
  return book.author;
}

export function bookMeta(book: BookSummary): string {
  const parts: string[] = [];
  if (book.pageCount != null) {
    parts.push(t("library.pageCount", { count: book.pageCount }));
  }
  if (book.chapterCount > 0) {
    parts.push(
      t(
        book.hasPseudoChapters ? "library.sectionCount" : "library.chapterCount",
        { count: book.chapterCount },
      ),
    );
  }
  return parts.join(" · ");
}

export function importProgressLabel(progress: ImportProgress | null): string {
  if (!progress) return t("library.importing");
  if (progress.type === "done") return t("library.finishing");
  if (progress.total && progress.total > 0) {
    if (progress.stage === "ocr") {
      return t("library.ocrPages", {
        page: progress.page ?? 0,
        total: progress.total,
      });
    }
    if (progress.stage === "layout") {
      return t("library.layoutPages", {
        page: progress.page ?? 0,
        total: progress.total,
      });
    }
    return t("library.readingPages", {
      page: progress.page ?? 0,
      total: progress.total,
    });
  }
  return t("library.importing");
}

function scopeLabel(warning: string): string {
  const counts = /(\d+) of (\d+) page/.exec(warning);
  return counts
    ? t("library.notice.pagesOf", { done: counts[1], total: counts[2] })
    : t("library.notice.somePages");
}

export function presentImportWarnings(warnings: string[]): string[] {
  const notices: string[] = [];
  for (const warning of warnings) {
    if (warning.startsWith("scanned_pdf_ocr")) {
      notices.push(
        t("library.notice.scannedOcr"),
      );
    } else if (warning.startsWith("multi_column_upgraded")) {
      notices.push(
        t("library.notice.multiColumn"),
      );
    } else if (warning.startsWith("scanned_pages_unread")) {
      const scope = scopeLabel(warning);
      // The engine appends "; install the … model … to read them with OCR"
      // when the accurate path is not installed. #31 public-copy law: the
      // notice names the destination and the model's ROLE (the Models
      // panel's own public name), never the model or engine.
      notices.push(
        /;\s*install\b/.test(warning)
          ? t("library.notice.scannedSkippedInstall", { scope })
          : t("library.notice.scannedSkipped", { scope }),
      );
    } else if (warning.startsWith("undecodable_pages")) {
      const scope = scopeLabel(warning);
      notices.push(t("library.notice.undecodable", { scope }));
    }
  }
  return notices;
}

export function presentImportError(raw: string): string {
  if (raw.startsWith("ERR_DRM")) {
    return t("library.importError.drm");
  }
  if (raw.startsWith("ERR_SCANNED")) {
    return t("library.importError.scanned");
  }
  if (raw.startsWith("ERR_DUPLICATE")) {
    const message = raw.slice("ERR_DUPLICATE:".length).trim();
    return message || t("library.importError.duplicate");
  }
  if (raw.startsWith("ERR_OPEN")) {
    return t("library.importError.open");
  }
  return t("library.importError.generic", { reason: raw });
}

export function coverInitials(title: string): string {
  const words = title
    .split(/\s+/)
    .map((word) => word.replace(/[^\p{L}\p{N}]/gu, ""))
    .filter((word) => word.length > 0);
  return words
    .slice(0, 2)
    .map((word) => word[0]!.toUpperCase())
    .join("");
}

export const COVER_TONE_COUNT = 5;

export function coverTone(seed: string): number {
  let hash = 0;
  for (let index = 0; index < seed.length; index += 1) {
    hash = (hash * 31 + seed.charCodeAt(index)) | 0;
  }
  return Math.abs(hash) % COVER_TONE_COUNT;
}

export interface ImportPlan {
  pageCount: number;
  reflowed: boolean;
  layoutAvailable: boolean;
  layoutSeconds: number;
  onGpu: boolean;
  silent: boolean;
  recommendLayout: boolean;
  cpuSeconds?: number;
  offerBoth?: boolean;
}

export function approximateDuration(seconds: number): string {
  if (seconds < 90)
    return t("library.eta.seconds", { n: Math.max(1, Math.round(seconds)) });
  const minutes = seconds / 60;
  if (minutes < 90) return t("library.eta.minutes", { n: Math.round(minutes) });
  const hours = minutes / 60;
  return hours < 10
    ? t("library.eta.hours", { n: hours.toFixed(hours < 3 ? 1 : 0) })
    : t("library.eta.overHours", { n: Math.floor(hours) });
}

/**
 * What to say before an import that will take a while.
 *
 * The cost is paid at import and the benefit arrives at listening time, so the copy has
 * to name the consequence rather than the mechanism — "layout analysis" means nothing to
 * someone deciding whether to wait. The measured numbers do the persuading.
 */
// Copy lives in the catalog: library.layoutConsequence / library.staleConsequence.

/**
 * What this pass costs and what it buys, for the dialog that asks before spending it.
 *
 * The reason has to be told apart: a book that was never analysed is being offered running
 * heads, and a book analysed by an older extractor is being offered the labels this build
 * harvests. Telling the second one about running heads answers a question it did not ask.
 */
export function layoutPrompt(
  plan: ImportPlan,
  reason: "unanalysed" | "stale" = "unanalysed",
): {
  eta: string;
  cpuEta: string;
  consequence: string;
  heavy: boolean;
  offerBoth: boolean;
} | null {
  if (plan.silent) return null;
  return {
    eta: approximateDuration(plan.layoutSeconds),
    cpuEta: approximateDuration(plan.cpuSeconds ?? 0),
    consequence: t(
      reason === "stale" ? "library.staleConsequence" : "library.layoutConsequence",
    ),
    heavy: !plan.recommendLayout,
    offerBoth: plan.offerBoth ?? (!plan.onGpu && plan.layoutAvailable && !plan.reflowed),
  };
}

export interface ExtractionProgress {
  bookId: string;
  page: number;
  pages: number;
}

/**
 * Offer the re-read, mirroring `library::extraction_blocked_reason`'s first three arms.
 *
 * `layoutAnalysed` alone withheld it from exactly the books whose labels were oldest: a
 * book read by an earlier extractor carries none of the labels this build harvests, so
 * being analysed once is not the same as being up to date. The backend stays the
 * authority — this only decides whether the menu item is worth showing before asking.
 */
export function canImproveExtraction(book: BookSummary): boolean {
  const analysedAndCurrent = book.layoutAnalysed && !book.blocksStale;
  return !analysedAndCurrent && !book.reflowed && !book.isScanned;
}

export function deleteBookErrorMessage(raw: string): string {
  if (raw.startsWith("ERR_EXTRACTION_ACTIVE")) {
    return t("library.deleteError.rereading");
  }
  if (raw.startsWith("ERR_JOB_ACTIVE")) {
    return t("library.deleteError.jobActive");
  }
  return t("library.deleteError.generic");
}

export interface ActiveExtraction {
  bookId: string;
  page: number;
  pages: number;
}

export function extractionProgressLabel(
  progress: ExtractionProgress | null,
): string {
  if (!progress) return t("library.rereading");
  if (progress.pages > 0) {
    return t("library.rereadingPages", {
      page: progress.page,
      pages: progress.pages,
    });
  }
  return t("library.rereading");
}

export function extractionErrorMessage(raw: string): string {
  if (raw.startsWith("ERR_ALREADY_ANALYSED")) {
    return t("library.rereadError.alreadyAnalysed");
  }
  if (raw.startsWith("ERR_NO_LAYOUT_SUITE")) {
    return t("library.rereadError.noLayoutSuite");
  }
  if (raw.startsWith("ERR_REFLOWED")) {
    return t("library.rereadError.reflowed");
  }
  if (raw.startsWith("ERR_JOB_ACTIVE")) {
    return t("library.rereadError.jobActive");
  }
  if (raw.startsWith("ERR_EXTRACTION_ACTIVE")) {
    return t("library.rereadError.extractionActive");
  }
  if (raw.startsWith("ERR_CANCELLED")) {
    return t("library.rereadError.cancelled");
  }
  if (raw.startsWith("ERR_LEASE_LOST")) {
    return t("library.rereadError.leaseLost");
  }
  if (raw.startsWith("ERR_CLAIM_LOST")) {
    return t("library.rereadError.claimLost");
  }
  if (raw.startsWith("ERR_CHAPTERS_MOVED")) {
    return t("library.rereadError.chaptersMoved");
  }
  if (raw.startsWith("ERR_NO_BLOCKS")) {
    return t("library.rereadError.noBlocks");
  }
  if (raw.startsWith("ERR_NO_BOOK")) {
    return t("library.rereadError.noBook");
  }
  if (raw.startsWith("ERR_SCANNED")) {
    return t("library.rereadError.scanned");
  }
  return t("library.rereadError.generic");
}

export interface ExtractionPlan extends ImportPlan {
  blocked: string | null;
}

export interface ExtractionOutcome {
  pages: number;
  blocksVersion: string;
  warnings: string[];
  layoutPassRan: boolean;
}

export function extractionOutcomeNotice(
  outcome: ExtractionOutcome,
  layoutRequested: boolean = true,
): string | null {
  if (!layoutRequested) {
    return null;
  }
  if (outcome.layoutPassRan) {
    const mismatch = outcome.warnings.find((warning) =>
      warning.startsWith("layout_geometry_mismatch"),
    );
    return mismatch
      ? t("library.rereadOutcome.mismatch")
      : null;
  }
  const degraded = outcome.warnings.find((warning) =>
    warning.startsWith("layout_"),
  );
  if (!degraded) {
    return t("library.rereadOutcome.notRun");
  }
  if (degraded.startsWith("layout_pass_partial")) {
    return t("library.rereadOutcome.partial");
  }
  if (degraded.startsWith("layout_geometry_mismatch")) {
    return t("library.rereadOutcome.mismatch");
  }
  if (degraded.startsWith("layout_suite_absent")) {
    return t("library.rereadOutcome.suiteAbsent");
  }
  return t("library.rereadOutcome.failed");
}
