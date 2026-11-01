# Provenance (AGENTS.md §5a): import layout of kokoro 0.9.4 and misaki 0.9.4 (PyPI sdists,
# checked 2026-10-20): `kokoro/pipeline.py` runs `from misaki import en, espeak` at import,
# builds `espeak.EspeakFallback(british=...)` for lang 'a'/'b' inside `try/except Exception`
# and `espeak.EspeakG2P(language=...)` for every lang outside a/b/j/z; `misaki/en.py` runs
# `from num2words import num2words` and calls it with to='cardinal'|'ordinal'|'year' or a float.
import sys
import types
from typing import Any

from txtcaster_engine.tts.backend import TTSBackendError
from txtcaster_engine.tts.g2p import numbers
from txtcaster_engine.tts.g2p.unknown_words import BartFallback


class UnsupportedLanguageError(TTSBackendError):
    pass


def _fallback(british: bool = False) -> BartFallback:
    return BartFallback(british=british)


class _NoPhonemizer:
    def __init__(self, language: str, *args: Any, **kwargs: Any) -> None:
        raise UnsupportedLanguageError(f"no permissive pronunciation model for {language!r}")


def _backend_module() -> types.ModuleType:
    module = types.ModuleType("misaki.espeak")
    vars(module).update(EspeakFallback=_fallback, EspeakG2P=_NoPhonemizer)
    return module


def install() -> None:
    sys.modules["num2words"] = numbers
    backend = _backend_module()
    sys.modules["misaki.espeak"] = backend
    import misaki

    vars(misaki)["espeak"] = backend
    loaded_english = sys.modules.get("misaki.en")
    if loaded_english is not None:
        vars(loaded_english)["num2words"] = numbers.num2words
    loaded_pipeline = sys.modules.get("kokoro.pipeline")
    if loaded_pipeline is not None:
        vars(loaded_pipeline)["espeak"] = backend
