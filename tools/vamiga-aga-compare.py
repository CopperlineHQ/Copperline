#!/usr/bin/env python3
"""Compare one Copperline raw screenshot against a vAmiga AGA reference dump.

`tools/vamigats-compare.py` quantizes both sides back to 4-bit guns and
compares them component by component. That works for the OCS/ECS vAmigaTS
cases, but not on vAmiga 5.0's AGA setup: vAmiga runs a YUV
brightness/contrast/saturation monitor model over the palette, so COLOR00
`$008` leaves its framebuffer as `$000072` where Copperline replicates the
nibble to `$000088`, and a component compare scores an exact structural match
as a difference.

This comparer is transform-invariant instead. It requires only that the
colour correspondence between the two frames is a consistent bijection: every
vAmiga colour must map to one Copperline colour across the whole frame.
Pixels off that majority mapping are the real mismatches, so a probe whose
render agrees region for region scores 0.000% however the two emulators
expand the guns.

Inputs:
  <vamiga.raw>  716x285 RGB, from tools/vamiga-ref.sh
  <copperline.png>  716x570 line-doubled raw shot, captured with
                    COPPERLINE_HCENTER=0 COPPERLINE_SHOT_RAW=1

vAmiga's cutout starts two beam lines below Copperline's framebuffer row 0
(the same Y_SHIFT vamigats-compare.py applies), so the alignment search
defaults to a small window around that. Every candidate offset is scored over
the whole overlap - no subsampling, which on a probe's periodic bar pattern
would happily pick the wrong alignment.
"""
import sys
import struct
import zlib

W, H = 716, 285


def read_png_rgb(path):
    """Decode a PNG to packed RGB bytes; returns (data, width, height, channels)."""
    with open(path, 'rb') as f:
        data = f.read()
    assert data[:8] == b'\x89PNG\r\n\x1a\n', path
    pos, idat, w, h, ct, bd = 8, b'', None, None, None, None
    while pos < len(data):
        ln, typ = struct.unpack('>I4s', data[pos:pos + 8])
        chunk = data[pos + 8:pos + 8 + ln]
        if typ == b'IHDR':
            w, h, bd, ct = struct.unpack('>IIBB', chunk[:10])
        elif typ == b'IDAT':
            idat += chunk
        pos += 12 + ln
    assert bd == 8, f"{path}: expected 8-bit channels"
    raw = zlib.decompress(idat)
    ch = {0: 1, 2: 3, 6: 4}[ct]
    stride = w * ch
    out = bytearray(w * h * ch)
    prev = bytearray(stride)
    p = 0
    for y in range(h):
        filt = raw[p]
        p += 1
        line = bytearray(raw[p:p + stride])
        p += stride
        if filt == 1:
            for i in range(ch, stride):
                line[i] = (line[i] + line[i - ch]) & 0xFF
        elif filt == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif filt == 3:
            for i in range(stride):
                a = line[i - ch] if i >= ch else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif filt == 4:
            for i in range(stride):
                a = line[i - ch] if i >= ch else 0
                b = prev[i]
                c = prev[i - ch] if i >= ch else 0
                pp = a + b - c
                pa, pb, pc = abs(pp - a), abs(pp - b), abs(pp - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return bytes(out), w, h, ch


def load_vamiga(path):
    raw = open(path, 'rb').read()
    if len(raw) != W * H * 3:
        sys.exit(f"{path}: expected {W*H*3} bytes (716x285 RGB), got {len(raw)}")
    return raw


def load_copperline(path):
    px, w, h, ch = read_png_rgb(path)
    if (w, h) != (W, H * 2):
        sys.exit(f"{path}: expected {W}x{H*2} (line-doubled raw shot), got {w}x{h}")
    out = bytearray()
    for y in range(H):
        row = px[(2 * y) * w * ch:(2 * y + 1) * w * ch]
        for x in range(w):
            out += row[x * ch:x * ch + 3]
    return bytes(out)


def colour_ids(pix):
    """Pack a frame into one id byte per pixel, plus the id -> RGB table."""
    ids = {}
    table = []
    out = bytearray(len(pix) // 3)
    for i in range(len(pix) // 3):
        key = pix[i * 3:i * 3 + 3]
        v = ids.get(key)
        if v is None:
            v = len(table)
            if v > 255:
                sys.exit("more than 256 distinct colours in a frame")
            ids[key] = v
            table.append(key)
        out[i] = v
    return bytes(out), table


def score(vaid, clid, ncl, dy, dx):
    """Mismatch fraction against the majority colour mapping at one offset.

    Counts (vAmiga id, Copperline id) pairs in a flat table, keeps each
    vAmiga id's most frequent partner, and calls every other pixel a
    mismatch. Exact: the whole overlap is scanned, no subsampling.
    """
    counts = [0] * (256 * ncl)
    total = 0
    x0 = max(0, dx)
    x1 = min(W, W + dx)
    for y in range(H):
        cy = y - dy
        if not 0 <= cy < H:
            continue
        vrow = y * W
        crow = cy * W - dx
        total += x1 - x0
        for x in range(x0, x1):
            counts[vaid[vrow + x] * ncl + clid[crow + x]] += 1
    if not total:
        return 1.0, 0, 0, {}, False
    good = 0
    fwd = {}
    for a in range(256):
        base = a * ncl
        row = counts[base:base + ncl]
        best = max(row)
        if best:
            good += best
            fwd[a] = row.index(best)
    injective = len(set(fwd.values())) == len(fwd)
    return (total - good) / total, total - good, total, fwd, injective


def main():
    args = [a for a in sys.argv[1:] if not a.startswith('--')]
    flags = [a for a in sys.argv[1:] if a.startswith('--')]
    if len(args) != 2:
        sys.exit(
            "usage: tools/vamiga-aga-compare.py <vamiga.raw> <copperline.png>\n"
            "       [--dy=A:B] [--dx=A:B] [--verbose]"
        )

    def rng(name, default):
        for f in flags:
            if f.startswith(f'--{name}='):
                lo, hi = f.split('=')[1].split(':')
                return range(int(lo), int(hi) + 1)
        return default

    dyr = rng('dy', range(0, 5))
    dxr = rng('dx', range(-2, 3))
    verbose = '--verbose' in flags

    va = load_vamiga(args[0])
    cl = load_copperline(args[1])
    vaid, vatab = colour_ids(va)
    clid, cltab = colour_ids(cl)
    ncl = len(cltab)

    best = None
    for dy in dyr:
        for dx in dxr:
            frac, bad, total, fwd, injective = score(vaid, clid, ncl, dy, dx)
            if total and (best is None or frac < best[0]):
                best = (frac, bad, total, dy, dx, fwd, injective)
    if best is None:
        sys.exit("no overlap for any offset in the search window")
    frac, bad, total, dy, dx, fwd, injective = best

    print(f"{frac*100:8.3f}%  {bad}/{total} px  dy={dy} dx={dx} "
          f"colours={len(fwd)} bijective={injective}")
    if verbose:
        for a in sorted(fwd):
            print(f"    vAmiga {vatab[a].hex()} -> Copperline {cltab[fwd[a]].hex()}")
    return 0


if __name__ == '__main__':
    sys.exit(main())
