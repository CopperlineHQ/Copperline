# Fuzzing Copperline's parsers

Downloaded media, packages, save states, configuration and debug information
reach parsers before guest code runs. The nine targets below treat parser
errors as expected and leave panics, sanitizer findings, timeouts and excess
memory visible to libFuzzer.

## Running

```sh
cargo install cargo-fuzz --locked
cd fuzz
cargo +nightly fuzz run dms
cargo +nightly fuzz run floppy_image   # ADF/extADF/DMS/SCP/IPF/gzip/zip
cargo +nightly fuzz run cd_image       # CUE/BIN, bare ISO, NRG, CHD
cargo +nightly fuzz run hardfile_classification # HDF/HDZ/CHD classification
cargo +nightly fuzz run savestate      # .clstate machine images
cargo +nightly fuzz run config_toml    # RawConfig TOML and pure accessors
cargo +nightly fuzz run archives       # LHA/ZIP listing, reads, extraction
cargo +nightly fuzz run debug_info     # hunk/LINE/DWARF and ELF
cargo +nightly fuzz run control_protocol # CCP framing, JSON-RPC, auth, codecs
```

Use the rustup Cargo proxy for `+nightly`; if a package-manager Cargo comes
first on PATH, run `RUSTC="$(rustup which --toolchain nightly rustc)" rustup run nightly cargo fuzz run <target>`.

A bounded local campaign, matching the scheduled job:

```sh
cargo +nightly fuzz run --codegen-units=16 archives -- -max_total_time=900 -timeout=10 -rss_limit_mb=2048 -max_len=1048576
```

Crashes land in `artifacts/<target>/`. Reproduce one by passing the file to
that target. File-backed harnesses use unique temporary directories so
workers cannot share or overwrite inputs. Config fuzzing deserializes
without resolving paths or opening devices. Control fuzzing exercises the
wire parser and authentication without sockets or command execution.
Debug-info fuzzing does not open source paths or execute the program.

The archive harness uses 64 KiB per member, 256 KiB total output and 128
headers so legitimate expansions reach a limit error quickly. The same
bounded reader serves the production package APIs, whose defaults are
256 MiB per member, 1 GiB total output and 16,384 entries; library scans
use a 1 MiB slave cap. Declared sizes are checked early, actual output is
bounded independently, and reads reach EOF to preserve checksum checks.

## CI and retained regressions

`ci.yml` builds all targets and runs each for ten seconds on every PR and
push. `fuzz.yml` runs all nine independently for fifteen minutes daily,
and can be started with `workflow_dispatch`. GitHub schedules run the
workflow on the default branch, so a maintenance-branch change must be
merged forward before its daily schedule becomes active.

The campaign builds use 16 codegen units to keep instrumentation builds
within the job budget. Both jobs cap each input at 1 MiB, memory at 2 GiB and each execution at ten
seconds; job wall-clock limits also cover builds and hangs. Scheduled jobs
restore each target's evolved corpus and save it for the next campaign.
Crashes, logs and scheduled corpora are uploaded even on failure and retained
for 30 days. A failing target does not cancel the other scheduled targets.

`cargo test --test fuzz_regressions` replays committed boundary inputs and
retained crashes using the **same harness functions** in `harnesses/`,
without libFuzzer or nightly. Follow
[`tests/fuzz-regressions/README.md`](../tests/fuzz-regressions/README.md)
to minimize and retain a new failure. CI artifacts preserve evidence;
a triaged fix commits its minimized reproducer for permanent stable replay.
A successful short campaign is smoke coverage, not a sustained-fuzzing claim.

## Seeds and dependencies

Regenerate the small owned parser seeds and initial boundary replay files:

```sh
python3 tools/generate_parser_seeds.py
```

This does not remove discovered corpus entries or later retained crashes.
The ELF seed is compiled from the committed guest debug-info probe source.
The save-state target also starts from `corpus/savestate/current.clstate`.
Whenever `savestate::STATE_VERSION` or a chunk version in
`src/savestate/chunk.rs` changes, regenerate that seed from this directory:

```sh
cargo +nightly run --example generate_savestate_seed
```

The fuzz crate follows the root Copperline package by path, with parser and
JIT features but no desktop/device/network backends. Its `Cargo.lock` is
derived from the production `Cargo.lock` and ignored, avoiding a second
committed dependency graph. Before the first local build (and after a root
lock update), run from `fuzz/`:

```sh
cp ../Cargo.lock Cargo.lock
cargo metadata --format-version=1 > /dev/null
```

This keeps shared dependency versions pinned to the shipped code while
resolving the fuzz-only dependencies.
