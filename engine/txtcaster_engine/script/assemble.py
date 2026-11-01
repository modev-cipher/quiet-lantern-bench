import re
from dataclasses import dataclass

from txtcaster_engine.geometry import bbox_within_page, block_coverage
from txtcaster_engine.ingest.blocks import TextBlock
from txtcaster_engine.script.cues import cue_opener
from txtcaster_engine.script.shape import looks_like_prose

HEADING_FONT_RATIO = 1.15
HEADING_MAX_WORDS = 12
SUPPRESSION_COVERAGE_MIN = 0.7
SUPPRESSION_CONFIDENCE_MIN = 0.5
SUPPRESSIBLE_REGION_TYPES = frozenset({"table", "equation", "figure", "chart"})
DECORATIVE_REGION_TYPE = "decorative"
EXPLANATION_UNAVAILABLE = "A detailed explanation was unavailable."


@dataclass
class SourceUnit:
    chapter_idx: int
    page_no: int
    kind: str
    text: str
    figure_id: str | None = None
    region_type: str | None = None
    caption: str | None = None
    explanation: str | None = None
    block_index: int | None = None
    label: str | None = None


@dataclass(frozen=True)
class FigureAnchor:
    region_id: str
    page_no: int
    bbox: tuple[float, float, float, float]
    region_type: str
    caption: str | None = None
    explanation: str | None = None
    confidence: float = 1.0


_FIGURE_LABEL_RE = re.compile(r"^\s*((?:figure|fig\.|table|equation)\s+\d+)\b", re.IGNORECASE)


def _figure_cue(figure: FigureAnchor) -> str:
    parts = [cue_opener(figure.region_type)]
    if figure.caption:
        parts.append(figure.caption.strip().rstrip(".") + ".")
    if figure.explanation:
        parts.append(figure.explanation.strip().rstrip(".") + ".")
    else:
        parts.append(EXPLANATION_UNAVAILABLE)
    return " ".join(parts)


def figure_prompt_block(unit: SourceUnit) -> str:
    lines = [f"type: {unit.region_type}"] if unit.region_type else []
    if unit.caption:
        lines.append(f"caption: {unit.caption.strip()}")
    if unit.explanation:
        lines.append(f"description: {unit.explanation.strip()}")
    return "\n".join(lines)


def _suppressing_regions(
    figures: list[FigureAnchor],
    page_size: tuple[float, float] | None,
) -> list[FigureAnchor]:
    regions = [
        figure
        for figure in figures
        if figure.region_type in SUPPRESSIBLE_REGION_TYPES
        and figure.confidence >= SUPPRESSION_CONFIDENCE_MIN
    ]
    if page_size is None:
        return []
    width, height = page_size
    return [region for region in regions if bbox_within_page(region.bbox, width, height)]


def _covered_blocks(blocks: list[TextBlock], regions: list[FigureAnchor]) -> set[int]:
    if not regions:
        return set()
    covered: set[int] = set()
    for index, block in enumerate(blocks):
        if looks_like_prose(block.text):
            continue
        for region in regions:
            if block_coverage(block.bbox, region.bbox) >= SUPPRESSION_COVERAGE_MIN:
                covered.add(index)
                break
    return covered


def _figure_slot(figure: FigureAnchor, blocks: list[TextBlock]) -> int:
    label = _FIGURE_LABEL_RE.match(figure.caption or "")
    if label is not None:
        needle = label.group(1).lower().replace("fig.", "figure")
        for index, block in enumerate(blocks):
            haystack = block.text.lower().replace("fig.", "figure")
            if re.search(rf"(?<!\w){re.escape(needle)}(?!\d)", haystack):
                return index
    figure_y = (figure.bbox[1] + figure.bbox[3]) / 2
    figure_x = (figure.bbox[0] + figure.bbox[2]) / 2
    page_midpoint = max((block.bbox[2] for block in blocks), default=figure.bbox[2]) / 2
    figure_column = 0 if figure_x <= page_midpoint else 1
    same_column = [
        (index, block) for index, block in enumerate(blocks) if block.column == figure_column
    ]
    candidates = same_column or list(enumerate(blocks))
    for index, block in candidates:
        block_y = (block.bbox[1] + block.bbox[3]) / 2
        if block_y >= figure_y:
            return index
    if same_column:
        return same_column[-1][0] + 1
    return len(blocks)


def _body_font_size(pages: list[list[TextBlock]]) -> float:
    sizes: dict[float, int] = {}
    for blocks in pages:
        for block in blocks:
            if block.role == "body" and block.font_size > 0:
                bucket = round(block.font_size * 2) / 2
                sizes[bucket] = sizes.get(bucket, 0) + len(block.text.split())
    if not sizes:
        return 0.0
    return max(sizes, key=lambda size: sizes[size])


_PREFIX_PAT = (
    r"(?:chapter|sections?|parts?|appendix|appendices|modules?|units?|"
    r"lectures?|cases?|examples?|box(?:es)?)"
)
_STEM_NUM_PAT = r"\d+(?:\.\d+)+"
_SINGLE_NUM_PAT = r"\d+"
_ALPHA_NUM_PAT = r"[A-Z](?:\.\d+)+"
_ROMAN_PAT = r"[IVXLCDM]+(?:\.[IVXLCDM]+)*"
_LETTER_PAT = r"[A-Z]\b"
_NUMBERING_PAT = (
    f"(?:{_STEM_NUM_PAT}|{_ALPHA_NUM_PAT}|{_ROMAN_PAT}|{_SINGLE_NUM_PAT}|{_LETTER_PAT})"
)

