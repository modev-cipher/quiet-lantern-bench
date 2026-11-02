# Drives the installed app the way a person does: real mouse and keyboard on names read through UI Automation.
import glob, json, os, re, sqlite3, sys, time
from PIL import ImageGrab
from pywinauto import Desktop, keyboard

OUT, BOOK = sys.argv[1:3]
PHASE = sys.argv[3] if len(sys.argv) > 3 else "full"
DATA = os.path.join(os.environ["APPDATA"], "txtcaster")
DB = os.path.join(DATA, "db.sqlite")
os.makedirs(OUT, exist_ok=True)
step_no = 0


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def main_window():
    return Desktop(backend="uia").window(title="txtCaster", control_type="Window")


def elements():
    return main_window().descendants()


def name_of(e):
    try:
        return e.element_info.name or ""
    except Exception:
        return ""


def dump(tag):
    global step_no
    step_no += 1
    base = os.path.join(OUT, f"{step_no:02d}-{tag}")
    ImageGrab.grab().save(base + ".png")
    with open(base + ".txt", "w", encoding="utf-8") as f:
        for e in elements():
            n = name_of(e)
            if n:
                r = e.rectangle()
                f.write(f"{e.element_info.control_type}\t{n[:120]}\t{r.left},{r.top},{r.right},{r.bottom}\n")
    log("dump", tag)


def find(pattern, types=None, timeout=60):
    end = time.time() + timeout
    rx = re.compile(pattern)
    while time.time() < end:
        try:
            hits = [e for e in elements() if rx.search(name_of(e)) and (types is None or e.element_info.control_type in types)]
            hits = [e for e in hits if e.rectangle().width() > 0]
            if hits:
                return hits
        except Exception as err:
            log("find retry", str(err)[:120])
        time.sleep(1.5)
    dump("missing")
    raise SystemExit(f"not found: {pattern}")


CLICKABLE = ("Button", "Hyperlink", "RadioButton", "TabItem", "ListItem", "CheckBox", "Group")


def click(pattern, timeout=60, types=CLICKABLE):
    e = find(pattern, types, timeout)[0]
    log("click", name_of(e)[:70])
    e.click_input()
    time.sleep(1.5)


def click_near(pattern, anchor, timeout=60):
    a = find(anchor, None, timeout)[0].rectangle()
    hits = find(pattern, ("Button",), timeout)
    below = [h for h in hits if h.rectangle().top >= a.top]
    e = min(below or hits, key=lambda h: h.rectangle().top - a.top)
    log("click", name_of(e), "near", anchor)
    e.click_input()
    time.sleep(1.5)


def scalar(sql):
    con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True, timeout=5)
    try:
        return con.execute(sql).fetchone()[0]
    finally:
        con.close()


def until(what, fn, seconds, every=5):
    end = time.time() + seconds
    while time.time() < end:
        try:
            v = fn()
            if v:
                log("ok", what, v)
                return v
        except Exception as err:
            log(what, "retry", str(err)[:120])
        time.sleep(every)
    dump("timeout")
    raise SystemExit("timeout: " + what)


def file_dialog(path):
    def present():
        for w in main_window().children():
            if w.element_info.class_name == "#32770" and name_of(w) == "Open":
                return w
        return None

    dlg = until("file dialog", present, 60, 2)
    dump("file-dialog")
    dlg.set_focus()
    keyboard.send_keys("%n")
    time.sleep(0.5)
    keyboard.send_keys(path.replace("(", "{(}").replace(")", "{)}"), with_spaces=True, pause=0.01)
    time.sleep(0.5)
    keyboard.send_keys("{ENTER}")


if PHASE == "failure":
    find(sys.argv[4], None, 60)
    dump("startup-failure")
    log("startup failure screen shown")
    sys.exit(0)

until("window", lambda: main_window().exists(timeout=1), 120, 2)
time.sleep(8)
dump("wizard")
click("^Continue$")
dump("wizard-voice")
click("^Skip setup$")
time.sleep(3)
dump("library")
click("^Import book$")
file_dialog(BOOK)
time.sleep(4)
dump("engine-sheet")
find("Reader engine", None, 60)
click_near("^Install$", "Reader engine")
dump("reader-installing")
until("book imported", lambda: scalar("select count(*) from book") == 1, 600)
time.sleep(4)
dump("imported")

def open_book():
    click("^Shelf$", 20)
    time.sleep(2)
    click("^Open book$", 30)
    time.sleep(4)


def variant():
    with open(os.path.join(DATA, "engine", "install.json"), encoding="utf-8") as f:
        return json.load(f)["variant"]


click("^Cast$")
time.sleep(2)
dump("cast-scope")
click("^Next: Sound")
dump("cast-sound-reader-engine")
click("^Local$")
dump("cast-local-on-reader")
click("^Get this model$")
time.sleep(3)
dump("full-sheet")
find("Full engine", None, 60)
click_near("^Install$", "Full engine")
dump("full-installing")
until("cpu engine", lambda: variant() == "cpu", 1200)
time.sleep(8)
dump("after-full-install")
click_near("^(Download|Get|Install)", r"Pro\+", 60)
dump("kokoro-downloading")
until("kokoro installed", lambda: scalar("select count(*) from model_install where model_id='kokoro-82m'") == 1, 1200)
dump("kokoro-installed")
click("^Shelf$", 20)
time.sleep(2)
dump("shelf-2")
click("^Cast$")
time.sleep(3)
dump("cast-draft-resumed")
click("^Cast now$")
time.sleep(5)
dump("cast-started")


def job_state():
    state = scalar("select state from render_job order by rowid desc limit 1")
    log("job", state)
    return state if state in ("DONE", "FAILED", "CANCELLED") else None


state = until("job end", job_state, 3600, 20)
dump("job-end")
if state != "DONE":
    raise SystemExit("cast ended " + state)
mp3s = glob.glob(os.path.join(DATA, "exports", "**", "*.mp3"), recursive=True)
log("outputs", mp3s)
with open(os.path.join(OUT, "outputs.json"), "w", encoding="utf-8") as f:
    json.dump(mp3s, f)
open_book()
time.sleep(6)
dump("playing-no-audio-device")
if not main_window().exists(timeout=2):
    raise SystemExit("app closed during playback without an audio device")
log("full journey done")
