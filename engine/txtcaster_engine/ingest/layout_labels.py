"""One layout pass over the document, yielding every block label and the page upgrades.

The fast path extracts with PyMuPDF, which has no notion of a running head, so the
heuristic in `headers.py` infers furniture from repetition across pages. Measured over
eleven books that reaches 9% of it: alternating verso/recto heads each cover about half
the pages, under the 0.6 repetition threshold, and a head whose wording changes every
section is unreachable at any threshold. The layout model already knows.

The same conversion answers two questions at once — which blocks are furniture, and what
the accurate extractor makes of a page whose fast-path text looked weak — so it runs
once. Previously the upgrade pass built its own converter and re-converted up to forty
pages one at a time that this pass was already converting, loading the model twice.

It harvests **every** label the model reports, not only the two that name furniture. The
conversion has already computed them; discarding them left the fast path with no `code`
label at all — measured, 0 of 14 850 blocks on `C++ Concurrency in Action 2e` — so
scripting's cue classifier fell back to a text heuristic whose measured precision on that
book is 0.727, and roughly 27% of its spoken code cues replaced real prose. The negative
labels are what buy that back: with only `code` harvested the same blind sample scores
0.798 precision, because every prose block still carries no label and still reaches the
heuristic; with every label it scores 0.946. A label we keep is one short string on one
block (+6.9% of `page.text_json` on that book) and it is consumed by
`script/cues.decide_code`, so this is not a wider envelope for nothing.
"""

import logging
import sys
from collections.abc import Callable
from dataclasses import dataclass, field

from txtcaster_engine.ingest.blocks import TextBlock
from txtcaster_engine.ingest.errors import IngestCancelled
from txtcaster_engine.ingest.headers import ROLE_BY_LABEL
from txtcaster_engine.residency import flush_accelerator_cache_mib, release_traceback_locals

logger = logging.getLogger(__name__)

CHUNK_PAGES = 25
LINE_SHARE_MIN = 0.5


@dataclass(frozen=True)
class LabelBox:
    page_no: int
    label: str
    bbox: tuple[float, float, float, float]


@dataclass
class LayoutPass:
    labels: list[LabelBox] = field(default_factory=list)
    upgraded: dict[int, list[TextBlock]] = field(default_factory=dict)
    pages_read: int = 0
    partial: bool = False


def _line_share(block: tuple[float, float, float, float], item: LabelBox) -> float:
    height = block[3] - block[1]
    if height <= 0:
        return 0.0
    overlap = min(block[3], item.bbox[3]) - max(block[1], item.bbox[1])
    return max(overlap, 0.0) / height


def _meets_horizontally(block: tuple[float, float, float, float], item: LabelBox) -> bool:
    return min(block[2], item.bbox[2]) - max(block[0], item.bbox[0]) > 0


def _hits(block: tuple[float, float, float, float], item: LabelBox) -> bool:
    return _meets_horizontally(block, item) and _line_share(block, item) >= LINE_SHARE_MIN


@dataclass(frozen=True)
class StampResult:
    blocks: int = 0
    items_matched: int = 0
    items: int = 0

    @property
    def item_coverage(self) -> float:
        """The share of *furniture* layout regions that found a home.

        The right health signal, and not `blocks / items`: docling splits a running head
        into the folio and the title, so two items routinely land on one merged block and
        a healthy book sits near 0.5 on that ratio. Coverage measured over eleven books
        was 0.998-1.000, so a low value really does mean the two extractors disagree about
        page geometry.

        It counts furniture only, and that survived the harvest widening on purpose. The
        0.998-1.000 calibration and the 0.8 floor derived from it were measured over
        `page_header`/`page_footer` regions; every other region is a different population,
        because docling emits pictures and table cells that PyMuPDF never turns into a
        block and which therefore cannot match anything. Measured on
        `C++ Concurrency in Action 2e`: 21/21 furniture regions matched (1.000) while all
        6 429 regions together matched 0.849 — folding them in would move a healthy book
        to within 0.05 of a warning that means "the two extractors disagree about page
        geometry", which would then be false.
        """
        return self.items_matched / self.items if self.items else 1.0


def _stamp_pass(
    pages: list[list[TextBlock]],
    by_page: dict[int, list[LabelBox]],
    first_page_no: int,
) -> int:
    stamped = 0
    for index, blocks in enumerate(pages):
        candidates = by_page.get(first_page_no + index)
        if not candidates:
            continue
        for block in blocks:
            if block.label is not None:
                continue
            for item in candidates:
                if _hits(block.bbox, item):
                    block.label = item.label
                    stamped += 1
                    break
    return stamped


