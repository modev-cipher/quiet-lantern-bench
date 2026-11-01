import logging
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass
from pathlib import Path

from txtcaster_engine.cancel import CancelRegistry
from txtcaster_engine.ingest.errors import IngestCancelled, IngestError
from txtcaster_engine.llm.backend import LLMBackendError, ResourcePlan
from txtcaster_engine.residency import Lease, ResidencyManager, model_key
from txtcaster_engine.stage import Event, stage_events
from txtcaster_engine.vision.backend import LlamaVisionBackend, VisionBackend
from txtcaster_engine.vision.prompts import CONFIDENCE_FLOOR, parse_caption, prompt_for

MAX_TOKENS = 256

VISION_TASK = "vision"
VISION_VARIANT = "mmproj"

VisionFactory = Callable[[str, str, ResourcePlan, int | None, str], VisionBackend]

_LOG = logging.getLogger(__name__)


def vision_key(model_id: str) -> tuple[str, str, str]:
    return model_key(VISION_TASK, model_id, VISION_VARIANT)


def default_vision_factory(
    model_path: str,
    mmproj_path: str,
    plan: ResourcePlan,
    threads: int | None,
    family: str,
) -> VisionBackend:
    return LlamaVisionBackend(model_path, mmproj_path, plan, threads, family)


@dataclass(frozen=True)
class VisionRegionInput:
    region_id: str
    crop_path: str
    region_type: str


def vision_events(
    regions: list[VisionRegionInput],
    model_path: str,
    mmproj_path: str,
    resource_plan: dict[str, object],
    model_family: str,
    threads: int | None,
    region_timeout_s: float | None,
    job_id: str,
    registry: CancelRegistry,
    factory: VisionFactory = default_vision_factory,
    residency: ResidencyManager | None = None,
    lease: Lease | None = None,
) -> AsyncIterator[Event]:
    def work(emit: Callable[[Event], None]) -> None:
        flags: set[str] = set()
        try:
            if residency is not None and lease is not None:
                residency.mark_loading(lease)
            resident = lease.backend if lease is not None else None
            if resident is not None:
                backend = resident
            else:
                try:
                    plan = ResourcePlan.from_json(resource_plan)
                    backend = factory(model_path, mmproj_path, plan, threads, model_family)
                except LLMBackendError as exc:
                    raise IngestError(exc.code, exc.message) from exc
                if residency is not None and lease is not None:
                    try:
                        residency.attach(lease, backend)
                    except Exception:
                        try:
                            backend.unload()
                        except Exception:
                            _LOG.warning(
                                "vision backend failed to unload after an attach error",
                                exc_info=True,
                            )
                        raise
            worker_lease: Lease | None = None
            if residency is not None and lease is not None:
                worker_lease = residency.retain(lease.key)
            captions: list[dict[str, object]] = []
            total = len(regions)

            def is_cancelled() -> bool:
                return registry.is_cancelled(job_id)

            try:
                for done, region in enumerate(regions, start=1):
                    if is_cancelled():
                        raise IngestCancelled()
                    payload = _describe_region(
                        backend, region, region_timeout_s, flags, is_cancelled
                    )
                    captions.append(payload)
                    emit(("caption", payload))
                    emit(("progress", {"stage": "vision", "figure": done, "total": total}))
            finally:
                if residency is None or lease is None:
                    try:
                        backend.unload()
                    except Exception:
                        _LOG.warning("vision backend failed to unload", exc_info=True)
                elif worker_lease is not None:
                    residency.release(worker_lease)
            emit(("result", {"captions": captions, "flags": sorted(flags)}))
        finally:
            if residency is not None and lease is not None:
                residency.release(lease)

    return stage_events(job_id, registry, work, stage="vision")


def _describe_region(
    backend: VisionBackend,
    region: VisionRegionInput,
    region_timeout_s: float | None,
    flags: set[str],
    is_cancelled: Callable[[], bool],
) -> dict[str, object]:
    generic: dict[str, object] = {
        "region_id": region.region_id,
        "caption": None,
        "explanation": None,
        "confidence": 0.0,
    }
    crop = Path(region.crop_path)
    if not crop.is_file():
        flags.add("vision_crop_missing")
        return generic
    image_png = crop.read_bytes()
    try:
        raw = backend.describe(
            image_png, prompt_for(region.region_type), MAX_TOKENS, region_timeout_s, is_cancelled
        )
    except LLMBackendError as exc:
        if exc.code == "ERR_OOM":
            raise IngestError(exc.code, exc.message) from exc
        if exc.code == "ERR_CANCELLED":
            raise IngestCancelled() from exc
        if exc.code == "ERR_VISION_TIMEOUT":
            flags.add("vision_budget_exceeded")
        else:
            flags.add("vision_generate_failed")
        return generic
    parsed = parse_caption(region.region_id, raw)
    if parsed is None:
        flags.add("vision_output_malformed")
        return generic
    if parsed.confidence < CONFIDENCE_FLOOR:
        flags.add("vision_low_confidence")
        return generic
    return parsed.to_json()
