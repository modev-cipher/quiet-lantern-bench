"""pypdfium2 implementation of the `Ingestor` seam (ING-1: C1, C3, C6–C9, C11).

API facts taken from the installed package (pypdfium2 5.12.0, PDFium build 7947,
introspected 2026-09-03) and https://pypdfium2.readthedocs.io/en/stable/python_api.html:
`PdfDocument(path)` raises `PdfiumError(err_code)` — 4 `FPDF_ERR_PASSWORD`, 3
`FPDF_ERR_FORMAT` — and `FileNotFoundError` for a missing path; `PdfPage.get_size()` is the
displayed (post-`/Rotate`) size; `PdfTextPage.get_charbox(i)` is (left, bottom, right, top)
in the *unrotated* PDF frame, so every box goes through `FPDF_PageToDevice`, PDFium's own
page→display mapping, to reach the top-left frame the rest of the engine uses;
`FPDFText_GetUnicode` / `FPDFText_HasUnicodeMapError` / `FPDFText_IsGenerated` are the
per-char facts §3.4 keys on (PDFium inserts CR/LF and space chars and counts them).
Measured on the gap fixtures: PDFium reports a map error for every Type3 char decoded
through standard glyph names although the code points are right, so the flag is kept as
the per-page `unicode_map_errors` counter and U+FFFD is substituted only for code point 0,
C0 controls and lone surrogates — what an Identity-encoded CID font without ToUnicode
actually degrades to. The counter's threshold is ING-3's decision, not this adapter's.

ING-5 (ruling #48, per-page font decoding fallbacks): a flagged char is first re-decoded from
the page's own /Encoding /Differences through the glyph list (`glyphnames.py`), because pdfium
returns the raw charcode when a simple font's ToUnicode lacks the code (MathType symbol fonts
in TeX-typeset books; Ghostscript Type 3 re-renders); what stays undecodable is counted, and a
page that shows text (`textobjects.py`, the lexed BT..ET rule the stress census shares) yet
keeps fewer than `DECODABLE_FLOOR` of its glyphs carries the `undecodable_text` marker. Type 3
fonts also break `FPDFText_GetFontSize`: with a non-standard /FontMatrix (`[1 0 0 -1]` from
dvips/Ghostscript bitmaps, `.00488` from Ghostscript html prints) the `Tf` operand is in glyph
space (`Tf 0.24` for a 6 pt glyph), so on a page carrying such a font (`FontMap.glyph_space_size`)
a glyph whose loose box is more than `TYPE3_SIZE_RATIO` times taller than that size takes the
box height as its size; Type 3 fonts with the standard 0.001 matrix (MathJax glyphs) are left
alone. Without it every glyph opened its own line and the layer spent minutes on one-glyph blocks.

`FPDF_GetLastError` is process-global and a zero-page load fails without setting it, so a
password code (4) is trusted only when the file structurally declares encryption: the last
`startxref` (read from a 2 KB tail) locates either the `trailer` dictionary (ISO 32000-1
§7.5.5) or the cross-reference stream dictionary `/Type /XRef` (§7.5.8), `/Encrypt` must be a
top-level key of that dictionary (§7.6.1), and `/Prev` is followed at most `MAX_PREV_HOPS`
times. Entries come from a §7.3.7 key/value walk (`_pdf_dict_entries`), so `/Encrypt` inside
a comment, literal or hex string, nested dictionary or array never counts, and at most
`TAIL_BYTES + DICT_BYTES × (MAX_PREV_HOPS + 1)` bytes are read. pypdfium2
cannot answer this itself: `FPDF_LoadDocument` returns no handle on a password failure, so
`FPDF_GetDocPermissions` / metadata are unavailable exactly when the question is asked.

Text (C4/C5) is walked char by char — *loose* char boxes (font ascent/descent, not glyph
ink) because that is the box the native engine's word rects and the follow-along highlight
were measured against — and handed to the deterministic static text layer in
`textlayer.py` (ING-2): baseline clustering into lines, gap/indent/size clustering into
blocks, recursive XY-cut for columns, top-down left-right reading order. The shapes that
come back are the native adapter's `RawBlock` / `RawWord` shapes exactly.
"""

import ctypes
import io
import logging
import math
import re
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from typing import BinaryIO

import pypdfium2 as pdfium
import pypdfium2.raw as pdfium_c

from txtcaster_engine.ingest.errors import IngestError
from txtcaster_engine.ingest.glyphnames import unicode_for
from txtcaster_engine.ingest.ingestor import Bbox, PageText, RawBlock
from txtcaster_engine.ingest.pdf_resources import (
    FontMap,
    PdfObjects,
    page_fonts,
    page_streams,
    pattern_image_count,
)
from txtcaster_engine.ingest.scanned import is_scanned_page
from txtcaster_engine.ingest.textlayer import (
    Char,
    Shape,
    group_lines,
    join_lone_numbers,
    join_script_runs,
    join_split_lines,
    linear_order,
    reading_order,
    split_at_gutters,
    structure,
    to_page_shapes,
    xy_cut,
)
from txtcaster_engine.ingest.textobjects import (
    DECODABLE_FLOOR,
    glyph_counts,
    is_undecodable_text,
    text_objects_in,
)

