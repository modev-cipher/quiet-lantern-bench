"""The one scanned-page rule, shared by every backend.

A page is `scanned` iff it has zero non-whitespace glyphs in its text layer AND at least one
image XObject. Whitespace is `str.strip` whitespace, so U+00A0 and the other Unicode spaces count
as whitespace; whitespace-only glyph runs are not text. A blank page (no glyphs, no image) and a
vector-only page (drawn paths, no image XObject) are not scanned; they are empty pages.

Image evidence: pdfium walks its instantiated page-object tree through nested Form XObjects
(pypdfium2 `max_depth`, `FORM_XOBJECT_DEPTH`, a recursion bound on a tree that cannot cycle) and
sees inline images (`BI … EI`) as image objects; pymupdf's `dict` image blocks cover both too.
Images painted through a tiling Pattern never appear in pdfium's page-object tree (only the filled
path does) and pypdfium2 exposes no resource dictionaries, so for a whitespace-only page with paths
and no image object the pdfium adapter falls back to a minimal PDF-syntax scan of the file
(`pdf_resources.py`): page → inherited /Resources → /Pattern → PatternType 1 cells → their /XObject
images, recursing through Form XObjects to exhaustion with a `seen` set against cycles, the same
walk the text-object rule uses (`textobjects.py`); objects inside FlateDecode object streams are
inflated. Known gaps of that route: encrypted or non-Flate object streams are not read, and a
declared-but-unpainted image inside a pattern cell counts as painted.
"""


def is_scanned_page(glyph_text: str, image_count: int) -> bool:
    return not glyph_text.strip() and image_count > 0
