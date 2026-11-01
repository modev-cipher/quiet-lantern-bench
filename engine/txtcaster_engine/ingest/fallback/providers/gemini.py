"""Google Gemini over the Interactions API — one class, two model ids: `gemini-3.5-flash-lite`
(cascade step 2) and `gemini-3.8-flash` (step 3, table_entropy entry point). Ruling #40.

Wire facts (read 2026-09-06):
- https://ai.google.dev/gemini-api/docs/structured-output — `POST
  https://generativelanguage.googleapis.com/v1beta/interactions`, header `x-goog-api-key`;
  JSON output via `response_format: {type: "text", mime_type: "application/json", schema}`;
  enum, required, additionalProperties and null-in-type supported; "not all JSON Schema
  features are supported".
- https://ai.google.dev/gemini-api/docs/interactions/image-understanding.md.txt — inline
  image content item `{type: "image", data: <base64>, mime_type: "image/jpeg"}`; inline
  request cap 20 MB; `generation_config.thinking_level: "minimal"` shown.
- https://ai.google.dev/gemini-api/docs/interactions/media-resolution.md.txt — per-item
  `resolution` (Gemini 3 only) with low 280 / medium 560 / high 1120 / ultra_high 2240
  image tokens; "quality typically saturates at medium" for documents.
- https://ai.google.dev/static/api/interactions.md.txt — request fields `model`, `input`,
  `system_instruction`, `response_format`, `generation_config` (`thinking_level`,
  `max_output_tokens`), `store`; response `status` (completed | failed | incomplete |
  budget_exceeded ...), `steps[]` with `model_output` content `{type: "text", text}`,
  `usage.total_input_tokens` / `total_output_tokens` / `total_thought_tokens`; `errors[]`.
- https://ai.google.dev/gemini-api/docs/batch-api — Batch is generateContent-only
  (`models/{model}:batchGenerateContent`, JSONL lines `{key, request: {contents,
  generation_config}}`, states JOB_STATE_SUCCEEDED/FAILED/EXPIRED, 48 h expiry, 50% price).
  https://ai.google.dev/api/generate-content — `contents[].parts[]` with `text` and
  `inline_data {mime_type, data}`, `system_instruction {parts: {text}}`,
  `generationConfig.response_mime_type` / `response_schema`.
- Prices: survey §1 (list, read 2026-09-03): flash-lite $0.30 / $2.50, flash $0.75 / $3.75
  per 1M tokens, Batch 50%. Flash prices DOUBLE on 2027-01-01 ($1.50 / $7.50; Batch $0.75 /
  $3.75) — survey §0 pricing note. Survey §5: standard Gemini keys are being switched off
  "on September 2026"; onboarding must ask for a key restricted to the Gemini API.
"""

from __future__ import annotations

from txtcaster_engine.ingest.fallback.envelope import ENVELOPE_SCHEMA, SYSTEM_PROMPT
from txtcaster_engine.ingest.fallback.keys import Secret
from txtcaster_engine.ingest.fallback.providers.base import (
    PROMPT_TEXT_TOKENS,
    PageImage,
    ProviderOutcome,
    WireRequest,
    WireResponse,
    region_instruction,
)
from txtcaster_engine.ingest.fallback.trigger import FallbackTrigger

FLASH_LITE_MODEL = "gemini-3.5-flash-lite"
FLASH_MODEL = "gemini-3.8-flash"
FLASH_LITE_ID = f"gemini:{FLASH_LITE_MODEL}"
FLASH_ID = f"gemini:{FLASH_MODEL}"
INTERACTIONS_URL = "https://generativelanguage.googleapis.com/v1beta/interactions"
BATCH_URL = "https://generativelanguage.googleapis.com/v1beta/models/{model}:batchGenerateContent"
PRICES_PER_M = {FLASH_LITE_MODEL: (0.30, 2.50), FLASH_MODEL: (0.75, 3.75)}
FLASH_PRICE_DOUBLES_ON = "2027-01-01"
MAX_OUTPUT_TOKENS = 4096
MEDIUM_RESOLUTION_TOKENS = 560


