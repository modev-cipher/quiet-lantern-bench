import hashlib
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path

from txtcaster_engine.align.backend import AlignBackend
from txtcaster_engine.align.version import ALIGN_ALGO_VERSION, align_base_version
from txtcaster_engine.ingest.errors import IngestCancelled, IngestError
from txtcaster_engine.oom import is_device_unavailable, is_oom
from txtcaster_engine.tts.backend import Word
from txtcaster_engine.tts.cache import cache_path, write_atomic
from txtcaster_engine.tts.words import (
    TIMEBASE_TRIMMED,
    TIMEBASE_UNKNOWN,
    decode_envelope,
    encode_envelope,
)

MAX_ATTEMPTS = 2
DEFAULT_ALIGN_TIMEOUT_S = 60.0
FAILED_FLAG = "align_failed"
EMPTY_FLAG = "align_empty"


@dataclass
class AlignInput:
    idx: int
    text: str
    audio_path: str
    audio_hash: str


@dataclass
class AlignResult:
    idx: int
    words: list[Word] = field(default_factory=list)
    cache_hit: bool = False
    flags: list[str] = field(default_factory=list)
    words_timebase: str = TIMEBASE_UNKNOWN

    def to_json(self) -> dict[str, object]:
        return {
            "idx": self.idx,
            "words": [word.to_json() for word in self.words],
            "cache_hit": self.cache_hit,
            "flags": list(self.flags),
            "words_timebase": self.words_timebase,
        }


def align_cache_key(
    audio_hash: str,
    model_id: str,
    version: int = ALIGN_ALGO_VERSION,
) -> str:
    raw = f"{audio_hash}:{model_id}:{align_base_version(version)}"
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def _load_cached(
    cache_dir: str,
    audio_hash: str,
    model_id: str,
    version: int = ALIGN_ALGO_VERSION,
) -> list[Word] | None:
    path = cache_path(cache_dir, align_cache_key(audio_hash, model_id, version=version))
    if not path.is_file():
        return None
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError:
        return None
    envelope = decode_envelope(raw)
    if envelope is None:
        return None
    entries = envelope.entries
    words: list[Word] = []
    for entry in entries:
        if not (isinstance(entry, dict) and {"w", "s", "e"} <= entry.keys()):
            return None
        try:
            word = Word(w=str(entry["w"]), s=int(entry["s"]), e=int(entry["e"]))
        except (TypeError, ValueError):
            return None
        if word.s < 0 or word.e < word.s:
            return None
        words.append(word)
    if not words:
        return None
    return words


def _store(
    cache_dir: str,
    audio_hash: str,
    model_id: str,
    words: list[Word],
    version: int = ALIGN_ALGO_VERSION,
) -> None:
    path = cache_path(cache_dir, align_cache_key(audio_hash, model_id, version=version))
    write_atomic(path, encode_envelope(words, TIMEBASE_TRIMMED))


def _align_one(
    segment: AlignInput,
    backend: AlignBackend,
    model_id: str,
    language: str | None,
    cache_dir: str,
    timeout_s: float = DEFAULT_ALIGN_TIMEOUT_S,
) -> AlignResult:
    cached = _load_cached(cache_dir, segment.audio_hash, model_id)
    if cached is not None:
        return AlignResult(
            idx=segment.idx,
            words=cached,
            cache_hit=True,
            words_timebase=TIMEBASE_TRIMMED,
        )

    if not Path(segment.audio_path).is_file():
        return AlignResult(
            idx=segment.idx,
            flags=[FAILED_FLAG, "error:audio file missing"],
        )

    last_error: Exception | None = None
    got_empty = False
    for _ in range(MAX_ATTEMPTS):
        executor = ThreadPoolExecutor(max_workers=1)
        try:
            future = executor.submit(backend.align, segment.audio_path, segment.text, language)
            try:
                words = future.result(timeout=timeout_s)
            except TimeoutError as exc:
                executor.shutdown(wait=False, cancel_futures=True)
                raise IngestError("ERR_DEVICE_UNAVAILABLE", "alignment timed out") from exc
        except IngestError:
            raise
        except Exception as exc:
            if is_oom(exc) or is_device_unavailable(exc):
                raise
            last_error = exc
            continue
        finally:
            executor.shutdown(wait=False)
        if not words and segment.text.strip():
            got_empty = True
            continue
        _store(cache_dir, segment.audio_hash, model_id, words)
        return AlignResult(idx=segment.idx, words=words, words_timebase=TIMEBASE_TRIMMED)

    if got_empty:
        return AlignResult(idx=segment.idx, flags=[EMPTY_FLAG])
    message = str(last_error) if last_error else "alignment failed"
    return AlignResult(idx=segment.idx, flags=[FAILED_FLAG, f"error:{message}"])


def align_segments(
    segments: list[AlignInput],
    backend: AlignBackend,
    model_id: str,
    language: str | None,
    cache_dir: str,
    workers: int = 1,
    on_result: Callable[[AlignResult], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
    timeout_s: float = DEFAULT_ALIGN_TIMEOUT_S,
) -> list[AlignResult]:
    results: list[AlignResult] = []

    def run(segment: AlignInput) -> AlignResult:
        if is_cancelled is not None and is_cancelled():
            raise IngestCancelled()
        return _align_one(segment, backend, model_id, language, cache_dir, timeout_s=timeout_s)

    if workers <= 1:
        for segment in segments:
            result = run(segment)
            results.append(result)
            if on_result is not None:
                on_result(result)
        return results

    with ThreadPoolExecutor(max_workers=workers) as pool:
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
