import json
from dataclasses import dataclass
from typing import Any

from txtcaster_engine.tts.backend import Word

WORDS_VERSION = 2
TIMEBASE_TRIMMED = "trimmed"
TIMEBASE_UNKNOWN = "unknown"
DECLARABLE_TIMEBASES = frozenset({TIMEBASE_TRIMMED})


@dataclass(frozen=True)
class WordsEnvelope:
    entries: list[Any]
    timebase: str


def encode_envelope(words: list[Word], timebase: str) -> bytes:
    declared = timebase if timebase in DECLARABLE_TIMEBASES else TIMEBASE_UNKNOWN
    payload = {
        "v": WORDS_VERSION,
        "timebase": declared,
        "words": [word.to_json() for word in words],
    }
    return json.dumps(payload).encode("utf-8")


def decode_envelope(raw: str) -> WordsEnvelope | None:
    try:
        parsed = json.loads(raw)
    except ValueError:
        return None
    if isinstance(parsed, list):
        return WordsEnvelope(entries=parsed, timebase=TIMEBASE_UNKNOWN)
    if not isinstance(parsed, dict):
        return None
    entries = parsed.get("words")
    if not isinstance(entries, list):
        return None
    if parsed.get("v") != WORDS_VERSION:
        return WordsEnvelope(entries=entries, timebase=TIMEBASE_UNKNOWN)
    declared = parsed.get("timebase")
    if not isinstance(declared, str) or declared not in DECLARABLE_TIMEBASES:
        return WordsEnvelope(entries=entries, timebase=TIMEBASE_UNKNOWN)
    return WordsEnvelope(entries=entries, timebase=declared)
