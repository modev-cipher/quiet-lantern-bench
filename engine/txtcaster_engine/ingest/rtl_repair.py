"""Right-to-left word order for blocks the layout model re-read, from the fast path's geometry.

The layout model returns a producer's stream order, which for some producers is each line's
words right to left; the fast path orders the same words from glyph geometry (`bidi`). A
block's direction is decided by the fast words inside that block's box alone: where the content
matches (below), each of the model's words has a place in the fast order and consecutive RTL
words vote by whether their places rise or fall; otherwise adjacent pairs of RTL words are
matched both ways. A Latin word or number has no vote either way, so an embedded LTR run kept in
order cannot outvote the reversed Arabic around it, and text elsewhere on the page cannot
outvote the block.

A backwards block is repaired only when its content is provably the same on both sides: the
letters and digits of the layout model's words must split exactly into the fast words in its
box, every word used once and nothing left over (a fast word may hold several of the model's
words where the text layer dropped a space), and both must carry the same vowel marks. The
block then reads the fast words in their order, split again at the model's word boundaries, so
the model's spaces are kept and only the order changes; punctuation is the text layer's, placed
from geometry, where the model's sits on the wrong side of a reversed word. A block that
fails the check keeps the model's text and is reported unresolved: an incomplete or
undecodable region is never put in its place. The block's box, label and place
in the page's reading order are the model's either way, so column order is never touched.
"""

from __future__ import annotations

import unicodedata
from collections import Counter
from dataclasses import dataclass, replace

from txtcaster_engine.ingest import arabic_shaping, bidi
from txtcaster_engine.ingest.blocks import TextBlock
from txtcaster_engine.ingest.ingestor import RawWord


def _content(ch: str) -> bool:
    return unicodedata.category(ch)[0] in "LN"


def key(token: str) -> str:
    """A word's letters and digits, without marks or punctuation, Presentation Forms folded."""
    return "".join(ch for ch in arabic_shaping.letters(token) if _content(ch))


def _rtl_pairs(keys: list[str]) -> set[tuple[str, str]]:
    return {
        (first, second)
        for first, second in zip(keys, keys[1:], strict=False)
        if bidi.has_rtl(first) and bidi.has_rtl(second)
    }


def _model_tokens(text: str) -> list[str]:
    """The model's words, a space before a combining mark dropped (the mark's own letter is
    on the left of that space, so the space splits one word in two)."""
    tokens: list[str] = []
    for token in arabic_shaping.letters(text).split():
        if tokens and unicodedata.category(token[0]) == "Mn":
            tokens[-1] += token
        else:
            tokens.append(token)
    return tokens


def _split(word: str, pieces: Counter[str]) -> list[str] | None:
    if not word:
        return []
    for size in range(len(word), 0, -1):
        piece = word[:size]
        if pieces[piece] <= 0:
            continue
        pieces[piece] -= 1
        rest = _split(word[size:], pieces)
        if rest is not None:
            return [piece, *rest]
        pieces[piece] += 1
    return None


def _cut(raw: str, sizes: list[int]) -> list[str]:
    """`raw` cut after each run of `sizes` letters and digits, marks kept on their letter."""
    out: list[str] = []
    current = ""
    counted = 0
    targets = iter(sizes)
    target = next(targets, None)
    for ch in raw:
        if target is not None and counted == target and _content(ch):
            out.append(current)
            current, counted = "", 0
            target = next(targets, None)
        current += ch
        counted += _content(ch)
    out.append(current)
    return [part for part in out if part]


def _inside(word: RawWord, box: tuple[float, float, float, float]) -> bool:
    x0, y0, x1, y1 = box
    return x0 <= (word[0] + word[2]) / 2 <= x1 and y0 <= (word[1] + word[3]) / 2 <= y1


@dataclass
class Outcome:
    blocks: list[TextBlock]
    repaired: int = 0
    unresolved: int = 0


def reads_backwards(model: list[str], fast: list[str]) -> bool:
    reference = _rtl_pairs(fast)
    return len(_rtl_pairs(model[::-1]) & reference) > len(_rtl_pairs(model) & reference)


def _marks(text: str) -> Counter[str]:
    return Counter(ch for ch in arabic_shaping.letters(text) if unicodedata.category(ch) == "Mn")


def rebuild(text: str, fast_raw: list[str]) -> tuple[str, list[str]] | None:
    """The block's text in the fast words' order and the model's words in that order, or None
    when the content differs."""
    pieces = Counter(key(token) for token in _model_tokens(text) if key(token))
    words: list[str] = []
    order: list[str] = []
    for raw in fast_raw:
        word = key(raw)
        if not word:
            words.append(raw)
            continue
        parts = _split(word, pieces)
        if parts is None:
            return None
        order.extend(parts)
        words.extend(_cut(raw, [len(part) for part in parts[:-1]]))
    if +pieces or _marks(text) != _marks(" ".join(fast_raw)):
        return None
    return " ".join(words), order


def _runs_backwards(model: list[str], order: list[str]) -> bool:
    """Whether consecutive RTL words of the model fall later-then-earlier in the fast order
    more often than the other way round (a word repeated takes its places in turn)."""
    places: dict[str, list[int]] = {}
    for index, word in enumerate(order):
        places.setdefault(word, []).append(index)
    ranks = [places[word].pop(0) for word in model]
    forward = backward = 0
    for (first, a), (second, b) in zip(
        zip(model, ranks, strict=True), zip(model[1:], ranks[1:], strict=True), strict=False
    ):
        if bidi.has_rtl(first) and bidi.has_rtl(second):
            forward += a < b
            backward += a > b
    return backward > forward


def _region(words: list[RawWord], box: tuple[float, float, float, float]) -> list[str]:
    """The fast words inside `box`: its lines top to bottom, each line in the fast order.

    Within a line the fast order is the logical one; between lines it follows the fast
    blocks, which in a table can run a cell's lines out of sequence."""
    lines: dict[tuple[int, int], list[tuple[int, RawWord]]] = {}
    for index, word in enumerate(words):
        if _inside(word, box):
            lines.setdefault((word[5], word[6]), []).append((index, word))
    ordered = sorted(
        lines.values(),
        key=lambda line: (sum(w[1] + w[3] for _, w in line) / (2 * len(line)), line[0][0]),
    )
    return [arabic_shaping.letters(word[4]) for line in ordered for _, word in line]


def repair(blocks: list[TextBlock], words: list[RawWord]) -> Outcome:
    outcome = Outcome(blocks=[])
    for block in blocks:
        region = _region(words, block.bbox)
        model = [k for k in (key(token) for token in _model_tokens(block.text)) if k]
        plan = rebuild(block.text, region)
        if plan is None:
            if reads_backwards(model, [k for k in (key(raw) for raw in region) if k]):
                outcome.unresolved += 1
            outcome.blocks.append(block)
            continue
        text, order = plan
        if _runs_backwards(model, order):
            outcome.repaired += 1
            outcome.blocks.append(replace(block, text=text))
        else:
            outcome.blocks.append(block)
    return outcome
