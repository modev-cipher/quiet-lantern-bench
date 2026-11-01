import logging
from collections.abc import AsyncIterator, Callable

from txtcaster_engine.cancel import CancelRegistry
from txtcaster_engine.device import PlanError, parse_device
from txtcaster_engine.ingest.errors import IngestError
from txtcaster_engine.residency import ResidencyError, ResidencyManager, model_key
from txtcaster_engine.stage import Event, stage_events
from txtcaster_engine.tts.backend import TTSBackend, TTSBackendError, torch_cuda_available
from txtcaster_engine.tts.pipeline import SegmentInput, SegmentResult, synthesize_segments
from txtcaster_engine.tts.voice import resolve_voice

DEFAULT_WORKERS = 2

logger = logging.getLogger(__name__)

TTS_TASK = "tts"

FAKE_TTS_MODEL_ID = "fake-tts"
KOKORO_FAMILY = "kokoro"
CHATTERBOX_FAMILY = "chatterbox"


def resolve_family(model_id: str) -> str:
    return model_id.split("-", 1)[0]


def tts_key(model_id: str, model_dir: str, device: str) -> tuple[str, str, str]:
    return model_key(TTS_TASK, model_id, f"{model_dir}|{device}")


def create_backend(
    model_id: str, model_dir: str, device: str, allow_fake_tts: bool = False
) -> TTSBackend:
    if model_id == FAKE_TTS_MODEL_ID:
        if not allow_fake_tts:
            raise TTSBackendError(f"unsupported tts model: {model_id}")
        from txtcaster_engine.tts.backend import FakeBackend

        return FakeBackend()
    family = resolve_family(model_id)
    if family == KOKORO_FAMILY:
        from txtcaster_engine.tts.kokoro_backend import KokoroBackend

        return KokoroBackend(model_dir, device=device)
    if family == CHATTERBOX_FAMILY:
        from txtcaster_engine.tts.chatterbox_backend import ChatterboxBackend

        return ChatterboxBackend(model_dir, device=device)
    raise TTSBackendError(f"unsupported tts model: {model_id}")


def _workers(resource_plan: dict[str, object]) -> int:
    raw = resource_plan.get("tts_workers")
    if isinstance(raw, int) and raw > 0:
        return raw
    return DEFAULT_WORKERS


def tts_events(
    segments: list[dict[str, object]],
    voice_profile: dict[str, object],
    model_id: str,
    model_dir: str,
    cache_dir: str,
    resource_plan: dict[str, object],
    job_id: str,
    registry: CancelRegistry,
    allow_fake_tts: bool = False,
    residency: ResidencyManager | None = None,
    stage: str | None = None,
    language: str | None = None,
) -> AsyncIterator[Event]:
    def work(emit: Callable[[Event], None]) -> None:
        inputs = [
            SegmentInput(idx=int(str(seg.get("idx", 0))), text=str(seg.get("text", "")))
            for seg in segments
        ]
        voice_id, params = resolve_voice(voice_profile)
        try:
            planned = parse_device(resource_plan)
        except PlanError as exc:
            raise IngestError(exc.code, exc.message) from exc
        device = "cpu"
        flags: list[str] = []
        if planned.is_gpu:
            fake_capable = allow_fake_tts and model_id == FAKE_TTS_MODEL_ID
            if fake_capable or torch_cuda_available():
                device = planned.torch_device() or "cuda"
            else:
                flags.append("tts_cpu_fallback")

        def make_backend() -> TTSBackend:
            return create_backend(model_id, model_dir, device, allow_fake_tts)

        try:
            if residency is None:
                lease = None
                backend = make_backend()
            else:
                lease = residency.acquire(
                    tts_key(model_id, model_dir, device), make_backend, stage=stage
                )
                backend = lease.backend
        except ResidencyError as exc:
            raise IngestError(exc.code, exc.message) from exc
        except TTSBackendError as exc:
            raise IngestError("ERR_TTS_MODEL", exc.message) from exc

        total = len(inputs)
        done = 0

        def on_result(result: SegmentResult) -> None:
            nonlocal done
            done += 1
            logger.debug(
                "tts segment idx=%d cache_hit=%s duration_ms=%d wav_bytes=%d flags=%d",
                result.idx,
                result.cache_hit,
                result.duration_ms,
                result.wav_bytes,
                len(result.flags),
            )
            emit(
                (
                    "result",
                    {
                        "stage": "tts",
                        "done": done,
                        "total": total,
                        "result": result.to_json(),
                    },
                )
            )

        try:
            try:
                backend.check_voice(voice_id, params)
            except TTSBackendError as exc:
                raise IngestError("ERR_TTS_MODEL", exc.message) from exc
            synthesize_segments(
                inputs,
                backend,
                model_id,
                voice_id,
                params,
                cache_dir,
                workers=_workers(resource_plan),
                on_result=on_result,
                is_cancelled=lambda: registry.is_cancelled(job_id),
                language=language,
            )
        finally:
            if residency is not None and lease is not None:
                residency.release(lease)
                if stage is not None:
                    residency.evict_stage(stage)
        emit(("done", {"stage": "tts", "done": done, "total": total, "flags": flags}))

    return stage_events(job_id, registry, work, stage="tts")
