"""Provider keys held so that no repr, log line, error string or cache key can carry them."""

from __future__ import annotations

import re
from collections.abc import Iterable
from dataclasses import dataclass

REDACTED = "<redacted>"


class Secret:
    __slots__ = ("_value",)

    def __init__(self, value: str) -> None:
        self._value = value.strip()

    def expose(self) -> str:
        return self._value

    def __bool__(self) -> bool:
        return bool(self._value)

    def __repr__(self) -> str:
        return f"Secret({REDACTED})"

    def __str__(self) -> str:
        return REDACTED

    def __eq__(self, other: object) -> bool:
        return NotImplemented

    __hash__ = None  # type: ignore[assignment]


@dataclass(frozen=True, repr=False)
class ProviderKeys:
    openai: Secret | None = None
    google: Secret | None = None

    def __repr__(self) -> str:
        return f"ProviderKeys(openai={self.openai is not None}, google={self.google is not None})"

    def for_vendor(self, vendor: str) -> Secret | None:
        return {"openai": self.openai, "google": self.google}.get(vendor)

    def secrets(self) -> list[Secret]:
        return [s for s in (self.openai, self.google) if s]


def redact(text: str, secrets: Iterable[Secret]) -> str:
    for secret in secrets:
        raw = secret.expose()
        if len(raw) >= 8:
            text = re.sub(re.escape(raw), REDACTED, text)
    return text
