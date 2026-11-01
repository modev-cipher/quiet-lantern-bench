"""The one text-object rule (owner ruling #48), shared by the engine and the stress census.

A page *shows text* when a BT..ET scope in its content stream, or in a Form XObject it paints,
executes a showing operator (Tj, TJ, ', ") with a non-empty string operand. The evidence is
lexed, never grepped: comments, literal and hex strings and inline-image data can never count.
A page that shows text yet yields fewer decodable glyphs than `DECODABLE_FLOOR` of its shown
glyphs is `undecodable_text`: its text objects exist but the font maps do not reach Unicode.
Shown glyphs are the backend's non-whitespace chars; a glyph is decodable unless it is U+FFFD, a
C0 control other than TAB/CR/LF or a lone surrogate, which is what an unmapped charcode degrades
to on either backend (pdfium substitutes U+FFFD, pymupdf emits the raw byte). The same counts,
the same floor and the same text-object evidence apply to native and pdfium pages (#48). What is
symmetric is the rule, not the verdict: `undecodable_text` means "this backend could not decode
this page's text", so a page one decoder reads and the other cannot (a glyph name only MuPDF's
list knows) is marked under the backend that failed and read untouched under the one that did
not; marking it there would send correctly read text to a paid fallback (#36).

Text-object evidence walks the page's content stream and every Form XObject it paints to
exhaustion, a `seen` set stopping cycles; the pattern image census in `pdf_resources.py` walks
the same way. The only bounded walk left is pdfium's own instantiated object tree
(`FPDFPage_GetObject`, pypdfium2 `max_depth`), which is not PDF syntax and cannot cycle.

The floor is argued from the tier-4 corpus (docs/notes/ing-5-type3-cmap.md): over its 1,386
pages with glyphs the decodable ratio under pdfium is min 0.766 (a GraphBLAS slide whose
operator glyphs have no map), p1 0.850, p5 0.984, median 1.0, and no page with text objects
has zero glyphs; a page whose fonts have no usable map loses every glyph. Half the glyphs sits
between the worst readable page (0.77) and total loss (0.0) and is the point below which the
readable residue is no longer prose in any language; the marker fires there, not at the first
lost glyph.

Before a glyph counts as lost it gets the per-page decoding fallback (pdfium adapter,
`glyphnames.py`): when a simple font's ToUnicode misses a code pdfium hands back the raw
charcode (`*` for MathType's angbracketleft, LF for mapsto, U+0013 for phi) and flags a map
error, so a flagged char is looked up in the page's /Encoding /Differences (the font pdfium
names when it names one, every font on the page otherwise) and resolved through the glyph
list; one distinct answer wins, disagreement or an opaque name leaves pdfium's text.
"""

from __future__ import annotations

import re

DECODABLE_FLOOR = 0.5
_EI = re.compile(rb"(?:^|[\x00\t\n\x0c\r ])EI(?=[\x00\t\n\x0c\r ]|$)")
_WS = b"\x00\t\n\x0c\r "
_DELIM = b"()<>[]{}/%"
_SHOWING = {b"Tj", b"TJ", b"'", b'"'}


def lex(stream: bytes) -> list[tuple[str, bytes]]:
    out: list[tuple[str, bytes]] = []
    i, n = 0, len(stream)
    while i < n:
        c = stream[i : i + 1]
        if c in _WS:
            i += 1
        elif c == b"%":
            while i < n and stream[i : i + 1] not in b"\r\n":
                i += 1
        elif c == b"(":
            depth, i, start = 1, i + 1, i + 1
            while i < n and depth:
                ch = stream[i : i + 1]
                if ch == b"\\":
                    i += 1
                elif ch == b"(":
                    depth += 1
                elif ch == b")":
                    depth -= 1
                i += 1
            out.append(("str", stream[start : i - 1]))
        elif c == b"<":
            if stream[i + 1 : i + 2] == b"<":
                out.append(("other", b"<<"))
                i += 2
            else:
                end = stream.find(b">", i + 1)
                end = n if end < 0 else end
                out.append(("str", bytes(ch for ch in stream[i + 1 : end] if ch not in _WS)))
                i = end + 1
        elif c == b">":
            if stream[i + 1 : i + 2] == b">":
                out.append(("other", b">>"))
                i += 2
            else:
                out.append(("other", b">"))
                i += 1
        elif c in b"[]{}":
            out.append(("other", c))
            i += 1
        elif c == b"/":
            j = i + 1
            while j < n and stream[j : j + 1] not in _WS and stream[j : j + 1] not in _DELIM:
                j += 1
            out.append(("name", stream[i + 1 : j]))
            i = j
        elif c in _DELIM:
            out.append(("other", c))
            i += 1
        else:
            j = i + 1
            while j < n and stream[j : j + 1] not in _WS and stream[j : j + 1] not in _DELIM:
                j += 1
            tok = stream[i:j]
            out.append(("other" if tok[:1] in b"+-.0123456789" else "op", tok))
            i = j
            if tok == b"ID":
                image_end = _EI.search(stream, j)
                i = image_end.end() if image_end else n
                out.append(("op", b"EI"))
    return out


def text_objects_in(stream: bytes) -> int:
    count, in_bt, shown, pending = 0, False, False, False
    for kind, tok in lex(stream):
        if kind == "str":
            pending = pending or bool(tok.strip())
        elif kind == "op":
            if tok == b"BT":
                in_bt, shown = True, False
            elif tok == b"ET":
                count += int(in_bt and shown)
                in_bt = False
            elif tok in _SHOWING and in_bt and pending:
                shown = True
            pending = False
    return count


def painted_names(stream: bytes) -> list[bytes]:
    toks = lex(stream)
    return [
        toks[k - 1][1]
        for k in range(1, len(toks))
        if toks[k] == ("op", b"Do") and toks[k - 1][0] == "name"
    ]


def is_decodable_glyph(char: str) -> bool:
    code = ord(char)
    return not (
        code == 0xFFFD
        or (code < 0x20 and code not in (0x09, 0x0A, 0x0D))
        or 0xD800 <= code <= 0xDFFF
    )


def glyph_counts(text: str) -> tuple[int, int]:
    shown = [char for char in text if not char.isspace()]
    return len(shown), sum(1 for char in shown if is_decodable_glyph(char))


def is_undecodable_text(text_objects: int, shown: int, decodable: int) -> bool:
    if text_objects == 0:
        return False
    return shown == 0 or decodable < DECODABLE_FLOOR * shown
