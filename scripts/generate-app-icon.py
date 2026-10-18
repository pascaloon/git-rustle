"""Package the approved artwork as a Windows ICO (requires Pillow).

Run from any directory: python scripts/generate-app-icon.py
Normal Cargo builds use the checked-in ICO and do not require Python.
"""

from pathlib import Path

from PIL import Image


assets = Path(__file__).resolve().parent.parent / "assets"
sizes = [(size, size) for size in (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)]
with Image.open(assets / "app-icon.png") as artwork:
    if artwork.width != artwork.height:
        raise ValueError("Application icon artwork must be square")
    artwork.convert("RGBA").save(assets / "app-icon.ico", format="ICO", sizes=sizes)

with Image.open(assets / "app-icon.ico") as icon:
    if icon.info["sizes"] != set(sizes):
        raise ValueError("Generated ICO is missing expected sizes")
print("Generated assets/app-icon.ico (16–256 px)")
