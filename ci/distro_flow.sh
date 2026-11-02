#!/bin/bash
# Clicks are screen coordinates for a 1100x720 window centred on 1400x900, measured on the shipped layout.
set -u
APP=$1 BOOK=$2 OUT=$3
A=$HOME/.local/share/txtcaster
export DISPLAY=:9 GDK_BACKEND=x11 GTK_USE_PORTAL=0 RUST_LOG=info
unset WAYLAND_DISPLAY
: > "$OUT/result.txt"
pass() { echo "PASS $*" | tee -a "$OUT/result.txt"; }
fail() { echo "FAIL $*" | tee -a "$OUT/result.txt"; shot "fail"; finish 1; }
shot() { import -window root "$OUT/$1.png" 2>/dev/null; }
pause() { python3 -I -c "import time;time.sleep($1)"; }
c() { xdotool mousemove "$1" "$2" click 1; }
seekbar() { echo $(( 411 + (925 - 411) * $1 / 100 )) 710; }
listen() {
  timeout "$1" parec -d probe.monitor --format=s16le --rate=24000 --channels=1 >"$OUT/rec.raw" 2>>"$OUT/parec.log"
  python3 -I -c "
import sys,array,math
d=open(sys.argv[1],'rb').read();a=array.array('h');a.frombytes(d[:len(d)//2*2])
print(int(math.sqrt(sum(x*x for x in a)/max(1,len(a)))))" "$OUT/rec.raw"
  rm -f "$OUT/rec.raw"
}
db() { sqlite3 "$A/db.sqlite" "$1" 2>/dev/null; }
alive() { pgrep -x txtcaster >/dev/null; }
finish() {
  pkill -x txtcaster
  sleep 2
  tail -n 40 "$OUT/app.log" | cut -c1-220 > "$OUT/app-tail.txt"
  exit "$1"
}
start() {
  "$APP" >>"$OUT/app.log" 2>&1 &
  sleep 5
  for _ in $(seq 60); do
    xdotool search --onlyvisible --name '^txtCaster$' >/dev/null 2>&1 && return 0
    alive || return 1
    sleep 1
  done
  return 1
}
Xvfb :9 -screen 0 1400x900x24 -nolisten tcp >"$OUT/x.log" 2>&1 &
sleep 2
printf test | gnome-keyring-daemon --unlock --components=secrets >"$OUT/kr.log" 2>&1
openbox >/dev/null 2>&1 &
pipewire >"$OUT/pw.log" 2>&1 &
sleep 3
pipewire-pulse >"$OUT/pp.log" 2>&1 &
sleep 2
for _ in $(seq 10); do
  pgrep -x wireplumber >/dev/null || { wireplumber >>"$OUT/wp.log" 2>&1 & }
  sleep 3
  pactl load-module module-null-sink sink_name=probe >/dev/null 2>&1 && pactl set-default-sink probe && break
done
pactl list short sinks | grep -q probe || fail "harness: test sink"
pactl info | grep -E 'Server Name|Default Sink' | cut -c1-80

start || { tail -n 20 "$OUT/app.log" | cut -c1-200; fail "first launch shows a window"; }
pass "first launch shows a window"
sleep 12
shot 01-wizard
colors=$(magick "$OUT/01-wizard.png" -format %k info:)
[ "${colors:-0}" -gt 200 ] || fail "wizard renders (colors=$colors)"
pass "wizard renders ($colors colors)"
c 1067 713; pause 3; shot 02-wizard-voice
alive || fail "wizard step 2 keeps the app alive"
pkill -x txtcaster; for _ in $(seq 15); do alive || break; sleep 1; done
alive && fail "app exits on request"
pass "app exits on request"
grep -q . "$A/db.sqlite" 2>/dev/null || fail "database created"
now=$(date +%s)000
sqlite3 "$A/db.sqlite" "insert or replace into model_install(model_id,path,sha256,disk_bytes,source,installed_at) values('kokoro-82m','tts/kokoro-82m/','96b0ef0b25c6c42a6be424d082c4b3ddfbf7701ac43d5de7e9b6d2c77662e8ec',348674924,'curated',$now); insert or replace into app_setting(key,value,updated_at) values('onboarding_complete','true',$now),('default_model.tts','kokoro-82m',$now);" || fail "seed the profile"
start || fail "second launch shows a window"
sleep 10
shot 03-library
c 1139 201; pause 4; shot 04-dialog
xdotool key ctrl+l; pause 1; xdotool type --delay 15 "$BOOK"; pause 1; shot 05-path; xdotool key Return
for _ in $(seq 60); do [ "$(db 'select count(*) from book')" = 1 ] && break; alive || break; sleep 2; done
[ "$(db 'select count(*) from book')" = 1 ] || fail "import a book"
pass "import a book"
pause 3; shot 06-imported
c 360 438; pause 4; shot 07-reader
c 452 242; pause 4; shot 08-cast-scope
c 1048 752; pause 4; shot 09-cast-sound
alive || fail "cast form Sound step renders the voice flag without crashing"
pass "cast form Sound step renders the voice flag"
c 918 752; pause 4; shot 10-cast-started
state=
for _ in $(seq 180); do
  state=$(db 'select state from render_job order by rowid desc limit 1')
  case "$state" in DONE|FAILED|CANCELLED) break;; esac
  alive || break
  sleep 5
done
[ "$state" = DONE ] || { fail "cast finishes (state=$state)"; }
pass "real Kokoro cast on the CPU engine finishes"
mp3=$(find "$A/exports" -name '*.mp3' | head -1)
[ -s "$mp3" ] || fail "cast writes an MP3"
pass "cast writes an MP3 ($(stat -c %s "$mp3") bytes)"
pause 2; shot 11-done
c 180 247; pause 3; shot 12-library-done
c 360 438; pause 3
rms=$(listen 6)
shot 13-playing
[ "${rms:-0}" -gt 100 ] || { pactl list short sink-inputs | cut -c1-80; fail "playback reaches the audio server (rms=$rms)"; }
pass "playback reaches the audio server (rms=$rms)"
# libpulse 15.99 to 16.1 can leave the player silent after a seek: seek while playing, then pause, seek and play.
heard=0 tries=0
for p in 20 65 35 80 10 55 25 70 45 15 60 30; do
  tries=$((tries + 1))
  c $(seekbar $p); pause 1
  rms=$(listen 3)
  if [ "${rms:-0}" -gt 100 ]; then heard=$((heard + 1)); else echo "seek to $p% while playing: silent (rms=$rms)"; fi
done
for p in 40 75 20 60 5 50; do
  tries=$((tries + 1))
  c 668 750; pause 2; c $(seekbar $p); pause 2; c 668 750; pause 1
  rms=$(listen 3)
  if [ "${rms:-0}" -gt 100 ]; then heard=$((heard + 1)); else echo "pause, seek to $p%, play: silent (rms=$rms)"; fi
done
shot 13b-after-seeks
common=$(for p in $(pgrep -x WebKitWebProces); do grep -o '/[^ ]*libpulsecommon-[0-9.]*\.so' "/proc/$p/maps"; done | sort -u | head -n 1)
[ "$heard" = "$tries" ] || fail "playback resumes after every seek ($heard/$tries, ${common:-no libpulse})"
pass "playback resumes after every seek ($tries)"
version=${common##*libpulsecommon-}; version=${version%.so}
[ -n "$common" ] && [ "$(printf '%s\n' 17.0 "$version" | sort -V | head -n 1)" = 17.0 ] \
  || fail "the player loads libpulse 17 or later (${common:-none})"
pass "the player loads $common"
alive || fail "app alive at the end"
pass "app alive at the end"
if grep -q 'CSP violation' "$OUT/app.log"; then
  grep 'CSP violation' "$OUT/app.log" | cut -c1-200 | sort -u | head -8
  fail "content-security violations"
fi
pass "no content-security violations"
pkill -x txtcaster; for _ in $(seq 15); do alive || break; sleep 1; done
pactl unload-module module-null-sink; sleep 2
pactl list short sinks | cut -c1-70
start || fail "launch with no audio output"
sleep 8
c 360 438; pause 4; shot 14-no-output-opened; c 668 750; pause 30; shot 15-no-output
alive || fail "no audio output keeps the app alive"
pass "no audio output keeps the app alive"
pkill -x txtcaster; for _ in $(seq 15); do alive || break; sleep 1; done
pkill -x wireplumber; pkill -x pipewire-pulse; pkill -x pipewire; sleep 2
start || fail "launch with no audio server"
sleep 8
c 360 438; pause 4; c 668 750; pause 30; shot 16-no-server
alive || fail "no audio server keeps the app alive"
pass "no audio server keeps the app alive"
finish 0
