#!/usr/bin/env bash
set -euo pipefail
if command -v nvidia-smi >/dev/null 2>&1 && gpu_list="$(nvidia-smi -L 2>/dev/null)" && [ -n "$gpu_list" ]; then
  echo "[sync] Detected NVIDIA GPU with CUDA support. Syncing with --group gpu..."
  exec uv sync --no-group cpu --group gpu "$@"
else
  echo "[sync] No NVIDIA GPU detected. Syncing with --group cpu..."
  exec uv sync --no-group gpu --group cpu "$@"
fi
