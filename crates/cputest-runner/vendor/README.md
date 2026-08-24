# Vendored cputest runner

`m68k_cpu_tester.c` / `m68k_cpu_tester.h` / `cputest_defines.h` are vendored
from Daniel Collin's MIT-licensed API wrapper around Toni Wilen's WinUAE
680x0 CPU tester. Upstream:

    https://github.com/emoon/m68k_cpu_tester_api
    commit 025b999239800357e95065fe5b9a15ea5b300fa7

(`cputest_defines.h` originates in WinUAE's `cputest/` directory, GPL, same
license family as this repository.) The runner parses the generated `.dat`
test sets, drives a CPU implementation through a callback, and performs all
comparison and reporting.

The upstream runner predates the current generator's data layout; the local
changes (each marked "Local patch" in the source) port the newer format
handling over from WinUAE's `cputest/main.c` and expose two settings the
embedding runner needs:

- the data-file footer check accepts the newer layout, which appends a
  file-count byte after the CT_END_FINISH terminator;
- `struct registers` carries the newer generator's end-of-instruction PC,
  branch target, and cycle-count records (CT_ENDPC / CT_BRANCHTARGET /
  CT_CYCLES), which both restore paths parse, with the delta-decoded bases
  seeded per test file;
- the end-of-instruction sentinel (`ILLEGAL` + `NOP`) and taken-branch
  target words are planted into the test image and word-swapped each CCR
  round, mirroring the generator's `doopcodeswap` protocol;
- `validate_exception` skips the newer length-prefixed exception detail
  records (the exception NUMBER is still verified; frame contents are
  checked through the register comparison);
- out-of-range CT_ABSOLUTE_LONG branch targets are accepted (the newer
  generator emits them for targets outside test memory);
- `m68k_tester_addressing_mask()` exposes the mask parsed from the data
  header (bit 31 of the level word selects 32-bit addressing), and
  `m68k_tester_fpu_model()` the FPU configuration, so the Rust side can
  mirror the generated machine.
- `M68KTester_run_tests` honours its documented contract (1 when every
  test passed, 0 otherwise): upstream returns 0 on every path, so a clean
  run was indistinguishable from a mismatch. A mnemonic skipped for a CPU
  level mismatch or missing `lmem.dat`/`hmem.dat` counts as failed, and
  every fatal abort (unreadable or malformed test data, allocation
  failure) exits with status 1 instead of the WinUAE original's 0, so a
  broken data set cannot read as a pass in CI.

`capstone-stub/` shadows the capstone disassembler include the runner uses
for pretty-printing failing instructions; the stub reports no disassembly
and the runner prints raw opcode words instead, avoiding the dependency.

The test DATA is generated separately by the (GPL) cputest generator, built
from the same repository at build time by `tools/cputest-gen.sh` -- see that
script; the generator and its data never enter this repository.
