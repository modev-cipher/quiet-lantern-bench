# Drives a real desktop session in a VM: keyboard and tablet through QMP, controls through AT-SPI
# in the guest, audio measured from the emulated sound card's capture file on the host.
import array
import json
import math
import os
import socket
import subprocess
import sys
import time

KIND, PACKAGE, OUT = sys.argv[1:4]
os.makedirs(OUT, exist_ok=True)
RESULTS = os.path.join(OUT, "result.txt")
SSH = ["sshpass", "-p", "testpass", "ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", "-p", "2222", "tester@127.0.0.1"]
SCP = ["sshpass", "-p", "testpass", "scp", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", "-P", "2222"]
SESSION = "export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus;"
step = 0


def note(line):
    print(line, flush=True)
    with open(RESULTS, "a", encoding="utf-8") as f:
        f.write(line + "\n")


def check(ok, what):
    note(("PASS " if ok else "FAIL ") + what)
    if not ok:
        shot("fail")
        guest("journalctl --user -u txtcaster-test --no-pager -n 60 | cut -c1-220", 20)
        sys.exit(1)


def guest(command, timeout=120, session=True, quiet=False):
    full = (SESSION if session else "") + command
    try:
        done = subprocess.run(SSH + [full], capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        print("guest timeout:", command[:80], flush=True)
        return 124, ""
    if not quiet and done.stdout.strip():
        print(done.stdout.strip()[-3000:], flush=True)
    return done.returncode, done.stdout


class Qmp:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(path)
        self.file = self.sock.makefile("r")
        self.file.readline()
        self.call("qmp_capabilities")

    def call(self, name, **args):
        self.sock.sendall(json.dumps({"execute": name, "arguments": args}).encode())
        while True:
            reply = json.loads(self.file.readline())
            if "return" in reply or "error" in reply:
                return reply


qmp = Qmp("qmp.sock")
KEYS = {" ": "spc", "/": "slash", ".": "dot", "-": "minus", "_": ("shift", "minus"), "~": ("shift", "grave_accent")}


def key(*names):
    qmp.call("send-key", keys=[{"type": "qcode", "data": n} for n in names])
    time.sleep(0.08)


def typed(text):
    for ch in text:
        if ch in KEYS:
            mapped = KEYS[ch]
            key(*(mapped if isinstance(mapped, tuple) else (mapped,)))
        elif ch.isupper():
            key("shift", ch.lower())
        else:
            key(ch)


def shot(tag):
    global step
    step += 1
    path = os.path.abspath(os.path.join(OUT, f"{step:02d}-{tag}.ppm"))
    qmp.call("screendump", filename=path)
    time.sleep(0.5)
    subprocess.run(["convert", path, path[:-3] + "png"], check=False)
    if os.path.exists(path):
        os.remove(path)


def ui(*args, timeout=90):
    return guest("python3 /tmp/guest_ui.py " + " ".join(f"'{a}'" for a in args), timeout)[0]


def click(pattern, wait=2.0):
    code = ui("click", pattern, "45")
    if code != 0:
        guest("python3 /tmp/guest_ui.py dump | cut -c1-90 | head -70", 60)
    check(code == 0, f"click {pattern}")
    time.sleep(wait)


def rms_of_new_audio(start, seconds):
    time.sleep(seconds)
    if not os.path.exists("guest.wav"):
        return 0
    with open("guest.wav", "rb") as f:
        f.seek(max(start, 44))
        data = f.read()
    samples = array.array("h")
    samples.frombytes(data[: len(data) // 2 * 2])
    if not samples:
        return 0
    return int(math.sqrt(sum(s * s for s in samples) / len(samples)))


def app_pid():
    return guest("pgrep -x txtcaster | head -1", 15, quiet=True)[1].strip()


def launch():
    command = "/usr/bin/txtcaster" if KIND == "deb" else "/home/tester/txtCaster.AppImage"
    guest("systemctl --user reset-failed txtcaster-test 2>/dev/null; true", 15, quiet=True)
    guest(f"systemd-run --user --unit=txtcaster-test --collect --setenv=RUST_LOG=info {command}", 30)
    end = time.time() + 90
    while time.time() < end:
        if ui("present", timeout=20) == 0:
            return True
        time.sleep(3)
    return False


def quit_app():
    guest("pkill -x txtcaster; sleep 4; pgrep -x txtcaster | wc -l", 20)


# --- session
end = time.time() + 300
while time.time() < end:
    if guest("loginctl list-sessions --no-legend | grep -c gdm", 15, session=False, quiet=True)[1].strip() not in ("", "0"):
        break
    time.sleep(5)
time.sleep(15)
shot("greeter")
key("ret")
time.sleep(4)
typed("testpass")
key("ret")
end = time.time() + 180
while time.time() < end:
    if "tester" in guest("loginctl list-sessions --no-legend", 15, session=False, quiet=True)[1]:
        break
    time.sleep(3)
time.sleep(25)
shot("desktop")
sid = guest("loginctl list-sessions --no-legend | awk '$3==\"tester\"{print $1;exit}'", 15, session=False, quiet=True)[1].strip()
session_type = guest(f"loginctl show-session {sid} -p Type --value", 15, session=False, quiet=True)[1].strip()
check(session_type == "wayland", f"real GNOME session is {session_type}")
guest("gsettings set org.gnome.desktop.session idle-delay 0; gsettings set org.gnome.desktop.screensaver lock-enabled false; gsettings set org.gnome.desktop.interface toolkit-accessibility true", 20)
guest("cat /etc/os-release | grep PRETTY; gnome-shell --version; uname -r; pactl info | grep -E 'Server Name|Default Sink'; pactl list short sinks | cut -c1-90", 20)
code, _ = guest("printf s3cret | secret-tool store --label=probe app txtcaster-test && secret-tool lookup app txtcaster-test", 30)
check(code == 0, "login unlocked the real keyring")
guest("secret-tool clear app txtcaster-test", 20, quiet=True)
guest("pactl set-sink-volume @DEFAULT_SINK@ 100%; pactl set-sink-mute @DEFAULT_SINK@ 0", 15, quiet=True)

# --- install
for local, remote in (("ci/guest_ui.py", "/tmp/guest_ui.py"), ("book.pdf", "/home/tester/Documents/book.pdf")):
    subprocess.run(SCP + [local, f"tester@127.0.0.1:{remote}"], check=True)
subprocess.run(SCP + [PACKAGE, f"tester@127.0.0.1:/home/tester/{os.path.basename(PACKAGE)}"], check=True)
if KIND == "deb":
    code, _ = guest(f"sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q /home/tester/{os.path.basename(PACKAGE)} 2>&1 | tail -n 2 | cut -c1-160", 400)
    check(code == 0, "deb installs with apt")
else:
    guest(f"mv /home/tester/{os.path.basename(PACKAGE)} /home/tester/txtCaster.AppImage; chmod +x /home/tester/txtCaster.AppImage; dpkg -l libfuse2t64 fuse3 2>/dev/null | tail -n 2 | cut -c1-60", 30)
A = "/home/tester/.local/share/txtcaster"
guest(f"mkdir -p {A}/engine {A}/models/tts", 20, quiet=True)
strip = open("engine-pack/.strip").read().strip()
ssh_words = " ".join(f"'{w}'" for w in SSH)
unpack = subprocess.run(f"cat engine-pack/*.tar.zst.* | {ssh_words} 'tar --zstd -x -C {A}/engine --strip-components={strip}'", shell=True)
check(unpack.returncode == 0, "engine unpacked")
with open("install.json", "w", encoding="utf-8") as f:
    json.dump({"engineVersion": "0.1.0", "engineApi": 1, "variant": "cpu", "platform": "linux-x86_64"}, f)
subprocess.run(SCP + ["install.json", f"tester@127.0.0.1:{A}/engine/install.json"], check=True)
guest(f"ls {A}/engine | head -4", 30)
subprocess.run(SCP + ["-r", "models", f"tester@127.0.0.1:{A}/models/tts/kokoro-82m"], check=True)

# --- first launch
if KIND == "deb":
    guest("ls /usr/share/applications | grep -i txt; grep -E '^(Exec|Icon|Categories)' /usr/share/applications/*xt*aster*.desktop", 20)
check(launch(), "first launch shows the app on the desktop")
time.sleep(10)
shot("wizard")
pid = app_pid()
guest(f"tr '\\0' '\\n' </proc/{pid}/environ | grep -E '^(WAYLAND_DISPLAY|XDG_SESSION_TYPE|GDK_BACKEND|APPDIR)='; cat /proc/{pid}/mountinfo | grep -c 'mount_txtCas'", 20)
_, sockets = guest(f"python3 /tmp/guest_ui.py sockets {pid}", 30)
counts = dict(line.split() for line in sockets.splitlines() if len(line.split()) == 2)
check(counts.get("wayland-0", "0") != "0" and counts.get("X11-unix/X", "0") == "0", f"native Wayland client, not XWayland ({counts})")
guest("python3 /tmp/guest_ui.py dump | cut -c1-90 | head -30", 40)
quit_app()
now = int(time.time() * 1000)
guest(f"sqlite3 {A}/db.sqlite \"insert or replace into model_install(model_id,path,sha256,disk_bytes,source,installed_at) values('kokoro-82m','tts/kokoro-82m/','96b0ef0b25c6c42a6be424d082c4b3ddfbf7701ac43d5de7e9b6d2c77662e8ec',348674924,'curated',{now}); insert or replace into app_setting(key,value,updated_at) values('onboarding_complete','true',{now}),('default_model.tts','kokoro-82m',{now});\"", 20)

# --- journey
check(launch(), "second launch")
time.sleep(10)
shot("library")
click("^Import book$", 4)
shot("file-dialog")
key("ctrl", "l")
time.sleep(1)
typed("/home/tester/Documents/book.pdf")
time.sleep(1)
shot("file-path")
key("ret")
imported = False
for _ in range(45):
    imported = guest(f"sqlite3 {A}/db.sqlite 'select count(*) from book'", 15, quiet=True)[1].strip() == "1"
    if imported:
        break
    time.sleep(2)
shot("imported")
check(imported, "import a book through the native file dialog")
time.sleep(3)
click("^Cast$", 3)
click("^Next: Sound", 3)
shot("cast-sound")
check(bool(app_pid()), "cast form Sound step renders the voice flag")
click("^Cast now$", 5)
state = ""
for _ in range(180):
    state = guest(f"sqlite3 {A}/db.sqlite 'select state from render_job order by rowid desc limit 1'", 15, quiet=True)[1].strip()
    if state in ("DONE", "FAILED", "CANCELLED"):
        break
    time.sleep(5)
shot("cast-end")
check(state == "DONE", f"real Kokoro cast finishes ({state})")
click("^Shelf$", 3)
start = os.path.getsize("guest.wav") if os.path.exists("guest.wav") else 0
click("^Open book$", 2)
level = rms_of_new_audio(start, 8)
shot("playing")
check(level > 100, f"playback reaches the sound card (rms={level})")
click("^Settings$", 3)
click("^Providers$", 3)
shot("providers")
code = ui("has", "vault is locked|No system keyring", timeout=30)
check(code != 0, "providers use the real keyring, no vault prompt")
check(bool(app_pid()), "app alive at the end")
guest("journalctl --user -u txtcaster-test --no-pager | grep -iE 'CSP violation|panic| ERROR ' | cut -c1-200 | head -10", 20)
code, csp = guest("journalctl --user -u txtcaster-test --no-pager | grep -c 'CSP violation'", 20, quiet=True)
check(csp.strip() == "0", f"no content-security violations ({csp.strip()})")
quit_app()
PROFILE = f"/home/tester/.local/share/io.github.meedoomostafa.txtcaster/localstorage/tauri_localhost_0.localstorage"
READ_KEYS = f"sqlite3 '{PROFILE}' 'select key from ItemTable order by key'"
kept = guest(READ_KEYS, 20, quiet=True)[1].split()
check(len(kept) > 0, f"preferences are saved in the live profile ({len(kept)} keys)")
for _ in range(2):
    check(launch(), "relaunch")
    time.sleep(8)
    quit_app()
    now_keys = guest(READ_KEYS, 20, quiet=True)[1].split()
    missing = sorted(set(kept) - set(now_keys))
    check(not missing, f"saved preferences survive a relaunch (missing {missing})")
moves = guest("journalctl --user -u txtcaster-test --no-pager | grep -c 'granular merge'; ls -d /home/tester/.local/share/*.migrated 2>/dev/null | wc -l", 20, quiet=True)[1].split()
check(moves[1:2] == ["0"], f"the webview profile was not archived ({moves})")
guest("journalctl --user -u txtcaster-test --no-pager | cut -c1-240 | tail -n 200 > /tmp/app.log", 20, quiet=True)
subprocess.run(SCP + ["tester@127.0.0.1:/tmp/app.log", os.path.join(OUT, "app.log")], check=False)
note("JOURNEY PASS")
