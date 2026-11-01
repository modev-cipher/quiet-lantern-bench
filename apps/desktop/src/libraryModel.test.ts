import { t } from "./i18n";
import { describe, expect, it } from "vitest";

import { BANNED_PUBLIC_TOKENS } from "./i18n/bannedPublicTokens";

import {
  approximateDuration,
  bookDisplayAuthor,
  bookDisplayTitle,
  bookMeta,
  canImproveExtraction,
  coverInitials,
  coverTone,
  COVER_TONE_COUNT,
  deleteBookErrorMessage,
  extractionErrorMessage,
  extractionOutcomeNotice,
  extractionProgressLabel,
  type ExtractionOutcome,
  importProgressLabel,
  layoutPrompt,
  presentImportError,
  presentImportWarnings,
  type BookSummary,
  type ImportPlan,
} from "./libraryModel";

const base: BookSummary = {
  id: "b1",
  title: "Fixture Book",
  author: "Fixture Author",
  pageCount: 642,
  chapterCount: 51,
  language: "en",
  isScanned: false,
  coverPath: null,
  createdAt: 0,
  hasPseudoChapters: false,
  layoutAnalysed: false,
  reflowed: false,
  blocksStale: false,
};

describe("placeholder metadata fallback", () => {
  it("keeps real titles and authors untouched", () => {
    expect(bookDisplayTitle(base)).toBe("Fixture Book");
    expect(bookDisplayAuthor(base)).toBe("Fixture Author");
  });

  it("never renders the literal placeholder strings a bad PDF ships", () => {
    const bad = { ...base, title: "Title", author: "Author" };
    expect(bookDisplayTitle(bad)).toBe("Untitled");
    expect(bookDisplayAuthor(bad)).toBeNull();
    expect(bookDisplayTitle({ ...base, title: " TITLE " })).toBe("Untitled");
    expect(bookDisplayAuthor({ ...base, author: " author " })).toBeNull();
  });

  it("omits the author line when there is no author at all", () => {
    expect(bookDisplayAuthor({ ...base, author: null })).toBeNull();
  });

  it("does not swallow titles that merely contain the word", () => {
    expect(bookDisplayTitle({ ...base, title: "A Title of Sorts" })).toBe(
      "A Title of Sorts",
    );
    expect(bookDisplayAuthor({ ...base, author: "Ann Author" })).toBe(
      "Ann Author",
    );
  });
});

describe("bookMeta", () => {
  it("formats pages and chapters", () => {
    expect(bookMeta(base)).toBe("642 pages · 51 chapters");
  });

  it("uses singular forms", () => {
    expect(bookMeta({ ...base, pageCount: 1, chapterCount: 1 })).toBe(
      "1 page · 1 chapter",
    );
  });

  it("calls pseudo chapters sections", () => {
    expect(bookMeta({ ...base, hasPseudoChapters: true })).toBe(
      "642 pages · 51 sections",
    );
  });

  it("omits missing counts", () => {
    expect(bookMeta({ ...base, pageCount: null, chapterCount: 0 })).toBe("");
  });
});

describe("importProgressLabel", () => {
  it("defaults before any event", () => {
    expect(importProgressLabel(null)).toBe("Importing…");
  });

  it("shows page progress", () => {
    expect(importProgressLabel({ type: "progress", page: 10, total: 40 })).toBe(
      "Reading pages 10/40…",
    );
  });

  it("shows finishing after done", () => {
    expect(importProgressLabel({ type: "done" })).toBe("Finishing…");
  });

  it("shows ocr stage progress", () => {
    expect(
      importProgressLabel({
        type: "progress",
        page: 5,
        total: 20,
        stage: "ocr",
      }),
    ).toBe("OCR recognition 5/20…");
  });

  it("shows layout stage progress", () => {
    expect(
      importProgressLabel({
        type: "progress",
        page: 15,
        total: 20,
        stage: "layout",
      }),
    ).toBe("Analysing layout 15/20…");
  });
});

