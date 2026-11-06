# -*- mode: python ; coding: utf-8 -*-
import importlib.metadata
import importlib.util
import os
from pathlib import Path

from PyInstaller.utils.hooks import (
    collect_data_files,
    collect_dynamic_libs,
    collect_submodules,
    copy_metadata,
)

NVIDIA_LICENCES = "licenses/nvidia"


def _carries_nvidia_libraries():
    torch = importlib.util.find_spec("torch")
    locations = list(torch.submodule_search_locations or []) if torch else []
    on_windows = any((Path(root) / "lib" / "cudart64_13.dll").is_file() for root in locations)
    try:
        importlib.metadata.distribution("nvidia-cuda-runtime")
    except importlib.metadata.PackageNotFoundError:
        return on_windows
    return True


# The CUDA and cuDNN licences cover the libraries torch carries on Windows; on Linux each
# NVIDIA wheel also contributes its own licence file.
def _nvidia_licence_datas():
    if not _carries_nvidia_libraries():
        return []
    entries = [(str(path), NVIDIA_LICENCES) for path in sorted(Path("licenses/nvidia").glob("*.txt"))]
    nvshmem_licence = os.environ.get("NVSHMEM_LICENSE")
    if nvshmem_licence:
        entries.append((nvshmem_licence, NVIDIA_LICENCES))
    for dist in importlib.metadata.distributions():
        name = dist.metadata["Name"] or ""
        if not name.lower().startswith("nvidia-") or name.lower() == "nvidia-nvshmem-cu13":
            continue
        for file in dist.files or []:
            if "licen" in file.name.lower() and not file.name.endswith(".py"):
                entries.append((str(file.locate()), f"{NVIDIA_LICENCES}/{name}"))
    return entries


def _short_enough(entry):
    dest = entry[0].replace("\\", "/")
    return ".dist-info/licenses/third_party" not in dest and len(dest) <= 180


# GPU drivers belong to the user's system: the Vulkan loader and the NVIDIA driver
# libraries are never bundled. llama.cpp's CUDA module finds cudart and cuBLAS through
# the copies torch ships, so other copies PyInstaller resolves from the build toolkit go.
DRIVER_LIBRARIES = ("libvulkan.so", "vulkan-1.dll", "libcuda.so", "nvcuda.dll", "libnvidia-ml.so")
# Outside the CUDA EULA's Attachment A redistributable list and linked by nothing the
# engine loads (profiler metrics and multi-GPU cuSOLVER).
UNLISTED_NVIDIA = (
    "libnvperf_host.so",
    "libnvperf_target.so",
    "libpcsamplingutil.so",
    "libcusolvermg.so",
    "nvperf_host.dll",
    "nvperf_target.dll",
    "pcsamplingutil.dll",
    "cusolvermg64_12.dll",
)
# torch's kernel compiler; the engine turns torch.compile off, so nothing imports it.
UNUSED_LIBRARIES = ("libtriton.so",)
CUDA_RUNTIME = {
    "libcudart.so.13",
    "libcublas.so.13",
    "libcublaslt.so.13",
    "cudart64_13.dll",
    "cublas64_13.dll",
    "cublaslt64_13.dll",
}
TORCH_RUNTIME_DIRS = ("nvidia/cu13/lib/", "torch/lib/")


def _basename(entry):
    return entry[0].replace("\\", "/").rsplit("/", 1)[-1].lower()


def _shippable(entry):
    name = _basename(entry)
    return not any(
        name == lib or name.startswith(lib + ".")
        for lib in DRIVER_LIBRARIES + UNLISTED_NVIDIA + UNUSED_LIBRARIES
    )


def _one_cuda_runtime(binaries):
    owned = {
        _basename(entry)
        for entry in binaries
        if _basename(entry) in CUDA_RUNTIME
        and entry[0].replace("\\", "/").lower().startswith(TORCH_RUNTIME_DIRS)
    }
    return [
        entry
        for entry in binaries
        if _basename(entry) not in owned
        or entry[0].replace("\\", "/").lower().startswith(TORCH_RUNTIME_DIRS)
    ]


# NVRTC opens its builtins library at runtime, so dependency analysis never sees it;
# without it torch cannot compile the kernels some CUDA operations need.
def _nvrtc_builtins(binaries):
    extra = []
    for entry in binaries:
        if not _basename(entry).startswith("libnvrtc.so"):
            continue
        dest = entry[0].replace("\\", "/").rpartition("/")[0]
        for builtins in sorted(Path(entry[1]).parent.glob("libnvrtc-builtins.so.*")):
            extra.append((f"{dest}/{builtins.name}" if dest else builtins.name, str(builtins), "BINARY"))
    return extra


def _not_test_module(name):
    return ".tests" not in name and not name.rpartition(".")[2].startswith("test_")


# Provenance (AGENTS.md §5a): the engine's only diffusers consumers are
# chatterbox/models/s3gen/matcha/{decoder,transformer}.py and the selftest probe
# (diffusers.models.lora.LoRACompatibleLinear — #105, provenance in selftest.py; the class is
# present through diffusers 0.40.0, the ==0.29.0 pin is upstream chatterbox-tts's, 2026-09-10).
# These are the imported entry modules; Analysis chases their static closure, so
# nothing else from diffusers' ~400 collected submodules is needed. Measured by
# grepping the reference venv's chatterbox for `diffusers` imports. Checked: 2026-08-15.
DIFFUSERS_HIDDENIMPORTS = [
    "diffusers",
    "diffusers.models.activations",
    "diffusers.models.attention",
    "diffusers.models.attention_processor",
    "diffusers.models.lora",
    "diffusers.utils.torch_utils",
]


