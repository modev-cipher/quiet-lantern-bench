"""ING-6 bidi rule: the logical order of a visual text row from glyph geometry (ruling #48, Lane 1).

The Unicode Bidirectional Algorithm (UAX #9) resolves a paragraph into embedding levels and lays
glyphs out visually: each level run is reversed in turn (rule L2), so on an RTL base line the RTL
words advance leftwards while any embedded LTR run (Latin letters, digits, operators, code) keeps
advancing rightwards inside its own slot. A PDF text layer hands back the result of that layout, not
its input: producers write glyphs in stream order that may be logical (WeasyPrint, Skia), visual
(Aspose, iTextSharp) or per-word reversed (pdfium's own text page), and no order is marked. The rule
here inverts L2 from geometry alone, so both backends emit one logical order for one page:

1. A row's base direction is its dominant strong script (UBA classes R / AL against L) by run
   count; on a tie the page's base decides on an RTL page, letters otherwise; per row, not per
   page. Rows without an RTL glyph are returned untouched.
2. Glyphs are clustered into words along x (whitespace or a gap over WORD_GAP em); non-spacing
   marks stay with their base. A cluster is R if it holds an R glyph, else L if it holds an L glyph,
   else weak (digits, punctuation, symbols: UBA EN / AN / ES / CS / ON).
3. On an R base row the clusters are read right to left. A maximal visual run of L and weak clusters
   that contains an L cluster is one atom, read left to right and never split; its glyphs run x
   ascending. An R cluster's glyphs run x descending. A weak cluster between two R clusters (a digit
   run) stays at its slot and reads x ascending (rules W2 / L2: a number takes the embedding
   direction but is never mirrored). A weak cluster set apart from an atom's edge by over
   LABEL_GAP em (a circled exercise numeral, a list label) is a slot of its own, not part of the
    atom. Sentence punctuation at an atom's outer edge is a neutral of the base direction (N2):
    it is read after the atom when it sits where the base continues, before it otherwise
    (`f(x).` from the drawn `.f(x)`); paired brackets and quotes stay with the atom. A digit run
    against a bracket whose extractor sequence exactly reverses its geometry (OCR RTL-boxed verse
    numerals such as `[158`) reads in extractor order instead.
   On an L base row the mirror image holds for R runs. An R cluster that also holds L glyphs
   (an Arabic prefix or question mark set solid against a Latin token) resolves its glyphs by the
   same rule, so the Latin run inside keeps its x ascending order.
4. Latin identifiers, technical terms and code tokens are L clusters by class and so fall under 3
   with no word list: the invariant is structural, not lexical.
5. Column and row order for a page follow the page's base direction (rule 1 over all its text):
   right column first on an RTL page; cell text inside a table follows rule 1 per cell; a row of
   an RTL page without an RTL glyph is still its cells read right to left.
6. Bracket shapes (L4 inverted): a layer that records drawn shapes, or an extractor that mirrors
   the brackets it resolves as RTL, leaves the logical row with brackets swapped. They are swapped
   back when that balances the row; when neither reading balances and the count is even, each
   bracket takes the orientation its nesting demands.
7. No geometry, no reordering: a row whose glyphs mostly share one box (a line the layer boxed as
   a whole) keeps its stream order; a same-box run over two glyphs is an opaque unit, a two-glyph
   one is a ligature decomposition, which a producer's ToUnicode may give in either order, even
   glyph by glyph within one font. Inside an RTL cluster it opens with a letter that joins
   forward (Unicode gives it an initial or medial form), so when only one order opens that way
   it is the reading (a lam-alef written alef first). When both do, the order Unicode lists as
   a ligature (Presentation Forms) is read; a pair listed both ways, or neither, is read
   reversed. A zero-width glyph is a
   decomposition piece only next to its lam-alef partner (the one mandatory Arabic ligature;
   Skia gives a hamza-alef a zero-width box of its own). A zero-width space glyph (pdfium's own
   word breaks, wrong inside Arabic words) separates only across a gap over SPACE_MIN em; a
   space glyph with a span separates only as a real forward gap under SPACE_MAX em that covers
   no glyph centre (a space synthesised across two visual pieces spans the glyphs between them).
8. Row assembly is the same on both backends: a gap over ATOM_GAP em ends an atom (a cell or a
   column boundary); a native band splits into cells at a gap over CELL_GAP_EM em (pymupdf puts
   side-by-side exercise or table cells in one line), visual rows are lines whose boxes overlap
   by half the shorter, a pymupdf line boundary inside a row is at least a word boundary, and a
   cell the rule leaves alone keeps its lines and stream order. On the pdfium layer an embedded
   run set in another size fails the equal-top test of the line joiner and is re-joined to its
   row when the vertical centres lie within half an em and no full-height vertical rule stands
   between (a circled numeral is drawn as short rule pieces, not a wall). Rule 1 counts runs, so
   an embedded formula or phrase is one run however many tokens it holds.
"""

