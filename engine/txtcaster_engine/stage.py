import asyncio
import logging
import time
from collections.abc import AsyncGenerator, Callable, Mapping

from txtcaster_engine.cancel import CancelRegistry
from txtcaster_engine.ingest.errors import IngestCancelled, IngestError
from txtcaster_engine.logctx import job_id_var
from txtcaster_engine.oom import is_device_unavailable, is_oom

Event = tuple[str, Mapping[str, object]]

KEEPALIVE_INTERVAL_S = 15.0

logger = logging.getLogger(__name__)


async def stage_events(
    job_id: str,
    registry: CancelRegistry,
    work: Callable[[Callable[[Event], None]], None],
    keepalive_interval_s: float = KEEPALIVE_INTERVAL_S,
    stage: str = "stage",
) -> AsyncGenerator[Event, None]:
    if keepalive_interval_s <= 0:
        raise ValueError("keepalive_interval_s must be positive")
    loop = asyncio.get_running_loop()
    queue: asyncio.Queue[Event | None] = asyncio.Queue()

    def emit(event: Event) -> None:
        loop.call_soon_threadsafe(queue.put_nowait, event)

    def run() -> None:
        job_token = job_id_var.set(job_id)
        started = time.perf_counter()
        status = "completed"
        logger.info("stage %s enter", stage)
        try:
            if registry.shutdown_requested():
                raise IngestCancelled()
            work(emit)
        except IngestCancelled:
            status = "cancelled"
            emit(("cancelled", {"job_id": job_id}))
        except IngestError as exc:
            if is_device_unavailable(exc):
                status = "error code=ERR_DEVICE_UNAVAILABLE"
                emit(("error", {"code": "ERR_DEVICE_UNAVAILABLE", "message": exc.message}))
            elif is_oom(exc):
                status = "error code=ERR_OOM"
                emit(("error", {"code": "ERR_OOM", "message": exc.message}))
            else:
                status = f"error code={exc.code}"
                emit(("error", {"code": exc.code, "message": exc.message}))
            logger.warning("stage %s refused: %s cause_type=%s", stage, status, type(exc).__name__)
        except Exception as exc:
            if is_device_unavailable(exc):
                status = "error code=ERR_DEVICE_UNAVAILABLE"
                logger.warning("stage %s device unavailable: %s", stage, exc)
                emit(("error", {"code": "ERR_DEVICE_UNAVAILABLE", "message": str(exc)}))
            elif is_oom(exc):
                status = "error code=ERR_OOM"
                logger.warning("stage %s oom: %s", stage, exc)
                emit(("error", {"code": "ERR_OOM", "message": "out of memory"}))
            else:
                status = "error code=ERR_INTERNAL"
                logger.exception("stage %s failed", stage)
                emit(("error", {"code": "ERR_INTERNAL", "message": "internal error"}))
        finally:
            logger.info(
                "stage %s exit: status=%s duration_s=%.2f",
                stage,
                status,
                time.perf_counter() - started,
            )
            job_id_var.reset(job_token)
            loop.call_soon_threadsafe(queue.put_nowait, None)

    task = loop.run_in_executor(None, run)
    try:
        cancellation_observed = False
        while True:
            try:
                event = await asyncio.wait_for(queue.get(), timeout=keepalive_interval_s)
            except TimeoutError:
                if registry.is_cancelled(job_id):
                    cancellation_observed = True
                    continue
                if cancellation_observed:
                    continue
                yield ("keepalive", {})
                continue
            if event is None:
                break
            yield event
    finally:
        registry.request_cancel(job_id)
        task.add_done_callback(lambda _done: registry.clear(job_id))
        await asyncio.shield(task)