ARTIFACT_MARK = "Artifact"
ARTIFACT_TOLERANCE = 1.0
DEVICE_UNITS_PER_POINT = 100
FONT_NAME_BYTES = 256
TYPE3_SIZE_RATIO = 2.0
INK_ABUTTING_EM = 0.03
INK_ADVANCE_SHARE = 0.2
INK_MIN_PAIRS = 20
INK_MIN_SPACES = 5
INK_BIN_EM = 0.02
INK_VALLEY_BINS = 3
INK_VALLEY_SHARE = 0.02
INK_MODE_MAX_EM = 0.5
INK_SPACE_PERCENTILE = 0.05
INK_SPACE_FALLBACK_EM = 0.15
INK_ROW_OVERLAP = 0.5
REPLACEMENT_CHAR = "�"
HYPHEN_MARKER = 0x02


def engine_identity() -> str:
    return f"pdfium={pdfium.PDFIUM_INFO.build}|pypdfium2={pdfium.PYPDFIUM_INFO.version}"


@dataclass
class _Char:
    x0: float
    y0: float
    x1: float
    y1: float
    text: str
    font_size: float
    generated: bool
    map_error: bool
    invisible: bool = False
    seq: int | None = None  # text-page char index (extractor sequence, provenance only)
    artifact: bool = False

    @property
    def is_space(self) -> bool:
        return self.text.isspace() or self.text == ""

    @property
    def is_line_break(self) -> bool:
        return self.text in ("\r", "\n")


class _Frame:
    """Maps unrotated PDF-frame points to the displayed top-left frame of one page."""

    def __init__(self, page: pdfium.PdfPage) -> None:
        self._raw = page.raw
        width, height = page.get_size()
        self.width = float(width)
        self.height = float(height)
        self._dev_w = int(round(self.width * DEVICE_UNITS_PER_POINT))
        self._dev_h = int(round(self.height * DEVICE_UNITS_PER_POINT))
        self._analytic: tuple[float, float, float, float] | None = None
        if page.get_rotation() == 0:
            left, bottom, right, top = page.get_bbox()
            if right > left and top > bottom:
                sx = _f32(self._dev_w / (right - left))
                sy = _f32(self._dev_h / (top - bottom))
                self._analytic = (sx, _f32(-left * sx), -sy, _f32(top * sy))

    def point(self, x: float, y: float) -> tuple[float, float]:
        if self._analytic is None:
            return self._point_slow(x, y)
        a, e, d, f = self._analytic
        fx = _f32(_f32(a * x) + e)
        fy = _f32(_f32(d * y) + f)
        return math.floor(fx + 0.5) / DEVICE_UNITS_PER_POINT, math.floor(
            fy + 0.5
        ) / DEVICE_UNITS_PER_POINT

    def _point_slow(self, x: float, y: float) -> tuple[float, float]:
        dx = ctypes.c_int()
        dy = ctypes.c_int()
        ok = pdfium_c.FPDF_PageToDevice(
            self._raw, 0, 0, self._dev_w, self._dev_h, 0, x, y, ctypes.byref(dx), ctypes.byref(dy)
        )
        if not ok:
            raise RuntimeError("FPDF_PageToDevice failed")
        return dx.value / DEVICE_UNITS_PER_POINT, dy.value / DEVICE_UNITS_PER_POINT

    def box(self, left: float, bottom: float, right: float, top: float) -> Bbox:
        ax, ay = self.point(left, top)
        bx, by = self.point(right, bottom)
        return (min(ax, bx), min(ay, by), max(ax, bx), max(ay, by))


def _f32(value: float) -> float:
    return ctypes.c_float(value).value


def _needs_replacement(code: int) -> bool:
    """The code points `_decode` turns into U+FFFD: C0 controls other than TAB/CR/LF and the
    line-end hyphen marker, lone surrogates, and anything past U+10FFFF. The same predicate
    feeds the per-page `unicode_map_errors` count, so every substitution is counted."""
    return (
        (code < 0x20 and code not in (0x09, 0x0A, 0x0D, HYPHEN_MARKER))
        or 0xD800 <= code <= 0xDFFF
        or code > 0x10FFFF
    )


def _decode(code: int) -> str:
    """§3.4 code-point normalisation (ING-1 narrowing kept, ING-2 hyphen marker added).

    PDFium's text page dehyphenates: a hyphen glyph that ends a line is replaced by the
    marker U+0002 and the line break after it is dropped (cpdf_textpage.cpp, `IsHyphen`),
    so `transduc-⏎tion` arrives as `transduc\x02tion`. The marker becomes HYPHEN-MINUS; the
    line boundary is recovered geometrically by the text layer because the next glyph sits
    on the next baseline. Native keeps the glyph's own code point there (`-`, or U+00AD for
    Word's soft hyphen), so soft-hyphen pages differ by that one code point and are recorded
    as such in the ING-2 note. Code point 0, other C0 controls, lone surrogates and
    out-of-range values become U+FFFD; the charbox is kept so geometry stays aligned.
    """
    if code in (0x09, 0x0D, 0x0A):
        return chr(code)
    if code == HYPHEN_MARKER:
        return "-"
    if code < 0x20 or 0xD800 <= code <= 0xDFFF or code > 0x10FFFF:
        return REPLACEMENT_CHAR
    return chr(code)


_INKLESS_RENDER_MODES = frozenset(
    {pdfium_c.FPDF_TEXTRENDERMODE_INVISIBLE, pdfium_c.FPDF_TEXTRENDERMODE_CLIP}
)


