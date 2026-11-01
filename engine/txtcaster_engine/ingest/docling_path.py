import logging
import sys
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING

# Provenance (AGENTS.md §5 third-party integration rule):
# docling 2.113.0 / docling-core, verified against the official documentation
# on 2026-08-06: https://docling-project.github.io/docling/concepts/docling_document/
# The documentation describes `body` and `furniture` as two trees. The implementation
# does something different and the difference matters: furniture items are children of
# `#/body` like everything else, tagged `content_layer=ContentLayer.FURNITURE`, and the
# `furniture` tree is left empty. `iterate_items` filters on that tag, defaulting to
# `{BODY}`, which is why a default traversal returns no running heads at all.
# `included_content_layers={BODY, FURNITURE}` is the supported way to include them.
# Scanning the flat `texts` list instead would also pick up BACKGROUND, INVISIBLE and
# NOTES items and lose reading order. Guarded by a conformance test that runs a real
# conversion, because this behaviour is not what the prose documentation implies.
from txtcaster_engine.ingest import arabic_shaping
from txtcaster_engine.ingest.blocks import TextBlock
from txtcaster_engine.ingest.docling_support import RAPIDOCR_REPO_FOLDER
from txtcaster_engine.ingest.errors import IngestCancelled, IngestError
from txtcaster_engine.ingest.headers import ROLE_BY_LABEL as _ROLE_BY_LABEL
from txtcaster_engine.residency import flush_accelerator_cache_mib, release_traceback_locals

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from docling_core.types.doc.common.content_layer import ContentLayer
    from docling_core.types.doc.document import DoclingDocument

BODY_FONT_SIZE = 11.0
HEADING_FONT_SIZE = 22.0

_HEADING_LABELS = {"section_header", "title"}

_SKIPPED_LABELS = {"picture", "table", "document_index", "form", "key_value_region"}

_OCR_DET_PATH = "torch/PP-OCRv4/det/en_PP-OCRv3_det_mobile.pth"
_OCR_CLS_PATH = "torch/PP-OCRv4/cls/ch_ptocr_mobile_v2.0_cls_mobile.pth"
_OCR_REC_PATH = "torch/PP-OCRv4/rec/en_PP-OCRv4_rec_mobile.pth"
_OCR_KEYS_PATH = "paddle/PP-OCRv4/rec/en_PP-OCRv4_rec_mobile/en_dict.txt"
_OCR_FONT_PATH = "resources/fonts/FZYTK.TTF"


def _block_for_item(
    label: str, text: str, bbox: tuple[float, float, float, float]
) -> TextBlock | None:
    if label in _SKIPPED_LABELS or not text:
        return None
    text = arabic_shaping.letters(text)
    role = _ROLE_BY_LABEL.get(label, "body")
    font_size = HEADING_FONT_SIZE if label in _HEADING_LABELS else BODY_FONT_SIZE
    return TextBlock(
        bbox=bbox,
        text=text,
        font_size=font_size,
        role=role,
        reading_order=0,
        column=0,
        label=label,
    )


def _segment_texts(text: str, charspans: list[tuple[int, int]]) -> list[str]:
    if len(charspans) == 1:
        return [text.strip()]
    segments = [text[max(0, start) : max(0, end)].strip() for start, end in charspans]
    if not any(segments):
        segments[0] = text.strip()
    return segments


def _content_layers() -> set["ContentLayer"]:
    from docling_core.types.doc.common.content_layer import ContentLayer

    return {ContentLayer.BODY, ContentLayer.FURNITURE}


def _page_map_from_document(document: "DoclingDocument") -> dict[int, list[TextBlock]]:
    from docling_core.types.doc.items.text import TextItem

    layers = _content_layers()

    page_numbers = sorted(document.pages)
    heights = {no: float(document.pages[no].size.height) for no in page_numbers}
    pages: dict[int, list[TextBlock]] = {no: [] for no in page_numbers}

    def emit(item: "TextItem") -> None:
        segments = _segment_texts(item.text, [prov.charspan for prov in item.prov])
        for prov, segment in zip(item.prov, segments, strict=True):
            page_no = prov.page_no
            if page_no not in pages:
                continue
            top_left = prov.bbox.to_top_left_origin(heights[page_no])
            block = _block_for_item(
                str(item.label),
                segment,
                (float(top_left.l), float(top_left.t), float(top_left.r), float(top_left.b)),
            )
            if block is not None:
                block.reading_order = len(pages[page_no])
                pages[page_no].append(block)

    for item, _ in document.iterate_items(included_content_layers=layers):
        if not isinstance(item, TextItem) or not item.prov:
            continue
        emit(item)
    return pages


def _pages_from_document(document: "DoclingDocument") -> list[list[TextBlock]]:
    pages = _page_map_from_document(document)
    return [pages[no] for no in sorted(pages)]


