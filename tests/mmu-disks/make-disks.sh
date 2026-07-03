#!/bin/sh -e
# Build the two MMU test floppies used by the mmu_library_boot_* integration
# tests (tests/image_regression.rs) and by the mmu-test*.toml configs:
#
#   mmu-test.adf   boot disk: a Workbench 3.1 disk with s/startup-sequence
#                  replaced by the staged marker script in this directory
#   mmu-libs.adf   data disk: mmu.library + CPU libraries, MuScan, MuForce
#                  (from Aminet MMULib, by Thomas Richter) and the committed
#                  lawbreaker binary (built from lawbreaker.c with
#                  m68k-amigaos-gcc -noixemul -O)
#
# Needs: xdftool (pip install amitools), lha, curl, and a Workbench 3.1
# disk image. Usage:
#
#   tests/mmu-disks/make-disks.sh "/path/to/WB-3_1.ADF" [output-dir]
#
# The output directory defaults to the current directory; point it at your
# test-assets/ directory (see tests/README.md).

WB_DISK="$1"
OUT="${2:-.}"
HERE="$(cd "$(dirname "$0")" && pwd)"

if [ -z "$WB_DISK" ] || [ ! -f "$WB_DISK" ]; then
    echo "usage: $0 /path/to/WB-3_1.ADF [output-dir]" >&2
    exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "== fetching MMULib from Aminet"
curl -fsSL -o "$WORK/MMULib.lha" https://aminet.net/util/libs/MMULib.lha
(cd "$WORK" && lha xq MMULib.lha)

echo "== building mmu-libs.adf"
LIBS="$OUT/mmu-libs.adf"
rm -f "$LIBS"
xdftool "$LIBS" create + format "MMULib" ffs
xdftool "$LIBS" makedir Libs
for f in mmu.library 680x0.library 68040.library 68060.library 68030.library 68020.library; do
    xdftool "$LIBS" write "$WORK/MMULib/Libs/$f" "Libs/$f"
done
xdftool "$LIBS" write "$WORK/MMULib/MuTools/MuScan" MuScan
xdftool "$LIBS" write "$WORK/MMULib/MuTools/MuForce" MuForce
xdftool "$LIBS" write "$WORK/MMULib/Install/TestCPU" TestCPU
xdftool "$LIBS" write "$HERE/lawbreaker" lawbreaker

echo "== building mmu-test.adf"
BOOT="$OUT/mmu-test.adf"
cp "$WB_DISK" "$BOOT"
xdftool "$BOOT" delete "s/startup-sequence"
xdftool "$BOOT" write "$HERE/startup-sequence" "s/startup-sequence"

echo "== done: $BOOT $LIBS"
