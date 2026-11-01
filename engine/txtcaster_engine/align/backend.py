"""CTC forced alignment backends for txtCaster.

Provenance & Category A conformance:
- whisperX forced alignment: Ported and adapted from whisperx/alignment.py
  (https://github.com/m-bain/whisperX, BSD-2-Clause License, Copyright (c) 2022 Max Bain).
- Hugging Face transformers Wav2Vec2:
  https://huggingface.co/docs/transformers/model_doc/wav2vec2
  (checked 2026-09-14).
- Hugging Face offline loading (local_files_only=True):
  https://huggingface.co/docs/transformers/main_classes/model#transformers.PreTrainedModel.from_pretrained
  (checked 2026-09-14).
- PyTorch CTC Forced Alignment Tutorial (trellis DP & backtracking):
  https://pytorch.org/audio/stable/tutorials/forced_alignment_tutorial.html
  (checked 2026-09-14).
- Model card & license: facebook/wav2vec2-base-960h is licensed under Apache-2.0
  (commercial-OK permissive license, verified live from
  https://huggingface.co/api/models/facebook/wav2vec2-base-960h on 2026-09-14).
"""

from __future__ import annotations

import functools
import gc
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from txtcaster_engine.tts.backend import Word

if TYPE_CHECKING:
    import torch

CT2_DEVICE_INDEX = 0


@functools.cache
def ct2_cuda_available() -> bool:
    try:
        import ctranslate2
    except Exception:
        return False
    try:
        return int(ctranslate2.get_cuda_device_count()) > 0
    except Exception:
        return False


class AlignBackendError(Exception):
    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.message = message


class AlignBackend:
    def unload(self) -> None:
        return None

    def align(self, audio_path: str, text: str, language: str | None) -> list[Word]:
        raise NotImplementedError


class FakeAlignBackend(AlignBackend):
    def __init__(self, fail_texts: set[str] | None = None) -> None:
        self.fail_texts = fail_texts or set()
        self.calls: list[str] = []

    def align(self, audio_path: str, text: str, language: str | None) -> list[Word]:
        self.calls.append(text)
        if text in self.fail_texts:
            raise AlignBackendError("synthetic align failure")
        import wave

        with wave.open(audio_path, "rb") as reader:
            frames = reader.getnframes()
            rate = reader.getframerate()
        if frames <= 0 or rate <= 0:
            return []
        total_ms = round(frames * 1000 / rate)
        tokens = text.split()
        if not tokens:
            return []
        step = total_ms / len(tokens)
        words: list[Word] = []
        for i, token in enumerate(tokens):
            start = round(i * step)
            end = round((i + 1) * step)
            words.append(Word(w=token, s=start, e=max(end, start + 1)))
        return words


class FasterWhisperBackend(AlignBackend):
    def __init__(self, model_dir: str, device: str = "cpu") -> None:
        from pathlib import Path

        from faster_whisper import WhisperModel

        if not (Path(model_dir) / "model.bin").is_file():
            raise AlignBackendError(f"align model files missing in: {model_dir}")
        compute_type = "float16" if device.startswith("cuda") else "int8"
        resolved_device = "cuda" if device.startswith("cuda") else "cpu"
        try:
            self.model = WhisperModel(
                model_dir,
                device=resolved_device,
                device_index=CT2_DEVICE_INDEX,
                compute_type=compute_type,
                local_files_only=True,
            )
        except Exception as exc:
            raise AlignBackendError(f"failed to load align model: {exc}") from exc

    def unload(self) -> None:
        model = getattr(self, "model", None)
        self.model = None
        if model is None:
            return
        del model
        gc.collect()

    def align(self, audio_path: str, text: str, language: str | None) -> list[Word]:
        try:
            segments, _info = self.model.transcribe(
                audio_path,
                language=language,
                word_timestamps=True,
                beam_size=5,
                condition_on_previous_text=False,
                vad_filter=False,
            )
            words: list[Word] = []
            for segment in segments:
                for word in segment.words or []:
                    token = word.word.strip()
                    if not token:
                        continue
                    words.append(
                        Word(w=token, s=round(word.start * 1000), e=round(word.end * 1000))
                    )
            return words
        except AlignBackendError:
            raise
        except Exception as exc:
            raise AlignBackendError(f"alignment failed: {exc}") from exc


@dataclass
class _Point:
    token_index: int
    time_index: int
    score: float


@dataclass
class _Segment:
    label: str
    token_index: int
    start: int
    end: int
    score: float


def _get_trellis(
    emission: torch.Tensor,
    tokens: list[int],
    blank_id: int = 0,
) -> torch.Tensor:
    import torch

    num_frame = emission.size(0)
    num_tokens = len(tokens)
    trellis = torch.empty((num_frame + 1, num_tokens + 1), dtype=emission.dtype)
    trellis[0, 0] = 0.0
    trellis[1:, 0] = torch.cumsum(emission[:, blank_id], 0)
    trellis[0, -num_tokens:] = -float("inf")
    trellis[-num_tokens:, 0] = float("inf")
    for t in range(num_frame):
        trellis[t + 1, 1:] = torch.maximum(
            trellis[t, 1:] + emission[t, blank_id],
            trellis[t, :-1] + emission[t, tokens],
        )
    return trellis


