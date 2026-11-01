"""Arabic joining for text the engine draws itself (EPUB reflow) through Pillow's BASIC layout
engine, which does no OpenType shaping. Letters map to their Unicode Presentation Forms
(A: FB50–FBFF, B: FE70–FEFF) by joining context — isolated, initial, medial, final — and a
lam followed by an alef to the ligature; `drawn` then puts the string in visual order for a
left-to-right glyph painter: right-to-left letters reversed, digit and Latin runs kept, paired
brackets mirrored, each non-spacing mark placed BEFORE its base so a zero-advance mark lands
over the base's slot (drawn after it, the BASIC engine paints it over the next glyph). Joining
classes derive from the forms Unicode defines (`unicodedata` decompositions): a letter with a
medial form is dual-joining, one with only a final form right-joining; tatweel and ZWJ join both
sides, ZWNJ breaks a join, marks (Mn) are transparent. Measuring and painting only: extraction
keeps the logical text. No GPOS, so mark placement is approximate and no other ligatures form."""

from __future__ import annotations

import unicodedata
from functools import lru_cache

from txtcaster_engine.ingest.bidi import PAIRS

FORMS: dict[str, dict[str, str]] = {}
for _cp in (*range(0xFB50, 0xFC00), *range(0xFE70, 0xFF00)):
    _decomposition = unicodedata.decomposition(chr(_cp))
    if not _decomposition.startswith("<"):
        continue
    _tag, *_bases = _decomposition.split()
    _base = "".join(chr(int(b, 16)) for b in _bases)
    if len(_bases) == 1 or (len(_bases) == 2 and _base[0] == "ل"):
        FORMS.setdefault(_base, {})[_tag.strip("<>")] = chr(_cp)

DUAL = frozenset(b for b, f in FORMS.items() if len(b) == 1 and "medial" in f) | {"ـ", "‍"}
RIGHT = frozenset(b for b, f in FORMS.items() if len(b) == 1 and "final" in f) - DUAL
ALEFS = frozenset(b[1] for b in FORMS if len(b) == 2)
ZERO_WIDTH = frozenset({"‌", "‍"})


def _kind(ch: str) -> str:
    if ch in DUAL:
        return "D"
    if ch in RIGHT:
        return "R"
    return "T" if ch and unicodedata.category(ch) == "Mn" else "N"


def _neighbour(chars: list[str], i: int, step: int) -> str:
    j = i + step
    while 0 <= j < len(chars) and _kind(chars[j]) == "T":
        j += step
    return chars[j] if 0 <= j < len(chars) else ""


def _is_presentation_form(ch: str) -> bool:
    point = ord(ch)
    return 0xFB50 <= point <= 0xFDFF or 0xFE70 <= point <= 0xFEFF


def letters(text: str) -> str:
    """Presentation Forms back to the letters and marks they draw (NFKC); the rest unchanged.

    The inverse of `shape` for text read from a page that stores the drawn forms. An isolated
    mark form normalises to a space and the mark; the space is dropped so the mark stays on
    its letter instead of splitting the word."""
    if not any(_is_presentation_form(ch) for ch in text):
        return text
    return "".join(
        unicodedata.normalize("NFKC", ch).lstrip(" ") or ch if _is_presentation_form(ch) else ch
        for ch in text
    )


@lru_cache(maxsize=200_000)
def shape(token: str) -> str:
    """Presentation forms in logical order; characters without forms pass through."""
    chars = list(token)
    out: list[str] = []
    skip = -1
    for i, ch in enumerate(chars):
        kind = _kind(ch)
        if i == skip or ch in ZERO_WIDTH:
            continue
        if kind not in ("D", "R") or ch not in FORMS:
            out.append(ch)
            continue
        joins_prev = _kind(_neighbour(chars, i, -1)) == "D"
        following = _neighbour(chars, i, 1)
        if ch == "ل" and following in ALEFS:
            skip = chars.index(following, i + 1)
            out.append(FORMS[ch + following]["final" if joins_prev else "isolated"])
            continue
        joins_next = kind == "D" and _kind(following) in ("D", "R")
        form = {(True, True): "medial", (True, False): "final", (False, True): "initial"}.get(
            (joins_prev, joins_next), "isolated"
        )
        out.append(FORMS[ch].get(form) or FORMS[ch].get("isolated") or ch)
    return "".join(out)


@lru_cache(maxsize=200_000)
def drawn(token: str) -> str:
    """The string to hand a left-to-right glyph painter for one token: shaped, and for a token
    holding right-to-left letters put in visual order — units reversed, where a unit is a
    letter with its marks (marks first), a run of digits or Latin letters with the separators
    between them (kept left-to-right), or one other character (paired brackets mirrored)."""
    shaped = shape(token)
    classes = [unicodedata.bidirectional(ch) for ch in shaped]
    if not any(cls in ("R", "AL") for cls in classes):
        return shaped
    in_run = [cls in ("L", "EN", "AN") for cls in classes]
    for i in range(1, len(shaped) - 1):
        if classes[i] in ("CS", "ES") and in_run[i - 1] and in_run[i + 1]:
            in_run[i] = True
    units: list[str] = []
    runs: list[bool] = []
    for ch, cls, run in zip(shaped, classes, in_run, strict=True):
        if cls == "NSM" and units:
            units[-1] = units[-1] + ch if runs[-1] else ch + units[-1]
        elif run and units and runs[-1]:
            units[-1] += ch
        else:
            units.append(ch if run else PAIRS.get(ch, ch))
            runs.append(run)
    return "".join(reversed(units))