from __future__ import annotations

import unicodedata
from collections import Counter
from collections.abc import Sequence
from dataclasses import dataclass, replace
from typing import Any, Protocol, TypeVar, cast

from .bidi_numeric import NUMBERS, classify, resolve_classes, resolve_numeric_runs

WORD_GAP = 0.25
ATOM_GAP = 1.5
LABEL_GAP = 0.75
SPACE_MAX = 2.0
SPACE_MIN = 0.1
LIGATURE_TOL = 0.1
LAM_ALEF = frozenset("لاأإآ")
STRONG_R = frozenset({"R", "AL"})
WEAK = frozenset({"EN", "AN", "ES", "ET", "CS", "ON", "BN"})
PAIRS = {"(": ")", ")": "(", "[": "]", "]": "[", "{": "}", "}": "{"}
SENTENCE = frozenset(".,:;!?،؛؟…")
JOINS_FORWARD = frozenset(
    {"ـ"}
    | {
        chr(int(decomposition.split()[1], 16))
        for point in (*range(0xFB50, 0xFC00), *range(0xFE70, 0xFF00))
        if (decomposition := unicodedata.decomposition(chr(point))).split()[:1]
        in (["<initial>"], ["<medial>"])
        and len(decomposition.split()) == 2
    }
)
LIGATURES = frozenset(
    "".join(chr(int(part, 16)) for part in decomposition.split()[1:])
    for point in (*range(0xFB50, 0xFE00), *range(0xFE70, 0xFF00))
    if (decomposition := unicodedata.decomposition(chr(point))).startswith("<")
    and len(decomposition.split()) == 3
)


class Glyph(Protocol):
    x0: float
    y0: float
    x1: float
    y1: float
    text: str
    font_size: float


G = TypeVar("G", bound=Glyph)
Tally = Counter[str]  # numeric_order_provenance: agree / conflict / no_seq


def strong_class(ch: str) -> str:
    kind = unicodedata.bidirectional(ch)
    if kind in STRONG_R:
        return "R"
    if kind == "L":
        return "L"
    return "N" if kind in WEAK else ""


def has_rtl(text: str) -> bool:
    return any(unicodedata.bidirectional(ch) in STRONG_R for ch in text)


def base_direction(text: str) -> str:
    kinds = [{strong_class(ch) for ch in word} for word in text.split()]
    r_words = sum(1 for k in kinds if "R" in k)
    l_words = sum(1 for k in kinds if "L" in k and "R" not in k)
    if r_words != l_words:
        return "R" if r_words > l_words else "L"
    r = sum(1 for ch in text if unicodedata.bidirectional(ch) in STRONG_R)
    left = sum(1 for ch in text if unicodedata.bidirectional(ch) == "L")
    return "R" if r > left else "L"


@dataclass
class _Cluster:
    units: list[list[Glyph]]
    kind: str

    @property
    def x0(self) -> float:
        return min(_extent(u)[0] for u in self.units)

    @property
    def x1(self) -> float:
        return max(_extent(u)[1] for u in self.units)

    @property
    def size(self) -> float:
        return max(g.font_size for u in self.units for g in u)


