"""Page or region image for the wire: JPEG, long edge <= 1300 px, short edge <= 1000 px
(survey §1 tile basis; §6 says pre-downscale on every vendor). Rendered through the
adapter's own `render_png`, so the crop frame is the engine's top-left page frame."""

from __future__ import annotations

import io
import math

from PIL import Image

from txtcaster_engine.ingest.fallback.providers.base import PageImage
from txtcaster_engine.ingest.fallback.trigger import FallbackTrigger
from txtcaster_engine.ingest.ingestor import Ingestor

MAX_LONG_EDGE = 1300
MAX_SHORT_EDGE = 1000
JPEG_QUALITY = 85
REGION_PAD_PT = 6.0


def render_trigger(doc: Ingestor, trigger: FallbackTrigger) -> PageImage:
    width_pt, height_pt = doc.page_size(trigger.page)
    clip = trigger.region_bbox
    if clip is not None:
        x0, y0, x1, y1 = clip
        clip = (
            max(0.0, x0 - REGION_PAD_PT),
            max(0.0, y0 - REGION_PAD_PT),
            min(width_pt, x1 + REGION_PAD_PT),
            min(height_pt, y1 + REGION_PAD_PT),
        )
        width_pt, height_pt = clip[2] - clip[0], clip[3] - clip[1]
    scale = _fit_scale(width_pt, height_pt)
    png = doc.render_png(trigger.page, scale=scale, clip=clip)
    return jpeg_from_png(png)


def _fit_scale(width_pt: float, height_pt: float) -> float:
    if width_pt <= 0 or height_pt <= 0:
        return 1.0
    long_pt, short_pt = max(width_pt, height_pt), min(width_pt, height_pt)
    return min(MAX_LONG_EDGE / long_pt, MAX_SHORT_EDGE / short_pt)


def jpeg_from_png(png: bytes) -> PageImage:
    with Image.open(io.BytesIO(png)) as source:
        image = source.convert("RGB")
    long_edge, short_edge = max(image.size), min(image.size)
    factor = min(1.0, MAX_LONG_EDGE / long_edge, MAX_SHORT_EDGE / short_edge)
    if factor < 1.0:
        size = (max(1, math.floor(image.width * factor)), max(1, math.floor(image.height * factor)))
        image = image.resize(size, Image.Resampling.LANCZOS)
    buffer = io.BytesIO()
    image.save(buffer, format="JPEG", quality=JPEG_QUALITY, optimize=True)
    return PageImage.from_jpeg(buffer.getvalue(), image.width, image.height)
