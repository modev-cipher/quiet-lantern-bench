"""Wire types shared by every adapter and the single real HTTPS transport.

The key travels in one request header only; `WireRequest.headers` is the sole place it exists
outside the `Secret`, and `describe()` is what logs and errors are allowed to see.

`WireResponse.transport` is stamped by the transport that carried the request, and the ledger
prices from it: `https_wire` is synchronous and stamps `sync`; only a Batch transport may stamp
`batch`. `PROMPT_TEXT_TOKENS` is the survey §1 fixed assumption (~400 text tokens of prompt per
call) used with each provider's image-token formula when a provider returned no usage.
Usage rule for every adapter: block ABSENT -> tokens None (estimated); block present -> each
half read verbatim with a missing half counted as 0 (provider-reported), so 0/0 stays $0.
"""

from __future__ import annotations

import base64
import hashlib
import json
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Literal, Protocol

from txtcaster_engine.ingest.fallback.keys import Secret
from txtcaster_engine.ingest.fallback.trigger import FallbackTrigger

DEFAULT_TIMEOUT_S = 90.0
PROMPT_TEXT_TOKENS = 400
SURVEY_PAGE_PX = (1000, 1300)


@dataclass(frozen=True)
class PageImage:
    jpeg: bytes
    width: int
    height: int
    sha256: str

    @classmethod
    def from_jpeg(cls, jpeg: bytes, width: int = 0, height: int = 0) -> PageImage:
        return cls(jpeg, width, height, hashlib.sha256(jpeg).hexdigest())

    def data_url(self) -> str:
        return "data:image/jpeg;base64," + self.base64()

    def base64(self) -> str:
        return base64.b64encode(self.jpeg).decode("ascii")


@dataclass(frozen=True)
class WireRequest:
    provider_id: str
    url: str
    headers: dict[str, str] = field(repr=False)
    body: dict[str, object] = field(repr=False)

    def describe(self) -> str:
        return f"{self.provider_id} POST {self.url} ({len(json.dumps(self.body))} bytes)"


@dataclass(frozen=True)
class WireResponse:
    status: int
    body: dict[str, object]
    transport: Literal["sync", "batch"] = "sync"


Wire = Callable[[WireRequest], WireResponse]


@dataclass(frozen=True)
class ProviderOutcome:
    """`input_tokens` / `output_tokens` are None only when the provider sent NO usage block;
    an explicit zero is provider-reported zero (not billed) and must stay zero."""

    text: str | None
    input_tokens: int | None
    output_tokens: int | None
    error: str | None = None
    retryable: bool = False


class Provider(Protocol):
    provider_id: str
    vendor: str
    model_id: str
    price_in_per_m: float
    price_out_per_m: float

    def build_request(
        self, image: PageImage, trigger: FallbackTrigger, key: Secret
    ) -> WireRequest: ...

    def parse_response(self, response: WireResponse) -> ProviderOutcome: ...

    def estimate_input_tokens(self, image: PageImage) -> int: ...


def https_wire(request: WireRequest, timeout: float = DEFAULT_TIMEOUT_S) -> WireResponse:
    payload = json.dumps(request.body).encode()
    headers = {"Content-Type": "application/json", **request.headers}
    req = urllib.request.Request(request.url, data=payload, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return WireResponse(int(resp.status), _json_body(resp.read()), "sync")
    except urllib.error.HTTPError as exc:
        return WireResponse(int(exc.code), _json_body(exc.read()), "sync")


def _json_body(raw: bytes) -> dict[str, object]:
    try:
        data = json.loads(raw.decode("utf-8", "replace"))
    except ValueError:
        return {"raw": raw[:512].decode("utf-8", "replace")}
    return data if isinstance(data, dict) else {"raw": data}


def image_px(image: PageImage) -> tuple[int, int]:
    if image.width > 0 and image.height > 0:
        return image.width, image.height
    return SURVEY_PAGE_PX


def region_instruction(trigger: FallbackTrigger) -> str:
    if trigger.kind == "table_entropy":
        return (
            "This image is a table region whose row and column structure could not be read "
            "reliably. Return every row as one spoken table_row item, naming the column for "
            "each cell, in top-to-bottom order."
        )
    if trigger.kind == "undecodable_text":
        return (
            "This page image shows text whose fonts could not be decoded from the file. "
            "Read the whole page."
        )
    return "This is a scanned page image with no extractable text. Read the whole page."
