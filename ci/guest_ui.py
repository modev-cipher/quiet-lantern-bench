# Runs inside the VM's desktop session: finds the app's controls by name through AT-SPI.
import re
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402


def app_node():
    desktop = Atspi.get_desktop(0)
    for index in range(desktop.get_child_count()):
        node = desktop.get_child_at_index(index)
        if node is not None and "txtcaster" in (node.get_name() or "").lower():
            return node
    return None


def walk(node, out):
    if node is None or len(out) > 6000:
        return out
    try:
        out.append(node)
        for index in range(node.get_child_count()):
            walk(node.get_child_at_index(index), out)
    except Exception:
        pass
    return out


def nodes():
    root = app_node()
    return walk(root, []) if root is not None else []


def find(pattern, timeout):
    rx = re.compile(pattern)
    end = time.time() + timeout
    while time.time() < end:
        for node in nodes():
            try:
                if rx.search(node.get_name() or ""):
                    if node.get_action_iface() is not None:
                        return node
            except Exception:
                pass
        time.sleep(1)
    return None


command = sys.argv[1]
if command == "dump":
    for node in nodes():
        try:
            name = node.get_name() or ""
            if name:
                print(f"{node.get_role_name()}\t{name[:100]}")
        except Exception:
            pass
elif command == "present":
    sys.exit(0 if app_node() is not None else 1)
elif command == "has":
    rx = re.compile(sys.argv[2])
    sys.exit(0 if any(rx.search(n.get_name() or "") for n in nodes()) else 1)
elif command == "click":
    node = find(sys.argv[2], float(sys.argv[3]) if len(sys.argv) > 3 else 30)
    if node is None:
        print("not found:", sys.argv[2])
        sys.exit(2)
    node.get_action_iface().do_action(0)
