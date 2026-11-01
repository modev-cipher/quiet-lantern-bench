import json
import logging
import re
from collections.abc import Callable
from dataclasses import replace

from txtcaster_engine.ingest.errors import IngestCancelled
from txtcaster_engine.oom import is_oom
from txtcaster_engine.script.assemble import SourceUnit
from txtcaster_engine.script.chunker import CHARS_PER_TOKEN, Chunk, chunk_units
from txtcaster_engine.script.cues import decide_code
from txtcaster_engine.script.teacher import CTX_SAFETY_TOKENS, GenerateFn

logger = logging.getLogger(__name__)

LLM_NORMALIZED_FLAG = "llm_normalized"
MAX_ATTEMPTS = 3
NORMALIZE_CTX_FRACTION = 0.25
NORMALIZE_EXPANSION = 1.6
NORMALIZE_MIN_CHUNK_CHARS = 512


def normalize_chunk_budget_chars(ctx: int) -> int:
    return max(NORMALIZE_MIN_CHUNK_CHARS, int(ctx * NORMALIZE_CTX_FRACTION) * CHARS_PER_TOKEN)


def normalize_budget_tokens(source_chars: int, prompt_chars: int, ctx: int) -> int:
    desired = int(source_chars / CHARS_PER_TOKEN * NORMALIZE_EXPANSION) + 1
    remaining = ctx - prompt_chars // CHARS_PER_TOKEN - CTX_SAFETY_TOKENS
    return max(1, min(desired, remaining))


NORMALIZE_SYSTEM = (
    "You prepare text to be read aloud by a text-to-speech engine. Rewrite each "
    "paragraph below into its exact spoken form: expand numbers, units, and "
    "abbreviations into words, read mathematical symbols and formulas out loud "
    '(for example "E = mc\u00b2" becomes "E equals m c squared"), remove citation '
    'brackets like "[12]", and replace URLs with "see the link on this page". '
    "Do NOT reword, summarize, reorder, add, or omit anything else; keep every "
    "sentence otherwise verbatim. Output ONLY a JSON array of strings with "
    "exactly one string per input paragraph, in the same order."
)
STRICT_SUFFIX = (
    "\nYour previous answer was invalid. Respond with NOTHING except a valid JSON "
    "array of strings, one per input paragraph, same order, same count."
)

_FENCE_RE = re.compile(r"^```[a-zA-Z]*\n|\n?```$")


def build_prompt(paragraphs: list[str], strict: bool) -> str:
    parts = [NORMALIZE_SYSTEM]
    if strict:
        parts.append(STRICT_SUFFIX)
    numbered = "\n\n".join(f"{i + 1}. {text}" for i, text in enumerate(paragraphs))
    parts.append(f"\nPARAGRAPHS ({len(paragraphs)}):\n{numbered}\n\nJSON:")
    return "\n".join(parts)


def parse_normalized(raw: str, expected: int) -> list[str] | None:
    cleaned = _FENCE_RE.sub("", raw.strip()).strip()
    start = cleaned.find("[")
    end = cleaned.rfind("]")
    if start < 0 or end <= start:
        return None
    try:
        data = json.loads(cleaned[start : end + 1])
    except json.JSONDecodeError:
        return None
    if not isinstance(data, list) or len(data) != expected:
        return None
    texts: list[str] = []
    for item in data:
        if not isinstance(item, str) or not item.strip():
            return None
        texts.append(item.strip())
    return texts


def _normalize_chunk(generate: GenerateFn, paragraphs: list[str], ctx: int) -> list[str] | None:
    source_chars = sum(len(text) for text in paragraphs)
    for attempt in range(MAX_ATTEMPTS):
        prompt = build_prompt(paragraphs, strict=attempt > 0)
        budget = normalize_budget_tokens(source_chars, len(prompt), ctx)
        try:
            raw = generate(prompt, budget)
        except Exception as exc:
            if is_oom(exc):
                raise
            logger.warning(
                "llm normalization failed (attempt %d/%d): %s", attempt + 1, MAX_ATTEMPTS, exc
            )
            continue
        texts = parse_normalized(raw, len(paragraphs))
        if texts is not None:
            return texts
        logger.warning(
            "llm normalization output malformed (attempt %d/%d): %d chars against a %d token "
            "budget for %d paragraph(s)",
            attempt + 1,
            MAX_ATTEMPTS,
            len(raw),
            budget,
            len(paragraphs),
        )
    return None


def _eligible(unit: SourceUnit) -> bool:
    return unit.kind == "text" and not decide_code(unit.label, unit.text).is_code


def normalize_units(
    units: list[SourceUnit],
    ctx: int,
    generate: GenerateFn,
    on_chunk: Callable[[int, int], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
) -> tuple[list[SourceUnit], list[bool]]:
    chunks: list[Chunk] = chunk_units(units, ctx, budget_chars=normalize_chunk_budget_chars(ctx))
    total = len(chunks)
    if on_chunk is not None:
        on_chunk(0, total)
    result: list[SourceUnit] = []
    normalized: list[bool] = []
    for done, chunk in enumerate(chunks, start=1):
        if is_cancelled is not None and is_cancelled():
            raise IngestCancelled()
        eligible = [unit for unit in chunk.units if _eligible(unit)]
        texts = (
            _normalize_chunk(generate, [unit.text for unit in eligible], ctx) if eligible else None
        )
        texts_iter = iter(texts) if texts is not None else None
        for unit in chunk.units:
            if texts_iter is None or not _eligible(unit):
                result.append(unit)
                normalized.append(False)
                continue
            result.append(replace(unit, text=next(texts_iter)))
            normalized.append(True)
        if on_chunk is not None:
            on_chunk(done, total)
    return result, normalized
