import logging
from collections.abc import Callable
from typing import TypedDict

from txtcaster_engine.ingest.errors import IngestCancelled, IngestError
from txtcaster_engine.llm.backend import LLMBackend
from txtcaster_engine.script.assemble import FigureAnchor, SourceUnit, assemble_units
from txtcaster_engine.script.canonical import CanonicalPage
from txtcaster_engine.script.chunker import Chunk
from txtcaster_engine.script.cues import decide_code, format_cue_tally
from txtcaster_engine.script.normalize_llm import LLM_NORMALIZED_FLAG, normalize_units
from txtcaster_engine.script.roles import apply_union_roles
from txtcaster_engine.script.segment import ScriptSegment, segment_units
from txtcaster_engine.script.suppression import Suppressed, collect_suppressed
from txtcaster_engine.script.teacher import (
    NARRATOR_FALLBACK_FLAG,
    anchored_segments,
    teach_units,
)

logger = logging.getLogger(__name__)

__all__ = ["LLM_NORMALIZED_FLAG", "NARRATOR_FALLBACK_FLAG", "CanonicalPage", "generate_script"]


def _as_int(value: object, default: int) -> int:
    if isinstance(value, bool):
        return default
    if isinstance(value, (int, float, str)):
        try:
            return int(value)
        except (TypeError, ValueError):
            return default
    return default


def _scoped_pages(scope: dict[str, object], page_count: int) -> tuple[int, int]:
    kind = scope.get("kind")
    if kind == "pages":
        start = _as_int(scope.get("page_start", 1), 1)
        end = _as_int(scope.get("page_end", page_count), page_count)
        start = max(1, start)
        end = min(page_count, max(start, end))
        return start, end
    if kind == "chapters" or (isinstance(kind, str) and "chapter" in kind):
        bounds = scope.get("chapter_page_bounds")
        if isinstance(bounds, list) and bounds:
            starts = [
                _as_int(item.get("page_start") if isinstance(item, dict) else 1, 1)
                for item in bounds
            ]
            ends = [
                _as_int(item.get("page_end") if isinstance(item, dict) else page_count, page_count)
                for item in bounds
            ]
            return max(1, min(starts)), min(page_count, max(ends))
        if "chapter_ids" in scope or kind == "chapter_ids":
            raise IngestError(
                "ERR_SCOPE_UNRESOLVED",
                "chapter scope requires resolved chapter_page_bounds",
            )
    elif "chapter_ids" in scope:
        raise IngestError(
            "ERR_SCOPE_UNRESOLVED",
            "chapter scope requires resolved chapter_page_bounds",
        )
    return 1, page_count


def _clamp_page_range(
    start: int,
    end: int,
    min_page: int,
    max_page: int,
) -> tuple[int, int]:
    lo = max(min_page, start)
    hi = min(max_page, max(lo, end))
    return lo, hi


def _in_scope_pages(scope: dict[str, object], min_page: int, max_page: int) -> set[int] | None:
    """Return the discrete set of pages the user selected, or None for
    'all pages in the (start, end) range'. For chapter scoping, only
    pages inside the declared bounds are included — no hull filling."""
    kind = scope.get("kind")
    if kind != "chapters":
        return None
    bounds = scope.get("chapter_page_bounds")
    if not isinstance(bounds, list) or not bounds:
        return None
    pages: set[int] = set()
    for item in bounds:
        if not isinstance(item, dict):
            continue
        start = _as_int(item.get("page_start"), min_page)
        end = _as_int(item.get("page_end"), start)
        start, end = _clamp_page_range(start, end, min_page, max_page)
        for page_no in range(start, end + 1):
            pages.add(page_no)
    return pages


def _chapter_bounds(scope: dict[str, object], start: int, end: int) -> list[tuple[int, int, int]]:
    bounds = scope.get("chapter_page_bounds")
    if isinstance(bounds, list) and bounds:
        return [
            (
                _as_int(item.get("idx") if isinstance(item, dict) else 0, 0),
                *_clamp_page_range(
                    _as_int(item.get("page_start") if isinstance(item, dict) else start, start),
                    _as_int(item.get("page_end") if isinstance(item, dict) else end, end),
                    start,
                    end,
                ),
            )
            for item in bounds
        ]
    return [(0, start, end)]


