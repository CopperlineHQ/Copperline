#!/usr/bin/env python3
"""Render CL png vs VA raw as a stacked viewable PNG: VA | CL | diff."""
import sys, struct, zlib, os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vamigats_ref as vref  # noqa: E402

W, H, Y_SHIFT = vref.W, vref.H, vref.Y_SHIFT

def write_png(path, w, h, rgb):
    def chunk(typ, data):
        return struct.pack('>I', len(data)) + typ + data + struct.pack('>I', zlib.crc32(typ+data)&0xffffffff)
    raw = bytearray()
    for y in range(h):
        raw.append(0); raw += rgb[y*w*3:(y+1)*w*3]
    out = b'\x89PNG\r\n\x1a\n'
    out += chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
    out += chunk(b'IDAT', zlib.compress(bytes(raw), 9))
    out += chunk(b'IEND', b'')
    open(path,'wb').write(out)

def main():
    case = sys.argv[1]  # dir containing <stem>.png and <stem>.vamiga.raw
    outpath = sys.argv[2] if len(sys.argv)>2 else '/tmp/diffview.png'
    stem = sys.argv[3] if len(sys.argv)>3 else os.path.basename(case.rstrip('/'))
    png = os.path.join(case, stem+'.png')
    raw = os.path.join(case, stem+'.vamiga.raw')
    ref = open(raw,'rb').read()
    if len(ref) != W*H*3:
        sys.exit(f"error: {raw}: expected {W*H*3} bytes ({W}x{H} RGB), got {len(ref)}")
    shot = vref.shot_rows(png)
    if shot is None:
        sys.exit(f"error: {png}: expected a {W}x{2*H} line-doubled CL shot")
    # The diff panel uses the dump's own valid box and colour encoding
    # (see vamigats_ref.py); what vAmiga filled with its checkerboard is
    # drawn dark blue, not judged.
    table = vref.decoders()
    cl_rows, va_rows, (x1, y1, x2, y2) = vref.aligned_rows(shot, ref)
    name, _ = vref.best_decoder(cl_rows, va_rows, table)
    member = table[name].member
    # Build three panels at full H rows (va at native, cl halved), 3px gap
    gap = 4
    outH = H*3 + gap*2
    out = bytearray(W*outH*3)
    for y in range(Y_SHIFT, H):
        va_row = ref[y*W*3:(y+1)*W*3]
        cl_row = shot[y-Y_SHIFT]
        out[y*W*3:(y+1)*W*3] = va_row
        out[(H+gap+y)*W*3:(H+gap+y+1)*W*3] = cl_row
        judged = y1 <= y < y2
        for x in range(W):
            va = va_row[x*3:x*3+3]
            cl = cl_row[x*3:x*3+3]
            oi3 = ((2*H+2*gap+y)*W + x)*3
            if not (judged and x1 <= x < x2):
                out[oi3:oi3+3] = bytes([0,0,48])
            elif not (member[cl[0]*256 + va[0]] and member[cl[1]*256 + va[1]]
                      and member[cl[2]*256 + va[2]]):
                out[oi3:oi3+3] = bytes([255,0,255])
            else:
                g = va[0]//3
                out[oi3:oi3+3] = bytes([g,g,g])
    write_png(outpath, W, outH, bytes(out))
    print(f"wrote {outpath} ({W}x{outH}) VA(top)/CL(mid)/diff(bot), {name} encoding")

if __name__=='__main__':
    main()
