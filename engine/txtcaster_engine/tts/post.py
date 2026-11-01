import io
import wave
from collections.abc import Sequence
from dataclasses import dataclass, field

import numpy as np
from numpy.typing import NDArray

from txtcaster_engine.tts.backend import Word

TARGET_SAMPLE_RATE = 24_000
SILENCE_THRESHOLD = 0.001
PEAK_CEILING = 0.98


@dataclass(frozen=True)
class Postprocessed:
    samples: NDArray[np.float32]
    words: list[Word] = field(default_factory=list)


def voiced_span(samples: NDArray[np.float32]) -> tuple[int, int]:
    voiced = np.flatnonzero(np.abs(samples) > SILENCE_THRESHOLD)
    if voiced.size == 0:
        return 0, 0
    return int(voiced[0]), int(voiced[-1]) + 1


def trim_silence(samples: NDArray[np.float32]) -> NDArray[np.float32]:
    start, stop = voiced_span(samples)
    return samples[start:stop]


def resample(samples: NDArray[np.float32], source_rate: int) -> NDArray[np.float32]:
    """Linear interpolation to `TARGET_SAMPLE_RATE`. Measured, and improvable.

    This branch was dead until Piper: kokoro and chatterbox both emit 24 000, so
    `source_rate == TARGET_SAMPLE_RATE` always returned early. Piper's 22 050 is the first
    rate to reach the interpolation, and getting the length arithmetic wrong is audible --
    22 050 samples labelled 24 000 play 8.84% fast (24000/22050 = 1.088435).

    Measured quality of the linear path at 22 050 -> 24 000: passband droop -0.06 dB at
    1 kHz falling to -7.82 dB at 11 kHz, SNR 26.8 dB in band and 7.2 dB on sibilance.
    Judged acceptable for spoken-word audio, where the top octave carries little and the
    output is an MP3 audiobook, but it is not transparent. `scipy.signal.resample_poly(
    samples, 160, 147)` is the exact rational conversion for this pair and measures about
    +10 dB better; scipy is already in the dependency closure. Left as an available
    improvement rather than an unknown, because the drop lands on sibilance and a future
    backend at a lower rate would suffer more.
    """
    if source_rate == TARGET_SAMPLE_RATE or samples.size == 0:
        return samples
    target_len = max(1, round(samples.size * TARGET_SAMPLE_RATE / source_rate))
    positions = np.linspace(0.0, samples.size - 1, target_len)
    return np.interp(positions, np.arange(samples.size), samples).astype(np.float32)


def peak_safe_gain(samples: NDArray[np.float32]) -> NDArray[np.float32]:
    if samples.size == 0:
        return samples
    peak = float(np.max(np.abs(samples)))
    if peak <= PEAK_CEILING:
        return samples
    return (samples * (PEAK_CEILING / peak)).astype(np.float32)


def shift_words(words: Sequence[Word], lead_trim_ms: float, span_ms: int) -> list[Word]:
    shifted: list[Word] = []
    for word in words:
        start = min(max(round(word.s - lead_trim_ms), 0), span_ms)
        end = min(max(round(word.e - lead_trim_ms), start), span_ms)
        shifted.append(Word(w=word.w, s=start, e=end))
    return shifted


def postprocess(
    samples: NDArray[np.float32], source_rate: int, words: Sequence[Word] = ()
) -> Postprocessed:
    mono = samples.astype(np.float32)
    if mono.ndim > 1:
        mono = mono.mean(axis=tuple(range(1, mono.ndim))).astype(np.float32)
    resampled = resample(mono, source_rate)
    start, stop = voiced_span(resampled)
    trimmed = peak_safe_gain(resampled[start:stop])
    lead_trim_ms = start * 1000 / TARGET_SAMPLE_RATE
    return Postprocessed(
        samples=trimmed,
        words=shift_words(words, lead_trim_ms, duration_ms(trimmed)),
    )


def encode_wav(samples: NDArray[np.float32]) -> bytes:
    clipped = np.clip(samples, -1.0, 1.0)
    pcm = (clipped * 32767.0).astype(np.int16)
    buffer = io.BytesIO()
    with wave.open(buffer, "wb") as writer:
        writer.setnchannels(1)
        writer.setsampwidth(2)
        writer.setframerate(TARGET_SAMPLE_RATE)
        writer.writeframes(pcm.tobytes())
    return buffer.getvalue()


def duration_ms(samples: NDArray[np.float32]) -> int:
    return round(samples.size * 1000 / TARGET_SAMPLE_RATE)
