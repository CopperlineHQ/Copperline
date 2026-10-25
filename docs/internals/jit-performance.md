# JIT throughput investigation — 25 October 2026

This change reduces chipset scheduling overhead and lets paced
`speed = "fastest"` spend spare host time on CPU-only work. Enable it with
`--jit --cpu-speed fastest` on a supported 68020+ configuration. Fixed-clock
JIT and fresh unpaced runs retain deterministic instruction billing.

The motivation was the [benchwork comparison](https://github.com/codewiz/benchwork/blob/main/results/emulators.md)
and the direct-memory proposal in [m68k-rs issue 221](https://github.com/benletchford/m68k-rs/issues/221).
The measurements below isolate mechanisms; they do not reproduce that AmigaOS
benchmark or its capped host clock, and do not establish WinUAE performance
parity. The Intel CPU's model name includes “2.00GHz”, but turbo was not disabled
or capped for these runs.

## Changes

The implementation is based on `main` at `427fb1721`:

| Commit | Change |
| --- | --- |
| `8589a8b51` | Synchronize retired instructions before ordinary bus accesses; bound batches by hardware deadlines, up to 4096 instructions. |
| `b1c96bb7d` | Offer read-only ROM and several local RAM regions to native and portable traces, with permission, range and MMU checks. |
| `258e80747` | Generate region lookup inline to avoid helper-call register spilling in the common memory path. |
| `09a80c14f` | Batch chipset intervals proven to contain no bus activity, stopping at fetches and other observable events. |
| `ceaf71e53` | Run bounded CPU-only bursts before presentation, and let the baseline governor reduce CPU work below configured MHz under overload. |

`vendor/m68k` contains the published 0.14.7 source with the documented boundary
and memory-region extensions. Its original MIT licence is preserved. Remove
the path dependency when an upstream release supplies equivalent APIs.

Spare bursts leave chipset time still for local CPU work. Shared-bus accesses
retain their normal cost and yield; STOP yields too. Hardware deadlines and
fractional-clock carries remain part of regular JIT execution. The burst
phase reserves presentation time and checks the host deadline between batches.
A cold compilation can still overrun a quantum; this is not a hard host latency
bound. The timing model is detailed in [](cpu.md) and [](timing.md).

## Method

The maintained diagnostic is `examples/jit_throughput.rs`. Its memory loop
loads, increments and stores a longword, then branches. IRQs are masked and
DMA is inactive. It uses a 68040 with FPU support, seven CPU clocks per colour
clock (about 24.83 MHz), and synthetic ROM/RAM contents. No AmigaOS, display
renderer or live audio contributes to these measurements.

`paced` warms each machine for 250 quanta (about 4.2 guest seconds), then
measures 100 quanta (about 1.68 guest seconds). This gives the old adaptive
governor time to settle. MIPS is retired guest instructions divided by
**host elapsed seconds**, including pacing waits. Guest seconds and pacing
slips are recorded separately so guest slowdown cannot inflate the result.

`mem` first warms 1,048,576 instructions, then measures 67,108,864 instructions
without wall-clock pacing. Factor 1 uses configured instruction billing;
factor 80 raises that billing rate to roughly 2 GHz guest throughput. The
chipset-only subtest advances ten PAL guest seconds with idle DMA.

The Intel comparison uses the same diagnostic source and release build flags
on both revisions. The old production source is unchanged `427fb1721`; only
the diagnostic is copied into its `examples/` directory. Paced trials alternate
between the preserved old and new binaries, with no compiler running during
measurement. Results are medians of three trials; raw output accompanies this
report.

```sh
cargo build --release --locked --no-default-features --features cpu-jit \
  --example jit_throughput
./target/release/examples/jit_throughput 3 paced
./target/release/examples/jit_throughput 3 mem
```

Hosts:

- Intel MacBookPro16,2, Core i5-1038NG7, macOS 26.7.1, Rust 1.97.1, AC power.
- M5 MacBook Pro, macOS 27.0.1, Rust 1.99.1. The local build also enables the
  `frontend` feature; the diagnostic does not construct a frontend.

The Intel throughput snapshot contains the final paced and unpaced diagnostic
paths, but predates the final Warp minimum-clock guard. Warp was not exercised
on Intel. The final tree, including that guard, was tested on AArch64.

## Results

### Intel before and after

All CPU figures are millions of retired guest instructions per host second.
The factor column only applies to unpaced machine runs. A code/data bank
pair means that ROM or RAM code and its data occupy the stated physical regions.

| Workload | Before | After | Throughput ratio |
| --- | ---: | ---: | ---: |
| Paced fixed | 24.84 | 24.81 | 1.00× |
| Paced fastest | 70.59 | 403.31 | 5.71× |
| RAM code, same-bank data, factor 1 | 86.02 | 166.06 | 1.93× |
| RAM code, same-bank data, factor 80 | 283.71 | 600.74 | 2.12× |
| RAM code, second-bank data, factor 1 | 19.22 | 134.06 | 6.98× |
| RAM code, second-bank data, factor 80 | 23.67 | 341.72 | 14.44× |
| ROM code, first-bank data, factor 1 | 58.00 | 161.08 | 2.78× |
| ROM code, first-bank data, factor 80 | 122.83 | 610.12 | 4.97× |
| ROM code, second-bank data, factor 1 | 57.84 | 158.97 | 2.75× |
| ROM code, second-bank data, factor 80 | 120.80 | 574.41 | 4.76× |
| Bare core, batch 64 | 360.25 | 310.39 | 0.86× |
| Bare core, batch 4096 | 806.60 | 667.32 | 0.83× |

The idle-chipset subtest costs **1.347 → 0.679
host seconds per ten guest seconds**, a 49.6%
reduction. In the new paced-fastest trials, 1.682728 guest seconds take
1.682521–1.683169 host seconds, with zero pacing slips in every trial.

The bare-core loops regress by 14–18% across the four tested batch sizes.
The new direct-memory path adds permission checks and region resolution;
its common-path overhead is a remaining tuning target. Larger deadline-bounded
batches and direct ROM/other-bank coverage outweigh that cost in every measured
unpaced whole-machine case. The fixed paced case remains at its configured
instruction rate.

### M5 result

The same paced diagnostic reaches a median **1,109 host MIPS** in fastest
mode, versus **24.83 MIPS** fixed. Fastest samples range from 910 to 1,120 MIPS;
all have zero pacing slips and closely matching guest/wall time. There is no
unchanged-main M5 comparison in this report, and the hosts use different Rust
versions, so these figures should not be read as a controlled CPU comparison.

[Raw trial output and binary identities](jit-performance-data.txt) include all
recorded comparison samples, including the bare-core regressions.

## Correctness checks

The final implementation passed:

- 2,996 Copperline library tests (five explicitly ignored tests), using
  the CI profile with `--no-default-features --features cpu-jit`.
- All 49 visual probes: the 48 existing precise-chipset references and the
  added JIT bus-boundary probe. The JIT probe's ROM/cache bootstrap phase change
  is explained in `timing-test/README.md`; native-prefix/MMIO ordering is also
  checked against one-instruction scheduling.
- 237 portable CPU-core tests, 321 native AArch64 core tests, and 373 native
  Intel x86-64 core tests (one ignored on Intel).
- A real guest executable launched through bundled AROS with `--run`,
  `--run-args 7 --exit-on-return`, in fixed and fastest paced modes. Both
  returned the expected AmigaDOS status 7.
- `cargo fmt --check` and
  `cargo clippy --all-targets --all-features --locked -- -D warnings`.
- The HTML manual build with `myst build --html --ci --strict`.

Focused regressions cover CIA deadlines, starting timers inside a batch,
MMIO after a native prefix, read-only stores and read-modify-write operations,
region-boundary accesses, MMU permissions, self-modifying code, replacing
backing memory, cache exclusions, save/restore bank selection, STOP, fractional
clock restoration, expired burst deadlines and overload backoff. Inactive-span
tests compare every serialized Bus field with the per-quantum reference across
random intervals and frame boundaries.

These checks establish the tested ordering, state and rendering invariants.
Application-level speed and compatibility still depend on code placement,
shared-bus traffic, FPU usage, DMA and frontend load. Full benchwork workloads,
Windows/Linux hosts and a host capped at 2 GHz have not been measured here.
