# Vendored m68k core

Vendored from https://github.com/benletchford/m68k-rs
Commit: 50d1f63 (published as crates.io `m68k` 0.1.5), MIT licensed.
Fetched: 2026-05-31.

Copperline depends on this via a path dependency
(`m68k = { package = "copperline-m68k", path = "crates/m68k" }`) so we can give
it accurate MC68000 cycle timing + a prefetch queue. See `CYCLE_TIMING_GAP.md`
for the measured baseline gap and the fix plan.

## Relocation + package rename (2026-06-28)

Moved from `vendor/m68k` to `crates/m68k` and the package renamed from `m68k` to
`copperline-m68k` (the importable library name is kept as `m68k` via `[lib]
name`, and the root crate aliases it back with `package = "copperline-m68k"`, so
no source `use m68k::...` changed). Two reasons, both from issue #57:

- Distro/OBS packaging vendors crates.io deps into a directory named `vendor/`
  and points `[source.vendored-sources] directory = "vendor"` at it; cargo then
  demands a `.cargo-checksum.json` for every subdir. Our committed path crate in
  `vendor/m68k` has no checksum file, so offline builds failed
  ("failed to load checksum .cargo-checksum.json of m68k v0.1.5"). `crates/`
  does not collide with the vendoring target.
- The package name no longer matches the upstream crates.io `m68k`, and the dep
  carries no version requirement, so no tool, `[patch]`, or future workspace
  member can ever substitute the unmodified upstream crate for this fork.

## Local changes vs upstream 50d1f63

- `tests/singlestep_m68000_v1_tests.rs`: capture the fixtures' cycle `length`
  and bus `transactions` (upstream parsed but discarded them) and add
  `cycle_gap_report` (an `#[ignore]`d measurement of the cycle / bus-access gap).
- `src/core/instructions/shift_rotate.rs`: long register shift/rotate base is 8
  (was 6), gated to the 68000/68010 prefetch-queue CPUs.
- Part D (cycle counts): per-instruction-class cycle-total fixes across
  `decode.rs` / `instructions/*.rs`, gated to the 68000 (48.3% -> 0.9%
  mismatch, totals ratio 0.999).
- Part E (cycle-exact core, all gated to `CpuType::M68000`):
  - `tests/singlestep_m68000_v1_tests.rs`: full per-transaction parsing
    (FixtureAccess: direction/address/data/strobes/cycle offset), a
    RecordingBus test double, and two more `#[ignore]`d reports:
    `access_sequence_gap_report` (prefetch-model scoreboard) and
    `access_timing_gap_report` (sync-timing scoreboard).
  - `src/core/cpu.rs`: real 2-word prefetch queue (IRC/IRD) with
    consume/top-up/invalidate operations; 68000 bus-order helpers
    (read_long_predec_68000, write_long_mm_interleaved_68000);
    `internal_cycles()`/`flush_sync()` intra-instruction clock reporting.
  - `src/core/memory.rs`: `AddressBus::sync(cpu_clocks)` default no-op trait
    method -- hosts receive internal (non-bus) clocks immediately before
    each bus access (Moira-style).
  - `src/core/ea.rs`, `execute.rs`, `decode.rs`, `exceptions.rs`,
    `instructions/*.rs`: prefetch-queue fetch paths, flow-change refills,
    microcode bus-order quirks (exception stacking order, RMW write order,
    MOVEM extra read, CLR/Scc read-before-write, ...), and per-class
    internal-cycle placement.
  - Final numbers: access sequences 99.9% fixture-exact, per-access timing
    99.6% exact, cycle totals 0.999. See CYCLE_TIMING_GAP.md "Part E".

## Upstream review (post-0.1.5, reviewed 2026-06-09)

Reviewed upstream `benletchford/m68k-rs` through `9f5dab4` (8 commits past the
`50d1f63` fork point). Findings:

