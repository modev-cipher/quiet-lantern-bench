# Provenance (AGENTS.md §5a): Resemble AI Chatterbox cloned-reference contract.
# Source: https://github.com/resemble-ai/chatterbox @ 5de7a54 (src/chatterbox/tts.py
#   prepare_conditionals / generate(audio_prompt_path=...), models/s3gen/const.py)
# Package: https://pypi.org/project/chatterbox-tts/0.1.7/ (MIT; the wheel's tts.py is
#   byte-identical to 5de7a54)
# Checked: 2026-08-08.
# Upstream facts this module's validation is derived from:
#   - the reference recording is passed to the model as a filesystem path and read
#     with librosa.load, so any format libsndfile decodes is admissible — this module
#     must not be stricter than the decoder that will actually read the file.
#   - conditioning uses at most ENC_COND_LEN = 6 s (speaker tokens) and
#     DEC_COND_LEN = 10 s (generator prompt), which is why a reference shorter than
#     the 3 s floor below is refused rather than silently under-conditioning.

import hashlib
from pathlib import Path

from txtcaster_engine.ingest.errors import IngestError

PRESET_KIND = "preset"
CLONED_KIND = "cloned"

MIN_REFERENCE_SECONDS = 3.0
READ_CHUNK_BYTES = 1 << 20
REFERENCE_PARAM_KEYS = ("reference_wav_path", "ref_sha256", "latents_path")

ResolvedVoice = tuple[str, dict[str, object] | None]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(READ_CHUNK_BYTES):
            digest.update(chunk)
    return digest.hexdigest()


def _reference_seconds(path: Path) -> float:
    import soundfile

    info = soundfile.info(str(path))
    if info.samplerate <= 0:
        raise soundfile.LibsndfileError(0, "reference recording declares a zero sample rate")
    return float(info.frames) / float(info.samplerate)


def _cloned_reference(profile: dict[str, object], params: dict[str, object]) -> dict[str, object]:
    raw = profile.get("reference_wav_path")
    reference = str(raw) if raw else ""
    if not reference:
        raise IngestError(
            "ERR_VOICE_REFERENCE_MISSING",
            "This voice has no recording saved. Record or import a sample again.",
        )
    path = Path(reference)
    if not path.is_file():
        raise IngestError(
            "ERR_VOICE_REFERENCE_MISSING",
            "The recording for this voice is no longer on disk. Record or import a sample again.",
        )
    try:
        seconds = _reference_seconds(path)
        digest = sha256_file(path)
    except OSError as exc:
        raise IngestError(
            "ERR_VOICE_REFERENCE_MISSING",
            "The recording for this voice could not be opened. Record or import a sample again.",
        ) from exc
    except Exception as exc:
        raise IngestError(
            "ERR_VOICE_REFERENCE_UNREADABLE",
            "This voice's recording is not audio we can read. "
            "Import a WAV, FLAC or OGG file, or record a new sample.",
        ) from exc
    if seconds < MIN_REFERENCE_SECONDS:
        raise IngestError(
            "ERR_VOICE_REFERENCE_SHORT",
            f"The recording for this voice is only {seconds:.1f} seconds long. "
            f"Cloning needs at least {MIN_REFERENCE_SECONDS:.0f} seconds of speech.",
        )
    expected = params.get("ref_sha256")
    if isinstance(expected, str) and expected and expected != digest:
        raise IngestError(
            "ERR_VOICE_REFERENCE_CHANGED",
            "The recording for this voice has changed since it was saved. "
            "Preview it again to confirm it still sounds right.",
        )
    resolved = dict(params)
    resolved["reference_wav_path"] = str(path)
    resolved["ref_sha256"] = digest
    latents_raw = params.get("latents_path")
    if latents_raw and Path(str(latents_raw)).is_file():
        resolved["latents_path"] = str(latents_raw)
    else:
        candidate = path.parent / "latents.safetensors"
        if candidate.is_file():
            resolved["latents_path"] = str(candidate)
    return resolved


def resolve_voice(voice_profile: dict[str, object]) -> ResolvedVoice:
    kind_raw = voice_profile.get("kind")
    kind = str(kind_raw) if kind_raw else PRESET_KIND
    params_raw = voice_profile.get("params")
    params: dict[str, object] | None = dict(params_raw) if isinstance(params_raw, dict) else None

    if kind != CLONED_KIND:
        preset = voice_profile.get("preset_voice_id")
        voice_id = str(preset) if preset else ""
        if not voice_id:
            raise IngestError("ERR_VOICE", "voice profile has no preset_voice_id")
        if params is not None:
            for key in REFERENCE_PARAM_KEYS:
                params.pop(key, None)
        return voice_id, params

    profile_id = voice_profile.get("id")
    voice_id = str(profile_id) if profile_id else ""
    if not voice_id:
        raise IngestError("ERR_VOICE", "cloned voice profile has no id")
    cloned = _cloned_reference(voice_profile, params or {})
    seed = voice_profile.get("seed")
    if isinstance(seed, int) and not isinstance(seed, bool):
        cloned["seed"] = seed & 0xFFFFFFFF
    return voice_id, cloned