def _contiguous_runs(pages: list[CanonicalPage]) -> list[list[CanonicalPage]]:
    if not pages:
        return []
    runs: list[list[CanonicalPage]] = [[pages[0]]]
    for page in pages[1:]:
        if page.page_no == runs[-1][-1].page_no + 1:
            runs[-1].append(page)
        else:
            runs.append([page])
    return runs


def _narrator_segments(units: list[SourceUnit], normalized: list[bool]) -> list[ScriptSegment]:
    segments: list[ScriptSegment] = []
    run: list[SourceUnit] = []
    run_flag = False

    def flush() -> None:
        nonlocal run
        if not run:
            return
        flags = [LLM_NORMALIZED_FLAG] if run_flag else []
        segments.extend(segment_units(run, flags=flags))
        run = []

    for unit, flag in zip(units, normalized, strict=True):
        if run and flag != run_flag:
            flush()
        run_flag = flag
        run.append(unit)
    flush()
    for idx, segment in enumerate(segments):
        segment.idx = idx
    return segments


def _section_ranges(
    ordered: list[CanonicalPage], section_bounds: list[tuple[int, int]] | None
) -> list[tuple[int, int]]:
    if not section_bounds:
        return []
    index_of = {page.page_no: index for index, page in enumerate(ordered)}
    ranges: list[tuple[int, int]] = []
    for page_start, page_end in section_bounds:
        present = [index_of[no] for no in range(page_start, page_end + 1) if no in index_of]
        if present:
            ranges.append((min(present), max(present) + 1))
    return ranges


def assemble_script_units(
    canonical_pages: list[CanonicalPage],
    scope: dict[str, object],
    figures: list[FigureAnchor] | None = None,
    section_bounds: list[tuple[int, int]] | None = None,
) -> list[SourceUnit]:
    """The pages->units preamble of generate_script, factored so the desktop
    cloud Script path can fetch assembled units over the local seam (provider
    charter S-1: unit assembly stays engine-side; the provider ladder runs in
    Rust). Pure local text processing - no keys, no network."""
    if not canonical_pages:
        raise IngestError(
            "ERR_NO_BLOCKS",
            "no canonical text blocks were supplied; scripting never re-extracts, so "
            "the book must be re-ingested first",
        )
    ordered = sorted(canonical_pages, key=lambda page: page.page_no)
    all_pages = [page.blocks for page in ordered]
    all_heights = [page.height_pt for page in ordered]
    apply_union_roles(all_pages, all_heights, _section_ranges(ordered, section_bounds))
    first = ordered[0].page_no
    start, end = _scoped_pages(scope, ordered[-1].page_no)
    start = max(start, first)
    end = max(start, min(end, ordered[-1].page_no))
    bounds = _chapter_bounds(scope, start, end)
    in_scope = _in_scope_pages(scope, first, ordered[-1].page_no)
    selected = [
        page
        for page in ordered
        if start <= page.page_no <= end and (in_scope is None or page.page_no in in_scope)
    ]
    if in_scope is not None and not any(page.page_no in in_scope for page in selected):
        raise IngestError(
            "ERR_NO_BLOCKS",
            "no canonical text blocks cover the selected pages",
        )
    if not selected:
        raise IngestError(
            "ERR_NO_BLOCKS",
            "no canonical text blocks cover the selected pages",
        )
    pages = [page.blocks for page in selected]
    page_sizes = [(page.width_pt, page.height_pt) for page in selected]
    if not any(block.text.strip() for blocks in pages for block in blocks):
        raise IngestError(
            "ERR_NO_TEXT",
            f"pages {start}-{end} contain no readable text; the stored blocks are "
            f"empty, so re-import this book to re-extract it",
        )
    if in_scope is None:
        units = assemble_units(pages, start, bounds, figures, page_sizes=page_sizes)
    else:
        units = []
        for run in _contiguous_runs(selected):
            units.extend(
                assemble_units(
                    [page.blocks for page in run],
                    run[0].page_no,
                    bounds,
                    figures,
                    page_sizes=[(page.width_pt, page.height_pt) for page in run],
                )
            )
        if not units:
            raise IngestError(
                "ERR_NO_TEXT",
                "the selected chapter pages contain no readable text; the stored blocks are "
                "empty, so re-import this book to re-extract it",
            )
    return units


