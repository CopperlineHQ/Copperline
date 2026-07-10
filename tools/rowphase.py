#!/usr/bin/env python3
"""Content-anchored per-row (beam-line) classification for cross-emulator
screenshot comparison, with the calibrated vertical alignment applied.

Why this exists: comparing Copperline shots against vAmiga raws (or FS-UAE
screenshots) by hand invites two recurring, hard-to-spot errors that have
each produced false "one beam line early/late" verdicts:

  1. Copperline raw shots are LINE-DOUBLED (716x570): one beam line is two
     PNG rows. Reading PNG rows as beam lines fabricates 2x phase errors.
  2. Copperline's framebuffer row 0 sits two beam lines above vAmiga's
     regression cutout (Y_SHIFT = 2, see tools/vamigats-compare.py). The
     naive "vAmiga row y == Copperline row 2y" mapping fabricates a
     phantom 2-line phase shift.

This tool reduces each input to NATIVE beam rows, classifies every row as
content ('P') or background ('.'), and reports:

  - the content anchor (first 'P' row) per file,
  - the class run-length string (periodic raster effects read directly
    off it, e.g. cc7's 3-painted+1-dark cell cycle is "P3 .1 P3 .1 ..."),
  - with --period N, the phase histogram of '.' rows relative to the
    anchor (mod N) - an alignment-independent phase measure,
  - with two files, a per-row class comparison over the aligned overlap.

Inputs (auto-detected):
  *.vamiga.raw            716x285 RGB, native rows (tools/vamiga-ref.sh)
  *.png                   line-doubled shot: Copperline raw screenshot
                          (COPPERLINE_SHOT_RAW=1 COPPERLINE_HCENTER=0,
                          716x570) or an FS-UAE full-frame screenshot.
                          Use --native for a PNG that is not line-doubled.

Alignment for two files (choose with --align, default picks by type):
  calibrated   vAmiga row y == Copperline native row y - 2 (Y_SHIFT). The
               photo-calibrated absolute mapping; only valid for a
               Copperline-vs-vAmiga pair of the same emulated moment.
  anchor       align the two content anchors. Use for FS-UAE shots or
               captures of different moments of a static effect, where
               the absolute crop offset is unknown.

Usage:
  tools/rowphase.py CL.png                          # single-file report
  tools/rowphase.py CL.png case.vamiga.raw          # calibrated compare
  tools/rowphase.py fsuae.png case.vamiga.raw --align anchor --period 4
  tools/rowphase.py CL.png VA.raw --rows 60:120     # limit report range

Exit status: 0 = no class mismatch in the compared overlap (or single
file), 1 = mismatch, 2 = usage/input error.
"""

import struct
import sys
import zlib

# Calibrated vertical anchor between a Copperline raw shot and a vAmiga
# regression raw: Copperline's framebuffer row 0 sits two beam lines above
# vAmiga's cutout start. Keep in lockstep with tools/vamigats-compare.py.
Y_SHIFT = 2

VA_W, VA_H = 716, 285

# A row is "content" when at least this many pixels differ from the
# frame's background colour. Low enough to catch a single sprite bar,
# high enough to ignore capture-edge noise.
CONTENT_PIXEL_THRESHOLD = 8

# Channel quantization difference between the sources (Copperline expands
# 4-bit channels with x17, vAmiga v4.4 with x16): treat pixels within this
# per-channel distance as equal to background.
BG_CHANNEL_TOLERANCE = 24


