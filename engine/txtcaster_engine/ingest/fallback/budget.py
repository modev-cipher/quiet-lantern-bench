"""Per-book budget guard: 5% of pages or 200 pages, whichever is smaller (ruling #40), and an
optional dollar cap. Beyond the cap a page keeps its deterministic linear reading and carries
the typed note `fallback_budget_exhausted`; nothing is silently dropped."""

from __future__ import annotations

import math
from dataclasses import dataclass

from txtcaster_engine.ingest.fallback.ledger import CostLedger

BUDGET_EXHAUSTED = "fallback_budget_exhausted"


@dataclass(frozen=True)
class BudgetGuard:
    page_count: int
    cap_fraction: float = 0.05
    cap_pages: int = 200
    cap_usd: float | None = None

    def __post_init__(self) -> None:
        by_fraction = max(1, math.ceil(self.page_count * self.cap_fraction))
        object.__setattr__(self, "cap_pages", min(self.cap_pages, by_fraction))

    def allows(self, ledger: CostLedger, page: int) -> bool:
        if any(entry.page == page for entry in ledger.entries):
            return True
        if ledger.billed_pages >= self.cap_pages:
            return False
        return self.cap_usd is None or ledger.total_usd < self.cap_usd
