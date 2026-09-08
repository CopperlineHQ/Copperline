#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Replace Hack Regular's zero with the upstream forward-slash design."""

import argparse
import hashlib
from pathlib import Path

from fontTools.pens.cu2quPen import Cu2QuPen
from fontTools.pens.pointPen import PointToSegmentPen
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont
from fontTools.ufoLib.glifLib import readGlyphFromString


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "base", type=Path, help="epaint_default_fonts 0.35.0 Hack-Regular.ttf"
    )
    args = parser.parse_args()
    expected = "15f55cc0c85a2988d2b4b3a8cdb5d77fdfbaf319e1bb5309d725db9818fb7125"
    if hashlib.sha256(args.base.read_bytes()).hexdigest() != expected:
        parser.error("base font does not match epaint_default_fonts 0.35.0")

    directory = Path(__file__).resolve().parent
    font = TTFont(args.base, recalcTimestamp=False)
    zero = font.getBestCmap()[ord("0")]
    original_glyphs = {
        name: font["glyf"][name].compile(font["glyf"])
        for name in font.getGlyphOrder()
    }
    original_metrics = font["hmtx"].metrics.copy()
    pen = TTGlyphPen(None)
    readGlyphFromString(
        (directory / "zero.glif").read_text(),
        pointPen=PointToSegmentPen(Cu2QuPen(pen, max_err=1.0)),
    )
    font["glyf"][zero] = pen.glyph()
    # Distinguish the derivative in font viewers without changing its metrics.
    names = {
        1: "Hack Slash",
        3: "Copperline: Hack Slash: 3.003",
        4: "Hack Slash Regular",
        6: "HackSlash-Regular",
    }
    for record in font["name"].names:
        if record.nameID in names:
            record.string = names[record.nameID].encode(record.getEncoding())
    output = directory / "HackSlash-Regular.ttf"
    font.save(output)

    rebuilt = TTFont(output)
    changed = {
        name
        for name in rebuilt.getGlyphOrder()
        if rebuilt["glyf"][name].compile(rebuilt["glyf"]) != original_glyphs[name]
    }
    if changed != {zero} or rebuilt["hmtx"].metrics != original_metrics:
        raise RuntimeError("rebuild changed glyphs or spacing beyond the zero outline")
    print(f"{output.name}: {hashlib.sha256(output.read_bytes()).hexdigest()}")
    print(f"Only {zero} changed; all {len(original_glyphs)} advances are preserved.")


if __name__ == "__main__":
    main()