def unit_to_json(unit: SourceUnit) -> dict[str, object]:
    return {
        "chapter_idx": unit.chapter_idx,
        "page_no": unit.page_no,
        "kind": unit.kind,
        "text": unit.text,
        "figure_id": unit.figure_id,
        "region_type": unit.region_type,
        "caption": unit.caption,
        "explanation": unit.explanation,
        "block_index": unit.block_index,
        "label": unit.label,
    }


class LectureChunkPayload(TypedDict):
    """The per-chunk seam payload: unit dicts as unit_to_json emits them
    (validated as LectureUnit at the HTTP boundary), plus the ladder outcome."""

    units: list[dict[str, object]]
    texts: list[str] | None


def _required_int(data: dict[str, object], key: str) -> int:
    value = data[key]
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"seam unit field {key!r} must be an int, got {type(value).__name__}")
    return value


def _required_str(data: dict[str, object], key: str) -> str:
    value = data[key]
    if not isinstance(value, str):
        raise TypeError(f"seam unit field {key!r} must be a str, got {type(value).__name__}")
    return value


def _optional_str(data: dict[str, object], key: str) -> str | None:
    value = data.get(key)
    return value if isinstance(value, str) else None


def _optional_int(data: dict[str, object], key: str) -> int | None:
    value = data.get(key)
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def unit_from_json(data: dict[str, object]) -> SourceUnit:
    unit = SourceUnit(
        chapter_idx=_required_int(data, "chapter_idx"),
        page_no=_required_int(data, "page_no"),
        kind=_required_str(data, "kind"),
        text=_required_str(data, "text"),
    )
    unit.figure_id = _optional_str(data, "figure_id")
    unit.region_type = _optional_str(data, "region_type")
    unit.caption = _optional_str(data, "caption")
    unit.explanation = _optional_str(data, "explanation")
    unit.block_index = _optional_int(data, "block_index")
    unit.label = _optional_str(data, "label")
    return unit


def segments_from_lecture(
    chunks: list[LectureChunkPayload],
) -> list[ScriptSegment]:
    """Segment the Rust ladder's per-chunk outcomes: texts present means a
    valid Teacher lecture (marker-anchored assembly), texts None means the
    recorded narrator fallback for that chunk's units. Re-indexed across the
    whole job like teach_units does."""
    segments: list[ScriptSegment] = []
    for entry in chunks:
        units = [unit_from_json(item) for item in entry["units"]]
        texts = entry["texts"]
        if texts is None:
            segments.extend(segment_units(units, flags=[NARRATOR_FALLBACK_FLAG]))
            continue
        chunk = Chunk(
            idx=0,
            chapter_idx=units[0].chapter_idx if units else 0,
            page_no=units[0].page_no if units else 0,
            units=units,
        )
        segments.extend(anchored_segments(chunk, list(texts)))
    for idx, segment in enumerate(segments):
        segment.idx = idx
    return segments


