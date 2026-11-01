import json
import logging
from collections.abc import Callable
from dataclasses import dataclass, field

from txtcaster_engine.ingest import rtl_repair
from txtcaster_engine.ingest.blocks import TextBlock, extract_page
from txtcaster_engine.ingest.chapters import Chapter, detect_chapters
from txtcaster_engine.ingest.docling_support import check_artifacts, docling_available
from txtcaster_engine.ingest.epub import EpubStructure, is_epub, parse_epub_structure
from txtcaster_engine.ingest.errors import IngestCancelled
from txtcaster_engine.ingest.fallback.runner import FallbackRunner, FallbackSettings
from txtcaster_engine.ingest.headers import apply_roles, detect_repeated_regions
from txtcaster_engine.ingest.ingestor import Ingestor
from txtcaster_engine.ingest.language import detect_language
from txtcaster_engine.ingest.pdf import open_document
from txtcaster_engine.ingest.version import extractor_version

logger = logging.getLogger(__name__)

LANGUAGE_SAMPLE_PAGES = 10
MAX_UPGRADE_PAGES = 40
LABEL_MATCH_MIN_SHARE = 0.8


@dataclass
class IngestResult:
    book_id: str
    page_count: int
    is_scanned: bool
    path: str
    language: str | None
    title: str | None
    author: str | None
    chapters: list[Chapter]
    warnings: list[str]
    pages: list[list[TextBlock]] = field(repr=False)
    page_sizes: list[tuple[float, float]] = field(default_factory=list, repr=False)
    page_quality: list[dict[str, object]] = field(default_factory=list, repr=False)
    blocks_version: str = ""
    layout_pass_ran: bool = False
    """The layout model read this whole document.

    A positive signal, because absence of failure is not success: a scanned PDF never
    reaches the layout pass and an EPUB with a spine has no page geometry to analyse, and
    neither emits a degradation warning. A caller inferring completion from the absence of
    warnings marks both as analysed and retires the retry that would have helped.

    It answers only "did the model read the whole document", so it is **not** the absence of
    every warning. A geometry mismatch means the model read everything and the regions did
    not line up with the extracted text — running the identical pass again returns the
    identical mismatch, so leaving the book retryable offers a multi-hour no-op. That
    degradation rides the warnings channel instead. A partial pass is the opposite case:
    pages genuinely went unread, so re-running can still help and the flag stays false.
    """

    reflowed: bool = False
    """The source has no fixed pages, so it has no running heads to find."""

    fallback: dict[str, object] | None = None
    """Managed vision cascade record: per-book cost ledger and per-page outcome (ING-FALLBACK)."""

    scanned_pages: list[int] = field(default_factory=list)
    undecodable_pages: list[int] = field(default_factory=list)

    def to_manifest(self) -> dict[str, object]:
        return {
            "book_id": self.book_id,
            "page_count": self.page_count,
            "is_scanned": self.is_scanned,
            "path": self.path,
            "language": self.language,
            "title": self.title,
            "author": self.author,
            "chapters": [chapter.to_json() for chapter in self.chapters],
            "warnings": self.warnings,
            "blocks_version": self.blocks_version,
            "blocks_page_count": len(self.pages),
            "layout_pass_ran": self.layout_pass_ran,
            "reflowed": self.reflowed,
            "fallback": self.fallback,
            "scanned_pages": self.scanned_pages,
            "scanned_page_count": len(self.scanned_pages),
            "undecodable_pages": self.undecodable_pages,
        }

    def page_payloads(self) -> list[dict[str, object]]:
        payloads: list[dict[str, object]] = []
        for index, blocks in enumerate(self.pages):
            width, height = self.page_sizes[index] if index < len(self.page_sizes) else (0.0, 0.0)
            quality = self.page_quality[index] if index < len(self.page_quality) else {}
            payloads.append(
                {
                    "page_no": index + 1,
                    "width_pt": width,
                    "height_pt": height,
                    "needs_upgrade": needs_upgrade(quality),
                    "text_json": _page_envelope(blocks, quality),
                }
            )
        return payloads


