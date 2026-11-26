"""Shared helpers for reading vAmiga regression dumps against Copperline shots.

A vAmiga regression dump (`<case>.vamiga.raw`, or the `<script>.raw` vAmigaTS
ships beside every script) is 716x285 RGB8. Copperline's
COPPERLINE_SHOT_RAW screenshot is the same cutout line-doubled (716x570).
Three things have to be known before the two can be compared:

Alignment. vAmiga's cutout starts at colour clock 0x31 (RegressionTester
X1 = 4 * 0x31), the same beam position as Copperline's framebuffer origin,
so x needs no shift. Copperline's row 0 sits two beam lines above the
cutout start (VBLANK_MAX + 1): dump row y is screenshot row 2 * (y - 2).

The valid box. A script can narrow the dump with `screenshot set cutout`
(the test author masking a region that is not stable), and an NTSC frame
ends above the PAL cutout. vAmiga leaves a grey checkerboard everywhere
outside, which is not picture and must not be scored. The box is
recovered from the dump itself.

The colour encoding. Dumps come in three flavours, depending on the vAmiga
build that recorded them:
  old  a 4-bit gun expanded linearly, n * 16 (occasionally one less)
  ecs  vAmiga's COLOR monitor palette applied to n * 16
  aga  the same palette applied to an 8-bit gun (every AGA dump)
  id   the identity RGB palette (VAMIGA_RGB=1 references)
The COLOR palette linearises with gamma 2.8, truncates to 8 bits and
re-encodes with gamma 2.2, which also crushes the darkest shades to black.
The encoding is not recorded anywhere, so a case is scored under each and
the best fit wins: a wrong decoder moves every coloured pixel.
"""
import struct
import zlib

W, H = 716, 285
Y_SHIFT = 2
# Texture coordinates of the dump's top-left pixel (RegressionTester X1, Y1).
TEX_X1, TEX_Y1 = 196, 26
GAMMA_IN, GAMMA_OUT = 2.8, 2.2
ROW = W * 3


def read_png_rgb(path):
    """Decode an 8-bit grey/RGB/RGBA PNG without third-party modules."""
    with open(path, 'rb') as f:
        data = f.read()
    assert data[:8] == b'\x89PNG\r\n\x1a\n', path
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
        filt = raw[p]
        p += 1
        line = bytearray(raw[p:p+stride])
        p += stride
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


def shot_rows(path):
    """The 285 beam lines of a COPPERLINE_SHOT_RAW screenshot as RGB rows,
    or None when the picture is not the 716x570 raw framebuffer."""
    img, w, h, ch = read_png_rgb(path)
    if (w, h) != (W, H * 2):
        return None
    rows = []
    for y in range(H):
        line = img[2*y*w*ch:(2*y + 1)*w*ch]
        if ch == 4:
            # Drop alpha (a blanked border is written transparent).
            line = b''.join(line[i:i+3] for i in range(0, len(line), 4))
        elif ch == 1:
            line = bytes(b for v in line for b in (v, v, v))
        rows.append(line)
    return rows


def dump_rows(ref):
    return [ref[y*ROW:(y + 1)*ROW] for y in range(H)]


def _checker_row(parity):
    return bytes(v for x in range(W)
                 for v in ((0x22,) * 3 if parity == ((x + TEX_X1) >> 3 & 1) else (0x44,) * 3))


# vAmiga's fill is a two-grey checkerboard with 8-pixel cells. Its rows come
# in two phases; which rows carry which depends on where the fill came from
# (outside a cutout the cells are 8 rows tall, in the part of the texture an
# NTSC frame never draws they are 4), so a row is recognised by its content,
# not by its position.
CHECKER_ROWS = (_checker_row(0), _checker_row(1))


def _run(row, pixels):
    """How many of `pixels` (x positions, in order) match the checkerboard,
    in whichever phase fits this row best."""
    best = 0
    for chk in CHECKER_ROWS:
        n = 0
        for x in pixels:
            if row[x*3:x*3+3] != chk[x*3:x*3+3]:
                break
            n += 1
        best = max(best, n)
    return best


