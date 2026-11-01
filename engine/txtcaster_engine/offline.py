# Provenance (AGENTS.md §5a): Hugging Face Hub offline mode.
# Source: https://huggingface.co/docs/huggingface_hub/main/en/package_reference/environment_variables
# Checked: 2026-08-08.
# Upstream facts this module depends on:
#   - HF_HUB_OFFLINE: "If set, no HTTP calls will be made to the Hugging Face Hub.
#     If you try to download files, only the cached files will be accessed."
#   - huggingface_hub reads it once, into huggingface_hub.constants.HF_HUB_OFFLINE, at
#     import time; TRANSFORMERS_OFFLINE is honoured as its alias. Setting either after
#     the first import is a no-op, which is why this runs from the package __init__
#     rather than from a request handler.

import os

OFFLINE_ENV_DEFAULTS = {
    "HF_HUB_OFFLINE": "1",
    "TRANSFORMERS_OFFLINE": "1",
}


def enable_offline_defaults(environ: dict[str, str] | None = None) -> dict[str, str]:
    target = os.environ if environ is None else environ
    applied: dict[str, str] = {}
    for key, value in OFFLINE_ENV_DEFAULTS.items():
        if not target.get(key):
            target[key] = value
            applied[key] = value
    return applied