def _is_artifact(handle: object) -> bool:
    for index in range(pdfium_c.FPDFPageObj_CountMarks(handle)):
        mark = pdfium_c.FPDFPageObj_GetMark(handle, index)
        size = ctypes.c_ulong()
        if not pdfium_c.FPDFPageObjMark_GetName(mark, None, 0, ctypes.byref(size)):
            continue
        buffer = (ctypes.c_ushort * (size.value // 2 + 1))()
        pdfium_c.FPDFPageObjMark_GetName(mark, buffer, size.value, ctypes.byref(size))
        if bytes(buffer)[: size.value].decode("utf-16-le").rstrip("\x00") == ARTIFACT_MARK:
            return True
    return False


def _object_info(
    textpage_raw: object, index: int, cache: dict[int, tuple[float, bool, bool]]
) -> tuple[float, bool, bool]:
    """(y-scale of the char's text object matrix, object paints no ink, object is marked
    `/Artifact`), memoised per object handle — one `FPDFText_GetTextObject` lookup per char.

    `FPDFText_GetFontSize` is the raw `Tf` operand. Producers that set `Tf 1` and size the
    text through the text matrix (the NIST volumes: `Tf 1`, matrix 10.91) would otherwise
    report every glyph as 1 pt and collapse every font-relative tolerance in the text layer.
    Render mode 3 (invisible) or 7 (clip only) paints nothing: the OCR layer of a searchable
    scan is drawn this way (ING-12) — its glyph boxes are search anchors over the page image,
    not marks on the page.
    """
    handle = pdfium_c.FPDFText_GetTextObject(textpage_raw, index)
    key = ctypes.cast(handle, ctypes.c_void_p).value or 0
    if key == 0:
        return 1.0, False, False
    info = cache.get(key)
    if info is None:
        matrix = pdfium_c.FS_MATRIX()
        ok = pdfium_c.FPDFPageObj_GetMatrix(handle, ctypes.byref(matrix))
        scale = math.hypot(matrix.b, matrix.d) if ok else 1.0
        mode = pdfium_c.FPDFTextObj_GetTextRenderMode(handle)
        info = cache[key] = (
            scale if scale > 0 else 1.0,
            mode in _INKLESS_RENDER_MODES,
            _is_artifact(handle),
        )
    return info


def _font_name(textpage_raw: object, index: int) -> bytes:
    buffer = ctypes.create_string_buffer(FONT_NAME_BYTES)
    flags = ctypes.c_int(0)
    length = pdfium_c.FPDFText_GetFontInfo(
        textpage_raw, index, buffer, FONT_NAME_BYTES, ctypes.byref(flags)
    )
    name = buffer.raw[: max(int(length) - 1, 0)]
    return name.split(b"+", 1)[-1] if len(name) > 7 and name[6:7] == b"+" else name


def _by_glyph_name(code: int, font: bytes, fonts: Sequence[FontMap]) -> str | None:
    named = [f for f in fonts if f.base_name == font] if font else []
    found = {
        hit
        for f in named or fonts
        if (name := f.differences.get(code)) is not None
        if (hit := unicode_for(name)) is not None
    }
    return found.pop() if len(found) == 1 else None


def _walk_chars(
    textpage: pdfium.PdfTextPage,
    frame: _Frame,
    font_maps: Callable[[], Sequence[FontMap]] = lambda: (),
) -> list[_Char]:
    raw = textpage.raw
    chars: list[_Char] = []
    objects: dict[int, tuple[float, bool, bool]] = {}
    resolved: dict[tuple[bytes, int], str | None] = {}
    fonts: Sequence[FontMap] | None = None
    for index in range(textpage.count_chars()):
        code = pdfium_c.FPDFText_GetUnicode(raw, index)
        map_error = pdfium_c.FPDFText_HasUnicodeMapError(raw, index) == 1 or _needs_replacement(
            code
        )
        generated = pdfium_c.FPDFText_IsGenerated(raw, index) == 1
        text = _decode(code)
        if map_error and not generated:
            fonts = font_maps() if fonts is None else fonts
            key = (_font_name(raw, index), code)
            if key not in resolved:
                resolved[key] = _by_glyph_name(code, key[0], fonts) if fonts else None
            if resolved[key] is not None:
                text, map_error = str(resolved[key]), False
        try:
            left, bottom, right, top = textpage.get_charbox(index, loose=True)
        except pdfium.PdfiumError:
            left = bottom = right = top = 0.0
        x0, y0, x1, y1 = frame.box(left, bottom, right, top)
        scale, invisible, artifact = _object_info(raw, index, objects)
        font_size = float(pdfium_c.FPDFText_GetFontSize(raw, index)) * scale
        if y1 - y0 > TYPE3_SIZE_RATIO * font_size:
            fonts = font_maps() if fonts is None else fonts
            if any(f.glyph_space_size for f in fonts):
                font_size = y1 - y0
        chars.append(
            _Char(
                x0,
                y0,
                x1,
                y1,
                text,
                font_size,
                generated,
                map_error,
                invisible,
                seq=index,
                artifact=artifact,
            )
        )
    return chars


def _anchor_spaces(
    page: pdfium.PdfPage, textpage: pdfium.PdfTextPage, chars: list[_Char]
) -> list[_Char]:
    """ING-8: put pdfium's zero-width space back where the producer's own space object stands.

    Producers that write every glyph as its own text object (Chromium/Skia) write the space as
    one too. pdfium's text page drops that glyph and hands back a zero-width, size-1 space at an
    x of its own bookkeeping, one glyph or more away from the boundary, so the layer's position
    rule cuts the wrong pair. The page-object API still has the space object: a zero-width text
    object between the two glyph objects the space separates in the stream. Such a space is moved
    to that object's x, whether or not pdfium flags it generated (the flag does not track the
    producer's object). A zero-width space with no such object (pdfium's own inter-object
    guess, kerned words, marks) is left exactly where pdfium put it, so nothing else changes.
    """
    zero = [i for i, c in enumerate(chars) if c.text.isspace() and c.x1 == c.x0]
    if not zero:
        return chars
    raw = textpage.raw
    order: dict[int, int] = {}
    space_x: dict[int, tuple[float, float, float]] = {}
    left, bottom, right, top = (ctypes.c_float() for _ in range(4))
    for k in range(pdfium_c.FPDFPage_CountObjects(page.raw)):
        obj = pdfium_c.FPDFPage_GetObject(page.raw, k)
        if pdfium_c.FPDFPageObj_GetType(obj) != pdfium_c.FPDF_PAGEOBJ_TEXT:
            continue
        order[ctypes.cast(obj, ctypes.c_void_p).value or 0] = k
        if (
            pdfium_c.FPDFPageObj_GetBounds(obj, left, bottom, right, top)
            and right.value == left.value
        ):
            space_x[k] = (left.value, bottom.value, top.value)
    if not space_x:
        return chars

    def object_of(i: int) -> int | None:
        handle = pdfium_c.FPDFText_GetTextObject(raw, i)
        return order.get(ctypes.cast(handle, ctypes.c_void_p).value or 0)

    out = list(chars)
    n = len(chars)
    frame = _Frame(page)
    for i in zero:
        prev = next((j for j in range(i - 1, -1, -1) if not chars[j].is_space), None)
        nxt = next((j for j in range(i + 1, n) if not chars[j].is_space), None)
        if prev is None or nxt is None:
            continue
        a, b = object_of(prev), object_of(nxt)
        if a is None or b is None:
            continue
        lo, hi = sorted((a, b))
        p, q = chars[prev], chars[nxt]
        gap = (min(p.x1, q.x1), max(p.x0, q.x0))  # the boundary interval between the two glyphs
        band = (min(p.y0, q.y0), max(p.y1, q.y1))
        between = []
        for k in range(lo + 1, hi):
            if k in space_x:
                x, y0, _x, y1 = frame.box(
                    space_x[k][0], space_x[k][1], space_x[k][0], space_x[k][2]
                )
                if gap[0] <= x <= gap[1] and y0 <= band[1] and y1 >= band[0]:
                    between.append(x)
        if len(between) == 1:
            c = chars[i]
            out[i] = _Char(
                between[0],
                c.y0,
                between[0],
                c.y1,
                c.text,
                c.font_size,
                c.generated,
                c.map_error,
                c.invisible,
                seq=c.seq,
                artifact=c.artifact,
            )
    return out


def _percentile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    return ordered[int(round(q * (len(ordered) - 1)))]


def _valley(gaps: list[float]) -> tuple[float, float] | None:
    """(start, end) in em of the first low-density run above the dense intra-word gap mode.

    Histogram at INK_BIN_EM; the mode is the fullest bin under INK_MODE_MAX_EM; the valley opens
    at the first INK_VALLEY_BINS consecutive bins above it holding together at most
    INK_VALLEY_SHARE of the pairs and ends at the next bin holding more than that share on its
    own (the word-gap mode, when the producer wrote no spaces), else at 1 em. No such run: None.
    """
    bins = int(round(1.0 / INK_BIN_EM))
    counts = [0] * (bins + INK_VALLEY_BINS)
    for gap in gaps:
        counts[min(max(int(gap / INK_BIN_EM), 0), bins - 1)] += 1
    mode = max(range(int(INK_MODE_MAX_EM / INK_BIN_EM)), key=lambda b: counts[b])
    floor = INK_VALLEY_SHARE * len(gaps)
    for b in range(mode + 1, bins):
        if sum(counts[b : b + INK_VALLEY_BINS]) <= floor:
            end = next((e for e in range(b, bins) if counts[e] > floor), bins)
            return b * INK_BIN_EM, end * INK_BIN_EM
    return None


def _row_normalised(
    chars: list[_Char], raw: list[tuple[int, int, float, bool, bool, bool]]
) -> list[tuple[int, int, float, bool, bool, bool]]:
    """Gaps in em of the ROW's tallest glyph, not the pair's: a row run is a chain of same-row
    pairs; every pair in it is divided by the run's maximum glyph height. Per-pair heights smear
    the valley — the same 1.9 pt word gap is 0.28 em under an `f` and 0.44 em under an `e`, and a
    comma's 1.4 pt bearing is 0.33 em of an x-height letter — so word gaps after tall letters fell
    under the threshold and punctuation after short letters sat above it (ING-7 round 2)."""
    out: list[tuple[int, int, float, bool, bool, bool]] = []
    k = 0
    while k < len(raw):
        end = k
        while end + 1 < len(raw) and raw[end + 1][0] == raw[end][1]:
            end += 1
        glyphs = [chars[raw[k][0]]] + [chars[raw[j][1]] for j in range(k, end + 1)]
        size = max(max(c.y1 - c.y0 for c in glyphs), 1e-6)
        for j in range(k, end + 1):
            prev_i, i, gap_pt, forward, spaced, generated = raw[j]
            out.append((prev_i, i, gap_pt / size, forward, spaced, generated))
        k = end + 1
    return out


def _same_row(a: _Char, b: _Char) -> bool:
    overlap = min(a.y1, b.y1) - max(a.y0, b.y0)
    return overlap >= INK_ROW_OVERLAP * min(a.y1 - a.y0, b.y1 - b.y0)


def segment_ink_boxes(chars: list[_Char]) -> list[_Char]:
    """ING-7 (ruling #49): word segmentation on a page whose char boxes are glyph ink.

    pdfium's loose char box is the glyph's ink when the font's type ascent equals its descent
    (Ghostscript re-renders of dvips bitmap fonts: `/FontBBox [0 0 1 -1]`, no /FontDescriptor),
    so consecutive letters of one word are 0.06–0.22 em apart and the layer's WORD_MARGIN read
    them as words. Measured on tier 4 (ING-7 note §2): on such pages 0–1 % of the glyph pairs
    with no space between them abut (gap under INK_ABUTTING_EM), on every advance-box page 55–100 %
    do; that share is the gate, so pages with advance boxes are returned untouched. On an
    ink-box page the word threshold sits in the middle of the low-density valley above the
    intra-word gap mode (`_valley`: the side bearings end at 0.24 em on the corpus) whose far
    side is the nearest of the next occupied gap mode, the INK_SPACE_PERCENTILE gap of the pairs
    pdfium itself separated with a generated space (its advance-based word breaks, 0.34 em and
    up) and INK_SPACE_FALLBACK_EM above the valley's start.
    A forward pair under the threshold is one word: the earlier glyph's box is extended to the
    next glyph (the advance box), both glyphs take the pair's larger font size (a comma's ink is
    a tenth of a letter's and ING-5's box-height size would otherwise cut it out of the row) and
    any generated space between them is dropped; a pair the producer separated with its own
    space glyph is left alone. The same signature — boxes that do not abut — also comes from
    fixed-pitch code listings set on a column pitch wider than the glyph advance (TeX listings
    in CMTT), where the rule closes the pitch gaps inside a token. Right-to-left runs (pdfium hands
    an RTL word's glyphs with x decreasing) never split on WORD_MARGIN, so only generated spaces
    are dropped there. Per page, geometric, no font names.
    """
    raw: list[tuple[int, int, float, bool, bool, bool]] = []
    prev = -1
    between: list[int] = []
    for i, ch in enumerate(chars):
        if ch.is_line_break:
            prev, between = -1, []
            continue
        if ch.is_space:
            if prev >= 0:
                between.append(i)
            continue
        if prev >= 0 and _same_row(chars[prev], ch):
            p = chars[prev]
            forward = ch.x0 >= p.x0
            gap_pt = (ch.x0 - p.x1) if forward else (p.x0 - ch.x1)
            generated = any(chars[k].generated for k in between)
            raw.append((prev, i, gap_pt, forward, bool(between), generated))
        prev, between = i, []
    pairs = _row_normalised(chars, raw)
    plain = [gap for _, _, gap, forward, spaced, _ in pairs if forward and not spaced]
    if len(plain) < INK_MIN_PAIRS:
        return chars
    if sum(1 for gap in plain if gap < INK_ABUTTING_EM) >= INK_ADVANCE_SHARE * len(plain):
        return chars
    valley = _valley(plain)
    if valley is None:
        return chars
    lo, nxt = valley
    hi = min(lo + INK_SPACE_FALLBACK_EM, nxt)
    spaces = [gap for _, _, gap, forward, _, generated in pairs if forward and generated]
    if len(spaces) >= INK_MIN_SPACES:
        hi = min(hi, _percentile(spaces, INK_SPACE_PERCENTILE))
    threshold = (lo + hi) / 2 if hi > lo else lo
    drop: set[int] = set()
    for prev_i, i, gap, forward, spaced, generated in pairs:
        if gap >= threshold or (spaced and not generated) or (generated and gap >= lo):
            continue
        p, ch = chars[prev_i], chars[i]
        if forward and ch.x0 > p.x1:
            p.x1 = ch.x0
        p.font_size = ch.font_size = max(p.font_size, ch.font_size)
        drop.update(k for k in range(prev_i + 1, i) if chars[k].generated and chars[k].is_space)
    return [c for k, c in enumerate(chars) if k not in drop]


def _matrix_chain(obj: pdfium.PdfObject) -> list[pdfium_c.FS_MATRIX]:
    chain: list[pdfium_c.FS_MATRIX] = []
    node: pdfium.PdfObject | None = obj
    while node is not None:
        matrix = pdfium_c.FS_MATRIX()
        if not pdfium_c.FPDFPageObj_GetMatrix(node.raw, ctypes.byref(matrix)):
            return []
        chain.append(matrix)
        node = node.container
    return chain


def _apply(chain: list[pdfium_c.FS_MATRIX], x: float, y: float) -> tuple[float, float]:
    for m in chain:
        x, y = m.a * x + m.c * y + m.e, m.b * x + m.d * y + m.f
    return x, y


def _path_points(obj: pdfium.PdfObject, frame: _Frame) -> list[tuple[float, float, bool]]:
    chain = _matrix_chain(obj)
    points: list[tuple[float, float, bool]] = []
    for i in range(pdfium_c.FPDFPath_CountSegments(obj.raw)):
        seg = pdfium_c.FPDFPath_GetPathSegment(obj.raw, i)
        x, y = ctypes.c_float(), ctypes.c_float()
        if not pdfium_c.FPDFPathSegment_GetPoint(seg, ctypes.byref(x), ctypes.byref(y)):
            continue
        moveto = pdfium_c.FPDFPathSegment_GetType(seg) == pdfium_c.FPDF_SEGMENT_MOVETO
        points.append((*frame.point(*_apply(chain, x.value, y.value)), moveto))
        if pdfium_c.FPDFPathSegment_GetClose(seg) and points:
            head = next(p for p in reversed(points) if p[2])
            points.append((head[0], head[1], False))
    return points


def _walk_shapes(page: pdfium.PdfPage, frame: _Frame) -> list[Shape]:
    shapes: list[Shape] = []
    kinds = {pdfium_c.FPDF_PAGEOBJ_IMAGE: "image", pdfium_c.FPDF_PAGEOBJ_FORM: "figure"}
    for obj in page.get_objects(filter=(pdfium_c.FPDF_PAGEOBJ_PATH, *kinds)):
        if obj.type != pdfium_c.FPDF_PAGEOBJ_PATH:
            if obj.level == 0:
                shapes.append((kinds[obj.type], frame.box(*obj.get_bounds())))
            continue
        points = _path_points(obj, frame)
        for (x0, y0, _), (x1, y1, moveto) in zip(points, points[1:], strict=False):
            if moveto:
                continue
            box = (min(x0, x1), min(y0, y1), max(x0, x1), max(y0, y1))
            if abs(y1 - y0) <= 1.0 and abs(x1 - x0) > 1.0:
                shapes.append(("hline", box))
            elif abs(x1 - x0) <= 1.0 and abs(y1 - y0) > 1.0:
                shapes.append(("vline", box))
    return shapes


FORM_XOBJECT_DEPTH = 15


def _image_count(page: pdfium.PdfPage, pattern_images: Callable[[], int]) -> int:
    kinds = (pdfium_c.FPDF_PAGEOBJ_IMAGE, pdfium_c.FPDF_PAGEOBJ_PATH)
    types = [obj.type for obj in page.get_objects(filter=kinds, max_depth=FORM_XOBJECT_DEPTH)]
    images = types.count(pdfium_c.FPDF_PAGEOBJ_IMAGE)
    if images or pdfium_c.FPDF_PAGEOBJ_PATH not in types:
        return images
    return pattern_images()


def _page_text_from_chars(
    chars: list[_Char],
    page_box: Bbox,
    shapes: Sequence[Shape] = (),
    image_count: Callable[[], int] = lambda: 0,
    text_objects: Callable[[], int] = lambda: 0,
    tagged: bool = False,
) -> PageText:
    """C4/C5 via the deterministic static text layer (ING-2, `textlayer.py`)."""
    layer_chars = [
        Char(c.x0, c.y0, c.x1, c.y1, c.text, c.font_size, invisible=c.invisible, seq=c.seq)
        for c in chars
    ]
    glyph_text = "".join(c.text for c in chars if not c.generated)
    shown, decodable = glyph_counts(glyph_text)
    if not glyph_text.strip():
        if is_scanned_page(glyph_text, image_count()):
            return PageText(needs_fallback=("scanned",))
        undecodable = is_undecodable_text(text_objects(), shown, decodable)
        return PageText(needs_fallback=("undecodable_text",) if undecodable else ())
    undecodable = decodable < DECODABLE_FLOOR * shown and is_undecodable_text(
        text_objects(), shown, decodable
    )
    lines = join_split_lines(split_at_gutters(group_lines(layer_chars)))
    lines = join_lone_numbers(join_script_runs(lines, shapes), shapes)
    blocks, markers = structure(lines, page_box, shapes)
    ordered = linear_order(blocks) if markers else reading_order(xy_cut(blocks, page_box))
    raw_blocks, words = to_page_shapes(ordered)
    return PageText(
        blocks=raw_blocks,
        words=words,
        artifact_share=_artifact_share(chars, raw_blocks) if tagged else (),
        unicode_map_errors=sum(1 for c in chars if c.map_error),
        needs_fallback=markers + (("undecodable_text",) if undecodable else ()),
    )


def _artifact_share(chars: list[_Char], blocks: Sequence[RawBlock]) -> tuple[float, ...]:
    """Per block, the share of its glyphs drawn by `/Artifact` marked content (ISO 32000-1
    §14.8.2.2): page furniture the author declared, located by glyph centre."""
    marked = [c for c in chars if not c.generated and not c.is_space]
    if not any(c.artifact for c in marked):
        return ()
    shares: list[float] = []
    for (x0, y0, x1, y1), _text, _size in blocks:
        inside = [
            c.artifact
            for c in marked
            if x0 - ARTIFACT_TOLERANCE <= (c.x0 + c.x1) / 2 <= x1 + ARTIFACT_TOLERANCE
            and y0 - ARTIFACT_TOLERANCE <= (c.y0 + c.y1) / 2 <= y1 + ARTIFACT_TOLERANCE
        ]
        shares.append(sum(inside) / len(inside) if inside else 0.0)
    return tuple(shares)


_LOG = logging.getLogger(__name__)


class PdfiumDocument:
    backend = "pdfium"
    source_label = "pdfium"
    is_reflowable = False

    def __init__(self, doc: pdfium.PdfDocument, book_path: str = "") -> None:
        self._doc = doc
        self._path = book_path
        self._objects: PdfObjects | None = None
        self.page_count: int = len(doc)
        self._tagged = bool(doc.is_tagged())

    def _syntax(self) -> PdfObjects | None:
        if not self._path:
            return None
        if self._objects is None:
            self._objects = PdfObjects.from_path(self._path)
        return self._objects

    def _pattern_images(self, index: int) -> int:
        try:
            objects = self._syntax()
            return pattern_image_count(objects, index) if objects else 0
        except Exception as exc:
            _LOG.warning("pattern census unavailable for page %d: %s", index, type(exc).__name__)
            return 0

    def _font_maps(self, index: int) -> list[FontMap]:
        try:
            objects = self._syntax()
            return page_fonts(objects, index) if objects else []
        except Exception as exc:
            _LOG.warning("font census unavailable for page %d: %s", index, type(exc).__name__)
            return []

    def _text_objects(self, index: int) -> int:
        try:
            objects = self._syntax()
            if objects is None:
                return 0
            return sum(text_objects_in(s) for s in page_streams(objects, index))
        except Exception as exc:
            _LOG.warning(
                "text-object census unavailable for page %d: %s", index, type(exc).__name__
            )
            return 0

    def page_size(self, index: int) -> tuple[float, float]:
        try:
            page = self._doc.get_page(index)
        except Exception as exc:
            _LOG.error(
                "pdfium could not load page %d for size: %s: %s", index, type(exc).__name__, exc
            )
            return (0.0, 0.0)
        try:
            width, height = page.get_size()
            return float(width), float(height)
        finally:
            page.close()

    def page_text(self, index: int) -> PageText:
        try:
            page = self._doc.get_page(index)
        except Exception as exc:
            _LOG.error("pdfium could not load page %d: %s: %s", index, type(exc).__name__, exc)
            return PageText(extract_failed=type(exc).__name__)
        try:
            textpage = page.get_textpage()
            try:
                frame = _Frame(page)
                page_box = (0.0, 0.0, frame.width, frame.height)
                try:
                    shapes = _walk_shapes(page, frame)
                except Exception:
                    shapes = []
                return _page_text_from_chars(
                    segment_ink_boxes(
                        _anchor_spaces(
                            page,
                            textpage,
                            _walk_chars(textpage, frame, lambda: self._font_maps(index)),
                        )
                    ),
                    page_box,
                    shapes,
                    lambda: _image_count(page, lambda: self._pattern_images(index)),
                    lambda: self._text_objects(index),
                    self._tagged,
                )
            finally:
                textpage.close()
        except Exception as exc:
            _LOG.error(
                "static text layer failed on page %d: %s: %s", index, type(exc).__name__, exc
            )
            return PageText(extract_failed=type(exc).__name__)
        finally:
            page.close()

    def render_png(self, index: int, scale: float, clip: Bbox | None = None) -> bytes:
        page = self._doc.get_page(index)
        try:
            crop = (0.0, 0.0, 0.0, 0.0)
            if clip is not None:
                width, height = page.get_size()
                x0, y0, x1, y1 = clip
                x0, y0 = max(0.0, x0), max(0.0, y0)
                x1, y1 = min(float(width), x1), min(float(height), y1)
                if x1 <= x0 or y1 <= y0:
                    x0, y0, x1, y1 = 0.0, 0.0, float(width), float(height)
                crop = (x0, float(height) - y1, float(width) - x1, y0)
            bitmap = page.render(scale=scale, crop=crop, rev_byteorder=True, may_draw_forms=False)
            try:
                image = bitmap.to_pil()
                buffer = io.BytesIO()
                image.save(buffer, format="PNG")
                return buffer.getvalue()
            finally:
                bitmap.close()
        finally:
            page.close()

    def toc(self) -> list[list[object]]:
        entries: list[list[object]] = []
        for bookmark in self._doc.get_toc(max_depth=15):
            dest = bookmark.get_dest()
            page_index = dest.get_index() if dest is not None else None
            page_no = page_index + 1 if page_index is not None else -1
            entries.append([int(bookmark.level) + 1, bookmark.get_title(), page_no])
        return entries

    def metadata(self) -> dict[str, str]:
        raw = self._doc.get_metadata_dict(skip_empty=True)
        return {str(key).lower(): str(value) for key, value in raw.items()}

    def chapter_count(self) -> int:
        raise IngestError("ERR_BAD_REQUEST", "not a reflowed document")

    def page_number_from_location(self, spine_index: int) -> int:
        raise IngestError("ERR_BAD_REQUEST", "not a reflowed document")

    def close(self) -> None:
        self._objects = None
        self._doc.close()


TAIL_BYTES = 2048
DICT_BYTES = 65536
MAX_PREV_HOPS = 8
"""Implementer-chosen heuristics, not specification values: ISO 32000-1 §7.5.5 bounds neither
the distance of `startxref` from EOF nor a trailer's size, and sets no `/Prev` chain length.
A file beyond any of them reads as not-declared, which surfaces as ERR_OPEN — never a false
ERR_DRM."""


def _read_at(handle: BinaryIO, offset: int, size: int) -> bytes:
    handle.seek(max(offset, 0))
    return handle.read(size)


def _last_startxref(handle: BinaryIO) -> int | None:
    handle.seek(0, io.SEEK_END)
    end = handle.tell()
    tail = _read_at(handle, end - TAIL_BYTES, TAIL_BYTES)
    offsets = re.findall(rb"startxref\s+(\d+)", tail)
    return int(offsets[-1]) if offsets else None


_INDIRECT_REF = re.compile(rb"\d+[\x00\t\n\x0c\r ]+\d+[\x00\t\n\x0c\r ]+R")
_INTEGER = re.compile(rb"\d+")
_WHITESPACE = b" \t\r\n\x0c\x00"
_NAME_TOKEN = re.compile(rb"/[^\x00\t\n\x0c\r /\[\]<>(){}%]*")
_NAME_ESCAPE = re.compile(rb"#([0-9A-Fa-f]{2})")
_BARE_TOKEN = re.compile(rb"[^\x00\t\n\x0c\r /\[\]<>(){}%]+")
_REF_TAIL = re.compile(
    rb"[\x00\t\n\x0c\r ]+\d+[\x00\t\n\x0c\r ]+R(?![^\x00\t\n\x0c\r /\[\]<>(){}%])"
)


def _skip_object(buf: bytes, pos: int, end: int) -> int | None:
    if buf.startswith(b"<<", pos) or buf[pos] == 0x5B:
        depth = 0
        while pos < end:
            if buf.startswith(b"<<", pos) or buf[pos] == 0x5B:
                depth, pos = depth + 1, pos + (2 if buf[pos] == 0x3C else 1)
            elif buf.startswith(b">>", pos) or buf[pos] == 0x5D:
                depth, pos = depth - 1, pos + (2 if buf[pos] == 0x3E else 1)
                if depth == 0:
                    return pos
            elif buf[pos] in _WHITESPACE:
                pos += 1
            else:
                nxt = _skip_atom(buf, pos, end)
                if nxt is None:
                    return None
                pos = nxt
        return None
    return _skip_atom(buf, pos, end)


def _skip_atom(buf: bytes, pos: int, end: int) -> int | None:
    if buf[pos] == 0x28:
        nest, pos = 1, pos + 1
        while pos < end and nest:
            nest += {0x28: 1, 0x29: -1}.get(buf[pos], 0)
            pos += 2 if buf[pos] == 0x5C else 1
        return pos if nest == 0 else None
    if buf[pos] == 0x3C:
        close = buf.find(b">", pos)
        return None if close < 0 else close + 1
    if buf[pos] == 0x25:
        eol = re.search(rb"[\r\n]", buf[pos:])
        return end if eol is None else pos + eol.end()
    if buf[pos] == 0x2F:
        match = _NAME_TOKEN.match(buf, pos)
        return None if match is None else match.end()
    match = _BARE_TOKEN.match(buf, pos)
    return None if match is None else match.end()


def _pdf_dict_entries(buf: bytes, start: int) -> dict[bytes, bytes] | None:
    """Top-level key → raw value bytes of the dict at ``buf[start]`` (ISO 32000-1 §7.3.7).

    A dictionary is a sequence of key/value pairs; every key is a name object and
    every value is exactly one object, so names in value position are never keys.
    Strings, hex strings, comments and nested containers are skipped as single
    tokens. Returns None if the dict is malformed or never closes.
    """
    if not buf.startswith(b"<<", start):
        return None
    entries: dict[bytes, bytes] = {}
    pos, end = start + 2, len(buf)
    while pos < end:
        if buf[pos] in _WHITESPACE:
            pos += 1
        elif buf[pos] == 0x25:
            nxt = _skip_atom(buf, pos, end)
            if nxt is None:
                return None
            pos = nxt
        elif buf.startswith(b">>", pos):
            return entries
        elif buf[pos] == 0x2F:
            key_match = _NAME_TOKEN.match(buf, pos)
            if key_match is None:
                return None
            pos = key_match.end()
            while pos < end and (buf[pos] in _WHITESPACE or buf[pos] == 0x25):
                pos = pos + 1 if buf[pos] != 0x25 else (_skip_atom(buf, pos, end) or end)
            if pos >= end or buf.startswith(b">>", pos):
                return None
            value_start = pos
            nxt = _skip_object(buf, pos, end)
            if nxt is None:
                return None
            pos = nxt
            ref = _REF_TAIL.match(buf, pos)
            if ref is not None and _BARE_TOKEN.fullmatch(buf, value_start, nxt):
                pos = ref.end()
            key = _NAME_ESCAPE.sub(lambda m: bytes([int(m.group(1), 16)]), key_match.group())
            entries[key] = buf[value_start:pos]
        else:
            return None
    return None


def _trailer_declares_encryption(handle: BinaryIO, offset: int) -> bool:
    seen: set[int] = set()
    for _ in range(MAX_PREV_HOPS + 1):
        if offset in seen:
            return False
        seen.add(offset)
        chunk = _read_at(handle, offset, DICT_BYTES)
        trailer = chunk.find(b"trailer")
        start = chunk.find(b"<<", trailer if trailer >= 0 else 0)
        entries = _pdf_dict_entries(chunk, start) if start >= 0 else None
        if entries is None:
            return False
        if trailer < 0 and entries.get(b"/Type", b"").strip() != b"/XRef":
            return False
        encrypt = entries.get(b"/Encrypt", b"").strip()
        if encrypt.startswith(b"<<") or _INDIRECT_REF.fullmatch(encrypt):
            return True
        prev = _INTEGER.fullmatch(entries.get(b"/Prev", b"").strip())
        if prev is None:
            return False
        offset = int(prev.group())
    return False


def _declares_encryption(book_path: str) -> bool:
    try:
        with open(book_path, "rb") as handle:
            offset = _last_startxref(handle)
            return offset is not None and _trailer_declares_encryption(handle, offset)
    except (OSError, ValueError):
        return False


def open_pdfium(book_path: str) -> PdfiumDocument:
    try:
        doc = pdfium.PdfDocument(book_path)
    except pdfium.PdfiumError as exc:
        if exc.err_code == pdfium_c.FPDF_ERR_PASSWORD and _declares_encryption(book_path):
            raise IngestError(
                "ERR_DRM", "document is encrypted (DRM) and cannot be ingested"
            ) from exc
        raise IngestError("ERR_OPEN", f"cannot open file: {exc}") from exc
    except Exception as exc:
        raise IngestError("ERR_OPEN", f"cannot open file: {exc}") from exc
    if len(doc) == 0:
        doc.close()
        raise IngestError("ERR_EMPTY", "document has no pages")
    return PdfiumDocument(doc, book_path)
