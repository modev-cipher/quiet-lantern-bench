"""Cheap look at a document before committing to importing it.

The layout pass costs 0.25 s/page on a mid-range GPU and 11.4 s/page on CPU, so a
613-page book is three minutes on one machine and nearly two hours on another. The
difference has to be shown to the user before the import starts, and that needs the page
count — which is the only thing here that is expensive to guess and cheap to read.
"""

from txtcaster_engine.ingest.epub import is_epub
from txtcaster_engine.ingest.errors import IngestError
from txtcaster_engine.ingest.pdf import open_document


def probe_document(book_path: str) -> dict[str, object]:
    doc = open_document(book_path)
    try:
        page_count = doc.page_count
    finally:
        doc.close()
    if page_count <= 0:
        raise IngestError("ERR_EMPTY", "document has no pages")
    return {
        "page_count": page_count,
        "reflowed": is_epub(book_path),
    }
