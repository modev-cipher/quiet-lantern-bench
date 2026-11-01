import threading
from collections.abc import Callable

import pysbd

CLAUSE_SEPARATORS = ("; ", ", ")
MIN_CLAUSE_CUT_RATIO = 0.6

_local = threading.local()


def segmenter() -> pysbd.Segmenter:
    instance = getattr(_local, "segmenter", None)
    if instance is None:
        instance = pysbd.Segmenter(language="en", clean=False)
        _local.segmenter = instance
    return instance


def sentences(text: str) -> list[str]:
    return [stripped for part in segmenter().segment(text) if (stripped := str(part).strip())]


def _cut_point(window: str) -> int:
    floor = int(len(window) * MIN_CLAUSE_CUT_RATIO)
    for separator in CLAUSE_SEPARATORS:
        found = window.rfind(separator)
        if found >= floor:
            return found + len(separator) - 1
    return window.rfind(" ")


def _largest_fit(text: str, measure: Callable[[str], int], budget: int) -> int:
    low, high = 1, len(text)
    while low < high:
        mid = (low + high + 1) // 2
        if measure(text[:mid]) <= budget:
            low = mid
        else:
            high = mid - 1
    return low


def split_oversized(sentence: str, measure: Callable[[str], int], budget: int) -> list[str]:
    if measure(sentence) <= budget:
        return [sentence]
    parts: list[str] = []
    remaining = sentence
    while measure(remaining) > budget:
        window_end = _largest_fit(remaining, measure, budget)
        split_at = _cut_point(remaining[:window_end])
        if split_at <= 0:
            parts.append(remaining[:window_end])
            remaining = remaining[window_end:]
            continue
        parts.append(remaining[:split_at].rstrip())
        remaining = remaining[split_at:].lstrip()
    if remaining:
        parts.append(remaining)
    return parts


def chunk_text(text: str, measure: Callable[[str], int], budget: int) -> list[str]:
    """Sentence-preserving chunks no larger than `budget` under `measure`.

    `measure` and `budget` carry no defaults on purpose: the two engines that use this
    disagree about what a unit is -- chatterbox counts its own BPE tokens against a
    truncation ceiling, piper counts espeak phoneme ids against a cost ceiling -- and a
    default here would silently hand one engine the other's regime.
    """
    chunks: list[str] = []
    current = ""
    for sentence in sentences(text):
        for piece in split_oversized(sentence, measure, budget):
            if not piece:
                continue
            if not current:
                current = piece
            elif measure(f"{current} {piece}") <= budget:
                current = f"{current} {piece}"
            else:
                chunks.append(current)
                current = piece
    if current:
        chunks.append(current)
    return chunks
