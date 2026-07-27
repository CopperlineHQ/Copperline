#!/usr/bin/env python3
"""Make a UAE-1ADF extended ADF loadable by vAmiga headless.

vAmiga's EADFFile loader is stricter than WinUAE/FS-UAE and Copperline: it
rejects an extended ADF whose track count is outside [160, 168], or whose
type-0 ("standard") tracks do not carry exactly 11*512*8 = 45056 used bits
(EADFFile.cpp checkIntegrity). Some rippers/exporters pad an 80-cylinder demo
disk out to 83 cylinders with blank cylinders that they store as type-0 tracks
whose used-bit count is the raw track length rather than 45056 -- vAmiga then
fails with "Unsupported standard track size" and cannot run the disk, so there
is no vAmiga reference for that title (e.g. bin-genx.adf, the Gen-X demo).

This rewrites such an image so vAmiga accepts it, without altering any track the
demo actually uses:
  - trailing type-0 tracks beyond index 159 that are entirely blank are dropped
    (down to a standard 160-track / 80-cylinder image);
  - any remaining type-0 track whose used-bit count is not 45056 is retyped to
    type-1 (raw MFM) so vAmiga treats it as a custom track instead of a
    malformed standard one.
The track *data* is untouched; only the track table (count/type) changes.

Usage: tools/ext-adf-to-vamiga.py <in.adf> <out.adf>
"""
import struct
import sys

STD_USED_BITS = 11 * 512 * 8  # 45056: what vAmiga requires of a type-0 track
MIN_TRACKS = 160


def load(path):
    d = open(path, "rb").read()
    if d[:8] != b"UAE-1ADF":
        sys.exit(f"{path}: not a UAE-1ADF extended ADF")
    ntracks = (d[10] << 8) | d[11]
    descs, avails, types, used = [], [], [], []
    for t in range(ntracks):
        base = 12 + 12 * t
        descs.append(bytearray(d[base:base + 12]))
        types.append((d[base + 2] << 8) | d[base + 3])
        avails.append(struct.unpack(">I", d[base + 4:base + 8])[0])
        used.append(struct.unpack(">I", d[base + 8:base + 12])[0])
    data_off = 12 + 12 * ntracks
    offsets, o = [], data_off
    for a in avails:
        offsets.append(o)
        o += a
    return d, ntracks, descs, avails, types, used, offsets


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    src, dst = sys.argv[1], sys.argv[2]
    d, ntracks, descs, avails, types, used, offsets = load(src)

    keep = ntracks
    while keep > MIN_TRACKS:
        t = keep - 1
        chunk = d[offsets[t]:offsets[t] + avails[t]]
        is_blank = not any(chunk)
        is_bad_std = types[t] == 0 and used[t] != STD_USED_BITS
        if is_blank and is_bad_std:
            keep -= 1
        else:
            break

    retyped = 0
    for t in range(keep):
        if types[t] == 0 and used[t] != STD_USED_BITS:
            descs[t][3] = 0x01  # type 0 -> 1 (raw MFM), low byte of the type u16
            retyped += 1

    out = bytearray(b"UAE-1ADF")
    out += struct.pack(">HH", 0, keep)
    for t in range(keep):
        out += descs[t]
    out += d[12 + 12 * ntracks:offsets[keep - 1] + avails[keep - 1]]

    open(dst, "wb").write(out)
    print(f"{src}: {ntracks} tracks -> {dst}: {keep} tracks "
          f"(dropped {ntracks - keep} trailing blank, retyped {retyped})")


if __name__ == "__main__":
    main()