describe("presentImportError", () => {
  it("maps DRM errors to refusal copy", () => {
    expect(presentImportError("ERR_DRM: document is encrypted")).toContain(
      "DRM-protected",
    );
  });

  it("maps scanned errors", () => {
    expect(presentImportError("ERR_SCANNED: scanned images")).toContain(
      "scanned images",
    );
  });

  // AT-2-2 / #31 public-copy law: the refusal points at the Models panel by
  // the model's public ROLE name, never by model or engine name.
  it("names the OCR destination without any model or engine name", () => {
    const copy = presentImportError("ERR_SCANNED: scanned images");
    expect(copy).not.toMatch(BANNED_PUBLIC_TOKENS);
    expect(copy).toContain("Page layout model");
    expect(copy).toContain("Settings → Models");
  });

  it("keeps duplicate message", () => {
    expect(
      presentImportError("ERR_DUPLICATE: 'My Book' is already in your library"),
    ).toBe("'My Book' is already in your library");
  });

  it("wraps unknown errors", () => {
    expect(presentImportError("boom")).toBe("Import failed: boom");
  });
});

describe("presentImportWarnings", () => {
  it("maps unread scanned pages to the install-OCR notice", () => {
    const notices = presentImportWarnings([
      "scanned_pages_unread: 561 of 1039 page(s) are images with no text layer and were not read; install the 'Docling layout suite' model (docling-layout) to read them with OCR",
    ]);
    expect(notices).toHaveLength(1);
    expect(notices[0]).toContain("561 of 1039 pages");
    // AT-2-2 (round-2 QA): the engine's advice names the model; the
    // Library notice must not (#31). It names the role and the destination.
    expect(notices[0]).not.toMatch(BANNED_PUBLIC_TOKENS);
    expect(notices[0]).toContain("Page layout model");
    expect(notices[0]).toContain("Settings → Models");
  });

  it("omits the install advice when the engine gave none", () => {
    const notices = presentImportWarnings([
      "scanned_pages_unread: 3 of 9 page(s) are images with no text layer and were not read",
    ]);
    expect(notices).toHaveLength(1);
    expect(notices[0]).toContain("3 of 9 pages");
    expect(notices[0]).not.toContain("install");
    expect(notices[0]).not.toMatch(BANNED_PUBLIC_TOKENS);
  });

  it("maps undecodable font pages to the skipped-pages notice", () => {
    const notices = presentImportWarnings([
      "undecodable_pages: 2 of 18 page(s) show text whose fonts could not be decoded and were not read",
    ]);
    expect(notices).toHaveLength(1);
    expect(notices[0]).toContain("2 of 18 pages");
    expect(notices[0]).toContain("fonts");
  });

  it("renders no scanned notice without the warning", () => {
    expect(
      presentImportWarnings(["layout_declined: imported without layout analysis"]),
    ).toHaveLength(0);
  });

  it("maps scanned OCR warning to user copy", () => {
    const notices = presentImportWarnings([
      "scanned_pdf_ocr: text extracted with OCR; accuracy may vary",
    ]);
    expect(notices).toHaveLength(1);
    expect(notices[0]).toContain("scanned images");
    expect(notices[0]).toContain("OCR");
  });

  it("maps multi-column upgrade warning to user copy", () => {
    const notices = presentImportWarnings(["multi_column_upgraded: [2, 5]"]);
    expect(notices).toHaveLength(1);
    expect(notices[0]).toContain("multi-column");
  });

  it("ignores internal-only warnings", () => {
    expect(presentImportWarnings(["multi_column_pages: [88, 89]"])).toEqual(
      [],
    );
  });
});

describe("coverInitials", () => {
  it("takes first letters of two words", () => {
    expect(coverInitials("Fixture Book Title")).toBe("FB");
  });

  it("handles single word and punctuation", () => {
    expect(coverInitials("(untitled)")).toBe("U");
  });
});

describe("coverTone", () => {
  it("returns an index within range", () => {
    for (const seed of ["a", "Fixture Book", "book-1", ""]) {
      const tone = coverTone(seed);
      expect(tone).toBeGreaterThanOrEqual(0);
      expect(tone).toBeLessThan(COVER_TONE_COUNT);
    }
  });

  it("is deterministic for the same seed", () => {
    expect(coverTone("The Pragmatic Programmer")).toBe(
      coverTone("The Pragmatic Programmer"),
    );
  });

  it("varies across different seeds", () => {
    const tones = new Set(
      ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"].map(coverTone),
    );
    expect(tones.size).toBeGreaterThan(1);
  });
});

