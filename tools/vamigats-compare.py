#!/usr/bin/env python3
"""Compare Copperline raw screenshots against vAmiga reference dumps.

Walks a COPPERLINE_VAMIGATS_OUT tree: for each <case>.vamiga.raw
(716x285 RGB) find <case>.png (716x570 line-doubled RGBA raw shot),
halve its rows, and report the fraction of differing pixels.
"""
import os, sys, struct, zlib

W, H = 716, 285

def read_png_rgb(path):
    with open(path, 'rb') as f:
        data = f.read()
    assert data[:8] == b'\x89PNG\r\n\x1a\n', path
    pos, idat, w, h, ct, bd = 8, b'', None, None, None, None
    while pos < len(data):
        ln, typ = struct.unpack('>I4s', data[pos:pos+8])
        chunk = data[pos+8:pos+8+ln]
        if typ == b'IHDR':
            w, h, bd, ct = struct.unpack('>IIBB', chunk[:10])
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
            for i in range(ch, stride): line[i] = (line[i] + line[i-ch]) & 0xFF
        elif filt == 2:
            for i in range(stride): line[i] = (line[i] + prev[i]) & 0xFF
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

# Copperline expands 4-bit channels with x17 (0xF -> 255); vAmiga v4.4
# with x16 (0xF -> 240, with occasional +-1 from its texture path), and
# its cutout sits 16 pixels right of Copperline's framebuffer origin.
X_SHIFT = 16
# Copperline's framebuffer row 0 sits two beam lines above vAmiga's
# cutout start (VBLANK_MAX + 1).
Y_SHIFT = 2

def nib_cl(v):
    return (v + 8) // 17

def nib_va(v):
    return min(15, (v + 8) // 16)

def compare(case_png, case_raw, tol):
    ref = open(case_raw, 'rb').read()
    if len(ref) != W*H*3:
        return None, f"bad raw size {len(ref)}"
    img, w, h, ch = read_png_rgb(case_png)
    if (w, h) != (W, H*2):
        return None, f"bad png size {w}x{h}"
    diff = 0
    total = 0
    for y in range(Y_SHIFT, H):
        cy = 2 * (y - Y_SHIFT)
        row = img[cy*w*ch:cy*w*ch + w*ch]
        rr = ref[y*W*3:(y+1)*W*3]
        for x in range(W - X_SHIFT):
            cx = x + X_SHIFT   # Copperline x for vAmiga x
            total += 1
            if (abs(nib_cl(row[cx*ch]) - nib_va(rr[x*3])) > tol or
                abs(nib_cl(row[cx*ch+1]) - nib_va(rr[x*3+1])) > tol or
                abs(nib_cl(row[cx*ch+2]) - nib_va(rr[x*3+2])) > tol):
                diff += 1
    return diff / total, None

def main():
    root = sys.argv[1]
    tol = int(sys.argv[2]) if len(sys.argv) > 2 else 0
    results = []
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            if f.endswith('.vamiga.raw'):
                stem = f[:-len('.vamiga.raw')]
                png = os.path.join(dirpath, stem + '.png')
                raw = os.path.join(dirpath, f)
                if not os.path.exists(png):
                    print(f"MISSING-PNG {os.path.relpath(raw, root)}")
                    continue
                frac, err = compare(png, raw, tol)
                rel = os.path.relpath(os.path.join(dirpath, stem), root)
                if err:
                    print(f"ERROR {rel}: {err}")
                else:
                    results.append((frac, rel))
    results.sort(reverse=True)
    for frac, rel in results:
        print(f"{frac*100:8.3f}%  {rel}")

if __name__ == '__main__':
    main()
