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
    llama_backend=vulkan
    llama_cmake="-DGGML_BACKEND_DL=ON -DGGML_NATIVE=OFF -DGGML_VULKAN=ON"
    ;;
  cuda13)
    groups=(--no-group cpu --group gpu)
    distpath="dist-cuda13"
    llama_backend=cuda
    llama_cmake="-DGGML_BACKEND_DL=ON -DGGML_NATIVE=OFF -DGGML_CUDA=ON -DGGML_CUDA_NCCL=OFF"
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
  # llama.cpp loads its GPU backend as a module (GGML_BACKEND_DL), so a machine without the
  # Vulkan loader or the NVIDIA driver skips that backend instead of failing to start. The
  # toolchains are the pinned ones in scripts/release/gpu-toolchain.json on every OS.
  toolchains="${TXTCASTER_TOOLCHAIN_DIR:-$HOME/.cache/txtcaster-toolchains}"
  release="$(cd ../scripts/release && pwd)"
  toolchain_env="$(uv run --no-project --python 3.11 python -I "$release/fetch_gpu_toolchain.py" "$llama_backend" "$toolchains")"
  eval "$toolchain_env"
  tools="$UV_PROJECT_ENVIRONMENT-tools"
  uv venv --python 3.11 --allow-existing "$tools" >/dev/null
  tools="$(cd "$tools" && pwd)"
  tools_bin="$tools/bin"; [ -d "$tools_bin" ] || tools_bin="$tools/Scripts"
  tools_python="$tools_bin/python.exe"; [ -f "$tools_python" ] || tools_python="$tools_bin/python"
  uv pip install --python "$tools_python" --require-hashes -r "$release/build-tools.txt"
  export PATH="$tools_bin:$PATH" CMAKE_GENERATOR=Ninja CMAKE_ARGS="$llama_cmake"
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
      command -v cl.exe >/dev/null || { echo "run from an MSVC x64 developer environment (vcvarsall.bat x64)" >&2; exit 1; } ;;
  esac
  # uv reuses a wheel it built earlier whatever CMAKE_ARGS says, and --refresh-package does
  # not invalidate it: a CUDA build after a Vulkan one would get the Vulkan wheel.
  uv cache clean llama-cpp-python
  uv sync "${groups[@]}" --group build --reinstall-package llama-cpp-python
  find "$UV_PROJECT_ENVIRONMENT" -path "*llama_cpp/lib/*ggml-$llama_backend.*" | grep -q . \
    || { echo "llama-cpp-python was built without its $llama_backend backend" >&2; exit 1; }
  if [ "$(uname -s)" = Linux ]; then
    # llama-cpp-python adds the toolkit's link directories to every library's RUNPATH;
    # the shipped libraries resolve only beside themselves and from the system.
    find "$UV_PROJECT_ENVIRONMENT" -path '*llama_cpp/lib/*.so*' -type f -exec patchelf --set-rpath '$ORIGIN' {} +
  fi
  if [ "$variant" = cuda13 ] && [ "$(uname -s)" = Linux ]; then
    # torch requires libnvshmem_host.so.3 (via libtorch_nvshmem.so). It ships from NVIDIA's
    # Apache-2.0 3.8.0 redist archive, not the 3.4.5 wheel torch pins; the fetch refuses any
    # other licence. The bootstrap and transport plugins serve multi-node jobs and stay out.
    nvshmem_env="$(uv run --no-project --python 3.11 python -I "$release/fetch_gpu_toolchain.py" nvshmem "$toolchains")"
    eval "$nvshmem_env"
    nvshmem_lib="$(find "$UV_PROJECT_ENVIRONMENT" -path '*nvidia/nvshmem/lib' -type d)"
    [ -d "$nvshmem_lib" ] || { echo "torch's NVSHMEM directory is missing" >&2; exit 1; }
    rm -f "$nvshmem_lib"/*
    install -m 0755 "$NVSHMEM_HOST_LIB" "$nvshmem_lib/libnvshmem_host.so.3"
    install -m 0644 "$NVSHMEM_LICENSE" "$tools/NVSHMEM-3.8.0-LICENSE.txt"
    export NVSHMEM_LICENSE="$tools/NVSHMEM-3.8.0-LICENSE.txt"
  fi
  uv run --no-sync --group build pyinstaller --noconfirm --clean --distpath "$distpath" txtcaster-engine.spec
  bundle="$distpath/txtcaster-engine/_internal"
  find "$bundle/llama_cpp/lib" -name "*ggml-$llama_backend.*" | grep -q . \
    || { echo "the onedir lacks the $llama_backend backend module" >&2; exit 1; }
  if [ "$(uname -s)" = Linux ]; then
    leaked="$(find "$bundle/llama_cpp/lib" -name '*.so*' -type f -exec readelf -d {} + | grep -E 'R(UN)?PATH' | grep -v '\[\$ORIGIN\]' || true)"
    [ -z "$leaked" ] || { echo "a llama.cpp library searches a build-host path: $leaked" >&2; exit 1; }
  fi
  if [ "$variant" = cuda13 ] && [ "$(uname -s)" = Linux ]; then
    shipped="$(find "$bundle" -name 'libnvshmem_host.so*' -type f)"
    [ "$(sha256sum < "$shipped")" = "$(sha256sum < "$NVSHMEM_HOST_LIB")" ] \
      || { echo "the onedir's libnvshmem_host is not the pinned Apache-2.0 build: $shipped" >&2; exit 1; }
    ! find "$bundle" -name 'nvshmem_*.so*' | grep -q . || { echo "NVSHMEM plugins reached the onedir" >&2; exit 1; }
    test -s "$bundle/licenses/nvidia/NVSHMEM-3.8.0-LICENSE.txt" || { echo "the NVSHMEM licence is missing" >&2; exit 1; }
    echo "NVSHMEM: $(sha256sum < "$shipped" | cut -c1-16) from the 3.8.0 redist archive"
  fi
  if [ "$variant" = cuda13 ]; then
    find "$bundle" \( -name 'libnvrtc-builtins.so.*' -o -iname 'nvrtc-builtins64_*.dll' \) | grep -q . \
      || { echo "the onedir lacks NVRTC's builtins library" >&2; exit 1; }
    ! find "$bundle" \( -name 'libtriton.so' -o -path '*/triton/*' \) | grep -q . \
      || { echo "triton reached the onedir" >&2; exit 1; }
  fi
  driver="$(find "$bundle" \( -iname 'libvulkan.so*' -o -iname 'vulkan-1.dll' -o -iname 'libcuda.so*' -o -iname 'nvcuda.dll' \) | head -n 1)"
  [ -z "$driver" ] || { echo "the onedir must not carry a GPU driver library: $driver" >&2; exit 1; }
fi
echo "onedir build at engine/$distpath/txtcaster-engine/ (variant: $variant, venv: $UV_PROJECT_ENVIRONMENT)"
# Bundled assets must land in the onedir tree (PyInstaller 6 puts datas under _internal/).
font="$distpath/txtcaster-engine/_internal/txtcaster_engine/assets/fonts/NotoNaskhArabic-Regular.ttf"
test -s "$font" || { echo "missing bundled font: $font" >&2; exit 1; }
echo "bundled font present: $font"
