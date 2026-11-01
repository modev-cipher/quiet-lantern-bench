# Provenance (AGENTS.md §5a): misaki's grapheme-to-phoneme fallback network.
# Source: https://github.com/hexgrad/misaki/blob/232a5e32/misaki/en.py (checked 2026-10-20):
#   `FallbackNetwork` loads `PeterReid/graphemes_to_phonemes_en_{us,gb}` with
#   `BartForConditionalGeneration`, maps graphemes through `config.grapheme_chars` (unknown -> 3)
#   between BOS 1 and EOS 2, keeps generated ids above 3 through `config.phoneme_chars`, and
#   returns `(phonemes, 1)`.
# Weights: https://huggingface.co/PeterReid/graphemes_to_phonemes_en_us revision a5631b28 and
#   https://huggingface.co/PeterReid/graphemes_to_phonemes_en_gb revision d8357d50, Apache-2.0
#   (checked 2026-10-20).
import hashlib
import math
import re
import threading
from pathlib import Path
from typing import Any, Protocol

from txtcaster_engine.tts.backend import TTSBackendError

WEIGHTS_DIR = Path(__file__).resolve().parent / "weights"
WEIGHT_FILES = ("config.json", "generation_config.json", "model.safetensors")
GENERATION_CONFIG_SHA256 = "79a11240b01c6d34cab0b122b63a2b2dd5938c4d37d1530e14144d27e8ba6272"
PINNED_SHA256 = {
    "us": {
        "config.json": "8deb3537fb29c63cd9f20d75515ae06e4c92f1b6db0703a2d45bca95b33a53a4",
        "generation_config.json": GENERATION_CONFIG_SHA256,
        "model.safetensors": "dc4a02e62d4fcb4bb4097ecf00db89b8e1a12a549a52ab6adfbba220b80a55c5",
    },
    "gb": {
        "config.json": "e4f248e6af0cfb6cb54aea6ff0168d16b6f3cdabed438c487cde4ab0815d1bac",
        "generation_config.json": GENERATION_CONFIG_SHA256,
        "model.safetensors": "4994f474bb6f4584076a4e98189caaec11aa3f773a0d82d5bc263a03dd07e703",
    },
}
BOS, EOS, LAST_SPECIAL = 1, 2, 3
LENGTH_HEADROOM = 8


class FallbackWeightsError(TTSBackendError):
    pass


class _Token(Protocol):
    text: str


def verify_weights(directory: Path, pins: dict[str, str]) -> None:
    for name in WEIGHT_FILES:
        path = directory / name
        if not path.is_file():
            raise FallbackWeightsError(f"pronunciation fallback file missing: {path}")
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if digest != pins[name]:
            raise FallbackWeightsError(f"pronunciation fallback file differs from its pin: {path}")


class BartFallback:
    def __init__(self, british: bool) -> None:
        variant = "gb" if british else "us"
        directory = WEIGHTS_DIR / variant
        verify_weights(directory, PINNED_SHA256[variant])
        import torch
        from transformers import BartForConditionalGeneration

        self._torch = torch
        loader: Any = BartForConditionalGeneration
        self._model: Any = loader.from_pretrained(str(directory), local_files_only=True).eval()
        config = self._model.config
        self._grapheme_ids = {g: i for i, g in enumerate(config.grapheme_chars)}
        self._phonemes = dict(enumerate(config.phoneme_chars))
        self._positions = int(config.max_position_embeddings)
        letters = "".join(g for g in self._grapheme_ids if g != "_")
        self._spellable = f"[{re.escape(letters)}]+"
        self._lock = threading.Lock()

    def __call__(self, token: _Token) -> tuple[str, int]:
        runs = re.findall(self._spellable, token.text)
        return " ".join("".join(map(self._spell, self._windows(run))) for run in runs), 1

    def _windows(self, run: str) -> list[str]:
        capacity = self._positions - 2
        count = math.ceil(len(run) / capacity)
        size = math.ceil(len(run) / count)
        return [run[start : start + size] for start in range(0, len(run), size)]

    def _spell(self, text: str) -> str:
        ids = [BOS, *(self._grapheme_ids[g] for g in text), EOS]
        with self._lock, self._torch.no_grad():
            generated = self._model.generate(
                input_ids=self._torch.tensor([ids]),
                max_length=min(self._positions, len(ids) + LENGTH_HEADROOM),
            )
        ids = [t for t in generated[0].tolist() if t > LAST_SPECIAL]
        return "".join(self._phonemes.get(t, "") for t in ids)
