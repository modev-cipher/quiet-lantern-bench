# Provenance (AGENTS.md §5a): Resemble AI Chatterbox.
# Source: https://github.com/resemble-ai/chatterbox @ 5de7a54 (src/chatterbox/tts.py,
#   models/s3gen/const.py, models/s3tokenizer/s3tokenizer.py)
# Package: https://pypi.org/project/chatterbox-tts/0.1.7/ (wheel tts.py is byte-identical
#   to 5de7a54; MIT)
# Checked: 2026-08-08.
# Load-bearing upstream facts this module depends on:
#   - ChatterboxTTS.from_local(ckpt_dir, device) reads five files from ckpt_dir;
#     from_pretrained() calls hf_hub_download and is therefore never used.
#   - ChatterboxTTS.sr == S3GEN_SR == 24000.
#   - generate() takes no seed; sampling reads the global torch RNG.
#   - generate() mutates self.conds (prepare_conditionals on audio_prompt_path, and
#     the emotion_adv rebuild), so conds handling and generation share one lock.
#   - generate() re-runs prepare_conditionals on every call that passes
#     audio_prompt_path, so conditionals are prepared once and cached here.
#   - t3.inference is capped at max_new_tokens=1000 speech tokens at 25 tokens/s,
#     i.e. 40 s of audio, and truncates silently past that.
#   - EnTokenizer (models/tokenizers, tokenizer.json) is a 704-entry English BPE
#     exposed as model.tokenizer with text_to_tokens(text) -> tensor of ids.
#     Measured on tokenizer.json from the 0.1.7 checkpoint (2026-08-14): English prose
#     encodes at ~0.61 tokens/char; accented Latin and digits are in-vocabulary; every
#     Chinese/Japanese/Arabic/Hebrew character maps to [UNK] (id 1) and decodes to
#     nothing, so UNK-heavy text produces unbounded babble instead of speech.
#     SLOWEST_TOKENS_PER_SECOND below is 10 chars/s (the char-budget floor this module
#     shipped with) x 0.61 tokens/char, rounded down to 6.0.

import gc
import hashlib
import threading
from collections.abc import Callable
from pathlib import Path
from typing import Any

from txtcaster_engine.tts import chunk as chunking
from txtcaster_engine.tts.backend import (
    SynthResult,
    TTSBackend,
    TTSBackendError,
    configure_torch_threads,
)
from txtcaster_engine.tts.voice import sha256_file

CHATTERBOX_SAMPLE_RATE = 24_000
REQUIRED_FILES = (
    "ve.safetensors",
    "t3_cfg.safetensors",
    "s3gen.safetensors",
    "tokenizer.json",
    "conds.pt",
)

MAX_SPEECH_TOKENS = 1000
SPEECH_TOKENS_PER_SECOND = 25
SPEECH_SECONDS_CEILING = MAX_SPEECH_TOKENS / SPEECH_TOKENS_PER_SECOND
SLOWEST_CHARS_PER_SECOND = 10.0
SLOWEST_TOKENS_PER_SECOND = 6.0
CHUNK_HEADROOM = 0.6
MAX_CHUNK_CHARS = int(SPEECH_SECONDS_CEILING * SLOWEST_CHARS_PER_SECOND * CHUNK_HEADROOM)
MAX_CHUNK_TOKENS = int(SPEECH_SECONDS_CEILING * SLOWEST_TOKENS_PER_SECOND * CHUNK_HEADROOM)
TRUNCATION_GUARD_SECONDS = SPEECH_SECONDS_CEILING * 0.975
UNK_TOKEN_ID = 1
MAX_UNSPEAKABLE_SHARE = 0.5

MAX_CONDITIONALS_CACHED = 4
CHUNK_ATTEMPTS = 3

DEFAULT_EXAGGERATION = 0.5
DEFAULT_CFG_WEIGHT = 0.5
DEFAULT_TEMPERATURE = 0.8


def chunk_text(
    text: str,
    measure: Callable[[str], int] = len,
    budget: int = MAX_CHUNK_CHARS,
) -> list[str]:
    return chunking.chunk_text(text, measure, budget)


def derive_chunk_seed(job_seed: int, chunk: str, attempt: int = 0) -> int:
    seed_bytes = (int(job_seed) & 0xFFFFFFFF).to_bytes(4, "big")
    salt = b"" if attempt == 0 else f":retry{attempt}".encode()
    digest = hashlib.sha256(seed_bytes + chunk.encode("utf-8") + salt).digest()
    return int.from_bytes(digest[:4], "big")


def _float(params: dict[str, object], key: str, fallback: float) -> float:
    if key not in params:
        return fallback
    raw = params[key]
    if isinstance(raw, bool) or not isinstance(raw, int | float):
        raise TTSBackendError(f"tts parameter {key} must be a number, got {raw!r}")
    return float(raw)


def _int(params: dict[str, object], key: str, fallback: int) -> int:
    if key not in params:
        return fallback
    raw = params[key]
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise TTSBackendError(f"tts parameter {key} must be a whole number, got {raw!r}")
    return raw