def _backtrack(
    trellis: torch.Tensor,
    emission: torch.Tensor,
    tokens: list[int],
    blank_id: int = 0,
) -> list[_Point] | None:
    import torch

    j = trellis.size(1) - 1
    t_start = torch.argmax(trellis[:, j]).item()
    path: list[_Point] = []
    for t in range(int(t_start), 0, -1):
        stayed = trellis[t - 1, j] + emission[t - 1, blank_id]
        changed = trellis[t - 1, j - 1] + emission[t - 1, tokens[j - 1]]
        prob = emission[t - 1, tokens[j - 1] if changed > stayed else blank_id].exp().item()
        path.append(_Point(j - 1, t - 1, float(prob)))
        if changed > stayed:
            j -= 1
            if j == 0:
                break
    else:
        return None
    return path[::-1]


def _merge_repeats(path: list[_Point], transcript: list[str]) -> list[_Segment]:
    i1, i2 = 0, 0
    segments: list[_Segment] = []
    while i1 < len(path):
        while i2 < len(path) and path[i1].token_index == path[i2].token_index:
            i2 += 1
        score = sum(path[k].score for k in range(i1, i2)) / (i2 - i1)
        segments.append(
            _Segment(
                label=transcript[path[i1].token_index],
                token_index=path[i1].token_index,
                start=path[i1].time_index,
                end=path[i2 - 1].time_index + 1,
                score=score,
            )
        )
        i1 = i2
    return segments


def _extract_mappable_chars(token: str, vocab_dict: dict[str, int]) -> list[str]:
    normalized = token.replace("’", "'").replace("‘", "'")
    chars: list[str] = []
    for ch in normalized:
        ch_upper = ch.upper()
        if ch_upper in vocab_dict and ch_upper not in ("<PAD>", "<s>", "</s>", "<UNK>", "|"):
            chars.append(ch_upper)
    return chars


def _interpolate_missing_spans(
    spans: list[tuple[int, int] | None],
    duration_ms: int,
) -> list[tuple[int, int]]:
    n = len(spans)
    if n == 0:
        return []
    known_indices = [i for i, s in enumerate(spans) if s is not None]
    if not known_indices:
        return [
            (
                round(i * duration_ms / n),
                max(round(i * duration_ms / n), round((i + 1) * duration_ms / n)),
            )
            for i in range(n)
        ]

    result = list(spans)
    first_k = known_indices[0]
    if first_k > 0:
        first_start = result[first_k][0]  # type: ignore[index]
        for i in range(first_k):
            s = round(i * first_start / first_k)
            e = round((i + 1) * first_start / first_k)
            result[i] = (s, max(s, e))

    last_k = known_indices[-1]
    if last_k < n - 1:
        last_end = result[last_k][1]  # type: ignore[index]
        rem = n - 1 - last_k
        span_len = max(0, duration_ms - last_end)
        for step, i in enumerate(range(last_k + 1, n)):
            s = round(last_end + step * span_len / rem)
            e = round(last_end + (step + 1) * span_len / rem)
            result[i] = (s, max(s, e))

    for k in range(len(known_indices) - 1):
        idx_left = known_indices[k]
        idx_right = known_indices[k + 1]
        gap = idx_right - idx_left - 1
        if gap > 0:
            left_end = result[idx_left][1]  # type: ignore[index]
            right_start = result[idx_right][0]  # type: ignore[index]
            if right_start < left_end:
                right_start = left_end
            span_len = right_start - left_end
            for step, i in enumerate(range(idx_left + 1, idx_right)):
                s = round(left_end + step * span_len / gap)
                e = round(left_end + (step + 1) * span_len / gap)
                result[i] = (s, max(s, e))

    final_spans: list[tuple[int, int]] = []
    prev_s = 0
    for s, e in result:  # type: ignore[misc]
        clamped_s = max(0, min(duration_ms, s))
        clamped_e = max(clamped_s, min(duration_ms, e))
        if clamped_s < prev_s:
            clamped_s = prev_s
            clamped_e = max(clamped_s, clamped_e)
        prev_s = clamped_s
        final_spans.append((clamped_s, clamped_e))

    return final_spans