def _build_converter(artifacts_dir: str) -> object:
    from txtcaster_engine.tts.backend import configure_torch_threads

    configure_torch_threads()
    from docling.datamodel.base_models import InputFormat
    from docling.datamodel.pipeline_options import PdfPipelineOptions, RapidOcrOptions
    from docling.document_converter import DocumentConverter, PdfFormatOption

    ocr_root = Path(artifacts_dir) / RAPIDOCR_REPO_FOLDER
    ocr_options = RapidOcrOptions(
        backend="torch",
        lang=["english"],
        det_model_path=str(ocr_root / _OCR_DET_PATH),
        cls_model_path=str(ocr_root / _OCR_CLS_PATH),
        rec_model_path=str(ocr_root / _OCR_REC_PATH),
        rec_keys_path=str(ocr_root / _OCR_KEYS_PATH),
        font_path=str(ocr_root / _OCR_FONT_PATH),
    )
    pipeline_options = PdfPipelineOptions(
        artifacts_path=Path(artifacts_dir),
        do_ocr=True,
        ocr_options=ocr_options,
    )
    return DocumentConverter(
        format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=pipeline_options)}
    )


def _convert_document(
    converter: object, book_path: str, page_range: tuple[int, int] | None
) -> "DoclingDocument":
    document, _ = _convert_document_status(converter, book_path, page_range)
    return document


def _convert_document_status(
    converter: object, book_path: str, page_range: tuple[int, int] | None
) -> tuple["DoclingDocument", bool]:
    """Convert, and report whether docling considered the conversion complete.

    `PARTIAL_SUCCESS` is accepted because a partly converted chunk still yields usable
    labels, but the caller has to know: treated as a success it stamps the book analysed and
    retires the retry for pages docling itself said it did not finish.
    """
    from docling.datamodel.base_models import ConversionStatus
    from docling.document_converter import DocumentConverter

    assert isinstance(converter, DocumentConverter)
    try:
        if page_range is None:
            result = converter.convert(book_path, raises_on_error=True)
        else:
            result = converter.convert(book_path, raises_on_error=True, page_range=page_range)
    except IngestCancelled:
        raise
    except Exception as exc:
        raise IngestError("ERR_OCR", f"docling conversion failed: {exc}") from exc
    if result.status not in (ConversionStatus.SUCCESS, ConversionStatus.PARTIAL_SUCCESS):
        raise IngestError("ERR_OCR", f"docling conversion status: {result.status.value}")
    return result.document, result.status == ConversionStatus.SUCCESS


CHUNK_PAGES = 25


def convert_pdf_accurate(
    book_path: str,
    artifacts_dir: str,
    on_page: Callable[[int, int], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
    page_count: int | None = None,
) -> list[list[TextBlock]]:
    if is_cancelled is not None and is_cancelled():
        raise IngestCancelled()
    if page_count is not None and page_count <= 0:
        return []

    outer_error = sys.exc_info()[1]
    converter: object | None = None
    all_pages: list[list[TextBlock]] = []
    try:
        converter = _build_converter(artifacts_dir)
        if page_count is None:
            document = _convert_document(converter, book_path, None)
            if is_cancelled is not None and is_cancelled():
                raise IngestCancelled()
            pages = _pages_from_document(document)
            if on_page is not None and pages:
                on_page(len(pages), len(pages))
            return pages

        for start in range(1, page_count + 1, CHUNK_PAGES):
            if is_cancelled is not None and is_cancelled():
                raise IngestCancelled()
            end = min(start + CHUNK_PAGES - 1, page_count)
            document = _convert_document(converter, book_path, (start, end))
            if is_cancelled is not None and is_cancelled():
                raise IngestCancelled()
            chunk_pages = _page_map_from_document(document)
            for page_no in range(start, end + 1):
                all_pages.append(chunk_pages.get(page_no, []))
            if on_page is not None:
                on_page(end, page_count)

        if len(all_pages) < page_count:
            all_pages.extend([] for _ in range(page_count - len(all_pages)))
        return all_pages[:page_count]
    except BaseException as error:
        release_traceback_locals(error, outer_error)
        raise
    finally:
        converter = None
        logger.info("layout residency: freed=%dMiB pass=extract", flush_accelerator_cache_mib())


def convert_pdf_pages_accurate(
    book_path: str,
    artifacts_dir: str,
    page_numbers: list[int],
    is_cancelled: Callable[[], bool] | None = None,
    on_page: Callable[[int, int], None] | None = None,
) -> dict[int, list[TextBlock]]:
    if not page_numbers:
        return {}
    if is_cancelled is not None and is_cancelled():
        raise IngestCancelled()
    outer_error = sys.exc_info()[1]
    converter: object | None = None
    try:
        converter = _build_converter(artifacts_dir)
        upgraded: dict[int, list[TextBlock]] = {}
        ordered = sorted(set(page_numbers))
        for idx, page_no in enumerate(ordered, 1):
            if is_cancelled is not None and is_cancelled():
                raise IngestCancelled()
            document = _convert_document(converter, book_path, (page_no, page_no))
            pages = _page_map_from_document(document)
            upgraded[page_no] = pages.get(page_no, [])
            if on_page is not None:
                on_page(idx, len(ordered))
        return upgraded
    except BaseException as error:
        release_traceback_locals(error, outer_error)
        raise
    finally:
        converter = None
        logger.info("layout residency: freed=%dMiB pass=upgrade", flush_accelerator_cache_mib())
