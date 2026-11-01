#!/usr/bin/env bash
set -euo pipefail

tag="autobuild-2026-09-30-13-08"
asset="ffmpeg-n8.1.3-9-g29e619e767-linux64-lgpl-shared-8.1.tar.xz"
sha256="d4d7b6936f492c0b866b1cb4a29a1bbae09d6a064b4ec4eb90c95bed7969535a"
ffmpeg_commit="29e619e767cde9045a75c29bc9a8278ae7b3a98b"
gpl3_sha256="8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903"
src_tauri="${1:-apps/desktop/src-tauri}"
dest="$src_tauri/ffmpeg"
libs="$src_tauri/ffmpeg-lib"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl -fsSL --retry 5 --retry-all-errors -o "$work/ffmpeg.tar.xz" \
  "https://github.com/BtbN/FFmpeg-Builds/releases/download/${tag}/${asset}"
echo "${sha256}  $work/ffmpeg.tar.xz" | sha256sum -c -
curl -fsSL --retry 5 --retry-all-errors -o "$work/COPYING.GPLv3" \
  "https://raw.githubusercontent.com/FFmpeg/FFmpeg/${ffmpeg_commit}/COPYING.GPLv3"
echo "${gpl3_sha256}  $work/COPYING.GPLv3" | sha256sum -c -
mkdir "$work/unpacked"
tar -xf "$work/ffmpeg.tar.xz" -C "$work/unpacked" --strip-components=1

rm -rf "$dest" "$libs"
mkdir -p "$dest" "$libs"
cp "$work/unpacked/bin/ffmpeg" "$work/unpacked/bin/ffprobe" "$dest/"
for soname in "$work"/unpacked/lib/*.so.[0-9]*; do
  case "$soname" in
    *.so.*.*) ;;
    *) cp -L "$soname" "$libs/" ;;
  esac
done
cp "$work/unpacked/LICENSE.txt" "$dest/LICENSE.txt"
grep -q "GNU LESSER GENERAL PUBLIC LICENSE" "$dest/LICENSE.txt" \
  || { echo "${asset} licence is not the LGPL text" >&2; exit 1; }
cp "$work/COPYING.GPLv3" "$dest/COPYING.GPLv3.txt"
cat > "$dest/SOURCE.txt" <<SOURCE
FFmpeg 8.1 (LGPL-3.0-or-later), as built by BtbN/FFmpeg-Builds; the AppImage packaging
rewrites only the library search path.
LICENSE.txt is the LGPLv3 text; COPYING.GPLv3.txt is the GPLv3 text it builds on.

Corresponding source:
  FFmpeg:        https://github.com/FFmpeg/FFmpeg/tree/${ffmpeg_commit}
  Build scripts: https://github.com/BtbN/FFmpeg-Builds/tree/${tag}
  Binary:        https://github.com/BtbN/FFmpeg-Builds/releases/tag/${tag}
                 ${asset}
                 sha256 ${sha256}

The shared libraries live in ../lib next to this folder (AppImage: ../.. inside the image).
You may replace ffmpeg, ffprobe and those libraries with your own build of the same FFmpeg
version.
SOURCE

mkdir -p "$work/run/ffmpeg"
cp "$dest/ffmpeg" "$dest/ffprobe" "$work/run/ffmpeg/"
cp -r "$libs" "$work/run/lib"
"$work/run/ffmpeg/ffprobe" -hide_banner -version >/dev/null
banner="$("$work/run/ffmpeg/ffmpeg" -hide_banner -buildconf)"
case "$banner" in *--enable-gpl*|*--enable-nonfree*)
  echo "${asset} is not an LGPL build" >&2; exit 1;;
esac
grep -q libmp3lame <<<"$("$work/run/ffmpeg/ffmpeg" -hide_banner -encoders)" \
  || { echo "${asset} has no libmp3lame encoder" >&2; exit 1; }
grep -q loudnorm <<<"$("$work/run/ffmpeg/ffmpeg" -hide_banner -filters)" \
  || { echo "${asset} has no loudnorm filter" >&2; exit 1; }
du -sh "$dest" "$libs"
