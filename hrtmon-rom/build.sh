#!/bin/sh
# Build Copperline's bundled HRTMon freezer-cartridge image from the
# upstream source (see README.md in this directory).
#
#   ./hrtmon-rom/build.sh          # assemble into hrtmon-rom/build/hrtmon.rom
#   ./hrtmon-rom/build.sh bundle   # ... and copy it to assets/hrtmon/hrtmon.rom
#
# Environment:
#   VASM         vasmm68k_mot binary (default: from PATH)
#   NDK_INCLUDE  NDK assembler includes (exec/*.i, hardware/custom.i,
#                devices/hardblocks.i); default /opt/amiga/m68k-amigaos/ndk-include
#   HRTMON_SRC   an existing checkout of https://github.com/wepl/hrtmon at
#                the pinned commit; cloned into build/ when unset
set -eu

HRTMON_URL=https://github.com/wepl/hrtmon.git
HRTMON_COMMIT=3af8d5105f1e01ecc7961475568826e564995068
# The date the $VER string carries: the pinned commit's date, so the image
# is reproducible rather than stamped with the build day (upstream's
# Makefile writes `date` into .date).
HRTMON_DATE='(19.02.2021)'

VASM=${VASM:-vasmm68k_mot}
NDK_INCLUDE=${NDK_INCLUDE:-/opt/amiga/m68k-amigaos/ndk-include}

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
build="$here/build"
mkdir -p "$build"

if [ -z "${HRTMON_SRC:-}" ]; then
    HRTMON_SRC="$build/hrtmon"
    if [ ! -d "$HRTMON_SRC/.git" ]; then
        git clone --quiet "$HRTMON_URL" "$HRTMON_SRC"
    fi
    git -C "$HRTMON_SRC" checkout --quiet "$HRTMON_COMMIT"
fi
if [ -d "$HRTMON_SRC/.git" ]; then
    head=$(git -C "$HRTMON_SRC" rev-parse HEAD)
    if [ "$head" != "$HRTMON_COMMIT" ]; then
        echo "build.sh: $HRTMON_SRC is at $head, expected $HRTMON_COMMIT" >&2
        exit 1
    fi
fi
src="$HRTMON_SRC/src/HRTmonV2.s"
[ -f "$src" ] || { echo "build.sh: $src not found" >&2; exit 1; }

# The three build switches HRTmonV2.s carries at its top, flipped for the
# UAE cartridge build (ORG $A10000, cartridge header, custom-register
# snapshot at $A9F000), plus two assembler fixes:
#
# - vasm's automatic absolute-to-PC-relative optimisation (OPT a, on by
#   default outside Devpac mode) is switched off. The source opens its
#   vasm block with MC68030, so the optimiser is free to rewrite the
#   entry code's absolute operands as (d16,PC), and for TST that is a
#   68020+ form: a 68000 or 68010 takes an illegal-instruction exception
#   at the first freeze. The source chooses the CPU explicitly around
#   every privileged or 020+ instruction it means to use (MC68010 ...
#   movec ... MC68000) and writes PC-relative operands out where it wants
#   them, so with the optimiser off the shared code assembles for every
#   CPU exactly as written.
# - A guard around HRTMon's own AFB_68060 bit definition: NDK 3.2 and
#   later already define it in exec/execbase.i and vasm refuses the
#   redefinition.
#
# The vendored source is never edited; the patched copy lives in build/.
patched="$build/HRTmonV2.s"
LC_ALL=C sed \
    -e 's/^UAE = 0$/UAE = 1/' \
    -e 's/^CARTRIDGE = 0\(.*\)$/CARTRIDGE = 1\1/' \
    -e 's/^SAVE_CUSTOM = 0$/SAVE_CUSTOM = 1/' \
    -e '/^[[:space:]]*IFD __VASM$/,/^[[:space:]]*ENDC$/ s/^\([[:space:]]*\)OPT o1+$/&\
\1OPT a-/' \
    -e 's/^ BITDEF AF,68060,7\(.*\)$/ IFND AFB_68060\
 BITDEF AF,68060,7\1\
 ENDC/' \
    "$src" > "$patched"
for switch in 'UAE = 1' 'CARTRIDGE = 1' 'SAVE_CUSTOM = 1'; do
    LC_ALL=C grep -q "^$switch" "$patched" || {
        echo "build.sh: failed to set $switch in $patched" >&2
        exit 1
    }
done
LC_ALL=C sed -n '/^[[:space:]]*IFD __VASM$/,/^[[:space:]]*ENDC$/p' "$patched" \
    | LC_ALL=C grep -q '^[[:space:]]*OPT a-$' || {
    echo "build.sh: failed to add OPT a- to the vasm block in $patched" >&2
    exit 1
}
printf '%s' "$HRTMON_DATE" > "$build/.date"

# Upstream's ASMBASE flags (Makefile, the vasm branch) with the binary
# output module; the source's `include src/copper.s` lines resolve through
# the checkout's own root on the include path.
out="$build/hrtmon.rom"
(cd "$build" && "$VASM" \
    -I"$NDK_INCLUDE" -I"$HRTMON_SRC" \
    -ignore-mult-inc -nosym -quiet -wfail \
    -opt-allbra -opt-clr -opt-lsl -opt-movem -opt-nmoveq -opt-pea \
    -opt-size -opt-st \
    -Fbin -o "$out" "$patched")

# Sanity: the cartridge header ("HRT!" at +4) and the 2.39 version words at
# +56/+58 (the config block layout is fixed; WinUAE reads the same offsets).
id=$(dd if="$out" bs=1 skip=4 count=4 2>/dev/null)
[ "$id" = 'HRT!' ] || { echo "build.sh: no HRT! header at +4 in $out" >&2; exit 1; }
ver=$(od -An -tx1 -j56 -N4 "$out" | tr -d ' \n')
[ "$ver" = '00020027' ] || { echo "build.sh: version words $ver at +56, expected 00020027 (2.39)" >&2; exit 1; }

size=$(wc -c < "$out" | tr -d ' ')
echo "built $out ($size bytes, HRTMon 2.39)"
if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$out"
elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$out"
fi

if [ "${1:-}" = bundle ]; then
    cp "$out" "$repo/assets/hrtmon/hrtmon.rom"
    echo "bundled as assets/hrtmon/hrtmon.rom"
fi