def needs_upgrade(quality: dict[str, object]) -> bool:
    return bool(quality.get("multi_column")) or bool(quality.get("low_confidence"))


def _page_envelope(blocks: list[TextBlock], quality: dict[str, object]) -> str:
    return json.dumps(
        {
            "quality": quality,
            "blocks": [_raw_block(block) for block in blocks],
        },
        ensure_ascii=False,
        separators=(",", ":"),
    )


def _raw_block(block: TextBlock) -> dict[str, object]:
    """Persist raw extraction output only.

    `role` is dropped because it is recomputed at read time and is the field most likely
    to change. `label` is kept: it is what the layout model reported, it costs one short
    string on the pages a layout pass touched, and without it scripting cannot see what
    ingest saw and would narrate every running head the model had already identified.
    """
    payload = block.to_json()
    payload.pop("role", None)
    if payload.get("label") is None:
        payload.pop("label", None)
    return payload


def _report_page(
    on_page: Callable[..., None] | None,
    page_no: int,
    total: int,
    stage: str,
) -> None:
    if on_page is None:
        return
    try:
        on_page(page_no, total, stage=stage)
    except TypeError:
        on_page(page_no, total)


def _run_layout_pass(
    book_path: str,
    doc: Ingestor,
    pages: list[list[TextBlock]],
    page_count: int,
    upgrade_pages: set[int],
    docling_artifacts_dir: str | None,
    is_cancelled: Callable[[], bool] | None,
    on_page: Callable[..., None] | None,
) -> tuple[list[int], bool, list[str]]:
    """Read the layout model over the document: block labels plus page upgrades.

    Only on the fast path. OCR-extracted blocks already carry the model's labels, and an
    EPUB is reflowed to synthetic pages whose geometry bears no relation to the source
    file, so there is nothing to find and nothing to match against.

    Returns the pages that were upgraded, whether the model actually read the whole
    document, and any warnings to surface. Degradation is
    reported through the manifest, not only the log, because it is worth the difference
    between recall 1.000 and 0.093 and the user is the only one who can act on it.
    """
    if not _accurate_path_ready(docling_artifacts_dir):
        return (
            [],
            False,
            [
                "layout_suite_absent: running heads and page numbers are detected by text "
                "repetition alone, which finds a small fraction of them"
            ],
        )
    assert docling_artifacts_dir is not None
    from txtcaster_engine.ingest.layout_labels import run_layout_pass, stamp_labels

    def on_layout_page(page_no: int, total: int) -> None:
        _report_page(on_page, page_no, total, "layout")

    try:
        outcome = run_layout_pass(
            book_path,
            docling_artifacts_dir,
            page_count,
            upgrade_pages=upgrade_pages,
            on_page=on_layout_page if on_page is not None else None,
            is_cancelled=is_cancelled,
        )
    except IngestCancelled:
        raise
    except Exception as exc:
        logger.warning("layout pass unavailable (%s: %s)", type(exc).__name__, exc)
        return (
            [],
            False,
            [
                f"layout_pass_failed: {type(exc).__name__}; running heads are detected by "
                "text repetition alone for this book"
            ],
        )

    unresolved: list[int] = []
    repaired = 0
    for page_no, blocks in outcome.upgraded.items():
        if not 1 <= page_no <= len(pages):
            continue
        try:
            words = doc.page_text(page_no - 1).words
        except Exception as exc:
            logger.warning(
                "page %d: no word geometry to check the upgrade's RTL order (%s: %s)",
                page_no,
                type(exc).__name__,
                exc,
            )
            words = []
        fixed = rtl_repair.repair(blocks, words)
        repaired += fixed.repaired
        if fixed.unresolved:
            unresolved.append(page_no)
        pages[page_no - 1] = fixed.blocks
    if repaired:
        logger.info("layout pass: %d upgraded block(s) put back in RTL reading order", repaired)

    stamp = stamp_labels(pages, outcome.labels)
    warnings: list[str] = []
    if unresolved:
        warnings.append(
            f"rtl_order_unresolved: {sorted(unresolved)}; the layout model read right-to-left "
            "words in reverse there and the text layer could not confirm a repair"
        )
    if outcome.partial:
        warnings.append(
            f"layout_pass_partial: analysed {outcome.pages_read} of {page_count} page(s); "
            "the remainder falls back to text repetition"
        )
    if stamp.items and stamp.item_coverage < LABEL_MATCH_MIN_SHARE:
        warnings.append(
            f"layout_geometry_mismatch: only {stamp.items_matched} of {stamp.items} "
            "running-head region(s) lined up with the extracted text, so most running "
            "heads will still be read aloud"
        )
    logger.info(
        "layout pass: %d region(s) over %d page(s), %d furniture region(s) %.0f%% matched, "
        "%d block(s) labelled, %d page(s) upgraded",
        len(outcome.labels),
        outcome.pages_read,
        stamp.items,
        stamp.item_coverage * 100,
        stamp.blocks,
        len(outcome.upgraded),
    )
    return sorted(outcome.upgraded), not outcome.partial, warnings


