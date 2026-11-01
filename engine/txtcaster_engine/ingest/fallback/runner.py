"""Binds the cascade to one document: eligibility from ING-2's marker, page image from the
adapter, reading spliced into the page's blocks at the region's slot. A no-op unless the
backend is pdfium (owner #46: native stays the default and never enters here)."""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from pathlib import Path

from txtcaster_engine.ingest.blocks import TextBlock
from txtcaster_engine.ingest.fallback.budget import BudgetGuard
from txtcaster_engine.ingest.fallback.cache import ReadingCache
from txtcaster_engine.ingest.fallback.cascade import (
    Cascade,
    CascadeConfig,
    CascadeResult,
    default_providers,
)
from txtcaster_engine.ingest.fallback.image import render_trigger
from txtcaster_engine.ingest.fallback.keys import ProviderKeys
from txtcaster_engine.ingest.fallback.ledger import CostLedger
from txtcaster_engine.ingest.fallback.providers.base import Wire, https_wire
from txtcaster_engine.ingest.fallback.trigger import trigger_for_page
from txtcaster_engine.ingest.ingestor import Ingestor, PageText

FALLBACK_FONT_SIZE = 11.0

_LOG = logging.getLogger(__name__)


@dataclass(frozen=True, repr=False)
class FallbackSettings:
    keys: ProviderKeys
    theta: float = 0.6
    cache_dir: Path | None = None
    cap_usd: float | None = None
    cap_fraction: float = 0.05
    cap_pages: int = 200
    wire: Wire | None = None

    def __repr__(self) -> str:
        return f"FallbackSettings(keys={self.keys!r}, theta={self.theta})"

    def enabled(self) -> bool:
        return bool(self.keys.secrets())


@dataclass
class FallbackRunner:
    doc: Ingestor
    cascade: Cascade
    ledger: CostLedger
    results: dict[int, CascadeResult] = field(default_factory=dict)

    @classmethod
    def create(cls, settings: FallbackSettings, doc: Ingestor) -> FallbackRunner | None:
        if getattr(doc, "backend", "") != "pdfium" or not settings.enabled():
            return None
        cache_dir = settings.cache_dir
        ledger = CostLedger()
        cascade = Cascade(
            providers=default_providers(),
            wire=settings.wire or https_wire,
            keys=settings.keys,
            cache=ReadingCache(cache_dir),
            ledger=ledger,
            budget=BudgetGuard(
                page_count=doc.page_count,
                cap_fraction=settings.cap_fraction,
                cap_pages=settings.cap_pages,
                cap_usd=settings.cap_usd,
            ),
            config=CascadeConfig(theta=settings.theta),
        )
        return cls(doc=doc, cascade=cascade, ledger=ledger)

    def apply(self, page_index: int, page: PageText, blocks: list[TextBlock]) -> list[TextBlock]:
        trigger = trigger_for_page(page, page_index)
        if trigger is None:
            return blocks
        try:
            image = render_trigger(self.doc, trigger)
        except Exception as exc:
            _LOG.warning(
                "fallback image unavailable on page %d: %s: %s",
                page_index + 1,
                type(exc).__name__,
                exc,
            )
            return blocks
        result = self.cascade.run(trigger, image) if self.cascade else None
        if result is None:
            return blocks
        self.results[page_index] = result
        if result.reading is None:
            return blocks
        width, height = self.doc.page_size(page_index)
        bbox = trigger.region_bbox or (0.0, 0.0, float(width), float(height))
        spliced = [
            TextBlock(
                bbox=bbox,
                text=item.text,
                font_size=FALLBACK_FONT_SIZE,
                role="body",
                reading_order=order,
                column=0,
                label=item.kind,
            )
            for order, item in enumerate(result.reading.items)
        ]
        if trigger.kind in ("scanned", "undecodable_text"):
            return spliced
        kept = [b for b in blocks if not _inside(b.bbox, bbox)]
        merged = sorted(kept + spliced, key=lambda b: (b.bbox[1], b.bbox[0]))
        for order, block in enumerate(merged):
            block.reading_order = order
        return merged

    def summary(self) -> dict[str, object]:
        return {
            "ledger": self.ledger.to_json(),
            "pages": {str(page + 1): result.to_json() for page, result in self.results.items()},
        }

    def exhausted_pages(self) -> list[int]:
        return sorted(
            page + 1 for page, r in self.results.items() if r.note == "fallback_budget_exhausted"
        )

    def failed_pages(self) -> list[int]:
        return sorted(page + 1 for page, r in self.results.items() if r.error)


def _inside(
    inner: tuple[float, float, float, float], outer: tuple[float, float, float, float]
) -> bool:
    cx, cy = (inner[0] + inner[2]) / 2, (inner[1] + inner[3]) / 2
    return outer[0] <= cx <= outer[2] and outer[1] <= cy <= outer[3]
