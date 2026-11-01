"""ING-2 static text layer: deterministic chars → lines → blocks → XY-cut → reading order.

No ML, no engine objects. Input is the pdfium adapter's per-char stream (top-left frame,
y down); output is the native adapter's `RawBlock` / `RawWord` shapes so `blocks.py` and
`render.page_geometry` are untouched. LTR only. Spec: docs/notes/ing-2-static-text-layer.md.

Tunable thresholds (all font-size relative unless stated): DIAGRAM_BLOCKS blocks on a page, of which
DIAGRAM_SINGLE in ten are lone single lines, over DIAGRAM_STROKES drawn strokes, starting at
DIAGRAM_SPREAD
distinct left edges, with word labels beside each other on DIAGRAM_BESIDE rows, make a diagram;
DIAGRAM_GLYPHS
one-letter word labels mean a shredded text layer instead. FRAME_LINES lines in a lattice band with
no interior
vertical rule mark a frame. Wrapped lines inside a cell sit within CELL_LEADING font sizes; a row
start
matching the table pitch within PITCH_TOL points is a row. GAP_LINES lines under no row rule inside
a lattice,
or CENTRED_LABELS row labels centred in wrapped cells of a shattered table with UNRESOLVED_LINES
lines or more,
make a region conflict. A rival region sharing CONTAINED of the smaller area is the same structure
seen twice.

ING-3 thresholds (backend-neutral reading order, both adapters' blocks): NARROW_LEAF 0.15 — a text
column narrower than 15 % of the page is narrower than any column measure in the corpus (four-column
pages set columns at 20–22 % of the width; label and number columns at 2–9 %); COLUMN_BAND 0.08 —
the midpoint classifier's own band, kept so the cut fires only where it did; ALIGNED_TOPS 0.6 —
cells of one row share a top, prose paragraphs on a baseline grid coincide at most about a third of
the time (grid pitch 12 pt, 2 pt tolerance); UNEQUAL_COLUMNS 0.6 — text columns are set to equal
measure, a label column beside its cells is under 0.6 of the cell column's width on every corpus
table measured; LABEL_ROWS 3 — two aligned labels are a coincidence, three are a table; STACK_GAP
2.0 — a display line sits within two of its own heights of the paragraph above or below it (display
skip is under one line); HEAD_GAP 1.5 — a column head is followed by its column within 1.5 of its
height, a running header stands at least a blank line clear; FRAGMENT_GLYPHS 6 / FRAGMENT_SIZE 0.85
/ FRAGMENT_MIN 0.3 — sub- and superscripts are set at 0.7 of the text size (0.85 leaves room for
TeX's 0.7–0.8 scriptsize; 0.3 excludes pdfium's degenerate punctuation boxes at 0.06–0.1);
FRAGMENT_GAP 0.4 em — a script or a list marker abuts its text within 0.35 em, a margin note stands
a word space (≥ 0.5 em) away; a hanging list number in the text size, whose median word height
happens to sit under 0.85, is not one); SCRIPT_REACH 0.25 — a script's box never reaches more than a
quarter of the row height beyond the row box (a caption or a label on the line above does);
SCRIPT_OFFSET 0.2 — a sub- or superscript's centre sits at least a fifth of the row height off the
row's centre (TeX shifts scripts by 0.25–0.45 of the size); FRAGMENT_ROW 1.8 — one row with an
operator or a script is at most 1.8 sizes tall, two rows at 12 pt leading are 2.0; RULE_THICKNESS 2
pt (native adapter) — drawn rules in the corpus are 0.4–1.5 pt, cell fills are taller; the 1–3 pt
tolerances on rule and edge tests are the layer's existing snapping tolerance (`_snap`,
`_merge_segments`) applied to the same geometry.

Round-3 arguments for the remaining ING-3 constants. COLUMN_MAX 0.6 — a text column that can contain
a display line is at most 0.6 of the page width: two-column pages set columns at 0.40–0.47 of the
width, a one-column measure with margins at 0.55–0.60 (A4/Letter, 1-inch margins → 0.66 counting
only the text width, but such a page has no counterpart leaf to decide against); a leaf wider than
0.6 contains everything on the page and decides nothing (`_stacked_inside`, `_between`). 2×
containment — a display equation set inside a column is centred on it, so the column is at least
twice as wide as the piece unless the piece is nearly the full measure, when it is a paragraph line,
not a display piece. SHARED_HEIGHT 0.5 and `_blocks_beside` 0.5 — two blocks stand beside each other
when they share at least half the shorter one's height: a two-line lead-in and a one-line display
piece overlap by a line at most (≤ 0.5 of the two-line block) when they are stacked, a full line
(1.0 of the shorter) when they are beside each other; anything under 0.5 is the grazing of adjacent
rows. `_tall` 2× — an upright word's box is wider than tall for any word of two or more glyphs
(glyph advance ≈ 0.5–0.6 em, height 1.0–1.2 em: a two-glyph word is at least as wide as tall); a box
more than twice as tall as wide with two or more glyphs is set vertically. `_cell_dense` FRAME_LINES
× 0.8 × font_size — text rows are set at ≥ 1.0 × size leading (TeX 1.2, Word 1.15, slides ≥ 1.0), so
FRAME_LINES (6) rows occupy at least 6 × 0.8 × size only when the "lines" are cells of a table set
on a tighter pitch than prose, or overlap; 0.8 leaves 20 % for backends that report line boxes
tighter than the leading.
"""

import hashlib
import unicodedata
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field

from txtcaster_engine.ingest import bidi
from txtcaster_engine.ingest.dehyphenate import dehyphenate_block
from txtcaster_engine.ingest.ingestor import Bbox, RawBlock, RawWord

Shape = tuple[str, Bbox]
LATTICE_BUDGET = 4_000_000
OVERPRINT_SHARE = 0.5
OVERPRINT_GLYPHS = 2
_ACCENT_CATEGORIES = frozenset({"Lm", "Sk", "Mn", "Mc", "Me"})
WEAK_CUT = 1.1
MARGIN_LINES = 2
HEAD_GAP = 1.5
PEEL_GAP = 6.0
MARGIN_ZONE = 0.16
SHARED_HEIGHT = 0.5
JUSTIFIED_EM = 8.0
WIDE_ROW = 0.5
RUN_GAP = 2.5
PROSE_WORDS = 6
PROSE_CHARS = 36
PROSE_IDEOGRAPHS = 12
SIZE_RATIO = 2.5
VERTICAL_SIZE_RATIO = 1.3
MIN_ROW_GAP = -2.0
MAX_ROW_CANDIDATES = 200
LAYOUT_SHIFT = 0.05
CROWDED_CUTS = 3
CUT_EPSILON = 0.1

LINE_OVERLAP = 0.3
CHAR_MARGIN = 1.5
SPLIT_JOIN_EM = 3.0
GUTTER_GAP_EM = 0.9
GUTTER_LINES = 5
WORD_MARGIN = 0.15
COLUMN_GAP = 0.012
COLUMN_BLOCKS = 4
GAP_LINES = 2
CONTAINED = 0.75
UNRESOLVED_LINES = 10
CENTRED_LABELS = 2
DIAGRAM_BLOCKS = 8
DIAGRAM_SINGLE = 8
DIAGRAM_STROKES = 6
DIAGRAM_SPREAD = 4
DIAGRAM_BESIDE = 3
DIAGRAM_GLYPHS = 3
FRAME_LINES = 6
CELL_LEADING = 1.25
PITCH_TOL = 1.5
ROW_GAP = 0.02
BLOCK_GAP = 0.5
BLOCK_SIZE_DELTA = 0.15
BLOCK_INDENT = 0.8
NARROW_LEAF = 0.15
STACK_GAP = 2.0
COLUMN_MAX = 0.6
COLUMN_BAND = 0.08
ALIGNED_TOPS = 0.6
LABEL_ROWS = 3
UNEQUAL_COLUMNS = 0.6
FRAGMENT_GLYPHS = 6
FRAGMENT_GAP = 0.4
FRAGMENT_SIZE = 0.85
FRAGMENT_MIN = 0.3
FRAGMENT_ROW = 1.8
SCRIPT_OFFSET = 0.2
SCRIPT_REACH = 0.25
FOLDED = -1
"""Fraction of the anchor glyph's height a char centre may sit outside the band."""


@dataclass
class Char:
    x0: float
    y0: float
    x1: float
    y1: float
    text: str
    font_size: float
    invisible: bool = False  # paints no ink (render mode 3 / 7): an OCR layer's search anchor
    # Extractor sequence: PDFium's text-page char index (fpdf_text.h promises neither logical nor
    # content-stream order); None for synthesised chars and other extractors. Provenance only.
    seq: int | None = None


@dataclass
class Line:
    bbox: Bbox
    text: str
    font_size: float
    chars: list[Char] = field(default_factory=list)
    vertical: bool = False


@dataclass
class Block:
    bbox: Bbox
    lines: list[Line] = field(default_factory=list)

    @property
    def vertical(self) -> bool:
        return bool(self.lines) and self.lines[0].vertical

    @property
    def text(self) -> str:
        return "\n".join(line.text for line in self.lines).strip()

    @property
    def font_size(self) -> float:
        return max((line.font_size for line in self.lines), default=0.0)


def _union(a: Bbox, b: Bbox) -> Bbox:
    return (min(a[0], b[0]), min(a[1], b[1]), max(a[2], b[2]), max(a[3], b[3]))


def _gap_is_space(prev: Char, ch: Char, vertical: bool) -> bool:
    """pdfminer `word_margin`: an along-axis gap wider than WORD_MARGIN em is a word break
    even when the producer emitted no space char (table cells, some Word/LaTeX spacing)."""
    size = max(prev.font_size, ch.font_size, 1e-6)
    gap = ch.y0 - prev.y1 if vertical else ch.x0 - prev.x1
    return gap > WORD_MARGIN * size


def _spaced(chars: list[Char], vertical: bool) -> list[Char]:
    """The line's chars with a synthetic space inserted at every inferred word break."""
    out: list[Char] = []
    for ch in chars:
        glyphs = bool(out) and not out[-1].text.isspace() and not ch.text.isspace()
        if glyphs and _gap_is_space(out[-1], ch, vertical):
            out.append(Char(out[-1].x1, ch.y0, ch.x0, ch.y1, " ", ch.font_size))
        out.append(ch)
    return out


def _line_from(chars: list[Char], vertical: bool = False) -> Line:
    chars = _spaced(chars, vertical)
    box = (chars[0].x0, chars[0].y0, chars[0].x1, chars[0].y1)
    for c in chars[1:]:
        if not c.text.isspace():
            box = _union(box, (c.x0, c.y0, c.x1, c.y1))
    size = max(c.font_size for c in chars)
    text = "".join(c.text for c in chars).strip()
    return Line(box, text, size, chars, vertical)


def _centre(ch: Char) -> tuple[float, float]:
    return ((ch.x0 + ch.x1) / 2, (ch.y0 + ch.y1) / 2)


def _same_line(anchor: Char, prev: Char, ch: Char, vertical: bool) -> bool:
    """Perpendicular centre inside the band, and along-axis gap under CHAR_MARGIN em.

    The band is the taller glyph's extent widened by LINE_OVERLAP and the shorter glyph's
    centre must fall inside it, so a 6pt footnote mark that *opens* a row does not reject
    the 9pt body that follows it any more than a trailing superscript would. The gap is the
    distance between the two glyphs' along-axis intervals, so a run may advance in either
    direction (90° CW or CCW text; a kerned back-step) and still hold.
    """
    size = max(anchor.font_size, ch.font_size, 1e-6)
    ratio = VERTICAL_SIZE_RATIO if vertical else SIZE_RATIO
    if size > ratio * max(min(anchor.font_size, ch.font_size), 1e-6):
        return False
    if vertical:
        ch_h = ch.y1 - ch.y0
        y_overlap = max(0.0, min(prev.y1, ch.y1) - max(prev.y0, ch.y0))
        if y_overlap > (1.0 - LINE_OVERLAP) * min(prev.y1 - prev.y0, ch_h):
            return False
        big, small = sorted((anchor, ch), key=lambda c: c.x1 - c.x0, reverse=True)
        thick = max(big.x1 - big.x0, 1e-6)
        centre = _centre(small)[0]
        in_band = big.x0 - LINE_OVERLAP * thick <= centre <= big.x1 + LINE_OVERLAP * thick
        gap = max(ch.y0 - prev.y1, prev.y0 - ch.y1)
    else:
        big, small = sorted((anchor, ch), key=lambda c: c.y1 - c.y0, reverse=True)
        thick = max(big.y1 - big.y0, 1e-6)
        centre = _centre(small)[1]
        in_band = big.y0 - LINE_OVERLAP * thick <= centre <= big.y1 + LINE_OVERLAP * thick
        gap = max(ch.x0 - prev.x1, prev.x0 - ch.x1)
    return in_band and gap <= CHAR_MARGIN * size


def _is_vertical_candidate(first: Char, ch: Char, remaining: Sequence[Char]) -> bool:
    size_ratio = max(first.font_size, ch.font_size) / max(min(first.font_size, ch.font_size), 1e-6)
    if size_ratio > VERTICAL_SIZE_RATIO:
        return False
    (cx, cy) = _centre(ch)
    slack = LINE_OVERLAP * (first.y1 - first.y0)
    off_row = cy > first.y1 - slack or cy < first.y0 + slack
    in_band = first.x0 <= cx <= first.x1
    if not (off_row and in_band):
        return False
    ch_h = ch.y1 - ch.y0
    for nxt in remaining:
        if nxt.text in ("\r", "\n") or nxt.text.isspace() or nxt.text == "":
            continue
        y_overlap = max(0.0, min(ch.y1, nxt.y1) - max(ch.y0, nxt.y0))
        if y_overlap > (1.0 - LINE_OVERLAP) * min(ch_h, nxt.y1 - nxt.y0):
            return False
        break
    return True


def group_lines(chars: list[Char]) -> list[Line]:
    """pdfminer-class baseline clustering; tolerance relative to font size.

    A char joins the open line when its centre lies inside the anchor glyph's band (the
    tallest glyph so far, widened by LINE_OVERLAP) and the along-axis gap from the previous
    glyph is under CHAR_MARGIN em. That keeps 4pt-shifted super/subscripts on the parent
    line, opens a new line one row down, and splits table cells / column neighbours that
    share a baseline. The second glyph fixes the axis: a run stacking downward at one x is
    a rotated (vertical) line. PDFium's CR/LF chars are ignored: it emits them after every
    baseline shift, not only at row ends. Stream order within a line.
    """
    lines: list[Line] = []
    run: list[Char] = []
    glyphs: list[Char] = []
    anchor: Char | None = None
    vertical = False
    for i, ch in enumerate(chars):
        if ch.text in ("\r", "\n"):
            continue
        if ch.text.isspace() or ch.text == "":
            if run:
                run.append(ch)
            continue
        if glyphs and anchor is not None:
            if len(glyphs) == 1:
                first = glyphs[0]
                vertical = _is_vertical_candidate(first, ch, chars[i + 1 :])
                anchor = first
            if not _same_line(anchor, glyphs[-1], ch, vertical):
                lines.append(_line_from(run, vertical))
                run, glyphs, anchor, vertical = [], [], None, False
        run.append(ch)
        glyphs.append(ch)
        size = (ch.x1 - ch.x0) if vertical else (ch.y1 - ch.y0)
        if anchor is None or size > (
            (anchor.x1 - anchor.x0) if vertical else (anchor.y1 - anchor.y0)
        ):
            anchor = ch
    if run:
        lines.append(_line_from(run, vertical))
    return [line for line in lines if line.text]


def _closes_block(prev: Line, cur: Line) -> bool:
    if prev.vertical != cur.vertical:
        return True
    a, b = prev.bbox, cur.bbox
    if cur.vertical:
        a, b = (a[1], a[0], a[3], a[2]), (b[1], b[0], b[3], b[2])
    height = max(b[3] - b[1], 1e-6)
    overlap = min(a[2], b[2]) > max(a[0], b[0])
    gap = b[1] - a[3] if not cur.vertical else a[1] - b[3]
    if not overlap or gap > BLOCK_GAP * height or gap < -height:
        return True
    size = max(prev.font_size, cur.font_size, 1e-6)
    if abs(prev.font_size - cur.font_size) / size > BLOCK_SIZE_DELTA:
        return True
    return b[0] - a[0] > BLOCK_INDENT * size


def _lone_number(line: Line) -> bool:
    text = line.text.strip()
    return (
        0 < len(text) <= 14
        and all(ch.isdigit() or ch in ".,–-%$()" or ch.isspace() for ch in text)
        and any(ch.isdigit() for ch in text)
    )


def join_lone_numbers(lines: list[Line], shapes: Sequence[Shape] = ()) -> list[Line]:
    """A lone number that shares its baseline with exactly one other line is that line's tail (a
    numeric column read with its label) — unless a vertical rule stands between them: then they
    are two cells. A number is consumed once: when two lines each see it as their single mate
    (stacked equation lines 2 pt apart, both within tolerance of the shared equation label, ING-12
    F1) it joins the nearer line only, so no glyph is emitted twice."""
    walls = [b for kind, b in shapes if kind == "vline"]
    merged: dict[int, Line] = {}
    used: set[int] = set()
    claims: dict[int, list[Line]] = {}
    for line in lines:
        if line.vertical or _lone_number(line):
            continue
        mates = [
            other
            for other in lines
            if other is not line
            and not other.vertical
            and abs(other.bbox[1] - line.bbox[1]) <= 1.5
            and abs(other.bbox[3] - line.bbox[3]) <= 1.5
        ]
        if len(mates) == 1 and any(
            line.bbox[2] <= w[0] <= mates[0].bbox[0] and w[1] < line.bbox[3] and w[3] > line.bbox[1]
            for w in walls
        ):
            continue
        if len(mates) == 1 and mates[0].bbox[0] >= line.bbox[2] and _lone_number(mates[0]):
            claims.setdefault(id(mates[0]), []).append(line)
    numbers = {id(ln): ln for ln in lines}
    for number_id, claimants in claims.items():
        number = numbers[number_id]
        line = min(claimants, key=lambda ln: number.bbox[0] - ln.bbox[2])
        used.add(number_id)
        merged[id(line)] = _line_from(line.chars + number.chars)
    return [merged.get(id(ln), ln) for ln in lines if id(ln) not in used]


def group_blocks(lines: list[Line]) -> list[Block]:
    """Vertical gap, indentation, or font-size change closes a block.

    Lines are consumed in stream order (which is baseline order after `group_lines`); a line
    joins the open block when it horizontally overlaps the previous line, sits within
    BLOCK_GAP line-heights below it, keeps the font size within BLOCK_SIZE_DELTA, and is not
    indented by more than BLOCK_INDENT em relative to it. Anything else opens a new block.
    """
    blocks: list[Block] = []
    open_lines: list[Line] = []
    for line in lines:
        if open_lines and _closes_block(open_lines[-1], line):
            blocks.append(_block_from(open_lines))
            open_lines = []
        open_lines.append(line)
    if open_lines:
        blocks.append(_block_from(open_lines))
    return blocks


