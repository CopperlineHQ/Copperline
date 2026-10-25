# JIT throughput investigation — 25 October 2026

This work reduces chipset scheduling overhead, gives native traces access to
multiple local RAM/ROM mappings, and adds spare-time CPU bursts to paced `--jit
--cpu-speed fastest` execution. Performance remains workload dependent; the
application and live-audio results below are the acceptance checks, not just the
synthetic loop.

The starting point is `main` at `427fb1721`. The motivation was the [benchwork
comparison](https://github.com/codewiz/benchwork/blob/main/results/emulators.md)
and [m68k-rs issue 221](https://github.com/benletchford/m68k-rs/issues/221).
These runs do not reproduce benchwork's full workloads or capped host clock, and
do not establish WinUAE performance parity. Intel turbo remained enabled; the
CPU model's “2.00GHz” label does not describe an enforced frequency cap.

## Remaining work before merge

The final comparison still has two application-level regressions: fixed JIT
cannot sustain the live-audio workload at real time on this Intel host, and
fastest JIT takes longer to complete compression. The stack-allocated core
microbenchmark also retains a smaller overhead. These are explicit follow-up
targets; this report does not qualify the branch as ready to merge.

Shared-bus execution uses single-instruction batches to preserve DMA and
interrupt ordering. Earlier profiles identified repeated setup/dispatch as a
cost in that path. A further optimization must preserve those ordering checks;
merely increasing the shared-bus batch budget would invalidate the boundary
guarantees. An eight-second sample of the final fastest compression run,
starting three seconds after launch, records 1,578 of 5,332 main-thread samples
in inlined `step_slice_jit` execution, 568 in physical-memory operations, 450 in
beam advancement and 258 in batch-stop checks. This identifies work to
investigate; a matching baseline profile is still needed to attribute the 6.2%
regression.

All five Cargo consumers now pin the combined proposals in the CopperlineHQ
fork at `d70b438ff5d57e840267079ade1efa6c26647fc8`. The vendor copy is removed,
and CI checks that their lockfiles resolve the same core. A separate follow-up
PR will switch back to upstream after both proposals are accepted.

## Upstream integration

The CPU APIs are proposed upstream in [m68k-rs
#222](https://github.com/benletchford/m68k-rs/pull/222) (batch boundaries) and
[#223](https://github.com/benletchford/m68k-rs/pull/223) (physical RAM/ROM
regions). The [CopperlineHQ fork commit](https://github.com/CopperlineHQ/m68k-rs/commit/d70b438ff5d57e840267079ade1efa6c26647fc8)
combines `32912f6` and `7581836` with a linear history. Its Git tree
`023ba080522fbfa71e2e47bacc4f8e8d595d0659` is identical to the integration commit
`2f7cffb` used for the Intel measurements below. It is a normal Git dependency pinned by full revision;
Copperline carries no vendor copy of the core.

Copperline's final measured source is `0c06716e0`. In addition to inactive
chipset intervals and spare-time execution, the follow-up fixes are:

- `706883376`: defer local instruction billing until a device/shared-bus access
  or batch exit, preserve the initial hardware deadline, avoid redundant
  deadline prediction for single instructions, and charge posted-write overlap
  to interrupt entry.
- `782c16bac`: retain the established 85% regular-load target and adaptive
  billed clock alongside spare bursts. Count regular and spare instructions
  separately when deciding whether work is clock-bound.
- `0c06716e0`: skip direct-pointer capture for single-instruction batches while
  retaining bank selection and cache exclusions.

The initial 50% governor target made real application loops slower on Intel,
despite a large synthetic gain. Earlier per-instruction synchronization and
deadline prediction also caused audio underruns. Intermediate measurements are
retained in the raw data; they are not presented as final gains.

## Intel application results

Both revisions use Rust 1.97.1, release fat LTO, one codegen unit and
`--no-default-features --features frontend,cpu-jit` on a Core i5-1038NG7
MacBookPro16,2, macOS 26.7.1, on AC power. Preserved baseline/candidate binaries
alternate, with no compiler running during measurement.

LhA 2.15's `lha_68040` compresses a deterministic 4,500,000-byte text file. Wall
time includes boot and compression. Every run must return zero and its
299,312-byte archive must extract to the original SHA-256. The figures are
medians of three trials per mode; lower is better.

| Mode | Baseline seconds | Candidate seconds | Candidate / baseline |
| --- | ---: | ---: | ---: |
| fixed | 21.475 | 19.174 | 0.893x |
| fastest | 17.306 | 18.374 | 1.062x |

Fixed mode reduces this application's median wall time by 10.7%. Fastest mode
takes 6.2% longer than baseline. That remaining regression prevents claiming a
general application speedup from the synthetic result.

### Live audio and DMA

Spaceballs' State of the Art supplies a sustained Paula, DMA and rendering
workload. Each run captures screenshots at 30, 60 and 90 guest seconds, uses
live CPAL audio, and exits at the final capture. This covers the paced headless
frontend, not a window/GPU presentation path. Lower wall time is better; a
real-time run takes approximately 90 seconds.

| Mode | Revision | Wall seconds | Reported underruns | Underruns after first 30 reports |
| --- | --- | ---: | ---: | ---: |
| fixed | baseline | 90.144 | 0 | 0 |
| fixed | candidate | 105.815 | 76 | 45 |
| fastest | baseline | 90.126 | 0 | 0 |
| fastest | candidate | 90.103 | 0 | 0 |

The candidate's fixed mode needs 105.815 seconds for 90 guest seconds, versus
90.144 on baseline, and still underruns during steady playback. This remains a
PR-readiness issue for shared-bus/DMA-heavy execution.

Only `audio profile: queue_depth=` records are counted. The underrun counter
reports failed reads; it does not count every silent output frame while the
backend refills. A local WAV capture confirms active, non-silent audio in all
three 30-second intervals. Captures show the demo progressing through its intro
and dancer scenes; baseline and candidate fixed captures differ in phase, so
this visual inspection is not a byte-identical rendering comparison.

### Synthetic paced throughput

`examples/jit_throughput.rs` warms 250 quanta and measures 100 quanta of a 68040
memory loop with inactive DMA. Each sample reports host and guest elapsed time,
plus pacing slips. Figures below are medians of three alternating runs, in
millions of retired guest instructions per **host** second, including pacing
waits. Higher is better.

| Mode | Baseline MIPS | Candidate MIPS |
| --- | ---: | ---: |
| fixed | 24.83 | 24.86 |
| fastest | 70.36 | 447.13 |

### Unpaced memory and chipset cost

The same diagnostic warms 1,048,576 instructions and measures 67,108,864 without
pacing. Factor 1 uses configured instruction billing; factor 80 raises that
billing rate to roughly 2 GHz guest throughput. These synthetic loops have
masked IRQs, inactive DMA and no renderer. Medians of three alternating
processes are shown below; higher MIPS is better.

| Code | Data | Factor | Baseline MIPS | Candidate MIPS | Ratio |
| --- | --- | ---: | ---: | ---: | ---: |
| RAM | first RAM bank | 1 | 84.57 | 169.03 | 2.00x |
| RAM | first RAM bank | 80 | 285.11 | 590.38 | 2.07x |
| RAM | second RAM bank | 1 | 19.54 | 134.51 | 6.88x |
| RAM | second RAM bank | 80 | 24.03 | 345.23 | 14.37x |
| ROM | first RAM bank | 1 | 59.04 | 173.39 | 2.94x |
| ROM | first RAM bank | 80 | 125.78 | 583.00 | 4.64x |
| ROM | second RAM bank | 1 | 57.65 | 166.34 | 2.89x |
| ROM | second RAM bank | 80 | 119.30 | 595.66 | 4.99x |

Advancing ten guest seconds with an idle chipset costs 1.289 -> 0.646 host
seconds (49.9% less).

 The diagnostic's stack-allocated bare-core loop remains a separate check:

| Batch | Baseline MIPS | Candidate MIPS | Ratio |
| ---: | ---: | ---: | ---: |
| 64 | 366.31 | 344.16 | 0.94x |
| 256 | 646.41 | 591.08 | 0.91x |
| 1024 | 805.94 | 788.21 | 0.98x |
| 4096 | 863.94 | 776.50 | 0.90x |

## Core overhead

The region PR specializes native code for single-window versus multi-window
capabilities. Single-window traces retain the original bounds checks and pointer
arithmetic. An explicit empty-table guard avoids constructing and scanning
default regions at each batch entry. Intel disassembly of the warmed memory loop
has identical instructions/register allocation after normalizing CPU-field
offsets.

A separate bare-core harness uses one 64 KiB writable window at `0x200000`, a
68040, and `MOVE.L (A0),D0; ADDQ.L #1,D0; MOVE.L D0,(A0); BRA`. It warms
1,048,576 instructions, then measures 67,108,864, checking retirement counts and
the final stored increment. Builds use the same Intel compiler, fat LTO and one
codegen unit. With heap-allocated `Box<CpuCore>`, medians of 15 samples per
batch are:

| Batch | Upstream `856b8045` MIPS | Region PR `7581836` MIPS | Change |
| ---: | ---: | ---: | ---: |
| 64 | 560.73 | 568.73 | +1.4% |
| 256 | 747.68 | 744.10 | -0.5% |
| 1024 | 780.78 | 808.67 | +3.6% |
| 4096 | 822.61 | 828.55 | +0.7% |

The stack-allocated version remains placement-sensitive: large-batch samples
range from near baseline to approximately 21% slower. Moving state to the heap
removes that gap in this measurement, but the precise microarchitectural cause
has not been established. This is a remaining limitation, not a claim that every
single-window workload is faster. Both allocations and all retained samples are
in the raw data.

## Correctness checks

The combined upstream integration passes 3,001 Copperline library tests (five
ignored), all 49 visual probes, and archive round-trip checks. Formatting and
strict all-target/all-feature Clippy pass on the Copperline branch.

New coverage includes pending interrupts after SR unmasking, native-prefix/MMIO
ordering, CIA deadlines and timer starts, posted-write accounting, cache
exclusions in single-instruction batches, and save/restore into a fresh machine
with warmed ROM and three RAM mappings. Restore compares CPU state, RAM,
selected bank, bus advancement and emulated time with uninterrupted execution.

Upstream #223's warmed native tests enter compiled bodies directly, then check
fallback against stepped execution. They cover both postincrement operands, ROM
writes, MMU page crossings, ATC write protection/misses, capability changes,
self-modification, replaced backing memory and alias rejection. Its all-feature
suite passes 1,114 tests (nine ignored); native suites pass 323 on AArch64 and
375 on Intel (one ignored). Upstream Linux, Windows and Intel macOS CI pass.
Copperline application performance on Windows/Linux has not been measured.

On the local M5, a single paired compression sample of the pre-synchronization
integration versus the final candidate takes 10.113 -> 5.411 seconds in paced
fastest mode. Fixed runs both finish at 18.743 guest seconds. A separate unpaced
30-second DMA run isolates the smaller window setup: 10.892 -> 9.989 host
seconds, with byte-identical screenshots. These are follow-up checks, not a
controlled comparison between the M5 and Intel hosts.

## Reproduction

The LhA executable comes from the [Aminet
archive](https://mirrors.xmission.com/aminet/util/arc/lha_68k.lha). Generate the
input with 45,000 lines, for `i` from zero through 44,999:

```python
f'{i:08x} Copperline Motorola 68040 chipset scheduling and direct memory throughput {i*i:016x}\n'
```

The SHA-256 is
`44521e57d459ef06a6714c2ec4bc19bc8b82cb209c1d7d83eda7b13cdc6d99b2`. Use A1200,
68040 at 25 MHz, 8 MiB fast RAM, Kickstart 3.1, JIT, no audio, an empty
read-only ENV volume and a frozen RTC at `2005-03-18 01:58:29`. Launch with
`--run lha_68040 --run-args "-q a archive.lha input.txt" --exit-on-return` and
`COPPERLINE_HEADLESS_PACED=1`. Select fixed and fastest separately. Preserve the
input timestamp between trials.

For the audio workload, use A500, 68020 at 14.18 MHz, 512 KiB chip RAM, 512 KiB
slow RAM, 8 MiB fast RAM, Kickstart 1.3, JIT, `--audio`, and the demo in DF0.
Enable `COPPERLINE_HEADLESS_PACED=1`, `COPPERLINE_REAL_PACING_PROFILE=1` and
`COPPERLINE_AUDIO_PROFILE=1`.

For synthetic throughput:

```sh
cargo build --release --locked --no-default-features --features frontend,cpu-jit \
  --example jit_throughput
./target/release/examples/jit_throughput 3 paced
./target/release/examples/jit_throughput 3 mem
```

[Raw measurements and binary identities](jit-performance-data.txt) retain the
original investigation, upstream core comparisons, intermediate regressions,
final application/audio trials and reproduction inputs. ROMs, disk images and
third-party executables are local assets and are not committed.
