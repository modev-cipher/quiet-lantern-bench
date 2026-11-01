import json
from collections.abc import AsyncIterator, Mapping

from starlette.responses import StreamingResponse

SSE_HEADERS = {
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "X-Accel-Buffering": "no",
}


def format_sse_event(event: str, data: Mapping[str, object]) -> str:
    return f"event: {event}\ndata: {json.dumps(data)}\n\n"


def sse_response(events: AsyncIterator[tuple[str, Mapping[str, object]]]) -> StreamingResponse:
    async def stream() -> AsyncIterator[str]:
        async for event, data in events:
            yield format_sse_event(event, data)

    return StreamingResponse(stream(), media_type="text/event-stream", headers=SSE_HEADERS)