def _block_from(lines: list[Line]) -> Block:
    box = lines[0].bbox
    for line in lines[1:]:
        box = _union(box, line.bbox)
    return Block(box, list(lines))


def _widest_gap(blocks: list[Block], axis: int, minimum: float) -> float | None:
    """Coordinate of the widest whitespace gap ≥ `minimum` along `axis` (0 = x, 1 = y)."""
    spans = sorted((b.bbox[axis], b.bbox[axis + 2]) for b in blocks)
    best: tuple[float, float] | None = None
    reach = spans[0][1]
    for lo, hi in spans[1:]:
        if lo - reach >= minimum and (best is None or lo - reach > best[0]):
            best = (lo - reach, (reach + lo) / 2)
        reach = max(reach, hi)
    return None if best is None else best[1]


def _split(blocks: list[Block], axis: int, at: float) -> tuple[list[Block], list[Block]]:
    low = [b for b in blocks if (b.bbox[axis] + b.bbox[axis + 2]) / 2 <= at]
    return low, [b for b in blocks if (b.bbox[axis] + b.bbox[axis + 2]) / 2 > at]


def xy_cut(blocks: list[Block], page_box: Bbox) -> list[list[Block]]:
    """Recursive XY-cut; one leaf per column/region, leaves in reading order.

    Column (x) cuts are preferred and taken whenever a whitespace gutter of at least
    COLUMN_GAP × page width runs the region's full height. A row (y) cut of at least
    ROW_GAP × page height is taken only when it exposes a column cut in one of its halves,
    so a heading over a single-column body stays one leaf. On a rotated page (most blocks
    vertical) columns are read right→left, which is the first line of 90°-CW text.
    """
    if not blocks:
        return []
    width = page_box[2] - page_box[0]
    rotated = sum(b.vertical for b in blocks) * 2 > len(blocks)
    ccw = rotated and _ccw([line for b in blocks for line in b.lines])
    right_first = (not ccw) if rotated else _page_rtl(blocks)

    def cut(region: list[Block]) -> list[list[Block]]:
        if len(region) < 2:
            return [region]
        at = _column_cut(region, width)
        if at is not None:
            left, right = _split(region, 0, at)
            cap = _cap_block(left, right, width)
            if cap is not None:
                return [[cap]] + cut([b for b in region if b is not cap])
            return cut(right) + cut(left) if right_first else cut(left) + cut(right)
        at = _layout_change(region, width)
        if at is not None:
            top, bottom = _split(region, 1, at)
            return cut(top) + cut(bottom)
        return [region]

    head: list[list[Block]] = []
    tail: list[list[Block]] = []
    rest = list(blocks)
    while len(rest) > 1:
        at = _peel_margin(rest, page_box)
        if at is None:
            break
        top, bottom = _split(rest, 1, at)
        if _peel_side(rest, page_box) == "top":
            head.append(top)
            rest = bottom
        else:
            tail.insert(0, bottom)
            rest = top
    return head + cut(rest) + tail


def _share_height(a: list[Block], b: list[Block]) -> bool:
    if not a or not b:
        return False
    top = max(min(x.bbox[1] for x in a), min(x.bbox[1] for x in b))
    bottom = min(max(x.bbox[3] for x in a), max(x.bbox[3] for x in b))
    shortest = min(max(x.bbox[3] for x in g) - min(x.bbox[1] for x in g) for g in (a, b))
    return bottom - top >= SHARED_HEIGHT * max(shortest, 1e-6)


def _columnar(side: list[Block]) -> bool:
    """A wrapped block or a region is a column; so is a stack of COLUMN_BLOCKS single lines that
    share a
    left edge (a list). Scattered single lines (plot ticks, legend entries, diagram labels) are
    not."""
    if any(len(_flatten([b])) > 1 or len(b.lines) > 1 for b in side):
        return True
    lefts = sorted(b.bbox[0] for b in side)
    run = best = 1
    for a, b in zip(lefts, lefts[1:], strict=False):
        run = run + 1 if b - a <= 3.0 else 1
        best = max(best, run)
    return best >= COLUMN_BLOCKS


def _column_cut(region: list[Block], width: float) -> float | None:
    if len(region) < 2:
        return None
    at = _widest_gap(region, 0, COLUMN_GAP * width)
    if at is None:
        return None
    left, right = _split(region, 0, at)
    return at if _share_height(left, right) and (_columnar(left) or _columnar(right)) else None


def _has_column_cut(region: list[Block], width: float) -> bool:
    return _column_cut(region, width) is not None


def _column_headings(edge: list[Block], rest: list[Block]) -> bool:
    if len(edge) < 2 or len(rest) < 2:
        return False
    gutter = _gutter_interval(rest, max(b.bbox[2] for b in rest) - min(b.bbox[0] for b in rest))
    if gutter is None:
        return False
    lo, hi = gutter
    left = [b for b in edge if b.bbox[2] <= lo + 3]
    right = [b for b in edge if b.bbox[0] >= hi - 3]
    return bool(left) and bool(right) and len(left) + len(right) == len(edge)


def _peel_side(region: list[Block], page_box: Bbox) -> str | None:
    height = page_box[3] - page_box[1]
    gaps = _row_gaps(region, PEEL_GAP)
    for at, side in ((gaps[0], "top"), (gaps[-1], "bottom")) if gaps else ():
        top, bottom = _split(region, 1, at)
        edge = top if side == "top" else bottom
        if not edge:
            continue
        in_margin = (
            max(b.bbox[3] for b in edge) <= page_box[1] + MARGIN_ZONE * height
            if side == "top"
            else min(b.bbox[1] for b in edge) >= page_box[3] - MARGIN_ZONE * height
        )
        rest = bottom if side == "top" else top
        if (
            in_margin
            and sum(len(b.lines) for b in _flatten(edge)) <= MARGIN_LINES
            and not _column_headings(edge, rest)
            and not _column_head(edge, rest, page_box)
        ):
            return side
    return None


def _column_head(edge: list[Block], rest: list[Block], page_box: Bbox) -> bool:
    """The edge is the head of one column, not a running header: `rest` cuts into columns, every
    edge block lies on one side of that cut, and the nearest block of that side follows within
    HEAD_GAP of the edge's height. A running header stands a clear space above both columns."""
    width = page_box[2] - page_box[0]
    at = _column_cut(rest, width)
    if at is None or not edge:
        return False
    left = all(b.bbox[2] <= at for b in edge)
    right = all(b.bbox[0] >= at for b in edge)
    if not (left or right):
        return False
    side = (
        [b for b in rest if (b.bbox[2] <= at) if left]
        if left
        else [b for b in rest if b.bbox[0] >= at]
    )
    if not side:
        return False
    bottom = max(b.bbox[3] for b in edge)
    top = min(b.bbox[1] for b in edge)
    nearest = min(abs(b.bbox[1] - bottom) for b in side)
    nearest = min(nearest, min(abs(top - b.bbox[3]) for b in side))
    return nearest <= HEAD_GAP * max(bottom - top, 1e-6)


def _peel_margin(region: list[Block], page_box: Bbox) -> float | None:
    gaps = _row_gaps(region, PEEL_GAP)
    side = _peel_side(region, page_box)
    if side is None or not gaps:
        return None
    return gaps[0] if side == "top" else gaps[-1]


def _row_gaps(region: list[Block], minimum: float) -> list[float]:
    spans = sorted((b.bbox[1], b.bbox[3]) for b in region)
    cuts: list[float] = []
    reach = spans[0][1]
    for lo, hi in spans[1:]:
        if lo - reach >= minimum:
            cuts.append((reach + lo) / 2)
        reach = max(reach, hi)
    return cuts


def _clean_split(region: list[Block], at: float) -> bool:
    return not any(b.bbox[0] < at < b.bbox[2] for b in region)


def _cap_block(left: list[Block], right: list[Block], width: float) -> Block | None:
    for side, other in ((left, right), (right, left)):
        top = min(side, key=lambda b: b.bbox[1])
        other_top = min(b.bbox[1] for b in other)
        centre = (top.bbox[0] + top.bbox[2]) / 2
        if top.bbox[3] <= other_top and width / 3 <= centre <= 2 * width / 3:
            return top
    return None


def _gutter_interval(region: list[Block], width: float) -> tuple[float, float] | None:
    spans = sorted((b.bbox[0], b.bbox[2]) for b in region)
    best: tuple[float, float, float] | None = None
    reach = spans[0][1]
    for lo, hi in spans[1:]:
        if lo - reach >= COLUMN_GAP * width and (best is None or lo - reach > best[0]):
            best = (lo - reach, reach, lo)
        reach = max(reach, hi)
    return None if best is None else (best[1], best[2])


def _structure_continues(other: list[Block], gutter: tuple[float, float], width: float) -> bool:
    lo, hi = gutter
    straddlers = sorted(
        (b for b in other if b.bbox[0] < lo - 2 and b.bbox[2] > hi + 2), key=lambda b: b.bbox[1]
    )
    if not straddlers:
        return True
    first = straddlers[0]
    limit = first.bbox[1] + 1.0
    capped = [
        b
        for b in other
        if b.bbox[3] <= limit
        and b is not first
        and b.bbox[0] >= first.bbox[0] - 3
        and b.bbox[2] <= first.bbox[2] + 3
        and first.bbox[1] - b.bbox[3] <= 1.5 * (b.bbox[3] - b.bbox[1])
    ]
    # Blocks set directly above the straddler inside its x-range are its heading, caption or the
    # last row of a panel it closes, not column structure: a short block (≤ MARGIN_LINES lines) on
    # any side (a heading; a table's last row of cells above its caption, typst 2604 p13), and a
    # tall block on ONE side of the gutter only (a source panel's last rows above the result table,
    # `rw4_lshort_math` p7; cells above a table caption, ar_med p29). Tall blocks on BOTH sides are
    # two column paragraphs ending at a page-wide spanner — the columns themselves (round 1,
    # `ch2_hhs_pag2_ch3` p2, `rw1_fda_*`, `rw1_springer_*`) — and count.
    tall = [b for b in capped if len(b.lines) > MARGIN_LINES]
    both_sides = any(b.bbox[2] <= hi for b in tall) and any(b.bbox[0] >= lo for b in tall)
    above = [
        b
        for b in other
        if b.bbox[3] <= limit
        and b is not first
        and (b not in capped or (both_sides and len(b.lines) > MARGIN_LINES))
    ]
    if not above:
        return False
    has_left = any(b.bbox[2] <= hi for b in above)
    has_right = any(b.bbox[0] >= lo for b in above)
    if has_left and has_right:
        return True
    return any(len(_flatten([b])) > 1 or len(b.lines) > 1 for b in above)


def _row_candidates(region: list[Block]) -> list[float]:
    tops = sorted({round(b.bbox[1] - 1.0, 1) for b in region})
    return sorted(set(_row_gaps(region, MIN_ROW_GAP)) | set(tops[1:]))


def _crosses(region: list[Block], at: float, slack: float = 6.0) -> bool:
    return any(b.bbox[1] < at - slack and b.bbox[3] > at + slack for b in region)


def _layout_change(region: list[Block], width: float) -> float | None:
    for at in _row_candidates(region)[:MAX_ROW_CANDIDATES]:
        top, bottom = _split(region, 1, at)
        if not top or not bottom or _crosses(region, at):
            continue
        cut_top = _column_cut(top, width)
        cut_bottom = _column_cut(bottom, width)
        if (cut_top is None) != (cut_bottom is None):
            gutter = _gutter_interval(top if cut_top is not None else bottom, width)
            other = bottom if cut_top is not None else top
            if gutter is None or _structure_continues(other, gutter, width):
                continue
            return at
        if cut_top is None and cut_bottom is None:
            if _band_below(top, bottom, width):
                return at
            continue
        if cut_top is None or cut_bottom is None:
            continue
        if _clean_split(bottom, cut_top) or _clean_split(top, cut_bottom):
            continue
        return at
    return None


def _baseline_pairs(left: list[Block], right: list[Block], tol: float = 2.0) -> int:
    singles = [b for b in left if len(b.lines) == 1]
    return sum(any(abs(a.bbox[1] - b.bbox[1]) <= tol for b in right) for a in singles)


def _band_below(top: list[Block], bottom: list[Block], width: float) -> bool:
    """A two-column band sandwiched between full-width rows (a poster, a factsheet, a report with a
    full-width callout) has no column cut in either half of any single row cut. The cut above the
    band is
    still a layout change when the band itself, grown block by block from the top of `bottom` until
    the
    first block that straddles its gutter, has a column cut that neither the rows above nor below
    continue."""
    ordered = sorted(bottom, key=lambda b: (b.bbox[1], b.bbox[0]))
    band: list[Block] = []
    for i, block in enumerate(ordered):
        band.append(block)
        at = _column_cut(band, width)
        if at is None:
            continue
        rest = ordered[i + 1 :]
        if not rest or min(r.bbox[1] for r in rest) < max(b.bbox[3] for b in band) - 1.0:
            continue
        left, right = _split(band, 0, at)
        if _baseline_pairs(left, right) >= 2:
            continue
        gutter = _gutter_interval(band, width)
        if gutter is None:
            continue
        if _structure_continues(rest, gutter, width) or _structure_continues(top, gutter, width):
            continue
        return True
    return False


def _order_key(block: Block, rotated: bool | str) -> tuple[float, float]:
    """Top-down then left-right for an upright leaf. For a rotated leaf (90°-CW text) the
    first line is the rightmost, so right-to-left then top-down. y is quantised to a point so
    blocks that share a row up to sub-point jitter are still read left to right. One key
    per leaf: a lone rotated stamp inside an upright leaf is ordered by its y like the rest."""
    x0, y0, x1, y1 = block.bbox
    if rotated == "ccw":
        return (round(x0), -y1)
    if rotated:
        return (-round(x1), y0)
    return (round(y0), x0)


def _page_rtl(blocks: list[Block]) -> bool:
    return bidi.base_direction("".join(b.text for b in blocks)) == "R"


def reading_order(leaves: list[list[Block]]) -> list[Block]:
    """Top-down, left-right within each leaf; leaves concatenated (LTR)."""
    ordered: list[Block] = []
    rtl = _page_rtl([b for leaf in leaves for b in leaf])
    for leaf in leaves:
        vertical = sum(b.vertical for b in leaf) * 2 > len(leaf)
        if not vertical:
            ordered.extend(_upright_rows(leaf, rtl))
            continue
        rotated: bool | str = "ccw" if _ccw([ln for b in leaf for ln in b.lines]) else True
        ordered.extend(sorted(leaf, key=lambda b: _order_key(_after_collisions(b, leaf), rotated)))
    return ordered


def _upright_rows(leaf: list[Block], rtl: bool = False) -> list[Block]:
    placed = sorted(leaf, key=lambda b: _order_key(_after_collisions(b, leaf), False))
    rows: list[tuple[Bbox, list[Block]]] = []
    for block in placed:
        box = _after_collisions(block, leaf).bbox
        home = next(
            (
                members
                for seed, members in rows
                if _overlap_ratio(seed, box) >= 0.5
                and (box[3] - box[1]) <= 2.5 * (seed[3] - seed[1])
            ),
            None,
        )
        if home is not None:
            home.append(block)
        else:
            rows.append((box, [block]))
    out: list[Block] = []
    for _, members in rows:
        out.extend(sorted(members, key=lambda b: -b.bbox[2] if rtl else b.bbox[0]))
    return out


def _tall(word: RawWord) -> bool:
    return len(word[4]) > 1 and (word[3] - word[1]) > 2.0 * (word[2] - word[0])


def blocks_from_shapes(raw: Sequence[RawBlock], words: Sequence[RawWord]) -> list[Block]:
    """Layer blocks rebuilt from a backend's own blocks and words, in the backend's order.

    A word belongs to the raw block whose box holds its centre; one Line per (block, line_no) run
    of words, vertical when the run's box stands taller than twice its width. A block without words
    keeps one Line per text row so the cut still sees how many rows it wraps."""
    lines: list[dict[int, list[RawWord]]] = [{} for _ in raw]
    for w in words:
        k = _block_of(raw, w)
        if k is not None:
            lines[k].setdefault(w[6], []).append(w)
    out: list[Block] = []
    for k, (bbox, text, size) in enumerate(raw):
        rows: list[Line] = []
        for _, ws in sorted(lines[k].items()):
            box = (
                min(w[0] for w in ws),
                min(w[1] for w in ws),
                max(w[2] for w in ws),
                max(w[3] for w in ws),
            )
            tall = (box[3] - box[1]) > 2.0 * (box[2] - box[0])
            vertical = tall and (len(ws) > 1 or _tall(ws[0]))
            rows.append(Line(box, " ".join(w[4] for w in ws), size, [], vertical))
        if not rows:
            rows = [Line(bbox, row, size) for row in text.split("\n") if row.strip()] or [
                Line(bbox, text, size)
            ]
        out.append(Block(bbox, rows))
    return out


def _span(leaf: list[Block]) -> float:
    return max(b.bbox[2] for b in leaf) - min(b.bbox[0] for b in leaf)


def _aligned_tops(a: list[Block], b: list[Block], tol: float = 2.0) -> float:
    return sum(any(abs(p.bbox[1] - q.bbox[1]) <= tol for q in b) for p in a) / len(a)


def _table_like(a: list[Block], b: list[Block], width: float) -> bool:
    """Side-by-side leaves that are table columns, not text columns: one is narrower than
    NARROW_LEAF of the page (a label or number column), one holds nothing but lone numbers
    (`_figures`: equation numbers, a figure column), one is a run of row labels beside the
    other's single wrapped block (`_label_rows`), or the tops of most blocks on each side align
    with a block top on the other (cells of the same rows) and the leaves are single-line cells or
    of unequal width (under UNEQUAL_COLUMNS of each other: a label column beside its cells).
    Text columns are set to equal measure, so wrapped paragraphs aligned on a baseline grid across
    equal columns do not count."""
    if min(_span(a), _span(b)) < NARROW_LEAF * width:
        return True
    if _figures(a) or _figures(b) or _label_rows(a, b) or _label_rows(b, a):
        return True
    aligned = (
        len(a) >= 2
        and len(b) >= 2
        and _aligned_tops(a, b) >= ALIGNED_TOPS
        and _aligned_tops(b, a) >= ALIGNED_TOPS
    )
    if not aligned:
        return False
    if all(len(x.lines) == 1 for x in a + b):
        return True
    return min(_span(a), _span(b)) < UNEQUAL_COLUMNS * max(_span(a), _span(b))


def _figures(leaf: list[Block]) -> bool:
    """Every line of the leaf is a lone number (`_lone_number`): equation numbers down a margin or a
    column of figures, never a text column."""
    lines = [ln for b in leaf for ln in b.lines]
    return bool(lines) and all(_lone_number(ln) for ln in lines)