_DELIM_PUNCT = r"[:—–-]"
_DELIM_PUNCT_PAT = rf"(?:\s*{_DELIM_PUNCT}\s*|\s*\.\s+)"

_ALPHA_TITLE_START = (
    r"[A-Z\u0370-\u03ff\u1f00-\u1fff\u0600-\u06ff\u4e00-\u9fff"
    r"\u00b1\u00d7\u00f7\u2211\u220f\u221a\u2202\u2207]"
)
_ALL_TITLE_START = rf"(?:{_ALPHA_TITLE_START}|[0-9])"

_TITLE_BRANCH = rf"(?:{_DELIM_PUNCT_PAT}{_ALL_TITLE_START}|\s+{_ALPHA_TITLE_START})"

_PREFIX_HEADING_RE = re.compile(
    rf"^{_PREFIX_PAT}(?:\s+{_NUMBERING_PAT}{_TITLE_BRANCH}|{_DELIM_PUNCT_PAT}{_ALL_TITLE_START})",
    re.IGNORECASE,
)

_NUMBERED_HEADING_RE = re.compile(
    rf"^(?:(?:{_STEM_NUM_PAT}|{_ALPHA_NUM_PAT}){_TITLE_BRANCH}|"
    rf"(?:{_ROMAN_PAT}|{_LETTER_PAT}){_DELIM_PUNCT_PAT}{_ALL_TITLE_START}|"
    rf"{_SINGLE_NUM_PAT}{_TITLE_BRANCH})"
)


def _is_heading(block: TextBlock, body_size: float) -> bool:
    if block.label in {"section_header", "title"}:
        return True
    if body_size <= 0:
        return False
    words = block.text.split()
    if not (0 < len(words) <= HEADING_MAX_WORDS):
        return False
    stripped = block.text.strip()
    if ";" in stripped:
        return False
    is_numbered_or_prefixed = bool(
        _PREFIX_HEADING_RE.match(stripped) or _NUMBERED_HEADING_RE.match(stripped)
    )
    if is_numbered_or_prefixed:
        if block.font_size < body_size * 0.98:
            return False
        return not (stripped.endswith((".", "!", "?")) and looks_like_prose(stripped))

    if stripped.endswith((".", ";", "!", "?")):
        return False
    return block.font_size >= body_size * HEADING_FONT_RATIO


def assemble_units(
    pages: list[list[TextBlock]],
    page_offset: int,
    chapter_bounds: list[tuple[int, int, int]],
    figures: list[FigureAnchor] | None = None,
    page_sizes: list[tuple[float, float]] | None = None,
) -> list[SourceUnit]:
    body_size = _body_font_size(pages)
    units: list[SourceUnit] = []
    for page_index, blocks in enumerate(pages):
        page_no = page_offset + page_index
        chapter_idx = _chapter_for_page(page_no, chapter_bounds)
        ordered = [
            block for block in sorted(blocks, key=lambda b: b.reading_order) if block.role == "body"
        ]
        page_figures = [
            figure
            for figure in figures or []
            if figure.page_no == page_no and figure.region_type != DECORATIVE_REGION_TYPE
        ]
        page_size = (
            page_sizes[page_index]
            if page_sizes is not None and page_index < len(page_sizes)
            else None
        )
        covered = _covered_blocks(ordered, _suppressing_regions(page_figures, page_size))
        ordered = [block for index, block in enumerate(ordered) if index not in covered]
        figures_by_slot: dict[int, list[FigureAnchor]] = {}
        for figure in page_figures:
            figures_by_slot.setdefault(_figure_slot(figure, ordered), []).append(figure)
        for slot_figures in figures_by_slot.values():
            slot_figures.sort(key=lambda figure: (figure.bbox[0], figure.bbox[1], figure.region_id))
        for slot, block in enumerate(ordered):
            for figure in figures_by_slot.get(slot, []):
                units.append(
                    SourceUnit(
                        chapter_idx=chapter_idx,
                        page_no=page_no,
                        kind="figure",
                        text=_figure_cue(figure),
                        figure_id=figure.region_id,
                        region_type=figure.region_type,
                        caption=figure.caption,
                        explanation=figure.explanation,
                    )
                )
            text = " ".join(block.text.split())
            if not text:
                continue
            kind = "heading" if _is_heading(block, body_size) else "text"
            units.append(
                SourceUnit(
                    chapter_idx=chapter_idx,
                    page_no=page_no,
                    kind=kind,
                    text=text,
                    block_index=block.reading_order,
                    label=block.label,
                )
            )
        for figure in figures_by_slot.get(len(ordered), []):
            units.append(
                SourceUnit(
                    chapter_idx=chapter_idx,
                    page_no=page_no,
                    kind="figure",
                    text=_figure_cue(figure),
                    figure_id=figure.region_id,
                    region_type=figure.region_type,
                    caption=figure.caption,
                    explanation=figure.explanation,
                )
            )
    return units


def _chapter_for_page(page_no: int, chapter_bounds: list[tuple[int, int, int]]) -> int:
    for chapter_idx, start, end in chapter_bounds:
        if start <= page_no <= end:
            return chapter_idx
    return chapter_bounds[0][0] if chapter_bounds else 0
