import asyncio
import contextlib
import threading
import uuid
from collections.abc import AsyncIterator, Mapping
from concurrent.futures import Future, ThreadPoolExecutor
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Literal

from fastapi import FastAPI, HTTPException, Request
from fastapi.concurrency import run_in_threadpool
from pydantic import BaseModel, ConfigDict, Field, SecretStr, field_validator, model_validator
from starlette.responses import JSONResponse, StreamingResponse

from txtcaster_engine import ENGINE_API, __version__
from txtcaster_engine.align.backend import ct2_cuda_available
from txtcaster_engine.align.endpoint import align_events
from txtcaster_engine.auth import BearerTokenMiddleware
from txtcaster_engine.cancel import CancelRegistry
from txtcaster_engine.ingest.endpoint import ingest_events
from txtcaster_engine.ingest.errors import IngestError
from txtcaster_engine.ingest.fallback.keys import ProviderKeys, Secret
from txtcaster_engine.ingest.fallback.runner import FallbackSettings
from txtcaster_engine.ingest.render_endpoint import render_events
from txtcaster_engine.ingest.version import (
    DEFAULT_SCALE,
    extractor_base_version,
    geometry_version,
)
from txtcaster_engine.layout.endpoint import layout_events
from txtcaster_engine.llm import ggml_runtime
from txtcaster_engine.llm.backend import LLMBackendError, ResourcePlan, gpu_offload_supported
from txtcaster_engine.llm.probe import probe_tok_s
from txtcaster_engine.llm.registry import LLMRegistry, _default_factory, llm_key
from txtcaster_engine.loopnoise import quiet_resets_during_close
from txtcaster_engine.oom import is_device_unavailable, is_oom
from txtcaster_engine.residency import (
    UNLOAD_LEASE_DRAIN_S,
    VISION_STAGE,
    ResidencyError,
    ResidencyManager,
    default_residency_policy,
)
from txtcaster_engine.script.assemble import FigureAnchor
from txtcaster_engine.script.canonical import parse_pages
from txtcaster_engine.script.endpoint import script_events
from txtcaster_engine.script.pipeline import (
    assemble_script_units,
    segments_from_lecture,
    unit_to_json,
)
from txtcaster_engine.sse import sse_response
from txtcaster_engine.tts.backend import torch_cuda_available
from txtcaster_engine.tts.endpoint import tts_events
from txtcaster_engine.vision.endpoint import (
    VisionRegionInput,
    default_vision_factory,
    vision_events,
    vision_key,
)


class CancelRequest(BaseModel):
    job_id: str


class FallbackRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    openai_key: SecretStr | None = None
    google_key: SecretStr | None = None
    theta: float = Field(default=0.6, ge=0.0, le=1.0)
    cache_dir: str | None = None
    cap_usd: float | None = Field(default=None, ge=0.0)
    cap_fraction: float = Field(default=0.05, ge=0.0, le=1.0)
    cap_pages: int = Field(default=200, ge=1)

    def settings(self) -> FallbackSettings:
        return FallbackSettings(
            keys=ProviderKeys(
                openai=Secret(self.openai_key.get_secret_value()) if self.openai_key else None,
                google=Secret(self.google_key.get_secret_value()) if self.google_key else None,
            ),
            theta=self.theta,
            cache_dir=Path(self.cache_dir) if self.cache_dir else None,
            cap_usd=self.cap_usd,
            cap_fraction=self.cap_fraction,
            cap_pages=self.cap_pages,
        )


class IngestRequest(BaseModel):
    book_path: str
    book_id: str
    docling_artifacts_dir: str | None = None
    docling_artifacts_sha: str | None = None
    layout: bool = True
    fallback: FallbackRequest | None = None


class ProbeRequest(BaseModel):
    book_path: str


class RenderPagesRequest(BaseModel):
    book_path: str
    content_hash: str
    pages: list[int]
    cache_dir: str
    scale: float = DEFAULT_SCALE
    with_geometry: bool = False


class LayoutRequest(BaseModel):
    book_id: str
    book_path: str
    pages: list[int]
    content_hash: str = Field(pattern=r"^[0-9a-f]{64}$")
    cache_dir: str
    docling_artifacts_dir: str | None = None
    device: str = "cpu"


class VisionRegion(BaseModel):
    region_id: str
    crop_path: str
    region_type: str


