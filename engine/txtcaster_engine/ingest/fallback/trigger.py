"""Eligibility is ING-2's decision, read off `PageText.needs_fallback`; this module never
inspects page content. Three marker kinds are billable (rulings #36.1, #48): `scanned` (zero
extractable glyphs on an image page), `high_entropy_table` (a ruled or clustered table
whose row/column association collided) and `undecodable_text` (ING-5: the page shows text
whose fonts do not reach Unicode, so the glyphs it did decode are below the floor).
`diagram`, `ambiguous_layout`, `region_conflict` and `interleaved_cells` read linearly and are
not fallback triggers.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from txtcaster_engine.ingest.ingestor import Bbox, PageText

TriggerKind = Literal["scanned", "table_entropy", "undecodable_text"]

MARKER_TO_KIND: dict[str, TriggerKind] = {
    "scanned": "scanned",
    "high_entropy_table": "table_entropy",
    "undecodable_text": "undecodable_text",
}


@dataclass(frozen=True)
class FallbackTrigger:
    kind: TriggerKind
    page: int
    region_bbox: Bbox | None = None

    def to_json(self) -> dict[str, object]:
        return {
            "kind": self.kind,
            "page": self.page,
            "region_bbox": list(self.region_bbox) if self.region_bbox else None,
        }


def trigger_for_page(page: PageText, page_index: int) -> FallbackTrigger | None:
    if "scanned" in page.needs_fallback and not page.blocks:
        return FallbackTrigger(kind="scanned", page=page_index)
    if "undecodable_text" in page.needs_fallback:
        return FallbackTrigger(kind="undecodable_text", page=page_index)
    if "high_entropy_table" in page.needs_fallback:
        return FallbackTrigger(kind="table_entropy", page=page_index)
    return None
