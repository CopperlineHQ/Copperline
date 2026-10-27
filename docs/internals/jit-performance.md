# Measuring JIT performance

JIT performance depends on the guest workload, memory placement, CPU model and
host. Measure real applications and sustained audio/video alongside synthetic
CPU loops. Keep fixed-clock and paced-fastest results separate: fixed mode is
deterministic, while fastest mode changes guest throughput with host load.
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
