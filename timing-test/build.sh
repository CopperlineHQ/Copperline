#!/bin/sh
# Assemble the timing test (default) or a named standalone probe and wrap it
# into a bootable ADF.
#
# Needs vasm (the Motorola-syntax m68k assembler, vasmm68k_mot) on PATH or in
# VASM. Get it from http://sun.hasenbraten.de/vasm/ and build with:
#   make CPU=m68k SYNTAX=mot
set -e
cd "$(dirname "$0")"

VASM="${VASM:-vasmm68k_mot}"
MAIN="${1:-test}"
OUT="${1:-timing-test}"
if ! command -v "$VASM" >/dev/null 2>&1; then
    echo "error: vasmm68k_mot not found; set VASM=/path/to/vasmm68k_mot" >&2
    exit 1
fi
if [ ! -f "$MAIN.asm" ]; then
    echo "error: probe source not found: $MAIN.asm" >&2
    exit 1
fi

"$VASM" -Fbin -m68000 -o boot.bin boot.asm
"$VASM" -Fbin -m68000 -o "$MAIN.bin" "$MAIN.asm"
python3 make_adf.py boot.bin "$MAIN.bin" "$OUT.adf"
echo "built $OUT.adf"
