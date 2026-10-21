"""Native GNOME/XWayland regression. Start the installed dashboard first."""
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path

import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, Gio, GLib
from PIL import Image

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

def call(dest, path, interface, method, args=None):
    return bus.call_sync(dest, path, interface, method, args, None,
                         Gio.DBusCallFlags.NONE, 3000, None).unpack()

def desktop(method, args=None):
    return call("com.openai.Codex.WindowControl", "/com/openai/Codex/WindowControl",
                "com.openai.Codex.WindowControl", method, args)

def main_window():
    return next(w for w in json.loads(desktop("ListWindows")[0])
                if (w.get("wm_class") or "").lower() == "m-dash"
                and w["title"] == "m-dash · AI resources")

def nodes(a):
    yield a
    for i in range(a.get_child_count()):
        child = a.get_child_at_index(i)
        if child:
            yield from nodes(child)

def open_strip():
    root = Atspi.get_desktop(0)
    for i in range(root.get_child_count()):
        app = root.get_child_at_index(i)
        if app.get_name().lower() == "m-dash":
            for node in nodes(app):
                if node.get_name() == "Open m-dash dashboard":
                    assert node.get_action_iface().do_action(0)
                    return
    raise AssertionError("Native strip restore button unavailable")

def open_tray():
    items = call("org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher",
                 "org.freedesktop.DBus.Properties", "Get",
                 GLib.Variant("(ss)", ("org.kde.StatusNotifierWatcher", "RegisteredStatusNotifierItems")))[0]
    item, = [v for v in items if "tray_icon_tray_app_accounts" in v]
    dest, path = item.split("@", 1)
    menu = call(dest, path, "org.freedesktop.DBus.Properties", "Get",
                GLib.Variant("(ss)", ("org.kde.StatusNotifierItem", "Menu")))[0]
    layout = call(dest, menu, "com.canonical.dbusmenu", "GetLayout", GLib.Variant("(iias)", (0, -1, [])))[1]
    action, = [v[0] for v in layout[2] if v[1].get("label") == "Open m-dash"]
    call(dest, menu, "com.canonical.dbusmenu", "Event",
         GLib.Variant("(isvu)", (action, "clicked", GLib.Variant("i", 0), 0)))

def assert_rendered():
    window = None
    for _ in range(30):
        try:
            window = main_window()
        except StopIteration:
            time.sleep(.1)
            continue
        if not window["hidden"] and window["focused"]:
            break
        time.sleep(.1)
    assert window is not None, "Restore did not show the dashboard"
    assert not window["hidden"], "Restore left the dashboard minimized"
    assert window["focused"], "Restore did not activate the dashboard"
    time.sleep(.3)  # Let GNOME's unminimize animation finish before checking pixels.
    # Verify the app header on screen; mapped/compositor state alone missed this bug.
    monitors = json.loads(desktop("GetMonitorLayout")[0])
    assert monitors, "Keep the Ubuntu monitors connected for native visual verification"
    width = max(m["x"] + m["width"] for m in monitors)
    stats = subprocess.check_output(["xwininfo", "-id", xid], text=True)
    x = int(next(s for s in stats.splitlines() if "Absolute upper-left X:" in s).split(":")[1])
    y = int(next(s for s in stats.splitlines() if "Absolute upper-left Y:" in s).split(":")[1])
    native_width = int(next(s for s in stats.splitlines() if s.strip().startswith("Width:")).split(":")[1])
    fd, name = tempfile.mkstemp(prefix="computer-use-linux-gnome-extension-restore-", suffix=".png")
    os.close(fd)
    try:
        assert desktop("CaptureScreenshot", GLib.Variant("(s)", (name,)))[0]
        image = Image.open(name).convert("RGB")
        scale = image.width / width
        native_scale = native_width / window["bounds"]["width"]
        left, top = x / native_scale, y / native_scale
        header = image.crop(tuple(round(v * scale) for v in (left, top, left + 350, top + 80))).resize((350, 80))
        for point in [(10, 10), (10, 60), (330, 10)]:
            assert max(abs(a - b) for a, b in zip(header.getpixel(point), (19, 24, 32))) < 5, "Dashboard header is not visible on screen"
        tile = header.crop((38, 12, 104, 66))
        assert sum(count for count, (r, g, b) in tile.getcolors(4096)
                   if r < 80 and g > 140 and b > 160) > 4, "M- brand mark is not visible on screen"
    finally:
        Path(name).unlink(missing_ok=True)

window = main_window()
assert window["client_type"] == "x11", "This regression qualifies XWayland"
other = next((w for w in json.loads(desktop("ListWindows")[0])
              if (w.get("wm_class") or "").lower() != "m-dash" and not w["hidden"]), None)
assert other, "Keep another desktop application open for the activation regression"
pid = str(window["pid"])
xid = subprocess.check_output(["xdotool", "search", "--all", "--pid", pid,
                               "--name", "^m-dash · AI resources$"], text=True).strip()
assert desktop("ActivateWindow", GLib.Variant("(t)", (window["window_id"],)))[0]
time.sleep(.3)
assert_rendered()
for route in ["strip", "tray", "second launch", "closed dashboard"]:
    if route == "closed dashboard":
        subprocess.run(["wmctrl", "-ic", hex(int(xid))], check=True)
    else:
        subprocess.run(["xdotool", "windowminimize", xid], check=True)
    time.sleep(.3)
    assert desktop("ActivateWindow", GLib.Variant("(t)", (other["window_id"],)))[0]
    time.sleep(.1)
    if route == "strip":
        open_strip()
    elif route == "second launch":
        subprocess.run(["gio", "launch", "/usr/share/applications/m-dash.desktop"], cwd="/tmp", check=True)
    else:
        open_tray()
    assert_rendered()
    assert str(main_window()["pid"]) == pid
    print(f"PASS {route}: same dashboard is unminimized, active and visibly rendered")