- The substantive upstream work is a **trace JIT** (`src/core/trace_jit.rs`)
  and an **opcode cache** (`src/core/op_cache.rs`) plus the supporting
  execute/decode/cpu refactor (a *conditional* rollback snapshot gated by
  `needs_rollback_snapshot`, `DecodedSimpleOp` fast paths, trace recording).
  This is a pure throughput optimization and is **deliberately not merged**: it
  reorders and elides the per-instruction fetch/prefetch/internal-cycle work
  that the local Part D/E changes rely on for cycle-exact MC68000 timing, which
  is the entire reason for this fork. Merging it would regress the 99.6-99.9%
  fixture-exact timing for a speed-up Copperline does not need (the host already
  paces to wall-clock time).
- The remaining upstream diffs are Clippy/formatting cleanups, `Cargo.toml`/
  `Cargo.lock` version bumps, and microbenchmarks -- no instruction-semantics
  or cycle-timing bug fixes to port.
- One genuinely useful item was taken: the post-fork regression test
  `test_odd_pc_fetch_after_register_only_instruction_does_not_restore_stale_snapshot`
  (guards the conditional-rollback optimization against leaking stale register
  state on a faulting opcode fetch). It is ported into
  `tests/unaligned_fetch_tests.rs`; the vendored cycle-exact core passes it,
  confirming the always-snapshot path here is already correct.

## Upstream re-review (reviewed 2026-06-14)

Re-reviewed `benletchford/m68k-rs` through `327a7c3` (8 further commits past the
`9f5dab4` review point, all dated 2026-06-11/12). Nothing to port -- every commit
is a throughput optimization in the trace-JIT / opcode-cache / decode-fast-path /
wasm machinery that this fork deliberately does not use, and several would
actively regress the Part D/E cycle-exact MC68000 timing:

- `Optimize postincrement long moves` (f57eeb0): adds an
  `exec_move_l_postinc_to_postinc` decode fast path that does a flat
  read_32/write_32 and returns a hardcoded 4 cycles, bypassing the prefetch
  top-up and the `write_long_mm_interleaved_68000` bus-order model this fork
  relies on for `MOVE.L (An)+,(Am)+`. Not merged.
- `Optimize register shift traces` (61bf70c): semantics-preserving closed-form
  rewrite of the ASR/ROL/ROR loops, but keeps the upstream base-6 cycle cost
  (this fork uses base 8 for long register shifts on the 68000/68010) and also
  edits `op_cache.rs`/`trace_jit.rs`. No correctness fix; not merged.
- The rest (`Fast-path extension control-flow steps` db11812 and its same-day
  revert 030b279, `Avoid duplicate simple-op decode fallback` f46c9e7, the two
  wasm trace-bookkeeping skips a48591e/15d8c98, and the `Bump m68k to 0.1.13`
  version bump 327a7c3) are pure speed/wasm/version changes with no
  instruction-semantics or cycle-timing bug fixes.

Upstream continues to chase throughput (re-adding the perf work that `9f5dab4`
itself rolled back); the host already paces to wall-clock time, so none of it
buys Copperline anything.

## Fixtures (not committed -- large)

The SingleStepTests `m68000` set (~182M) is gitignored. To run the harness /
gap report, fetch it once:

```sh
git clone --depth 1 https://github.com/SingleStepTests/m68000 /tmp/sst-m68000
mkdir -p crates/m68k/tests/fixtures/m68000
cp -R /tmp/sst-m68000/v1 crates/m68k/tests/fixtures/m68000/
```

Then, from `crates/m68k`:

```sh
# functional (final-state) suite
cargo test --release --test singlestep_m68000_v1_tests
# cycle / bus-access gap report
cargo test --release --test singlestep_m68000_v1_tests cycle_gap_report -- --ignored --nocapture
# access-sequence gap report (prefetch-model scoreboard)
cargo test --release --test singlestep_m68000_v1_tests access_sequence_gap_report -- --ignored --nocapture
# per-access timing gap report (sync-timing scoreboard)
cargo test --release --test singlestep_m68000_v1_tests access_timing_gap_report -- --ignored --nocapture
```

Set `M68K_GAP_FILE=<file>.json.bin` with the sequence report to dump the first
few mismatching cases of that file with both access streams side by side.
