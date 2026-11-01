import contextlib
import gc
import logging
from collections.abc import Callable
from pathlib import Path
from typing import Any

from txtcaster_engine.llm.backend import LLMBackendError, ResourcePlan, resolve_threads
from txtcaster_engine.llm.loader import build_llama
from txtcaster_engine.oom import is_oom

# Provenance (AGENTS.md §5 third-party integration rule):
# llama-cpp-python 0.3.33, verified against upstream main on 2026-07-27
# https://raw.githubusercontent.com/abetlen/llama-cpp-python/main/llama_cpp/llama_chat_format.py
# `MTMDChatHandler` (and its bare subclass `Gemma4ChatHandler`) registers the
# native projector teardown as an `mtmd_free` callback on a private
# `self._exit_stack`, and defines neither `close()` nor `__del__`. `Llama.close()`
# only closes `Llama._stack`, so dropping the handler never frees the mmproj/CLIP
# context: the projector stays resident for the process lifetime. Owning the
# handler and closing that stack ourselves is therefore required, not optional.
PROJECTOR_EXIT_STACK_ATTR = "_exit_stack"
PROJECTOR_HANDLER_NAMES = frozenset({"MTMDChatHandler", "Gemma4ChatHandler"})

logger = logging.getLogger(__name__)


class VisionBackend:
    def describe(
        self,
        image_png: bytes,
        prompt: str,
        max_tokens: int,
        timeout_s: float | None = None,
        is_cancelled: Callable[[], bool] | None = None,
    ) -> str:
        raise NotImplementedError

    def unload(self) -> None:
        raise NotImplementedError


def _chat_handler(mmproj_path: str, family: str, use_gpu: bool) -> Any:
    from llama_cpp.llama_chat_format import Gemma4ChatHandler, MTMDChatHandler

    handler_cls = Gemma4ChatHandler if family == "gemma" else MTMDChatHandler
    return handler_cls(clip_model_path=mmproj_path, verbose=False, use_gpu=use_gpu)


def _free_projector(handler: Any) -> None:
    if handler is None:
        return
    exit_stack = getattr(handler, PROJECTOR_EXIT_STACK_ATTR, None)
    close = getattr(exit_stack, "close", None)
    if close is None:
        if type(handler).__name__ in PROJECTOR_HANDLER_NAMES:
            logger.warning(
                "%s exposes no '%s' to close: the mmproj/CLIP context stays resident. "
                "llama-cpp-python likely changed its teardown; update the provenance note.",
                type(handler).__name__,
                PROJECTOR_EXIT_STACK_ATTR,
            )
        return
    with contextlib.suppress(Exception):
        close()


class LlamaVisionBackend(VisionBackend):
    def __init__(
        self,
        model_path: str,
        mmproj_path: str,
        plan: ResourcePlan,
        threads: int | None = None,
        family: str = "",
    ) -> None:
        if not Path(model_path).is_file():
            raise LLMBackendError("ERR_VISION_MODEL", f"gguf file not found: {model_path}")
        if not Path(mmproj_path).is_file():
            raise LLMBackendError("ERR_VISION_MODEL", f"mmproj file not found: {mmproj_path}")
        if threads is not None and threads < 1:
            raise LLMBackendError("ERR_PLAN", "threads must be >= 1")
        try:
            from llama_cpp import Llama
        except ImportError as exc:
            raise LLMBackendError("ERR_VISION_MODEL", "llama-cpp-python is not installed") from exc

        self.plan = plan
        use_gpu = plan.device.is_gpu
        n_threads, n_threads_batch = resolve_threads(threads)
        n_gpu_layers = plan.n_gpu_layers if use_gpu else 0
        self._chat_handler_ref: Any | None = None
        self._llama: Any | None = None
        try:
            self._chat_handler_ref = _chat_handler(mmproj_path, family, use_gpu)
            self._llama = build_llama(
                Llama,
                model_path=model_path,
                chat_handler=self._chat_handler_ref,
                n_gpu_layers=n_gpu_layers,
                n_ctx=plan.ctx,
                n_batch=plan.batch,
                offload_kqv=plan.kv_offload,
                n_threads=n_threads,
                n_threads_batch=n_threads_batch,
                verbose=False,
            )
        except Exception as exc:
            self.unload()
            code = "ERR_OOM" if is_oom(exc) else "ERR_VISION_LOAD"
            raise LLMBackendError(code, str(exc)) from exc

    def describe(
        self,
        image_png: bytes,
        prompt: str,
        max_tokens: int,
        timeout_s: float | None = None,
        is_cancelled: Callable[[], bool] | None = None,
    ) -> str:
        if self._llama is None:
            raise LLMBackendError("ERR_VISION_UNLOADED", "model has been unloaded")
        import base64
        import time

        data_uri = "data:image/png;base64," + base64.b64encode(image_png).decode("ascii")
        messages = [
            {
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": data_uri}},
                    {"type": "text", "text": prompt},
                ],
            }
        ]
        started = time.monotonic()
        parts: list[str] = []
        try:
            chunks = self._llama.create_chat_completion(
                messages=messages,
                max_tokens=max_tokens,
                temperature=0.0,
                stream=True,
            )
            for chunk in chunks:
                delta = chunk["choices"][0].get("delta", {})
                piece = delta.get("content")
                if isinstance(piece, str):
                    parts.append(piece)
                if is_cancelled is not None and is_cancelled():
                    raise LLMBackendError("ERR_CANCELLED", "vision generation cancelled")
                if timeout_s is not None and time.monotonic() - started > timeout_s:
                    raise LLMBackendError(
                        "ERR_VISION_TIMEOUT",
                        f"region description exceeded {timeout_s:g}s budget",
                    )
        except LLMBackendError:
            raise
        except Exception as exc:
            if is_oom(exc):
                raise LLMBackendError("ERR_OOM", str(exc)) from exc
            raise LLMBackendError("ERR_VISION_GENERATE", str(exc)) from exc
        if not parts:
            raise LLMBackendError("ERR_VISION_GENERATE", "model returned no text")
        return "".join(parts)

    def unload(self) -> None:
        llama = self._llama
        handler = self._chat_handler_ref
        self._llama = None
        self._chat_handler_ref = None
        if llama is None and handler is None:
            return
        try:
            if llama is not None:
                llama.close()
        except Exception:
            logger.exception("vision text model failed to close; freeing projector anyway")
        finally:
            _free_projector(handler)
            del llama, handler
            gc.collect()
