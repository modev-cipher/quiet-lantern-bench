import logging
import threading
import time
import traceback
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from types import TracebackType
from typing import Any

ModelKey = tuple[str, str, str]

DEFAULT_EVICTION_PRIORITY: tuple[str, ...] = ("align", "tts", "layout", "vision", "llm")

logger = logging.getLogger(__name__)


INTERACTIVE_TTS_IDLE_TIMEOUT_S = 300.0
UNLOAD_LEASE_DRAIN_S = 5.0
RESERVATION_GRACE_S = 120.0

VISION_STAGE = "VISION"
SCRIPT_STAGE = "SCRIPTING"


def model_key(task: str, model_id: str, variant: str = "") -> ModelKey:
    return (task, model_id, variant)


def describe_key(key: ModelKey) -> str:
    task, model_id, variant = key
    device = variant.rsplit("|", 1)[-1] if "|" in variant else variant
    return f"{task}/{model_id}({device})" if device else f"{task}/{model_id}"


class ResidencyError(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


def release_traceback_locals(error: BaseException, stop_at: BaseException | None = None) -> None:
    """Drop the locals a propagating traceback holds, so the flush after it can reclaim them.

    Rebinding the caller's own name is not enough on the raising path: every frame the
    exception passed through is kept alive by `__traceback__`, and a frame that took the
    converter as a parameter still references it. Measured — a weak reference to the
    converter is still live inside the `finally` of both `docling_path` entry points
    without this.

    The whole chain, not just `error`, because the shape that matters here is
    `raise IngestError(...) from exc` at `docling_path.py:167`: the wrapper's own traceback
    starts at the `raise`, and the frames that actually hold the converter — docling's, and
    `_convert_document_status`'s — hang off `__cause__`. Measured: clearing only
    `error.__traceback__` leaves the converter alive, clearing the chain does not.

    `stop_at` is the exception that was already being handled when the caller was entered.
    Without it the walk escapes through `__context__`, which Python fills from the thread's
    live exception state, and strips the post-mortem locals of an unrelated error further up
    the stack — the data most worth having when reading an OOM incident. No caller reaches
    ingest from inside an `except` today, so that was latent rather than live, but this is a
    shared utility and the boundary belongs in it.

    Only locals go: the traceback's file, line and function survive, so `format_exc()` and
    every message are unchanged. A debugger inspecting locals after the fact would not be.
    Note separately that the `except ... raise` this is called from moves the *caller's* own
    traceback entry from the failing statement to the bare `raise`; deeper frames still name
    the origin.
    """
    seen: set[int] = set()
    queue: list[BaseException] = [error]
    while queue:
        current = queue.pop()
        if current is stop_at or id(current) in seen:
            continue
        seen.add(id(current))
        traceback.clear_frames(current.__traceback__)
        related: list[BaseException | None] = [current.__cause__, current.__context__]
        related.extend(getattr(current, "exceptions", ()) or ())
        queue.extend(item for item in related if item is not None)


def flush_accelerator_cache_mib() -> int:
    """Flush the torch caching allocator and report the MiB it returned to the driver.

    Reserved, not allocated. Allocated has already fallen by the time a converter's last
    reference goes — measured 402.5 to 8.7 MiB on `del` alone — while the driver still
    shows the reserved high-water mark, which is the number the offload planner reads
    back as free VRAM. Reserved delta is therefore the only honest answer to "how much
    did this flush actually hand back".
    """
    from txtcaster_engine.tts.backend import configure_torch_threads

    configure_torch_threads()
    try:
        import torch
    except ImportError:
        return 0
    if not torch.cuda.is_available():
        return 0
    before = torch.cuda.memory_reserved()
    torch.cuda.empty_cache()
    return max(0, before - torch.cuda.memory_reserved()) // (1024 * 1024)


def flush_accelerator_cache() -> None:
    flush_accelerator_cache_mib()


@dataclass(frozen=True)
class ResidencyPolicy:
    max_residents: int = 1
    idle_timeout_s: float | None = None
    task_idle_timeouts: Mapping[str, float | None] = field(default_factory=dict)
    coresident_tasks: frozenset[str] = frozenset()
    eviction_priority: tuple[str, ...] = DEFAULT_EVICTION_PRIORITY

    def capacity_for(self, task: str) -> int:
        if task in self.coresident_tasks:
            return max(self.max_residents, len(self.coresident_tasks))
        return self.max_residents

    def idle_timeout_for(self, task: str) -> float | None:
        if task in self.task_idle_timeouts:
            return self.task_idle_timeouts[task]
        return self.idle_timeout_s


def default_residency_policy() -> ResidencyPolicy:
    return ResidencyPolicy(
        max_residents=1,
        task_idle_timeouts={"tts": INTERACTIVE_TTS_IDLE_TIMEOUT_S},
    )


@dataclass
class _Resident:
    key: ModelKey
    backend: Any
    stage: str | None
    pinned: bool
    leases: int = 0
    idle_since: float | None = None
    reserved_at: float = 0.0
    loading: bool = False


class Lease:
    def __init__(self, manager: "ResidencyManager", key: ModelKey, backend: Any) -> None:
        self._manager = manager
        self.key = key
        self.backend = backend
        self.released = False

    def __enter__(self) -> Any:
        return self.backend

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self._manager.release(self)

    def release(self) -> None:
        self._manager.release(self)


class ResidencyManager:
    def __init__(
        self,
        policy: ResidencyPolicy | None = None,
        clock: Callable[[], float] = time.monotonic,
        flush: Callable[[], None] = flush_accelerator_cache,
    ) -> None:
        self._policy = policy or ResidencyPolicy()
        self._clock = clock
        self._flush = flush
        self._lock = threading.RLock()
        self._drained = threading.Condition(self._lock)
        self._residents: dict[ModelKey, _Resident] = {}

    @property
    def policy(self) -> ResidencyPolicy:
        return self._policy

    def acquire(
        self,
        key: ModelKey,
        factory: Callable[[], Any],
        stage: str | None = None,
        pinned: bool = False,
    ) -> Lease:
        with self._lock:
            resident = self._residents.get(key)
            if resident is not None:
                self._require_loaded(resident)
                if stage is not None:
                    resident.stage = stage
                resident.pinned = resident.pinned or pinned
                return self._lease_locked(resident)
            self._make_room_for(key)
            resident = _Resident(
                key=key,
                backend=None,
                stage=stage,
                pinned=pinned,
                reserved_at=self._clock(),
                loading=True,
            )
            self._residents[key] = resident
            lease = self._lease_locked(resident)
            logger.info(
                "admitting %s (stage=%s, residents=%d)",
                describe_key(key),
                stage,
                len(self._residents),
            )

        loaded = False
        try:
            backend = factory()
            with self._lock:
                resident.backend = backend
                resident.loading = False
                lease.backend = backend
            loaded = True
            return lease
        finally:
            if not loaded:
                lease.released = True
                with self._lock:
                    current = self._residents.get(key)
                    if current is resident and resident.backend is None:
                        del self._residents[key]

    def reserve(self, key: ModelKey, stage: str | None = None, pinned: bool = False) -> Lease:
        with self._lock:
            resident = self._residents.get(key)
            if resident is None:
                self._make_room_for(key)
                resident = _Resident(
                    key=key,
                    backend=None,
                    stage=stage,
                    pinned=pinned,
                    reserved_at=self._clock(),
                )
                self._residents[key] = resident
                logger.info(
                    "reserving %s (stage=%s, residents=%d)",
                    describe_key(key),
                    stage,
                    len(self._residents),
                )
            else:
                self._require_loaded(resident)
            return self._lease_locked(resident)

    def retain(self, key: ModelKey) -> Lease:
        with self._lock:
            resident = self._residents.get(key)
            if resident is None:
                raise ResidencyError("ERR_RESIDENCY", f"no resident model for slot {key}")
            self._require_loaded(resident)
            return self._lease_locked(resident)

    def mark_loading(self, lease: Lease) -> None:
        with self._lock:
            resident = self._residents.get(lease.key)
            if resident is not None:
                resident.loading = True

    def attach(self, lease: Lease, backend: Any) -> None:
        with self._lock:
            resident = self._residents.get(lease.key)
            if resident is None:
                raise ResidencyError("ERR_RESIDENCY", f"slot {lease.key} is no longer reserved")
            resident.backend = backend
            resident.loading = False
            lease.backend = backend
            logger.info(
                "attached %s %.1fs after reservation (stage=%s)",
                describe_key(lease.key),
                self._clock() - resident.reserved_at,
                resident.stage,
            )

    def release(self, lease: Lease) -> None:
        with self._lock:
            if lease.released:
                return
            lease.released = True
            resident = self._residents.get(lease.key)
            if resident is None:
                return
            resident.leases = max(0, resident.leases - 1)
            if resident.leases > 0:
                return
            resident.idle_since = self._clock()
            self._drained.notify_all()
            if resident.backend is None:
                self._residents.pop(resident.key, None)
                return
            if resident.pinned or self._policy.idle_timeout_for(resident.key[0]) is not None:
                logger.info(
                    "keeping %s resident (pinned=%s, stage=%s)",
                    describe_key(resident.key),
                    resident.pinned,
                    resident.stage,
                )
                return
            self._unload_locked(resident)
            self._flush_quietly()

    def get(self, key: ModelKey) -> Any | None:
        with self._lock:
            resident = self._residents.get(key)
            return None if resident is None else resident.backend

    def resident_keys(self) -> list[ModelKey]:
        with self._lock:
            return list(self._residents)

    def lease_count(self, key: ModelKey) -> int:
        with self._lock:
            resident = self._residents.get(key)
            return 0 if resident is None else resident.leases

    def find_key(self, task: str, model_id: str) -> ModelKey | None:
        with self._lock:
            for key in self._residents:
                if key[0] == task and key[1] == model_id:
                    return key
        return None

    def unload_key(self, key: ModelKey, wait_s: float = 0.0) -> bool:
        deadline = time.monotonic() + wait_s
        with self._lock:
            resident = self._residents.get(key)
            if resident is None:
                return False
            resident.pinned = False
            if resident.backend is None and not resident.loading:
                self._unload_locked(resident)
                self._flush_quietly()
                return True
            while resident.leases > 0:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ResidencyError(
                        "ERR_BUSY",
                        f"model '{resident.key[1]}' is in use by "
                        f"{resident.leases} active request(s)",
                    )
                self._drained.wait(remaining)
                resident = self._residents.get(key)
                if resident is None:
                    self._flush_quietly()
                    return True
            self._unload_locked(resident)
            self._flush_quietly()
            return True

    def unload_model(self, task: str, model_id: str, wait_s: float = 0.0) -> bool:
        key = self.find_key(task, model_id)
        if key is None:
            return False
        return self.unload_key(key, wait_s=wait_s)

    def evict_stage(self, stage: str) -> list[ModelKey]:
        with self._lock:
            targets = [
                resident
                for resident in self._residents.values()
                if resident.stage == stage and resident.leases == 0
            ]
            evicted = [resident.key for resident in targets]
            for resident in targets:
                self._unload_locked(resident)
            if evicted:
                self._flush_quietly()
            return evicted

    def sweep_idle(self) -> list[ModelKey]:
        with self._lock:
            self._reclaim_stale_reservations_locked()
        now = self._clock()
        with self._lock:
            targets = [
                resident
                for resident in self._residents.values()
                if self._is_idle_expired(resident, now)
            ]
            evicted = [resident.key for resident in targets]
            for resident in targets:
                self._unload_locked(resident)
            if evicted:
                self._flush_quietly()
            return evicted

    def unload_all(self) -> tuple[list[ModelKey], list[ModelKey]]:
        with self._lock:
            unloaded: list[ModelKey] = []
            retained: list[ModelKey] = []
            for resident in list(self._residents.values()):
                if resident.leases > 0:
                    retained.append(resident.key)
                    logger.warning(
                        "refusing to unload %s: %d active lease(s) still hold it",
                        describe_key(resident.key),
                        resident.leases,
                    )
                    continue
                unloaded.append(resident.key)
                self._unload_locked(resident)
            if unloaded:
                self._flush_quietly()
            return unloaded, retained

    def _require_loaded(self, resident: _Resident) -> None:
        if resident.backend is None:
            raise ResidencyError(
                "ERR_BUSY",
                f"model slot {resident.key} is reserved and not yet loaded",
            )

    def _lease_locked(self, resident: _Resident) -> Lease:
        lease = Lease(self, resident.key, resident.backend)
        resident.leases += 1
        resident.idle_since = None
        return lease

    def _is_idle_expired(self, resident: _Resident, now: float) -> bool:
        if resident.leases > 0 or resident.pinned or resident.idle_since is None:
            return False
        timeout = self._policy.idle_timeout_for(resident.key[0])
        if timeout is None:
            return False
        return now - resident.idle_since > timeout

    def _reclaim_stale_reservations_locked(self) -> None:
        now = self._clock()
        stale = [
            resident
            for resident in self._residents.values()
            if resident.backend is None
            and not resident.loading
            and now - resident.reserved_at > RESERVATION_GRACE_S
        ]
        for resident in stale:
            logger.warning(
                "reclaiming slot %s: reserved %.0fs ago and never loaded",
                describe_key(resident.key),
                now - resident.reserved_at,
            )
            self._residents.pop(resident.key, None)

    def _make_room_for(self, key: ModelKey) -> None:
        self._reclaim_stale_reservations_locked()
        self._sweep_idle_locked()
        capacity = self._policy.capacity_for(key[0])
        while len(self._residents) >= capacity:
            victim = self._pick_victim()
            if victim is None:
                holders = ", ".join(sorted(r.key[1] for r in self._residents.values()))
                raise ResidencyError(
                    "ERR_BUSY",
                    f"model slot held by '{holders}'; retry after it is released",
                )
            self._unload_locked(victim)
            self._flush_quietly()

    def _sweep_idle_locked(self) -> None:
        now = self._clock()
        expired = [r for r in self._residents.values() if self._is_idle_expired(r, now)]
        for resident in expired:
            self._unload_locked(resident)
        if expired:
            self._flush_quietly()

    def _pick_victim(self) -> _Resident | None:
        candidates = [
            resident
            for resident in self._residents.values()
            if resident.leases == 0 and not resident.pinned
        ]
        if not candidates:
            return None
        priority = self._policy.eviction_priority

        def rank(resident: _Resident) -> tuple[int, float]:
            task = resident.key[0]
            order = priority.index(task) if task in priority else len(priority)
            return (order, resident.idle_since or 0.0)

        return min(candidates, key=rank)

    def _unload_locked(self, resident: _Resident) -> None:
        logger.info(
            "unloading %s (stage=%s, leases=%d)",
            describe_key(resident.key),
            resident.stage,
            resident.leases,
        )
        self._residents.pop(resident.key, None)
        if resident.backend is not None:
            started = self._clock()
            try:
                if hasattr(resident.backend, "unload"):
                    resident.backend.unload()
                logger.info(
                    "unloaded %s in %.1fs",
                    describe_key(resident.key),
                    self._clock() - started,
                )
            except Exception:
                logger.exception("backend %s failed to unload cleanly", describe_key(resident.key))

    def _flush_quietly(self) -> None:
        try:
            self._flush()
        except Exception:
            logger.exception("accelerator cache flush failed")
