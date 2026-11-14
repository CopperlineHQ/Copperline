# Measuring performance

Performance depends on the guest workload, memory placement, CPU model,
execution mode and host. Measure real applications and sustained audio/video
alongside synthetic CPU loops. Keep fixed-clock and paced-fastest results
separate: fixed mode is deterministic, while fastest mode changes guest
throughput with host load.
See [CPU execution](cpu.md#jit-fastest-speed) and
[pacing](timing.md#jit-fastest-pacing) for the scheduling model.

## Establish a comparison

Build both revisions with the same Rust toolchain, target, features and release
profile. Preserve each binary, its source revision, dependency revision and
SHA-256 before starting. Record the host CPU, operating system, power mode and
any enforced frequency limit; a CPU's model string is not a frequency cap.

Alternate baseline and candidate runs, with no compiler or competing benchmark
running on the host. Preserve guest inputs and timestamps. Use several trials
and report medians together with the individual measurements. A single fast
sample, synthetic MIPS result or different host does not establish an
application improvement or parity with another emulator.

Use `--factory` or an explicit configuration so a saved launcher default cannot
change the machine. Pin the RTC with `--rtc-time` and `--rtc-frozen` when guest
software reads it. Keep the ROM, media, memory layout and cache settings equal
between revisions. ROMs, disk images and third-party executables remain local
assets.

## CI AROS performance gate

The `Performance` workflow runs `AROS performance gate` on every PR and on
pushes to `main` and `v1`, with no path filter. It compares `copperline-bench`
executables built using the same stable Rust toolchain, `--release --locked
--no-default-features --features bench-bin`: once from the full commit SHA in
`tools/performance/baseline.json`, and once from the candidate (the PR merge
commit on a PR). This pins a cumulative comparison point before Bus or
workspace refactoring. It does not move to each PR's parent automatically.
Both branches share this one baseline, including any differences between
`main` and `v1`; a forward merge does not select a different baseline.
Require **AROS performance gate** in the protected branches' rules to make
the check a mandatory merge gate; adding the workflow does not change those
repository settings.

The baseline cache holds only the executable, keyed by its source SHA,
the complete `rustc -Vv` output and workflow hash, runner OS/architecture
and Ubuntu image family. Workflow changes invalidate the build recipe too.
An exact hit skips baseline compilation; a miss builds it with the same
command. There are no fallback keys. Only pushes to `main` or `v1` save
shared baseline caches; PRs restore them without saving merge-ref copies.
The candidate is always built. Cache key/hit records accompany the binary
hash and source/toolchain provenance in the performance artifact.

The tracked TOML files specify PAL A500 (68000, 512 KiB chip and slow RAM)
and A1200 (68EC020, 2 MiB chip RAM) cold boots with empty drives. Both use
fixed CPU clocks, interpreter execution, zeroed RAM and a frozen RTC. The
same explicitly named AROS ROM pair is used for both revisions, with its
SHA-256 hashes pinned in the policy. Each machine runs for 30 emulated
seconds both core-only and with CPU rendering, post-processing and
deinterlacing. This covers common CPU/bus/chipset/render paths without
proprietary assets. It does not cover native JIT throughput, disk-heavy
applications, live audio, GPU presentation or every expansion board.

Both compilations finish before timing starts. Each of the four cases gets
one discarded warmup per binary, then five baseline/candidate pairs on the
same runner, reversing order every pair. The gate compares the median of
the paired wall-time-per-emulated-frame ratios and fails above a 10%
slowdown. Improvements in one case cannot offset a regression in another.
The threshold allows some shared-host jitter; absolute milliseconds from
different hosts or workflow runs are not compared.

Missing or malformed output, nonzero exit, a 120-second sample timeout,
incomplete emulated time, or mismatched emulated frame counts/presented
counts/render checksums fails the case. A median absolute deviation above
5% of the median paired ratio also fails, reporting unstable timing instead
of claiming success. This measures noise in the value used for the verdict;
host drift shared by both sides of each pair cancels instead of failing a
comparison with identical ratios. No outlier is removed and no fast retry
is selected. Each binary must still repeat the same workload across its
own warmup and measured trials.

The benchmark's existing checksum is a coarse presentation check, not an
image comparison: keep the golden-render and functional suites required
alongside this check. The benchmark's frame-time sample count can exceed
the emulated frame count because a core step may stop at an internal
boundary; throughput uses elapsed wall time and retired emulated frames.

The job summary lists each median, paired percentage change and result.
When the two internally repeatable workloads differ, the case fails with
**workload changed, baseline refresh needed**. Its medians, paired ratios
and noise measurement are retained in `report.json`, and the summary
marks the percentage change as informational. These numbers describe the
change but cannot establish equal-work performance parity.

The `aros-performance` artifact retains raw warmup/trial logs, all paired
measurements in `report.json`, configuration/ROM/binary hashes, source SHAs,
lockfile hashes, toolchain and host details for 14 days, including failures.
Review these before rerunning a noisy job. A stalled job is bounded to
30 minutes. The Python gate tests explicitly exercise slower candidates,
missing output, timeouts, workload mismatches and unstable samples.

To reproduce locally, first build and preserve the baseline binary in a
separate checkout of the pinned revision, then build the candidate with
the same command, toolchain and target. Print the required baseline SHA
with `python3 tools/check-performance.py --print-baseline`. With no builds
or competing workloads running, invoke:

```sh
python3 tools/check-performance.py \
  --baseline-binary /path/to/baseline/target/release/copperline-bench \
  --candidate-binary target/release/copperline-bench
```

Results go to `target/performance-gate/` (`--output` selects another
directory). The harness strips inherited `COPPERLINE_*` settings, passes
explicit absolute config/ROM paths, and gives each process a fresh working
directory, so local diagnostics, assets and saved launcher defaults cannot
alter the workload. Run its asset-free supervision tests with:

```sh
python3 -m unittest discover -s tools/tests -p test_performance.py -v
```

Update the baseline SHA, ROM hashes or workloads only in an explicit,
reviewed change. Preserve the old and proposed baseline comparisons and
explain any intended emulation or workload changes. Do not refresh the
baseline merely to make a slowdown pass, or copy a developer machine's
absolute timings into the policy. The 10% limit and 5% noise threshold
should be revisited with retained hosted-run evidence if runner variance
changes.

For an intended timing or rendering correction that changes the workload
signature, keep the old baseline during review so the correction's own
cost remains visible. Attach its failed performance artifact and the
functional/golden evidence explaining the intended signature change. If
branch rules require this check, a maintainer can record and approve an
override for that specific PR under the repository's protection policy.
After merging the correction, refresh the pin to its immutable merged
commit in a separate reviewed PR and rerun the gate. Preserve the original
comparison alongside the refresh. Pointing the fix's own PR at itself
would hide the performance effect that required the refresh.

## Application throughput

An archive compressor exercises CPU-local loops, OS calls and storage. For an
LhA workload, use A1200, 68040 at 25 MHz, 8 MiB fast RAM, Kickstart 3.1, JIT,
no audio, and a mounted empty read-only ENV volume. The [direct execution
guide](../guide/run.md) describes executable and volume setup. Use the same
`lha_68040` binary for every run.

Generate a deterministic input file:

```python
from pathlib import Path

text = ''.join(
    f'{i:08x} Copperline Motorola 68040 chipset scheduling and direct memory throughput {i*i:016x}\n'
    for i in range(45_000)
)
Path('input.txt').write_bytes(text.encode('ascii'))
```

The input contains 4,500,000 bytes and has SHA-256
`44521e57d459ef06a6714c2ec4bc19bc8b82cb209c1d7d83eda7b13cdc6d99b2`.
Preserve its modification time and use a fresh output directory for each trial.
Launch with `--run lha_68040 --run-args "-q a archive.lha input.txt"
--exit-on-return`, a frozen RTC at `2005-03-18 01:58:29`, and the chosen
`--cpu-speed fixed` or `--cpu-speed fastest` setting.

Set `COPPERLINE_HEADLESS_PACED=1` to exercise the same pacing policy as an
interactive session. Record host elapsed time and guest elapsed time: wall time
includes boot and compression, and guest time reveals a machine falling behind
real time. Require exit status zero and extract every output archive to verify
that its contents hash to the original input. Throughput without correct output
is not an accepted result.

### A build tool's mix (`osbench`)

A build run inside AmigaOS is mostly not a CPU loop: it is short stretches
of compiled code between exec and dos.library calls, small-file traffic and
process launches. `guest/osbench` produces that mix in five phases that can
be run one at a time (its README has the command line and the machine, an
A3000 with Kickstart 3.x and a `copperhf` hardfile it formats itself):
compiled C in fast RAM, exec allocator calls, small files on FFS, and
launches of a command loaded from the volume. It reports the emulated time
each phase took.

Read two numbers from every run, because they bind in different modes. The
process's CPU time is the host cost of the work, which is what limits a
high fixed `clock_mhz` once the host is saturated. The emulated time is
what a session that keeps real time waits for, and at a high clock it is
almost all shared-bus time: chip-RAM reads of `AbsExecBase`, INTENA writes
from `Disable` and `Enable`, CIA and board register accesses. A change can
move either without the other, so report both, per phase, with a bare
format-only run subtracted for the boot.

Things this workload has shown, worth checking again after changes to the
batch path:

- OS code ends a batch every few dozen instructions, so anything done per
  batch start costs like something done per instruction.
- Code that executes in place from an expansion board's ROM is a device
  access per opcode word unless the board declares the ROM plain
  ([the batch execution mode](cpu.md#the-batch-jit-execution-mode)).
- Compiled C spends most of its host time in the core's decoded loop, not
  in compiled traces, and about one instruction in eight reaches `CpuBus`
  through the full dispatcher.
- `speed = "fastest"` finishes a fixed amount of work later than a high
  fixed clock run flat out: it holds the machine at real time and leaves
  the host headroom, where an overloaded fixed clock simply lets emulated
  time fall behind. Use a fixed clock to finish a batch job soonest and
  `fastest` to keep the machine responsive while it runs.

## Audio, DMA and rendering

Use a sustained Paula/DMA/rendering workload as well as the CPU-heavy program.
One reproducible example is State of the Art with an A500-shaped machine, 68020
at 14.18 MHz, 512 KiB chip RAM, 512 KiB slow RAM, 8 MiB fast RAM, Kickstart 1.3,
JIT and the disk in DF0. Run with live `--audio` and capture frames at 30, 60 and
90 guest seconds. Enable `COPPERLINE_HEADLESS_PACED=1`,
`COPPERLINE_REAL_PACING_PROFILE=1` and `COPPERLINE_AUDIO_PROFILE=1`.

Report host time for the full guest interval, audio underruns during startup
and steady playback, and screenshots showing that the guest kept progressing.
Count only `audio profile: queue_depth=` records: their underrun counter records
failed reads, not every silent frame while the backend refills. Capture a WAV
separately when checking that the guest actually generated audio. A paced
headless audio run does not validate the window/GPU presentation path.

Use golden-render tests and fresh-machine save/restore comparisons to check
correctness independently of speed. Fastest-mode captures need not be
byte-identical because guest throughput is host-dependent.

## Synthetic loops and profiles

The diagnostic example separates paced throughput, local-memory placement,
chipset cost and bare-core overhead:

```sh
cargo build --release --locked --no-default-features --features frontend,cpu-jit \
  --example jit_throughput
./target/release/examples/jit_throughput 3 paced
./target/release/examples/jit_throughput 3 mem
```

Report instructions per host second for paced tests; instructions per guest
second alone can hide failure to sustain real time. Warm the JIT before timing,
and retain CPU/cache/memory placement details when comparing bare-core loops.

Profile the same workload interval in both binaries. A saved state can skip
boot and loading, but use a matching state for each machine configuration and
record its provenance. Keep profiles separate from timed trials because sampling
adds overhead. Attribute an improvement only after repeating the application
and correctness checks; reducing setup work must preserve device deadlines,
interrupt recognition, cache coherence and fresh-machine restore behavior.
