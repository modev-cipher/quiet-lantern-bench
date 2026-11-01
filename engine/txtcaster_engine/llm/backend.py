import functools
import os
from dataclasses import dataclass

from txtcaster_engine.device import DevicePlan, PlanError, parse_device


def available_cpus() -> int:
    try:
        return len(os.sched_getaffinity(0))
    except (AttributeError, OSError):
        return os.cpu_count() or 1


def resolve_threads(threads: int | None) -> tuple[int, int]:
    if threads is not None:
        return threads, threads
    cpus = available_cpus()
    return max(1, cpus // 2), max(1, cpus - 2)


@functools.cache
def gpu_offload_supported() -> bool:
    try:
        from llama_cpp import llama_supports_gpu_offload
    except Exception:
        return False
    try:
        return bool(llama_supports_gpu_offload())
    except Exception:
        return False


class LLMBackendError(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


@dataclass(frozen=True)
class ResourcePlan:
    model_id: str
    device: DevicePlan
    n_gpu_layers: int
    n_layers_total: int
    kv_offload: bool
    ctx: int
    batch: int

    @staticmethod
    def from_json(raw: dict[str, object]) -> "ResourcePlan":
        def read_int(key: str, minimum: int) -> int:
            value = raw.get(key)
            if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
                raise LLMBackendError(
                    "ERR_PLAN", f"resource_plan.{key} must be an int >= {minimum}"
                )
            return value

        model_id = raw.get("model_id")
        if not isinstance(model_id, str) or not model_id:
            raise LLMBackendError("ERR_PLAN", "resource_plan.model_id must be a non-empty string")
        try:
            device = parse_device(raw)
        except PlanError as exc:
            raise LLMBackendError(exc.code, exc.message) from exc
        kv_offload = raw.get("kv_offload", True)
        if not isinstance(kv_offload, bool):
            raise LLMBackendError("ERR_PLAN", "resource_plan.kv_offload must be a bool")
        n_gpu_layers = read_int("n_gpu_layers", 0)
        n_layers_total = read_int("n_layers_total", 1)
        if n_gpu_layers > n_layers_total + 1:
            raise LLMBackendError(
                "ERR_PLAN",
                "resource_plan.n_gpu_layers exceeds the full offload slot count",
            )
        return ResourcePlan(
            model_id=model_id,
            device=device,
            n_gpu_layers=0 if not device.is_gpu else n_gpu_layers,
            n_layers_total=n_layers_total,
            kv_offload=kv_offload,
            ctx=read_int("ctx", 512),
            batch=read_int("batch", 1),
        )


class LLMBackend:
    def generate(
        self,
        prompt: str,
        max_tokens: int,
        temperature: float = 0.0,
        stop: list[str] | None = None,
    ) -> str:
        raise NotImplementedError

    def context_window(self) -> int:
        raise NotImplementedError

    def unload(self) -> None:
        raise NotImplementedError