def _narration_words(blocks: list[TextBlock]) -> int:
    return sum(len(block.text.split()) for block in blocks if block.role == "body")


def _normalize_language(value: str | None) -> str | None:
    if not value:
        return None
    code = value.strip().lower().split("-", 1)[0]
    return code or None


def _metadata_field(doc: Ingestor, key: str) -> str | None:
    value = doc.metadata().get(key)
    if not isinstance(value, str):
        return None
    cleaned = value.strip()
    return cleaned or None


def _body_text(pages: list[list[TextBlock]]) -> str:
    return " ".join(block.text for blocks in pages for block in blocks if block.role == "body")


def _extract_pages(
    doc: Ingestor,
    on_page: Callable[[int, int], None] | None,
    is_cancelled: Callable[[], bool] | None,
    fallback: FallbackRunner | None = None,
) -> tuple[list[list[TextBlock]], list[float], list[int], list[int], list[int], list[int]]:
    page_count = doc.page_count
    pages: list[list[TextBlock]] = []
    page_heights: list[float] = []
    multi_column_pages: list[int] = []
    low_confidence_pages: list[int] = []
    scanned_pages: list[int] = []
    undecodable_pages: list[int] = []
    for page_no in range(page_count):
        if is_cancelled is not None and is_cancelled():
            raise IngestCancelled()
        page, blocks, is_multi_column, low_confidence = extract_page(doc, page_no)
        if "scanned" in page.needs_fallback and not page.blocks:
            scanned_pages.append(page_no + 1)
        if "undecodable_text" in page.needs_fallback:
            undecodable_pages.append(page_no + 1)
        if fallback is not None:
            blocks = fallback.apply(page_no, page, blocks)
        pages.append(blocks)
        page_heights.append(doc.page_size(page_no)[1])
        if is_multi_column:
            multi_column_pages.append(page_no + 1)
        if low_confidence:
            low_confidence_pages.append(page_no + 1)
        _report_page(on_page, page_no + 1, page_count, "extract")
    if is_cancelled is not None and is_cancelled():
        raise IngestCancelled()
    return (
        pages,
        page_heights,
        multi_column_pages,
        low_confidence_pages,
        scanned_pages,
        undecodable_pages,
    )


def _unread_undecodable_warning(unread: int, page_count: int) -> str:
    return (
        f"undecodable_pages: {unread} of {page_count} page(s) show text whose fonts could not "
        "be decoded and were not read"
    )


