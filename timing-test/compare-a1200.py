#!/usr/bin/env python3
# Compare Copperline's A1200 timing-test rows against the FS-UAE A1200 reference.
#
# Reference capture (FS-UAE 3.2.35, core from WinUAE 3300b2), A1200: 68EC020 at
# 14.19 MHz, AGA, 2 MB chip, no slow RAM, KS 3.1 (40.68), PAL -- the same machine
# tt-a1200.toml describes. Run `fs-uae tt-a1200.fs-uae`, which serves the probe's
# serial stream on tcp://127.0.0.1:1234, and read the 32 words off that socket.
#
# Rows 19, 20, 22 and 27 are raw VHPOSR beam positions, not tick counts, so a
# ratio is meaningless for them; rows 0, 1, 9 and 13 probe slow RAM and read the
# 00000000 sentinel on a machine without it.
import os
import subprocess
import sys

FS = [
    0x00000000, 0x00000000, 0x19BB, 0x11A2, 0x1004, 0x1337, 0x3803, 0x0CD0,
    0x377D, 0x00000000, 0x0237, 0x02DD, 0x023C, 0x00000000, 0x0CD3, 0x02DD,
    0x11A6, 0x11A4, 0x0248, 0x0019, 0x0084, 0x119D, 0x000D, 0x2731,
    0x479D, 0x017F, 0x622E, 0x641B, 0x1338, 0x1337, 0x0CD0, 0x04D2,
]

DESC = {
    0: "slowR", 1: "slowW", 2: "chipR", 3: "chipW", 4: "move", 5: "shift",
    6: "mul", 7: "dbra", 8: "frame", 9: "slowRd/f", 10: "cw1024",
    11: "cw/6bpl", 12: "cw/8spr", 13: "dbraSlow", 14: "dbraChip",
    15: "cw/6bpl+8spr", 16: "cw/f", 17: "cw/f+VB", 18: "cw/3bpl",
    19: "VBentry", 20: "SOFTend", 21: "cw/chain", 22: "VBraise",
    23: "blitClr", 24: "blitFill", 25: "blitLine", 26: "fill+3bpl",
    27: "copperPoll", 28: "pair", 29: "pairRAW", 30: "dbraBC", 31: "div/6bpl",
}

RAW = {19, 20, 22, 27}


def copperline_rows():
    out = subprocess.run(
        ["../target/release/copperline", "--config", "tt-a1200.toml",
         "--noaudio", "--screenshot-after", "16", os.devnull],
        capture_output=True, text=True, cwd=sys.path[0] or ".",
    ).stdout
    words = out.replace("\0", "").split()
    return [int(w, 16) for w in words
            if len(w) == 8 and all(c in "0123456789ABCDEFabcdef" for c in w)][:32]


rows = copperline_rows()
if len(rows) < 32:
    print(f"only {len(rows)} rows: {rows}")
    sys.exit(1)

print(f"{'row':>3} {'desc':13} {'CL':>7} {'FS-UAE':>7} {'CL/FS':>6}")
worst = []
for i in range(32):
    ref, got = FS[i], rows[i]
    if i in RAW or ref == 0:
        state = "ok" if got == ref else "DIFF"
        print(f"{i:>3} {DESC[i]:13} {got:>7} {ref:>7} {'(raw)':>6} {state}")
        continue
    ratio = got / ref
    off = abs(ratio - 1)
    flag = "<<<" if off > 0.15 else ("<<" if off > 0.05 else "")
    if off > 0.05:
        worst.append((off, i))
    print(f"{i:>3} {DESC[i]:13} {got:>7} {ref:>7} {ratio:>6.2f} {flag}")

worst.sort(reverse=True)
print("worst:", [f"r{i}({DESC[i]} {o * 100:.0f}%)" for o, i in worst[:10]])
