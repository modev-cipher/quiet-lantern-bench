import json
from dataclasses import dataclass

CONFIDENCE_FLOOR = 0.4

_BASE = (
    "You are describing a {noun} from a book page so it can be read aloud to a listener "
    "who cannot see it. {focus} "
    "Respond with only a JSON object with keys: "
    '"caption" (one short sentence naming what it shows), '
    '"explanation" (2-3 sentences with the key takeaway and why it matters in context), '
    '"confidence" (a number from 0 to 1 for how sure you are).'
)

_FOCUS = {
    "chart": (
        "Identify the axes or categories, the units if visible, and the overall trend "
        "or comparison the chart makes."
    ),
    "equation": (
        "Read the equation symbolically in words, naming each symbol, then state what "
        "the relationship means."
    ),
    "table": (
        "Summarize what the table compares across its rows and columns and the most "
        "notable value or pattern."
    ),
    "figure": "Describe the main subject, any labels, and the point the figure illustrates.",
}

_NOUN = {
    "chart": "chart",
    "equation": "equation",
    "table": "table",
    "figure": "figure",
}


def _first_json_object(raw: str) -> dict[str, object] | None:
    decoder = json.JSONDecoder()
    start = raw.find("{")
    while start != -1:
        try:
            payload, _ = decoder.raw_decode(raw, start)
        except json.JSONDecodeError:
            start = raw.find("{", start + 1)
            continue
        if isinstance(payload, dict):
            return payload
        start = raw.find("{", start + 1)
    return None


@dataclass(frozen=True)
class FigureCaption:
    region_id: str
    caption: str
    explanation: str
    confidence: float

    def to_json(self) -> dict[str, object]:
        return {
            "region_id": self.region_id,
            "caption": self.caption,
            "explanation": self.explanation,
            "confidence": self.confidence,
        }


def prompt_for(region_type: str) -> str:
    key = region_type if region_type in _FOCUS else "figure"
    return _BASE.format(noun=_NOUN[key], focus=_FOCUS[key])


def parse_caption(region_id: str, raw: str) -> FigureCaption | None:
    payload = _first_json_object(raw)
    if payload is None:
        return None
    caption = payload.get("caption")
    explanation = payload.get("explanation")
    confidence = payload.get("confidence")
    if not isinstance(caption, str) or not caption.strip():
        return None
    if not isinstance(explanation, str) or not explanation.strip():
        return None
    if isinstance(confidence, bool) or not isinstance(confidence, (int, float)):
        return None
    clamped = min(1.0, max(0.0, float(confidence)))
    return FigureCaption(
        region_id=region_id,
        caption=caption.strip(),
        explanation=explanation.strip(),
        confidence=clamped,
    )
