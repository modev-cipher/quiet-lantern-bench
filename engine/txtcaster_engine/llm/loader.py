# Provenance (AGENTS.md §5a): llama-cpp-python 0.3.33 (pinned). Checked
# 2026-09-27 against the installed wheel and upstream llama_cpp/llama.py:
# Llama cleanup runs through close() -> self._stack.close(), where _stack is
# an ExitStack created early in __init__; __del__ delegates to close(), so
# partial constructor failures must be closed explicitly (as below) rather
# than left to GC.
import contextlib
import gc
import logging
from typing import Any

logger = logging.getLogger(__name__)


def build_llama(llama_cls: Any, **kwargs: Any) -> Any:
    if not isinstance(llama_cls, type):
        return llama_cls(**kwargs)

    built: list[Any] = []

    class _Tracked(llama_cls):  # type: ignore[misc]
        def __init__(self, *args: Any, **kw: Any) -> None:
            built.append(self)
            super().__init__(*args, **kw)

    try:
        return _Tracked(**kwargs)
    except BaseException:
        for partial in built:
            close = getattr(partial, "close", None)
            if close is None:
                continue
            with contextlib.suppress(Exception):
                close()
        built.clear()
        gc.collect()
        raise
