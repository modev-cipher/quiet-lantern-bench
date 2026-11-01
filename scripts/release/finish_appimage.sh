#!/usr/bin/env bash
# The AppImage must use the host's libwayland: a bundled older copy segfaults inside
# NVIDIA's EGL at startup. linuxdeploy offers no exclusion, so repack without it.
set -euo pipefail
bundle="${1:?appimage bundle directory required}"
tool="${HOME}/.cache/tauri/linuxdeploy-plugin-appimage.AppImage"
appdir="$(ls -d "$bundle"/*.AppDir)"
image="$(ls "$bundle"/*.AppImage)"
rm -f "$appdir"/usr/lib/libwayland-*.so*
rm -f "$image"
cd "$bundle"
ARCH=x86_64 LDAI_OUTPUT="$(basename "$image")" "$tool" --appimage-extract-and-run --appdir "$(basename "$appdir")"
test -s "$image"
