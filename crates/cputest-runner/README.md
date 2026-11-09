# CPU conformance

This standalone crate tests the published `m68k` package selected by the
emulator's lockfile. It is outside the root build; `cargo test` at the
repository root does not run these tests or dependency unit tests.

## PR gate

`CPU conformance (020+)` runs on every PR and push to `main`/`v1`, without
path filters. It combines:

- The pinned WinUAE generator's cases for every entry in `pr-opcodes.txt`
  on 68020, 68030, 68040 and 68060, using full addressing extensions and
  32-bit addresses. It covers moves, indexed address calculation, bitfields,
  compare-and-swap, and long multiplication/division, including the 060's
  exception behavior for unsupported instruction forms.
- Asset-free 030/040/060 PMOVE/MOVEC privilege and MMU enable checks,
  page-table walks, PFLUSHA/ATC invalidation, write protection,
  invalid data/code pages, a translated guest load and write-fault frames.
- A hot arithmetic/store loop compared against instruction stepping on all
  four models, also with translation enabled on 030/040/060 and after changing
  the hot instruction. Registers, flags, PC, stack banks and memory must agree.
  The crate enables `m68k/jit` and runs on a native host; nonzero trace calls
  and retired instructions therefore prove Cranelift execution. Decoded
  fallback does not count. Without `jit`, these counters would also count
  portable traces, which is why the feature is mandatory here.

`tools/check-m68k-locks.py` separately checks that all CPU consumers resolve
the same version, source and checksum. To enforce a merge gate, require
`CPU conformance (020+)` and the lockfile check in GitHub branch protection.
Repository settings are separate from this workflow.

The workflow explicitly selects Bash with `pipefail`, so logging through
`tee` preserves runner failures. The PR loop records every CPU's result and
fails after all four runs if any failed. Failure-injection tests execute the
actual workflow commands with controlled runner/generator failures before
the real conformance runs.

Scheduled and manually dispatched runs additionally generate full instruction
sets for 68000/010, alongside the same 020+ subset. Jobs have explicit time
limits and retain generator/runner logs for seven days. Generated data and generator binaries
are temporary, not committed or cached between workflow runs.

## Run locally

The generator needs Git, Python 3, a C++ compiler and zlib headers. Linux x86-64
builds natively; macOS builds the generator for x86-64 (Rosetta on Apple Silicon).
The Rust tests use the host's native JIT backend.

From the repository root:

```sh
python3 -m unittest discover -s tools/tests -p test_cputest_workflow.py -v
cargo test --release --locked --manifest-path crates/cputest-runner/Cargo.toml
cargo build --release --locked --manifest-path crates/cputest-runner/Cargo.toml
for cpu in 68020 68030 68040 68060; do
    crates/cputest-runner/tools/cputest-gen.sh /tmp/cputest-pr "$cpu" /tmp/cputest-build pr
    crates/cputest-runner/target/release/cputest-runner /tmp/cputest-pr all "$cpu" --pr
done
```

Omit the final `pr` generator argument and runner `--pr` for a full corpus.
An individual mnemonic/size directory can replace `all` without `--pr`.
The PR opcode list is shared by generation and validation: missing or extra
directories fail. Data must use the pinned generator's version-16 format,
with contiguous numbered files, matching generation IDs and a final
completion footer. Header-only, all-skipped and zero-case runs fail too, as
does a CPU case that reaches the instruction ceiling without terminating. A generator that
prints an unknown mnemonic and still exits successfully cannot pass `--pr`.

## Limits

Generated tests compare registers, memory stores and exception numbers. The C
adapter currently skips detailed exception-frame records; it does not validate
their contents despite older documentation claiming otherwise. The separate
MMU tests assert selected architectural frame fields and sizes, not every
exception/pipeline detail. The PR corpus uses no FPU. Functional conformance
does not qualify 020+ cycle timing or replace emulator bus/chipset tests.

Full 020+ corpora remain available locally for adapter development. With the
current pinned generator/adapter, full replay has unresolved `MOVE.L` trailing
instruction-memory differences on 020/030/040 and a stopped `LPSTOP` case on
060. They are not enabled as scheduled full-suite jobs until replay is
reconciled; the required 020+ check uses the explicitly listed, validated
subset. No mismatches are waived or converted to passes by the runner.

Generator and adapter provenance are in [vendor/README.md](vendor/README.md).