def _is_mark(g: Glyph) -> bool:
    return bool(g.text) and unicodedata.bidirectional(g.text[0]) == "NSM"


def _lam_alef(a: Glyph, b: Glyph) -> bool:
    return {a.text, b.text} <= LAM_ALEF and a.text != b.text and (a.text == "ل" or b.text == "ل")


def _same_box(a: Glyph, b: Glyph) -> bool:
    return abs(a.x0 - b.x0) <= LIGATURE_TOL and abs(a.x1 - b.x1) <= LIGATURE_TOL


def _units(glyphs: Sequence[Glyph]) -> list[list[Glyph]]:
    units: list[list[Glyph]] = []
    pending: list[Glyph] = []
    marks: list[Glyph] = []
    for g in glyphs:
        if g.text.isspace():
            units.append([g])
        elif _is_mark(g) and units and not pending and _same_box(units[-1][-1], g):
            units[-1].append(g)
        elif _is_mark(g):
            marks.append(g)
        elif g.x1 - g.x0 <= LIGATURE_TOL:
            pending.append(g)
        elif pending and not _lam_alef(pending[-1], g):
            units.extend([[q] for q in pending])
            units.append([g])
            pending = []
        elif units and not pending and _same_box(units[-1][0], g):
            units[-1].append(g)
        else:
            units.append([*pending, g])
            pending = []
    units.extend([[q] for q in pending])
    bases = [u for u in units if not u[0].text.isspace()]
    for m in marks:
        centre = (m.x0 + m.x1) / 2
        if bases:
            min(bases, key=lambda u: abs(sum(_extent(u)) / 2 - centre)).append(m)
        else:
            units.append([m])
    return units


def _extent(unit: list[Glyph]) -> tuple[float, float]:
    wide = [g for g in unit if g.x1 - g.x0 > LIGATURE_TOL] or unit
    return min(g.x0 for g in wide), max(g.x1 for g in wide)


def _ligature_order(body: list[Glyph]) -> list[Glyph]:
    readings = [body, body[::-1]]
    opening = [r for r in readings if r[0].text in JOINS_FORWARD]
    if len(opening) == 1:
        return opening[0]
    listed = [r for r in opening if r[0].text + r[1].text in LIGATURES]
    return listed[0] if len(listed) == 1 else body[::-1]


def _read_unit(unit: list[Glyph], rtl: bool) -> list[Glyph]:
    body = [g for g in unit if not _is_mark(g)]
    if len(body) > 2:
        return list(unit)
    marks = sorted((g for g in unit if _is_mark(g)), key=lambda g: unicodedata.combining(g.text[0]))
    ordered = _ligature_order(body) if rtl and len(body) == 2 else body
    return ordered + marks


def _separates(space: Glyph, centres: list[float]) -> bool:
    span = space.x1 - space.x0
    if not 0.0 < span <= SPACE_MAX * max(space.font_size, 1e-6):
        return False
    return not any(space.x0 <= c <= space.x1 for c in centres)


def _clusters(glyphs: Sequence[Glyph]) -> list[_Cluster]:
    out: list[_Cluster] = []
    prev: list[Glyph] | None = None
    units = _units(glyphs)
    centres = [sum(_extent(u)) / 2 for u in units if not u[0].text.isspace()]
    zero_space = False
    for unit in sorted(units, key=lambda u: sum(_extent(u)) / 2):
        head = unit[0].text
        if not head or head.isspace():
            if unit[0].x1 == unit[0].x0:
                zero_space = True
            elif _separates(unit[0], centres):
                prev = None
            continue
        size = max(unit[0].font_size, prev[0].font_size if prev else 0.0, 1e-6)
        gap = _extent(unit)[0] - _extent(prev)[1] if prev else 0.0
        if prev is None or gap > WORD_GAP * size or (zero_space and gap > SPACE_MIN * size):
            out.append(_Cluster([], "N"))
        zero_space = False
        out[-1].units.append(unit)
        prev = unit
    for c in out:
        classes = {strong_class(g.text[0]) for u in c.units for g in u if g.text}
        c.kind = "R" if "R" in classes else "L" if "L" in classes else "N"
    return out


