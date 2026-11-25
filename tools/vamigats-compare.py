#!/usr/bin/env python3
"""Compare Copperline raw screenshots against vAmiga reference dumps.

Walks a COPPERLINE_VAMIGATS_OUT tree: for each <case>.vamiga.raw
(716x285 RGB) find <case>.png (716x570 line-doubled raw shot) and report
the fraction of differing pixels, worst first.

    vamigats-compare.py <out-root> [tol] [--xshift N] [--jobs N] [--decoder]

The dump's valid box and colour encoding are worked out per case (see
tools/vamigats_ref.py): pixels vAmiga filled with its checkerboard -- outside
a script's `screenshot set cutout`, or below an NTSC frame -- are not scored,
and the encoding that fits best is used. `tol` widens the match by that many
4-bit colour steps; `--decoder` appends the chosen encoding to each line.

Horizontal anchor: aligned frames need no x shift. The previous value of 16
was an artifact: the zero-diff cases were blank screens (shift-invariant),
striped test patterns alias modulo their period, and Copperline's line-start
colour writes surface 16 px late (a separate divergence this shift used to
hide). Note: DDFSTRT $30 lores pictures currently sit 2 px right of vAmiga's
- a real placement divergence under investigation; score those cases with
--xshift 2 to isolate it.
"""
import os
import sys
from concurrent.futures import ProcessPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vamigats_ref as vref  # noqa: E402

_TABLE = None


def _init(tol):
    global _TABLE
    _TABLE = vref.decoders(tol)


def compare(job):
    rel, png, raw, x_shift = job
    ref = open(raw, 'rb').read()
    if len(ref) != vref.W * vref.H * 3:
        return rel, None, f"bad raw size {len(ref)}"
    shot = vref.shot_rows(png)
    if shot is None:
        return rel, None, "bad png size"
    cl_rows, va_rows, (x1, y1, x2, y2) = vref.aligned_rows(shot, ref, x_shift)
    total = (x2 - x1) * (y2 - y1)
    if not total:
        return rel, None, "reference holds no picture"
    name, diff = vref.best_decoder(cl_rows, va_rows, _TABLE)
    return rel, (diff / total, name), None


def main():
    args = sys.argv[1:]
    options = {'--xshift': 0, '--jobs': os.cpu_count() or 1}
    show_decoder = '--decoder' in args
    args = [a for a in args if a != '--decoder']
    for name in options:
        for i, a in enumerate(args):
            if a == name or a.startswith(name + '='):
                if '=' in a:
                    options[name] = int(a.split('=', 1)[1])
                    del args[i]
                else:
                    options[name] = int(args[i + 1])
                    del args[i:i + 2]
                break
    if not args:
        sys.exit(__doc__)
    root = args[0]
    tol = int(args[1]) if len(args) > 1 else 0
    jobs = []
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            if f.endswith('.vamiga.raw'):
                stem = f[:-len('.vamiga.raw')]
                png = os.path.join(dirpath, stem + '.png')
                raw = os.path.join(dirpath, f)
                rel = os.path.relpath(os.path.join(dirpath, stem), root)
                if not os.path.exists(png):
                    print(f"MISSING-PNG {os.path.relpath(raw, root)}")
                    continue
                jobs.append((rel, png, raw, options['--xshift']))
    results = []
    with ProcessPoolExecutor(max(1, options['--jobs']), initializer=_init,
                             initargs=(tol,)) as pool:
        for rel, scored, err in pool.map(compare, jobs, chunksize=8):
            if err:
                print(f"ERROR {rel}: {err}")
            else:
                results.append((scored[0], rel, scored[1]))
    results.sort(reverse=True)
    for frac, rel, name in results:
        suffix = f"  {name}" if show_decoder else ""
        print(f"{frac*100:8.3f}%  {rel}{suffix}")


if __name__ == '__main__':
    main()
