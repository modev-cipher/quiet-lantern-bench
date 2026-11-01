"""Reflow geometry for EPUB and other reflowable documents: the fixed page the layout engine
lays HTML onto, part of every cache identity (`version.render_identity`). Lives apart from the
adapters so the version strings and the pdfium path import no pymupdf."""

REFLOW_PAGE_WIDTH = 612
REFLOW_PAGE_HEIGHT = 792
REFLOW_FONT_SIZE = 11
REFLOW_LAYOUT_VERSION = 2
"""Bump whenever the reflow layout or its painting changes: it is part of `render_identity`,
so page PNG cache keys and the geometry stamp change and an imported book is offered a re-read.
2: Arabic text is drawn with the bundled Naskh face, joined and in right-to-left visual order
(epub_adapter, arabic_shaping); version 1 painted every Arabic character as a .notdef box in
left-to-right order, and those PNGs would otherwise be served from the cache forever.
"""
