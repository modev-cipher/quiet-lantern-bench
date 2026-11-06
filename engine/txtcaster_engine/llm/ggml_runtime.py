# Provenance (AGENTS.md §5a): ggml C API vendored by llama-cpp-python 0.3.33
# (llama.cpp 78d2f524, ggml 0.15.3). Checked 2026-11-03 against
# vendor/llama.cpp/ggml/include/ggml-backend.h (struct ggml_backend_dev_props,
# device and registry enumeration) and ggml/src/ggml-backend-reg.cpp, whose
# ggml_backend_load_all() searches only the executable and working directories:
# a frozen engine keeps its backend modules in llama_cpp/lib and loads them itself.
#   https://github.com/ggml-org/llama.cpp/blob/78d2f524/ggml/include/ggml-backend.h
#   https://github.com/ggml-org/llama.cpp/blob/78d2f524/ggml/src/ggml-backend-reg.cpp
# The CUDA backend module links cudart and cuBLAS without an rpath to them, so the
# copies torch ships (nvidia/cu13/lib on Linux, torch/lib on Windows) are loaded first.
# The Linux wheel stores libggml.so, .so.0 and .so.0.15.3 as separate files: opening
# libggml.so beside libllama's libggml.so.0 makes a second, empty backend registry, so
# ggml is reached through libllama's own handle (llama_cpp.llama_cpp._lib, whose dlsym
# searches libllama's dependencies). Windows loads each DLL once by name.
import ctypes
import logging
import sys
import threading
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass, field, replace
from pathlib import Path

logger = logging.getLogger(__name__)

DEVICE_TYPE_GPU = 1
DEVICE_TYPE_IGPU = 2

BACKEND_MODULE = {"linux": "libggml-{}.so", "win32": "ggml-{}.dll"}

CUDA_RUNTIME: dict[str, tuple[tuple[str, ...], tuple[str, ...]]] = {
    "linux": (
        ("nvidia", "cu13", "lib"),
        ("libcudart.so.13", "libcublasLt.so.13", "libcublas.so.13"),
    ),
    "win32": (
        ("torch", "lib"),
        ("cudart64_13.dll", "cublasLt64_13.dll", "cublas64_13.dll"),
    ),
}


@dataclass(frozen=True)
class GpuDevice:
    backend: str
    name: str
    description: str
    integrated: bool
    memory_total: int
    memory_free: int
    pci: str | None

    def to_json(self) -> dict[str, str | int | bool | None]:
        return {
            "backend": self.backend,
            "name": self.name,
            "description": self.description,
            "integrated": self.integrated,
            "memory_total": self.memory_total,
            "memory_free": self.memory_free,
            "pci": self.pci,
        }


@dataclass(frozen=True)
class GgmlRuntime:
    backends: tuple[str, ...]
    devices: tuple[GpuDevice, ...]
    error: str | None = None
    memory: Callable[[], Sequence[int | None]] | None = field(
        default=None, compare=False, repr=False
    )

    def with_current_memory(self) -> "GgmlRuntime":
        if self.memory is None or not self.devices:
            return self
        try:
            readings = list(self.memory())
        except Exception as exc:
            logger.warning("gpu memory could not be read: %s", exc)
            return self
        devices = tuple(
            replace(device, memory_free=free)
            if index < len(readings) and (free := readings[index]) is not None
            else device
            for index, device in enumerate(self.devices)
        )
        return replace(self, devices=devices)


class _DeviceCaps(ctypes.Structure):
    _fields_ = [
        ("async_ops", ctypes.c_bool),
        ("host_buffer", ctypes.c_bool),
        ("buffer_from_host_ptr", ctypes.c_bool),
        ("events", ctypes.c_bool),
    ]


class _DeviceProps(ctypes.Structure):
    _fields_ = [
        ("name", ctypes.c_char_p),
        ("description", ctypes.c_char_p),
        ("memory_free", ctypes.c_size_t),
        ("memory_total", ctypes.c_size_t),
        ("type", ctypes.c_int),
        ("device_id", ctypes.c_char_p),
        ("caps", _DeviceCaps),
    ]


def has_backend_module(lib_dir: Path, backend: str, platform: str) -> bool:
    pattern = BACKEND_MODULE.get(platform)
    return pattern is not None and (lib_dir / pattern.format(backend)).is_file()


def cuda_runtime_libraries(roots: Iterable[Path], platform: str) -> list[Path]:
    layout = CUDA_RUNTIME.get(platform)
    if layout is None:
        return []
    parts, names = layout
    for root in roots:
        directory = root.joinpath(*parts)
        paths = [directory / name for name in names]
        if all(path.is_file() for path in paths):
            return paths
    return []


def _search_roots() -> list[Path]:
    roots = []
    bundle = getattr(sys, "_MEIPASS", None)
    if isinstance(bundle, str):
        roots.append(Path(bundle))
    roots.extend(Path(entry) for entry in sys.path if entry)
    return roots


def _preload(paths: Sequence[Path]) -> None:
    mode = getattr(ctypes, "RTLD_GLOBAL", 0)
    for path in paths:
        ctypes.CDLL(str(path), mode=mode)


