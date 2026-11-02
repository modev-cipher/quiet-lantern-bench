# Runs the uninstaller the way a person does and ticks "Delete the application data" through UI Automation.
import os
import subprocess
import sys
import time

from PIL import ImageGrab
from pywinauto import Desktop

OUT, EXE = sys.argv[1:3]
os.makedirs(OUT, exist_ok=True)


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def shot(tag):
    ImageGrab.grab().save(os.path.join(OUT, f"uninstall-{tag}.png"))


def uninstaller():
    end = time.time() + 90
    while time.time() < end:
        for w in Desktop(backend="uia").windows():
            if "Uninstall" in (w.window_text() or ""):
                return Desktop(backend="uia").window(handle=w.handle)
        time.sleep(1)
    shot("missing")
    raise SystemExit("uninstaller window not found")


subprocess.Popen([EXE])
window = uninstaller()
log("window", window.window_text())
window.set_focus()
time.sleep(2)
shot("confirm")
box = window.child_window(title_re="(?i)delete.*data", control_type="CheckBox")
box.wait("visible", timeout=30)
box.click_input()
time.sleep(1)
if box.get_toggle_state() != 1:
    shot("unticked")
    raise SystemExit("the delete application data box did not tick")
log("ticked", box.window_text())
window.child_window(title_re="^Uninstall$", control_type="Button").click_input()
end = time.time() + 300
while time.time() < end:
    if not window.exists(timeout=1):
        log("uninstaller closed itself")
        break
    try:
        close = [
            b
            for b in window.descendants(control_type="Button")
            if b.window_text() in ("Close", "Finish")
            and b.parent().element_info.control_type != "TitleBar"
        ]
        if close and close[0].is_enabled():
            shot("done")
            close[0].click_input()
            log("uninstall finished")
            break
    except Exception as err:
        log("wait", str(err)[:100])
    time.sleep(2)
else:
    shot("timeout")
    raise SystemExit("the uninstaller did not finish")