class VisionRequest(BaseModel):
    regions: list[VisionRegion]
    model_id: str
    model_path: str
    mmproj_path: str
    model_family: str = ""
    resource_plan: dict[str, object]
    threads: int | None = None
    region_timeout_s: float | None = Field(default=None, gt=0)


class ScriptFigure(BaseModel):
    region_id: str = Field(min_length=1, max_length=64, pattern=r"^[A-Za-z0-9_-]+$")
    page_no: int = Field(ge=1)
    bbox: tuple[float, float, float, float]
    region_type: str = Field(min_length=1)
    caption: str | None = None
    explanation: str | None = None
    confidence: float = Field(default=1.0, ge=0.0, le=1.0)

    @field_validator("region_id", "region_type")
    @classmethod
    def non_blank(cls, value: str) -> str:
        if not value.strip():
            raise ValueError("must not be blank")
        return value.strip()

    @field_validator("caption", "explanation")
    @classmethod
    def blank_to_none(cls, value: str | None) -> str | None:
        return value.strip() or None if value is not None else None

    @model_validator(mode="after")
    def ordered_bbox(self) -> "ScriptFigure":
        x0, y0, x1, y1 = self.bbox
        if not all(value == value and abs(value) != float("inf") for value in self.bbox):
            raise ValueError("bbox coordinates must be finite")
        if x1 <= x0 or y1 <= y0:
            raise ValueError("bbox must have positive width and height")
        return self


class ScriptRequest(BaseModel):
    scope: dict[str, object]
    mode: Literal["narrator", "teacher"] = "narrator"
    model_id: str | None = None
    figure_captions: list[ScriptFigure] = Field(default_factory=list)
    pages: list[dict[str, object]] = Field(default_factory=list)
    section_bounds: list[tuple[int, int]] = Field(default_factory=list)


class ScriptUnitsRequest(BaseModel):
    scope: dict[str, object]
    figure_captions: list[ScriptFigure] = Field(default_factory=list)
    pages: list[dict[str, object]] = Field(default_factory=list)
    section_bounds: list[tuple[int, int]] = Field(default_factory=list)


class LectureUnit(BaseModel):
    """Mirror of unit_from_json's contract: the four keys it reads directly are
    required; the rest it fetches with .get, so they default to None here."""

    chapter_idx: int
    page_no: int
    kind: str
    text: str
    figure_id: str | None = None
    region_type: str | None = None
    caption: str | None = None
    explanation: str | None = None
    block_index: int | None = None
    label: str | None = None


class LectureChunk(BaseModel):
    units: list[LectureUnit]
    texts: list[str] | None = None


class SegmentsFromLectureRequest(BaseModel):
    chunks: list[LectureChunk]


class TtsRequest(BaseModel):
    segments: list[dict[str, object]]
    voice_profile: dict[str, object]
    model_id: str
    model_dir: str
    cache_dir: str
    resource_plan: dict[str, object] = {}
    stage: str | None = None
    language: str | None = None


class AlignRequest(BaseModel):
    segments: list[dict[str, object]]
    model_id: str
    model_dir: str
    cache_dir: str
    language: str | None = None
    resource_plan: dict[str, object] = {}
    stage: str | None = None


class CancelResponse(BaseModel):
    job_id: str
    cancelled: bool


class ShutdownResponse(BaseModel):
    cancelled: bool


class ModelLoadRequest(BaseModel):
    model_path: str
    resource_plan: dict[str, object]
    threads: int | None = None


class ModelLoadResponse(BaseModel):
    model_id: str
    loaded: bool


class ModelUnloadRequest(BaseModel):
    model_id: str


class ModelUnloadResponse(BaseModel):
    model_id: str
    unloaded: bool


class BenchmarkLlmRequest(BaseModel):
    model_path: str
    resource_plan: dict[str, object]
    threads: int | None = None
    max_tokens: int = 64


class BenchmarkLlmResponse(BaseModel):
    device: str
    tok_s: float
    tokens: int
    elapsed_s: float


