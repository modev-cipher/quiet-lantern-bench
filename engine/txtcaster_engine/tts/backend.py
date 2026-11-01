import contextlib
import functools
import hashlib
import math
from dataclasses import dataclass, field

import numpy as np
from numpy.typing import NDArray

TORCH_INTRAOP_THREADS = 4
TORCH_INTEROP_THREADS = 1


@functools.cache
def configure_torch_threads() -> bool:
    try:
        import torch
    except Exception:
        return False
    with contextlib.suppress(Exception):
        torch.set_num_threads(TORCH_INTRAOP_THREADS)
    with contextlib.suppress(Exception):
        torch.set_num_interop_threads(TORCH_INTEROP_THREADS)
    return True


@functools.cache
def torch_cuda_available() -> bool:
    try:
        import torch
    except Exception:
        return False
    configure_torch_threads()
    try:
        return bool(torch.cuda.is_available())
    except Exception:
        return False


@dataclass
class Word:
    w: str
    s: int
    e: int

    def to_json(self) -> dict[str, object]:
        return {"w": self.w, "s": self.s, "e": self.e}


@dataclass
class SynthResult:
    samples: NDArray[np.float32]
    sample_rate: int
    words: list[Word] = field(default_factory=list)
    is_truncated: bool = False


class TTSBackendError(Exception):
    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.message = message


class TTSBackend:
    @property
    def requires_language_param(self) -> bool:
        return False

    def unload(self) -> None:
        return None

    def check_voice(self, voice_id: str, params: dict[str, object] | None) -> None:
        return None

    def synthesize(
        self,
        text: str,
        voice_id: str,
        params: dict[str, object] | None,
        language: str | None = None,
    ) -> SynthResult:
        raise NotImplementedError


class FakeBackend(TTSBackend):
    def __init__(
        self,
        sample_rate: int = 24_000,
        fail_texts: set[str] | None = None,
        fail_attempts: dict[str, int] | None = None,
        truncate_texts: set[str] | None = None,
    ) -> None:
        self.sample_rate = sample_rate
        self.fail_texts = fail_texts or set()
        self.fail_attempts = dict(fail_attempts or {})
        self.truncate_texts = truncate_texts or set()
        self.calls: list[str] = []

    def synthesize(
        self,
        text: str,
        voice_id: str,
        params: dict[str, object] | None,
        language: str | None = None,
    ) -> SynthResult:
        self.calls.append(text)
        if text in self.fail_texts:
            raise TTSBackendError("synthetic failure")
        remaining = self.fail_attempts.get(text, 0)
        if remaining > 0:
            self.fail_attempts[text] = remaining - 1
            raise TTSBackendError("transient synthetic failure")
        tokens = text.split()
        duration_s = max(0.2, 0.25 * len(tokens))
        n = round(duration_s * self.sample_rate)
        seed = int.from_bytes(hashlib.sha256(f"{text}:{voice_id}".encode()).digest()[:4], "big")
        freq = 200.0 + (seed % 400)
        t = np.arange(n, dtype=np.float32) / self.sample_rate
        samples = (0.5 * np.sin(2 * math.pi * freq * t)).astype(np.float32)
        words: list[Word] = []
        per_token = duration_s * 1000 / max(1, len(tokens))
        for i, token in enumerate(tokens):
            words.append(Word(w=token, s=round(i * per_token), e=round((i + 1) * per_token)))
        return SynthResult(
            samples=samples,
            sample_rate=self.sample_rate,
            words=words,
            is_truncated=text in self.truncate_texts,
        )
