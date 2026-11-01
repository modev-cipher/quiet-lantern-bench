from collections.abc import AsyncIterator, Callable

from txtcaster_engine.cancel import CancelRegistry
from txtcaster_engine.ingest.docling_support import check_artifacts, docling_available
from txtcaster_engine.layout.convert import classifier_available, detect_regions
from txtcaster_engine.stage import Event, stage_events


def layout_events(
    book_path: str,
    pages: list[int],
    content_hash: str,
    cache_dir: str,
    job_id: str,
    registry: CancelRegistry,
    docling_artifacts_dir: str | None = None,
    device: str = "cpu",
) -> AsyncIterator[Event]:
    def work(emit: Callable[[Event], None]) -> None:
        flags: list[str] = []
        # Same order as `manifest._accurate_path_ready`: the Docling import is last, so a
        # request with no or incomplete artifacts never loads PyTorch (ruling #36).
        if (
            docling_artifacts_dir is None
            or not check_artifacts(docling_artifacts_dir).is_complete()
            or not docling_available()
        ):
            emit(("result", {"regions": [], "flags": ["layout_skipped"]}))
            return
        with_classifier = classifier_available(docling_artifacts_dir)
        if not with_classifier:
            flags.append("layout_classifier_absent")

        def on_page(done: int, total: int) -> None:
            emit(("progress", {"stage": "layout", "page": done, "total": total}))

        regions = detect_regions(
            book_path,
            docling_artifacts_dir,
            pages,
            content_hash,
            cache_dir,
            with_classifier,
            on_page=on_page,
            is_cancelled=lambda: registry.is_cancelled(job_id),
            device=device,
        )
        emit(("result", {"regions": [r.to_json() for r in regions], "flags": flags}))

    return stage_events(job_id, registry, work, stage="layout")
