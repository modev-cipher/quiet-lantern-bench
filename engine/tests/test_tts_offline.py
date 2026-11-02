# Conformance fixture for the provider contracts recorded in
# txtcaster_engine/offline.py and txtcaster_engine/selftest.py.
# Hugging Face Hub offline mode:
#   https://huggingface.co/docs/huggingface_hub/main/en/package_reference/environment_variables
# Chatterbox: https://github.com/resemble-ai/chatterbox @ 5de7a54 (MIT)
# Perth watermarker: https://github.com/resemble-ai/Perth (MIT)
# Checked: 2026-08-08.

import io
import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

from txtcaster_engine.offline import OFFLINE_ENV_DEFAULTS, enable_offline_defaults
from txtcaster_engine.selftest import IMPORT_PROBES, check_imports

PROBE = textwrap.dedent(
    """
    import socket
    import sys


    class Blocked(socket.socket):
        def connect(self, *args, **kwargs):
            raise AssertionError("chatterbox opened a network connection at import time")

        def connect_ex(self, *args, **kwargs):
            raise AssertionError("chatterbox opened a network connection at import time")


    def refuse(*args, **kwargs):
        raise AssertionError("chatterbox opened a network connection at import time")


    socket.socket = Blocked
    socket.create_connection = refuse

    import txtcaster_engine  # noqa: F401
    import chatterbox.tts
    from chatterbox.models.tokenizers import EnTokenizer
    from huggingface_hub import constants

    assert chatterbox.tts.ChatterboxTTS is not None
    assert EnTokenizer is not None
    assert constants.HF_HUB_OFFLINE is True, "the engine did not default huggingface_hub offline"
    print("OFFLINE OK")
    """
)


def networkless_env() -> dict[str, str]:
    env = dict(os.environ)
    for key in OFFLINE_ENV_DEFAULTS:
        env.pop(key, None)
    return env


def test_import_probe_reports_every_module() -> None:
    out = io.StringIO()
    assert check_imports(out) == 0
    text = out.getvalue()
    for module_name, _ in IMPORT_PROBES:
        assert f"import ok {module_name}" in text
    assert "watermarker ok" in text
    assert "IMPORT CHECK PASS" in text


def test_import_probe_rejects_missing_language_metadata_without_downloading(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import spacy

    def refuse(*args: object, **kwargs: object) -> None:
        pytest.fail("the packaged probe attempted a language download")

    monkeypatch.setattr(spacy.util, "is_package", lambda name: False)
    monkeypatch.setattr("spacy.cli.download", refuse)
    out = io.StringIO()
    assert check_imports(out) == 1
    assert "LANGUAGE FAIL" in out.getvalue()


def test_import_probe_fails_when_the_watermarker_weights_are_unreachable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import perth

    def refuse() -> None:
        raise FileNotFoundError("perth_net_250000.pth.tar")

    monkeypatch.setattr(perth, "PerthImplicitWatermarker", refuse)
    out = io.StringIO()
    assert check_imports(out) == 1
    assert "WATERMARK FAIL" in out.getvalue()
    assert "IMPORT CHECK FAIL" in out.getvalue()


def test_import_probe_fails_loudly_on_a_missing_module(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        "txtcaster_engine.selftest.IMPORT_PROBES", (("txtcaster_engine.selftest", "nope"),)
    )
    out = io.StringIO()
    assert check_imports(out) == 1
    assert "IMPORT CHECK FAIL" in out.getvalue()


def test_importing_the_engine_defaults_hugging_face_offline() -> None:
    environ: dict[str, str] = {}
    assert enable_offline_defaults(environ) == dict(OFFLINE_ENV_DEFAULTS)
    assert environ == dict(OFFLINE_ENV_DEFAULTS)


def test_an_explicit_offline_choice_is_not_overridden() -> None:
    environ = {"HF_HUB_OFFLINE": "0"}
    assert enable_offline_defaults(environ) == {"TRANSFORMERS_OFFLINE": "1"}
    assert environ["HF_HUB_OFFLINE"] == "0"


def test_chatterbox_imports_offline_without_being_told_to() -> None:
    pytest.importorskip("chatterbox")
    result = subprocess.run(
        [sys.executable, "-c", PROBE],
        capture_output=True,
        text=True,
        env=networkless_env(),
        timeout=600,
    )
    assert result.returncode == 0, result.stderr
    assert "OFFLINE OK" in result.stdout


def test_check_imports_fails_when_pronunciation_weights_are_missing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from txtcaster_engine.tts.g2p import unknown_words

    monkeypatch.setattr(unknown_words, "WEIGHTS_DIR", tmp_path)
    out = io.StringIO()
    assert check_imports(out) == 1
    assert "pronunciation fallback" in out.getvalue()
