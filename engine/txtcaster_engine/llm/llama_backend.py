# Provenance (AGENTS.md §5a): llama-cpp-python 0.3.33 (pinned), the LLM slot
# behind Teacher mode. Checked 2026-09-27 against the installed wheel
# (llama_cpp.Llama exposes close() and the Llama(**kwargs) constructor used
# here) and upstream API docs:
#   https://llama-cpp-python.readthedocs.io/en/latest/api-reference/#llama.Llama
#   (also verified against https://github.com/abetlen/llama-cpp-python 0.3.33).
import gc
import logging
import math
from pathlib import Path
from typing import Any

from txtcaster_engine.llm import ggml_runtime
from txtcaster_engine.llm.backend import (
    LLMBackend,
    LLMBackendError,
    ResourcePlan,
    resolve_threads,
)
from txtcaster_engine.llm.loader import build_llama
from txtcaster_engine.oom import is_oom, load_failure_code

logger = logging.getLogger(__name__)

MAX_TEMPERATURE = 2.0


class LlamaBackend(LLMBackend):
    def __init__(self, model_path: str, plan: ResourcePlan, threads: int | None = None) -> None:
        path = Path(model_path)
        if not path.is_file():
            raise LLMBackendError("ERR_LLM_MODEL", f"gguf file not found: {model_path}")
        if threads is not None and threads < 1:
            raise LLMBackendError("ERR_PLAN", "threads must be >= 1")
        try:
            from llama_cpp import Llama
        except ImportError as exc:
            raise LLMBackendError("ERR_LLM_MODEL", "llama-cpp-python is not installed") from exc
        ggml_runtime.load()

        self.plan = plan
        self.threads = threads
        n_threads, n_threads_batch = resolve_threads(threads)
        n_gpu_layers = plan.n_gpu_layers if plan.device.is_gpu else 0
        try:
            self._llama: Any | None = build_llama(
                Llama,
                model_path=str(path),
                n_gpu_layers=n_gpu_layers,
                n_ctx=plan.ctx,
                n_batch=plan.batch,
                offload_kqv=plan.kv_offload,
                n_threads=n_threads,
                n_threads_batch=n_threads_batch,
                verbose=False,
            )
        except Exception as exc:
            code = load_failure_code(exc, n_gpu_layers > 0, "ERR_LLM_LOAD")
            raise LLMBackendError(code, str(exc)) from exc

    def generate(
        self,
        prompt: str,
        max_tokens: int,
        temperature: float = 0.0,
        stop: list[str] | None = None,
    ) -> str:
        if self._llama is None:
            raise LLMBackendError("ERR_LLM_UNLOADED", "model has been unloaded")
        if not math.isfinite(temperature) or not 0.0 <= temperature <= MAX_TEMPERATURE:
            raise LLMBackendError("ERR_PLAN", f"temperature must be within [0, {MAX_TEMPERATURE}]")
        try:
            completion = self._llama.create_chat_completion(
                messages=[{"role": "user", "content": prompt}],
                max_tokens=max_tokens,
                temperature=temperature,
                stop=stop or [],
            )
        except Exception as exc:
            if is_oom(exc):
                raise LLMBackendError("ERR_OOM", str(exc)) from exc
            raise
        choice = completion["choices"][0]
        finish_reason = choice.get("finish_reason")
        if finish_reason is not None and finish_reason != "stop":
            logger.info(
                "llm stopped early: finish_reason=%s max_tokens=%d chars=%d",
                finish_reason,
                max_tokens,
                len(choice["message"].get("content") or ""),
            )
        content = choice["message"]["content"]
        if not isinstance(content, str):
            raise LLMBackendError("ERR_LLM_GENERATE", "model returned no text")
        return content

    def context_window(self) -> int:
        return self.plan.ctx

    def unload(self) -> None:
        llama = self._llama
        if llama is None:
            return
        self._llama = None
        llama.close()
        del llama
        gc.collect()
