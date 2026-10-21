"""Native GNOME/XWayland check: minimize m-dash before running with system Python."""
import json
from gi.repository import Gio

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
windows = json.loads(bus.call_sync(
    "com.openai.Codex.WindowControl", "/com/openai/Codex/WindowControl",
    "com.openai.Codex.WindowControl", "ListWindows", None, None,
    Gio.DBusCallFlags.NONE, 2000, None,
).unpack()[0])
strip = next((w for w in windows if w["wm_class"].lower() == "m-dash" and w["title"] == "m-dash · Allowances"), None)
assert strip is not None, "The minimized dashboard must show a native compact strip"
assert not strip["hidden"], "The native strip must be visible"
assert (strip["bounds"]["width"], strip["bounds"]["height"]) == (528, 88), strip["bounds"]
print("PASS native compact strip is visible at 528 x 88 logical pixels")