describe("approximateDuration", () => {
  it("keeps short waits in seconds", () => {
    expect(approximateDuration(30)).toBe("about 30 seconds");
  });

  it("switches to minutes once seconds stop being useful", () => {
    expect(approximateDuration(153)).toBe("about 3 minutes");
  });

  it("switches to hours rather than saying 116 minutes", () => {
    expect(approximateDuration(6982)).toBe("about 1.9 hours");
  });

  it("does not pretend to precision for very long waits", () => {
    expect(approximateDuration(15_340)).toBe("about 4 hours");
  });
});

describe("layoutPrompt", () => {
  const plan = (over: Partial<ImportPlan> = {}): ImportPlan => ({
    pageCount: 613,
    reflowed: false,
    layoutAvailable: true,
    layoutSeconds: 153,
    onGpu: true,
    silent: false,
    recommendLayout: true,
    ...over,
  });

  it("says nothing when the wait is not worth a decision", () => {
    expect(layoutPrompt(plan({ silent: true }))).toBeNull();
  });

  it("states the consequence, not the mechanism", () => {
    const prompt = layoutPrompt(plan());
    expect(prompt?.eta).toBe("about 3 minutes");
    expect(prompt?.consequence).toContain("read aloud");
    expect(prompt?.heavy).toBe(false);
  });

  it("tells a stale book what re-reading actually buys it", () => {
    const prompt = layoutPrompt(plan(), "stale");
    expect(prompt?.consequence).toBe(t("library.staleConsequence"));
    expect(prompt?.consequence).toContain("Audio you already made is untouched");
  });

  it("keeps the running-head reason for a book that was never analysed", () => {
    expect(layoutPrompt(plan())?.consequence).toBe(t("library.layoutConsequence"));
  });

  it("marks a CPU-bound wait as heavy so the dialog can pre-select declining", () => {
    const prompt = layoutPrompt(plan({ layoutSeconds: 6982, onGpu: false, recommendLayout: false }));
    expect(prompt?.eta).toBe("about 1.9 hours");
    expect(prompt?.heavy).toBe(true);
  });

  it("offers both options with CPU eta on CPU-bound machines", () => {
    const prompt = layoutPrompt(
      plan({
        layoutSeconds: 6982,
        cpuSeconds: 40,
        offerBoth: true,
        onGpu: false,
        recommendLayout: false,
      }),
    );
    expect(prompt?.eta).toBe("about 1.9 hours");
    expect(prompt?.cpuEta).toBe("about 40 seconds");
    expect(prompt?.heavy).toBe(true);
    expect(prompt?.offerBoth).toBe(true);
  });
});

describe("canImproveExtraction", () => {
  it("offers the action to a book imported without the layout pass", () => {
    expect(canImproveExtraction({ ...base, layoutAnalysed: false })).toBe(true);
  });

  it("withholds it once the layout model has read the book", () => {
    expect(
      canImproveExtraction({ ...base, layoutAnalysed: true, blocksStale: false }),
    ).toBe(false);
  });

  it("offers it again when a newer extractor would read the book differently", () => {
    expect(
      canImproveExtraction({ ...base, layoutAnalysed: true, blocksStale: true }),
    ).toBe(true);
  });
});

describe("extractionProgressLabel", () => {
  it("names the page it is on so an hours-long pass is not a blank spinner", () => {
    expect(
      extractionProgressLabel({ bookId: "b1", page: 275, pages: 613 }),
    ).toBe("Re-reading pages 275/613…");
  });

  it("falls back before the first event arrives", () => {
    expect(extractionProgressLabel(null)).toBe("Re-reading…");
  });

  it("does not print a denominator it does not have", () => {
    expect(extractionProgressLabel({ bookId: "b1", page: 3, pages: 0 })).toBe(
      "Re-reading…",
    );
  });
});