def _label_rows(labels: list[Block], cells: list[Block]) -> bool:
    """LABEL_ROWS or more single-line blocks of one leaf lie within the other leaf's single wrapped
    block and start where its lines do: row labels next to a column of cells (a table set as one
    block per column by the backend)."""
    if len(cells) != 1 or len(cells[0].lines) < LABEL_ROWS:
        return False
    top, bottom = cells[0].bbox[1] - 2.0, cells[0].bbox[3] + 2.0
    inside = [b for b in labels if len(b.lines) == 1 and top <= b.bbox[1] <= bottom]
    if len(inside) < LABEL_ROWS:
        return False
    tops = [ln.bbox[1] for ln in cells[0].lines]
    hits = sum(any(abs(b.bbox[1] - t) <= 2.0 for t in tops) for b in inside)
    return hits >= ALIGNED_TOPS * len(inside)


def _x_gap(a: list[Block], b: list[Block]) -> float:
    lo = max(min(x.bbox[0] for x in a), min(x.bbox[0] for x in b))
    hi = min(max(x.bbox[2] for x in a), max(x.bbox[2] for x in b))
    return lo - hi


def _y_gap(a: list[Block], b: list[Block]) -> float:
    lo = max(min(x.bbox[1] for x in a), min(x.bbox[1] for x in b))
    hi = min(max(x.bbox[3] for x in a), max(x.bbox[3] for x in b))
    return max(0.0, lo - hi)


def _stacked_inside(
    a: list[Block], b: list[Block], leaves: Sequence[list[Block]], width: float
) -> bool:
    """A narrow leaf whose x-extent lies inside the other leaf's, within STACK_GAP of its own height
    above or below it: a display line (an equation with its number) set between the paragraphs of
    the column that contains it. It belongs to that column, not to whatever stands beside it. The
    containing leaf must be a text column: narrower than COLUMN_MAX of the page, holding wrapped
    text, at least twice the inner leaf's width, and not overlapping in x a leaf that stands beside
    the narrow one and extending beyond the container (a full-width title above two columns
    contains everything and decides nothing; an equation number inside the same column is not a
    counterpart); and the piece must have column matter above and below it (`_between`): two
    stacked label leaves, or a header label over its cell column, are table matter and take the
    shared-height rules."""
    for inner, outer in ((a, b), (b, a)):
        x0, x1 = min(x.bbox[0] for x in inner), max(x.bbox[2] for x in inner)
        ox0, ox1 = min(x.bbox[0] for x in outer), max(x.bbox[2] for x in outer)
        if ox1 - ox0 > COLUMN_MAX * width or not (x0 >= ox0 - 3 and x1 <= ox1 + 3):
            continue
        if x1 - x0 >= NARROW_LEAF * width or not _wrapped(outer) or ox1 - ox0 < 2 * (x1 - x0):
            continue
        height = max(x.bbox[3] for x in inner) - min(x.bbox[1] for x in inner)
        if _y_gap(inner, outer) > STACK_GAP * max(height, 1e-6):
            continue
        beside = [
            leaf
            for leaf in leaves
            if leaf is not inner and leaf is not outer and _share_height(inner, leaf)
        ]
        if any(
            (min(x.bbox[0] for x in lf) < ox1 and max(x.bbox[2] for x in lf) > ox0)
            and not (
                min(x.bbox[0] for x in lf) >= ox0 - 3 and max(x.bbox[2] for x in lf) <= ox1 + 3
            )
            for lf in beside
        ):
            continue
        if not _between(inner, leaves, width):
            continue
        return True
    return False


def _between(inner: list[Block], leaves: Sequence[list[Block]], width: float) -> bool:
    """Column matter stands both above and below the narrow leaf, within STACK_GAP of its height,
    each block containing it in x and narrower than COLUMN_MAX of the page: a display line set
    between a column's paragraphs. A header label has cells below it only."""
    x0, x1 = min(x.bbox[0] for x in inner), max(x.bbox[2] for x in inner)
    top, bottom = min(x.bbox[1] for x in inner), max(x.bbox[3] for x in inner)
    reach = STACK_GAP * max(bottom - top, 1e-6)
    above = below = False
    for leaf in leaves:
        if leaf is inner:
            continue
        for b in leaf:
            wide = b.bbox[2] - b.bbox[0] > COLUMN_MAX * width
            if wide or not (x0 >= b.bbox[0] - 3 and x1 <= b.bbox[2] + 3):
                continue
            above = above or (b.bbox[1] < top and top - b.bbox[3] <= reach)
            below = below or (b.bbox[3] > bottom and b.bbox[1] - bottom <= reach)
    return above and below


def _merge_tables(leaves: list[list[Block]], width: float) -> list[list[Block]]:
    """Table-like leaves that stand beside another leaf, or stacked inside one (`_stacked_inside`),
    join a neighbour whose x-extent overlaps theirs first and the vertically nearest of those (a
    display equation's pieces and its number stay in the column that contains them, never the
    column across the gutter), then the one whose block tops they align with most, then the
    nearest, until none is left."""
    out = [list(leaf) for leaf in leaves]
    while True:
        pairs = [
            (_x_gap(a, b) > 0, _y_gap(a, b), -_aligned_tops(a, b) * len(a), _x_gap(a, b), i, j)
            for i, a in enumerate(out)
            for j, b in enumerate(out)
            if i < j
            and not any(x.vertical for x in a + b)
            and not (_sidebar(a, width, out) or _sidebar(b, width, out))
            and (_share_height(a, b) or _stacked_inside(a, b, out, width))
            and _table_like(a, b, width)
        ]
        if not pairs:
            return out
        _, _, _, _, i, j = min(pairs)
        if len(out[j]) > len(out[i]):
            i, j = j, i
        out[i].extend(out.pop(j))


def column_order(
    raw: Sequence[RawBlock],
    words: Sequence[RawWord],
    page_box: Bbox,
    shapes: Callable[[], Sequence[Shape]] = lambda: (),
) -> list[int] | None:
    """Reading order of a backend's blocks through the XY-cut (indices into `raw`), or None when
    the page is rotated, has no matter on both sides of its middle band, has its gutter bent by a
    wide table (the layer's `ambiguous_layout`), the cut finds no side-by-side leaves (blocks
    sharing a band, `_blocks_beside`) of which one holds wrapped text, a drawn rule runs across a
    pair of them (`shapes`, read only then:
    cells of one ruled row are not columns), or the page is a diagram by the layer's own rule
    (`_diagram`: scattered labels beside each other over strokes); in those cases the backend's
    own order stands.
    Side-by-side leaves that form a table are read row-major as one leaf."""
    blocks = blocks_from_shapes(raw, words)
    width = page_box[2] - page_box[0]
    if len(blocks) < 2:
        return None
    if sum(b.vertical for b in blocks) * 2 > len(blocks) or _bent_gutter(blocks, width, False):
        return None
    drawn = list(shapes())
    # C26 (S5-DESIGN §2.4): a table unit is one leaf on this path too; a page holding one always
    # gets an order here, so blocks.py's column fallback never re-sorts it by page direction
    units = _native_table_units(raw, words, drawn)
    members = {i for _, idx in units for i in idx}
    expansion = {id(b): idx for b, idx in units}
    index = {id(b): [i] for i, b in enumerate(blocks) if i not in members}
    blocks = [b for i, b in enumerate(blocks) if i not in members] + [b for b, _ in units]
    if not units and not _two_sided(blocks, page_box):
        return None
    leaves = _merge_tables(xy_cut(blocks, page_box), width)
    if not units:
        pairs = [
            (a, b) for i, a in enumerate(leaves) for b in leaves[i + 1 :] if _blocks_beside(a, b)
        ]
        if not pairs or not all(_wrapped(a) or _wrapped(b) for a, b in pairs):
            return None
        if all(_sidebar(a, width, leaves) or _sidebar(b, width, leaves) for a, b in pairs):
            return None
        if any(_ruled_across(a, b, drawn) for a, b in pairs) or _diagram(blocks, drawn):
            return None
    return [i for b in reading_order(leaves) for i in expansion.get(id(b), index.get(id(b), []))]


def _line_of_words(ws: Sequence[RawWord], size: float) -> Line:
    return Line(
        (
            min(w[0] for w in ws),
            min(w[1] for w in ws),
            max(w[2] for w in ws),
            max(w[3] for w in ws),
        ),
        " ".join(w[4] for w in ws),
        size,
    )


def _native_table_units(
    raw: Sequence[RawBlock], words: Sequence[RawWord], shapes: Sequence[Shape]
) -> list[tuple[Block, list[int]]]:
    """Table units over a backend's own blocks (native path): the backend's lines are re-cut at
    word gaps wider than one em (pymupdf hands a whole header row, or a label glued to its cell,
    as one line), the shared `_table_regions` runs over those lines, and every unit becomes one
    Block whose members are the raw block indices in `_table_cells` order. A raw block only partly
    inside a unit voids that unit (the cell boundary is not the block boundary; stay honest)."""
    # a word's block is the raw block whose box holds its centre (as `blocks_from_shapes`): the
    # backend's own block numbers are not indices into `raw`
    by_line: dict[tuple[int, int], list[RawWord]] = {}
    for w in words:
        k = _block_of(raw, w)
        if k is not None:
            by_line.setdefault((k, int(w[6])), []).append(w)
    fine: list[Line] = []
    owner: dict[int, int] = {}
    for (k, _), ws in sorted(by_line.items()):
        size = float(raw[k][2])
        run: list[RawWord] = []
        for w in sorted(ws, key=lambda w: w[0]):
            if run and w[0] - max(r[2] for r in run) > max(size, 1.0):
                fine.append(_line_of_words(run, size))
                owner[id(fine[-1])] = k
                run = []
            run.append(w)
        if run:
            fine.append(_line_of_words(run, size))
            owner[id(fine[-1])] = k
    units: list[tuple[Block, list[int]]] = []
    used: set[int] = set()
    for region in _table_regions(fine, shapes):
        if not isinstance(region, TableRegion):
            continue
        inside = [ln for ln in fine if _inside(Block(ln.bbox, [ln]), region)]
        inside_ids = {id(ln) for ln in inside}
        members = {owner[id(ln)] for ln in inside}
        outside = {owner[id(ln)] for ln in fine if id(ln) not in inside_ids}
        if members & outside or members & used:
            continue
        cells = _table_cells(inside, region)
        order: list[int] = []
        for cell in cells:
            for ln in cell.lines:
                if owner[id(ln)] not in order:
                    order.append(owner[id(ln)])
        units.append((Block(region.bbox, [ln for c in cells for ln in c.lines]), order))
        used |= members
    return units


def _fine_lines(
    raw: Sequence[RawBlock], words: Sequence[RawWord]
) -> tuple[list[Line], dict[int, int], dict[int, list[RawWord]]]:
    """A backend's lines re-cut at word gaps wider than one em (pymupdf hands a header row, or a
    label glued to its cell, as one line): the fine lines, each one's owning raw block (by
    geometry, `_block_of`) and its words."""
    by_line: dict[tuple[int, int], list[RawWord]] = {}
    for w in words:
        k = _block_of(raw, w)
        if k is not None:
            by_line.setdefault((k, int(w[6])), []).append(w)
    fine: list[Line] = []
    owner: dict[int, int] = {}
    runs: dict[int, list[RawWord]] = {}
    for (k, _), ws in sorted(by_line.items()):
        size = float(raw[k][2])
        run: list[RawWord] = []
        for w in sorted(ws, key=lambda w: w[0]):
            if run and w[0] - max(r[2] for r in run) > max(size, 1.0):
                fine.append(_line_of_words(run, size))
                owner[id(fine[-1])], runs[id(fine[-1])] = k, run
                run = []
            run.append(w)
        if run:
            fine.append(_line_of_words(run, size))
            owner[id(fine[-1])], runs[id(fine[-1])] = k, run
    return fine, owner, runs


def table_units_native(
    raw: Sequence[RawBlock], words: Sequence[RawWord], shapes: Sequence[Shape] = ()
) -> tuple[list[RawBlock], list[RawWord]] | None:
    """C26 (S5-DESIGN §2.3, table-local): for a backend whose blocks may carry several table cells
    in one block, the page's raw blocks and words with every table unit's member blocks replaced,
    in place, by one RawBlock per cell — text rebuilt from the words in the backend's own logical
    order, line by line, in `_table_cells` order; every other block and word byte-identical.
    None when the page holds no unit, so a prose page is never touched."""
    fine, owner, runs = _fine_lines(raw, words)
    pos = {id(w): i for i, w in enumerate(words)}
    units: list[tuple[set[int], list[tuple[Bbox, str, float, list[list[RawWord]]]]]] = []
    used: set[int] = set()
    for region in _table_regions(fine, shapes):
        if not isinstance(region, TableRegion):
            continue
        inside = [ln for ln in fine if _inside(Block(ln.bbox, [ln]), region)]
        ids = {id(ln) for ln in inside}
        members = {owner[id(ln)] for ln in inside}
        outside = {owner[id(ln)] for ln in fine if id(ln) not in ids}
        if members & outside or members & used:
            continue
        cells: list[tuple[Bbox, str, float, list[list[RawWord]]]] = []
        for cell in _table_cells(inside, region):
            rows = [
                sorted(runs[id(ln)], key=lambda w: pos[id(w)])
                for ln in sorted(cell.lines, key=lambda ln: ln.bbox[1])
            ]
            text = "\n".join(" ".join(w[4] for w in ws) for ws in rows).strip()
            if text:
                cells.append((cell.bbox, text, float(raw[owner[id(cell.lines[0])]][2]), rows))
        if cells:
            units.append((members, cells))
            used |= members
    if not units:
        return None
    first = {min(m): cells for m, cells in units}
    home = {id(w): _block_of(raw, w) for w in words}
    new_raw: list[RawBlock] = []
    new_words: list[RawWord] = []
    for i, blk in enumerate(raw):
        for box, text, size, rows in first.get(i, []):
            no = len(new_raw)
            new_raw.append((box, text, size))
            for row_no, ws in enumerate(rows):
                new_words.extend((w[0], w[1], w[2], w[3], w[4], no, row_no) for w in ws)
        if i in used:
            continue
        no = len(new_raw)
        new_raw.append(blk)
        new_words.extend(
            (w[0], w[1], w[2], w[3], w[4], no, int(w[6])) for w in words if home[id(w)] == i
        )
    new_words.extend(w for w in words if home[id(w)] is None)
    return new_raw, new_words


def _ruled_across(a: list[Block], b: list[Block], shapes: Sequence[Shape]) -> bool:
    """A horizontal rule that spans the gap between two side-by-side leaves inside their shared
    band, or a vertical rule standing in that gap for most of the band, makes them cells of one
    table, whatever their text looks like. Rules drawn per cell are merged into one segment first
    (`_merge_segments`, as the lattice rule does)."""
    left, right = (a, b) if min(x.bbox[0] for x in a) <= min(x.bbox[0] for x in b) else (b, a)
    gap_lo = max(x.bbox[2] for x in left)
    gap_hi = min(x.bbox[0] for x in right)
    top = max(min(x.bbox[1] for x in left), min(x.bbox[1] for x in right))
    bottom = min(max(x.bbox[3] for x in left), max(x.bbox[3] for x in right))
    if bottom <= top:
        return False
    hs = _merge_segments([b for kind, b in shapes if kind == "hline"], axis=0)
    vs = _merge_segments([b for kind, b in shapes if kind == "vline"], axis=1)
    for x0, y0, x1, _y1 in hs:
        if top - 3 <= y0 <= bottom + 3 and x0 <= gap_lo + 3 and x1 >= gap_hi - 3:
            return True
    for x0, y0, _x1, y1 in vs:
        if gap_lo - 3 <= x0 <= gap_hi + 3 and y0 <= top + 3 and y1 >= bottom - 3:
            return True
    return False


def _sidebar(leaf: list[Block], width: float, leaves: Sequence[list[Block]] = ()) -> bool:
    """A margin-note column: a leaf narrower than NARROW_LEAF of the page that holds wrapped text
    and stands outboard of every other leaf (nothing to its left, or nothing to its right, within
    3 pt; `_merge_tables` tests it against every leaf). It is never merged as a label column and,
    when it is the only thing standing beside the
    text, the page is not two columns either — its notes are read where the backend set them, at
    their paragraph's height (ECB annual accounts: a margin callout beside the paragraph under a
    chart's source line). A narrow wrapped leaf with matter on both sides (a table's wrapped cell
    column inside a two-column paper) is table matter and keeps the shared-height rules."""
    if not (_span(leaf) < NARROW_LEAF * width and _wrapped(leaf)):
        return False
    x0, x1 = min(b.bbox[0] for b in leaf), max(b.bbox[2] for b in leaf)
    others = [b for other in leaves if other is not leaf for b in other]
    if not others:
        return True
    return all(b.bbox[0] >= x1 - 3 for b in others) or all(b.bbox[2] <= x0 + 3 for b in others)


def _blocks_beside(a: list[Block], b: list[Block]) -> bool:
    """Two leaves stand side by side when some block of one shares at least half the shorter
    block's height with some block of the other. A leaf's union box can span a short line's band
    without any of its blocks doing so (a centred display equation above and below a lead-in
    line); such leaves are stacked, not columns."""
    for x in a:
        for y in b:
            shared = min(x.bbox[3], y.bbox[3]) - max(x.bbox[1], y.bbox[1])
            if shared >= 0.5 * max(1e-6, min(x.bbox[3] - x.bbox[1], y.bbox[3] - y.bbox[1])):
                return True
    return False


def _two_sided(blocks: list[Block], page_box: Bbox) -> bool:
    """At least two blocks end left of the page's middle band and two start right of it: the page
    has matter on both sides, the precondition the midpoint classifier also demanded."""
    mid = (page_box[0] + page_box[2]) / 2
    band = (page_box[2] - page_box[0]) * COLUMN_BAND
    left = sum(1 for b in blocks if b.bbox[2] <= mid + band)
    right = sum(1 for b in blocks if b.bbox[0] >= mid - band)
    return left >= 2 and right >= 2


def _wrapped(leaf: list[Block]) -> bool:
    """A text column holds at least one block of two or more lines; a leaf of single-line blocks
    only (cells, labels, ticks) is not one."""
    return any(len(b.lines) >= 2 for b in leaf)


def _covered(inner: Bbox, outer: Bbox) -> float:
    w = min(inner[2], outer[2]) - max(inner[0], outer[0])
    h = min(inner[3], outer[3]) - max(inner[1], outer[1])
    area = (inner[2] - inner[0]) * (inner[3] - inner[1])
    return max(0.0, w) * max(0.0, h) / area if area > 0 else 0.0


