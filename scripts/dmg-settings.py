"""dmgbuild 1.6.7 settings; run from the repo root with -D app=/stapled/m-dash.app.

Copies an already signed/stapled app without rebuilding or signing it again.
"""
import json
from pathlib import Path

root = Path.cwd()
application = Path(defines["app"]).resolve()  # supplied by dmgbuild
assert application.name == "m-dash.app" and application.is_dir()
config = json.loads((root / "src-tauri/tauri.macos.conf.json").read_text())
layout = config["bundle"]["macOS"]["dmg"]
format = "UDZO"
files = [str(application)]
symlinks = {"Applications": "/Applications"}
icon = str(application / "Contents/Resources/icon.icns")
background = str(root / "src-tauri" / layout["background"])
window_rect = ((100, 100), (layout["windowSize"]["width"], layout["windowSize"]["height"]))
icon_locations = {
    "m-dash.app": (layout["appPosition"]["x"], layout["appPosition"]["y"]),
    "Applications": (layout["applicationFolderPosition"]["x"], layout["applicationFolderPosition"]["y"]),
}
default_view = "icon-view"
grid_spacing = 80
icon_size = 104
text_size = 14