def _atoms(clusters: list[_Cluster], embedded: str) -> list[list[_Cluster]]:
    atoms: list[list[_Cluster]] = []
    run: list[_Cluster] = []
    for c in clusters:
        wide = bool(run) and c.x0 - run[-1].x1 > ATOM_GAP * max(run[-1].size, c.size, 1e-6)
        if c.kind in (embedded, "N") and not wide:
            run.append(c)
            continue
        atoms.extend(_peel(run, embedded))
        run = [c] if c.kind in (embedded, "N") else []
        if not run:
            atoms.append([c])
    atoms.extend(_peel(run, embedded))
    return atoms


def _peel(run: list[_Cluster], embedded: str) -> list[list[_Cluster]]:
    if not any(r.kind == embedded for r in run):
        return [[r] for r in run]

    def apart(a: _Cluster, b: _Cluster) -> bool:
        return b.x0 - a.x1 > LABEL_GAP * max(a.size, b.size)

    head, tail = 0, len(run)
    while run[head].kind == "N" and apart(run[head], run[head + 1]):
        head += 1
    while run[tail - 1].kind == "N" and apart(run[tail - 2], run[tail - 1]):
        tail -= 1
    return [[r] for r in run[:head]] + [run[head:tail]] + [[r] for r in run[tail:]]


def _unit_kind(unit: list[Glyph]) -> str:
    classes = {strong_class(g.text[0]) for g in unit if g.text and not _is_mark(g)}
    return "R" if "R" in classes else "L" if "L" in classes else "N"


def _base_glyph(unit: list[Glyph]) -> Glyph:
    return next((g for g in unit if not _is_mark(g) and g.x1 - g.x0 > LIGATURE_TOL), unit[0])


def _emit_number(units: list[list[Glyph]], tally: Tally, bracketed: bool = False) -> list[Glyph]:
    """One number group (UAX #9 W1-W6 run of EN or AN units) read left to right, digits ordered by
    GEOMETRY (x ascending) except for a bracketed verse numeral: when geometry conflicts with the
    extractor sequence (`Glyph.seq`, PDFium text-page index), the run sits against a bracket in its
    row, and the sequence exactly reverses the geometry (OCR RTL-boxed digits such as `[158` laid
    `1` rightmost), the extractor stream order is the authority. Provenance is recorded per
    multi-unit group in the caller's tally: "agree" / "conflict" between geometry and the
    extractor sequence or "no_seq" when a unit lacks one."""
    by_x = sorted(units, key=lambda u: sum(_extent(u)) / 2)
    if len(units) > 1:
        raw = [getattr(_base_glyph(u), "seq", None) for u in by_x]
        seqs: list[int] = [q for q in raw if q is not None]
        if len(seqs) < len(by_x):
            verdict = "no_seq"
        else:
            verdict = "agree" if seqs == sorted(seqs) else "conflict"
        tally[verdict] += 1
        witnesses = getattr(tally, "witnesses", None)  # optional per-group record, owner-attached
        if witnesses is not None:
            geometry = "".join(_base_glyph(u).text for u in by_x)
            sequence = None
            if verdict != "no_seq":
                by_seq = [u for _, u in sorted(zip(seqs, by_x, strict=True), key=lambda k: k[0])]
                sequence = "".join(_base_glyph(u).text for u in by_seq)
            witnesses.append({"geometry": geometry, "sequence": sequence, "verdict": verdict})
        if verdict == "conflict" and bracketed and seqs == sorted(seqs, reverse=True):
            by_seq = [u for _, u in sorted(zip(seqs, by_x, strict=True), key=lambda k: k[0])]
            return [g for u in by_seq for g in _read_unit(u, False)]
    return [g for u in by_x for g in _read_unit(u, False)]


