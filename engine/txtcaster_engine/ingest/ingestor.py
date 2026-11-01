"""The `Ingestor` seam ARCHITECTURE.md §3.5 and §9 name: everything the ingest and layout
paths need from a PDF engine, as page-index methods over plain Python values.

No engine object (document, page, pixmap, matrix) crosses this boundary. Coordinates are
points in the page's *displayed* frame: origin top-left, x right, y down, `/Rotate` applied,
which is the frame the reader's follow-along geometry and `TextBlock.bbox` already use.
"""

from dataclasses import dataclass, field
from typing import Protocol

Bbox = tuple[float, float, float, float]

RawBlock = tuple[Bbox, str, float]
"""(bbox, text, max font size) — the shape `blocks.extract_blocks` clusters and orders."""

RawWord = tuple[float, float, float, float, str, int, int]
"""(x0, y0, x1, y1, token, block_no, line_no) — the shape `render.page_geometry` folds."""


@dataclass
class PageText:
    blocks: list[RawBlock] = field(default_factory=list)
    words: list[RawWord] = field(default_factory=list)
    unicode_map_errors: int = 0
    extract_failed: str | None = None
    needs_fallback: tuple[str, ...] = ()
    artifact_share: tuple[float, ...] = ()


class Ingestor(Protocol):
    backend: str
    source_label: str
    page_count: int
    is_reflowable: bool

    def page_size(self, index: int) -> tuple[float, float]: ...

    def page_text(self, index: int) -> PageText: ...

    def render_png(self, index: int, scale: float, clip: Bbox | None = None) -> bytes: ...

    def toc(self) -> list[list[object]]: ...

    def metadata(self) -> dict[str, str]: ...

    def chapter_count(self) -> int: ...

    def page_number_from_location(self, spine_index: int) -> int: ...

    def close(self) -> None: ...
