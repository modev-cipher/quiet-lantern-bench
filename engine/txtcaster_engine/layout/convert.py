import logging
import sys
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING

from txtcaster_engine.ingest.errors import IngestCancelled
from txtcaster_engine.ingest.pdf import open_document
from txtcaster_engine.layout.crops import render_crop
from txtcaster_engine.layout.regions import (
    Bbox,
    Region,
    best_match,
    is_decorative_bbox,
    merge_overlapping,
    refine_with_classifier,
    region_id,
    region_type_for_label,
)
from txtcaster_engine.residency import flush_accelerator_cache_mib, release_traceback_locals

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from docling.datamodel.accelerator_options import AcceleratorOptions
    from docling.datamodel.document import ConversionResult

CLASSIFIER_REPO_FOLDER = "docling-project--DocumentFigureClassifier-v2.5"

_CLASSIFIER_PATHS = (
    Path(CLASSIFIER_REPO_FOLDER) / "model.safetensors",
    Path(CLASSIFIER_REPO_FOLDER) / "config.json",
    Path(CLASSIFIER_REPO_FOLDER) / "preprocessor_config.json",
)


def classifier_available(artifacts_dir: str) -> bool:
    root = Path(artifacts_dir)
    return all((root / rel).is_file() for rel in _CLASSIFIER_PATHS)


def _accelerator_options(device: str) -> "AcceleratorOptions":
    from docling.datamodel.accelerator_options import AcceleratorDevice, AcceleratorOptions

    resolved = AcceleratorDevice.CUDA if device.startswith("cuda") else AcceleratorDevice.CPU
    return AcceleratorOptions(device=resolved)


def _disable_torch_compile() -> None:
    from docling.datamodel.settings import settings

    settings.inference.compile_torch_models = False


def _build_converter(artifacts_dir: str, with_classifier: bool, device: str = "cpu") -> object:
    from txtcaster_engine.tts.backend import configure_torch_threads

    configure_torch_threads()
    _disable_torch_compile()
    from docling.datamodel.base_models import InputFormat
    from docling.datamodel.picture_classification_options import (
        DocumentPictureClassifierOptions,
    )
    from docling.datamodel.pipeline_options import (
        PdfPipelineOptions,
        TableFormerMode,
        TableStructureOptions,
    )
    from docling.document_converter import DocumentConverter, PdfFormatOption

    pipeline_options = PdfPipelineOptions(
        artifacts_path=Path(artifacts_dir),
        do_ocr=False,
        do_table_structure=True,
        table_structure_options=TableStructureOptions(mode=TableFormerMode.FAST),
        accelerator_options=_accelerator_options(device),
    )
    if with_classifier:
        pipeline_options.do_picture_classification = True
        pipeline_options.picture_classification_options = (
            DocumentPictureClassifierOptions.from_preset("document_figure_classifier_v2")
        )
    return DocumentConverter(
        format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=pipeline_options)}
    )


def _prov_bbox_topleft(prov: object, page_height: float) -> Bbox:
    box = prov.bbox.to_top_left_origin(page_height)  # type: ignore[attr-defined]
    return (float(box.l), float(box.t), float(box.r), float(box.b))


def _classifier_candidates(
    result: "ConversionResult", target_page: int | None = None
) -> list[tuple[Bbox, str]]:
    document = result.document
    heights = {no: float(page.size.height) for no, page in document.pages.items()}
    candidates: list[tuple[Bbox, str]] = []
    for picture in document.pictures:
        if not picture.prov or picture.meta is None or picture.meta.classification is None:
            continue
        predictions = picture.meta.classification.predictions
        if not predictions:
            continue
        top = max(predictions, key=lambda p: p.confidence or 0.0)
        prov = picture.prov[0]
        page_no = int(prov.page_no)
        if target_page is not None and page_no != target_page:
            continue
        if page_no not in heights:
            continue
        candidates.append((_prov_bbox_topleft(prov, heights[page_no]), str(top.class_name)))
    return candidates


def _table_candidates(
    result: "ConversionResult", target_page: int | None = None
) -> list[tuple[Bbox, dict[str, int]]]:
    document = result.document
    heights = {no: float(page.size.height) for no, page in document.pages.items()}
    candidates: list[tuple[Bbox, dict[str, int]]] = []
    for table in document.tables:
        if not table.prov:
            continue
        prov = table.prov[0]
        page_no = int(prov.page_no)
        if target_page is not None and page_no != target_page:
            continue
        if page_no not in heights:
            continue
        meta = {"rows": int(table.data.num_rows), "cols": int(table.data.num_cols)}
        candidates.append((_prov_bbox_topleft(prov, heights[page_no]), meta))
    return candidates


