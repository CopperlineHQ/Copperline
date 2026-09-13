#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate the MSIX logo assets from the brand artwork.

The Microsoft Store shows an app through a fixed set of named PNGs referenced
by AppxManifest.xml (see the uap:VisualElements element there). They are
committed under assets/ so a Windows packaging run needs no image tooling,
and this script regenerates them when the brand artwork changes:

    python3 packaging/windows/msix/generate-logos.py

Sources are the repository's own brand files: the icns carries a 1024x1024
master of the Copperline mark, which downscales cleanly to every square tile,
and the wordmark fills the wide tile where a lone mark would look lost.

Requires Pillow (pip install pillow). Run from the repository root.
"""

import sys
from pathlib import Path

try:
    from PIL import Image
except ImportError:
    sys.exit("Pillow is required: pip install pillow")

REPO = Path(__file__).resolve().parents[3]
MARK = REPO / "assets" / "brand" / "copperline.icns"
WORDMARK = REPO / "assets" / "brand" / "copperline-logo.png"
OUT = Path(__file__).resolve().parent / "assets"

# Plain square logos: the name Windows looks for, and the edge length in
# pixels at 100% scale. Windows scales these itself on high-DPI displays,
# which is why no scale-125/150/200/400 variants are shipped.
SQUARES = {
    "StoreLogo.png": 50,
    "Square44x44Logo.png": 44,
    "Square71x71Logo.png": 71,
    "Square150x150Logo.png": 150,
    "Square310x310Logo.png": 310,
}

# Target-size variants of the app-list icon, used for the taskbar, the Start
# list and file-type icons. The unplated form is the one Windows draws
# without a coloured backplate behind it; the artwork already has a
# transparent background, so both forms are the same image.
TARGET_SIZES = (16, 24, 32, 48, 256)


def master_mark() -> Image.Image:
    """The largest square rendering of the Copperline mark."""
    image = Image.open(MARK)
    # Pillow reads an icns at whichever size is asked for; the 1024 master is
    # the one the icns was built from.
    image.size = (1024, 1024)
    image.load()
    return image.convert("RGBA")


def square(mark: Image.Image, edge: int) -> Image.Image:
    return mark.resize((edge, edge), Image.LANCZOS)


def wide(width: int, height: int) -> Image.Image:
    """The wordmark centred on a transparent tile of the given size.

    The wide tile is a landscape strip, so it takes the wordmark rather than
    the mark, inset far enough that Windows' own tile padding does not clip
    the outermost letters.
    """
    word = Image.open(WORDMARK).convert("RGBA")
    usable = int(width * 0.86)
    scale = min(usable / word.width, (height * 0.7) / word.height)
    scaled = word.resize(
        (max(1, round(word.width * scale)), max(1, round(word.height * scale))),
        Image.LANCZOS,
    )
    tile = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    tile.paste(
        scaled,
        ((width - scaled.width) // 2, (height - scaled.height) // 2),
        scaled,
    )
    return tile


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    mark = master_mark()

    written = []
    for name, edge in SQUARES.items():
        square(mark, edge).save(OUT / name)
        written.append(name)

    for size in TARGET_SIZES:
        image = square(mark, size)
        for name in (
            f"Square44x44Logo.targetsize-{size}.png",
            f"Square44x44Logo.targetsize-{size}_altform-unplated.png",
        ):
            image.save(OUT / name)
            written.append(name)

    wide(310, 150).save(OUT / "Wide310x150Logo.png")
    written.append("Wide310x150Logo.png")

    print(f"wrote {len(written)} logo assets to {OUT.relative_to(REPO)}")


if __name__ == "__main__":
    main()
