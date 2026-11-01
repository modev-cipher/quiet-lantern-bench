import asyncio
from collections.abc import Callable
from typing import Any

ExceptionHandler = Callable[[asyncio.AbstractEventLoop, dict[str, Any]], object]


def is_reset_during_close(context: dict[str, Any]) -> bool:
    if not isinstance(context.get("exception"), ConnectionResetError):
        return False
    return "_call_connection_lost" in repr(context.get("handle"))


def quiet_resets_during_close(loop: asyncio.AbstractEventLoop) -> None:
    previous: ExceptionHandler | None = loop.get_exception_handler()

    def handler(current: asyncio.AbstractEventLoop, context: dict[str, Any]) -> None:
        if is_reset_during_close(context):
            return
        if previous is not None:
            previous(current, context)
        else:
            current.default_exception_handler(context)

    loop.set_exception_handler(handler)
