#!/usr/bin/env bash
# libpulse 15.99 to 16.1 keeps a corked stream's clock running before its first play, so GStreamer
# starts that far ahead and playback stalls for as long as it sat paused (fixed in 17.0). The
# AppImage, deb and rpm carry this pinned build (stage_libpulse.sh).
#   build_libpulse.sh DESTDIR   (installs DESTDIR/usr/lib/...; compiled-in paths stay /usr and /etc)
set -euo pipefail
dest="${1:?destination directory required}"
version=17.0
sha256=053794d6671a3e397d849e478a80b82a63cb9d8ca296bd35b73317bb5ceb87b5
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
curl -fsSL --retry 3 -o "$work/src.tar.xz" "https://freedesktop.org/software/pulseaudio/releases/pulseaudio-$version.tar.xz"
echo "$sha256  $work/src.tar.xz" | sha256sum -c -
tar -xJf "$work/src.tar.xz" -C "$work"
meson setup "$work/build" "$work/pulseaudio-$version" --prefix=/usr --sysconfdir=/etc --localstatedir=/var --libdir=lib --buildtype=release \
  -Ddaemon=false -Dclient=true -Ddoxygen=false -Dman=false -Dtests=false -Ddatabase=simple \
  -Dbashcompletiondir=no -Dzshcompletiondir=no -Ddbus=enabled -Dglib=disabled -Dgtk=disabled \
  -Dgsettings=disabled -Dx11=disabled -Dsystemd=disabled -Dasyncns=disabled -Davahi=disabled \
  -Dopenssl=disabled -Dorc=disabled -Dfftw=disabled -Dsoxr=disabled -Dspeex=disabled \
  -Dsamplerate=disabled -Dwebrtc-aec=disabled -Dbluez5=disabled -Djack=disabled -Dlirc=disabled \
  -Dudev=disabled -Dalsa=disabled -Dgstreamer=disabled -Dtcpwrap=disabled -Dvalgrind=disabled \
  -Delogind=disabled -Dconsolekit=disabled
meson compile -C "$work/build"
DESTDIR="$dest" meson install -C "$work/build"