def _unread_scanned_warning(unread: int, page_count: int, docling_artifacts_dir: str | None) -> str:
    text = (
        f"scanned_pages_unread: {unread} of {page_count} page(s) are images with no text "
        "layer and were not read"
    )
    if _accurate_path_ready(docling_artifacts_dir):
        return text
    return (
        text + "; install the 'Docling layout suite' model (docling-layout) to read them with OCR"
    )


def _fallback_warnings(runner: FallbackRunner | None) -> list[str]:
    if runner is None or not runner.results:
        return []
    ledger = runner.ledger
    warnings = [f"fallback_pages: {ledger.billed_pages} page(s), est_usd={ledger.total_usd:.4f}"]
    if runner.exhausted_pages():
        warnings.append(f"fallback_budget_exhausted: {runner.exhausted_pages()}")
    if runner.failed_pages():
        warnings.append(f"fallback_failed: {runner.failed_pages()}")
    return warnings


def _epub_toc_from_spine(
    structure: EpubStructure, doc: Ingestor
) -> tuple[list[list[object]], str | None]:
    chapter_count = doc.chapter_count()
    if chapter_count != len(structure.spine_hrefs):
        warning = (
            f"epub_spine_mismatch: parser found {len(structure.spine_hrefs)} spine documents "
            f"but renderer found {chapter_count}; falling back to heading detection"
        )
        return [], warning
    toc: list[list[object]] = []
    for entry in structure.toc:
        if not (0 <= entry.spine_index < chapter_count):
            continue
        page_no = doc.page_number_from_location(entry.spine_index) + 1
        toc.append([1, entry.title, page_no])
    return toc, None


def _ocr_pages(
    book_path: str,
    page_count: int,
    docling_artifacts_dir: str | None,
    on_page: Callable[..., None] | None,
    is_cancelled: Callable[[], bool] | None,
) -> list[list[TextBlock]]:
    from txtcaster_engine.ingest.docling_path import convert_pdf_accurate

    assert docling_artifacts_dir is not None

    def on_ocr_page(page_no: int, total: int) -> None:
        _report_page(on_page, page_no, total, "ocr")

    try:
        pages = convert_pdf_accurate(
            book_path,
            docling_artifacts_dir,
            on_ocr_page if on_page is not None else None,
            is_cancelled,
            page_count=page_count,
        )
    except TypeError:
        pages = convert_pdf_accurate(
            book_path,
            docling_artifacts_dir,
            on_ocr_page if on_page is not None else None,
            is_cancelled,
        )
    if len(pages) < page_count:
        pages = pages + [[] for _ in range(page_count - len(pages))]
    return pages[:page_count]


def _accurate_path_ready(docling_artifacts_dir: str | None) -> bool:
    """Whether the accurate (Docling) path can run for this ingest.

    Order matters and is pinned: the cheap, side-effect-free checks come first and
    `docling_available()` is the LAST condition, because it imports
    `docling.document_converter`, which loads PyTorch (measured 5-7 s and +500 MB RSS
    per process). The no-artifact fast path calls this on every book and must stay free
    of that import (ruling #36: zero GPU/PyTorch footprint on the fast path).
    """
    return (
        docling_artifacts_dir is not None
        and check_artifacts(docling_artifacts_dir).is_complete()
        and docling_available()
    )


def _upgrade_worklist(low_confidence_pages: list[int], scanned_pages: list[int]) -> set[int]:
    """Which weak pages the layout pass should also re-extract.

    An image-only page has no other text, and the pass converts every page anyway, so every
    scanned page is upgraded. `MAX_UPGRADE_PAGES` still bounds the low-confidence text pages,
    whose fast-path text is kept when the cap is exceeded; lifting that is a separate,
    measured change (R-AF).
    """
    worklist = set(scanned_pages)
    if len(low_confidence_pages) > MAX_UPGRADE_PAGES:
        logger.warning(
            "%d low-confidence page(s) exceed the %d-page upgrade cap, so none of them were "
            "upgraded; %d scanned page(s) still are",
            len(low_confidence_pages),
            MAX_UPGRADE_PAGES,
            len(worklist),
        )
        return worklist
    return worklist | set(low_confidence_pages)


