#!/bin/sh
# Build the WinUAE cputest generator and produce an instruction test set
# for the cputest-runner harness.
#
# Usage: cputest-gen.sh <output-dir> [cpu] [work-dir]
#
#   output-dir  where the generated data lands; the runner is then invoked as
#                 cputest-runner <output-dir> all <cpu>
#   cpu         68000 (default), 68010, 68020, 68030, 68040 or 68060
#   work-dir    where the generator is cloned and built
#               (default: <output-dir>/.cputest-build, kept for reuse)
#
# The generator is Toni Wilen's WinUAE cputest gencpu chain, via the portable
# packaging in emoon/m68k_cpu_tester_api (MIT). It is pinned to the commit the
# vendored runner sources in ../vendor were taken from. One local patch is
# required: upstream sysconfig.h force-defines __i386__, which poisons the
# macOS SDK's architecture dispatch (UNIX2003 symbol aliases, endian headers).
#
# On Apple Silicon the tools are built for x86_64 and run under Rosetta: the
# generator's flag-calculation code paths are tied to the x86 target in
# sysconfig.h. Linux x86-64 hosts build natively.
#
# Each CPU model generates ~25MB of .dat files (thousands of tests per
# opcode), which is why the data is generated locally instead of being
# committed.

set -e

TESTER_REPO="https://github.com/emoon/m68k_cpu_tester_api"
TESTER_COMMIT="025b999239800357e95065fe5b9a15ea5b300fa7"

OUT="${1:?usage: cputest-gen.sh <output-dir> [cpu] [work-dir]}"
CPU="${2:-68000}"
WORK="${3:-$OUT/.cputest-build}"

case "$CPU" in
    68000|68010|68020|68030|68040|68060) ;;
    *) echo "unsupported cpu: $CPU" >&2; exit 1 ;;
esac

mkdir -p "$OUT" "$WORK"
OUT=$(cd "$OUT" && pwd)
WORK=$(cd "$WORK" && pwd)

# x86_64 build on macOS (Rosetta on Apple Silicon), native elsewhere.
ARCH_FLAGS=
if [ "$(uname -s)" = "Darwin" ]; then
    ARCH_FLAGS="-arch x86_64"
fi
CXX="${CXX:-c++}"
CXXFLAGS="-O2 -w $ARCH_FLAGS -I. -Iinclude -Icputest"

# --- fetch and patch the generator ---------------------------------------

SRC="$WORK/m68k_cpu_tester_api"
if [ ! -d "$SRC" ]; then
    git clone "$TESTER_REPO" "$SRC"
fi
cd "$SRC"
git checkout -q "$TESTER_COMMIT"

# Drop the forced __i386__ define (see header comment).
python3 - <<'EOF'
path = "gencpu/sysconfig.h"
s = open(path).read()
bad = "#ifndef __i386__\n#define __i386__\n#endif\n"
if bad in s:
    s = s.replace(bad, "/* __i386__ define removed by cputest-gen.sh */\n")
    open(path, "w").write(s)
EOF

# --- build the generator chain: build68k -> gencpu -> cputester -----------

cd "$SRC/gencpu"

if [ ! -x build68k ]; then
    $CXX $CXXFLAGS build68k.cpp -o build68k
fi
[ -f cpudefs.cpp ] || ./build68k table68k > cpudefs.cpp

if [ ! -x gencpu_prog ]; then
    $CXX $CXXFLAGS gencpu.cpp missing.cpp readcpu.cpp cpudefs.cpp -o gencpu_prog
fi
[ -f cputbl_test.cpp ] || ./gencpu_prog .

if [ ! -x cputester ]; then
    $CXX $CXXFLAGS \
        -DCPUEMU_90 -DCPUEMU_91 -DCPUEMU_92 -DCPUEMU_93 -DCPUEMU_94 \
        -DCPUEMU_95 -DCPU_TESTER \
        cpudefs.cpp cpuemu_90_test.cpp cpuemu_91_test.cpp cpuemu_92_test.cpp \
        cpuemu_93_test.cpp cpuemu_94_test.cpp cpuemu_95_test.cpp \
        cputbl_test.cpp cputest.cpp cputest_support.cpp disasm.cpp fpp.cpp \
        fpp_softfloat.cpp ini.cpp newcpu_common.cpp readcpu.cpp \
        softfloat/softfloat.cpp softfloat/softfloat_decimal.cpp \
        softfloat/softfloat_fpsp.cpp \
        -lz -o cputester
fi

# --- generate the test set -------------------------------------------------

# The memory layout matches the runner's TesterBus regions. high_rom must be
# blanked or the generator expects a ROM image. feature_flags_mode=1 leaves
# the officially-undefined flag bits unverified.
cat > cputestgen.ini <<EOF
[cputest]
cpu=$CPU
cpu_address_space=68030
fpu=
verbose=1
path=data/
feature_gzip=0
test_low_memory_start=0x0000
test_low_memory_end=0x8000
test_high_memory_start=0x00ff8000
test_high_memory_end=0x01000000
high_rom=
test_memory_start=0x860000
test_memory_size=0x40000
opcode_memory_start=0x87ffa0
test_rounds=1
feature_exception3_data=0
feature_exception3_instruction=0
feature_target_src_ea=
feature_target_dst_ea=
feature_target_opcode_offset=
feature_safe_memory_start=
feature_safe_memory_size=
feature_safe_memory_mode=
feature_usp=0
feature_exception_vectors=
feature_flags_mode=1
feature_min_interrupt_mask=0
feature_interrupts=0
feature_sr_mask=0x0000
feature_loop_mode=0
feature_loop_mode_register=7
feature_loop_mode_68010=0
feature_full_extension_format=0
feature_addressing_modes_src=
feature_addressing_modes_dst=
feature_instruction_size=
mode=
[test=Basic]
cpu=68000-68060
enabled=1
mode=all
feature_sr_mask=0x0000
EOF

# The generator does not create its output tree itself.
mkdir -p "data/${CPU}_Basic"

./cputester

# The generator writes data/<cpu>_Basic; the runner expects <out>/<cpu>.
rm -f "$OUT/$CPU"
ln -s "$SRC/gencpu/data/${CPU}_Basic" "$OUT/$CPU"

echo
echo "Generated: $OUT/$CPU"
echo "Run with:  cargo run --release -- $OUT all $CPU"
