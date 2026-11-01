# Provenance (AGENTS.md §5a): Kokoro TTS, the default narration slot.
# Source: https://huggingface.co/hexgrad/Kokoro-82M (checked 2026-09-27):
#   sample rate 24000 Hz; usage `KPipeline(lang_code='a')` (American English);
#   pip pin `kokoro>=0.9.2` (engine pins kokoro==0.9.4 for the tts/g2p import shims).
# Source: https://github.com/hexgrad/kokoro/blob/main/kokoro/model.py (checked
#   2026-09-27): `MODEL_NAMES['hexgrad/Kokoro-82M'] = 'kokoro-v1_0.pth'` and the
#   default config fetch is `hf_hub_download(repo_id=…, filename='config.json')`;
#   `KModel(repo_id=…, config=…, model=…, disable_complex=False)`.
import gc
import threading
from pathlib import Path
from typing import Any

from txtcaster_engine.tts.backend import SynthResult, TTSBackend, TTSBackendError, Word

KOKORO_SAMPLE_RATE = 24_000
REPO_ID = "hexgrad/Kokoro-82M"
DEFAULT_LANG_CODE = "a"
ENGLISH_LANG_CODES = frozenset("ab")
SENTENCE_SPLIT_PATTERN = r"(?<=[.!?])\s+"
SENTENCE_SPLIT_PATTERN = r"(?<=[.!?])\s+"


class KokoroBackend(TTSBackend):
    def __init__(self, model_dir: str, device: str = "cpu") -> None:
        self.model_dir = Path(model_dir)
        config_path = self.model_dir / "config.json"
        weights_path = self.model_dir / "kokoro-v1_0.pth"
        if not config_path.is_file() or not weights_path.is_file():
            raise TTSBackendError(f"kokoro model files missing in {model_dir}")
        from txtcaster_engine.tts.backend import configure_torch_threads

        configure_torch_threads()
        from txtcaster_engine.tts.g2p import english

        english.install()
        import torch
        from kokoro import KModel, KPipeline

        if device.startswith("cuda") and not torch.cuda.is_available():
            device = "cpu"
        self.device = device
        model = KModel(repo_id=REPO_ID, config=str(config_path), model=str(weights_path)).to(device)
        model.eval()
        self._pipelines: dict[str, Any] = {}
        self._pipeline_cls = KPipeline
        self._model = model
        self._pipeline_lock = threading.Lock()
        self._synth_lock = threading.Lock()

    def unload(self) -> None:
        with self._pipeline_lock:
            self._pipelines.clear()
        model = self._model
        self._model = None
        if model is None:
            return
        del model
        gc.collect()

    def _pipeline(self, lang_code: str) -> Any:
        with self._pipeline_lock:
            if lang_code not in self._pipelines:
                pipeline = self._pipeline_cls(
                    lang_code=lang_code,
                    repo_id=REPO_ID,
                    model=self._model,
                    device=self.device,
                )
                if pipeline.lang_code in ENGLISH_LANG_CODES:
                    from txtcaster_engine.tts.g2p.unknown_words import (
                        BartFallback,
                        FallbackWeightsError,
                    )

                    if not isinstance(pipeline.g2p.fallback, BartFallback):
                        raise FallbackWeightsError(
                            "the pronunciation model for unknown English words did not load"
                        )
                self._pipelines[lang_code] = pipeline
            return self._pipelines[lang_code]

    def _lang_code(self, voice_id: str, params: dict[str, object] | None) -> str:
        return str((params or {}).get("lang_code", voice_id[:1] or DEFAULT_LANG_CODE))

    def check_voice(self, voice_id: str, params: dict[str, object] | None) -> None:
        self._pipeline(self._lang_code(voice_id, params))

    def _voice_path(self, voice_id: str) -> str:
        voice_file = self.model_dir / "voices" / f"{voice_id}.pt"
        if not voice_file.is_file():
            raise TTSBackendError(f"voice pack not installed: {voice_id}")
        return str(voice_file)

    def synthesize(
        self,
        text: str,
        voice_id: str,
        params: dict[str, object] | None,
        language: str | None = None,
    ) -> SynthResult:
        opts = params or {}
        speed_raw = opts.get("speed", 1.0)
        speed = float(speed_raw) if isinstance(speed_raw, int | float) else 1.0
        pipeline = self._pipeline(self._lang_code(voice_id, params))
        voice = self._voice_path(voice_id)

        import numpy as np

        chunks: list[Any] = []
        words: list[Word] = []
        offset_ms = 0.0
        with self._synth_lock:
            for result in pipeline(
                text, voice=voice, speed=speed, split_pattern=SENTENCE_SPLIT_PATTERN
            ):
                audio = result.audio
                if audio is None:
                    continue
                samples = audio.detach().cpu().numpy().astype(np.float32)
                for token in result.tokens or []:
                    start = getattr(token, "start_ts", None)
                    end = getattr(token, "end_ts", None)
                    word = (token.text or "").strip()
                    if not word or start is None or end is None:
                        continue
                    words.append(
                        Word(
                            w=word,
                            s=round(offset_ms + float(start) * 1000),
                            e=round(offset_ms + float(end) * 1000),
                        )
                    )
                offset_ms += samples.size * 1000 / KOKORO_SAMPLE_RATE
                chunks.append(samples)
        if not chunks:
            raise TTSBackendError("kokoro produced no audio")
        combined = chunks[0] if len(chunks) == 1 else np.concatenate(chunks)
        return SynthResult(samples=combined, sample_rate=KOKORO_SAMPLE_RATE, words=words)