def stamp_labels(
    pages: list[list[TextBlock]],
    labels: list[LabelBox],
    *,
    first_page_no: int = 1,
) -> StampResult:
    """Attach the layout model's labels to the blocks that share their line.

    Matching is by *line*, not by text or by area. The two extractors segment
    differently — docling splits a running head into the folio and the title
    (`8` + `Chapter 1. Introduction`) where PyMuPDF emits one block — so area ratios fail
    in both directions, and text matching fails because digits normalise to `#` and every
    bare number then matches its page's folio. A running head occupies its own line and a
    body paragraph never shares one with the footer, which is what makes the line the
    right key; requiring most of the *block's* height to be covered stops a small label
    from condemning a tall paragraph.

    Furniture is stamped in its own first pass, and that ordering is load-bearing. A block
    takes the first region that hits it and keeps it, so with one undifferentiated pass a
    `text` region that also covers a running head would win it whenever docling emitted it
    first — the running head would then be narrated, which is the loss the layout pass
    exists to prevent. Two passes make the precedence a property of the code rather than of
    docling's iteration order.
    """
    furniture: dict[int, list[LabelBox]] = {}
    rest: dict[int, list[LabelBox]] = {}
    for item in labels:
        target = furniture if item.label in ROLE_BY_LABEL else rest
        target.setdefault(item.page_no, []).append(item)

    stamped = _stamp_pass(pages, furniture, first_page_no)
    stamped += _stamp_pass(pages, rest, first_page_no)

    matched = 0
    for index, blocks in enumerate(pages):
        candidates = furniture.get(first_page_no + index)
        if not candidates:
            continue
        # Counted separately from stamping: several regions routinely land on one merged
        # block, and a block may only claim one of them, so counting during the stamp
        # loop would undercount coverage on exactly the books that work.
        matched += sum(1 for item in candidates if any(_hits(block.bbox, item) for block in blocks))
    total = sum(len(items) for items in furniture.values())
    return StampResult(blocks=stamped, items_matched=matched, items=total)


def run_layout_pass(
    book_path: str,
    artifacts_dir: str,
    page_count: int,
    upgrade_pages: set[int] | None = None,
    on_page: Callable[[int, int], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
) -> LayoutPass:
    """Convert the document in chunks, collecting furniture and requested upgrades.

    Partial results are kept: an out-of-memory failure on page 501 of 613 leaves 500
    pages of correct labels rather than dropping the whole book to the heuristic, so the
    failure is a slope instead of a cliff. Cancellation is checked per chunk, which bounds
    latency at roughly `CHUNK_PAGES` pages of work.
    """
    from txtcaster_engine.ingest.docling_path import (
        _build_converter,
        _content_layers,
        _convert_document_status,
        _page_map_from_document,
    )

    wanted = upgrade_pages or set()
    result = LayoutPass()
    outer_error = sys.exc_info()[1]
    converter: object | None = None
    try:
        converter = _build_converter(artifacts_dir)
        layers = _content_layers()

        for start in range(1, page_count + 1, CHUNK_PAGES):
            if is_cancelled is not None and is_cancelled():
                raise IngestCancelled()
            end = min(start + CHUNK_PAGES - 1, page_count)
            try:
                document, complete = _convert_document_status(converter, book_path, (start, end))
                if not complete:
                    result.partial = True
                    logger.warning(
                        "docling reported a partial conversion for pages %d-%d; the book stays "
                        "eligible for another attempt",
                        start,
                        end,
                    )
            except IngestCancelled:
                raise
            except Exception as exc:
                result.partial = True
                logger.warning(
                    "layout pass stopped at page %d of %d (%s: %s); keeping %d label(s) "
                    "already found",
                    start,
                    page_count,
                    type(exc).__name__,
                    str(exc),
                    len(result.labels),
                )
                return result

            heights = {no: float(document.pages[no].size.height) for no in document.pages}
            for item, _ in document.iterate_items(included_content_layers=layers):
                if not str(getattr(item, "label", "")):
                    continue
                for prov in getattr(item, "prov", []) or []:
                    height = heights.get(prov.page_no)
                    if height is None:
                        continue
                    box = prov.bbox.to_top_left_origin(height)
                    result.labels.append(
                        LabelBox(
                            page_no=prov.page_no,
                            label=str(getattr(item, "label", "")),
                            bbox=(float(box.l), float(box.t), float(box.r), float(box.b)),
                        )
                    )

            if wanted & set(range(start, end + 1)):
                converted = _page_map_from_document(document)
                for page_no in sorted(wanted & set(converted)):
                    blocks = converted.get(page_no)
                    if blocks:
                        result.upgraded[page_no] = blocks

            result.pages_read = end
            if on_page is not None:
                on_page(end, page_count)

        return result
    except BaseException as error:
        release_traceback_locals(error, outer_error)
        raise
    finally:
        converter = None
        logger.info("layout residency: freed=%dMiB pass=layout", flush_accelerator_cache_mib())