class ChatterboxBackend(TTSBackend):
    def __init__(self, model_dir: str, device: str = "cpu") -> None:
        self.model_dir = Path(model_dir)
        missing = [name for name in REQUIRED_FILES if not (self.model_dir / name).is_file()]
        if missing:
            raise TTSBackendError(
                f"chatterbox model files missing in {model_dir}: {', '.join(missing)}"
            )
        configure_torch_threads()
        import torch
        from chatterbox.tts import ChatterboxTTS

        if device.startswith("cuda") and not torch.cuda.is_available():
            device = "cpu"
        self.device = device
        model = ChatterboxTTS.from_local(self.model_dir, device)
        if model.sr != CHATTERBOX_SAMPLE_RATE:
            raise TTSBackendError(
                f"chatterbox reports sample rate {model.sr}, expected {CHATTERBOX_SAMPLE_RATE}"
            )
        self._model: Any = model
        self._rng_devices = [torch.device(device)] if device.startswith("cuda") else []
        self._conds: dict[tuple[str, str, float], Any] = {}
        self._synth_lock = threading.Lock()

    def unload(self) -> None:
        with self._synth_lock:
            self._conds.clear()
            model = self._model
            self._model = None
            if model is not None:
                model.conds = None
            del model
        gc.collect()
        import torch

        if torch.cuda.is_available():
            torch.cuda.empty_cache()

    def _conditionals(self, key: tuple[str, str, float], reference_path: str) -> Any:
        cached = self._conds.get(key)
        if cached is not None:
            self._conds[key] = self._conds.pop(key)
            return cached
        measured = sha256_file(Path(reference_path))
        if measured != key[1]:
            raise TTSBackendError(
                "the reference recording on disk does not match the one this voice was saved with"
            )
        self._model.prepare_conditionals(reference_path, exaggeration=key[2])
        prepared = self._model.conds
        self._conds[key] = prepared
        while len(self._conds) > MAX_CONDITIONALS_CACHED:
            self._conds.pop(next(iter(self._conds)))
        return prepared

    def _token_ids(self, text: str) -> list[int]:
        model = self._model
        if model is None:
            raise TTSBackendError("chatterbox model was unloaded")
        return [int(token) for token in model.tokenizer.text_to_tokens(text).flatten()]

    def _generate(self, chunk: str, seed: int, options: dict[str, float], conds: Any) -> Any:
        import torch

        with self._synth_lock:
            if self._model is None:
                raise TTSBackendError("chatterbox model was unloaded")
            self._model.conds = conds
            with torch.random.fork_rng(devices=self._rng_devices):
                torch.manual_seed(seed)
                return self._model.generate(chunk, **options)

    def _render_chunk(
        self, chunk: str, job_seed: int, options: dict[str, float], conds: Any
    ) -> Any:
        import numpy as np

        for attempt in range(CHUNK_ATTEMPTS):
            wav = self._generate(chunk, derive_chunk_seed(job_seed, chunk, attempt), options, conds)
            samples = wav.squeeze(0).detach().cpu().numpy().astype(np.float32)
            if samples.size / CHATTERBOX_SAMPLE_RATE < TRUNCATION_GUARD_SECONDS:
                return samples
        raise TTSBackendError(
            f"chatterbox ran into its {SPEECH_SECONDS_CEILING:.0f} s generation ceiling on every "
            f"attempt; this sentence would have been cut off mid-word"
        )

    def synthesize(
        self,
        text: str,
        voice_id: str,
        params: dict[str, object] | None,
        language: str | None = None,
    ) -> SynthResult:
        opts = params or {}
        reference_raw = opts.get("reference_wav_path")
        reference_path = str(reference_raw) if reference_raw else ""
        if not reference_path:
            raise TTSBackendError(f"voice profile {voice_id} has no reference recording")
        if not Path(reference_path).is_file():
            raise TTSBackendError(f"the reference recording for voice {voice_id} is missing")
        ref_sha256 = str(opts.get("ref_sha256") or "")
        if not ref_sha256:
            raise TTSBackendError(
                f"voice {voice_id} supplied a reference recording without its checksum"
            )
        options = {
            "exaggeration": _float(opts, "exaggeration", DEFAULT_EXAGGERATION),
            "cfg_weight": _float(opts, "cfg_weight", DEFAULT_CFG_WEIGHT),
            "temperature": _float(opts, "temperature", DEFAULT_TEMPERATURE),
        }
        job_seed = _int(opts, "seed", 0)
        conds_key = (reference_path, ref_sha256, options["exaggeration"])

        token_ids = self._token_ids(text)
        if token_ids:
            unspeakable = sum(1 for token in token_ids if token == UNK_TOKEN_ID) / len(token_ids)
            if unspeakable > MAX_UNSPEAKABLE_SHARE:
                raise TTSBackendError(
                    f"chatterbox cannot speak this text: {unspeakable:.0%} of it falls outside "
                    "the voice engine's English-only vocabulary"
                )

        chunks = chunk_text(
            text,
            measure=lambda piece: len(self._token_ids(piece)),
            budget=MAX_CHUNK_TOKENS,
        )
        if not chunks:
            raise TTSBackendError("chatterbox received no speakable text")

        import numpy as np

        with self._synth_lock:
            if self._model is None:
                raise TTSBackendError("chatterbox model was unloaded")
            conds = self._conditionals(conds_key, reference_path)

        rendered = [self._render_chunk(chunk, job_seed, options, conds) for chunk in chunks]
        combined = rendered[0] if len(rendered) == 1 else np.concatenate(rendered)
        return SynthResult(samples=combined, sample_rate=CHATTERBOX_SAMPLE_RATE, words=[])
