#!/bin/sh
# Run the same ADF on Copperline and vAmiga (headless) and report how much the
# two framebuffers differ, with a stacked VA/CL/diff PNG for eyeballing.
#
# Use it to investigate any OCS/ECS hardware behaviour: point it at a vAmigaTS
# case ADF, or at a custom probe you built with timing-test/build.sh (see
# timing-test/README.md for the boot.asm + test.asm + make_adf.py template).
# vAmiga 5.0's A1200_2MB setup provides an AGA reference.
#
# Usage:
#   tools/adf-compare.sh <adf> [seconds] [setup] [out-dir]
#
#   setup  A500_OCS_1MB (default) | A500_ECS_1MB | A500_PLUS_1MB |
#          A1000_OCS_1MB | A1200_2MB
#
# Env: VAHEADLESS overrides the vAmiga binary; COPPERLINE overrides the
# Copperline binary (default target/release/copperline).
set -e
cd "$(dirname "$0")/.."
REPO="$(pwd)"

ADF="$1"
SECS="${2:-9}"
SETUP="${3:-A500_OCS_1MB}"
OUTDIR="${4:-/tmp/adf-compare}"

COPPERLINE="${COPPERLINE:-$REPO/target/release/copperline}"
KICK="$REPO/test-assets/kick13.rom"

if [ -z "$ADF" ] || [ ! -f "$ADF" ]; then
    echo "usage: tools/adf-compare.sh <adf> [seconds] [setup] [out-dir]" >&2
    exit 2
fi
[ -x "$COPPERLINE" ] || { echo "error: build Copperline first (cargo build --release)" >&2; exit 1; }

# Map the vAmiga setup name to the equivalent Copperline machine.
case "$SETUP" in
    A1200_2MB)
        CHIPSET='revision = "AGA"'; CPU="68EC020"; CHIP="2M"; SLOW="0" ;;
    A500_ECS_1MB)
        CHIPSET='revision = "ECS"
agnus = "8372A"
denise = "OCS"'; CPU="68000"; CHIP="512K"; SLOW="512K" ;;
    A500_PLUS_1MB)
        CHIPSET='revision = "ECS"
agnus = "8375"
denise = "ECS"'; CPU="68000"; CHIP="512K"; SLOW="512K" ;;
    *)
        CHIPSET='revision = "OCS"'; CPU="68000"; CHIP="512K"; SLOW="512K" ;;
esac

stem="$(basename "$ADF")"; stem="${stem%.*}"
casedir="$OUTDIR/$stem"
mkdir -p "$casedir"

# --- Copperline side: raw 716x570 line-doubled shot, HCENTER off for alignment.
cfg="$casedir/${stem}.toml"
cat > "$cfg" <<EOF
rom = "$KICK"
[display]
overscan = "full"
[emulation]
speed = "turbo"
[cpu]
model = "$CPU"
fpu = false
[memory]
chip = "$CHIP"
fast = "0"
slow = "$SLOW"
[chipset]
$CHIPSET
video = "PAL"
[floppy.df0]
path = "$ADF"
write_protected = true
EOF

COPPERLINE_HCENTER=0 COPPERLINE_SHOT_RAW=1 "$COPPERLINE" \
    --config "$cfg" --noaudio --screenshot-after "$SECS" "$casedir/${stem}.png" >/dev/null 2>&1

# --- vAmiga side: 716x285 raw reference.
VAHEADLESS="$VAHEADLESS" tools/vamiga-ref.sh "$ADF" "$SECS" "$SETUP" \
    "$casedir/${stem}.vamiga.raw" "$KICK" >/dev/null

# --- Compare + visualise.
echo "== $stem ($SETUP, ${SECS}s) =="
python3 tools/vamigats-compare.py "$OUTDIR" 2>/dev/null | grep -F "/$stem" || \
    python3 tools/vamigats-compare.py "$casedir" 2>/dev/null
python3 tools/vamigats-diffview.py "$casedir" "$casedir/${stem}.diff.png" >/dev/null 2>&1 \
    && echo "diffview: $casedir/${stem}.diff.png  (VA top / CL mid / diff bottom)"
