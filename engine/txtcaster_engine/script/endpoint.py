from collections.abc import AsyncIterator, Callable

from txtcaster_engine.cancel import CancelRegistry
from txtcaster_engine.ingest.errors import IngestError
from txtcaster_engine.llm.backend import LLMBackend
from txtcaster_engine.residency import Lease, ModelKey, ResidencyError, ResidencyManager
from txtcaster_engine.script.assemble import FigureAnchor
from txtcaster_engine.script.canonical import CanonicalPage
from txtcaster_engine.script.pipeline import generate_script
from txtcaster_engine.script.suppression import Suppressed
from txtcaster_engine.stage import Event, stage_events


def script_events(
    canonical_pages: list[CanonicalPage],
    scope: dict[str, object],
    mode: str,
    job_id: str,
    registry: CancelRegistry,
    backend: LLMBackend | None = None,
    figures: list[FigureAnchor] | None = None,
    residency: ResidencyManager | None = None,
    llm_slot: ModelKey | None = None,
    section_bounds: list[tuple[int, int]] | None = None,
) -> AsyncIterator[Event]:
    def work(emit: Callable[[Event], None]) -> None:
        def on_chapter(done: int, total: int) -> None:
            emit(("progress", {"stage": "script", "chapter": done, "total": total}))

        def on_chunk(done: int, total: int) -> None:
            emit(("progress", {"stage": "script", "chunk": done, "total": total}))

        dropped_markers = 0
        suppressed: list[dict[str, object]] = []
        omitted_markers = 0

        def on_markers_dropped(count: int) -> None:
            nonlocal dropped_markers
            dropped_markers += count

        def on_markers_omitted(count: int) -> None:
            nonlocal omitted_markers
            omitted_markers += count

        def on_suppressed(records: list[Suppressed]) -> None:
            suppressed.extend(record.to_json() for record in records)

        lease: Lease | None = None
        if residency is not None and llm_slot is not None:
            try:
                lease = residency.retain(llm_slot)
            except ResidencyError as exc:
                raise IngestError(exc.code, exc.message) from exc
        try:
            segments = generate_script(
                canonical_pages,
                scope,
                mode,
                backend=backend,
                figures=figures,
                on_chapter=on_chapter,
                on_chunk=on_chunk,
                is_cancelled=lambda: registry.is_cancelled(job_id),
                on_markers_dropped=on_markers_dropped,
                on_markers_omitted=on_markers_omitted,
                on_suppressed=on_suppressed,
                section_bounds=section_bounds,
            )
        finally:
            if residency is not None and lease is not None:
                residency.release(lease)
        emit(
            (
                "segments",
                {
                    "segments": [segment.to_json() for segment in segments],
                    "figure_markers_dropped": dropped_markers,
                    "figure_markers_omitted": omitted_markers,
                    "suppressed": suppressed,
                },
            )
        )

    return stage_events(job_id, registry, work, stage="script")