def _after_collisions(block: Block, leaf: list[Block]) -> Block:
    if not isinstance(block, RegionBlock):
        return block
    x0, y0, x1, y1 = block.bbox
    for other in leaf:
        if isinstance(other, RegionBlock) or _covered(other.bbox, block.bbox) < 0.5:
            continue
        if any(_covered(other.bbox, member.bbox) >= 0.5 for member in block.members):
            y0 = max(y0, other.bbox[3])
    return Block((x0, y0, x1, y1), block.lines)


def order_hash(blocks: list[Block]) -> str:
    """Stable digest of ordered block bboxes+text, for determinism pins."""
    digest = hashlib.sha256()
    for block in blocks:
        digest.update(repr(tuple(_round(v) for v in block.bbox)).encode())
        digest.update(block.text.encode("utf-8", "surrogatepass"))
        digest.update(b"\x00")
    return digest.hexdigest()


def _round(value: float) -> float:
    return round(value, 3)


def _words(line: Line, block_no: int, line_no: int) -> list[RawWord]:
    out: list[RawWord] = []
    box: Bbox | None = None
    text: list[str] = []
    for ch in [*line.chars, Char(0, 0, 0, 0, " ", 0.0)]:
        if ch.text.isspace() or ch.text == "":
            if box is not None and text:
                x0, y0, x1, y1 = box
                out.append(
                    (
                        _round(x0),
                        _round(y0),
                        _round(x1),
                        _round(y1),
                        "".join(text),
                        block_no,
                        line_no,
                    )
                )
            box, text = None, []
            continue
        glyph = (ch.x0, ch.y0, ch.x1, ch.y1)
        box = glyph if box is None else _union(box, glyph)
        text.append(ch.text)
    return out


def logical_line(line: Line, page_rtl: bool = False) -> Line:
    clusters = None if line.vertical else bidi.logical_clusters(line.chars, page_rtl)
    if clusters is None:
        return line
    chars: list[Char] = []
    for cluster in clusters:
        if chars:
            prev, nxt = chars[-1], cluster[0]
            chars.append(Char(prev.x1, nxt.y0, nxt.x0, nxt.y1, " ", nxt.font_size))
        chars.extend(cluster)
    text = "".join(c.text for c in chars).strip()
    return Line(line.bbox, text, line.font_size, chars, False)


def to_page_shapes(blocks: list[Block]) -> tuple[list[RawBlock], list[RawWord]]:
    """Emit native-adapter C4/C5 shapes: RawBlock (bbox, text, size), RawWord 7-tuples."""
    raw_blocks: list[RawBlock] = []
    words: list[RawWord] = []
    page_rtl = _page_rtl(blocks)
    for block in _flatten(blocks):
        lines = [logical_line(line, page_rtl) for line in block.lines]
        text = dehyphenate_block("\n".join(line.text for line in lines)).strip()
        if not text:
            continue
        block_no = len(raw_blocks)
        x0, y0, x1, y1 = block.bbox
        raw_blocks.append(((_round(x0), _round(y0), _round(x1), _round(y1)), text, block.font_size))
        for line_no, line in enumerate(lines):
            words.extend(_words(line, block_no, line_no))
    return raw_blocks, words


@dataclass(frozen=True)
class Region:
    x0: float
    y0: float
    x1: float
    y1: float
    kind: str

    @property
    def bbox(self) -> Bbox:
        return (self.x0, self.y0, self.x1, self.y1)


def _raw_regions(lines: list[Line], page_box: Bbox, shapes: Sequence[Shape] = ()) -> list[Region]:
    frames = [Region(*box, kind) for kind, box in shapes if kind in ("figure", "image")]
    lattices = [r for r in _lattice_regions(shapes, lines) if not _framed(r, frames)]
    found = _figure_regions(lines, shapes) + lattices
    found += _alignment_regions(lines, page_box) + _keyvalue_regions(
        lines, page_box[2] - page_box[0]
    )
    found += _table_regions(lines, shapes, found)
    return found


TABLE_HEADER_WORDS = 4
TABLE_CLOSE_EM = 3.0
_CAPTION = None


def _caption_like(line: Line) -> bool:
    """A table caption line: `Table 3`, `TABLE 21.8`, `Tab. 2`, `Tabelle 1`, `Tableau 4`,
    `جدول ٣` …"""
    global _CAPTION
    if _CAPTION is None:
        import re

        _CAPTION = re.compile(
            r"^\s*(Table|TABLE|Tab\.|Tabelle|Tableau|Tabela|Cuadro|Tabla|جدول|表)\s*[\dSA-Z٠-٩]"
        )
    return bool(_CAPTION.match(line.text))


def _label_cell(line: Line) -> bool:
    """A header cell is a label, not a fragment of running text: at most TABLE_HEADER_WORDS words,
    not prose, not starting lowercase, not ending in sentence punctuation."""
    text = line.text.strip()
    if not text or len(text.split()) > TABLE_HEADER_WORDS or _is_prose(line):
        return False
    head = next((ch for ch in text if ch.isalnum()), "")
    return not head.islower() and text[-1] not in ",;:،؛†*"


def _table_regions(
    lines: list[Line], shapes: Sequence[Shape] = (), others: Sequence[Region] = ()
) -> list[Region]:
    """Unruled table with wrapped prose cells (C26 M2; S5-DESIGN §1-2): a header row of two or more
    short cells on one top, every body line beneath sitting under exactly one header cell, at least
    two of those columns wrapped (two or more lines) and starting on one top, and the unit CLOSED —
    by a line wider than the table on both sides (its caption) or by a horizontal rule under the
    last body line. Each item is evidence and all are required; a line spanning columns or a gap of
    TABLE_CLOSE_EM em ends the candidate without a region (prose columns under two headings are
    never closed this way). Table text order is left to `apply_regions`."""
    upright = [ln for ln in lines if not ln.vertical]
    regions: list[Region] = []
    for row in _rows_of(upright):
        cells = sorted(row, key=lambda ln: ln.bbox[0])
        if len(cells) < 2 or not all(_label_cell(ln) for ln in cells):
            continue
        top = max(ln.bbox[3] for ln in cells)
        size = max(ln.font_size for ln in cells)
        x0, x1 = min(ln.bbox[0] for ln in cells), max(ln.bbox[2] for ln in cells)
        taken = {id(ln) for ln in row}
        below = sorted(
            (ln for ln in upright if id(ln) not in taken and ln.bbox[1] >= top - 1.0),
            key=lambda ln: ln.bbox[1],
        )
        columns: dict[int, list[Line]] = {}
        # a column's x-extent is its header cell, widened by every body line assigned to it: a
        # short last line of a wrapped cell sits under the cell, not under the (narrower) header
        extents = {i: (c.bbox[0], c.bbox[2]) for i, c in enumerate(cells)}
        body: list[Line] = []
        closed = False
        for ln in below:
            if _caption_like(ln) and ln.bbox[1] - top <= TABLE_CLOSE_EM * size * max(1, len(body)):
                closed = True  # the table's caption under it closes the unit
                break
            if ln.bbox[0] <= x0 - 10.0 and ln.bbox[2] >= x1 + 10.0:
                break  # running text wider than the unit ends the candidate but is no evidence
            if body and ln.bbox[1] - max(b.bbox[3] for b in body) > TABLE_CLOSE_EM * size:
                break
            centre = (ln.bbox[0] + ln.bbox[2]) / 2
            under = [i for i, (lo, hi) in extents.items() if lo - 3.0 <= centre <= hi + 3.0]
            if len(under) != 1 or ln.bbox[2] - ln.bbox[0] > 1.5 * (x1 - x0) / len(cells):
                break
            lo, hi = extents[under[0]]
            extents[under[0]] = (min(lo, ln.bbox[0]), max(hi, ln.bbox[2]))
            columns.setdefault(under[0], []).append(ln)
            body.append(ln)
        if not body:
            continue
        if not closed:
            bottom = max(ln.bbox[3] for ln in body)
            header_top = min(c.bbox[1] for c in cells)
            closed = any(
                kind == "hline"
                and 0.0 <= b[1] - bottom <= 1.5 * size
                and min(b[2], x1) - max(b[0], x0) >= 0.8 * (x1 - x0)
                for kind, b in shapes
            ) or any(  # or the table's caption right above its header row
                _caption_like(ln) and 0.0 <= header_top - ln.bbox[3] <= TABLE_CLOSE_EM * size
                for ln in upright
            )
        wrapped = [col for col in columns.values() if len(col) >= 2]
        if not closed or len(columns) < 2 or len(wrapped) < 2:
            continue
        # a column holding no letter or digit (a bullet gutter) is a list, not a table column;
        # vertical strokes inside the candidate are a boxed diagram's or a ruled grid's (lattice)
        if any(not any(ch.isalnum() for ln in col for ch in ln.text) for col in columns.values()):
            continue
        low = max(ln.bbox[3] for ln in body)
        if any(
            kind == "vline"
            and b[0] >= x0 - 3.0
            and b[2] <= x1 + 3.0
            and b[3] >= top
            and b[1] <= low
            for kind, b in shapes
        ):
            continue
        # the first body row starts together in every wrapped column — within one line height,
        # the row-label tolerance `_unruled_rows` uses (a label set a few points below its cell)
        firsts = [min(ln.bbox[1] for ln in col) for col in wrapped]
        if max(firsts) - min(firsts) > 0.8 * size:
            continue
        # column boundaries: midpoints between neighbouring column extents; row bands: the header
        # top, then every top shared by two or more columns after a gap wider than the leading
        spans = sorted(extents[i] for i in columns)
        cols = [spans[0][0] - 1.0]
        cols += [(hi + lo) / 2 for (_, hi), (lo, _) in zip(spans, spans[1:], strict=False)]
        cols.append(spans[-1][1] + 1.0)
        starts: list[float] = []
        for col in columns.values():
            col.sort(key=lambda ln: ln.bbox[1])
            for prev, ln in zip([None, *col], col, strict=False):
                if prev is None or ln.bbox[1] - prev.bbox[3] > 0.6 * size:
                    starts.append(ln.bbox[1])
        rows = [min(c.bbox[1] for c in cells)]
        for y in sorted(starts):
            if sum(abs(y - s) <= 2.0 for s in starts) >= 2 and all(abs(y - r) > 2.0 for r in rows):
                rows.append(y)
        # a horizontal rule across the unit between header and last body line is a row boundary too
        # (rows set on rules carry no gap wider than the leading)
        bottom = max(ln.bbox[3] for ln in body)
        for kind, b in shapes:
            y = (b[1] + b[3]) / 2
            if (
                kind == "hline"
                and top - 1.0 <= y <= bottom
                and min(b[2], x1) - max(b[0], x0) >= 0.5 * (x1 - x0)
                and all(abs(y - r) > 2.0 for r in rows)
            ):
                rows.append(y)
        # a band whose columns hold the same number of short lines on pairwise-aligned tops is a
        # grid of single-line rows (a numeric table), not wrapped cells: every aligned top is a row
        bounds = sorted(rows) + [bottom + 1.0]
        for lo, hi in zip(bounds, bounds[1:], strict=False):
            stacks = [
                sorted(
                    (ln for ln in col if lo - 1.0 <= ln.bbox[1] < hi - 1.0),
                    key=lambda ln: ln.bbox[1],
                )
                for col in columns.values()
            ]
            stacks = [s for s in stacks if len(s) >= 2]
            if len(stacks) < 2 or len({len(s) for s in stacks}) != 1:
                continue
            if any(len(ln.text.split()) > TABLE_HEADER_WORDS for s in stacks for ln in s):
                continue
            if all(
                abs(a.bbox[1] - b.bbox[1]) <= 2.0
                for s in stacks[1:]
                for a, b in zip(stacks[0], s, strict=False)
            ):
                rows.extend(
                    ln.bbox[1]
                    for ln in stacks[0][1:]
                    if all(abs(ln.bbox[1] - r) > 2.0 for r in rows)
                )
        # column direction is the header row's own layout (S5-DESIGN §1 `direction_source`):
        # header cells with strong letters read in their script's direction; a header without
        # strong letters leaves the geometric left-to-right order and says so — never a text vote
        header_text = " ".join(c.text for c in cells)
        if any(bidi.strong_class(ch) in ("L", "R") for ch in header_text):
            direction, source = (
                ("rtl" if bidi.base_direction(header_text) == "R" else "ltr"),
                "header",
            )
        else:
            direction, source = "ltr", "geometry"
        box = _region_of(list(row) + body, "table").bbox
        candidate = TableRegion(*box, "table", tuple(cols), tuple(rows), direction, source)
        if any(_overlaps(candidate, o) for o in list(others) + regions):
            continue  # grids the other detectors already hold, or a nested start row, are not units
        regions.append(candidate)
    return regions


@dataclass(frozen=True)
class TableRegion(Region):
    """A `table` region carrying the structure `_table_regions` saw: `cols` = column boundaries
    (x, one more than the columns), `rows` = row-band tops, header first; `direction` = the
    column traversal ("ltr" | "rtl") with its `direction_source` ("header" | "geometry").
    `apply_regions` assembles its cells band by band with `_band_cells`, so a wrapped cell is one
    block whatever its script (the Latin row-start heuristic of `_cell_rows` never sees it), and
    never reverses it by the region's text majority."""

    cols: tuple[float, ...] = ()
    rows: tuple[float, ...] = ()
    direction: str = "ltr"
    direction_source: str = "geometry"


def _table_cells(lines: list[Line], region: TableRegion) -> list[Block]:
    """Header band first, then each body band; inside a band one block per column in the table's
    own column direction, lines top-down. Each cell's text keeps its own bidi (`logical_line`)."""
    tops = sorted(region.rows)
    bands: list[list[Line]] = [[] for _ in tops]
    for ln in lines:
        centre = (ln.bbox[1] + ln.bbox[3]) / 2
        idx = max((i for i, t in enumerate(tops) if centre >= t - 1.0), default=0)
        bands[idx].append(ln)
    cells: list[Block] = []
    for held in bands:
        band_cells = _band_cells(held, list(region.cols))
        cells.extend(reversed(band_cells) if region.direction == "rtl" else band_cells)
    return cells


def _clip(region: Region, rival: Region) -> Region | None | str:
    ox0, ox1 = max(region.x0, rival.x0), min(region.x1, rival.x1)
    oy0, oy1 = max(region.y0, rival.y0), min(region.y1, rival.y1)
    full_w = ox0 <= region.x0 + 1 and ox1 >= region.x1 - 1
    full_h = oy0 <= region.y0 + 1 and oy1 >= region.y1 - 1
    if full_w and full_h:
        return None
    if full_w:
        above, below = oy0 - region.y0, region.y1 - oy1
        y0, y1 = (region.y0, oy0) if above >= below else (oy1, region.y1)
        return Region(region.x0, y0, region.x1, y1, region.kind) if y1 - y0 > 2 else None
    if full_h:
        left, right = ox0 - region.x0, region.x1 - ox1
        x0, x1 = (region.x0, ox0) if left >= right else (ox1, region.x1)
        return Region(x0, region.y0, x1, region.y1, region.kind) if x1 - x0 > 2 else None
    shared = max(0.0, ox1 - ox0) * max(0.0, oy1 - oy0)
    if shared >= CONTAINED * min(_area(region), _area(rival)):
        return None
    return "conflict"


def _area(region: Region) -> float:
    return max(0.0, region.x1 - region.x0) * max(0.0, region.y1 - region.y0)


def _resolve(found: list[Region]) -> tuple[list[Region], bool]:
    kept: list[Region] = []
    conflict = False
    for region in found:
        current: Region | None = region
        for rival in kept:
            if current is None or rival.kind == current.kind or not _overlaps(rival, current):
                continue
            clipped = _clip(current, rival)
            if clipped == "conflict":
                conflict = True
                current = None
            else:
                current = clipped if isinstance(clipped, Region) else None
        if current is not None:
            kept.append(current)
    return _merge_same_kind(kept), conflict


def detect_regions(lines: list[Line], page_box: Bbox, shapes: Sequence[Shape] = ()) -> list[Region]:
    return _resolve(_raw_regions(lines, page_box, shapes))[0]


def order_within_region(lines: list[Line]) -> list[Line]:
    rows: list[list[Line]] = []
    for line in sorted(lines, key=lambda ln: (ln.bbox[1] + ln.bbox[3]) / 2):
        mid = (line.bbox[1] + line.bbox[3]) / 2
        if rows:
            anchor = rows[-1][0]
            tol = max(1.0, anchor.font_size * 0.5)
            if abs((anchor.bbox[1] + anchor.bbox[3]) / 2 - mid) <= tol:
                rows[-1].append(line)
                continue
        rows.append([line])
    ordered: list[Line] = []
    for row in rows:
        ordered.extend(sorted(row, key=lambda ln: ln.bbox[0]))
    return ordered


@dataclass
class RegionBlock(Block):
    members: list[Block] = field(default_factory=list)
    kind: str = ""


def _inside(block: Block, region: Region) -> bool:
    cx = (block.bbox[0] + block.bbox[2]) / 2
    cy = (block.bbox[1] + block.bbox[3]) / 2
    return region.x0 <= cx <= region.x1 and region.y0 <= cy <= region.y1


def _overlap_ratio(a: Bbox, b: Bbox) -> float:
    shared = min(a[3], b[3]) - max(a[1], b[1])
    return shared / max(1e-6, min(a[3] - a[1], b[3] - b[1]))


def _ccw(lines: list[Line]) -> bool:
    votes = [ln.chars[0].y0 > ln.chars[-1].y0 for ln in lines if ln.vertical and len(ln.chars) > 1]
    return sum(votes) * 2 > len(votes)


def row_major(blocks: list[Block], kind: str = "") -> list[Block]:
    vertical = sum(b.vertical for b in blocks) * 2 > len(blocks)
    if not vertical or kind == "figure":
        return _row_major_axis(blocks, axis=1, rows_reverse=False, cells_reverse=False)
    ccw = _ccw([line for b in blocks for line in b.lines])
    return _row_major_axis(blocks, axis=0, rows_reverse=not ccw, cells_reverse=ccw)


def _row_major_axis(
    blocks: list[Block], axis: int, rows_reverse: bool, cells_reverse: bool
) -> list[Block]:
    def swap(box: Bbox) -> Bbox:
        return (box[1], box[0], box[3], box[2]) if axis == 0 else box

    rows: list[tuple[Bbox, list[Block]]] = []
    for block in sorted(
        blocks, key=lambda b: (b.bbox[axis], b.bbox[1 - axis]), reverse=rows_reverse
    ):
        if rows and _overlap_ratio(swap(rows[-1][0]), swap(block.bbox)) >= 0.5:
            span, members = rows[-1]
            rows[-1] = (_union(span, block.bbox), members + [block])
        else:
            rows.append((block.bbox, [block]))
    ordered: list[Block] = []
    for _, members in rows:
        ordered.extend(sorted(members, key=lambda b: b.bbox[1 - axis], reverse=cells_reverse))
    return ordered


