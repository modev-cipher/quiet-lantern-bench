#!/usr/bin/env bash
# Builds Linux release pieces on the supported glibc baseline without touching this host's stack.
#   build_linux_container.sh app              -> deb, rpm, AppImage
#   build_linux_container.sh engine VARIANT   -> engine onedir (lite | cpu | cuda13)
#   build_linux_container.sh signer           -> update_sign
# RELEASE_BUILD_ROOT (default ~/.cache/txtcaster-release-build) holds target dirs and venvs.
# TXTCASTER_TOOLCHAIN_DIR (default ~/.cache/txtcaster-toolchains) holds the pinned GPU toolchains.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
root="${RELEASE_BUILD_ROOT:-$HOME/.cache/txtcaster-release-build}"
image="txtcaster-linux-build:22.04-$(cat "$repo"/scripts/release/{linux.Containerfile,build_libpulse.sh} | sha256sum | cut -c1-12)"
toolchains="${TXTCASTER_TOOLCHAIN_DIR:-$HOME/.cache/txtcaster-toolchains}"
mkdir -p "$root/home" "$toolchains"
podman image exists "$image" || podman build -t "$image" -f "$repo/scripts/release/linux.Containerfile" "$repo/scripts/release"
exec podman run --rm --userns=keep-id --security-opt label=disable \
  -v "$repo:/src" -v "$root:/build" -v "$toolchains:/toolchains" -w /src \
  -e HOME=/build/home -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" -e CARGO_HOME=/build/cargo -e CARGO_TARGET_DIR=/build/target -e CARGO_INCREMENTAL=0 \
  -e SQLX_OFFLINE=true -e CMAKE_ARGS=-DGGML_NATIVE=off -e APPIMAGE_EXTRACT_AND_RUN=1 \
  -e UV_CACHE_DIR=/build/uv-cache -e UV_PYTHON_INSTALL_DIR=/build/uv-python -e TXTCASTER_TOOLCHAIN_DIR=/toolchains \
  -e TXTCASTER_UPDATE_PUBKEY="${TXTCASTER_UPDATE_PUBKEY:-}" -e TAURI_BUILD_ARGS="${TAURI_BUILD_ARGS:-}" \
  "$image" /src/scripts/release/build_linux_inner.sh "$@"
