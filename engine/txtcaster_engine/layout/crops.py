import hashlib
from collections.abc import Callable
from pathlib import Path

from txtcaster_engine.ingest.ingestor import Ingestor
from txtcaster_engine.ingest.render import _write_atomic, cache_path
from txtcaster_engine.layout.regions import Bbox, bbox_canonical

CROP_SCALE = 2.0


def crop_cache_key(content_hash: str, page_no: int, bbox: Bbox, scale: float = CROP_SCALE) -> str:
    raw = f"{content_hash}:{page_no}:{bbox_canonical(bbox)}:{scale:g}"
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def render_crop(
    doc: Ingestor,
    content_hash: str,
    page_no: int,
    bbox: Bbox,
    cache_dir: str,
    scale: float = CROP_SCALE,
    write_atomic: Callable[[Path, bytes], None] = _write_atomic,
) -> str:
    key = crop_cache_key(content_hash, page_no, bbox, scale)
    target = cache_path(cache_dir, key)
    if not target.is_file():
        write_atomic(target, doc.render_png(page_no - 1, scale, clip=bbox))
    return str(target)