describe("extractionErrorMessage", () => {
  it.each([
    ["ERR_ALREADY_ANALYSED: ...", "already read with the layout model"],
    ["ERR_NO_LAYOUT_SUITE: ...", "layout model is not installed"],
    ["ERR_REFLOWED: ...", "no fixed pages"],
    ["ERR_JOB_ACTIVE: ...", "audio job is queued or running"],
    ["ERR_EXTRACTION_ACTIVE: ...", "already being re-read"],
    ["ERR_CANCELLED: ...", "kept its earlier text"],
    ["ERR_CHAPTERS_MOVED: ...", "chapter boundaries"],
    ["ERR_NO_BLOCKS: ...", "produced no text"],
    ["ERR_NO_BOOK: ...", "no longer in your library"],
    ["ERR_SCANNED: ...", "scanned images"],
    [
      "ERR_LEASE_LOST: another re-extraction took over this book, so this one stopped",
      "took over this book",
    ],
    [
      "ERR_CLAIM_LOST: this re-extraction lost its claim on the book and stopped",
      "lost its claim",
    ],
  ])("translates %s without leaking the code", (raw, expected) => {
    const message = extractionErrorMessage(raw);
    expect(message).toContain(expected);
    expect(message).not.toContain("ERR_");
  });

  it("does not echo an unrecognised backend failure to the user", () => {
    const message = extractionErrorMessage(
      "error returned from database: /home/someone/Books/private.pdf",
    );
    expect(message).not.toContain("/home/someone");
    expect(message).toBe("Re-reading failed. The book kept its earlier text.");
  });
});

describe("extractionOutcomeNotice", () => {
  const outcome = (
    layoutPassRan: boolean,
    warnings: string[] = [],
  ): ExtractionOutcome => ({
    pages: 613,
    blocksVersion: "extract=2;roles=1",
    warnings,
    layoutPassRan,
  });

  it("stays quiet only when the engine says the pass ran", () => {
    expect(extractionOutcomeNotice(outcome(true))).toBeNull();
    expect(
      extractionOutcomeNotice(outcome(true, ["scanned_pdf_ocr: ..."])),
    ).toBeNull();
  });

  it("tells a mismatched book that retrying will not help", () => {
    const notice = extractionOutcomeNotice(
      outcome(true, ["layout_geometry_mismatch: only 3 of 900"]),
    );
    expect(notice).toContain("same result");
  });

  it("reports a book the pass never ran on, even with no warning at all", () => {
    expect(extractionOutcomeNotice(outcome(false))).toContain(
      "did not read this book",
    );
  });

  it.each([
    ["layout_pass_partial: analysed 500 of 613", "Only part of this book"],
    ["layout_suite_absent: ...", "not installed"],
    ["layout_pass_failed: RuntimeError", "could not read this book"],
  ])("reports %s so the retry is not silently retired", (warning, expected) => {
    expect(extractionOutcomeNotice(outcome(false, [warning]))).toContain(
      expected,
    );
  });

  it("stays quiet when layout was not requested", () => {
    expect(extractionOutcomeNotice(outcome(false), false)).toBeNull();
  });
});

describe("canImproveExtraction", () => {
  it("withholds the offer from a reflowed book, which has no page furniture", () => {
    expect(
      canImproveExtraction({ ...base, layoutAnalysed: false, reflowed: true }),
    ).toBe(false);
  });

  it("withholds it from a scanned book rather than failing a round trip", () => {
    expect(
      canImproveExtraction({ ...base, layoutAnalysed: false, isScanned: true }),
    ).toBe(false);
  });
});

describe("deleteBookErrorMessage", () => {
  it("keeps the actionable half of a refused delete", () => {
    const message = deleteBookErrorMessage(
      "ERR_EXTRACTION_ACTIVE: this book's text is being re-extracted; stop that first",
    );
    expect(message).toContain("Stop that first");
    expect(message).toContain("delete");
    expect(message).not.toContain("ERR_");
  });

  it("tells someone to cancel the cast that is blocking the delete", () => {
    const message = deleteBookErrorMessage(
      "ERR_JOB_ACTIVE: an audio job is queued, running or paused for this book; cancel it first",
    );
    expect(message).toContain("Cancel it first");
    expect(message).not.toContain("ERR_");
  });

  it("does not tell someone their delete failed by talking about re-reading", () => {
    const message = deleteBookErrorMessage("error returned from database: locked");
    expect(message).toContain("delete");
    expect(message).not.toContain("Re-reading");
    expect(message).not.toContain("database");
  });
});