def apply_regions(
    blocks: list[Block], regions: Sequence[Region], shapes: Sequence[Shape] = ()
) -> tuple[list[Block], tuple[str, ...]]:
    """Group the lines of every detected region into one RegionBlock in table order and report
    the region markers: `interleaved_cells` when per-line cells of a region share a band and an
    extent (the row/column association is unsafe, the page reads linearly; not a fallback
    trigger), upgraded to `high_entropy_table` when `_overprinted` finds text glyphs drawn on
    top of other text glyphs (structural corruption, ruling #36)."""
    out = list(blocks)
    interleaved = overprinted = False
    for region in regions:
        lines = [
            ln
            for b in out
            if not isinstance(b, RegionBlock)
            for ln in b.lines
            if _inside(Block(ln.bbox, [ln]), region)
        ]
        if len(lines) < 2:
            continue
        taken = {id(ln) for ln in lines}
        rest = [b for b in out if not any(id(ln) in taken for ln in b.lines)]
        for b in out:
            kept = [ln for ln in b.lines if id(ln) not in taken]
            if kept and len(kept) < len(b.lines):
                rest.append(_block_from(kept))
        upright = sum(not ln.vertical for ln in lines) * 2 > len(lines)
        ordered: list[Block] = []
        if upright and isinstance(region, TableRegion) and region.cols and region.rows:
            ordered = _table_cells(lines, region)  # C26 M2: whole cells per row band
        if upright and region.kind == "lattice" and not ordered:
            ordered = _ruled_cells(lines, region, shapes)
        if upright and region.kind != "figure" and not ordered:
            ordered = _cell_rows(lines, _rule_rows(region, shapes))
        if not upright or region.kind == "figure" or not ordered:
            ordered = row_major([Block(line.bbox, [line]) for line in lines], region.kind)
        elif isinstance(region, TableRegion):
            pass  # C26 M1: a table unit's column order is its own (`_table_cells`), not a vote
        elif bidi.base_direction("".join(ln.text for ln in lines)) == "R":
            ordered = _row_major_axis(ordered, axis=1, rows_reverse=False, cells_reverse=True)
        bbox = ordered[0].bbox
        for b in ordered[1:]:
            bbox = _union(bbox, b.bbox)
        lines = [line for b in ordered for line in b.lines]
        per_line = [Block(line.bbox, [line]) for line in lines]
        if region.kind != "figure":
            pairs = _colliding_pairs(per_line)
            interleaved = interleaved or bool(pairs)
            overprinted = overprinted or _overprinted(pairs)
        out = rest + [RegionBlock(bbox, lines, ordered, region.kind)]
    if overprinted:
        return out, ("high_entropy_table",)
    return out, (("interleaved_cells",) if interleaved else ())


def _member(line: Line, region: Region, shapes: Sequence[Shape]) -> bool:
    """Topology-aware lattice membership: a line is in a ruled table only when a merged horizontal
    rule of that table lies directly above and another directly below its centre. Text in a gap of
    the grid (a legend parked inside the periodic table's outline) has no row rule over it and stays
    outside. Other region kinds keep bounding-box membership."""
    if region.kind != "lattice":
        return True
    cx, cy = (line.bbox[0] + line.bbox[2]) / 2, (line.bbox[1] + line.bbox[3]) / 2
    rules = _merge_segments(
        [b for kind, b in shapes if kind == "hline" and region.y0 - 3 <= b[1] <= region.y1 + 3],
        axis=0,
    )
    spanning = [h for h in rules if h[0] - 2 <= cx <= h[2] + 2]
    return any(h[1] <= cy for h in spanning) and any(h[1] >= cy for h in spanning)


def _flatten(blocks: list[Block]) -> list[Block]:
    flat: list[Block] = []
    for block in blocks:
        flat.extend(block.members if isinstance(block, RegionBlock) else [block])
    return flat


def structure(
    lines: list[Line], page_box: Bbox, shapes: Sequence[Shape] = ()
) -> tuple[list[Block], tuple[str, ...]]:
    regions, conflict = _resolve(_raw_regions(lines, page_box, shapes))
    conflict = conflict or any(_unresolved(r, lines, shapes) for r in regions)
    blocks, region_markers = apply_regions(group_blocks(lines), regions, shapes)
    markers = ambiguity(blocks, page_box, conflict)
    if "ambiguous_layout" not in markers and _bent_gutter(blocks, page_box[2] - page_box[0]):
        markers += ("ambiguous_layout",)
    if "ambiguous_layout" not in markers and _diagram(blocks, shapes):
        markers += ("diagram",)
    return blocks, markers + region_markers


def _unresolved(region: Region, lines: list[Line], shapes: Sequence[Shape]) -> bool:
    """A region whose structure the layer cannot resolve is a conflict, not a confident table:
    a lattice whose outline holds GAP_LINES or more upright lines under no row rule (a legend or
    a paragraph parked in a gap of the grid; rotated tables have their rules along the other axis
    and are exempt), or a borderless table of UNRESOLVED_LINES lines or
    more whose row labels are set at the vertical centre of wrapped cells (`_centred_labels`).
    Such pages read linearly and carry `region_conflict`."""
    inside = [ln for ln in lines if _inside(Block(ln.bbox, [ln]), region)]
    if region.kind == "lattice":
        gap = sum(1 for ln in inside if not ln.vertical and not _member(ln, region, shapes))
        if gap >= GAP_LINES:
            return True
    if region.kind == "alignment" and len(inside) >= UNRESOLVED_LINES:
        groups = _column_groups(inside, _column_positions(inside))
        shattered = sum(1 for g in groups if len(g) == 1) * 2 > len(groups)
        return shattered and _centred_labels(inside) >= CENTRED_LABELS
    return False


def _centred_labels(inside: list[Line]) -> int:
    """Row labels set at the vertical centre of a wrapped cell: the label's centre lies inside
    another column's tight multi-line run while its top aligns with none of that run's line tops,
    so no row start can be read from line tops. Counted over the leftmost column."""
    groups = _column_groups(inside, _column_positions(inside))
    if len(groups) < 2:
        return 0
    labels = min(groups, key=lambda g: min(ln.bbox[0] for ln in g))
    runs: list[list[Line]] = []
    for g in groups:
        if g is labels:
            continue
        for ln in sorted(g, key=lambda ln: ln.bbox[1]):
            if runs and runs[-1][-1] in g and _tight_below(ln, g):
                runs[-1].append(ln)
            else:
                runs.append([ln])
    count = 0
    for lab in labels:
        cy = (lab.bbox[1] + lab.bbox[3]) / 2
        for run in runs:
            if len(run) < 2 or not (run[0].bbox[1] < cy < run[-1].bbox[3]):
                continue
            if all(abs(ln.bbox[1] - lab.bbox[1]) > 2.0 for ln in run):
                count += 1
                break
    return count


def _cell_dense(block: Block) -> bool:
    height = block.bbox[3] - block.bbox[1]
    return len(block.lines) >= FRAME_LINES and len(block.lines) * 0.8 * block.font_size > height


def _bent_gutter(blocks: list[Block], width: float, regions_only: bool = True) -> bool:
    """Two stacked bands that both split into columns at different x, neither split clean, and a
    region (a table wider than its column) straddling the other band's gutter: the gutter bends
    around the table, and whether the table belongs to the left column is not decidable from
    geometry, so the page reads linearly. A plain change of column count is not this case. With
    `regions_only` off (a backend's own blocks, which carry no regions) a cell-dense block stands
    for the table: FRAME_LINES lines or more, packed tighter than its font size allows for rows."""
    for at in _row_candidates(blocks)[:MAX_ROW_CANDIDATES]:
        top, bottom = _split(blocks, 1, at)
        if not top or not bottom or _crosses(blocks, at):
            continue
        cut_top, cut_bottom = _column_cut(top, width), _column_cut(bottom, width)
        if cut_top is None or cut_bottom is None or abs(cut_top - cut_bottom) < COLUMN_GAP * width:
            continue
        if _clean_split(bottom, cut_top) or _clean_split(top, cut_bottom):
            continue
        for half, own_cut, other_cut in ((top, cut_top, cut_bottom), (bottom, cut_bottom, cut_top)):
            left, right = _split(half, 0, own_cut)
            for side in (left, right):
                wide = [b for b in side if b.bbox[0] < other_cut < b.bbox[2]]
                table = (len(side) == 1 and isinstance(side[0], RegionBlock)) or (
                    not regions_only and any(_cell_dense(b) for b in wide)
                )
                if wide and table:
                    return True
    return False


def _diagram(blocks: list[Block], shapes: Sequence[Shape]) -> bool:
    """Scattered single-line labels over drawn strokes (a flowchart, two plots side by side, a slide
    diagram): no reading order is geometric. Ruling: such a page is read linearly, top-down then
    left-right, and marked so the caller knows. A left-aligned stack of labels is a list, not a
    diagram."""
    if len(blocks) < DIAGRAM_BLOCKS:
        return False
    loose = [b for b in blocks if not isinstance(b, RegionBlock)]
    single = [b for b in loose if len(b.lines) == 1 and not b.vertical]
    if len(single) * 10 < DIAGRAM_SINGLE * len(loose):
        return False
    if sum(len(b.lines) for b in blocks if isinstance(b, RegionBlock)) > len(single):
        return False
    if sum(1 for kind, _ in shapes if kind in ("hline", "vline")) < DIAGRAM_STROKES:
        return False
    lefts = sorted(b.bbox[0] for b in single)
    spread = 1 + sum(1 for a, b in zip(lefts, lefts[1:], strict=False) if b - a > 3.0)
    if spread < DIAGRAM_SPREAD or _columnar(single):
        return False
    words = [b for b in single if not _lone_number(b.lines[0])]
    if sum(1 for b in words if len(b.text.strip()) == 1) >= DIAGRAM_GLYPHS:
        return False
    beside = sum(
        1
        for i, a in enumerate(words)
        if any(
            _overlap_ratio(a.bbox, b.bbox) >= 0.5 and b.bbox[0] >= a.bbox[2] for b in words[i + 1 :]
        )
    )
    return beside >= DIAGRAM_BESIDE


def _band(block: Block) -> tuple[float, float]:
    mid = (block.bbox[1] + block.bbox[3]) / 2
    half = 0.25 * max(block.font_size, 1.0)
    return mid - half, mid + half


def _colliding_pairs(cells: list[Block]) -> list[tuple[Block, Block]]:
    """Per-line cells of one region whose centre bands overlap and whose extents share at least
    half of the narrower cell: the layer cannot tell which row or column either belongs to."""
    pairs: list[tuple[Block, Block]] = []
    for i, a in enumerate(cells):
        for b in cells[i + 1 :]:
            (a0, a1), (b0, b1) = _band(a), _band(b)
            if min(a1, b1) - max(a0, b0) <= 0:
                continue
            shared = min(a.bbox[2], b.bbox[2]) - max(a.bbox[0], b.bbox[0])
            if shared >= OVERPRINT_SHARE * min(a.bbox[2] - a.bbox[0], b.bbox[2] - b.bbox[0]):
                pairs.append((a, b))
    return pairs


def _collides(cells: list[Block]) -> bool:
    return bool(_colliding_pairs(cells))


def _text_glyphs(cell: Block) -> list[Char]:
    """Glyphs that carry a cell's ink: visible letters and digits with a real box. Spaces,
    punctuation runs (leaders, rules and OCR residue drawn as text), accents and combining marks
    sit on or between other glyphs by design and never make a cell. Invisible glyphs (render mode
    3 / 7 — the OCR layer of a searchable scan) paint nothing, so nothing can be drawn over them
    and they cannot be drawn over anything: overprint is a property of ink (ruling #36, ING-12)."""
    return [
        ch
        for ln in cell.lines
        for ch in ln.chars
        if ch.text.strip()
        and not ch.invisible
        and ch.x1 > ch.x0
        and ch.y1 > ch.y0
        and ch.text[:1].isalnum()
        and unicodedata.category(ch.text[:1]) not in _ACCENT_CATEGORIES
    ]


def _drawn_twice(a: Block, b: Block) -> bool:
    """The same text at the same place (fake bold, shadows, a label re-drawn at another size):
    one object, not two cells."""
    if a.text.replace(" ", "") != b.text.replace(" ", ""):
        return False
    tolerance = OVERPRINT_SHARE * min(a.font_size, b.font_size)
    return all(
        abs((a.bbox[i] + a.bbox[i + 2]) - (b.bbox[i] + b.bbox[i + 2])) / 2 <= tolerance
        for i in (0, 1)
    )


def _overprinted(pairs: Sequence[tuple[Block, Block]]) -> bool:
    """Structural corruption (ruling #36): two colliding cells whose text glyphs are drawn on top
    of each other. Glyph boxes, not line boxes, decide: a fragment parked in the gap of a wider
    line, a superscript beside its base or an accent over it share the line's box but no glyph
    box. Each cell needs OVERPRINT_GLYPHS text glyphs, the overprinted glyph pairs must intersect
    by OVERPRINT_SHARE of the smaller glyph on both axes, and the covered width must reach
    OVERPRINT_SHARE of the narrower cell's glyph width; the same text drawn twice is one cell."""
    for a, b in pairs:
        if _drawn_twice(a, b):
            continue
        ga, gb = _text_glyphs(a), _text_glyphs(b)
        if len(ga) < OVERPRINT_GLYPHS or len(gb) < OVERPRINT_GLYPHS:
            continue
        narrow, wide = (ga, gb) if _glyph_width(ga) <= _glyph_width(gb) else (gb, ga)
        covered = sum(_overprint_width(ch, wide) for ch in narrow)
        if covered >= OVERPRINT_SHARE * _glyph_width(narrow):
            return True
    return False


def _glyph_width(glyphs: Sequence[Char]) -> float:
    return sum(ch.x1 - ch.x0 for ch in glyphs)


def _overprint_width(ch: Char, others: Sequence[Char]) -> float:
    best = 0.0
    for other in others:
        dx = min(ch.x1, other.x1) - max(ch.x0, other.x0)
        dy = min(ch.y1, other.y1) - max(ch.y0, other.y0)
        if dx >= OVERPRINT_SHARE * min(ch.x1 - ch.x0, other.x1 - other.x0) and dy >= (
            OVERPRINT_SHARE * min(ch.y1 - ch.y0, other.y1 - other.y0)
        ):
            best = max(best, dx)
    return best


def _word_starts(line: Line) -> list[float]:
    starts: list[float] = []
    prev_end: float | None = None
    for ch in line.chars:
        if ch.text.isspace():
            continue
        if prev_end is None or ch.x0 - prev_end >= line.font_size:
            starts.append(ch.x0)
        prev_end = ch.x1
    return starts


def _ideographs(text: str) -> int:
    return sum(
        1
        for ch in text
        if "\u2e80" <= ch <= "\u9fff"
        or "\uac00" <= ch <= "\ud7af"
        or "\uf900" <= ch <= "\ufaff"
        or "\uff00" <= ch <= "\uffef"
    )


def _is_prose(line: Line) -> bool:
    dense = (
        len(line.text.split()) >= PROSE_WORDS
        or len(line.text.strip()) >= PROSE_CHARS
        or _ideographs(line.text) >= PROSE_IDEOGRAPHS
    )
    return dense and len(_word_starts(line)) <= 1


def _runs(lines: list[Line]) -> list[list[Line]]:
    runs: list[list[Line]] = []
    for line in sorted(lines, key=lambda ln: ln.bbox[1]):
        if runs and line.bbox[1] - runs[-1][-1].bbox[3] <= RUN_GAP * line.font_size:
            runs[-1].append(line)
        else:
            runs.append([line])
    return runs


def _columns(starts: list[tuple[float, int]], tol: float = 3.0) -> list[set[int]]:
    clusters: list[tuple[float, set[int]]] = []
    for x, idx in sorted(starts):
        if clusters and x - clusters[-1][0] <= tol:
            clusters[-1][1].add(idx)
        else:
            clusters.append((x, {idx}))
    return [members for _, members in clusters if len(members) >= 3]


def _side_by_side(lines: list[Line]) -> bool:
    ordered = sorted(lines, key=lambda ln: ln.bbox[1])
    return any(
        _overlap_ratio(a.bbox, b.bbox) >= 0.5 and (a.bbox[2] <= b.bbox[0] or b.bbox[2] <= a.bbox[0])
        for a, b in zip(ordered, ordered[1:], strict=False)
    )


def _region_of(lines: list[Line], kind: str) -> Region:
    bbox = lines[0].bbox
    for line in lines[1:]:
        bbox = _union(bbox, line.bbox)
    return Region(*bbox, kind)


def _alignment_regions(lines: list[Line], page_box: Bbox) -> list[Region]:
    width = page_box[2] - page_box[0]
    regions: list[Region] = []
    upright = [ln for ln in lines if not ln.vertical]
    paragraph = _paragraph_lines(upright)
    for run in _runs([ln for ln in upright if id(ln) not in paragraph]):
        if len(run) < 3:
            continue
        columns = _column_clusters(run)
        if len(columns) < 2:
            continue
        members = [run[i] for i in sorted(set().union(*columns))]
        members = _trim_rows(_complete_rows(members, run, paragraph))
        table = len(members) >= 3 and _side_by_side(members) and _tabular(members)
        if table and not _gutter_aligned(members, width):
            members = _left_of_gutter(members, width)
            table = len(members) >= 3 and _side_by_side(members) and _tabular(members)
            table = table and len(_column_clusters(members)) >= 2
        if table:
            regions.append(_region_of(members, "alignment"))
    return regions


def _column_clusters(lines: list[Line]) -> list[set[int]]:
    """Word-start clusters plus line-end clusters: figures in a numeric column are right-aligned,
    so their starts are ragged (415 / 421 / 426) while their ends share one x."""
    starts = [(x, i) for i, ln in enumerate(lines) for x in _word_starts(ln)]
    ends = [(ln.bbox[2], i) for i, ln in enumerate(lines)]
    return _columns(starts) + _columns(ends)


def _row_mate(line: Line, pool: list[Line]) -> Line | None:
    mates = [
        other
        for other in pool
        if other is not line
        and other.bbox[0] >= line.bbox[2]
        and _overlap_ratio(line.bbox, other.bbox) >= 0.5
    ]
    return min(mates, key=lambda ln: ln.bbox[0]) if mates else None


