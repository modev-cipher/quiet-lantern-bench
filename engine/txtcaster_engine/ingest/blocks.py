from dataclasses import dataclass

from txtcaster_engine.ingest import bidi, textlayer
from txtcaster_engine.ingest.ingestor import Ingestor, PageText, RawBlock, RawWord

COLUMN_BAND_FRACTION = 0.08
COLUMN_STRADDLE_MAX_FRACTION = 0.15
ARTIFACT_MIN_SHARE = 0.9
FURNITURE_BAND_FRACTION = 0.12


@dataclass
class TextBlock:
    bbox: tuple[float, float, float, float]
    text: str
    font_size: float
    role: str
    reading_order: int
    column: int
    label: str | None = None
    """The layout model's own classification, when a layout pass produced this block.

    Distinct from `role`: a label is extraction output, as raw as `font_size`, and is
    persisted. `role` is computed at read time from labels plus heuristics and is not.
    """

    def to_json(self) -> dict[str, object]:
        return {
            "bbox": list(self.bbox),
            "text": self.text,
            "font_size": self.font_size,
            "role": self.role,
            "reading_order": self.reading_order,
            "column": self.column,
            "label": self.label,
        }


def _detect_columns(raw: list[RawBlock], page_width: float) -> tuple[list[int], bool, bool]:
    if len(raw) < 4 or page_width <= 0:
        return [0] * len(raw), False, False
    midpoint = page_width / 2
    band = page_width * COLUMN_BAND_FRACTION
    left = 0
    right = 0
    straddling = 0
    for bbox, _, _ in raw:
        if bbox[2] <= midpoint + band:
            left += 1
        elif bbox[0] >= midpoint - band:
            right += 1
        else:
            straddling += 1
    two_sided = left >= 2 and right >= 2
    two_columns = two_sided and straddling / len(raw) <= COLUMN_STRADDLE_MAX_FRACTION
    if not two_columns:
        return [0] * len(raw), False, two_sided
    columns = [0 if (bbox[0] + bbox[2]) / 2 < midpoint else 1 for bbox, _, _ in raw]
    return columns, True, straddling > 0


def _artifact_labels(
    raw: list[RawBlock], shares: tuple[float, ...], page_height: float
) -> list[str | None]:
    labels: list[str | None] = [None] * len(raw)
    if len(shares) != len(raw) or page_height <= 0:
        return labels
    for i, ((_x0, y0, _x1, y1), _text, _size) in enumerate(raw):
        if shares[i] < ARTIFACT_MIN_SHARE:
            continue
        if y1 <= page_height * FURNITURE_BAND_FRACTION:
            labels[i] = "artifact_header"
        elif y0 >= page_height * (1 - FURNITURE_BAND_FRACTION):
            labels[i] = "artifact_footer"
    return labels


def extract_blocks(doc: Ingestor, index: int) -> tuple[list[TextBlock], bool, bool]:
    return extract_page(doc, index)[1:]


def extract_page(doc: Ingestor, index: int) -> tuple[PageText, list[TextBlock], bool, bool]:
    page = doc.page_text(index)
    raw: list[RawBlock] = list(page.blocks)
    words: list[RawWord] = list(page.words)
    page_width, page_height = doc.page_size(index)
    pdfium = getattr(doc, "backend", "") == "pdfium"
    if not pdfium:
        # C26: a table unit's member blocks are replaced by its cells (text rebuilt from word
        # geometry in the unit's order) before any ordering; a page without a unit is untouched
        spliced = textlayer.table_units_native(
            raw, words, list(getattr(doc, "page_shapes", lambda _i: ())(index))
        )
        if spliced is not None:
            raw, words = spliced
    columns, is_multi_column, low_confidence = _detect_columns(raw, page_width)
    cut: list[int] | None = None
    if pdfium:
        order = list(range(len(raw)))
        low_confidence = low_confidence or bool(page.needs_fallback)
    elif (
        cut := textlayer.column_order(
            raw,
            words,
            (0.0, 0.0, page_width, page_height),
            lambda: getattr(doc, "page_shapes", lambda _i: ())(index),
        )
    ) is not None:
        order = cut
    elif is_multi_column:
        rtl = bidi.base_direction("".join(text for _, text, _ in raw)) == "R"
        order = sorted(
            range(len(raw)),
            key=lambda i: (-columns[i] if rtl else columns[i], raw[i][0][1], raw[i][0][0]),
        )
    else:
        order = list(range(len(raw)))
    labels = _artifact_labels(raw, page.artifact_share, page_height)
    blocks: list[TextBlock] = []
    for reading_order, i in enumerate(order):
        bbox, text, font_size = raw[i]
        blocks.append(
            TextBlock(
                bbox=bbox,
                text=text,
                font_size=font_size,
                role="body",
                reading_order=reading_order,
                column=columns[i],
                label=labels[i],
            )
        )
    return page, blocks, is_multi_column, low_confidence
