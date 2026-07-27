#!/usr/bin/env python3
"""Render CL png vs VA raw as a stacked viewable PNG: VA | CL | diff."""
import sys, struct, zlib, os

W, H = 716, 285
Y_SHIFT, X_SHIFT = 2, 0

def read_png_rgb(path):
    data = open(path, 'rb').read()
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
    ch = {0:1,2:3,6:4}[ct]; stride = w*ch
    out = bytearray(w*h*ch); prev = bytearray(stride); p = 0
    for y in range(h):
        filt = raw[p]; p += 1
        line = bytearray(raw[p:p+stride]); p += stride
        if filt == 1:
            for i in range(ch, stride): line[i] = (line[i]+line[i-ch]) & 0xFF
        elif filt == 2:
            for i in range(stride): line[i] = (line[i]+prev[i]) & 0xFF
        elif filt == 3:
            for i in range(stride):
                a = line[i-ch] if i>=ch else 0
                line[i] = (line[i]+((a+prev[i])>>1)) & 0xFF
        elif filt == 4:
            for i in range(stride):
                a = line[i-ch] if i>=ch else 0; b = prev[i]; c = prev[i-ch] if i>=ch else 0
                pp=a+b-c; pa,pb,pc=abs(pp-a),abs(pp-b),abs(pp-c)
                pr = a if (pa<=pb and pa<=pc) else (b if pb<=pc else c)
                line[i]=(line[i]+pr)&0xFF
        out[y*stride:(y+1)*stride]=line; prev=line
    return bytes(out), w, h, ch

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

def nib_cl(v): return (v+8)//17
def nib_va(v): return min(15,(v+8)//16)

def main():
    case = sys.argv[1]  # dir containing <stem>.png and <stem>.vamiga.raw
    outpath = sys.argv[2] if len(sys.argv)>2 else '/tmp/diffview.png'
    stem = sys.argv[3] if len(sys.argv)>3 else os.path.basename(case.rstrip('/'))
    png = os.path.join(case, stem+'.png')
    raw = os.path.join(case, stem+'.vamiga.raw')
    ref = open(raw,'rb').read()
    if len(ref) != W*H*3:
        sys.exit(f"error: {raw}: expected {W*H*3} bytes ({W}x{H} RGB), got {len(ref)}")
    img, w, h, ch = read_png_rgb(png)
    if w != W or h < 2*(H - Y_SHIFT):
        sys.exit(f"error: {png}: expected a {W}x{2*H} line-doubled CL shot, got {w}x{h}")
    # Build three panels at full H rows (va at native, cl halved), 3px gap
    gap = 4
    outH = H*3 + gap*2
    out = bytearray(W*outH*3)
    for y in range(Y_SHIFT, H):
        cy = 2*(y-Y_SHIFT)
        for x in range(W):
            # VA
            va = ref[(y*W+x)*3:(y*W+x)*3+3]
            oi = ((y)*W + x)*3
            out[oi:oi+3] = va
            # CL
            ci = (cy*w + x)*ch
            cl = img[ci:ci+3]
            oi2 = ((H+gap+y)*W + x)*3
            out[oi2:oi2+3] = cl
            # diff (nibble)
            d = (abs(nib_cl(img[ci])-nib_va(va[0]))>0 or
                 abs(nib_cl(img[ci+1])-nib_va(va[1]))>0 or
                 abs(nib_cl(img[ci+2])-nib_va(va[2]))>0)
            oi3 = ((2*H+2*gap+y)*W + x)*3
            if d:
                out[oi3:oi3+3] = bytes([255,0,255])
            else:
                g = va[0]//3
                out[oi3:oi3+3] = bytes([g,g,g])
    write_png(outpath, W, outH, bytes(out))
    print(f"wrote {outpath} ({W}x{outH}) VA(top)/CL(mid)/diff(bot)")

if __name__=='__main__':
    main()
