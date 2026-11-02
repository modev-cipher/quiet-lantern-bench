#!/bin/bash
# Installs the package on a clean image using only its declared dependencies, then runs the app as a normal user.
set -u
kind=$1
cd /w
mkdir -p out
. /etc/os-release
case $ID in ubuntu|debian) fam=deb;; fedora) fam=rpm;; arch) fam=arch;; *) echo "unknown $ID"; exit 2;; esac
echo "image: $PRETTY_NAME, kind: $kind"
case $fam in
  deb) export DEBIAN_FRONTEND=noninteractive; apt-get update -qq >/dev/null;;
  rpm) dnf -y -q makecache >/dev/null;;
  arch) pacman -Sy --noconfirm archlinux-keyring >/dev/null; pacman -Syu --noconfirm >/dev/null;;
esac
pkg_ok=1
if [ "$kind" = deb ]; then
  apt-get install -y -q --no-install-recommends ./pkg/*.deb > out/install.log 2>&1 || pkg_ok=0
  APP=/usr/bin/txtcaster
elif [ "$kind" = rpm ]; then
  dnf install -y --setopt=install_weak_deps=False ./pkg/*.rpm > out/install.log 2>&1 || pkg_ok=0
  APP=/usr/bin/txtcaster
else
  case $fam in
    deb) for p in libasound2t64 libasound2; do apt-get install -y -q --no-install-recommends "$p" >/dev/null 2>&1 && break; done
         apt-get install -y -q --no-install-recommends libegl1 libgles2 libfontconfig1 libfreetype6 libharfbuzz0b libfribidi0 libwayland-client0 libwayland-egl1 libwayland-cursor0 libwayland-server0 libx11-6 libxext6 libxrender1 libxi6 libxcursor1 libxrandr2 libxinerama1 libxcomposite1 libxdamage1 libxfixes3 libx11-xcb1 libxcb1 fuse3 >out/install.log 2>&1 || pkg_ok=0;;
    rpm) dnf install -y --setopt=install_weak_deps=False alsa-lib mesa-libEGL mesa-libGLES fontconfig freetype harfbuzz fribidi libwayland-client libwayland-egl libwayland-cursor libwayland-server libX11 libXext libXrender libXi libXcursor libXrandr libXinerama libXcomposite libXdamage libXfixes fuse3 >out/install.log 2>&1 || pkg_ok=0;;
    arch) pacman -S --noconfirm alsa-lib mesa fontconfig freetype2 harfbuzz fribidi wayland libx11 libxext libxrender libxi libxcursor libxrandr libxinerama libxcomposite libxdamage libxfixes fuse3 >out/install.log 2>&1 || pkg_ok=0;;
  esac
  cp pkg/*.AppImage /opt/txtcaster.AppImage; chmod 755 /opt/txtcaster.AppImage
  APP=/opt/txtcaster.AppImage
fi
tail -n 3 out/install.log | cut -c1-200
[ $pkg_ok = 1 ] || { echo "FAIL install"; tail -n 30 out/install.log; exit 1; }
if [ "$kind" != appimage ]; then
  echo "--- missing libraries"; ldd "$APP" | grep 'not found' && { echo "FAIL ldd"; exit 1; }
fi
case $fam in
  deb) apt-get install -y -q --no-install-recommends systemd xvfb openbox xdotool dbus dbus-x11 gnome-keyring pipewire pipewire-pulse wireplumber pulseaudio-utils imagemagick sqlite3 python3 zstd fonts-noto-core fonts-noto-color-emoji procps >>out/install.log 2>&1;;
  rpm) dnf install -y --setopt=install_weak_deps=False xorg-x11-server-Xvfb openbox xdotool dbus-daemon dbus-tools gnome-keyring pipewire pipewire-pulseaudio pipewire-utils wireplumber pulseaudio-utils ImageMagick sqlite python3 zstd tar google-noto-sans-fonts google-noto-color-emoji-fonts procps-ng util-linux >>out/install.log 2>&1;;
  arch) pacman -S --noconfirm xorg-server-xvfb openbox xdotool dbus gnome-keyring pipewire pipewire-pulse wireplumber libpulse imagemagick sqlite python zstd noto-fonts noto-fonts-emoji procps-ng >>out/install.log 2>&1;;
esac
for t in Xvfb openbox xdotool dbus-run-session pipewire wireplumber pactl parec sqlite3 python3 zstd; do command -v "$t" >/dev/null || { echo "harness tool missing: $t"; tail -n 20 out/install.log; exit 3; }; done
command -v magick >/dev/null || ln -s "$(command -v convert)" /usr/local/bin/magick
fc-list | grep -ic 'emoji' | sed 's/^/emoji fonts: /'
mkdir -p /run/systemd/seats /run/systemd/sessions /run/systemd/users /run/systemd/machines
useradd -m tester
A=/home/tester/.local/share/txtcaster
mkdir -p "$A/engine" "$A/models/tts"
cat engine-pack/*.tar.zst.* | tar --zstd -x -C "$A/engine" --strip-components=$(cat engine-pack/.strip)
printf '{"engineVersion":"0.1.0","engineApi":1,"variant":"cpu","platform":"linux-x86_64"}' > "$A/engine/install.json"
mv models "$A/models/tts/kokoro-82m"
chown -R tester: /home/tester out
chmod -R a+rX /w
if [ "$kind" = appimage ]; then
  echo "--- FUSE: can a user run it directly?"
  su tester -c "timeout 20 /opt/txtcaster.AppImage --appimage-version" 2>&1 | head -3
  export APPIMAGE_EXTRACT_AND_RUN=1
fi
install -d -o tester -m 700 /tmp/xdg-tester
su tester -s /bin/bash -c "cd /w && APPIMAGE_EXTRACT_AND_RUN=${APPIMAGE_EXTRACT_AND_RUN:-} XDG_RUNTIME_DIR=/tmp/xdg-tester dbus-run-session -- bash ci/distro_flow.sh $APP /w/book.pdf /w/out"
rc=$?
echo "=== $PRETTY_NAME $kind exit $rc"
cat out/result.txt 2>/dev/null
exit $rc
