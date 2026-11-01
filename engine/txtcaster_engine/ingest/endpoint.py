from collections.abc import AsyncIterator, Callable

from txtcaster_engine.cancel import CancelRegistry
from txtcaster_engine.ingest.fallback.runner import FallbackSettings
from txtcaster_engine.ingest.manifest import ingest_document
from txtcaster_engine.stage import Event, stage_events

PROGRESS_EVERY_PAGES = 5
PAGE_BLOCK_BATCH = 25


def ingest_events(
    book_path: str,
    book_id: str,
    job_id: str,
    registry: CancelRegistry,
    docling_artifacts_dir: str | None = None,
    docling_artifacts_sha: str | None = None,
    layout: bool = True,
    fallback: FallbackSettings | None = None,
) -> AsyncIterator[Event]:
    def work(emit: Callable[[Event], None]) -> None:
        def on_page(page_no: int, total: int, stage: str = "extract") -> None:
            if page_no % PROGRESS_EVERY_PAGES == 0 or page_no == total:
                emit(("progress", {"stage": stage, "page": page_no, "total": total}))

        result = ingest_document(
            book_path,
            book_id,
            on_page=on_page,
            is_cancelled=lambda: registry.is_cancelled(job_id),
            docling_artifacts_dir=docling_artifacts_dir,
            docling_artifacts_sha=docling_artifacts_sha,
            layout=layout,
            fallback=fallback,
        )
        payloads = result.page_payloads()
        for start in range(0, len(payloads), PAGE_BLOCK_BATCH):
            emit(("page_blocks", {"pages": payloads[start : start + PAGE_BLOCK_BATCH]}))
        emit(("manifest", result.to_manifest()))

    return stage_events(job_id, registry, work, stage="ingest")