# Provenance (AGENTS.md §5a): distributions whose *metadata* the engine's import path
# resolves, so the frozen bundle must answer importlib.metadata the same way the
# reference venv does. A frozen build that reports "not installed" is not merely
# degraded: transformers.utils.versions.require_version re-raises
# PackageNotFoundError, which aborted the packaged --check-imports probe on
# 'requests' (#88 package gate, both platforms).
# Derived by measurement, not recall: importlib.metadata.{version,distribution,
# metadata,requires} and pkg_resources.get_distribution were wrapped in the CI
# build venv (uv sync --no-group gpu --group cpu --group build) and every lookup
# recorded while importing the selftest probe set and the engine's own
# server/tts modules. Both entry sets yield this same set; names that fail to
# resolve in the reference venv too (tensorflow, wandb, xformers, bitsandbytes,
# gradio, matplotlib, ...) are optional-dependency probes whose callers swallow
# the miss, so absence is the correct answer for those and they stay out.
# Checked: 2026-08-10.
METADATA_DISTRIBUTIONS = (
    "Jinja2",
    "Pillow",
    "accelerate",
    "beautifulsoup4",
    "chatterbox-tts",
    "en-core-web-sm",
    "fastapi",
    "filelock",
    "hf_xet",
    "httpx",
    "huggingface-hub",
    "librosa",
    "numpy",
    "onnx",
    "onnxruntime",
    "opencv-python",
    "packaging",
    "pydantic",
    "pyyaml",
    "regex",
    "requests",
    "rich",
    "safetensors",
    "scipy",
    "tokenizers",
    "torch",
    "torchvision",
    "tqdm",
    "transformers",
)

hiddenimports = (
    collect_submodules("txtcaster_engine")
    + collect_submodules("kokoro")
    + collect_submodules("misaki")
    + collect_submodules("llama_cpp")
    + ["en_core_web_sm"]
    + collect_submodules("pyopenjtalk")
    + collect_submodules("jieba")
    + collect_submodules("pypinyin")
    + collect_submodules("faster_whisper")
    + collect_submodules("chatterbox")
    + collect_submodules("s3tokenizer")
    + DIFFUSERS_HIDDENIMPORTS
    + collect_submodules("librosa")
    + collect_submodules("sklearn", filter=_not_test_module)
    + collect_submodules("perth")
    + [
        "uvicorn.logging",
        "uvicorn.loops.auto",
        "uvicorn.protocols.http.auto",
        "uvicorn.protocols.websockets.auto",
        "uvicorn.lifespan.on",
        "fugashi",
        "unidic_lite",
        "jaconv",
        "mojimoji",
        "ordered_set",
        "cn2an",
        "pypinyin_dict",
        "pkg_resources",
        "torchaudio",
        "soundfile",
        "soxr",
        "conformer",
        "omegaconf",
        "onnx",
        "ml_dtypes",
    ]
)

datas = (
    collect_data_files("kokoro")
    + collect_data_files("misaki")
    + collect_data_files("en_core_web_sm")
    + collect_data_files("pyopenjtalk")
    + collect_data_files("unidic_lite")
    + collect_data_files("jieba")
    + collect_data_files("pypinyin")
    + collect_data_files("pypinyin_dict")
    + collect_data_files("cn2an")
    + collect_data_files("faster_whisper")
    + collect_data_files("perth")
    + collect_data_files("librosa")
    + collect_data_files("soundfile")
    + collect_data_files("numba")
    + collect_data_files("sklearn")
    # txtcaster_engine/assets/fonts: Noto Naskh Arabic (OFL 1.1) + OFL.txt, the EPUB page-mode
    # Arabic face (epub_adapter). Wheel ships it via hatchling packages=; the frozen bundle
    # needs it collected here, else Arabic pages fall back to .notdef boxes.
    + collect_data_files("txtcaster_engine")
    + _nvidia_licence_datas()
    + [entry for name in METADATA_DISTRIBUTIONS for entry in copy_metadata(name)]
)

binaries_extra = (
    collect_dynamic_libs("pyopenjtalk")
    + collect_dynamic_libs("fugashi")
    + collect_dynamic_libs("soundfile")
    + collect_dynamic_libs("soxr")
    + collect_dynamic_libs("llvmlite")
)

binaries = (
    collect_dynamic_libs("llama_cpp")
    + collect_dynamic_libs("ctranslate2")
    + binaries_extra
)

a = Analysis(
    ["txtcaster_engine/__main__.py"],
    pathex=["."],
    binaries=binaries,
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    runtime_hooks=[],
    excludes=["tkinter", "test", "triton"],
    # inflect decorates its engine with typeguard, which reads the source at import time.
    module_collection_mode={"inflect": "pyz+py"},
    noarchive=False,
)

a.datas = [entry for entry in a.datas if _short_enough(entry)]
a.binaries = _one_cuda_runtime(
    [entry for entry in a.binaries if _short_enough(entry) and _shippable(entry)]
)
a.binaries += _nvrtc_builtins(a.binaries)
if any(_basename(entry).startswith("libnvshmem_host") for entry in a.binaries) and not os.environ.get(
    "NVSHMEM_LICENSE"
):
    raise SystemExit("libnvshmem_host is bundled without NVSHMEM_LICENSE; build through scripts/build_engine.sh")

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="txtcaster-engine",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=True,
)

coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=False,
    name="txtcaster-engine",
)
