#!/usr/bin/env python3
"""Fast batch characterizer for vAmigaTS CL-vs-vAmiga divergences (numpy/PIL).

For each <case>.vamiga.raw (716x285 RGB) + <case>.png (716x570 RGBA line-
doubled CL shot) pair under a root, computes and prints (TSV, sorted by
diff% desc):
  diff%  best-shift class  band(TMB/LR)  rel-path
where:
  diff%  = fraction of differing pixels at X_SHIFT=0, by the same colour
           matching rule as tools/vamigats-compare.py
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

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vamigats_ref as vref  # noqa: E402

TABLE = vref.decoders()

W, H = 716, 285
Y = 2  # va row y maps to cl line-doubled row 2*(y-2)

def load(png, raw):
    """A Copperline screenshot and a vAmiga dump, aligned row for row.

    Returns (cl8, va_raw, va8, member, inside):
      cl8     the screenshot's 8-bit guns
      va_raw  the dump's bytes as stored
      va8     the dump mapped back to the nearest Copperline guns (for
              colour-keyed detectors only, never for differences)
      member  member[c, v]: dump value v is acceptable for Copperline gun c
              under the encoding that fits this dump best
      inside  the dump's valid box; everything vAmiga filled with its
              checkerboard (outside a script's cutout, below an NTSC
              frame) is left out of every count
    """
    ref = open(raw, 'rb').read()
    if len(ref) != W*H*3:
        return None
    im = np.asarray(Image.open(png).convert('RGB'))
    if im.shape[:2] != (H*2, W):
        return None
    cl8 = im[0::2]  # 285 rows (line-doubled -> take even)
    shot = [cl8[y].tobytes() for y in range(H)]
    cl_rows, va_rows, (x1, y1, x2, y2) = vref.aligned_rows(shot, ref)
    if not cl_rows:
        return None
    name, _ = vref.best_decoder(cl_rows, va_rows, TABLE)
    decoder = TABLE[name]
    member = np.frombuffer(decoder.member, dtype=np.uint8).reshape(256, 256) != 0
    inverse = np.frombuffer(decoder.inverse, dtype=np.uint8)
    va_raw = np.frombuffer(ref, dtype=np.uint8).reshape(H, W, 3)
    # align va rows Y..H-1 with cl rows 0..H-1-Y
    va_raw, cl8 = va_raw[Y:H], cl8[0:H-Y]
    inside = np.zeros((H-Y, W), dtype=bool)
    inside[y1-Y:y2-Y, x1:x2] = True
    return cl8, va_raw, inverse[va_raw], member, inside

def diff_mask(cl8, va_raw, member, inside, sx):
    """Pixels of the valid box where dump x and screenshot x + sx differ.

    A pixel matches when each gun's dump value is one the decoder accepts
    for the Copperline value, which is the rule vamigats-compare.py scores
    by. Comparing decoded colours instead would call a dark Copperline
    shade that the dump encoding crushes to black a difference. A column
    the shift pushes off the screenshot differs.
    """
    same = np.zeros(inside.shape, dtype=bool)
    if sx >= 0:
        same[:, :W-sx] = member[cl8[:, sx:], va_raw[:, :W-sx]].all(axis=2)
    else:
        same[:, -sx:] = member[cl8[:, :W+sx], va_raw[:, -sx:]].all(axis=2)
    return inside & ~same

CLI_BLUE = np.array([0, 85, 170])  # AmigaDOS 1.3 CLI background (boot-stuck)

def analyze(png, raw):
    L = load(png, raw)
    if L is None:
        return None
    cl8, va_raw, va8, member, inside = L
    R = inside.shape[0]
    total = int(inside.sum())
    # Boot-stuck detector: CL is the AmigaDOS 1.3 CLI (the test never ran /
    # hung), while vAmiga shows the test output. These are FUNCTIONAL/boot
    # failures, not rendering bugs, and inflate the divergence numbers.
    cl_blue = (np.abs(cl8.astype(int) - CLI_BLUE) < 24).all(2)[inside].mean()
    va_blue = (np.abs(va8.astype(int) - CLI_BLUE) < 24).all(2)[inside].mean()
    shifts = [-6, -4, -3, -2, -1, 0, 1, 2, 3, 4, 6]
    # per-row diff for every shift -> (nshift, R)
    masks = [diff_mask(cl8, va_raw, member, inside, sx) for sx in shifts]
    perrow = np.stack([mask.sum(axis=1) for mask in masks])
    zero_i = shifts.index(0)
    m0 = masks[zero_i]
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
    # content direction: a pixel is "on" when it could not pair with black
    # on the other side under the same matching rule
    va_on = ~member[0, va_raw].all(axis=2) & m0
    cl_on = ~member[cl8, 0].all(axis=2) & m0
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
