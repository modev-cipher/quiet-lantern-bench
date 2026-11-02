#!/usr/bin/env bash
# The AppImage must use the host's libwayland: a bundled older copy segfaults inside
# NVIDIA's EGL at startup. linuxdeploy offers no exclusion, so repack without it.
set -euo pipefail
bundle="${1:?appimage bundle directory required}"
tool="${HOME}/.cache/tauri/linuxdeploy-plugin-appimage.AppImage"
appdir="$(ls -d "$bundle"/*.AppDir)"
image="$(ls "$bundle"/*.AppImage)"
rm -f "$appdir"/usr/lib/libwayland-*.so*
# The bundled WebKit's Skia aborts the page on COLRv1 color fonts such as Fedora's Noto Color
# Emoji, freezing the window on the first emoji drawn. Plain glyphs replace color ones here.
mkdir -p "$appdir/usr/share/txtcaster"
cat > "$appdir/usr/share/txtcaster/fonts.conf" <<'FONTS'
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "fonts.dtd">
<fontconfig>
  <include ignore_missing="yes">/etc/fonts/fonts.conf</include>
  <selectfont><rejectfont><pattern><patelt name="color"><bool>true</bool></patelt></pattern></rejectfont></selectfont>
</fontconfig>
FONTS
sed -i '/^exec /i export FONTCONFIG_FILE="${FONTCONFIG_FILE:-$this_dir/usr/share/txtcaster/fonts.conf}"' "$appdir/AppRun"
grep -q FONTCONFIG_FILE "$appdir/AppRun"
# linuxdeploy's GTK hook forces X11, so Wayland sessions drew through XWayland with uneven frame
# pacing. GTK then picks the session's own backend, and a user's GDK_BACKEND still wins.
gtk_hook="$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
sed -i '/^export GDK_BACKEND=/d' "$gtk_hook"
if grep -q GDK_BACKEND "$gtk_hook"; then exit 1; fi
# See build_libpulse.sh: the pinned client library replaces the 22.04 one.
pulse="${LIBPULSE_PREFIX:-/opt/libpulse}"
rm -f "$appdir"/usr/lib/libpulse.so* "$appdir"/usr/lib/libpulsecommon-*.so
cp -L "$pulse/usr/lib/libpulse.so.0" "$appdir/usr/lib/libpulse.so.0"
cp "$pulse"/usr/lib/pulseaudio/libpulsecommon-*.so "$appdir/usr/lib/"
for lib in "$appdir"/usr/lib/libpulse.so.0 "$appdir"/usr/lib/libpulsecommon-*.so; do
  patchelf --set-rpath '$ORIGIN' "$lib"
done
test -e "$appdir/usr/lib/libpulsecommon-17.0.so"
rm -f "$image"
cd "$bundle"
ARCH=x86_64 LDAI_OUTPUT="$(basename "$image")" "$tool" --appimage-extract-and-run --appdir "$(basename "$appdir")"
test -s "$image"
