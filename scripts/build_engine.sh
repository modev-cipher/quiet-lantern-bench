#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../engine"

# ENGINE_VARIANT selects what the onedir carries: `cpu` (default, the universal baseline),
# `cuda13` (NVIDIA, driver >= 580, compute capability >= 7.5), or `lite` (no torch: document
# reading and cloud scripting for users whose narration and tutor run on their own providers).
variant="${ENGINE_VARIANT:-cpu}"
case "$variant" in
  cpu)
    groups=(--no-group gpu --group cpu)
    distpath="dist"
    ;;
  cuda13)
    groups=(--no-group cpu --group gpu)
    distpath="dist-cuda13"
    ;;
  lite)
    distpath="dist-lite"
    ;;
  *)
    echo "unknown ENGINE_VARIANT '$variant' (cpu | cuda13 | lite)" >&2
    exit 2
    ;;
esac

# Package from a dedicated venv per variant: syncing the dev venv here would
# reinstall torch wheels and replace a developer's stack (TECH_STACK.md §5).
export UV_PROJECT_ENVIRONMENT="${UV_PROJECT_ENVIRONMENT:-.venv-build-$variant}"
if [ "$variant" = lite ]; then
  constraints="$(mktemp)"
  trap 'rm -f "$constraints"' EXIT
  uv export --frozen --no-default-groups --group cpu --group build --no-emit-project --no-hashes \
    --no-annotate -o "$constraints" >/dev/null
  sed -i -E '/ @ (https?|git)/d; /^--/d' "$constraints"
  uv venv --python 3.11 --allow-existing "$UV_PROJECT_ENVIRONMENT"
  venv_python="$UV_PROJECT_ENVIRONMENT/bin/python"
  [ -x "$venv_python" ] || venv_python="$UV_PROJECT_ENVIRONMENT/Scripts/python.exe"
  uv pip install --python "$venv_python" --constraint "$constraints" \
    fastapi uvicorn numpy pillow pypdfium2 pysbd selectolax pyinstaller
  "$venv_python" -m PyInstaller --noconfirm --clean --distpath "$distpath" txtcaster-engine-lite.spec
else
  uv sync "${groups[@]}" --group build
  uv run --no-sync --group build pyinstaller --noconfirm --clean --distpath "$distpath" txtcaster-engine.spec
fi
echo "onedir build at engine/$distpath/txtcaster-engine/ (variant: $variant, venv: $UV_PROJECT_ENVIRONMENT)"
# Bundled assets must land in the onedir tree (PyInstaller 6 puts datas under _internal/).
font="$distpath/txtcaster-engine/_internal/txtcaster_engine/assets/fonts/NotoNaskhArabic-Regular.ttf"
test -s "$font" || { echo "missing bundled font: $font" >&2; exit 1; }
echo "bundled font present: $font"
