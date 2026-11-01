from txtcaster_engine.ingest.epub import is_epub
from txtcaster_engine.ingest.epub_adapter import open_epub
from txtcaster_engine.ingest.ingestor import Ingestor
from txtcaster_engine.ingest.pdfium_adapter import open_pdfium
from txtcaster_engine.ingest.reflow import REFLOW_FONT_SIZE as REFLOW_FONT_SIZE
from txtcaster_engine.ingest.reflow import REFLOW_PAGE_HEIGHT as REFLOW_PAGE_HEIGHT
from txtcaster_engine.ingest.reflow import REFLOW_PAGE_WIDTH as REFLOW_PAGE_WIDTH


def open_document(book_path: str) -> Ingestor:
    if is_epub(book_path):
        return open_epub(book_path)
    return open_pdfium(book_path)