def _emit(units: list[list[Glyph]], rtl: bool, sos: str, tally: Tally) -> list[Glyph]:
    """Units in logical order; letters keep their direction, each number run is one LTR slot. A run
    counts as bracketed when a bracket opens or closes it inside this unit sequence (a verse
    citation such as `[158`), which lets `_emit_number` reconcile OCR RTL-boxed digits."""
    runs = resolve_numeric_runs([(u, classify(_base_glyph(u).text)) for u in units], sos)
    out: list[Glyph] = []
    at = 0
    for run in runs:
        if run.cls in NUMBERS:
            edge = units[at - 1 : at] + units[at + len(run.units) : at + len(run.units) + 1]
            bracketed = any(_base_glyph(u).text in PAIRS for u in edge)
            out.extend(_emit_number(run.units, tally, bracketed))
        else:
            out.extend(g for u in run.units for g in _read_unit(u, rtl))
        at += len(run.units)
    return out


def _read(cluster: _Cluster, tally: Tally | None = None) -> list[Glyph]:
    tally = Counter() if tally is None else tally
    if cluster.kind != "R":
        return _emit(list(cluster.units), False, "L", tally)
    parts = [_Cluster([u], _unit_kind(u)) for u in cluster.units]
    if all(part.kind != "L" for part in parts):
        return _emit(list(reversed(cluster.units)), True, "R", tally)
    out: list[Glyph] = []
    pending: list[list[Glyph]] = []
    for atom in [*reversed(_atoms(parts, "L")), None]:
        if atom is not None and all(part.kind != "L" for part in atom):
            pending.extend(part.units[0] for part in atom)
            continue
        out.extend(_emit(pending, True, "R", tally))
        pending = []
        for part in atom or []:
            out.extend(_read_unit(part.units[0], part.kind == "R"))
    return out


def _balanced(texts: list[str]) -> bool:
    stack: list[str] = []
    for t in texts:
        if t in "([{":
            stack.append(t)
        elif t in ")]}" and (not stack or PAIRS[stack.pop()] != t):
            return False
    return not stack


def _nested(texts: list[str]) -> list[str]:
    out: list[str] = []
    stack: list[str] = []
    for t in texts:
        opener = t if t in "([{" else PAIRS[t]
        if stack and stack[-1] == opener:
            stack.pop()
            out.append(PAIRS[opener])
        else:
            stack.append(opener)
            out.append(opener)
    return out


def _unmirror(glyphs: list[Glyph]) -> list[Glyph]:
    brackets = [g.text for g in glyphs if g.text in PAIRS]
    if not brackets or _balanced(brackets):
        return glyphs
    if _balanced([PAIRS[t] for t in brackets]):
        fixed = [PAIRS[t] for t in brackets]
    elif len(brackets) % 2 == 0:
        fixed = _nested(brackets)
    else:
        return glyphs
    it = iter(fixed)
    return [replace(cast(Any, g), text=next(it)) if g.text in PAIRS else g for g in glyphs]


def _base(clusters: list[_Cluster], text: str, page_rtl: bool = False) -> str:
    r_runs = sum(1 for a in _atoms(clusters, "R") if any(c.kind == "R" for c in a))
    l_runs = sum(1 for a in _atoms(clusters, "L") if any(c.kind == "L" for c in a))
    if r_runs != l_runs:
        return "R" if r_runs > l_runs else "L"
    if page_rtl:
        return "R"
    r = sum(1 for ch in text if unicodedata.bidirectional(ch) in STRONG_R)
    left = sum(1 for ch in text if unicodedata.bidirectional(ch) == "L")
    return "R" if r > left else "L"


