# -*- mode: python ; coding: utf-8 -*-
from PyInstaller.utils.hooks import (
    collect_data_files,
    collect_dynamic_libs,
    collect_submodules,
    copy_metadata,
)


def _short_enough(entry):
    dest = entry[0].replace("\\", "/")
    return ".dist-info/licenses/third_party" not in dest and len(dest) <= 180


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
    excludes=["tkinter", "test"],
    noarchive=False,
)

a.datas = [entry for entry in a.datas if _short_enough(entry)]
a.binaries = [entry for entry in a.binaries if _short_enough(entry)]

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
