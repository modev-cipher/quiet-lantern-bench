#!/usr/bin/env bash
# Copies build_libpulse.sh's client library into DEST, where it loads its common library from
# beside itself: the AppImage's usr/lib, or the deb/rpm's /usr/lib/txtCaster/pulse.
#   stage_libpulse.sh PREFIX DEST
set -euo pipefail
prefix="${1:?build_libpulse.sh destination required}"
dest="${2:?destination directory required}"
mkdir -p "$dest"
rm -f "$dest"/libpulse.so* "$dest"/libpulsecommon-*.so
cp -L "$prefix/usr/lib/libpulse.so.0" "$dest/libpulse.so.0"
cp "$prefix"/usr/lib/pulseaudio/libpulsecommon-*.so "$dest/"
for lib in "$dest"/libpulse.so.0 "$dest"/libpulsecommon-*.so; do
  patchelf --set-rpath '$ORIGIN' "$lib"
done
test -e "$dest/libpulsecommon-17.0.so"
