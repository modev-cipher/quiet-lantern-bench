import hashlib
import json
import logging
import wave
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import suppress
from dataclasses import dataclass, field
from pathlib import Path

from txtcaster_engine.ingest.errors import IngestCancelled
from txtcaster_engine.logctx import job_id_var
from txtcaster_engine.oom import is_oom
from txtcaster_engine.tts.backend import TTSBackend, Word
from txtcaster_engine.tts.cache import cache_path, wav_cache_key, write_atomic
from txtcaster_engine.tts.post import duration_ms, encode_wav, postprocess
from txtcaster_engine.tts.words import (
    TIMEBASE_TRIMMED,
    TIMEBASE_UNKNOWN,
    decode_envelope,
    encode_envelope,
)

MAX_ATTEMPTS = 3
FAILED_FLAG = "tts_failed"
logger = logging.getLogger(__name__)

TRUNCATED_FLAG = "tts_truncated"
NATIVE_ALIGN_ID = "tts_native"


@dataclass
class SegmentInput:
    idx: int
    text: str


@dataclass
class SegmentResult:
    idx: int
    audio_hash: str | None
    wav_bytes: int
    duration_ms: int
    cache_hit: bool
    words: list[Word] = field(default_factory=list)
    flags: list[str] = field(default_factory=list)
    words_timebase: str = TIMEBASE_UNKNOWN

    def to_json(self) -> dict[str, object]:
        return {
            "idx": self.idx,
            "audio_hash": self.audio_hash,
            "wav_bytes": self.wav_bytes,
            "duration_ms": self.duration_ms,
            "cache_hit": self.cache_hit,
            "words": [word.to_json() for word in self.words],
            "flags": list(self.flags),
            "words_timebase": self.words_timebase,
        }


def align_cache_key(audio_hash: str, align_model_id: str = NATIVE_ALIGN_ID) -> str:
    raw = f"{audio_hash}:{align_model_id}"
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def _wav_duration_ms(data: bytes) -> int:
    import io

    with wave.open(io.BytesIO(data), "rb") as reader:
        frames = reader.getnframes()
        rate = reader.getframerate()
    return round(frames * 1000 / rate)


def _read_cached_wav(path: Path) -> tuple[bytes, int] | None:
    if not path.is_file():
        return None
    try:
        data = path.read_bytes()
        duration = _wav_duration_ms(data)
    except (OSError, EOFError, RuntimeError, wave.Error, ZeroDivisionError) as exc:
        logger.warning("tts cache dropped an unreadable wav key=%s: %s", path.stem, exc)
        with suppress(OSError):
            path.unlink()
        return None
    return data, duration


def _load_cached_words(cache_dir: str, audio_hash: str) -> tuple[list[Word], str]:
    path = cache_path(cache_dir, align_cache_key(audio_hash))
    if not path.is_file():
        return [], TIMEBASE_UNKNOWN
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError:
        return [], TIMEBASE_UNKNOWN
    envelope = decode_envelope(raw)
    if envelope is None:
        return [], TIMEBASE_UNKNOWN
    words: list[Word] = []
    for entry in envelope.entries:
        if not (isinstance(entry, dict) and {"w", "s", "e"} <= entry.keys()):
            return [], TIMEBASE_UNKNOWN
        try:
            word = Word(w=str(entry["w"]), s=int(entry["s"]), e=int(entry["e"]))
        except (TypeError, ValueError):
            return [], TIMEBASE_UNKNOWN
        if word.s < 0 or word.e < word.s:
            return [], TIMEBASE_UNKNOWN
        words.append(word)
    return words, envelope.timebase


def _store_words(cache_dir: str, audio_hash: str, words: list[Word]) -> None:
    path = cache_path(cache_dir, align_cache_key(audio_hash))
    write_atomic(path, encode_envelope(words, TIMEBASE_TRIMMED))


def _truncation_path(audio_hash: str) -> str:
    return f"{audio_hash}.ttsmeta"


def _load_cached_truncation(cache_dir: str, audio_hash: str) -> bool:
    path = cache_path(cache_dir, _truncation_path(audio_hash))
    if not path.is_file():
        return False
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError:
        return False
    try:
        payload = json.loads(raw)
    except ValueError:
        return False
    return bool(payload.get("is_truncated")) if isinstance(payload, dict) else False


