#!/usr/bin/env bash
# Builds, smoke-tests and packs the Windows engine variants. Run from the repository root
# inside an MSVC x64 developer environment (vcvarsall.bat x64): llama.cpp's Vulkan and CUDA
# backends compile with Ninja, cl.exe and nvcc, as llama.cpp's own Windows release does.
#   build_windows_engines.sh OUT_DIR SIGNER [GGUF]
# With GGUF, each engine with local AI also generates with that model (on the CPU when the
# machine has no usable GPU).
set -euo pipefail
out="${1:?output directory required}"
signer="${2:?update_sign path required}"
gguf="${3:-}"
mkdir -p "$out"
version="$(sed -n 's/^__version__ = "\(.*\)"/\1/p' engine/txtcaster_engine/__init__.py)"
for variant in cpu lite cuda13; do
  dist=engine/dist-$variant; [ "$variant" = cpu ] && dist=engine/dist
  flags=()
  case "$variant" in
    lite) flags=(--reader-only) ;;
    cpu) flags=(--expect-backend vulkan) ;;
    cuda13) flags=(--expect-backend cuda) ;;
  esac
  if [ -n "$gguf" ] && [ "$variant" != lite ]; then flags+=(--gguf "$gguf"); fi
  ENGINE_VARIANT=$variant ./scripts/build_engine.sh
  uv run --no-project python scripts/smoke_test.py "$dist/txtcaster-engine/txtcaster-engine.exe" "${flags[@]}"
  base="$out/txtcaster-engine-$version-windows-x86_64-$variant"
  "$signer" engine-pack --dir "$dist/txtcaster-engine" --out-prefix "$base"
  {
    sha256sum "$base".tar.zst.* | sed "s#$out/##; s/ \*/  /"
    echo "whole  $(cat "$base".tar.zst.* | sha256sum | cut -d' ' -f1)"
    stat -c '%n %s' "$base".tar.zst.* | sed "s#$out/##"
  } > "$base.hashes.txt"
  rm -rf "$dist" engine/build "engine/.venv-build-$variant" "engine/.venv-build-$variant-tools"
done
