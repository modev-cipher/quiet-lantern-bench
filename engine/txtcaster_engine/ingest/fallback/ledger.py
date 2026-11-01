"""Per-book cost ledger (ruling #40 contract item 6; honesty law).

One row per HTTP ATTEMPT that reached a provider: providers bill every request they receive, so a
5xx after upload, a retried 429 and a schema-rejected answer are all metered. `est_usd` is the
survey §1 formula (input x in_price + output x out_price per million tokens) at the price tier of
the TRANSPORT that carried the attempt — `sync` today; `batch` (50%) can only be recorded by a
Batch transport, which is not wired. When the provider returned usage it is used; when nothing
came back the input is the tile-formula estimate and `usage_source` says `estimated`.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal

Transport = Literal["sync", "batch"]
LedgerOutcome = Literal["ok", "schema_parse_error", "provider_error", "timeout", "cached"]
UsageSource = Literal["provider", "estimated", "none"]


def estimate_usd(
    price_in_per_m: float,
    price_out_per_m: float,
    input_tokens: int,
    output_tokens: int,
    transport: Transport = "sync",
) -> float:
    usd = (input_tokens * price_in_per_m + output_tokens * price_out_per_m) / 1_000_000
    return usd / 2 if transport == "batch" else usd


@dataclass(frozen=True)
class LedgerEntry:
    provider: str
    model: str
    input_tokens: int
    output_tokens: int
    est_usd: float
    cached: bool
    page: int
    attempt_no: int = 1
    outcome: LedgerOutcome = "ok"
    usage_source: UsageSource = "provider"
    transport: Transport = "sync"

    def to_json(self) -> dict[str, object]:
        return {
            "provider": self.provider,
            "model": self.model,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "est_usd": self.est_usd,
            "cached": self.cached,
            "page": self.page,
            "attempt_no": self.attempt_no,
            "outcome": self.outcome,
            "usage_source": self.usage_source,
            "transport": self.transport,
        }


@dataclass
class CostLedger:
    entries: list[LedgerEntry] = field(default_factory=list)

    def record(self, entry: LedgerEntry) -> LedgerEntry:
        self.entries.append(entry)
        return entry

    @property
    def total_usd(self) -> float:
        return sum(entry.est_usd for entry in self.entries if not entry.cached)

    @property
    def billed_pages(self) -> int:
        return len({entry.page for entry in self.entries if not entry.cached})

    @property
    def attempts(self) -> int:
        return sum(1 for entry in self.entries if not entry.cached)

    def to_json(self) -> dict[str, object]:
        return {
            "pages": self.billed_pages,
            "attempts": self.attempts,
            "est_usd": self.total_usd,
            "entries": [entry.to_json() for entry in self.entries],
        }