def _cluster_regions(
    result: "ConversionResult",
    with_classifier: bool,
    target_page: int | None = None,
) -> list[Region]:
    classifier_by_bbox = (
        _classifier_candidates(result, target_page=target_page) if with_classifier else []
    )
    tables_by_bbox = _table_candidates(result, target_page=target_page)
    regions: list[Region] = []
    for page in result.pages:
        layout = page.predictions.layout
        if layout is None or page.size is None:
            continue
        page_no = int(page.page_no)
        if target_page is not None and page_no != target_page:
            continue
        width = float(page.size.width)
        height = float(page.size.height)
        for cluster in layout.clusters:
            region_type = region_type_for_label(str(cluster.label))
            if region_type is None:
                continue
            bbox: Bbox = (
                float(cluster.bbox.l),
                float(cluster.bbox.t),
                float(cluster.bbox.r),
                float(cluster.bbox.b),
            )
            meta: dict[str, int] | None = None
            if region_type == "figure":
                class_name = best_match(bbox, classifier_by_bbox)  # type: ignore[arg-type]
                if class_name is not None:
                    region_type = refine_with_classifier(str(class_name))
            elif region_type == "table":
                matched = best_match(bbox, tables_by_bbox)  # type: ignore[arg-type]
                if matched is not None:
                    meta = dict(matched)  # type: ignore[call-overload]
            if region_type != "decorative" and is_decorative_bbox(bbox, width, height):
                region_type = "decorative"
            regions.append(
                Region(
                    page=page_no,
                    bbox=bbox,
                    type=region_type,
                    confidence=float(cluster.confidence),
                    meta=meta,
                )
            )
    return merge_overlapping(regions)


def detect_regions(
    book_path: str,
    artifacts_dir: str,
    pages: list[int],
    content_hash: str,
    cache_dir: str,
    with_classifier: bool,
    on_page: Callable[[int, int], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
    device: str = "cpu",
) -> list[Region]:
    if is_cancelled is not None and is_cancelled():
        raise IngestCancelled()
    outer_error = sys.exc_info()[1]
    converter: object | None = None
    try:
        converter = _build_converter(artifacts_dir, with_classifier, device)
        wanted = sorted(set(pages))
        regions: list[Region] = []
        doc = open_document(book_path)
        try:
            total = len(wanted)
            for index, page_no in enumerate(wanted, start=1):
                if is_cancelled is not None and is_cancelled():
                    raise IngestCancelled()
                result = _convert_result(converter, book_path, (page_no, page_no))
                if is_cancelled is not None and is_cancelled():
                    raise IngestCancelled()
                for region in _cluster_regions(result, with_classifier, target_page=page_no):
                    region.page = page_no
                    region.id = region_id(content_hash, page_no, region.bbox, region.type)
                    if region.type != "decorative":
                        region.crop_png_path = render_crop(
                            doc, content_hash, page_no, region.bbox, cache_dir
                        )
                    regions.append(region)
                if is_cancelled is not None and is_cancelled():
                    raise IngestCancelled()
                if on_page is not None:
                    on_page(index, total)
        finally:
            doc.close()
        return regions
    except BaseException as error:
        release_traceback_locals(error, outer_error)
        raise
    finally:
        converter = None
        logger.info(
            "layout residency: freed=%dMiB pass=layout-endpoint", flush_accelerator_cache_mib()
        )


def _convert_result(
    converter: object, book_path: str, page_range: tuple[int, int]
) -> "ConversionResult":
    from docling.datamodel.base_models import ConversionStatus
    from docling.document_converter import DocumentConverter

    from txtcaster_engine.ingest.errors import IngestError

    assert isinstance(converter, DocumentConverter)
    try:
        result = converter.convert(book_path, raises_on_error=True, page_range=page_range)
    except IngestCancelled:
        raise
    except Exception as exc:
        raise IngestError("ERR_LAYOUT", f"docling layout failed: {exc}") from exc
    if result.status not in (ConversionStatus.SUCCESS, ConversionStatus.PARTIAL_SUCCESS):
        raise IngestError("ERR_LAYOUT", f"docling layout status: {result.status.value}")
    return result
