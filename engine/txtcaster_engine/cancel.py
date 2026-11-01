import threading
from collections import OrderedDict

_MAX_CANCELLED = 1024


class CancelRegistry:
    def __init__(self) -> None:
        self._cancelled: OrderedDict[str, None] = OrderedDict()
        self._lock = threading.Lock()

    def request_cancel(self, job_id: str) -> None:
        with self._lock:
            self._cancelled.pop(job_id, None)
            self._cancelled[job_id] = None
            if len(self._cancelled) > _MAX_CANCELLED:
                oldest = next((k for k in self._cancelled if k != "*"), None)
                if oldest is not None:
                    del self._cancelled[oldest]

    def is_cancelled(self, job_id: str) -> bool:
        with self._lock:
            return job_id in self._cancelled or "*" in self._cancelled

    def clear(self, job_id: str) -> None:
        with self._lock:
            self._cancelled.pop(job_id, None)

    def acknowledge(self, job_id: str) -> None:
        self.clear(job_id)

    def request_cancel_all(self) -> None:
        with self._lock:
            self._cancelled["*"] = None

    def shutdown_requested(self) -> bool:
        with self._lock:
            return "*" in self._cancelled
