from collections.abc import Callable
from typing import cast

from txtcaster_engine.llm.backend import LLMBackend, LLMBackendError, ResourcePlan
from txtcaster_engine.residency import (
    SCRIPT_STAGE,
    ResidencyError,
    ResidencyManager,
    model_key,
)

BackendFactory = Callable[[str, ResourcePlan, int | None], LLMBackend]

LLM_TASK = "llm"
LLM_VARIANT = "text"


def _default_factory(model_path: str, plan: ResourcePlan, threads: int | None) -> LLMBackend:
    from txtcaster_engine.llm.llama_backend import LlamaBackend

    return LlamaBackend(model_path, plan, threads)


def llm_key(model_id: str) -> tuple[str, str, str]:
    return model_key(LLM_TASK, model_id, LLM_VARIANT)


class LLMRegistry:
    def __init__(
        self,
        factory: BackendFactory = _default_factory,
        residency: ResidencyManager | None = None,
    ) -> None:
        self._factory = factory
        self._residency = residency or ResidencyManager()

    @property
    def residency(self) -> ResidencyManager:
        return self._residency

    def load(self, model_path: str, plan: ResourcePlan, threads: int | None = None) -> LLMBackend:
        key = llm_key(plan.model_id)
        existing = self._residency.get(key)
        if existing is not None:
            return cast(LLMBackend, existing)
        try:
            for resident_key in self._residency.resident_keys():
                if resident_key[0] == LLM_TASK and resident_key != key:
                    self._residency.unload_key(resident_key)
        except ResidencyError as exc:
            raise LLMBackendError(exc.code, exc.message) from exc
        try:
            lease = self._residency.acquire(
                key,
                lambda: self._factory(model_path, plan, threads),
                stage=SCRIPT_STAGE,
                pinned=True,
            )
        except ResidencyError as exc:
            raise LLMBackendError(exc.code, exc.message) from exc
        backend = cast(LLMBackend, lease.backend)
        self._residency.release(lease)
        return backend

    def get(self, model_id: str) -> LLMBackend:
        backend = self._residency.get(llm_key(model_id))
        if backend is None:
            raise LLMBackendError("ERR_LLM_UNLOADED", f"model not loaded: {model_id}")
        return cast(LLMBackend, backend)

    def unload(self, model_id: str, wait_s: float = 0.0) -> bool:
        try:
            return self._residency.unload_model(LLM_TASK, model_id, wait_s=wait_s)
        except ResidencyError as exc:
            raise LLMBackendError(exc.code, exc.message) from exc

    def loaded_model_id(self) -> str | None:
        for task, model_id, _variant in self._residency.resident_keys():
            if task == LLM_TASK:
                return model_id
        return None