def generate_script(
    canonical_pages: list[CanonicalPage],
    scope: dict[str, object],
    mode: str,
    backend: LLMBackend | None = None,
    figures: list[FigureAnchor] | None = None,
    on_chapter: Callable[[int, int], None] | None = None,
    on_chunk: Callable[[int, int], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
    on_markers_dropped: Callable[[int], None] | None = None,
    on_markers_omitted: Callable[[int], None] | None = None,
    on_suppressed: Callable[[list[Suppressed]], None] | None = None,
    section_bounds: list[tuple[int, int]] | None = None,
) -> list[ScriptSegment]:
    if not canonical_pages:
        raise IngestError(
            "ERR_NO_BLOCKS",
            "no canonical text blocks were supplied; scripting never re-extracts, so "
            "the book must be re-ingested first",
        )
    ordered = sorted(canonical_pages, key=lambda page: page.page_no)
    all_pages = [page.blocks for page in ordered]
    all_heights = [page.height_pt for page in ordered]

    if is_cancelled is not None and is_cancelled():
        raise IngestCancelled()

    apply_union_roles(all_pages, all_heights, _section_ranges(ordered, section_bounds))

    first = ordered[0].page_no
    start, end = _scoped_pages(scope, ordered[-1].page_no)
    start = max(start, first)
    end = max(start, min(end, ordered[-1].page_no))
    bounds = _chapter_bounds(scope, start, end)
    in_scope = _in_scope_pages(scope, first, ordered[-1].page_no)

    selected = [
        page
        for page in ordered
        if start <= page.page_no <= end and (in_scope is None or page.page_no in in_scope)
    ]
    if in_scope is not None and not any(page.page_no in in_scope for page in selected):
        raise IngestError(
            "ERR_NO_BLOCKS",
            "no canonical text blocks cover the selected pages",
        )
    if not selected:
        raise IngestError(
            "ERR_NO_BLOCKS",
            "no canonical text blocks cover the selected pages",
        )
    pages = [page.blocks for page in selected]
    page_sizes = [(page.width_pt, page.height_pt) for page in selected]
    if on_suppressed is not None:
        suppressed_target = (
            selected if in_scope is None else [p for p in selected if p.page_no in in_scope]
        )
        on_suppressed(collect_suppressed(suppressed_target))

    if not any(block.text.strip() for blocks in pages for block in blocks):
        raise IngestError(
            "ERR_NO_TEXT",
            f"pages {start}-{end} contain no readable text; the stored blocks are "
            f"empty, so re-import this book to re-extract it",
        )

    logger.info(
        "scripting pages %d-%d of %d supplied page(s) in %s mode",
        start,
        end,
        len(ordered),
        mode,
    )
    if in_scope is None:
        units = assemble_units(pages, start, bounds, figures, page_sizes=page_sizes)
    else:
        units = []
        for run in _contiguous_runs(selected):
            units.extend(
                assemble_units(
                    [page.blocks for page in run],
                    run[0].page_no,
                    bounds,
                    figures,
                    page_sizes=[(page.width_pt, page.height_pt) for page in run],
                )
            )
        if not units:
            raise IngestError(
                "ERR_NO_TEXT",
                "the selected chapter pages contain no readable text; the stored blocks are "
                "empty, so re-import this book to re-extract it",
            )
    text_units = [unit for unit in units if unit.kind == "text"]
    logger.info(
        "%s",
        format_cue_tally(
            [unit.page_no for unit in text_units],
            [decide_code(unit.label, unit.text) for unit in text_units],
        ),
    )
    if mode == "teacher" and backend is not None:
        segments = teach_units(
            units,
            ctx=backend.context_window(),
            generate=lambda prompt, max_tokens, temperature=0.0: backend.generate(
                prompt, max_tokens, temperature
            ),
            on_chunk=on_chunk,
            is_cancelled=is_cancelled,
            on_markers_dropped=on_markers_dropped,
            on_markers_omitted=on_markers_omitted,
        )
    elif mode == "narrator" and backend is not None:
        normalized_units, normalized = normalize_units(
            units,
            ctx=backend.context_window(),
            generate=lambda prompt, max_tokens, temperature=0.0: backend.generate(
                prompt, max_tokens, temperature
            ),
            on_chunk=on_chunk,
            is_cancelled=is_cancelled,
        )
        segments = _narrator_segments(normalized_units, normalized)
    else:
        flags = [NARRATOR_FALLBACK_FLAG] if mode == "teacher" else []
        segments = segment_units(units, flags=flags)

    if on_chapter is not None:
        total_chapters = len({unit.chapter_idx for unit in units}) or 1
        on_chapter(total_chapters, total_chapters)
    return segments