def read_png_rgb(path):
    with open(path, 'rb') as f:
        data = f.read()
    if data[:8] != b'\x89PNG\r\n\x1a\n':
        raise ValueError(f"{path}: not a PNG")
    pos, idat, w, h, ct = 8, b'', None, None, None
    while pos < len(data):
        ln, typ = struct.unpack('>I4s', data[pos:pos+8])
        chunk = data[pos+8:pos+8+ln]
        if typ == b'IHDR':
            w, h, _bd, ct = struct.unpack('>IIBB', chunk[:10])
        elif typ == b'IDAT':
            idat += chunk
        pos += 12 + ln
    raw = zlib.decompress(idat)
    ch = {0: 1, 2: 3, 6: 4}[ct]
    stride = w * ch
    out = bytearray(w * h * ch)
    prev = bytearray(stride)
    p = 0
    for y in range(h):
        filt = raw[p]; p += 1
        line = bytearray(raw[p:p+stride]); p += stride
        if filt == 1:
            for i in range(ch, stride):
                line[i] = (line[i] + line[i-ch]) & 0xFF
        elif filt == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif filt == 3:
            for i in range(stride):
                a = line[i-ch] if i >= ch else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif filt == 4:
            for i in range(stride):
                a = line[i-ch] if i >= ch else 0
                b = prev[i]
                c = prev[i-ch] if i >= ch else 0
                pp = a + b - c
                pa, pb, pc = abs(pp-a), abs(pp-b), abs(pp-c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        out[y*stride:(y+1)*stride] = line
        prev = line
    return bytes(out), w, h, ch


class Frame:
    """A capture reduced to native beam rows of (r, g, b) pixels."""

    def __init__(self, path, native_override=None):
        self.path = path
        if path.endswith('.raw'):
            data = open(path, 'rb').read()
            if len(data) != VA_W * VA_H * 3:
                raise ValueError(
                    f"{path}: {len(data)} bytes, expected 716x285 RGB "
                    f"({VA_W*VA_H*3})")
            self.kind = 'vamiga'
            self.w = VA_W
            self.rows = [data[y*VA_W*3:(y+1)*VA_W*3] for y in range(VA_H)]
            self.ch = 3
        else:
            px, w, h, ch = read_png_rgb(path)
            doubled = not native_override if native_override is not None \
                else h % 2 == 0
            self.kind = 'copperline' if (w, h) == (VA_W, VA_H * 2) \
                else 'png'
            self.w = w
            self.ch = ch
            step = 2 if doubled else 1
            self.rows = [px[y*w*ch:(y+1)*w*ch] for y in range(0, h, step)]
        self.classes = None
        self.anchor = None

    def pixel(self, row, x):
        base = x * self.ch
        return row[base], row[base+1], row[base+2]

    def background(self):
        # Most common pixel of the frame's outermost rows: raster effects
        # never reach the vertical blanking border, so this is the border
        # colour even when the top content row is row 3-4.
        from collections import Counter
        counts = Counter()
        edges = self.rows[:2] + self.rows[-2:]
        for row in edges:
            for x in range(0, self.w, 4):
                counts[self.pixel(row, x)] += 1
        return counts.most_common(1)[0][0]

    def classify(self):
        if self.classes is not None:
            return
        bg = self.background()
        tol = BG_CHANNEL_TOLERANCE
        self.classes = []
        for row in self.rows:
            nonbg = 0
            for x in range(self.w):
                r, g, b = self.pixel(row, x)
                if (abs(r - bg[0]) > tol or abs(g - bg[1]) > tol
                        or abs(b - bg[2]) > tol):
                    nonbg += 1
                    if nonbg >= CONTENT_PIXEL_THRESHOLD:
                        break
            self.classes.append(
                'P' if nonbg >= CONTENT_PIXEL_THRESHOLD else '.')
        self.anchor = next(
            (i for i, c in enumerate(self.classes) if c == 'P'), None)

    def rle(self, start=0, end=None):
        end = len(self.classes) if end is None else end
        runs = []
        cur, n = None, 0
        for c in self.classes[start:end]:
            if c == cur:
                n += 1
            else:
                if cur is not None:
                    runs.append(f"{cur}{n}")
                cur, n = c, 1
        if cur is not None:
            runs.append(f"{cur}{n}")
        return ' '.join(runs)

    def phase_histogram(self, period):
        """Offsets (mod period) of background rows BETWEEN content rows,
        relative to the content anchor: interior '.' rows are the raster
        effect's dark lines; border rows above/below are excluded."""
        if self.anchor is None:
            return {}
        last_content = max(i for i, c in enumerate(self.classes)
                           if c == 'P')
        hist = {}
        for i in range(self.anchor, last_content):
            if self.classes[i] == '.':
                off = (i - self.anchor) % period
                hist[off] = hist.get(off, 0) + 1
        return hist


def report_single(frame, period, row_range):
    frame.classify()
    print(f"{frame.path}")
    print(f"  type: {frame.kind}, {len(frame.rows)} native rows x "
          f"{frame.w} px")
    if frame.anchor is None:
        print("  no content rows found")
        return
    print(f"  content anchor (first non-background row): "
          f"native row {frame.anchor}")
    start, end = row_range if row_range else (0, len(frame.classes))
    print(f"  row classes [{start}:{end}]: {frame.rle(start, end)}")
    if period:
        hist = frame.phase_histogram(period)
        pretty = ', '.join(f"+{k}: {v}" for k, v in sorted(hist.items()))
        print(f"  interior dark-row phase vs anchor (mod {period}): "
              f"{pretty or 'none'}")


def aligned_pairs(a, b, align):
    """Yield (row_a, row_b) index pairs over the comparable overlap."""
    if align == 'calibrated':
        # vAmiga row y == Copperline native row y - Y_SHIFT. Orient so
        # `a` is the Copperline side.
        if a.kind == 'vamiga':
            a, b = b, a
        for y in range(Y_SHIFT, len(b.rows)):
            n = y - Y_SHIFT
            if n < len(a.rows):
                yield n, y
    else:  # anchor
        delta = b.anchor - a.anchor
        for i in range(len(a.rows)):
            j = i + delta
            if 0 <= j < len(b.rows):
                yield i, j


def report_pair(a, b, align, period, row_range):
    a.classify()
    b.classify()
    if align == 'calibrated' and a.kind == 'vamiga':
        a, b = b, a
    for f in (a, b):
        report_single(f, period, row_range)
        print()
    if align == 'calibrated':
        print(f"compare (calibrated: vAmiga row y == {a.path} native row "
              f"y-{Y_SHIFT}, per tools/vamigats-compare.py):")
    else:
        print(f"compare (anchor-aligned: row {a.anchor} of {a.path} == "
              f"row {b.anchor} of {b.path}):")
    mismatches = []
    compared = 0
    for i, j in aligned_pairs(a, b, align):
        if row_range and not (row_range[0] <= i < row_range[1]):
            continue
        compared += 1
        if a.classes[i] != b.classes[j]:
            mismatches.append((i, j))
    print(f"  {compared} row pairs compared, {len(mismatches)} class "
          f"mismatches")
    for i, j in mismatches[:20]:
        print(f"    row {i} ({a.classes[i]}) vs row {j} "
              f"({b.classes[j]})")
    if len(mismatches) > 20:
        print(f"    ... {len(mismatches) - 20} more")
    if period:
        ha = a.phase_histogram(period)
        hb = b.phase_histogram(period)
        verdict = 'MATCH' if ha == hb else 'DIFFER'
        print(f"  anchor-relative dark-row phase (mod {period}): "
              f"{verdict}")
        if ha != hb:
            print(f"    {a.path}: {sorted(ha.items())}")
            print(f"    {b.path}: {sorted(hb.items())}")
        return 0 if not mismatches and ha == hb else 1
    return 0 if not mismatches else 1


def main():
    args = sys.argv[1:]
    files, align, period, row_range, native = [], None, None, None, False
    i = 0
    while i < len(args):
        a = args[i]
        if a == '--align':
            align = args[i+1]; i += 2
        elif a.startswith('--align='):
            align = a.split('=', 1)[1]; i += 1
        elif a == '--period':
            period = int(args[i+1]); i += 2
        elif a.startswith('--period='):
            period = int(a.split('=', 1)[1]); i += 1
        elif a == '--rows':
            lo, hi = args[i+1].split(':'); row_range = (int(lo), int(hi))
            i += 2
        elif a.startswith('--rows='):
            lo, hi = a.split('=', 1)[1].split(':')
            row_range = (int(lo), int(hi)); i += 1
        elif a == '--native':
            native = True; i += 1
        elif a in ('-h', '--help'):
            print(__doc__)
            return 0
        elif a.startswith('-'):
            print(f"unknown option {a}", file=sys.stderr)
            return 2
        else:
            files.append(a); i += 1

    if not files or len(files) > 2:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        frames = [Frame(f, native_override=native or None) for f in files]
    except (ValueError, OSError, KeyError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    if len(frames) == 1:
        report_single(frames[0], period, row_range)
        return 0

    if align is None:
        kinds = {f.kind for f in frames}
        align = 'calibrated' if kinds == {'copperline', 'vamiga'} \
            else 'anchor'
    if align not in ('calibrated', 'anchor'):
        print(f"unknown --align {align}", file=sys.stderr)
        return 2
    if align == 'calibrated':
        kinds = {f.kind for f in frames}
        if kinds != {'copperline', 'vamiga'}:
            print("calibrated alignment needs one Copperline 716x570 PNG "
                  "and one .vamiga.raw; use --align anchor otherwise",
                  file=sys.stderr)
            return 2
    return report_pair(frames[0], frames[1], align, period, row_range)


if __name__ == '__main__':
    sys.exit(main())
