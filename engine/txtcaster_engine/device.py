from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

BACKENDS = ("cpu", "cuda", "rocm", "vulkan", "metal")

PLAN_REVISION = 2


class PlanError(Exception):
    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.code = "ERR_PLAN"
        self.message = message


@dataclass(frozen=True)
class DevicePlan:
    backend: str
    ordinal: int | None = None
    key: str | None = None

    @property
    def is_gpu(self) -> bool:
        return self.backend != "cpu"

    @property
    def learned_key(self) -> str:
        if self.key:
            return self.key
        if self.backend == "cpu":
            return "cpu"
        if self.ordinal is None:
            return self.backend
        return f"{self.backend}:{self.ordinal}"

    def torch_device(self) -> str | None:
        if self.backend != "cuda":
            return None
        return "cuda"


def _backend(raw: object) -> str:
    if not isinstance(raw, str) or raw not in BACKENDS:
        raise PlanError(
            f"resource_plan.device.backend must be one of {', '.join(BACKENDS)}, got {raw!r}"
        )
    return raw


def _ordinal(raw: object) -> int | None:
    if raw is None:
        return None
    if not isinstance(raw, int) or isinstance(raw, bool) or raw < 0:
        raise PlanError(f"resource_plan.device.ordinal must be a non-negative int, got {raw!r}")
    return raw


def _key(raw: object) -> str | None:
    if raw is None:
        return None
    if not isinstance(raw, str) or not raw:
        raise PlanError(f"resource_plan.device.key must be a non-empty string, got {raw!r}")
    return raw


def _from_legacy(raw: str) -> DevicePlan:
    head, _, tail = raw.partition(":")
    backend = _backend(head)
    if backend == "cpu":
        return DevicePlan(backend="cpu")
    if not tail:
        return DevicePlan(backend=backend, ordinal=0)
    if not tail.isascii() or not tail.isdecimal():
        raise PlanError(f"resource_plan.device names no valid ordinal: {raw!r}")
    return DevicePlan(backend=backend, ordinal=int(tail))


def parse_device(plan: Mapping[str, Any]) -> DevicePlan:
    raw = plan.get("device", "cpu")
    if isinstance(raw, str):
        if not raw:
            raise PlanError("resource_plan.device must be a non-empty string")
        return _from_legacy(raw)
    if isinstance(raw, Mapping):
        backend = _backend(raw.get("backend"))
        ordinal = _ordinal(raw.get("ordinal"))
        if backend != "cpu" and ordinal is None:
            ordinal = 0
        return DevicePlan(backend=backend, ordinal=ordinal, key=_key(raw.get("key")))
    raise PlanError(f"resource_plan.device must be an object or a string, got {raw!r}")