def create_app(
    token: str,
    allow_fake_tts: bool = False,
    allow_fake_llm: bool = False,
    allow_fake_align: bool = False,
) -> FastAPI:
    with contextlib.suppress(ImportError):
        import transformers  # noqa: F401

    @asynccontextmanager
    async def lifespan(_app: FastAPI) -> AsyncIterator[None]:
        quiet_resets_during_close(asyncio.get_running_loop())
        yield

    app = FastAPI(title="txtCaster Engine", version=__version__, lifespan=lifespan)

    @app.exception_handler(IngestError)
    async def ingest_refusal(_request: Request, exc: IngestError) -> JSONResponse:
        return JSONResponse(
            status_code=422, content={"detail": {"code": exc.code, "message": exc.message}}
        )

    @app.exception_handler(Exception)
    async def unstructured_failure(_request: Request, exc: Exception) -> JSONResponse:
        if is_device_unavailable(exc):
            code, message = "ERR_DEVICE_UNAVAILABLE", "compute device unavailable"
        elif is_oom(exc):
            code, message = "ERR_OOM", "out of memory"
        else:
            code, message = "ERR_INTERNAL", "internal error"
        return JSONResponse(status_code=500, content={"detail": {"code": code, "message": message}})

    app.add_middleware(BearerTokenMiddleware, token=token)
    app.state.cancel_registry = CancelRegistry()
    app.state.allow_fake_tts = allow_fake_tts
    app.state.allow_fake_align = allow_fake_align
    if allow_fake_llm:
        from txtcaster_engine.llm.fake_backend import fake_aware_factory
        from txtcaster_engine.vision.fake_backend import fake_aware_vision_factory

        app.state.llm_factory = fake_aware_factory
        app.state.vision_factory = fake_aware_vision_factory
    else:
        app.state.llm_factory = _default_factory
        app.state.vision_factory = default_vision_factory
    app.state.residency = ResidencyManager(policy=default_residency_policy())
    app.state.llm_registry = LLMRegistry(
        factory=app.state.llm_factory, residency=app.state.residency
    )

    probes: dict[str, Future[bool]] = {}
    probe_lock = threading.Lock()

    def capability_probes() -> dict[str, Future[bool]]:
        with probe_lock:
            if not probes:
                pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="capability-probe")
                probes["gpu_offload"] = pool.submit(gpu_offload_supported)
                probes["torch_cuda"] = pool.submit(torch_cuda_available)
                probes["ct2_cuda"] = pool.submit(ct2_cuda_available)
                pool.shutdown(wait=False)
        return probes

    def probed(probe: Future[bool]) -> bool | None:
        if not probe.done():
            return None
        try:
            return bool(probe.result())
        except Exception:
            return False

    @app.get("/health")
    async def health() -> dict[str, object]:
        pending = capability_probes()
        payload: dict[str, object] = {
            "status": "ok",
            "version": __version__,
            "engine_api": ENGINE_API,
            "gpu_offload": True if allow_fake_llm else probed(pending["gpu_offload"]),
            "gpu_devices": None,
            "torch_cuda": probed(pending["torch_cuda"]),
            "ct2_cuda": probed(pending["ct2_cuda"]),
            "geometry_version": geometry_version(),
            "extract_version": extractor_base_version(),
        }
        if pending["gpu_offload"].done():
            runtime = ggml_runtime.load().with_current_memory()
            if runtime.error is None:
                payload["gpu_devices"] = [device.to_json() for device in runtime.devices]
            else:
                del payload["gpu_devices"]
        return payload

    @app.post("/cancel")
    async def cancel(body: CancelRequest, request: Request) -> CancelResponse:
        registry: CancelRegistry = request.app.state.cancel_registry
        registry.request_cancel(body.job_id)
        return CancelResponse(job_id=body.job_id, cancelled=True)

    @app.post("/shutdown")
    async def shutdown(request: Request) -> ShutdownResponse:
        registry: CancelRegistry = request.app.state.cancel_registry
        registry.request_cancel_all()
        return ShutdownResponse(cancelled=True)

    @app.post("/models/load")
    async def models_load(body: ModelLoadRequest, request: Request) -> ModelLoadResponse:
        registry: LLMRegistry = request.app.state.llm_registry
        try:
            plan = ResourcePlan.from_json(body.resource_plan)
            await run_in_threadpool(registry.load, body.model_path, plan, body.threads)
        except LLMBackendError as exc:
            status = 409 if exc.code == "ERR_BUSY" else 422 if exc.code == "ERR_PLAN" else 500
            raise HTTPException(
                status_code=status, detail={"code": exc.code, "message": exc.message}
            ) from exc
        return ModelLoadResponse(model_id=plan.model_id, loaded=True)

    @app.post("/models/unload")
    async def models_unload(body: ModelUnloadRequest, request: Request) -> ModelUnloadResponse:
        registry: LLMRegistry = request.app.state.llm_registry
        try:
            unloaded = await run_in_threadpool(registry.unload, body.model_id, UNLOAD_LEASE_DRAIN_S)
        except LLMBackendError as exc:
            raise HTTPException(
                status_code=409, detail={"code": exc.code, "message": exc.message}
            ) from exc
        return ModelUnloadResponse(model_id=body.model_id, unloaded=unloaded)

    @app.post("/benchmark/llm")
    async def benchmark_llm(body: BenchmarkLlmRequest, request: Request) -> BenchmarkLlmResponse:
        registry: LLMRegistry = request.app.state.llm_registry
        loaded = registry.loaded_model_id()
        if loaded is not None:
            raise HTTPException(
                status_code=409,
                detail={
                    "code": "ERR_BUSY",
                    "message": f"model '{loaded}' is loaded; unload before benchmarking",
                },
            )
        try:
            plan = ResourcePlan.from_json(body.resource_plan)
        except LLMBackendError as exc:
            raise HTTPException(
                status_code=422 if exc.code in {"ERR_PLAN", "ERR_PROBE"} else 500,
                detail={"code": exc.code, "message": exc.message},
            ) from exc

        residency: ResidencyManager = request.app.state.residency
        key = llm_key(plan.model_id)
        try:
            lease = residency.reserve(key, stage="benchmark")
        except ResidencyError as exc:
            raise HTTPException(
                status_code=409, detail={"code": exc.code, "message": exc.message}
            ) from exc

        try:
            result = await run_in_threadpool(
                probe_tok_s,
                body.model_path,
                plan,
                body.threads,
                body.max_tokens,
                request.app.state.llm_factory,
            )
        except LLMBackendError as exc:
            status = 422 if exc.code in {"ERR_PLAN", "ERR_PROBE"} else 500
            raise HTTPException(
                status_code=status, detail={"code": exc.code, "message": exc.message}
            ) from exc
        finally:
            residency.release(lease)
            residency.unload_key(key)
        return BenchmarkLlmResponse(
            device=result.device,
            tok_s=result.tok_s,
            tokens=result.tokens,
            elapsed_s=result.elapsed_s,
        )

    @app.post("/probe")
    async def probe(body: ProbeRequest) -> dict[str, object]:
        from txtcaster_engine.ingest.probe import probe_document

        return probe_document(body.book_path)

    @app.post("/ingest")
    async def ingest(body: IngestRequest, request: Request) -> StreamingResponse:
        registry: CancelRegistry = request.app.state.cancel_registry
        job_id = request.headers.get("X-Job-Id") or uuid.uuid4().hex
        return sse_response(
            ingest_events(
                body.book_path,
                body.book_id,
                job_id,
                registry,
                body.docling_artifacts_dir,
                body.docling_artifacts_sha,
                body.layout,
                fallback=body.fallback.settings() if body.fallback else None,
            )
        )

    @app.post("/render_pages")
    async def render(body: RenderPagesRequest, request: Request) -> StreamingResponse:
        registry: CancelRegistry = request.app.state.cancel_registry
        job_id = request.headers.get("X-Job-Id") or uuid.uuid4().hex
        return sse_response(
            render_events(
                body.book_path,
                body.content_hash,
                body.pages,
                body.cache_dir,
                body.scale,
                body.with_geometry,
                job_id,
                registry,
            )
        )

    @app.post("/layout")
    async def layout(body: LayoutRequest, request: Request) -> StreamingResponse:
        registry: CancelRegistry = request.app.state.cancel_registry
        job_id = request.headers.get("X-Job-Id") or uuid.uuid4().hex
        return sse_response(
            layout_events(
                body.book_path,
                body.pages,
                body.content_hash,
                body.cache_dir,
                job_id,
                registry,
                body.docling_artifacts_dir,
                device=body.device,
            )
        )

    @app.post("/vision")
    async def vision(body: VisionRequest, request: Request) -> StreamingResponse:
        registry: CancelRegistry = request.app.state.cancel_registry
        job_id = request.headers.get("X-Job-Id") or uuid.uuid4().hex
        residency: ResidencyManager = request.app.state.residency
        try:
            lease = residency.reserve(vision_key(body.model_id), stage=VISION_STAGE)
        except ResidencyError as exc:
            raise HTTPException(
                status_code=409,
                detail={"code": exc.code, "message": exc.message},
            ) from exc
        regions = [
            VisionRegionInput(
                region_id=r.region_id, crop_path=r.crop_path, region_type=r.region_type
            )
            for r in body.regions
        ]

        async def events() -> AsyncIterator[tuple[str, Mapping[str, object]]]:
            try:
                async for event in vision_events(
                    regions,
                    body.model_path,
                    body.mmproj_path,
                    body.resource_plan,
                    body.model_family,
                    body.threads,
                    body.region_timeout_s,
                    job_id,
                    registry,
                    factory=request.app.state.vision_factory,
                    residency=residency,
                    lease=lease,
                ):
                    yield event
            finally:
                residency.release(lease)

        return sse_response(events())

    @app.post("/script")
    async def script(body: ScriptRequest, request: Request) -> StreamingResponse:
        registry: CancelRegistry = request.app.state.cancel_registry
        job_id = request.headers.get("X-Job-Id") or uuid.uuid4().hex
        backend = None
        residency: ResidencyManager = request.app.state.residency
        llm_slot = None
        if body.model_id is not None:
            llm_registry: LLMRegistry = request.app.state.llm_registry
            try:
                backend = llm_registry.get(body.model_id)
            except LLMBackendError as exc:
                raise HTTPException(
                    status_code=409, detail={"code": exc.code, "message": exc.message}
                ) from exc
            llm_slot = llm_key(body.model_id)
        return sse_response(
            script_events(
                parse_pages(body.pages),
                body.scope,
                body.mode,
                job_id,
                registry,
                backend=backend,
                figures=[
                    FigureAnchor(
                        region_id=figure.region_id,
                        page_no=figure.page_no,
                        bbox=figure.bbox,
                        region_type=figure.region_type,
                        caption=figure.caption,
                        explanation=figure.explanation,
                        confidence=figure.confidence,
                    )
                    for figure in body.figure_captions
                ],
                residency=residency,
                llm_slot=llm_slot,
                section_bounds=body.section_bounds,
            )
        )

    @app.post("/script/units")
    async def script_units(body: ScriptUnitsRequest, request: Request) -> dict[str, object]:
        units = assemble_script_units(
            parse_pages(body.pages),
            body.scope,
            figures=[
                FigureAnchor(
                    region_id=figure.region_id,
                    page_no=figure.page_no,
                    bbox=figure.bbox,
                    region_type=figure.region_type,
                    caption=figure.caption,
                    explanation=figure.explanation,
                    confidence=figure.confidence,
                )
                for figure in body.figure_captions
            ],
            section_bounds=body.section_bounds,
        )
        return {"units": [unit_to_json(unit) for unit in units]}

    @app.post("/script/segments")
    async def script_segments(
        body: SegmentsFromLectureRequest, request: Request
    ) -> dict[str, object]:
        segments = segments_from_lecture(
            [
                {
                    "units": [unit.model_dump() for unit in chunk.units],
                    "texts": chunk.texts,
                }
                for chunk in body.chunks
            ]
        )
        return {"segments": [segment.to_json() for segment in segments]}

    @app.post("/tts")
    async def tts(body: TtsRequest, request: Request) -> StreamingResponse:
        registry: CancelRegistry = request.app.state.cancel_registry
        job_id = request.headers.get("X-Job-Id") or uuid.uuid4().hex
        return sse_response(
            tts_events(
                body.segments,
                body.voice_profile,
                body.model_id,
                body.model_dir,
                body.cache_dir,
                body.resource_plan,
                job_id,
                registry,
                allow_fake_tts=bool(request.app.state.allow_fake_tts),
                residency=request.app.state.residency,
                stage=body.stage,
                language=body.language,
            )
        )

    @app.post("/align")
    async def align(body: AlignRequest, request: Request) -> StreamingResponse:
        registry: CancelRegistry = request.app.state.cancel_registry
        job_id = request.headers.get("X-Job-Id") or uuid.uuid4().hex
        return sse_response(
            align_events(
                body.segments,
                body.model_id,
                body.model_dir,
                body.cache_dir,
                body.language,
                body.resource_plan,
                job_id,
                registry,
                allow_fake_align=bool(request.app.state.allow_fake_align),
                residency=request.app.state.residency,
                stage=body.stage,
            )
        )

    @app.get("/debug/sse-test")
    async def sse_test() -> StreamingResponse:
        async def events() -> AsyncIterator[tuple[str, Mapping[str, object]]]:
            total = 3
            for step in range(1, total):
                yield "progress", {"step": step, "total": total}
            yield "done", {"step": total, "total": total}

        return sse_response(events())

    return app
