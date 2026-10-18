"""Remove the corner-connected white matte from the approved orange icon.

Usage: python scripts/remove-icon-matte.py INPUT.png OUTPUT.png
Requires Pillow. Interior artwork is copied without modification.
"""

from collections import deque
from pathlib import Path
import sys

from PIL import Image


def remove_matte(source: Path, destination: Path) -> None:
    with Image.open(source) as image:
        original = image.convert("RGBA")
    if original.getchannel("A").getextrema() != (255, 255):
        raise ValueError("Expected the original opaque artwork")
    width, height = original.size
    pixels = original.load()
    exterior = bytearray(width * height)
    queue = deque([(0, 0), (width - 1, 0), (0, height - 1), (width - 1, height - 1)])
    while queue:
        x, y = queue.popleft()
        index = y * width + x
        if exterior[index]:
            continue
        r, g, b, _ = pixels[x, y]
        # Include the white-to-orange antialiased rim, but not opaque orange.
        # Connectivity keeps the pale wind ribbons and veins inside untouched.
        if not (r > 220 and g > 140 and b > 70 and r - b < 180):
            continue
        exterior[index] = 1
        for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
            if 0 <= nx < width and 0 <= ny < height:
                queue.append((nx, ny))

    result = original.copy()
    output = result.load()
    # The existing tile's orange perimeter, sampled from its opaque edge.
    orange = (252, 130, 58)
    delta = tuple(255 - channel for channel in orange)
    denominator = sum(channel * channel for channel in delta)
    for index, outside in enumerate(exterior):
        if not outside:
            continue
        x, y = index % width, index // width
        rgb = pixels[x, y][:3]
        coverage = sum((255 - channel) * d for channel, d in zip(rgb, delta)) / denominator
        # Suppress the near-white source's compression/texture noise.
        alpha = 0 if coverage < 0.04 else round(255 * min(1.0, coverage))
        output[x, y] = (*orange, alpha)

    assert all(output[x, y][3] == 0 for x, y in ((0, 0), (width - 1, 0), (0, height - 1), (width - 1, height - 1)))
    assert all(output[x, y] == pixels[x, y] for y in range(height) for x in range(width) if not exterior[y * width + x])
    result.save(destination)
    print(f"Removed corner matte; preserved {len(exterior) - sum(exterior):,} interior pixels exactly")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    remove_matte(Path(sys.argv[1]), Path(sys.argv[2]))