def _text(raw: bytes | None) -> str:
    return raw.decode("utf-8", errors="replace") if raw else ""


def _libraries(lib_dir: Path) -> tuple[ctypes.CDLL, ctypes.CDLL]:
    if sys.platform == "win32":
        from llama_cpp._ctypes_extensions import load_shared_library

        return load_shared_library("ggml", lib_dir), load_shared_library("ggml-base", lib_dir)
    from llama_cpp import llama_cpp as bindings

    linked: ctypes.CDLL = bindings._lib
    return linked, linked


def _bind(lib_dir: Path) -> tuple[ctypes.CDLL, ctypes.CDLL]:
    ggml, base = _libraries(lib_dir)
    ggml.ggml_backend_load_all_from_path.argtypes = [ctypes.c_char_p]
    ggml.ggml_backend_load_all_from_path.restype = None
    ggml.ggml_backend_reg_count.argtypes = []
    ggml.ggml_backend_reg_count.restype = ctypes.c_size_t
    ggml.ggml_backend_reg_get.argtypes = [ctypes.c_size_t]
    ggml.ggml_backend_reg_get.restype = ctypes.c_void_p
    ggml.ggml_backend_dev_count.argtypes = []
    ggml.ggml_backend_dev_count.restype = ctypes.c_size_t
    ggml.ggml_backend_dev_get.argtypes = [ctypes.c_size_t]
    ggml.ggml_backend_dev_get.restype = ctypes.c_void_p
    base.ggml_backend_reg_name.argtypes = [ctypes.c_void_p]
    base.ggml_backend_reg_name.restype = ctypes.c_char_p
    base.ggml_backend_dev_backend_reg.argtypes = [ctypes.c_void_p]
    base.ggml_backend_dev_backend_reg.restype = ctypes.c_void_p
    base.ggml_backend_dev_get_props.argtypes = [ctypes.c_void_p, ctypes.POINTER(_DeviceProps)]
    base.ggml_backend_dev_get_props.restype = None
    base.ggml_backend_dev_memory.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_size_t),
        ctypes.POINTER(ctypes.c_size_t),
    ]
    base.ggml_backend_dev_memory.restype = None
    return ggml, base


def _enumerate(ggml: ctypes.CDLL, base: ctypes.CDLL) -> GgmlRuntime:
    backends = tuple(
        _text(base.ggml_backend_reg_name(ggml.ggml_backend_reg_get(index)))
        for index in range(ggml.ggml_backend_reg_count())
    )
    devices = []
    handles = []
    for index in range(ggml.ggml_backend_dev_count()):
        device = ggml.ggml_backend_dev_get(index)
        props = _DeviceProps()
        base.ggml_backend_dev_get_props(device, ctypes.byref(props))
        if props.type not in (DEVICE_TYPE_GPU, DEVICE_TYPE_IGPU):
            continue
        registry = base.ggml_backend_dev_backend_reg(device)
        devices.append(
            GpuDevice(
                backend=_text(base.ggml_backend_reg_name(registry)),
                name=_text(props.name),
                description=_text(props.description),
                integrated=props.type == DEVICE_TYPE_IGPU,
                memory_total=int(props.memory_total),
                memory_free=int(props.memory_free),
                pci=_text(props.device_id).lower() or None,
            )
        )
        handles.append(device)

    def memory() -> list[int | None]:
        readings: list[int | None] = []
        for handle in handles:
            free, total = ctypes.c_size_t(0), ctypes.c_size_t(0)
            base.ggml_backend_dev_memory(handle, ctypes.byref(free), ctypes.byref(total))
            readings.append(int(free.value) if total.value else None)
        return readings

    return GgmlRuntime(backends=backends, devices=tuple(devices), memory=memory)


def _load_uncached() -> GgmlRuntime:
    try:
        import llama_cpp
    except Exception as exc:
        return GgmlRuntime(backends=(), devices=(), error=f"llama_cpp unavailable: {exc}")
    lib_dir = Path(llama_cpp.__file__).parent.absolute() / "lib"
    if has_backend_module(lib_dir, "cuda", sys.platform):
        runtime = cuda_runtime_libraries(_search_roots(), sys.platform)
        if not runtime:
            logger.warning("the CUDA backend is bundled but its CUDA runtime was not found")
        try:
            _preload(runtime)
        except OSError as exc:
            logger.warning("the CUDA runtime failed to load: %s", exc)
    try:
        ggml, base = _bind(lib_dir)
        ggml.ggml_backend_load_all_from_path(str(lib_dir).encode("utf-8"))
        return _enumerate(ggml, base)
    except Exception as exc:
        logger.warning("ggml backends unavailable: %s", exc)
        return GgmlRuntime(backends=(), devices=(), error=str(exc))


_lock = threading.Lock()
_runtime: GgmlRuntime | None = None


def load() -> GgmlRuntime:
    global _runtime
    with _lock:
        if _runtime is None:
            _runtime = _load_uncached()
        return _runtime