def _store_cached_truncation(cache_dir: str, audio_hash: str, is_truncated: bool) -> None:
    path = cache_path(cache_dir, _truncation_path(audio_hash))
    if not is_truncated:
        with suppress(OSError):
            path.unlink()
        return
    write_atomic(path, json.dumps({"is_truncated": True}).encode("utf-8"))


def _synthesize_one(
    segment: SegmentInput,
    backend: TTSBackend,
    model_id: str,
    voice_id: str,
    params: dict[str, object] | None,
    cache_dir: str,
    language: str | None = None,
) -> SegmentResult:
    canonical_params = dict(params or {})
    if backend.requires_language_param and "language" not in canonical_params:
        canonical_params["language"] = language or "en"
    key_params = canonical_params if canonical_params else None
    key = wav_cache_key(segment.text, model_id, voice_id, key_params)
    target = cache_path(cache_dir, key)
    cached = _read_cached_wav(target)
    if cached is not None:
        data, cached_duration_ms = cached
        cached_words, cached_timebase = _load_cached_words(cache_dir, key)
        cached_truncated = _load_cached_truncation(cache_dir, key)
        flags = [TRUNCATED_FLAG] if cached_truncated else []
        return SegmentResult(
            idx=segment.idx,
            audio_hash=key,
            wav_bytes=len(data),
            duration_ms=cached_duration_ms,
            cache_hit=True,
            words=cached_words,
            flags=flags,
            words_timebase=cached_timebase,
        )

    last_error: Exception | None = None
    for _ in range(MAX_ATTEMPTS):
        try:
            if language is not None or backend.requires_language_param:
                try:
                    synth = backend.synthesize(segment.text, voice_id, params, language=language)
                except TypeError:
                    synth = backend.synthesize(segment.text, voice_id, params)
            else:
                synth = backend.synthesize(segment.text, voice_id, params)
        except Exception as exc:
            if is_oom(exc):
                raise
            last_error = exc
            continue
        processed = postprocess(synth.samples, synth.sample_rate, synth.words)
        data = encode_wav(processed.samples)
        write_atomic(target, data)
        _store_words(cache_dir, key, processed.words)
        _store_cached_truncation(cache_dir, key, synth.is_truncated)
        flags = [TRUNCATED_FLAG] if synth.is_truncated else []
        return SegmentResult(
            idx=segment.idx,
            audio_hash=key,
            wav_bytes=len(data),
            duration_ms=duration_ms(processed.samples),
            cache_hit=False,
            words=processed.words,
            flags=flags,
            words_timebase=TIMEBASE_TRIMMED,
        )

    message = str(last_error) if last_error else "synthesis failed"
    return SegmentResult(
        idx=segment.idx,
        audio_hash=None,
        wav_bytes=0,
        duration_ms=0,
        cache_hit=False,
        flags=[FAILED_FLAG, f"error:{message}"],
    )


def synthesize_segments(
    segments: list[SegmentInput],
    backend: TTSBackend,
    model_id: str,
    voice_id: str,
    params: dict[str, object] | None,
    cache_dir: str,
    workers: int = 1,
    on_result: Callable[[SegmentResult], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
    language: str | None = None,
) -> list[SegmentResult]:
    results: list[SegmentResult] = []

    def run(segment: SegmentInput) -> SegmentResult:
        if is_cancelled is not None and is_cancelled():
            raise IngestCancelled()
        return _synthesize_one(
            segment, backend, model_id, voice_id, params, cache_dir, language=language
        )

    if workers <= 1:
        for segment in segments:
            result = run(segment)
            results.append(result)
            if on_result is not None:
                on_result(result)
        return results

    with ThreadPoolExecutor(
        max_workers=workers,
        initializer=job_id_var.set,
        initargs=(job_id_var.get(),),
    ) as pool:
        futures = [pool.submit(run, segment) for segment in segments]
        try:
            for future in as_completed(futures):
                result = future.result()
                results.append(result)
                if on_result is not None:
                    on_result(result)
        except BaseException:
            for future in futures:
                future.cancel()
            raise
    results.sort(key=lambda result: result.idx)
    return results
