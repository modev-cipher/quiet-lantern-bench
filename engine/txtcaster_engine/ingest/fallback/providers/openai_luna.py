"""OpenAI `gpt-5.6-luna` over the Responses API — cascade step 1 (ruling #40).

Wire facts (read 2026-09-06):
- https://developers.openai.com/api/docs/guides/structured-outputs — `POST /v1/responses`,
  `text.format = {type: "json_schema", name, strict: true, schema}`; every object needs
  `additionalProperties: false` and all properties in `required`; the answer is the
  `output[]` item of type `message` whose `content[]` holds `{type: "output_text", text}`; a
  refusal is a content item `{type: "refusal", refusal}` with status still `completed`;
  truncation is `status: "incomplete"` with `incomplete_details.reason = max_output_tokens`.
- https://developers.openai.com/api/docs/guides/images-vision — image input item
  `{type: "input_image", image_url: "data:image/jpeg;base64,...", detail}`; JPEG accepted;
  patch tokens = ceil(w/32) x ceil(h/32) x 1.2 for the gpt-5.6 family (1000x1300 -> 1,575).
- https://developers.openai.com/api/docs/guides/reasoning — `reasoning: {effort}` with
  values none/minimal/low/medium/high/xhigh/max (model dependent); reasoning tokens bill as
  output and appear in `usage.output_tokens_details.reasoning_tokens`.
- Prices: survey §1 (list, read 2026-09-03): $0.20 in / $1.20 out per 1M tokens; Batch and
  Flex at 50%. Survey §5: OpenAI forbids embedding keys in browsers; the desktop sidecar is a
  native process calling with the user's own project key, held in one `Authorization` header.
"""

from __future__ import annotations

import math

from txtcaster_engine.ingest.fallback.envelope import ENVELOPE_SCHEMA, SYSTEM_PROMPT
from txtcaster_engine.ingest.fallback.keys import Secret
from txtcaster_engine.ingest.fallback.providers.base import (
    PROMPT_TEXT_TOKENS,
    PageImage,
    ProviderOutcome,
    WireRequest,
    WireResponse,
    image_px,
    region_instruction,
)
from txtcaster_engine.ingest.fallback.trigger import FallbackTrigger

MODEL_ID = "gpt-5.6-luna"
PROVIDER_ID = f"openai:{MODEL_ID}"
RESPONSES_URL = "https://api.openai.com/v1/responses"
BATCH_ENDPOINT = "/v1/responses"
PRICE_IN_PER_M = 0.20
PRICE_OUT_PER_M = 1.20
MAX_OUTPUT_TOKENS = 4096
PATCH_PX = 32
PATCH_MULTIPLIER = 1.2


class OpenAILuna:
    provider_id = PROVIDER_ID
    vendor = "openai"
    model_id = MODEL_ID
    price_in_per_m = PRICE_IN_PER_M
    price_out_per_m = PRICE_OUT_PER_M

    def body(self, image: PageImage, trigger: FallbackTrigger) -> dict[str, object]:
        return {
            "model": self.model_id,
            "instructions": SYSTEM_PROMPT,
            "input": [
                {
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": region_instruction(trigger)},
                        {"type": "input_image", "image_url": image.data_url(), "detail": "high"},
                    ],
                }
            ],
            "reasoning": {"effort": "low"},
            "max_output_tokens": MAX_OUTPUT_TOKENS,
            "store": False,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "reading_envelope",
                    "strict": True,
                    "schema": ENVELOPE_SCHEMA,
                }
            },
        }

    def build_request(self, image: PageImage, trigger: FallbackTrigger, key: Secret) -> WireRequest:
        headers = {"Authorization": f"Bearer {key.expose()}"}
        return WireRequest(self.provider_id, RESPONSES_URL, headers, self.body(image, trigger))

    def batch_line(
        self, custom_id: str, image: PageImage, trigger: FallbackTrigger
    ) -> dict[str, object]:
        return {
            "custom_id": custom_id,
            "method": "POST",
            "url": BATCH_ENDPOINT,
            "body": self.body(image, trigger),
        }

    def parse_response(self, response: WireResponse) -> ProviderOutcome:
        input_tokens, output_tokens = _usage(response.body.get("usage"))
        if response.status != 200:
            retryable = response.status == 429 or response.status >= 500
            return ProviderOutcome(
                None, input_tokens, output_tokens, f"http_{response.status}", retryable
            )
        if response.body.get("status") == "incomplete":
            return ProviderOutcome(None, input_tokens, output_tokens, "incomplete")
        text = _output_text(response.body.get("output"))
        if text is None:
            return ProviderOutcome(None, input_tokens, output_tokens, "no_output_text")
        return ProviderOutcome(text, input_tokens, output_tokens)

    def estimate_input_tokens(self, image: PageImage) -> int:
        width, height = image_px(image)
        patches = math.ceil(width / PATCH_PX) * math.ceil(height / PATCH_PX)
        return math.ceil(patches * PATCH_MULTIPLIER) + PROMPT_TEXT_TOKENS


def _usage(usage: object) -> tuple[int | None, int | None]:
    if not isinstance(usage, dict):
        return None, None
    return int(usage.get("input_tokens") or 0), int(usage.get("output_tokens") or 0)


def _output_text(output: object) -> str | None:
    if not isinstance(output, list):
        return None
    for item in output:
        if not isinstance(item, dict) or item.get("type") != "message":
            continue
        for part in item.get("content") or []:
            if isinstance(part, dict) and part.get("type") == "output_text":
                text = part.get("text")
                return text if isinstance(text, str) else None
    return None
