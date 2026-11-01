import secrets

from starlette.middleware.base import BaseHTTPMiddleware, RequestResponseEndpoint
from starlette.requests import Request
from starlette.responses import JSONResponse, Response


class BearerTokenMiddleware(BaseHTTPMiddleware):
    def __init__(self, app, token: str) -> None:  # type: ignore[no-untyped-def]
        super().__init__(app)
        self._token = token

    async def dispatch(self, request: Request, call_next: RequestResponseEndpoint) -> Response:
        header = request.headers.get("authorization", "")
        scheme, _, credentials = header.partition(" ")
        if scheme.lower() != "bearer" or not secrets.compare_digest(
            credentials.encode("utf-8"), self._token.encode("utf-8")
        ):
            return JSONResponse(status_code=401, content={"detail": "unauthorized"})
        return await call_next(request)