def valid_box(ref):
    """(x1, y1, x2, y2), half-open, of the picture inside a dump: the part
    vAmiga did not fill with its checkerboard."""
    rows = dump_rows(ref)
    live = [y for y in range(H) if rows[y] not in CHECKER_ROWS]
    if not live:
        return 0, 0, 0, 0
    left = right = W
    for y in live:
        left = min(left, _run(rows[y], range(W)))
        if not left:
            break
    for y in live:
        right = min(right, _run(rows[y], range(W - 1, -1, -1)))
        if not right:
            break
    return left, live[0], W - right, live[-1] + 1


def _lin(v):
    return (v / 255.0) ** GAMMA_IN * 255.0


def _gam(i):
    i = max(0, min(255, i))
    return int(round((i / 255.0) ** (1.0 / GAMMA_OUT) * 255.0))


def _nib(c):
    return (c + 8) // 17


def _palette_accept(x):
    # The affine stage carries small cross-gun terms, so the truncation can
    # land one linear step either side; the re-encode rounds.
    out = set()
    for d in (-0.45, 0.0, 0.45):
        g = _gam(int(max(0.0, _lin(x) + d)))
        out.update((g - 1, g, g + 1))
    return out


class Decoder:
    """How one dump encoding relates to Copperline's 8-bit guns.

    table    Copperline gun value -> the dump value it should produce
    member   member[c * 256 + v] is 1 when dump value v is acceptable for c
    inverse  dump value -> the nearest Copperline gun value
    """

    def __init__(self, nominal, accept, tol, step):
        self.table = bytes(nominal(c) for c in range(256))
        member = bytearray(65536)
        for c in range(256):
            for near in range(c - step * tol, c + step * tol + 1, step):
                if 0 <= near <= 255:
                    for v in accept(near):
                        if 0 <= v <= 255:
                            member[c * 256 + v] = 1
        self.member = bytes(member)
        self.inverse = bytes(
            min(range(0, 256, step), key=lambda c: (abs(nominal(c) - v), c))
            for v in range(256))


def decoders(tol=0):
    """The dump encodings, widened by `tol` 4-bit colour steps."""
    return {
        'old': Decoder(lambda c: _nib(c) * 16,
                       lambda c: {_nib(c) * 16 - 1, _nib(c) * 16, _nib(c) * 16 + 1},
                       tol, 17),
        'ecs': Decoder(lambda c: _gam(int(_lin(_nib(c) * 16))),
                       lambda c: _palette_accept(_nib(c) * 16), tol, 17),
        'aga': Decoder(lambda c: _gam(int(_lin(c))), _palette_accept, tol * 17, 1),
        'id': Decoder(lambda c: c, lambda c: {c - 1, c, c + 1}, tol * 17, 1),
    }


def count_diff(cl_rows, va_rows, decoder):
    """Pixels of equally sized row lists that differ under `decoder`."""
    table, member = decoder.table, decoder.member
    diff = 0
    for a, b in zip(cl_rows, va_rows):
        if a.translate(table) == b:
            continue
        for i in range(0, len(a), 3):
            if not (member[a[i] * 256 + b[i]] and member[a[i+1] * 256 + b[i+1]]
                    and member[a[i+2] * 256 + b[i+2]]):
                diff += 1
    return diff


def aligned_rows(shot, ref, x_shift=0):
    """Crop a screenshot and a dump to the dump's valid box and line them
    up. Returns (cl_rows, va_rows, box) with box in dump coordinates;
    `x_shift` pairs dump x with screenshot x + x_shift."""
    x1, y1, x2, y2 = valid_box(ref)
    y1 = max(y1, Y_SHIFT)
    x1 = max(x1, -x_shift)
    x2 = min(x2, W - x_shift)
    if x2 <= x1 or y2 <= y1:
        return [], [], (x1, y1, x1, y1)
    va = dump_rows(ref)
    cl_rows = [shot[y - Y_SHIFT][(x1 + x_shift)*3:(x2 + x_shift)*3] for y in range(y1, y2)]
    va_rows = [va[y][x1*3:x2*3] for y in range(y1, y2)]
    return cl_rows, va_rows, (x1, y1, x2, y2)


def best_decoder(cl_rows, va_rows, table):
    """(name, differing pixels) of the encoding that fits best."""
    counts = {name: count_diff(cl_rows, va_rows, dec) for name, dec in table.items()}
    name = min(counts, key=lambda k: counts[k])
    return name, counts[name]
