"""Content-addressed reading cache keyed on (page image sha256, provider id, model id, prompt
sha256) so a page is never billed twice. The key is a digest, so no key material or page
content can leak through it. In-memory by default; `directory` persists JSON files."""

from __future__ import annotations

import contextlib
import hashlib
import json
import logging
import os
import tempfile
from pathlib import Path

from txtcaster_engine.ingest.fallback.envelope import Reading

_LOG = logging.getLogger(__name__)


def cache_key(image_sha256: str, provider_id: str, model_id: str, prompt_sha256: str) -> str:
    joined = "\n".join((image_sha256, provider_id, model_id, prompt_sha256))
    return hashlib.sha256(joined.encode()).hexdigest()


def _write_json_atomic(target: Path, text: str) -> None:
    fd, tmp_name = tempfile.mkstemp(dir=target.parent, prefix=".tmp-")
    try:
        with os.fdopen(fd, "w") as handle:
            handle.write(text)
        os.replace(tmp_name, target)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp_name)
        raise


class ReadingCache:
    def __init__(self, directory: Path | None = None) -> None:
        self._memory: dict[str, Reading] = {}
        self._directory = directory
        if directory is not None:
            directory.mkdir(parents=True, exist_ok=True)
            try:
                directory.chmod(0o700)
            except OSError as exc:
                _LOG.warning("reading cache directory could not be made private: %s", exc)

    def keys(self) -> list[str]:
        return list(self._memory)

    def get(self, key: str) -> Reading | None:
        hit = self._memory.get(key)
        if hit is not None or self._directory is None:
            return hit
        path = self._directory / f"{key}.json"
        if not path.is_file():
            return None
        try:
            reading = Reading.from_json(json.loads(path.read_text()))
        except (OSError, ValueError):
            _LOG.warning("cached reading %s is unreadable and reads as a miss", key[:12])
            return None
        self._memory[key] = reading
        return reading

    def put(self, key: str, reading: Reading) -> None:
        self._memory[key] = reading
        if self._directory is None:
            return
        try:
            _write_json_atomic(self._directory / f"{key}.json", json.dumps(reading.to_json()))
        except OSError:
            _LOG.warning(
                "cached reading %s could not be persisted; the billed result is kept", key[:12]
            )
