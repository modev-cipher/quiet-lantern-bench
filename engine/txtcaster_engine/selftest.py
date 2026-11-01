# Provenance (AGENTS.md §5a): the provider entry points this probe asserts exist in a
# frozen build, because PyInstaller's static analysis cannot see them.
# Chatterbox: https://github.com/resemble-ai/chatterbox @ 5de7a54 — ChatterboxTTS is
#   exported from chatterbox.tts; models/s3gen/matcha/transformer.py imports
#   diffusers.models.lora.LoRACompatibleLinear; the exact diffusers==0.29.0 pin is upstream
#   chatterbox-tts's own, and the class is still present through diffusers 0.40.0 (2026-09-10).
# Perth: https://github.com/resemble-ai/Perth — PerthImplicitWatermarker().apply_watermark(
#   wav, sample_rate=...). perth/__init__.py swallows the ImportError and leaves the
#   symbol None, and the weights load through pkg_resources.resource_filename, so this
#   probe constructs it and runs it rather than only importing the module.
# Checked: 2026-08-08.

import importlib
from typing import IO

IMPORT_PROBES: tuple[tuple[str, str | None], ...] = (
    ("chatterbox.tts", "ChatterboxTTS"),
    ("diffusers.models.lora", "LoRACompatibleLinear"),
    ("perth", "PerthImplicitWatermarker"),
    ("librosa", "load"),
    ("librosa.effects", "trim"),
    ("s3tokenizer", None),
    ("torchaudio", None),
    ("pysbd", "Segmenter"),
    ("kokoro", "KModel"),
    ("faster_whisper", "WhisperModel"),
)

WATERMARK_PROBE_SAMPLE_RATE = 24_000
WATERMARK_PROBE_SAMPLES = 2_400


def check_watermarker(out: IO[str]) -> int:
    try:
        import numpy as np
        import perth

        watermarker = perth.PerthImplicitWatermarker()
        marked = watermarker.apply_watermark(
            np.zeros(WATERMARK_PROBE_SAMPLES, dtype=np.float32),
            sample_rate=WATERMARK_PROBE_SAMPLE_RATE,
        )
    except Exception as exc:
        print(f"WATERMARK FAIL: {type(exc).__name__}: {exc}", file=out)
        return 1
    if getattr(marked, "size", 0) != WATERMARK_PROBE_SAMPLES:
        print(f"WATERMARK FAIL: returned {getattr(marked, 'size', None)} samples", file=out)
        return 1
    print("watermarker ok", file=out)
    return 0


def check_language_pipeline(out: IO[str]) -> int:
    try:
        import spacy

        if not spacy.util.is_package("en_core_web_sm"):
            print("LANGUAGE FAIL: en_core_web_sm metadata is missing", file=out)
            return 1
        nlp = spacy.load("en_core_web_sm", enable=["tok2vec", "tagger"])
        tokens = nlp("Welcome to reading.")
        if not tokens or not all(token.tag_ for token in tokens):
            print("LANGUAGE FAIL: English tagger returned no tags", file=out)
            return 1
    except Exception as exc:
        print(f"LANGUAGE FAIL: {type(exc).__name__}: {exc}", file=out)
        return 1
    print("language pipeline ok", file=out)
    return 0


def check_imports(out: IO[str]) -> int:
    from txtcaster_engine.tts.g2p import english

    english.install()
    failures = 0
    for module_name, attribute in IMPORT_PROBES:
        try:
            module = importlib.import_module(module_name)
        except Exception as exc:
            failures += 1
            print(f"IMPORT FAIL {module_name}: {type(exc).__name__}: {exc}", file=out)
            continue
        if attribute is not None and getattr(module, attribute, None) is None:
            failures += 1
            print(f"IMPORT FAIL {module_name}.{attribute} is missing", file=out)
            continue
        print(f"import ok {module_name}", file=out)
    failures += check_watermarker(out)
    failures += check_language_pipeline(out)
    print(f"IMPORT CHECK {'FAIL' if failures else 'PASS'}", file=out)
    return 1 if failures else 0
