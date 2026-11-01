import contextlib
import hashlib
import json
import os
import tempfile
from pathlib import Path


def wav_cache_key(text: str, model_id: str, voice_id: str, params: dict[str, object] | None) -> str:
    canonical = json.dumps(params or {}, sort_keys=True, separators=(",", ":"))
    raw = f"{text}:{model_id}:{voice_id}:{canonical}"
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def cache_path(cache_dir: str, key: str) -> Path:
    return Path(cache_dir) / key[:2] / key


def write_atomic(target: Path, data: bytes) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(dir=target.parent, prefix=".tmp-")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
        os.replace(tmp_name, target)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp_name)
        raise