def _upgrade_low_confidence_pages(
    book_path: str,
    pages: list[list[TextBlock]],
    low_confidence_pages: list[int],
    docling_artifacts_dir: str | None,
    is_cancelled: Callable[[], bool] | None,
    on_page: Callable[[int, int], None] | None = None,
) -> list[int]:
    if not low_confidence_pages:
        return []
    if not _accurate_path_ready(docling_artifacts_dir):
        logger.warning(
            "%d low-confidence page(s) will keep their fast-path text; the docling "
            "layout suite is not installed",
            len(low_confidence_pages),
        )
        return []
    if len(low_confidence_pages) > MAX_UPGRADE_PAGES:
        logger.warning(
            "%d low-confidence page(s) exceed the %d-page upgrade cap, so NONE were "
            "upgraded; a page count is a poor proxy for cost and this is the largest "
            "accuracy loss in ingest",
            len(low_confidence_pages),
            MAX_UPGRADE_PAGES,
        )
        return []
    from txtcaster_engine.ingest.docling_path import convert_pdf_pages_accurate

    assert docling_artifacts_dir is not None
    upgraded = convert_pdf_pages_accurate(
        book_path, docling_artifacts_dir, low_confidence_pages, is_cancelled, on_page
    )
    upgraded_pages: list[int] = []
    for page_no, blocks in sorted(upgraded.items()):
        if not blocks or not (1 <= page_no <= len(pages)):
            continue
        pages[page_no - 1] = blocks
        upgraded_pages.append(page_no)
    return upgraded_pages


