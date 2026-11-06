OOM_CODE = "ERR_OOM"
DEVICE_UNAVAILABLE_CODE = "ERR_DEVICE_UNAVAILABLE"
GPU_LOAD_CODE = "ERR_GPU_LOAD"

OOM_MARKERS = (
    "out of memory",
    "cublas_status_alloc_failed",
    "cublas status alloc failed",
    "cublas_status_not_initialized",
    "cudnn_status_alloc_failed",
    "cudnn status alloc failed",
    "cudnn_status_not_initialized",
    "cufft_alloc_failed",
    "insufficient memory",
    "failed to allocate",
    "failed to create llama_context",
    "erroroutofdevicememory",
    "device memory allocation of size",
    "vk::device::allocatememory",
)

DEVICE_UNAVAILABLE_MARKERS = (
    "is not found or cannot be loaded",
    "cannot open shared object file",
    "undefined symbol",
    "no cuda-capable device is detected",
    "no kernel image is available",
    "invalid device ordinal",
    "cuda driver version is insufficient for cuda runtime version",
    "system has unsupported display driver / cuda driver combination",
    "forward compatibility was attempted on non supported hw",
    "cuda error: initialization error",
)


def _exception_chain(exc: BaseException) -> list[BaseException]:
    seen: set[int] = set()
    chain: list[BaseException] = []
    current: BaseException | None = exc
    while current is not None and id(current) not in seen:
        seen.add(id(current))
        chain.append(current)
        current = current.__cause__ or current.__context__
    return chain


def classify_resource_failure(exc: BaseException) -> str | None:
    chain = _exception_chain(exc)
    messages = [str(link).lower() for link in chain]
    for message in messages:
        if any(marker in message for marker in DEVICE_UNAVAILABLE_MARKERS):
            return DEVICE_UNAVAILABLE_CODE
    for link, message in zip(chain, messages, strict=True):
        if type(link).__name__ == "OutOfMemoryError":
            return OOM_CODE
        if any(marker in message for marker in OOM_MARKERS):
            return OOM_CODE
    return None


def is_oom(exc: BaseException) -> bool:
    return classify_resource_failure(exc) == OOM_CODE


def is_device_unavailable(exc: BaseException) -> bool:
    return classify_resource_failure(exc) == DEVICE_UNAVAILABLE_CODE


def load_failure_code(exc: BaseException, on_gpu: bool, fallback: str) -> str:
    classified = classify_resource_failure(exc)
    if classified is not None:
        return classified
    return GPU_LOAD_CODE if on_gpu else fallback
