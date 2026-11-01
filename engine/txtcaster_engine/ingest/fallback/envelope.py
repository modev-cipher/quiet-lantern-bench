"""The one envelope every provider must fill (survey §2, ruling #40 contract item 2).

The schema is the same JSON object for OpenAI (`text.format` json_schema strict) and Gemini
(`response_format` type text + mime_type application/json + schema). Tables arrive as spoken
rows; coordinates, HTML and ASTs are forbidden and a wire answer containing them is a
`schema_parse_error`, never surfaced. Stability comes from this fixed prompt, the schema and
`normalise`, not from sampling controls (§2: none of the vendors offers a determinism knob).
"""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass

KINDS = ("paragraph", "heading", "table_row", "caption", "equation", "list_item")

SYSTEM_PROMPT = (
    "You are a document narrator for text-to-speech. Read the page image and return its "
    "content as a linear reading in the order a sighted reader would meet it. Rules: one "
    "item per paragraph, heading, caption, equation or list item; render every table as "
    "spoken rows, one item per row, naming the column for each cell, for example "
    "'Row 2: Transformer, cost 0.3, latency 12 ms'; inline sidebars, callouts and footnotes "
    "where a reader would encounter them; spell out mathematics in words unless it is a bare "
    "identifier; never output coordinates, HTML, Markdown tables, image descriptions of "
    "decorative elements, running headers or page numbers. If the page or region is "
    "unreadable, return an empty reading with a low confidence and say why in notes. "
    "Do not guess."
)

ENVELOPE_SCHEMA: dict[str, object] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["reading", "confidence", "notes"],
    "properties": {
        "reading": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["kind", "text", "order"],
                "properties": {
                    "kind": {"type": "string", "enum": list(KINDS)},
                    "text": {"type": "string"},
                    "order": {"type": "integer"},
                },
            },
        },
        "confidence": {"type": "number"},
        "notes": {"type": "string"},
    },
}

PROMPT_VERSION = "ing-fallback-envelope-v1"
PROMPT_SHA = hashlib.sha256(
    (PROMPT_VERSION + SYSTEM_PROMPT + json.dumps(ENVELOPE_SCHEMA, sort_keys=True)).encode()
).hexdigest()

_TAG = re.compile(r"</?[a-zA-Z][^>]*>")
_WS = re.compile(r"\s+")
_ROW_SPACE = re.compile(r"\s+([,.;:])")


class EnvelopeError(ValueError):
    """The wire answer did not satisfy the envelope; the cascade escalates."""


@dataclass(frozen=True)
class ReadingItem:
    kind: str
    text: str
    order: int

    def to_json(self) -> dict[str, object]:
        return {"kind": self.kind, "text": self.text, "order": self.order}


@dataclass(frozen=True)
class Reading:
    items: tuple[ReadingItem, ...]
    confidence: float
    notes: str = ""

    def to_json(self) -> dict[str, object]:
        return {
            "reading": [item.to_json() for item in self.items],
            "confidence": self.confidence,
            "notes": self.notes,
        }

    @classmethod
    def from_json(cls, data: dict[str, object]) -> Reading:
        return parse_envelope(json.dumps(data))


def _clean_text(text: str) -> str:
    return _ROW_SPACE.sub(r"\1", _WS.sub(" ", text)).strip()


def _item(raw: object) -> ReadingItem:
    if not isinstance(raw, dict) or set(raw) != {"kind", "text", "order"}:
        raise EnvelopeError("reading item must have exactly kind, text, order")
    kind, text, order = raw["kind"], raw["text"], raw["order"]
    if kind not in KINDS:
        raise EnvelopeError(f"unknown reading kind {kind!r}")
    if not isinstance(text, str) or _TAG.search(text):
        raise EnvelopeError("reading text must be plain speech-ready text")
    if isinstance(order, bool) or not isinstance(order, int):
        raise EnvelopeError("order must be an integer")
    return ReadingItem(kind=kind, text=_clean_text(text), order=order)


def parse_envelope(text: str) -> Reading:
    try:
        data = json.loads(text)
    except (TypeError, ValueError) as exc:
        raise EnvelopeError("wire answer is not JSON") from exc
    if not isinstance(data, dict) or not {"reading", "confidence"} <= set(data):
        raise EnvelopeError("envelope must be an object with reading and confidence")
    if set(data) - {"reading", "confidence", "notes"}:
        raise EnvelopeError("envelope carries fields outside the schema")
    confidence = data["confidence"]
    if isinstance(confidence, bool) or not isinstance(confidence, int | float):
        raise EnvelopeError("confidence must be a number")
    if not 0.0 <= float(confidence) <= 1.0:
        raise EnvelopeError("confidence must lie in 0..1")
    reading = data["reading"]
    if not isinstance(reading, list):
        raise EnvelopeError("reading must be a list")
    notes = data.get("notes", "")
    if not isinstance(notes, str) or _TAG.search(notes):
        raise EnvelopeError("notes must be plain text")
    items = tuple(sorted((_item(raw) for raw in reading), key=lambda item: item.order))
    return Reading(items=items, confidence=float(confidence), notes=_clean_text(notes))


def normalise(reading: Reading) -> Reading:
    return Reading(
        items=tuple(
            ReadingItem(item.kind, _clean_text(item.text), index + 1)
            for index, item in enumerate(reading.items)
        ),
        confidence=round(reading.confidence, 3),
        notes=_clean_text(reading.notes),
    )
