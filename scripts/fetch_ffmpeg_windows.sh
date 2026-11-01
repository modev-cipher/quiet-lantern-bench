#!/usr/bin/env bash
set -euo pipefail

tag="autobuild-2026-09-30-13-08"
asset="ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-shared-8.1.zip"
sha256="3e47bda1607740550141e37c0e49d1e5182b34699f15adfd137ee266d346811a"
ffmpeg_commit="29e619e767cde9045a75c29bc9a8278ae7b3a98b"
gpl3_sha256="8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903"
dest="${1:-apps/desktop/src-tauri/ffmpeg}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl -fsSL --retry 3 -o "$work/ffmpeg.zip" \
  "https://github.com/BtbN/FFmpeg-Builds/releases/download/${tag}/${asset}"
echo "${sha256}  $work/ffmpeg.zip" | sha256sum -c -
curl -fsSL --retry 3 -o "$work/COPYING.GPLv3" \
  "https://raw.githubusercontent.com/FFmpeg/FFmpeg/${ffmpeg_commit}/COPYING.GPLv3"
echo "${gpl3_sha256}  $work/COPYING.GPLv3" | sha256sum -c -
if command -v unzip >/dev/null 2>&1; then
  unzip -q "$work/ffmpeg.zip" -d "$work/unpacked"
else
  7z x -y -o"$work/unpacked" "$work/ffmpeg.zip" >/dev/null
fi

exe="$(find "$work/unpacked" -type f -path '*/bin/ffmpeg.exe' | head -n 1)"
[ -n "$exe" ] || { echo "::error::bin/ffmpeg.exe not found in ${asset}"; exit 1; }
bin="$(dirname "$exe")"
root="$(dirname "$bin")"

rm -rf "$dest"
mkdir -p "$dest"
cp "$exe" "$dest/"
[ -f "$bin/ffprobe.exe" ] || { echo "::error::bin/ffprobe.exe not found in ${asset}"; exit 1; }
cp "$bin/ffprobe.exe" "$dest/"
cp "$bin"/*.dll "$dest/"
licence="$(find "$root" -maxdepth 1 -type f -iname 'LICENSE*' | head -n 1)"
[ -n "$licence" ] || { echo "::error::no licence file in ${asset}"; exit 1; }
cp "$licence" "$dest/LICENSE.txt"
grep -q "GNU LESSER GENERAL PUBLIC LICENSE" "$dest/LICENSE.txt" \
  || { echo "::error::${asset} licence is not the LGPL text"; exit 1; }
cp "$work/COPYING.GPLv3" "$dest/COPYING.GPLv3.txt"
cat > "$dest/SOURCE.txt" <<SOURCE
FFmpeg 8.1 (LGPL-3.0-or-later), unmodified, as built by BtbN/FFmpeg-Builds.
LICENSE.txt is the LGPLv3 text; COPYING.GPLv3.txt is the GPLv3 text it builds on.

Corresponding source:
  FFmpeg:        https://github.com/FFmpeg/FFmpeg/tree/${ffmpeg_commit}
  Build scripts: https://github.com/BtbN/FFmpeg-Builds/tree/${tag}
  Binary:        https://github.com/BtbN/FFmpeg-Builds/releases/tag/${tag}
                 ${asset}
                 sha256 ${sha256}

You may replace ffmpeg.exe, ffprobe.exe and the DLLs in this folder with your own
build of the same FFmpeg version.
SOURCE

"$dest/ffprobe.exe" -hide_banner -version >/dev/null \
  || { echo "::error::${asset} ffprobe.exe does not start"; exit 1; }
banner="$("$dest/ffmpeg.exe" -hide_banner -buildconf)"
case "$banner" in *--enable-gpl*|*--enable-nonfree*)
  echo "::error::${asset} is not an LGPL build"; exit 1;;
esac
grep -q libmp3lame <<<"$("$dest/ffmpeg.exe" -hide_banner -encoders)" \
  || { echo "::error::${asset} has no libmp3lame encoder"; exit 1; }
grep -q loudnorm <<<"$("$dest/ffmpeg.exe" -hide_banner -filters)" \
  || { echo "::error::${asset} has no loudnorm filter"; exit 1; }
ls -la "$dest"
