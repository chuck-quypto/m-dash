"""Render installer type from bundled fonts, without installing system fonts.

Build-only requirements: fonttools, brotli, rsvg-convert.
"""
from pathlib import Path
import base64
import subprocess
import tempfile
import xml.etree.ElementTree as ET

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

root = Path(__file__).resolve().parent.parent
namespace = "http://www.w3.org/2000/svg"
ET.register_namespace("", namespace)
tree = ET.parse(root / "src-tauri/dmg-background.svg")
svg = tree.getroot()
for text in list(svg):
    if text.tag != f"{{{namespace}}}text":
        continue
    family = text.attrib["font-family"].split(",")[0]
    filename = {
        "CND Hybrid Display": "cnd-hybrid-display-v1.woff2",
        "Space Grotesk": "space-grotesk-latin-wght-normal.woff2",
        "Geist": "geist-latin-wght-normal.woff2",
    }[family]
    font = TTFont(root / "src/brand/fonts" / filename)
    if "fvar" in font:
        font = instantiateVariableFont(
            font, {"wght": int(text.get("font-weight", "400"))}, inplace=True,
        )
    glyphs, cmap = font.getGlyphSet(), font.getBestCmap()
    label = text.text
    assert label and all(ord(char) in cmap for char in label), "Missing installer glyph"
    scale = float(text.get("font-size")) / font["head"].unitsPerEm
    width = sum(glyphs[cmap[ord(char)]].width for char in label) * scale
    x = float(text.get("x")) - (width / 2 if text.get("text-anchor") == "middle" else 0)
    group = ET.Element(f"{{{namespace}}}g", {
        "fill": text.get("fill"), "aria-label": label,
        "transform": f'translate({x} {text.get("y")}) scale({scale} {-scale})',
    })
    cursor = 0
    for char in label:
        glyph = glyphs[cmap[ord(char)]]
        pen = SVGPathPen(glyphs)
        glyph.draw(pen)
        ET.SubElement(group, f"{{{namespace}}}path", {
            "d": pen.getCommands(), "transform": f"translate({cursor} 0)",
        })
        cursor += glyph.width
    svg.insert(list(svg).index(text), group)
    svg.remove(text)
for image in svg.iter(f"{{{namespace}}}image"):
    data = (root / "src-tauri" / image.attrib["href"]).read_bytes()
    assert data.startswith(b"\x89PNG\r\n\x1a\n"), "Installer image must be PNG"
    image.set("href", "data:image/png;base64," + base64.b64encode(data).decode("ascii"))
assert not list(svg.iter(f"{{{namespace}}}text")), "Installer text must be outlined"
with tempfile.TemporaryDirectory() as directory:
    outlined = Path(directory) / "background.svg"
    tree.write(outlined, encoding="unicode")
    subprocess.run(["rsvg-convert", str(outlined), "-o", str(root / "src-tauri/dmg-background.png")], check=True)
