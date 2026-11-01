"""Cost-first cascade (ruling #40): luna -> flash-lite -> flash, each step Ok | Escalate | Fail.

Escalation reasons: `low_confidence` (< theta, default 0.6), `schema_parse_error`,
`provider_error` (a non-retryable wire failure moves on; a retryable one is retried once and
then moves on), `table_entropy` (the trigger kind enters at the last step directly). When every
step fails the result carries the typed error `fallback_all_providers_failed` and the caller
keeps its deterministic linear reading; when the budget guard refuses, the note is
`fallback_budget_exhausted`. Every HTTP attempt is metered as one ledger row (failed and retried
attempts included, priced at the tier of the transport that carried them); cache hits are
metered at $0. Usage the provider reported is used verbatim, an explicit zero included (a
rejected request the provider did not bill is a $0 row, not an invented estimate); only an ABSENT
usage block falls back to the tile-formula estimate.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Literal

from txtcaster_engine.ingest.fallback.budget import BUDGET_EXHAUSTED, BudgetGuard
from txtcaster_engine.ingest.fallback.cache import ReadingCache, cache_key
from txtcaster_engine.ingest.fallback.envelope import (
    PROMPT_SHA,
    EnvelopeError,
    Reading,
    normalise,
    parse_envelope,
)
from txtcaster_engine.ingest.fallback.keys import ProviderKeys, redact
from txtcaster_engine.ingest.fallback.ledger import (
    CostLedger,
    LedgerEntry,
    LedgerOutcome,
    Transport,
    UsageSource,
    estimate_usd,
)
from txtcaster_engine.ingest.fallback.providers import gemini, openai_luna
from txtcaster_engine.ingest.fallback.providers.base import (
    PageImage,
    Provider,
    ProviderOutcome,
    Wire,
    WireRequest,
    WireResponse,
)
from txtcaster_engine.ingest.fallback.trigger import FallbackTrigger

_LOG = logging.getLogger(__name__)

DEFAULT_ORDER = (openai_luna.PROVIDER_ID, gemini.FLASH_LITE_ID, gemini.FLASH_ID)
ALL_FAILED = "fallback_all_providers_failed"
NO_KEY = "fallback_no_provider_key"

Outcome = Literal["ok", "escalate", "fail", "skipped"]
Reason = Literal[
    "low_confidence", "schema_parse_error", "provider_error", "table_entropy", "no_key", "cached"
]


def default_providers() -> list[Provider]:
    return [
        openai_luna.OpenAILuna(),
        gemini.Gemini(gemini.FLASH_LITE_MODEL),
        gemini.Gemini(gemini.FLASH_MODEL),
    ]


@dataclass(frozen=True)
class CascadeConfig:
    theta: float = 0.6
    prompt_sha: str = PROMPT_SHA
    retry_once: bool = True


@dataclass(frozen=True)
class StepRecord:
    provider_id: str
    outcome: Outcome
    reason: str | None = None
    confidence: float | None = None

    def to_json(self) -> dict[str, object]:
        return {
            "provider": self.provider_id,
            "outcome": self.outcome,
            "reason": self.reason,
            "confidence": self.confidence,
        }


@dataclass
class CascadeResult:
    trigger: FallbackTrigger
    reading: Reading | None = None
    provider_id: str | None = None
    steps: list[StepRecord] = field(default_factory=list)
    error: str | None = None
    note: str | None = None
    cost_usd: float = 0.0

    def to_json(self) -> dict[str, object]:
        return {
            "trigger": self.trigger.to_json(),
            "provider": self.provider_id,
            "steps": [step.to_json() for step in self.steps],
            "error": self.error,
            "note": self.note,
            "confidence": self.reading.confidence if self.reading else None,
            "cost_usd": self.cost_usd,
        }


class Cascade:
    def __init__(
        self,
        providers: list[Provider],
        wire: Wire,
        keys: ProviderKeys,
        cache: ReadingCache,
        ledger: CostLedger,
        budget: BudgetGuard,
        config: CascadeConfig | None = None,
    ) -> None:
        self._providers = providers
        self._wire = wire
        self._keys = keys
        self._cache = cache
        self._ledger = ledger
        self._budget = budget
        self._config = config or CascadeConfig()

    def run(self, trigger: FallbackTrigger, image: PageImage) -> CascadeResult:
        result = CascadeResult(trigger=trigger)
        if not self._budget.allows(self._ledger, trigger.page):
            result.note = BUDGET_EXHAUSTED
            return result
        start_idx = len(self._ledger.entries)
        providers = self._providers
        if trigger.kind == "table_entropy":
            providers = providers[-1:]
            for skipped in self._providers[:-1]:
                result.steps.append(StepRecord(skipped.provider_id, "skipped", "table_entropy"))
        for provider in providers:
            step = self._step(provider, trigger, image)
            result.steps.append(step)
            if step.outcome == "ok":
                result.provider_id = provider.provider_id
                result.reading = self._last_reading
                result.cost_usd = sum(e.est_usd for e in self._ledger.entries[start_idx:])
                return result
        result.cost_usd = sum(e.est_usd for e in self._ledger.entries[start_idx:])
        result.error = ALL_FAILED if any(s.outcome == "fail" for s in result.steps) else NO_KEY
        if all(s.outcome == "skipped" for s in result.steps):
            result.error = NO_KEY
        return result

    def _step(self, provider: Provider, trigger: FallbackTrigger, image: PageImage) -> StepRecord:
        key = self._keys.for_vendor(provider.vendor)
        if not key:
            return StepRecord(provider.provider_id, "skipped", "no_key")
        cached_key = cache_key(
            image.sha256, provider.provider_id, provider.model_id, self._config.prompt_sha
        )
        hit = self._cache.get(cached_key)
        if hit is not None:
            self._ledger.record(
                LedgerEntry(
                    provider.provider_id,
                    provider.model_id,
                    0,
                    0,
                    0.0,
                    cached=True,
                    page=trigger.page,
                    outcome="cached",
                    usage_source="none",
                )
            )
            return self._judge(provider, hit, cached=True)
        request = provider.build_request(image, trigger, key)
        attempts = 2 if self._config.retry_once else 1
        for attempt_no in range(1, attempts + 1):
            response = self._send(request)
            if isinstance(response, str):
                row: LedgerOutcome = "timeout" if response == "timeout" else "provider_error"
                self._meter(provider, image, trigger.page, attempt_no, None, row, "sync")
                return StepRecord(provider.provider_id, "fail", "provider_error")
            outcome = provider.parse_response(response)
            if outcome.text is None:
                self._meter(
                    provider,
                    image,
                    trigger.page,
                    attempt_no,
                    outcome,
                    "provider_error",
                    response.transport,
                )
                _LOG.info(
                    "fallback %s failed (%s)",
                    provider.provider_id,
                    redact(str(outcome.error), self._keys.secrets()),
                )
                if outcome.retryable and attempt_no < attempts:
                    continue
                return StepRecord(provider.provider_id, "fail", "provider_error")
            try:
                reading = normalise(parse_envelope(outcome.text))
            except EnvelopeError as exc:
                self._meter(
                    provider,
                    image,
                    trigger.page,
                    attempt_no,
                    outcome,
                    "schema_parse_error",
                    response.transport,
                )
                _LOG.info("fallback %s envelope rejected: %s", provider.provider_id, exc)
                return StepRecord(provider.provider_id, "escalate", "schema_parse_error")
            self._meter(
                provider, image, trigger.page, attempt_no, outcome, "ok", response.transport
            )
            self._cache.put(cached_key, reading)
            return self._judge(provider, reading, cached=False)
        return StepRecord(provider.provider_id, "fail", "provider_error")

    def _meter(
        self,
        provider: Provider,
        image: PageImage,
        page: int,
        attempt_no: int,
        outcome: ProviderOutcome | None,
        row_outcome: LedgerOutcome,
        transport: str,
    ) -> LedgerEntry:
        source: UsageSource = "provider"
        if outcome is not None and outcome.input_tokens is not None:
            input_tokens, output_tokens = outcome.input_tokens, outcome.output_tokens or 0
        else:
            est_out = len(outcome.text) // 4 if (outcome is not None and outcome.text) else 0
            input_tokens, output_tokens, source = (
                provider.estimate_input_tokens(image),
                est_out,
                "estimated",
            )
        tier: Transport = "batch" if transport == "batch" else "sync"
        usd = estimate_usd(
            provider.price_in_per_m, provider.price_out_per_m, input_tokens, output_tokens, tier
        )
        return self._ledger.record(
            LedgerEntry(
                provider.provider_id,
                provider.model_id,
                input_tokens,
                output_tokens,
                usd,
                cached=False,
                page=page,
                attempt_no=attempt_no,
                outcome=row_outcome,
                usage_source=source,
                transport=tier,
            )
        )

    def _judge(self, provider: Provider, reading: Reading, *, cached: bool) -> StepRecord:
        self._last_reading = reading
        if reading.confidence < self._config.theta:
            return StepRecord(
                provider.provider_id, "escalate", "low_confidence", reading.confidence
            )
        return StepRecord(
            provider.provider_id, "ok", "cached" if cached else None, reading.confidence
        )

    def _send(self, request: WireRequest) -> WireResponse | str:
        try:
            return self._wire(request)
        except TimeoutError:
            return "timeout"
        except Exception as exc:  # noqa: BLE001 - any transport failure is a typed provider_error
            _LOG.info(
                "fallback wire error: %s",
                redact(f"{type(exc).__name__}: {exc}", self._keys.secrets()),
            )
            return "provider_error"
