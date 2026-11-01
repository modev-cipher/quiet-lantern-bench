"""Batch path for whole-book background ingestion (ruling #40 contract item 3; survey §1, §4).

Builds each provider's batch payload from the same envelope and parses the output back into
`ProviderOutcome`s keyed by custom id, so the cascade's judge/ledger code is shared with the
sync path at half price. Job submission and polling are the background queue's concern and are
not wired here (see docs/notes/ing-fallback-mechanism.md, UNMEASURED).

Wire facts (read 2026-09-06):
- https://developers.openai.com/api/docs/guides/batch — upload JSONL to `POST /v1/files` with
  `purpose=batch`; `POST /v1/batches {input_file_id, endpoint: "/v1/responses",
  completion_window: "24h"}`; lines `{custom_id, method, url, body}`; output lines
  `{custom_id, response: {status_code, body}, error}`, order not preserved; 50,000 requests /
  200 MB per batch; 50% discount; 24 h window, expired requests land in the error file.
- https://ai.google.dev/gemini-api/docs/batch-api — `POST models/{model}:batchGenerateContent`
  with `batch.input_config.requests.requests[] {request, metadata: {key}}` (inline, < 20 MB) or a
  JSONL file `{key, request}`; poll `GET batches/{name}` until JOB_STATE_SUCCEEDED / FAILED /
  EXPIRED (48 h); results in `response.inlinedResponses[]` as GenerateContentResponse; 50% price.
  https://ai.google.dev/api/generate-content — `candidates[].content.parts[].text`,
  `usageMetadata.promptTokenCount / candidatesTokenCount / thoughtsTokenCount`.
Usage rule (same as the sync adapters): `usageMetadata` absent -> None/None; present -> each half
verbatim, a missing half counted as 0 (provider-reported).
"""

from __future__ import annotations

import json
from dataclasses import dataclass

from txtcaster_engine.ingest.fallback.providers import gemini, openai_luna
from txtcaster_engine.ingest.fallback.providers.base import PageImage, ProviderOutcome, WireResponse
from txtcaster_engine.ingest.fallback.trigger import FallbackTrigger

OPENAI_FILES_URL = "https://api.openai.com/v1/files"
OPENAI_BATCHES_URL = "https://api.openai.com/v1/batches"
COMPLETION_WINDOW = "24h"


@dataclass(frozen=True)
class BatchItem:
    custom_id: str
    image: PageImage
    trigger: FallbackTrigger


def openai_jsonl(items: list[BatchItem]) -> str:
    provider = openai_luna.OpenAILuna()
    lines = [provider.batch_line(i.custom_id, i.image, i.trigger) for i in items]
    return "\n".join(json.dumps(line) for line in lines) + "\n"


def openai_create_batch_body(input_file_id: str) -> dict[str, object]:
    return {
        "input_file_id": input_file_id,
        "endpoint": openai_luna.BATCH_ENDPOINT,
        "completion_window": COMPLETION_WINDOW,
    }


def parse_openai_output(jsonl: str) -> dict[str, ProviderOutcome]:
    provider = openai_luna.OpenAILuna()
    out: dict[str, ProviderOutcome] = {}
    for raw in jsonl.splitlines():
        if not raw.strip():
            continue
        line = json.loads(raw)
        custom_id = str(line.get("custom_id"))
        response = line.get("response") or {}
        if line.get("error") or not isinstance(response, dict):
            out[custom_id] = ProviderOutcome(None, None, None, "batch_error")
            continue
        status = int(response.get("status_code", 0) or 0)
        out[custom_id] = provider.parse_response(WireResponse(status, response.get("body") or {}))
    return out


def gemini_inline_batch(
    model_id: str, items: list[BatchItem], display_name: str
) -> dict[str, object]:
    provider = gemini.Gemini(model_id)
    requests = []
    for item in items:
        line = provider.batch_line(item.custom_id, item.image, item.trigger)
        requests.append({"request": line["request"], "metadata": {"key": item.custom_id}})
    return {
        "batch": {
            "display_name": display_name,
            "input_config": {"requests": {"requests": requests}},
        }
    }


def gemini_batch_url(model_id: str) -> str:
    return gemini.Gemini(model_id).batch_url()


def parse_gemini_output(inlined_responses: list[dict[str, object]]) -> dict[str, ProviderOutcome]:
    out: dict[str, ProviderOutcome] = {}
    for entry in inlined_responses:
        metadata = entry.get("metadata")
        key = str(metadata.get("key")) if isinstance(metadata, dict) else str(len(out))
        response = entry.get("response")
        if not isinstance(response, dict) or entry.get("error"):
            out[key] = ProviderOutcome(None, None, None, "batch_error")
            continue
        usage = response.get("usageMetadata")
        input_tokens: int | None = None
        output_tokens: int | None = None
        if isinstance(usage, dict):
            input_tokens = int(usage.get("promptTokenCount") or 0)
            output_tokens = int(usage.get("candidatesTokenCount") or 0) + int(
                usage.get("thoughtsTokenCount") or 0
            )
        text = _candidate_text(response.get("candidates"))
        if text is None:
            out[key] = ProviderOutcome(None, input_tokens, output_tokens, "no_output_text")
        else:
            out[key] = ProviderOutcome(text, input_tokens, output_tokens)
    return out


def _candidate_text(candidates: object) -> str | None:
    if not isinstance(candidates, list) or not candidates:
        return None
    first = candidates[0]
    content = first.get("content") if isinstance(first, dict) else None
    parts = content.get("parts") if isinstance(content, dict) else None
    if not isinstance(parts, list):
        return None
    for part in parts:
        if isinstance(part, dict) and isinstance(part.get("text"), str):
            return str(part["text"])
    return None