def ingest_document(
    book_path: str,
    book_id: str,
    on_page: Callable[[int, int], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
    docling_artifacts_dir: str | None = None,
    docling_artifacts_sha: str | None = None,
    layout: bool = True,
    fallback: FallbackSettings | None = None,
) -> IngestResult:
    structure = parse_epub_structure(book_path) if is_epub(book_path) else None
    doc = open_document(book_path)
    try:
        page_count = doc.page_count
        warnings: list[str] = []
        layout_pass_ran = False
        runner = FallbackRunner.create(fallback, doc) if fallback is not None else None
        (
            pages,
            page_heights,
            multi_column_pages,
            low_confidence_pages,
            scanned_pages,
            undecodable_pages,
        ) = _extract_pages(doc, on_page, is_cancelled, runner)
        whole_book_scanned = (
            structure is None and page_count > 0 and len(scanned_pages) == page_count
        )
        is_scanned = (
            whole_book_scanned and runner is None and _accurate_path_ready(docling_artifacts_dir)
        )
        if is_scanned:
            pages = _ocr_pages(book_path, page_count, docling_artifacts_dir, on_page, is_cancelled)
            unresolved_layout_pages: list[int] = []
            upgraded_pages = list(range(1, page_count + 1))
            multi_column_pages = []
            low_confidence_pages = []
            warnings.append("scanned_pdf_ocr: text extracted with OCR; accuracy may vary")
        else:
            if scanned_pages and runner is not None:
                warnings.append(
                    "scanned_pdf_fallback: scanned pages read through the vision fallback"
                )
            warnings.extend(_fallback_warnings(runner))
            if structure is None and layout:
                upgraded_pages, layout_pass_ran, layout_warnings = _run_layout_pass(
                    book_path,
                    doc,
                    pages,
                    page_count,
                    _upgrade_worklist(
                        sorted(set(low_confidence_pages) - set(scanned_pages)), scanned_pages
                    ),
                    docling_artifacts_dir,
                    is_cancelled,
                    on_page,
                )
                warnings.extend(layout_warnings)
            elif structure is None:
                upgraded_pages = []
                warnings.append(
                    "layout_declined: imported without layout analysis, so running heads "
                    "and page numbers are detected by text repetition alone"
                )
            else:
                upgraded_pages = []
            unresolved_layout_pages = sorted(
                (set(multi_column_pages) | set(low_confidence_pages))
                - set(upgraded_pages)
                - set(scanned_pages)
            )
            unread = [page_no for page_no in scanned_pages if not pages[page_no - 1]]
            if unread:
                warnings.append(
                    _unread_scanned_warning(len(unread), page_count, docling_artifacts_dir)
                )
            read_by_fallback = set(runner.results) if runner is not None else set()
            undecoded = [p for p in undecodable_pages if p - 1 not in read_by_fallback]
            if undecoded:
                warnings.append(_unread_undecodable_warning(len(undecoded), page_count))
        repeated = detect_repeated_regions(pages, page_heights)
        for blocks, height in zip(pages, page_heights, strict=True):
            apply_roles(blocks, height, repeated)
        page_words = [_narration_words(blocks) for blocks in pages]
        if structure is not None:
            toc, spine_warning = _epub_toc_from_spine(structure, doc)
            if spine_warning is not None:
                warnings.append(spine_warning)
        else:
            toc = doc.toc()
        chapters = detect_chapters(toc, pages, page_words)
        sample_text = _body_text(pages[:LANGUAGE_SAMPLE_PAGES])
        if unresolved_layout_pages:
            warnings.append(f"multi_column_pages: {unresolved_layout_pages}")
        if upgraded_pages:
            warnings.append(f"multi_column_upgraded: {upgraded_pages}")
        title = _metadata_field(doc, "title")
        author = _metadata_field(doc, "author")
        language = detect_language(sample_text, script_sample=_body_text(pages))
        if structure is not None:
            title = structure.title or title
            author = structure.author or author
            language = _normalize_language(structure.language) or language
        if upgraded_pages and not is_scanned:
            logger.info("docling upgraded %d low-confidence page(s)", len(upgraded_pages))
        upgraded = set(upgraded_pages)
        multi_column = set(multi_column_pages)
        low_confidence = set(low_confidence_pages)
        scanned = set(scanned_pages)
        page_quality: list[dict[str, object]] = [
            {
                "source": "docling" if page_no in upgraded else doc.source_label,
                "multi_column": page_no in multi_column,
                "low_confidence": page_no in low_confidence or page_no in scanned,
                "scanned": page_no in scanned,
            }
            for page_no in range(1, len(pages) + 1)
        ]
        page_sizes = [doc.page_size(page_no) for page_no in range(min(page_count, len(pages)))]
        flagged = sum(1 for quality in page_quality if needs_upgrade(quality))
        declared = sum(1 for chapter in chapters if not chapter.is_pseudo)
        logger.info(
            "extracted %d page(s) via %s: %d image-only, %d flagged for upgrade (%d multi-column, "
            "%d low-confidence), %s, version=%s",
            len(pages),
            "ocr" if is_scanned else "fast",
            len(scanned),
            flagged,
            len(multi_column),
            len(low_confidence),
            f"{declared} declared chapter(s)"
            if declared
            else f"no declared structure, {len(chapters)} pseudo window(s)",
            extractor_version(docling_artifacts_sha),
        )
        return IngestResult(
            book_id=book_id,
            page_count=page_count,
            is_scanned=is_scanned,
            path="accurate" if is_scanned else "fast",
            language=language,
            title=title,
            author=author,
            chapters=chapters,
            warnings=warnings,
            pages=pages,
            page_sizes=page_sizes,
            page_quality=page_quality,
            blocks_version=extractor_version(docling_artifacts_sha),
            layout_pass_ran=layout_pass_ran,
            fallback=runner.summary() if runner is not None else None,
            reflowed=is_epub(book_path),
            scanned_pages=scanned_pages,
            undecodable_pages=undecodable_pages,
        )
    finally:
        doc.close()
