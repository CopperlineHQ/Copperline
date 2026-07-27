#!/usr/bin/env python3
"""Fast batch characterizer for vAmigaTS CL-vs-vAmiga divergences (numpy/PIL).

For each <case>.vamiga.raw (716x285 RGB) + <case>.png (716x570 RGBA line-
doubled CL shot) pair under a root, computes and prints (TSV, sorted by
diff% desc):
  diff%  best-shift class  band(TMB/LR)  rel-path
where:
  diff%  = nibble-quantized fraction of differing pixels at X_SHIFT=0
  class:
    POS(shift+N:a->b) = a small X-shift collapses the diff -> positioning /
                        fetch-origin / scroll bug (Fable: fetch class)
    BLANK-CL          = VA has content where CL is background (missing render)
    EXTRA-CL          = CL has content where VA is background (spurious render)
    CONTENT/COLOR     = same structure, different pixel values -> decode bug
  band = % of differing pixels in Top/Mid/Bottom thirds and Left/Right halves
"""
import os, sys, numpy as np
from PIL import Image

W, H = 716, 285
Y = 2  # va row y maps to cl line-doubled row 2*(y-2)

def load(png, raw):
    ref = np.frombuffer(open(raw, 'rb').read(), dtype=np.uint8)
    if ref.size != W*H*3:
        return None
    va = ref.reshape(H, W, 3).astype(np.int16)
    im = np.asarray(Image.open(png).convert('RGB'))
    if im.shape[:2] != (H*2, W):
        return None
    cl = im[0::2].astype(np.int16)  # 285 rows (line-doubled -> take even)
    # nibble quantize
    van = np.minimum(15, (va + 8)//16)
    cln = (cl + 8)//17
    # align va rows Y..H-1 with cl rows 0..H-1-Y
    va_a = van[Y:H]        # (283,W,3)
    cl_a = cln[0:H-Y]      # (283,W,3)
    return va_a, cl_a

def diff_mask(va_a, cl_a, sx):
    if sx == 0:
        cl_s = cl_a
    else:
        cl_s = np.full_like(cl_a, 255)
        if sx > 0:
            cl_s[:, :W-sx] = cl_a[:, sx:]
        else:
            cl_s[:, -sx:] = cl_a[:, :W+sx]
    return (va_a != cl_s).any(axis=2)

CLI_BLUE = np.array([0, 85, 170])  # AmigaDOS 1.3 CLI background (boot-stuck)

def analyze(png, raw):
    L = load(png, raw)
    if L is None:
        return None
    va_a, cl_a = L
    R = va_a.shape[0]
    total = R * W
    # Boot-stuck detector: CL is the AmigaDOS 1.3 CLI (the test never ran /
    # hung), while vAmiga shows the test output. These are FUNCTIONAL/boot
    # failures, not rendering bugs, and inflate the divergence numbers.
    im = np.asarray(Image.open(png).convert('RGB'))[0::2].reshape(-1, 3)
    va_full = (np.frombuffer(open(raw, 'rb').read(), dtype=np.uint8)
               .reshape(-1, 3))
    cl_blue = (np.abs(im.astype(int) - CLI_BLUE) < 24).all(1).mean()
    va_blue = (np.abs(va_full.astype(int) - CLI_BLUE) < 24).all(1).mean()
    shifts = [-6, -4, -3, -2, -1, 0, 1, 2, 3, 4, 6]
    # per-row diff for every shift -> (nshift, R)
    perrow = np.stack([diff_mask(va_a, cl_a, sx).sum(axis=1) for sx in shifts])
    zero_i = shifts.index(0)
    m0 = diff_mask(va_a, cl_a, 0)
    base = int(m0.sum())
    dpct = base / total * 100
    if dpct < 1.0:
        return dpct, "OK", ""
    pr_base = perrow[zero_i]                 # (R,)
    pr_best_i = perrow.argmin(axis=0)        # (R,)
    pr_best = perrow.min(axis=0)             # (R,)
    # rows that meaningfully diverge
    div = pr_base > (W * 0.05)
    ndiv = int(div.sum())
    # of the diverging rows, how many are "shift-fixable" (a nonzero shift
    # cuts the row diff by >=60%) and what shifts do they want
    fixable = div & (pr_best < pr_base*0.4) & (np.array(shifts)[pr_best_i] != 0)
    nfix = int(fixable.sum())
    want = np.array(shifts)[pr_best_i][fixable]
    # band
    t = [int(m0[:R//3].sum()), int(m0[R//3:2*R//3].sum()), int(m0[2*R//3:].sum())]
    lh = int(m0[:, :W//2].sum()); rh = base - lh
    band = f"T{t[0]*100//base}/M{t[1]*100//base}/B{t[2]*100//base} L{lh*100//base}/R{rh*100//base}"
    # content direction
    va_on = va_a.any(axis=2) & m0
    cl_on = cl_a.any(axis=2) & m0
    va_only = int((va_on & ~cl_on).sum())
    cl_only = int((cl_on & ~va_on).sum())
    both = base - va_only - cl_only
    if cl_blue > 0.5 and va_blue < 0.3:
        cls = "BOOT-STUCK(CL=AmigaDOS CLI)"
    elif ndiv > 0 and nfix >= ndiv*0.5:
        lo, hi = int(want.min()), int(want.max())
        srange = f"{lo:+d}" if lo == hi else f"{lo:+d}..{hi:+d}"
        cls = f"POS(shift{srange};{nfix}/{ndiv}rows)"
    elif va_only > (cl_only+both)*2:
        cls = "BLANK-CL"
    elif cl_only > (va_only+both)*2:
        cls = "EXTRA-CL"
    else:
        cls = "CONTENT/COLOR"
    return dpct, cls, band

def main():
    root = sys.argv[1]
    minpct = float(sys.argv[2]) if len(sys.argv) > 2 else 0.0
    out = []
    for dp, _, files in os.walk(root):
        for f in files:
            if not f.endswith('.vamiga.raw'):
                continue
            stem = f[:-len('.vamiga.raw')]
            png = os.path.join(dp, stem+'.png')
            if not os.path.exists(png):
                continue
            rel = os.path.relpath(os.path.join(dp, stem), root)
            try:
                a = analyze(png, os.path.join(dp, f))
            except Exception as e:
                out.append((-1, 'ERR', str(e)[:30], rel)); continue
            if a is None:
                out.append((-1, 'ERR', 'load', rel)); continue
            dpct, cls, band = a
            if dpct >= minpct:
                out.append((dpct, cls, band, rel))
    out.sort(reverse=True)
    for dpct, cls, band, rel in out:
        print(f"{dpct:7.2f}\t{cls}\t{band}\t{rel}")

if __name__ == '__main__':
    main()