def _settle(read: list[list[Glyph]]) -> list[list[Glyph]]:
    flat = [g for c in read for g in c]
    lead = 0
    while lead < len(flat) and flat[lead].text in SENTENCE:
        lead += 1
    trail = len(flat)
    while trail > lead and flat[trail - 1].text in SENTENCE:
        trail -= 1
    if lead == 0 and trail == len(flat):
        return read
    head, tail = flat[:lead], flat[trail:]
    head_solid, tail_solid = len(read[0]) > lead, len(read[-1]) > len(tail)
    core = [list(c) for c in read]
    for g in head + tail:
        for c in core:
            if g in c:
                c.remove(g)
                break
    core = [c for c in core if c]
    if not core:
        return read
    if tail_solid:
        core[0][:0] = tail
    elif tail:
        core.insert(0, tail)
    if head_solid:
        core[-1].extend(head)
    elif head:
        core.append(head)
    return core


def logical_clusters(
    glyphs: Sequence[G], page_rtl: bool = False, authority: Tally | None = None
) -> list[list[G]] | None:
    """`authority` (caller-owned) counts, per multi-unit number group, whether geometry and the
    extractor sequence agree ("agree" / "conflict" / "no_seq"); ordering is by geometry except for
    a bracketed verse numeral, which a conflicting monotonic sequence outranks."""
    text = "".join(g.text for g in glyphs)
    rtl_row = has_rtl(text)
    boxes = Counter((g.x0, g.x1) for g in glyphs if not g.text.isspace())
    blind = sum(n for n in boxes.values() if n > 2)
    if (not rtl_row and not page_rtl) or 2 * blind >= sum(boxes.values()):
        return None
    clusters = _clusters(glyphs)
    base = _base(clusters, text, page_rtl) if rtl_row else "R"
    embedded = "L" if base == "R" else "R"
    atoms = _atoms(clusters, embedded)
    tally = Counter() if authority is None else authority
    out: list[list[Glyph]] = []
    for atom in reversed(atoms) if base == "R" else atoms:
        read = [_read(c, tally) for c in (reversed(atom) if embedded == "R" else atom)]
        out.extend(_settle(read) if any(c.kind == embedded for c in atom) else read)
    flat = _unmirror([g for c in out for g in c])
    sizes = [len(c) for c in out]
    out, at = [], 0
    for n in sizes:
        out.append(flat[at : at + n])
        at += n
    return cast(list[list[G]], out)


def token_class(token: str) -> str:
    """UBA class of a whitespace-delimited token the engine lays out itself: R if it holds a
    strong RTL letter, L if a strong LTR one, else its first number or separator class, else ON."""
    classes = [unicodedata.bidirectional(ch) for ch in token]
    if any(c in STRONG_R for c in classes):
        return "R"
    if "L" in classes:
        return "L"
    return next((c for c in classes if c in NUMBERS or c in ("ES", "ET", "CS")), "ON")


def visual_order(tokens: Sequence[str], rtl: bool) -> list[int]:
    """UAX #9 forward, at token granularity, for a line the reflow engine draws: indices of the
    logically ordered `tokens` in left-to-right visual order. W1-W6 (`resolve_classes`), N1/N2
    with numbers counting as R, I1/I2 levels, L2 reversal from level 2 down to 1. The inverse of
    `logical_clusters`, which recovers logical order from a page's geometry. Explicit embedding
    controls (LRE/RLO/…) are neutrals here."""
    sos = "R" if rtl else "L"
    resolved = resolve_classes([token_class(t) for t in tokens], sos)
    strong = ["R" if c in NUMBERS else c if c in ("L", "R") else "" for c in resolved]
    for i in range(len(resolved)):
        if strong[i]:
            continue
        before = next((s for s in reversed(strong[:i]) if s), sos)
        after = next((s for s in strong[i + 1 :] if s), sos)
        resolved[i] = before if before == after else sos
    levels = [2 if c in NUMBERS else 1 if c == "R" else 2 if rtl else 0 for c in resolved]
    order = list(range(len(tokens)))
    for level in (2, 1):
        i = 0
        while i < len(order):
            if levels[order[i]] < level:
                i += 1
                continue
            j = i
            while j < len(order) and levels[order[j]] >= level:
                j += 1
            order[i:j] = order[i:j][::-1]
            i = j
    return order
