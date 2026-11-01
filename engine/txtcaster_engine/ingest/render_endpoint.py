from collections.abc import AsyncIterator, Callable

from txtcaster_engine.cancel import CancelRegistry
from txtcaster_engine.ingest.render import render_pages
from txtcaster_engine.stage import Event, stage_events


def render_events(
    book_path: str,
    content_hash: str,
    pages: list[int],
    cache_dir: str,
    scale: float,
    with_geometry: bool,
    job_id: str,
    registry: CancelRegistry,
) -> AsyncIterator[Event]:
    def work(emit: Callable[[Event], None]) -> None:
        def on_page(done: int, total: int) -> None:
            emit(("progress", {"stage": "render", "page": done, "total": total}))

        results, skipped = render_pages(
            book_path,
            content_hash,
            pages,
            cache_dir,
            scale=scale,
            with_geometry=with_geometry,
            on_page=on_page,
            is_cancelled=lambda: registry.is_cancelled(job_id),
        )
        emit(("result", {"pages": [r.to_json() for r in results], "skipped": skipped}))

    return stage_events(job_id, registry, work, stage="render_pages")