class Gemini:
    vendor = "google"

    def __init__(self, model_id: str) -> None:
        if model_id not in PRICES_PER_M:
            raise ValueError(f"unknown gemini model {model_id!r}")
        self.model_id = model_id
        self.provider_id = f"gemini:{model_id}"
        self.price_in_per_m, self.price_out_per_m = PRICES_PER_M[model_id]

    def body(self, image: PageImage, trigger: FallbackTrigger) -> dict[str, object]:
        return {
            "model": self.model_id,
            "system_instruction": SYSTEM_PROMPT,
            "input": [
                {"type": "text", "text": region_instruction(trigger)},
                {
                    "type": "image",
                    "mime_type": "image/jpeg",
                    "data": image.base64(),
                    "resolution": "medium",
                },
            ],
            "response_format": {
                "type": "text",
                "mime_type": "application/json",
                "schema": ENVELOPE_SCHEMA,
            },
            "generation_config": {
                "thinking_level": "minimal",
                "max_output_tokens": MAX_OUTPUT_TOKENS,
            },
            "store": False,
        }

    def build_request(self, image: PageImage, trigger: FallbackTrigger, key: Secret) -> WireRequest:
        headers = {"x-goog-api-key": key.expose()}
        return WireRequest(self.provider_id, INTERACTIONS_URL, headers, self.body(image, trigger))

    def batch_line(
        self, custom_id: str, image: PageImage, trigger: FallbackTrigger
    ) -> dict[str, object]:
        return {
            "key": custom_id,
            "request": {
                "system_instruction": {"parts": {"text": SYSTEM_PROMPT}},
                "contents": [
                    {
                        "role": "user",
                        "parts": [
                            {"text": region_instruction(trigger)},
                            {"inline_data": {"mime_type": "image/jpeg", "data": image.base64()}},
                        ],
                    }
                ],
                "generation_config": {
                    "response_mime_type": "application/json",
                    "response_schema": ENVELOPE_SCHEMA,
                    "max_output_tokens": MAX_OUTPUT_TOKENS,
                },
            },
        }

    def batch_url(self) -> str:
        return BATCH_URL.format(model=self.model_id)

    def parse_response(self, response: WireResponse) -> ProviderOutcome:
        input_tokens, output_tokens = _usage(response.body.get("usage"))
        if response.status != 200:
            retryable = response.status == 429 or response.status >= 500
            return ProviderOutcome(
                None, input_tokens, output_tokens, f"http_{response.status}", retryable
            )
        status = response.body.get("status")
        if status != "completed":
            return ProviderOutcome(None, input_tokens, output_tokens, f"status_{status}")
        text = _model_text(response.body.get("steps"))
        if text is None:
            return ProviderOutcome(None, input_tokens, output_tokens, "no_output_text")
        return ProviderOutcome(text, input_tokens, output_tokens)

    def estimate_input_tokens(self, image: PageImage) -> int:
        return MEDIUM_RESOLUTION_TOKENS + PROMPT_TEXT_TOKENS


def _usage(usage: object) -> tuple[int | None, int | None]:
    if not isinstance(usage, dict):
        return None, None
    output = int(usage.get("total_output_tokens") or 0) + int(
        usage.get("total_thought_tokens") or 0
    )
    return int(usage.get("total_input_tokens") or 0), output


def _model_text(steps: object) -> str | None:
    if not isinstance(steps, list):
        return None
    for step in reversed(steps):
        if not isinstance(step, dict) or step.get("type") != "model_output":
            continue
        for part in step.get("content") or []:
            if isinstance(part, dict) and part.get("type") == "text":
                text = part.get("text")
                return text if isinstance(text, str) else None
    return None
