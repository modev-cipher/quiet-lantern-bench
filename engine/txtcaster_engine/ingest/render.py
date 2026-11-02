import contextlib
import hashlib
import logging
import os
import tempfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from txtcaster_engine.ingest.errors import IngestCancelled, IngestError
from txtcaster_engine.ingest.ingestor import Ingestor
from txtcaster_engine.ingest.pdf import open_document
from txtcaster_engine.ingest.version import (
    DEFAULT_SCALE as DEFAULT_SCALE,
)
from txtcaster_engine.ingest.version import geometry_version, render_identity

GEOMETRY_ENVELOPE_VERSION = 1
COORD_DECIMALS = 2
# Rasters cached before PDFium calls were serialized can be blank; a bump redraws every page.
RASTER_EPOCH = 2

_LOG = logging.getLogger(__name__)


@dataclass
class WordRect:
    x0: float
    y0: float
    x1: float
    y1: float
    token: str
    line: int

    def to_json(self) -> list[object]:
        return [self.x0, self.y0, self.x1, self.y1, self.token, self.line]


@dataclass
class PageGeometry:
    page_no: int
    version: str
    lines: list[list[float]]
    words: list[WordRect]
    page_width_pt: float
    page_height_pt: float

    def to_json(self) -> dict[str, object]:
        return {
            "v": GEOMETRY_ENVELOPE_VERSION,
            "pageWidthPt": self.page_width_pt,
            "pageHeightPt": self.page_height_pt,
            "lines": self.lines,
            "words": [word.to_json() for word in self.words],
        }


@dataclass
class PageRender:
    page_no: int
    hash: str
    bytes: int
    cached: bool
    geometry: PageGeometry | None = None

    def to_json(self) -> dict[str, object]:
        payload: dict[str, object] = {
            "page_no": self.page_no,
            "hash": self.hash,
            "bytes": self.bytes,
            "cached": self.cached,
        }
        if self.geometry is not None:
            payload["geometry_version"] = self.geometry.version
            payload["geometry"] = self.geometry.to_json()
        return payload


def page_cache_key(content_hash: str, page_no: int, scale: float) -> str:
    raw = f"{content_hash}:{page_no}:{scale:g}:{render_identity()}:raster={RASTER_EPOCH}"
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def _round(value: float) -> float:
    return round(float(value), COORD_DECIMALS)


def page_geometry(doc: Ingestor, page_no: int) -> PageGeometry:
    lines: list[list[float]] = []
    words: list[WordRect] = []
    slot_by_line: dict[tuple[int, int], int] = {}
    for x0, y0, x1, y1, token, block_no, line_no in doc.page_text(page_no - 1).words:
        if not token:
            continue
        key = (int(block_no), int(line_no))
        slot = slot_by_line.get(key)
        if slot is None:
            slot = len(lines)
            slot_by_line[key] = slot
            lines.append([_round(x0), _round(y0), _round(x1), _round(y1)])
        else:
            rect = lines[slot]
            rect[0] = min(rect[0], _round(x0))
            rect[1] = min(rect[1], _round(y0))
            rect[2] = max(rect[2], _round(x1))
            rect[3] = max(rect[3], _round(y1))
        words.append(
            WordRect(
                x0=_round(x0),
                y0=_round(y0),
                x1=_round(x1),
                y1=_round(y1),
                token=token,
                line=slot,
            )
        )
    width, height = doc.page_size(page_no - 1)
    return PageGeometry(
        page_no=page_no,
        version=geometry_version(),
        lines=lines,
        words=words,
        page_width_pt=_round(width),
        page_height_pt=_round(height),
    )


def cache_path(cache_dir: str, key: str) -> Path:
    return Path(cache_dir) / key[:2] / key


def _write_atomic(target: Path, data: bytes) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(dir=target.parent, prefix=".tmp-")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
        os.replace(tmp_name, target)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp_name)
        raise


def render_pages(
    book_path: str,
    content_hash: str,
    pages: list[int],
    cache_dir: str,
    scale: float = DEFAULT_SCALE,
    with_geometry: bool = False,
    on_page: Callable[[int, int], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
) -> tuple[list[PageRender], list[int]]:
    if not pages:
        raise IngestError("ERR_BAD_REQUEST", "pages must not be empty")
    if scale <= 0:
        raise IngestError("ERR_BAD_REQUEST", f"scale must be positive, got {scale}")
    doc = open_document(book_path)
    try:
        invalid = [p for p in pages if p < 1 or p > doc.page_count]
        if invalid:
            raise IngestError(
                "ERR_BAD_REQUEST",
                f"pages out of range 1..{doc.page_count}: {invalid}",
            )
        results: list[PageRender] = []
        skipped: list[int] = []
        total = len(pages)
        for done, page_no in enumerate(pages, start=1):
            if is_cancelled is not None and is_cancelled():
                raise IngestCancelled()
            render: PageRender
            key = page_cache_key(content_hash, page_no, scale)
            target = cache_path(cache_dir, key)
            try:
                cached = target.is_file()
                size = target.stat().st_size if cached else 0
            except OSError as exc:
                raise IngestError(
                    "ERR_IO",
                    f"disk read failed checking cached page {page_no}: {exc}",
                ) from exc
            if cached:
                render = PageRender(page_no=page_no, hash=key, bytes=size, cached=True)
            else:
                data: bytes
                try:
                    data = doc.render_png(page_no - 1, scale)
                except IngestCancelled:
                    raise
                except Exception as exc:
                    _LOG.error(
                        "page render failed on page %d: %s: %s",
                        page_no,
                        type(exc).__name__,
                        exc,
                    )
                    skipped.append(page_no)
                    if on_page is not None:
                        on_page(done, total)
                    continue
                try:
                    _write_atomic(target, data)
                except OSError as exc:
                    raise IngestError(
                        "ERR_IO",
                        f"disk write failed rendering page {page_no}: {exc}",
                    ) from exc
                render = PageRender(page_no=page_no, hash=key, bytes=len(data), cached=False)
            if with_geometry:
                try:
                    render.geometry = page_geometry(doc, page_no)
                except IngestCancelled:
                    raise
                except Exception as exc:
                    _LOG.error(
                        "page render failed on page %d: %s: %s",
                        page_no,
                        type(exc).__name__,
                        exc,
                    )
                    skipped.append(page_no)
                    if on_page is not None:
                        on_page(done, total)
                    continue
            results.append(render)
            if on_page is not None:
                on_page(done, total)
        if skipped:
            _LOG.warning("skipped rendering pages: %s", skipped)
        return results, skipped
    finally:
        doc.close()