def _tabular(lines: list[Line]) -> bool:
    words = sorted(
        max(
            sum(1 for w in ln.text.split() if any(ch.isalnum() for ch in w)),
            _ideographs(ln.text) // 2,
        )
        for ln in lines
    )
    text = "".join(ln.text for ln in lines).replace(" ", "")
    digits = sum(ch.isdigit() for ch in text) / max(1, len(text))
    if words[len(words) // 2] <= 3 or digits >= 0.15:
        return True
    clusters: list[list[Line]] = []
    for ln in sorted(lines, key=lambda x: x.bbox[0]):
        if clusters and abs(ln.bbox[0] - clusters[-1][0].bbox[0]) <= 3.0:
            clusters[-1].append(ln)
        else:
            clusters.append([ln])
    return any(len(c) >= 3 and _numeric_column(c) for c in clusters)


def _keyvalue_regions(lines: list[Line], page_width: float) -> list[Region]:
    regions: list[Region] = []
    upright = [ln for ln in lines if not ln.vertical]
    paragraph = _paragraph_lines(upright)
    for run in _runs([ln for ln in upright if id(ln) not in paragraph]):
        if len(run) < 3:
            continue
        labels = [ln for ln in run if len(ln.text.split()) <= 4 and _ideographs(ln.text) <= 8]
        for edge in _columns([(ln.bbox[2], i) for i, ln in enumerate(labels)]):
            mates = [(labels[i], _row_mate(labels[i], run)) for i in sorted(edge)]
            pairs = [(k, v) for k, v in mates if v is not None]
            if len(pairs) < 3:
                continue
            if len(_columns([(v.bbox[0], i) for i, (_, v) in enumerate(pairs)])) != 1:
                continue
            cells = [ln for pair in pairs for ln in pair]
            if _tabular(cells) and _gutter_aligned(cells, page_width):
                regions.append(_region_of(cells, "keyvalue"))
    return regions


def _disjoint(regions: list[Region]) -> list[Region]:
    kept: list[Region] = []
    for region in regions:
        cx, cy = (region.x0 + region.x1) / 2, (region.y0 + region.y1) / 2
        if not any(k.x0 <= cx <= k.x1 and k.y0 <= cy <= k.y1 for k in kept):
            kept.append(region)
    return kept


def _snap(values: list[float], tol: float = 3.0) -> list[float]:
    out: list[float] = []
    for v in sorted(values):
        if not out or v - out[-1] > tol:
            out.append(v)
    return out


def _lattice_regions(shapes: Sequence[Shape], lines: Sequence[Line] = ()) -> list[Region]:
    hs = _merge_segments([b for kind, b in shapes if kind == "hline"], axis=0)
    vs = _merge_segments([b for kind, b in shapes if kind == "vline"], axis=1)
    regions: list[Region] = []
    if len(hs) * len(vs) > LATTICE_BUDGET:
        return regions
    for v in vs:
        crossing = [h for h in hs if h[0] - 3 <= v[0] <= h[2] + 3 and v[1] - 3 <= h[1] <= v[3] + 3]
        if len(crossing) < 3:
            continue
        x0, x1 = min(h[0] for h in crossing), max(h[2] for h in crossing)
        cols = [
            w
            for w in vs
            if x0 - 3 <= w[0] <= x1 + 3 and any(w[1] - 3 <= h[1] <= w[3] + 3 for h in crossing)
        ]
        rows = _snap([h[1] for h in crossing])
        if len(_snap([w[0] for w in cols])) >= 3 and len(rows) >= 3:
            y0, y1 = min(w[1] for w in cols), max(w[3] for w in cols)
            region = Region(
                min(x0, min(w[0] for w in cols)),
                y0,
                max(x1, max(w[2] for w in cols)),
                y1,
                "lattice",
            )
            inside = [ln for ln in lines if _inside(Block(ln.bbox, [ln]), region)]
            if len(inside) >= 2 and not _framed_band(rows, cols, inside):
                regions.append(region)
    return _disjoint(regions)


def _framed_band(rows: list[float], cols: list[Bbox], inside: list[Line]) -> bool:
    """A slide frame with a divided header bar and footer bar crosses like a grid, but the body band
    between the bars is spanned by no vertical rule except the frame's own: one cell holding most of
    the
    text is a frame around a page, not a table row."""
    for top, bottom in zip(rows, rows[1:], strict=False):
        spanning = [w for w in cols if w[1] - 3 <= top and w[3] + 3 >= bottom]
        held = [ln for ln in inside if top <= (ln.bbox[1] + ln.bbox[3]) / 2 < bottom]
        if len(spanning) <= 2 and len(held) >= FRAME_LINES and len(held) * 10 > 6 * len(inside):
            return True
    return False


def _has_body_neighbour(line: Line, lines: list[Line]) -> bool:
    slack = 0.6 * max(line.font_size, 1.0)
    for other in lines:
        if other is line or abs(other.bbox[0] - line.bbox[0]) > 3.0 or len(other.text.split()) <= 3:
            continue
        gap = max(line.bbox[1] - other.bbox[3], other.bbox[1] - line.bbox[3])
        if gap <= slack:
            return True
    return False


def _figure_regions(lines: list[Line], shapes: Sequence[Shape]) -> list[Region]:
    regions: list[Region] = []
    for kind, box in shapes:
        if kind not in ("figure", "image"):
            continue
        frame = Region(*box, "figure")
        inside = [ln for ln in lines if _inside(Block(ln.bbox, [ln]), frame)]
        labels = [
            ln for ln in inside if len(ln.text.split()) <= 3 and not _has_body_neighbour(ln, lines)
        ]
        if len(labels) >= 3 and len(labels) * 2 >= len(inside):
            regions.append(_region_of(labels, "figure"))
    return regions


def _gaps(blocks: list[Block], axis: int, minimum: float) -> list[float]:
    spans = sorted((b.bbox[axis], b.bbox[axis + 2]) for b in blocks)
    widths: list[float] = []
    reach = spans[0][1]
    for lo, hi in spans[1:]:
        if lo - reach >= minimum:
            widths.append(lo - reach)
        reach = max(reach, hi)
    return sorted(widths, reverse=True)


def _overlaps(a: Region, b: Region) -> bool:
    return a.x0 < b.x1 and b.x0 < a.x1 and a.y0 < b.y1 and b.y0 < a.y1


def ambiguity(blocks: list[Block], page_box: Bbox, conflict: bool = False) -> tuple[str, ...]:
    flags: tuple[str, ...] = ("region_conflict",) if conflict else ()
    if len(blocks) < 2:
        return flags
    threshold = COLUMN_GAP * (page_box[2] - page_box[0])
    gaps = _gaps(blocks, 0, threshold)
    if not gaps:
        return flags
    crowded = sum(1 for g in gaps if g >= gaps[0] * (1 - CUT_EPSILON)) >= CROWDED_CUTS
    weak = gaps[0] < WEAK_CUT * threshold
    return flags + (("ambiguous_layout",) if weak or crowded else ())


def linear_order(blocks: list[Block]) -> list[Block]:
    lines = [Block(line.bbox, [line]) for b in _flatten(blocks) for line in b.lines]
    rtl = _page_rtl(lines)
    return sorted(lines, key=lambda b: (round(b.bbox[1]), -b.bbox[2] if rtl else b.bbox[0]))


def _merge_same_kind(regions: list[Region]) -> list[Region]:
    merged: list[Region] = []
    for region in regions:
        for i, kept in enumerate(merged):
            if kept.kind == region.kind and _overlaps(kept, region):
                merged[i] = Region(*_union(kept.bbox, region.bbox), kept.kind)
                break
        else:
            merged.append(region)
    return merged


def _framed(region: Region, frames: Sequence[Region]) -> bool:
    cx, cy = (region.x0 + region.x1) / 2, (region.y0 + region.y1) / 2
    return any(f.x0 <= cx <= f.x1 and f.y0 <= cy <= f.y1 for f in frames)


def _left_of_gutter(lines: list[Line], page_width: float) -> list[Line]:
    spans = sorted((ln.bbox[0], ln.bbox[2]) for ln in lines)
    best: tuple[float, float] | None = None
    reach = spans[0][1]
    for lo, hi in spans[1:]:
        if lo - reach >= COLUMN_GAP * page_width and (best is None or lo - reach > best[0]):
            best = (lo - reach, (reach + lo) / 2)
        reach = max(reach, hi)
    if best is None:
        return []
    return [ln for ln in lines if ln.bbox[2] <= best[1]]


def _gutter_aligned(lines: list[Line], page_width: float, share: float = 0.6) -> bool:
    spans = sorted((ln.bbox[0], ln.bbox[2]) for ln in lines)
    best: tuple[float, float] | None = None
    reach = spans[0][1]
    for lo, hi in spans[1:]:
        if lo - reach >= COLUMN_GAP * page_width and (best is None or lo - reach > best[0]):
            best = (lo - reach, (reach + lo) / 2)
        reach = max(reach, hi)
    if best is None:
        return True
    left = [ln for ln in lines if ln.bbox[2] <= best[1]]
    right = [ln for ln in lines if ln.bbox[0] >= best[1]]
    span = max(ln.bbox[2] for ln in left) - min(ln.bbox[0] for ln in left) if left else 0.0
    short = [ln for ln in left if ln.bbox[2] - ln.bbox[0] < 0.6 * span] or left
    aligned = sum(
        any(
            abs(a.bbox[1] - b.bbox[1]) <= 1.5
            or abs(a.bbox[3] - b.bbox[3]) <= 1.5
            or _overlap_ratio(a.bbox, b.bbox) >= 0.8
            for b in right
        )
        for a in short
    )
    return aligned >= share * max(1, len(short))


def _paragraph_lines(run: list[Line], depth: int = 3) -> set[int]:
    flagged: set[int] = set()
    groups: list[tuple[float, list[Line]]] = []
    for line in sorted(run, key=lambda ln: ln.bbox[0]):
        if groups and abs(line.bbox[0] - groups[-1][0]) <= 1.0:
            groups[-1][1].append(line)
        else:
            groups.append((line.bbox[0], [line]))
    for _, group in groups:
        chain: list[Line] = []
        for line in sorted(group, key=lambda ln: ln.bbox[1]):
            tight = bool(chain) and line.bbox[1] - chain[-1].bbox[3] <= 0.5 * line.font_size
            if tight:
                chain.append(line)
                continue
            if len(chain) >= depth and _paragraph_like(chain) and not _list_like(chain):
                flagged.update(id(ln) for ln in chain)
            chain = [line]
        if len(chain) >= depth and _paragraph_like(chain) and not _list_like(chain):
            flagged.update(id(ln) for ln in chain)
    return flagged


def _list_like(chain: list[Line]) -> bool:
    heads = [next((ch for ch in ln.text if ch.isalnum()), "") for ln in chain]
    capital = sum(1 for h in heads if h and (h.isupper() or h.isdigit()))
    return capital * 10 >= 8 * len(chain)


def _uniform_width(chain: list[Line]) -> bool:
    body = chain[:-1] if len(chain) > 1 else chain
    widest = max(ln.bbox[2] - ln.bbox[0] for ln in body)
    full = sum((ln.bbox[2] - ln.bbox[0]) >= 0.75 * widest for ln in body)
    return full * 10 >= 7 * len(body)


def _paragraph_like(chain: list[Line]) -> bool:
    dense = sum(_is_prose(ln) for ln in chain)
    if dense * 10 >= 7 * len(chain) and _uniform_width(chain):
        return True
    widths = sorted(ln.bbox[2] - ln.bbox[0] for ln in chain)
    words = sorted(sum(1 for w in ln.text.split() if any(ch.isalpha() for ch in w)) for ln in chain)
    glyphs = sorted(_ideographs(ln.text) for ln in chain)
    wordy = words[len(words) // 2] >= 3 or glyphs[len(glyphs) // 2] >= 10
    if widths[len(widths) // 2] < JUSTIFIED_EM * chain[0].font_size or not wordy:
        return False
    if sum(_leads_with_digit(ln) for ln in chain) * 2 > len(chain):
        return False
    right = sorted(ln.bbox[2] for ln in chain)[len(chain) // 2]
    flush = sum(abs(ln.bbox[2] - right) <= 2.0 for ln in chain[:-1])
    return flush * 10 >= 8 * max(1, len(chain) - 1)


def _complete_rows(members: list[Line], run: list[Line], paragraph: set[int]) -> list[Line]:
    taken = {id(ln) for ln in members}
    starts = [(ln.bbox[0], i) for i, ln in enumerate(run) if id(ln) not in paragraph]
    supported = {i for cluster in _columns(starts) for i in cluster} | {
        i for cluster in _pairs(starts) for i in cluster
    }
    extra = [
        run[i]
        for i in sorted(supported)
        if id(run[i]) not in taken
        and id(run[i]) not in paragraph
        and any(_overlap_ratio(run[i].bbox, m.bbox) >= 0.5 for m in members)
    ]
    return members + extra


def _pairs(starts: list[tuple[float, int]], tol: float = 3.0) -> list[set[int]]:
    clusters: list[tuple[float, set[int]]] = []
    for x, idx in sorted(starts):
        if clusters and x - clusters[-1][0] <= tol:
            clusters[-1][1].add(idx)
        else:
            clusters.append((x, {idx}))
    return [members for _, members in clusters if len(members) >= 2]


def _rows_of(lines: list[Line]) -> list[list[Line]]:
    rows: list[tuple[Bbox, list[Line]]] = []
    for line in sorted(lines, key=lambda ln: (ln.bbox[1], ln.bbox[0])):
        if rows and _overlap_ratio(rows[-1][0], line.bbox) >= 0.5:
            span, members = rows[-1]
            rows[-1] = (_union(span, line.bbox), members + [line])
        else:
            rows.append((line.bbox, [line]))
    return [members for _, members in rows]


def _trim_rows(members: list[Line]) -> list[Line]:
    rows = _rows_of(members)
    wide = [i for i, row in enumerate(rows) if len(row) >= 2 or len(_word_starts(row[0])) >= 2]
    if len(wide) < 2:
        return []
    return [ln for row in rows[wide[0] : wide[-1] + 1] for ln in row]


def _merge_segments(segments: list[Bbox], axis: int, tol: float = 3.0) -> list[Bbox]:
    other = 1 - axis
    merged: list[Bbox] = []
    for seg in sorted(segments, key=lambda b: (round(b[other] / tol), b[axis])):
        for i, kept in enumerate(merged):
            same_line = abs(kept[other] - seg[other]) <= tol
            touching = seg[axis] <= kept[axis + 2] + tol and seg[axis + 2] >= kept[axis] - tol
            if same_line and touching:
                merged[i] = _union(kept, seg)
                break
        else:
            merged.append(seg)
    return merged


def _column_positions(lines: list[Line], tol: float = 3.0) -> list[float]:
    clusters: list[tuple[float, int]] = []
    for x in sorted(ln.bbox[0] for ln in lines):
        if clusters and x - clusters[-1][0] <= tol:
            clusters[-1] = (clusters[-1][0], clusters[-1][1] + 1)
        else:
            clusters.append((x, 1))
    return [x for x, n in clusters if n >= 2]


def _spans_columns(line: Line, positions: list[float]) -> bool:
    inside = [p for p in positions if line.bbox[0] + 3 < p < line.bbox[2] - 3]
    return len(inside) >= 1


def _column_groups(lines: list[Line], positions: list[float]) -> list[list[Line]]:
    groups: list[tuple[Bbox, list[Line]]] = []
    for line in sorted(lines, key=lambda ln: (ln.bbox[2] - ln.bbox[0], ln.bbox[0], ln.bbox[1])):
        hits = [
            i
            for i, (head, _) in enumerate(groups)
            if min(head[2], line.bbox[2]) - max(head[0], line.bbox[0])
            >= 0.5 * max(1e-6, min(head[2] - head[0], line.bbox[2] - line.bbox[0]))
        ]
        if len(hits) == 1:
            head, members = groups[hits[0]]
            groups[hits[0]] = (head, members + [line])
        else:
            groups.append((line.bbox, [line]))
    ordered = sorted(groups, key=lambda g: min(ln.bbox[0] for ln in g[1]))
    return [members for _, members in ordered]


def _group_of(line: Line, groups: list[list[Line]]) -> list[Line]:
    return next((g for g in groups if any(ln is line for ln in g)), [line])


def _numeric_column(group: list[Line]) -> bool:
    return sum(_lone_number(ln) for ln in group) * 10 >= 7 * max(1, len(group))


def _leads_with_digit(line: Line) -> bool:
    text = line.text.strip().lstrip("([-–—+$€£%")
    return bool(text) and text[0].isdigit()


def _continuation(line: Line, group: list[Line]) -> bool:
    if not _tight_below(line, group) or _leads_with_digit(line):
        return False
    text = line.text.strip()
    head = next((ch for ch in text if ch.isalnum()), "")
    x0 = sorted(ln.bbox[0] for ln in group)[len(group) // 2]
    return head.islower() or not text[:1].isalnum() or line.bbox[0] > x0 + 3.0


def _row_boundaries(groups: list[list[Line]], known_tops: Sequence[float] = ()) -> list[Line]:
    found: list[Line] = []
    skipped: list[list[Line]] = []
    if groups:
        leftmost = min(groups, key=lambda g: min(ln.bbox[0] for ln in g))
        found.extend(ln for ln in leftmost if not _continuation(ln, leftmost))
    for y in _row_tops(groups):
        if any(abs(f.bbox[1] - y) <= 2.0 for f in found):
            continue
        at_top = [
            ln
            for g in groups
            for ln in g
            if abs(ln.bbox[1] - y) <= 2.0
            or (
                ln.bbox[1] < y + 2.0
                and ln.bbox[3] > y + 2.0
                and _overlap_ratio(ln.bbox, (0.0, y, 0.0, y + ln.font_size)) >= 0.5
            )
        ]
        if all(_continuation(ln, _group_of(ln, groups)) for ln in at_top):
            skipped.append(at_top)
            continue
        found.append(min(at_top, key=lambda ln: ln.bbox[0]))
    return _rows_on_pitch(found, skipped, known_tops)


def _rows_on_pitch(
    found: list[Line], skipped: list[list[Line]], known_tops: Sequence[float]
) -> list[Line]:
    """Table rows sit on the font leading plus cell padding; a wrapped line inside a cell sits on
    the bare leading. In a table whose row pitch exceeds the leading, a row of lowercase cells that
    starts exactly one row pitch below a known row is a row of its own, not a wrap."""
    tops = sorted([f.bbox[1] for f in found] + list(known_tops))
    every = sorted(set(tops) | {min(ln.bbox[1] for ln in g) for g in skipped})
    pitches = [b - a for a, b in zip(every, every[1:], strict=False) if b - a > 2.0]
    sizes = sorted(ln.font_size for ln in found) or sorted(
        ln.font_size for g in skipped for ln in g
    )
    if not pitches or not sizes:
        return found
    pitch = min(pitches)
    if pitch <= CELL_LEADING * sizes[len(sizes) // 2]:
        return found
    for at_top in sorted(skipped, key=lambda g: min(ln.bbox[1] for ln in g)):
        y = min(ln.bbox[1] for ln in at_top)
        if any(abs((y - top) - pitch) <= PITCH_TOL for top in tops):
            found.append(min(at_top, key=lambda ln: ln.bbox[0]))
            tops.append(y)
    return found


def _tight_below(line: Line, group: list[Line]) -> bool:
    slack = 0.5 * max(line.font_size, 1.0)
    above = [
        ln
        for ln in group
        if ln is not line and ln.bbox[1] < line.bbox[1] and ln.bbox[3] <= line.bbox[1] + slack
    ]
    if not above:
        return False
    prev = max(above, key=lambda ln: ln.bbox[3])
    return line.bbox[1] - prev.bbox[3] <= slack


def _ruled_cells(lines: list[Line], region: Region, shapes: Sequence[Shape]) -> list[Block]:
    """A ruled table's structure is its rules: a band between two horizontal rules is one row
    whatever
    the line tops inside it, and the vertical rules spanning that band are its cell walls. Each cell
    is
    read whole, top-down; cells left to right; rows top-down. Returns [] when the region has fewer
    than
    two row rules, so the aligned-top reading takes over (booktabs, borderless)."""
    rows = _rule_rows(region, shapes)
    if len(rows) < 2 or not any(region.y0 + 3 < y < region.y1 - 3 for y in rows):
        return []
    bounds = sorted({region.y0 - 1.0, region.y1 + 1.0} | set(rows))
    walls = _merge_segments(
        [b for kind, b in shapes if kind == "vline" and region.x0 - 3 <= b[0] <= region.x1 + 3],
        axis=1,
    )
    cells: list[Block] = []
    for top, bottom in zip(bounds, bounds[1:], strict=False):
        held = [ln for ln in lines if top <= (ln.bbox[1] + ln.bbox[3]) / 2 < bottom]
        if not held:
            continue
        interior = {
            w[0]
            for w in walls
            if region.x0 + 3 < w[0] < region.x1 - 3
            and w[1] <= top + 0.2 * (bottom - top)
            and w[3] >= bottom - 0.2 * (bottom - top)
        }
        xs = sorted({region.x0 - 1.0, region.x1 + 1.0} | (interior or set(_gutter_walls(held))))
        if any(ln.bbox[0] < x - 2 < ln.bbox[2] for ln in held for x in xs[1:-1]):
            return []
        for sub_top, sub_bottom in _unruled_rows(held, xs, top, bottom):
            cells.extend(
                _band_cells(
                    [ln for ln in held if sub_top <= (ln.bbox[1] + ln.bbox[3]) / 2 < sub_bottom], xs
                )
            )
    return cells


def _gutter_walls(held: list[Line]) -> list[float]:
    """Cell boundaries of a band without interior rules: the midpoints of whitespace gaps, at least
    one em
    wide, that no line of the band crosses."""
    em = sorted(ln.font_size for ln in held)[len(held) // 2]
    spans = sorted((ln.bbox[0], ln.bbox[2]) for ln in held)
    walls: list[float] = []
    reach = spans[0][1]
    for lo, hi in spans[1:]:
        if lo - reach >= em:
            walls.append((reach + lo) / 2)
        reach = max(reach, hi)
    return walls


def _unruled_rows(
    held: list[Line], xs: list[float], top: float, bottom: float
) -> list[tuple[float, float]]:
    """A band between two rules may hold several body rows with no rule between them (a header rule
    and a
    frame only). A line top is a row start when lines sit at it in two thirds of the occupied cell
    columns
    and not every one of them reads as a wrapped continuation; wrapped cells (lowercase, tight)
    never do."""

    def col(ln: Line) -> int:
        cx = (ln.bbox[0] + ln.bbox[2]) / 2
        return next(
            (i for i, (a, b) in enumerate(zip(xs, xs[1:], strict=False)) if a <= cx < b), -1
        )

    by_col: dict[int, list[Line]] = {}
    for ln in held:
        by_col.setdefault(col(ln), []).append(ln)
    tops: dict[float, list[Line]] = {}
    for ln in sorted(held, key=lambda ln: ln.bbox[1]):
        key = next((t for t in tops if abs(t - ln.bbox[1]) <= 2.0), ln.bbox[1])
        tops.setdefault(key, []).append(ln)
    first = min(by_col) if by_col else -1
    labels = by_col.get(first, [])

    def labelled(
        t: float,
    ) -> bool:
        return any(abs(ln.bbox[1] - t) <= 0.8 * ln.font_size for ln in labels)

    starts = [
        t
        for t, group in tops.items()
        if labelled(t)
        and len({col(ln) for ln in group}) * 3 >= 2 * len(by_col)
        and sum(not _continuation(ln, by_col[col(ln)]) for ln in group) * 2 > len(group)
    ]
    if len(starts) < 2:
        return [(top, bottom)]
    edges = [top] + [t - 1.0 for t in sorted(starts)[1:]] + [bottom]
    return list(zip(edges, edges[1:], strict=False))


def _band_cells(held: list[Line], xs: list[float]) -> list[Block]:
    cells: list[Block] = []
    if held:
        for left, right in zip(xs, xs[1:], strict=False):
            members = sorted(
                (ln for ln in held if left <= (ln.bbox[0] + ln.bbox[2]) / 2 < right),
                key=lambda ln: (ln.bbox[1], ln.bbox[0]),
            )
            if members:
                bbox = members[0].bbox
                for ln in members[1:]:
                    bbox = _union(bbox, ln.bbox)
                cells.append(Block(bbox, members))
    return cells


def _rule_rows(region: Region, shapes: Sequence[Shape]) -> list[float]:
    width = max(1e-6, region.x1 - region.x0)
    inside = [b for kind, b in shapes if kind == "hline" and region.y0 - 2 <= b[1] <= region.y1 + 2]
    ys = [
        (b[1] + b[3]) / 2
        for b in _merge_segments(inside, axis=0)
        if min(b[2], region.x1) - max(b[0], region.x0) >= 0.5 * width
    ]
    return _snap(ys)


def _cell_rows(lines: list[Line], rules: Sequence[float] = ()) -> list[Block]:
    positions = _column_positions(lines)
    groups = _column_groups(lines, positions)
    if not groups:
        return []
    span = max(ln.bbox[2] for ln in lines) - min(ln.bbox[0] for ln in lines)
    spanning = {
        id(ln)
        for g in groups
        for ln in g
        if _spans_groups(ln, [h for h in groups if h is not g])
        or ln.bbox[2] - ln.bbox[0] >= WIDE_ROW * span
    }
    groups = [[ln for ln in g if id(ln) not in spanning] for g in groups]
    groups = [g for g in groups if g]
    wide = [ln for ln in lines if id(ln) in spanning]
    plain = list(groups)
    groups.extend([ln] for ln in wide)
    starts = _dedupe_tops(
        sorted(
            wide + _row_boundaries(plain, [ln.bbox[1] for ln in wide]),
            key=lambda ln: (ln.bbox[1], ln.bbox[0]),
        )
    )
    ordered_starts = sorted(starts, key=lambda ln: ln.bbox[1])
    tops = [st.bbox[1] - 1.0 for st in ordered_starts]
    bounds = sorted({round(t, 1) for t in tops} | {round(y, 1) for y in rules})
    if not bounds:
        bounds = [min(ln.bbox[1] for ln in lines) - 1.0]
    if bounds and bounds[0] > min(ln.bbox[1] for ln in lines) - 1.0:
        bounds.insert(0, min(ln.bbox[1] for ln in lines) - 1.0)

    def band(line: Line) -> int:
        for st in ordered_starts:
            if st is not line and _overlap_ratio(st.bbox, line.bbox) >= 0.5:
                return band_of_top(st.bbox[1])
        return band_of_top((line.bbox[1] + line.bbox[3]) / 2)

    def band_of_top(y: float) -> int:
        idx = 0
        for i, top in enumerate(bounds):
            if y >= top:
                idx = i
        return idx

    cells: list[Block] = []
    for group in groups:
        by_band: dict[int, list[Line]] = {}
        for line in group:
            by_band.setdefault(band(line), []).append(line)
        for members in by_band.values():
            members.sort(key=lambda ln: (ln.bbox[1], ln.bbox[0]))
            bbox = members[0].bbox
            for ln in members[1:]:
                bbox = _union(bbox, ln.bbox)
            cells.append(Block(bbox, members))
    return sorted(cells, key=lambda c: (band(c.lines[0]), c.bbox[0], c.bbox[1]))


def _row_tops(groups: list[list[Line]], tol: float = 2.0, fresh_only: bool = False) -> list[float]:
    rows: list[tuple[Bbox, set[int]]] = []
    for gi, group in enumerate(groups):
        for line in group:
            if fresh_only and _tight_below(line, group):
                continue
            for i, (seed, members) in enumerate(rows):
                if _overlap_ratio(seed, line.bbox) >= 0.5 or abs(line.bbox[1] - seed[1]) <= tol:
                    rows[i] = (seed, members | {gi})
                    break
            else:
                rows.append((line.bbox, {gi}))
    return [seed[1] for seed, members in rows if len(members) >= 2]


def _dedupe_tops(starts: list[Line], tol: float = 2.0) -> list[Line]:
    kept: list[Line] = []
    for st in starts:
        if kept and abs(st.bbox[1] - kept[-1].bbox[1]) <= tol:
            continue
        kept.append(st)
    return kept


def _spans_groups(line: Line, others: list[list[Line]]) -> bool:
    hit: list[Bbox] = []
    for group in others:
        for member in group:
            shared = min(member.bbox[2], line.bbox[2]) - max(member.bbox[0], line.bbox[0])
            if shared >= 0.5 * max(1e-6, member.bbox[2] - member.bbox[0]):
                hit.append((min(m.bbox[0] for m in group), 0.0, max(m.bbox[2] for m in group), 0.0))
                break
    families: list[Bbox] = []
    for h in sorted(hit, key=lambda h: h[0]):
        if families and min(families[-1][2], h[2]) > max(families[-1][0], h[0]):
            families[-1] = _union(families[-1], h)
        else:
            families.append(h)
    return len(families) >= 2


def _edge_count(lines: list[Line], x: float, side: int, tol: float = 3.0) -> int:
    return sum(1 for ln in lines if abs(ln.bbox[side] - x) <= tol)


def join_split_lines(lines: list[Line]) -> list[Line]:
    merged: dict[int, Line] = {}
    used: set[int] = set()
    order = sorted(range(len(lines)), key=lambda i: (lines[i].bbox[1], lines[i].bbox[0]))
    for k, i in enumerate(order):
        left = merged.get(id(lines[i]), lines[i])
        if left.vertical or id(lines[i]) in used:
            continue
        for j in order[k + 1 :]:
            right = lines[j]
            if right.bbox[1] - left.bbox[1] > 1.5:
                break
            if right.vertical or id(right) in used or abs(right.bbox[3] - left.bbox[3]) > 1.5:
                continue
            gap = right.bbox[0] - left.bbox[2]
            size = max(left.font_size, right.font_size, 1.0)
            if gap < 0 or gap > SPLIT_JOIN_EM * size:
                continue
            if (
                _edge_count(lines, left.bbox[2], 2) >= 3
                or _edge_count(lines, right.bbox[0], 0) >= 3
            ):
                break
            if _lone_number(right) or _lone_number(left):
                break
            used.add(id(right))
            left = _line_from(left.chars + right.chars)
            merged[id(lines[i])] = left
    return [merged.get(id(ln), ln) for ln in lines if id(ln) not in used]


def _glyphs(line: Line) -> int:
    return sum(1 for c in line.chars if not c.text.isspace())


def _line_size(line: Line) -> float:
    """A line's type size as the median glyph height, so a bullet, an operator or a drop cap on the
    line does not stand for the whole row (the line's `font_size` is the maximum)."""
    heights = sorted(c.y1 - c.y0 for c in line.chars if not c.text.isspace())
    return heights[len(heights) // 2] if heights else line.font_size


def _fragment_host(frag: Bbox, host: Bbox, size: float, small: bool, stacked: bool = False) -> bool:
    """A fragment box shares a band with the host row (their vertical extents overlap by at least
    half the shorter of the two, so the next line's first words, grazing the row by a point or two,
    stay where they are; `stacked` — a script of the host's own backend block that clears the
    band by up to its own height — is accepted only by callers that can tell it from a hanging
    number, none today) and either
    abuts one end of it within FRAGMENT_GAP em or, when `small`, lies over the host's glyphs
    (their x-extents overlap), both of them one row tall (FRAGMENT_ROW
    of the size, so a row carrying an operator or a script still counts): a sub/superscript, a
    limit set above or below an operator, or a piece the band test dropped off the baseline. A
    fragment in the host's own size must abut one end of the row (`beside`); only a `small` one
    may lie over the row's glyphs. A margin note set small beside the row but a word-space away is
    neither. A two-row cell is never a host and never a
    fragment; a box under
    FRAGMENT_MIN of the size (pdfium's punctuation boxes) is not a glyph fragment."""
    gap = FRAGMENT_GAP * size
    overlap = min(frag[3], host[3]) - max(frag[1], host[1])
    shared = overlap >= 0.5 * min(frag[3] - frag[1], host[3] - host[1]) or (
        stacked and overlap >= -(frag[3] - frag[1])
    )
    over = frag[0] < host[2] and frag[2] > host[0]
    beside = abs(frag[0] - host[2]) <= gap or abs(frag[2] - host[0]) <= gap
    one_row = host[3] - host[1] <= FRAGMENT_ROW * size and frag[3] - frag[1] <= FRAGMENT_ROW * size
    reach = SCRIPT_REACH * (host[3] - host[1])
    within = stacked or (frag[1] >= host[1] - reach and frag[3] <= host[3] + reach)
    return shared and one_row and ((small and over and within) or beside)


def _piece(frag: Line | None, glyphs: int, host_glyphs: int, small: bool) -> bool:
    """A candidate fragment: short (≤ FRAGMENT_GLYPHS) or small, or a same-size row piece no longer
    than its host (pymupdf and the band test both cut a row at a script; the shorter piece joins
    the longer)."""
    return glyphs <= FRAGMENT_GLYPHS or small or glyphs <= host_glyphs


def join_inline_fragments(lines: list[Line]) -> list[Line]:
    """Not in the pdfium pipeline (measured, withdrawn): on tiers 2+3 joining even small fragments
    removed `high_entropy_table` markers from chemistry and drug-table pages and raised
    `missing_anchors` (st5_lt_chem_kinetics 0 → 18) while only `inline_splits` fell; kept as the
    line-level twin of `fold_fragments` for the fixture and a future gate. A small line
    (≤ FRAGMENT_SIZE of the host's size) that `_fragment_host` places on a longer line joins
    it after the host glyphs that start left of it (never inside a host word) and the row keeps
    its own font size, so the block it belongs to does not
    break on the fragment's size. Same-size row pieces are not joined here: on the chapter tiers
    that join moved binding counters the wrong way (tables and exercise layouts glued into rows),
    so the layer keeps `join_split_lines` for them. Rows holding RTL text are left to the bidi
    rules (ING-6)."""
    hosts = [ln for ln in lines if not ln.vertical and _glyphs(ln) > FRAGMENT_GLYPHS]
    merged: dict[int, Line] = {}
    used: set[int] = set()
    for frag in sorted(lines, key=_glyphs):
        if frag.vertical or bidi.has_rtl(frag.text) or id(frag) in merged:
            continue
        best: Line | None = None
        for host in hosts:
            row = merged.get(id(host), host)
            size, frag_size = _line_size(row), _line_size(frag)
            small = frag_size <= FRAGMENT_SIZE * size
            if host is frag or not small or frag_size < FRAGMENT_MIN * size:
                continue
            if bidi.has_rtl(row.text):
                continue
            if not _fragment_host(frag.bbox, row.bbox, size, small):
                continue
            if best is None or _mid(row.bbox) < _mid(best.bbox):
                best = host
        if best is None:
            continue
        row = merged.get(id(best), best)
        used.add(id(frag))
        at = sum(1 for c in row.chars if c.x0 < frag.bbox[0])
        joined = _line_from(row.chars[:at] + frag.chars + row.chars[at:])
        merged[id(best)] = Line(joined.bbox, joined.text, row.font_size, joined.chars)
    return [merged.get(id(ln), ln) for ln in lines if id(ln) not in used]


def _shape_rows(
    raw: Sequence[RawBlock], words: Sequence[RawWord]
) -> dict[tuple[int, int], list[RawWord]]:
    rows: dict[tuple[int, int], list[RawWord]] = {}
    for w in words:
        if not w[4].strip() or _tall(w):
            continue
        block = _block_of(raw, w)
        if block is not None:
            rows.setdefault((block, w[6]), []).append(w)
    return rows


def _block_of(raw: Sequence[RawBlock], w: RawWord) -> int | None:
    """Of the raw blocks whose box holds the word's centre, those whose text carries the token
    (boxes overlap where a script or a row piece sits over a paragraph, or where two paragraphs'
    boxes overlap by a line: the block that spells the word owns it), then the one whose vertical
    centre is nearest the word's, then the smaller; the backend's block numbers in its word list
    need not index its block list. When no box holds it (pymupdf's block box can
    exclude a stacked superscript line), a box within one word height, the word's own block number
    when that is one of them.
    A backend's own block numbers need not index its block list (pymupdf renumbers when sorting
    and skips image blocks)."""
    cx, cy = (w[0] + w[2]) / 2, (w[1] + w[3]) / 2
    inside = [
        k for k, (box, _, _) in enumerate(raw) if box[0] <= cx <= box[2] and box[1] <= cy <= box[3]
    ]
    if inside:
        carrying = [k for k in inside if w[4] in raw[k][1].split()]
        return min(
            carrying or inside,
            key=lambda k: (
                abs(cy - (raw[k][0][1] + raw[k][0][3]) / 2),
                (raw[k][0][2] - raw[k][0][0]) * (raw[k][0][3] - raw[k][0][1]),
            ),
        )
    slack = w[3] - w[1]
    near = [
        k
        for k, (box, _, _) in enumerate(raw)
        if box[0] - slack <= cx <= box[2] + slack and box[1] - slack <= cy <= box[3] + slack
    ]
    if not near:
        return None
    return w[5] if w[5] in near else near[0]


def _row_box(ws: Sequence[RawWord]) -> Bbox:
    return (
        min(w[0] for w in ws),
        min(w[1] for w in ws),
        max(w[2] for w in ws),
        max(w[3] for w in ws),
    )


def _row_size(ws: Sequence[RawWord]) -> float:
    """A row's type size read from its own words (the median word height), not from the block's
    maximum, so a drop cap or a display operator in the block does not size every row and one
    script-bearing word does not size its own."""
    heights = sorted(w[3] - w[1] for w in ws)
    return heights[len(heights) // 2]


def _line_index(lines: Sequence[str], row: Sequence[RawWord], hint: int) -> int | None:
    """The block text line that carries a row's own words: the line sharing the most tokens with
    the row's non-folded words, the backend's line number breaking ties. Line numbers alone drift
    when a block's text has more or fewer lines than the backend's word list (empty lines, a list
    and a heading in one block)."""
    own = {w[4] for w in row if w[5] != FOLDED}
    if not own or not lines:
        return hint if 0 <= hint < len(lines) else None
    best, score = None, (-1, 0)
    for i, line in enumerate(lines):
        hits = sum(1 for t in line.split() if t in own)
        key = (hits, -abs(i - hint))
        if key > score:
            best, score = i, key
    return best if score[0] > 0 else (hint if 0 <= hint < len(lines) else None)


def _dropped_line(lines: Sequence[str], row: Sequence[RawWord], hint: int) -> int | None:
    """The text line a folded-away row leaves behind: the line spelling exactly the row's tokens,
    nearest the backend's line number. Anything looser deletes prose — a lone script `m` scored by
    token hits took "Input: integer array F ∈ Z256, where m = 2^d … m = q" with it (NIST ML-KEM
    p19). With no exact line, nothing is deleted: a glyph shown twice is a smaller fault than a
    line lost."""
    tokens = [w[4] for w in row]
    hits = [i for i, line in enumerate(lines) if line.split() == tokens]
    return min(hits, key=lambda i: abs(i - hint)) if hits else None


def _heads_longer_line(lines: Sequence[str], row: Sequence[RawWord]) -> bool:
    """The row's tokens are a strict prefix of one of the block's text lines: the line's remaining
    words belong to the same backend row but were assigned elsewhere. Such a row is not a piece
    (`fold_fragments`). Rows that differ from every line by tokenisation alone are untouched."""
    tokens = [w[4] for w in row]
    return any(
        len(line.split()) > len(tokens) and line.split()[: len(tokens)] == tokens for line in lines
    )


def _with_pieces(original: str, row: Sequence[RawWord]) -> str:
    """The backend's own row text with the folded pieces (the words tagged FOLDED before the
    renumbering) inserted into it; the host's spelling, spacing and ligatures stay exactly as the
    backend emitted them. Invariant: the host's tokens keep their order, and a piece goes before the
    earliest token of any host word that follows it in the row — never after a token whose word
    stands right of the piece. The backend's word list and its text can disagree in order (pymupdf
    lists a fraction's denominator after the words that follow it on the baseline, though its text
    spells it first): a piece left of the whole row then still precedes the denominator. Host words
    the word list does not carry (zero-width boxes) are skipped, never dropped from the text; pieces
    after the last matched word go to the end."""
    tokens = original.split()
    taken: set[int] = set()
    slots: list[int | None] = []
    k = 0
    for w in row:
        if w[5] == FOLDED:
            slots.append(None)
            continue
        j = next((i for i in range(k, len(tokens)) if tokens[i] == w[4] and i not in taken), None)
        if j is None:
            j = next((i for i in range(len(tokens)) if tokens[i] == w[4] and i not in taken), None)
        if j is not None:
            taken.add(j)
            k = max(k, j + 1)
        slots.append(j)
    before: dict[int, list[str]] = {}
    tail: list[str] = []
    for i, w in enumerate(row):
        if w[5] != FOLDED:
            continue
        later = [j for j in slots[i + 1 :] if j is not None]
        # Pieces standing before every matched host word (the row's own text cut off at a script
        # by the backend: `Heat generation … (i.e.` before `I²R), but rather`) go to the very start,
        # ahead of host tokens the word list does not carry.
        first = not any(j is not None for j in slots[:i])
        at = 0 if first and later else (min(later) if later else None)
        (before.setdefault(at, []) if at is not None else tail).append(w[4])
    out: list[str] = []
    lead = before.get(0, [])
    for j, tok in enumerate(tokens):
        out.extend(before.get(j, []))
        # A token the word list does not carry, standing first in the text right after leading
        # pieces and spelling the tail of the last piece, is the backend's second copy of that glyph
        # (pymupdf emits `Banks,` as a piece and `, packs` as the line): drop the copy, never a
        # word.
        if (
            j == 0
            and lead
            and 0 not in taken
            and lead[-1].endswith(tok)
            and len(tok) < len(lead[-1])
        ):
            continue
        out.append(tok)
    out.extend(tail)
    return " ".join(out)


def _host_slot(piece: Sequence[RawWord], host: Sequence[RawWord]) -> int:
    """Index in `host` after which a script piece is inserted: after the host word whose x-range
    overlaps the piece's most (the token it is set on or over — a stacked script starts at its
    base's right edge, so it follows the token it overlaps), or the nearest word by x within the
    piece's own width, before that word when the piece's centre lies left of the word's centre.
    A piece never precedes the row's first word unless that word is its host and the piece sits
    left of it."""
    px0, px1 = min(w[0] for w in piece), max(w[2] for w in piece)
    pc = (px0 + px1) / 2
    best, score = None, None
    for i, w in enumerate(host):
        overlap = min(px1, w[2]) - max(px0, w[0])
        dist = 0.0 if overlap > 0 else min(abs(px0 - w[2]), abs(w[0] - px1))
        key = (-(overlap > 0), dist if overlap <= 0 else -overlap)
        if score is None or key < score:
            best, score = i, key
    if best is None:
        return 0
    w = host[best]
    if score is not None and score[0] == 0 and score[1] > px1 - px0:
        return sum(1 for x in host if x[0] < px0)
    if score is not None and score[0] < 0:
        # A script is set at its base's right edge, so it follows the token it overlaps — unless
        # the piece starts at or left of that token's own start: the token cannot be its base (it
        # is a script itself, glued to the line by the backend), and the two read top-down: the
        # higher precedes (typst `{(w, ŷ)}^M_{j=1}`: `M` before `j=1,`).
        py = (min(x[1] for x in piece) + max(x[3] for x in piece)) / 2
        above = py < (w[1] + w[3]) / 2
        if px0 <= w[0] + 1.0 and above:
            return best
        # A piece set above a token and wholly inside its span (not hanging off its right edge as a
        # superscript does), over a token of two or more glyphs, is a numerator over its
        # denominator and reads first (`1/rank_m`, `ε/|M|`); a limit over a one-glyph operator and
        # a superscript keep following their base.
        if above and px0 > w[0] + 1.0 and px1 <= w[2] - 1.0 and len(w[4]) >= 2:
            return best
        return best + 1
    return best + 1 if pc >= (w[0] + w[2]) / 2 else best


def fold_fragments(
    raw: Sequence[RawBlock], words: Sequence[RawWord]
) -> tuple[list[RawBlock], list[RawWord]]:
    """`join_inline_fragments` for a backend's own blocks: a row that `_piece` and
    `_fragment_host` place on a longer row joins it — a one-row block (a same-size piece beside
    the row, or a script over it) or a short, script-sized row inside a multi-row block set off
    the host's centre by SCRIPT_OFFSET (a script the backend set as its own line; a lone maths
    glyph on the baseline, shorter than the text but centred on its row, and a bullet keep their
    place in their block); a small piece lying over a row's glyphs, from any block, is likewise a
    script only when it sits off the row's centre (a diagram label of a slide crossing a text row's
    band is centred on it and stays its own block). A piece carries at least one alphanumeric
    glyph: rows of replacement or punctuation glyphs alone (a slide's `?` markers for glyphs the
    layer could not map) are not scripts and stay where the backend put them. A script piece is
    inserted at `_host_slot` (its rendered host token) when it is small and set off the row's
    centre by SCRIPT_OFFSET of the row height (a script is raised or lowered; a small piece on the
    baseline — a section number, a marker — is a word and takes the x-count), in a second pass once
    every same-size piece has joined its row, so a script whose host token sat in a neighbouring
    piece finds it; a same-size piece, short or long, after the words starting left of it. The host
    row's own text is kept and the pieces are placed into it (`_with_pieces`).
    Rows holding RTL text and rotated (tall) words are left alone. Block numbers in the words
    are renumbered; a block whose every row folded away is dropped."""
    rows = _shape_rows(raw, words)
    by_block: dict[int, list[int]] = {}
    for b, ln in rows:
        by_block.setdefault(b, []).append(ln)
    hosts = [
        (b, ln)
        for (b, ln), ws in rows.items()
        if sum(len(w[4]) for w in ws) > FRAGMENT_GLYPHS and not bidi.has_rtl(raw[b][1])
    ]
    folded: dict[tuple[int, int], tuple[int, int, bool]] = {}
    dropped: dict[tuple[int, int], list[RawWord]] = {}
    for scripts in (False, True):
        for (k, kln), ws in list(rows.items()):
            if (k, kln) in folded or bidi.has_rtl(raw[k][1]):
                continue
            glyphs = sum(len(w[4]) for w in ws)
            one_row = len(by_block.get(k, [])) == 1
            if not one_row and glyphs > FRAGMENT_GLYPHS:
                continue
            if not any(c.isalnum() for w in ws for c in w[4]):
                continue
            if _heads_longer_line(raw[k][1].split("\n"), ws):
                # A piece is emitted exactly once: a row that spells only the head of one of its
                # block's text lines has lost the line's other words to a neighbouring block (typst
                # 2608 p16: `2 for all` under a line ending "such that for all" leaves the row `2`
                # alone) and stays where the backend put it — folding the `2` would show it twice,
                # deleting the line would lose "for all".
                continue
            box = _row_box(ws)
            size = _row_size(ws)
            best: tuple[int, int, bool] | None = None
            for b, ln in hosts:
                if (b, ln) == (k, kln) or (b, ln) in folded:
                    continue
                host_words = rows[(b, ln)]
                host = _row_box(host_words)
                host_size = _row_size(host_words)
                small = size <= FRAGMENT_SIZE * host_size
                script = small or glyphs <= FRAGMENT_GLYPHS
                if script != scripts or size < FRAGMENT_MIN * host_size:
                    continue
                raised = abs(_mid(box) - _mid(host)) >= SCRIPT_OFFSET * (host[3] - host[1])
                gap = FRAGMENT_GAP * host_size
                beside = abs(box[0] - host[2]) <= gap or abs(box[2] - host[0]) <= gap
                if not one_row and not (small and raised):
                    continue
                if small and not beside and not raised:
                    continue
                if small and _over_glyphs(ws, host_words):
                    continue
                host_rows = len(by_block.get(b, []))
                if not _piece(None, glyphs, sum(len(w[4]) for w in host_words), small) and not (
                    one_row and host_rows > 1
                ):
                    continue
                if _fragment_host(box, host, host_size, small) and (
                    best is None
                    or abs(_mid(host) - _mid(box)) < abs(_mid(_row_box(rows[best[:2]])) - _mid(box))
                ):
                    best = (b, ln, script)
            if best is None:
                continue
            folded[(k, kln)] = best
            frag = rows.pop((k, kln))
            dropped[(k, kln)] = frag
            b, ln, script = best
            host_row = rows[(b, ln)]
            fbox, hbox = _row_box(frag), _row_box(host_row)
            raised = abs(_mid(fbox) - _mid(hbox)) >= SCRIPT_OFFSET * (hbox[3] - hbox[1])
            small = _row_size(frag) <= FRAGMENT_SIZE * _row_size(host_row)
            at = (
                _host_slot(frag, host_row)
                if small and raised
                else sum(1 for w in host_row if w[0] < frag[0][0])
            )
            rows[(b, ln)] = (
                host_row[:at]
                + [(w[0], w[1], w[2], w[3], w[4], FOLDED, ln) for w in frag]
                + host_row[at:]
            )
    if not folded:
        return list(raw), list(words)
    out_blocks: list[RawBlock] = []
    out_words: list[RawWord] = []
    for k, (bbox, text, size) in enumerate(raw):
        kept = [ln for ln in sorted(by_block.get(k, [])) if (k, ln) not in folded]
        if by_block.get(k) and not kept:
            continue
        new_no = len(out_blocks)
        lines = text.split("\n")
        # The folded row's own words find the text line they leave behind (`_dropped_line`, exact
        # match); the backend's line number alone pointed at another line of the block and deleted
        # it (ocw 8.04 p3: a lone `ℏ` row numbered 2 took the block's third text line with it).
        gone = {
            _dropped_line(lines, dropped[(k, ln)], ln)
            for ln in by_block.get(k, [])
            if (k, ln) in folded
        }
        gone.discard(None)
        for ln in kept:
            if any(f[:2] == (k, ln) for f in folded.values()):
                i = _line_index(lines, rows[(k, ln)], ln)
                if i is not None:
                    lines[i] = _with_pieces(lines[i], rows[(k, ln)])
        lines = [line for i, line in enumerate(lines) if i not in gone]
        # The block's box covers its same-size row pieces: a piece that reached across a gap between
        # the host and the block beside it closes that gap, so the cut does not read the two halves
        # of one line as two columns (typst 2604 p16: `Under calibration D²` | `_i ∼ χ²_d, and`).
        # Scripts do not grow the box: a limit under an operator is not a bridge to the next column
        # (fenics deck p9).
        for ln in kept:
            host_size = _row_size([w for w in rows[(k, ln)] if w[5] != FOLDED] or rows[(k, ln)])
            for w in rows[(k, ln)]:
                if w[5] == FOLDED and w[3] - w[1] > FRAGMENT_SIZE * host_size:
                    bbox = _union(bbox, (w[0], w[1], w[2], w[3]))
        out_blocks.append((bbox, "\n".join(lines), size))
        for ln in kept:
            out_words.extend((w[0], w[1], w[2], w[3], w[4], new_no, w[6]) for w in rows[(k, ln)])
    return out_blocks, out_words


def _over_glyphs(piece: Sequence[RawWord], host: Sequence[RawWord]) -> bool:
    """A small piece drawn over the middle of a host word — its x-range holds the word's centre and
    its band overlaps the word's by at least half the piece's height — is text laid over the row
    (a hidden duplicate label under a slide's visible one), not a script: a script touches its base
    at the base's right edge only, and a numerator stands clear above its denominator. The word must
    be a real word — two or more glyphs with a letter or digit, at least as wide as the piece — so a
    comma or period under a subscript's box does not stand for one — and the piece must cover the
    word's left third: a script hangs on its base's right half (NIST ML-KEM p19: `ℤ^256_m`, the
    subscript under `56`), a duplicate label starts where the word starts."""
    px0, px1 = min(w[0] for w in piece), max(w[2] for w in piece)
    top, bottom = min(w[1] for w in piece), max(w[3] for w in piece)
    for w in host:
        cx = (w[0] + w[2]) / 2
        wide = w[2] - w[0] >= px1 - px0 and len(w[4]) >= 2 and any(c.isalnum() for c in w[4])
        left = px0 <= w[0] + 0.3 * (w[2] - w[0])
        if (
            wide
            and left
            and px0 < cx < px1
            and min(bottom, w[3]) - max(top, w[1]) >= 0.5 * (bottom - top)
        ):
            return True
    return False


def _mid(box: Bbox) -> float:
    return (box[1] + box[3]) / 2


def _walled(a: Bbox, b: Bbox, shapes: Sequence[Shape]) -> bool:
    top, bottom = min(a[1], b[1]), max(a[3], b[3])
    return any(
        kind == "vline" and a[2] - 1 <= box[0] <= b[0] + 1 and box[1] <= top and box[3] >= bottom
        for kind, box in shapes
    )


def join_script_runs(lines: list[Line], shapes: Sequence[Shape] = ()) -> list[Line]:
    upright = sorted(
        (ln for ln in lines if not ln.vertical and ln.text), key=lambda ln: (ln.bbox[0], ln.bbox[1])
    )
    merged: dict[int, Line] = {}
    used: set[int] = set()
    for k, ln in enumerate(upright):
        if id(ln) in used:
            continue
        row = merged.get(id(ln), ln)
        for other in upright[k + 1 :]:
            size = max(row.font_size, other.font_size, 1.0)
            apart = abs(_mid(row.bbox) - _mid(other.bbox)) > 0.5 * size
            if id(other) in used or apart:
                continue
            if other.bbox[0] - row.bbox[2] > CHAR_MARGIN * size:
                break
            if not (bidi.has_rtl(row.text) or bidi.has_rtl(other.text)):
                continue
            if _walled(row.bbox, other.bbox, shapes):
                break
            gutter = other.bbox[0] - row.bbox[2] >= GUTTER_GAP_EM * size
            left = _edge_count(lines, row.bbox[2], 2) >= 3
            right = _edge_count(lines, other.bbox[0], 0) >= 3
            if gutter and left and right:
                continue
            used.add(id(other))
            row = _line_from(row.chars + other.chars)
            merged[id(ln)] = row
    return [merged.get(id(ln), ln) for ln in lines if id(ln) not in used]


def _internal_gaps(line: Line) -> list[tuple[float, float, int]]:
    gaps: list[tuple[float, float, int]] = []
    prev_end: float | None = None
    for i, ch in enumerate(line.chars):
        if ch.text.isspace():
            continue
        if prev_end is not None and ch.x0 - prev_end >= GUTTER_GAP_EM * max(line.font_size, 1.0):
            gaps.append((prev_end, ch.x0, i))
        prev_end = ch.x1
    return gaps


def _band_crossed(lines: list[Line], lo: float, hi: float, members: list[tuple[Line, int]]) -> bool:
    top = min(ln.bbox[1] for ln, _ in members)
    bottom = max(ln.bbox[3] for ln, _ in members)
    ids = {id(ln) for ln, _ in members}
    for ln in lines:
        if id(ln) in ids or ln.bbox[3] < top or ln.bbox[1] > bottom:
            continue
        spans = ln.bbox[0] < lo - 1 and ln.bbox[2] > hi + 1
        if spans and not any(
            g_lo <= lo + 1 and g_hi >= hi - 1 for g_lo, g_hi, _ in _internal_gaps(ln)
        ):
            return True
    return False


def split_at_gutters(lines: list[Line]) -> list[Line]:
    upright = [ln for ln in lines if not ln.vertical]
    bands: list[tuple[float, float, list[tuple[Line, int]]]] = []
    for ln in upright:
        for lo, hi, idx in _internal_gaps(ln):
            for k, (blo, bhi, members) in enumerate(bands):
                if max(lo, blo) < min(hi, bhi):
                    bands[k] = (max(lo, blo), min(hi, bhi), members + [(ln, idx)])
                    break
            else:
                bands.append((lo, hi, [(ln, idx)]))
    cuts: dict[int, set[int]] = {}
    for lo, hi, members in bands:
        if _band_crossed(upright, lo, hi, members):
            continue
        substantial = [
            (ln, idx)
            for ln, idx in members
            if sum(1 for c in ln.chars[:idx] if c.text.isalnum()) >= 6
            and sum(1 for c in ln.chars[idx:] if c.text.isalnum()) >= 2
        ]
        if len(substantial) >= GUTTER_LINES and len(substantial) * 10 >= 7 * len(members):
            for ln, idx in members:
                cuts.setdefault(id(ln), set()).add(idx)
    out: list[Line] = []
    for ln in lines:
        idxs = sorted(cuts.get(id(ln), ()))
        if not idxs:
            out.append(ln)
            continue
        start = 0
        for idx in idxs + [len(ln.chars)]:
            piece = ln.chars[start:idx]
            if any(not c.text.isspace() for c in piece):
                out.append(_line_from(piece, ln.vertical))
            start = idx
    return out