class Wav2Vec2CtcBackend(AlignBackend):
    model: Any | None
    processor: Any | None

    def __init__(self, model_dir: str, device: str = "cpu") -> None:
        from pathlib import Path

        if not Path(model_dir).is_dir():
            raise AlignBackendError(f"align model directory missing: {model_dir}")
        try:
            from transformers import Wav2Vec2ForCTC, Wav2Vec2Processor
        except Exception as exc:
            raise AlignBackendError(f"failed to import transformers: {exc}") from exc

        try:
            processor: Any = Wav2Vec2Processor.from_pretrained(model_dir, local_files_only=True)
            model: Any = Wav2Vec2ForCTC.from_pretrained(model_dir, local_files_only=True)
            model.to("cpu")
            model.eval()
            self.processor = processor
            self.model = model
        except Exception as exc:
            raise AlignBackendError(
                f"failed to load wav2vec2 model from {model_dir}: {exc}"
            ) from exc

        vocab = self.processor.tokenizer.get_vocab()
        self.vocab_dict: dict[str, int] = {k.upper(): v for k, v in vocab.items()}
        self.blank_id: int = vocab.get("<pad>", 0)

    def unload(self) -> None:
        model = getattr(self, "model", None)
        processor = getattr(self, "processor", None)
        self.model = None
        self.processor = None
        if model is not None:
            del model
        if processor is not None:
            del processor
        gc.collect()

    def align(self, audio_path: str, text: str, language: str | None) -> list[Word]:
        if language is not None and language.lower().strip() not in ("en", "english"):
            raise AlignBackendError(
                f"unsupported language: '{language}'; "
                "Wav2Vec2CtcBackend only supports English ('en')"
            )

        from pathlib import Path

        if not Path(audio_path).is_file():
            raise AlignBackendError(f"audio file not found: {audio_path}")

        try:
            import soundfile
            import torch
            import torchaudio.functional as F  # type: ignore[import-untyped]
        except Exception as exc:
            raise AlignBackendError(f"failed to import audio libraries: {exc}") from exc

        try:
            audio_data, sample_rate = soundfile.read(audio_path, dtype="float32")
        except Exception as exc:
            raise AlignBackendError(f"failed to decode audio file {audio_path}: {exc}") from exc

        waveform = torch.from_numpy(audio_data)
        if waveform.ndim == 1:
            waveform = waveform.unsqueeze(0)
        elif waveform.ndim == 2:
            waveform = waveform.t()
            if waveform.shape[0] > 1:
                waveform = torch.mean(waveform, dim=0, keepdim=True)
        else:
            raise AlignBackendError(
                f"unsupported audio format in {audio_path}: shape {waveform.shape}"
            )

        if sample_rate != 16000:
            waveform = F.resample(waveform, orig_freq=sample_rate, new_freq=16000)
        waveform = waveform.float()

        num_samples = waveform.shape[-1]
        duration_ms = max(0, round(num_samples * 1000 / 16000))

        raw_tokens = text.split()
        if not raw_tokens:
            return []

        if self.model is None or self.processor is None:
            raise AlignBackendError("wav2vec2 model is not loaded")

        if waveform.shape[-1] < 400:
            waveform = torch.nn.functional.pad(waveform, (0, 400 - waveform.shape[-1]))

        try:
            with torch.inference_mode():
                logits = self.model(waveform).logits
                emissions = torch.log_softmax(logits, dim=-1)
            emission = emissions[0].cpu()
        except Exception as exc:
            raise AlignBackendError(f"CTC model inference failed: {exc}") from exc

        num_frames = emission.size(0)

        target_chars: list[str] = []
        target_token_indices: list[int | None] = []
        first = True
        for token_idx, token in enumerate(raw_tokens):
            chars = _extract_mappable_chars(token, self.vocab_dict)
            if not chars:
                continue
            if not first:
                target_chars.append("|")
                target_token_indices.append(None)
            first = False
            for ch in chars:
                target_chars.append(ch)
                target_token_indices.append(token_idx)

        token_spans: dict[int, list[tuple[int, int]]] = {i: [] for i in range(len(raw_tokens))}
        if target_chars and num_frames >= len(target_chars):
            target_token_ids = [self.vocab_dict[ch] for ch in target_chars]
            trellis = _get_trellis(emission, target_token_ids, blank_id=self.blank_id)
            path = _backtrack(trellis, emission, target_token_ids, blank_id=self.blank_id)
            if path is not None:
                char_segs = _merge_repeats(path, target_chars)
                for seg in char_segs:
                    t_idx = target_token_indices[seg.token_index]
                    if t_idx is not None:
                        s_ms = round(seg.start * duration_ms / num_frames)
                        e_ms = round(seg.end * duration_ms / num_frames)
                        token_spans[t_idx].append((s_ms, e_ms))

        raw_spans: list[tuple[int, int] | None] = []
        for i in range(len(raw_tokens)):
            spans = token_spans[i]
            if spans:
                s = min(x[0] for x in spans)
                e = max(x[1] for x in spans)
                raw_spans.append((s, max(s, e)))
            else:
                raw_spans.append(None)

        final_spans = _interpolate_missing_spans(raw_spans, duration_ms)
        words: list[Word] = []
        for i, (s, e) in enumerate(final_spans):
            words.append(Word(w=raw_tokens[i], s=s, e=e))

        return words
