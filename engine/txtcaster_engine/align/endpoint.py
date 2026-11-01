from collections.abc import AsyncIterator, Callable

from txtcaster_engine.align.backend import AlignBackend, AlignBackendError
from txtcaster_engine.align.pipeline import AlignInput, AlignResult, align_segments
from txtcaster_engine.cancel import CancelRegistry
from txtcaster_engine.device import PlanError, parse_device
from txtcaster_engine.ingest.errors import IngestError
from txtcaster_engine.residency import ResidencyError, ResidencyManager, model_key
from txtcaster_engine.stage import Event, stage_events

DEFAULT_WORKERS = 2
ALIGN_TASK = "align"

FAKE_ALIGN_MODEL_ID = "fake-align"
DEFAULT_LANGUAGE = "en"
ALIGN_DEVICE = "cpu"


def create_backend(
    model_id: str, model_dir: str, device: str, allow_fake_align: bool = False
) -> AlignBackend:
    if model_id == FAKE_ALIGN_MODEL_ID:
        if not allow_fake_align:
            raise AlignBackendError(f"unsupported align model: {model_id}")
        from txtcaster_engine.align.backend import FakeAlignBackend

        return FakeAlignBackend()
    if model_id == "wav2vec2-base-960h":
        from txtcaster_engine.align.backend import Wav2Vec2CtcBackend

        return Wav2Vec2CtcBackend(model_dir, device=device)
    if model_id.startswith("whisperx"):
        from txtcaster_engine.align.backend import FasterWhisperBackend

        return FasterWhisperBackend(model_dir, device=device)
    raise AlignBackendError(f"unsupported align model: {model_id}")


def align_key(model_id: str, model_dir: str, device: str) -> tuple[str, str, str]:
    return model_key(ALIGN_TASK, model_id, f"{model_dir}|{device}")


def _workers(resource_plan: dict[str, object], device: str) -> int:
    raw = resource_plan.get("align_workers")
    if isinstance(raw, int) and raw > 0:
        return raw
    if device.startswith("cuda"):
        return 1
    return DEFAULT_WORKERS


def align_events(
    segments: list[dict[str, object]],
    model_id: str,
    model_dir: str,
    cache_dir: str,
    language: str | None,
    resource_plan: dict[str, object],
    job_id: str,
    registry: CancelRegistry,
    allow_fake_align: bool = False,
    residency: ResidencyManager | None = None,
    stage: str | None = None,
) -> AsyncIterator[Event]:
    def work(emit: Callable[[Event], None]) -> None:
        inputs = [
            AlignInput(
                idx=int(str(seg.get("idx", 0))),
                text=str(seg.get("text", "")),
                audio_path=str(seg.get("audio_path", "")),
                audio_hash=str(seg.get("audio_hash", "")),
            )
            for seg in segments
        ]
        for item in inputs:
            if not item.audio_hash:
                raise IngestError("ERR_ALIGN_INPUT", f"segment {item.idx} has no audio_hash")
        try:
            planned = parse_device(resource_plan)
        except PlanError as exc:
            raise IngestError(exc.code, exc.message) from exc
        device = ALIGN_DEVICE
        flags: list[str] = []
        if planned.is_gpu:
            flags.append("align_cpu_fallback")

        def make_backend() -> AlignBackend:
            return create_backend(model_id, model_dir, device, allow_fake_align)

        lease = None
        try:
            try:
                if residency is None:
                    backend = make_backend()
                else:
                    lease = residency.acquire(
                        align_key(model_id, model_dir, device), make_backend, stage=stage
                    )
                    backend = lease.backend
            except ResidencyError as exc:
                raise IngestError(exc.code, exc.message) from exc
            except AlignBackendError as exc:
                raise IngestError("ERR_ALIGN_MODEL", exc.message) from exc

            total = len(inputs)
            done = 0

            def on_result(result: AlignResult) -> None:
                nonlocal done
                done += 1
                emit(
                    (
                        "result",
                        {
                            "stage": "align",
                            "done": done,
                            "total": total,
                            "result": result.to_json(),
                        },
                    )
                )

            hint = language.strip().lower() if language else ""
            resolved_language = hint or DEFAULT_LANGUAGE

            align_segments(
                inputs,
                backend,
                model_id,
                resolved_language,
                cache_dir,
                workers=_workers(resource_plan, device),
                on_result=on_result,
                is_cancelled=lambda: registry.is_cancelled(job_id),
            )
        finally:
            if residency is not None and lease is not None:
                residency.release(lease)
                if stage is not None:
                    residency.evict_stage(stage)
        emit(("done", {"stage": "align", "done": done, "total": total, "flags": flags}))

    return stage_events(job_id, registry, work, stage="align")
