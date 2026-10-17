# A500 Copper launch and line BZERO follow-up

Measurements use a PAL A500 with 512 KiB chip and 512 KiB slow RAM, ECS Agnus
and OCS Denise, with One ROM live ROM replacement and checksum-protected USB
result collection. Each run backs up the current live ROM, verifies the test
image after writing it, and restores and verifies that run's backup before
releasing reset. ROM images and raw captures remain local assets.

Toni Wilen's EAB observations motivated the Copper-start and register-handoff
controls: [post 1426048](https://eab.abime.net/showpost.php?p=1426048) describes
possible Copper/blitter start inhibition, and
[post 1422393](https://eab.abime.net/showpost.php?p=1422393) describes register
updates after BBUSY clears while the last destination write is pending.
These are hypotheses to test, not substitutes for measurements.

## Provenance

Builders and immutable archives are in the sibling `amiga-hardware-probes`
repository. Each archive contains the exact assembler inputs and descriptors;
their source fingerprints were independently recomputed before interpretation.

| Suite | Source fingerprint | Trials |
| --- | --- | ---: |
| Copper WAIT, branch-free CPU | `b5549e3f000ca8e0eb973d67f04752b2c66c82aab432133073296d1160fdb524` | 1,024 |
| Same list, BLTSIZE replaced by NOOP | `59b0df645e65578d261ce8b65404afedb8f78ee4c21bd417bccdce1a937c8b43` | 1,024 |
| CPU BBUSY/register handoff | `836cfdc91ac3201e47f18c0770ddffc4e32be32054d215cec491b38cfb46d151` | 2,304 |
| Line A-data one-hot controls | `9a505b99b448c4fa93d69b644ebd72cc25191382417e591b0612c962dc210bf5` | 1,536 |

The Copperline baseline executable SHA-256 is
`f1111a474bb1ffe384a64a03226a5377be2af912aa9b63bf2e070f2cff5c78ce`.
The WinUAE executable SHA-256 is
`1060375cbfa4bbcde76e4ad2cd0876fca2f675a997ed33a51937ea678a2cbd09`.
WinUAE uses real-speed 68000, compatible CPU, CPU/memory/blitter cycle exact,
PAL ECS Agnus, no JIT, and matching RAM sizes. Its resolved configuration is
saved with each run. Copperline uses the matching ECS Agnus/OCS Denise config.

## Copper WAIT controls

The Copper starts a 16-word D-only or A-to-D blit and executes one or two
already-eligible BFD-clear WAITs before requesting COPER. The CPU executes
ROM NOPs or chip-memory reads without a loop branch or DMACONR polling.
The IRQ handler's first VHPOSR read is an observation of the complete path,
not a direct timestamp of the Copper WAIT edge.

All 1,024 rows complete successfully on each platform with matching marker
count, destination data and BBUSY-clear readback. Excluding horizontal
positions 0/1, which straddle VHPOSR's pipelined line wrap, and requiring equal
launch timestamps gives:

| Comparison against A500 | Matched rows | IRQ observation difference |
| --- | ---: | --- |
| Baseline Copperline, blit present | 218 | median -2 colour clocks |
| WinUAE, blit present | 246 | all exact |
| Baseline Copperline, no-blit control | 307 | all exact |
| WinUAE, no-blit control | 255 | all exact |

Requiring equal saved PCs as well leaves 150 Copperline blit rows with median
-2 clocks and 154 no-blit rows with exact timestamps. Removing DBRA did not
remove the residual. This isolates a blit-dependent path; it does not justify
adding a blanket IRQ delay or establish the reported start-inhibition bug.

Captures: `rom-amiga500-copper-wait-unrolled-run1/capture`,
`rom-amiga500-copper-wait-no-blit-control/capture`, and corresponding
`rom-copperline-*` / `rom-winuae-*` folders. The original loop control was also
repeated on A500; 1,023/1,024 records were identical between runs.

A candidate preventing a newly Copper-started blit's internal startup step
from executing in the MOVE's own quantum passed two focused ordering tests,
but produced byte-identical blit and no-blit result packets. It was removed
from the working code rather than presented as a timing fix. Its optimized
executable SHA-256 is
`b287df4d9c23d7dd762d4758227593f673a5ae34de92246fcc95b72a7b4da4dd`.
The next control starts the blit from the CPU with the Copper already waiting,
to distinguish launch ownership from the BFD-finished wake path.

## CPU register-handoff controls

All 2,304 A500 records complete without timeout. The observed immediate last-D
read and both settled destination endpoints equal the expected FFFF (D-only
and ABCD) or AAAA (A-to-D) on all three platforms. No stale final word is
observed by this polling sequence. This confirms the tested update sequence;
it does not prove that software reads the pending-write interval itself.

Launch beam timestamps match all 2,304 descriptors across the three platforms.
With extra display/Copper DMA disabled, all 576 handoff beam observations also
match A500 exactly. Four/six-plane contention exposes further Copperline
timing differences. WinUAE matches every clean four-plane and six-plane
handoff timestamp; the six-plane-plus-Copper-burst workload has 44 nonmatching
clean observations out of 548. It therefore must not be treated as hardware
truth for all contention cases. The handoff timestamp is measured after
polling, a memory read and BLTCON0 write; poll-loop quantization can magnify a
small preceding difference.

This kind repurposes raw result offsets: +14 first-clear DMACONR, +16 immediate
last-D read, +18 postwrite DMACONR, +20/+22 handoff beam reads, +24/+26 final
first/last D, +28 INTREQR, +30 settled beam, +32 status. Generic CSV column
names such as `counter`, `icr` or `saved_pc` do not describe those fields.

Capture: `rom-amiga500-blitter-handoff-kind49-run1/capture`; packet SHA-256
`480f5177aea788d870b4714d49d6b6465a9e9f8139e6c75941417cb4aa7a0b3b`.

## Line BZERO one-hot controls

All 1,536 records have identical launch/read/end beam observations and final
destination words on A500, baseline Copperline and WinUAE. Eight repeats of
each case are identical. Nonzero-B controls verify the effective A word:
the first pixel writes `ADAT >> ASHIFT`, and the second writes the next bit
position. Zero-B controls write zero to both destinations.

Only the two-pixel, zero-B wrap cases differ. With target `n` selecting
`ADAT = 0x8000 >> n`, A500 leaves BZERO set for:

| ASHIFT | A500 targets with BZERO set | Baseline Copperline targets |
| ---: | --- | --- |
| 0 | 2, 4 | none |
| 2 | 0, 2, 14, 15 | 14, 15 |
| 4 | 0, 12, 13, 14, 15 | 12, 13, 14, 15 |

Copperline differs in 40 records, WinUAE in 296; differences are confined to
the eight DMACONR samples' BZERO bit. BBUSY and destination words match.
The unexplained exceptions correspond to effective A positions 13 and 11.
Do not encode that incidence map as an unexplained constant. Next controls
must distinguish intermediate B/C inputs, minterm evaluation and flag
reduction using other minterms, multi-bit A inputs and non-wrap B shifts.

Capture: `rom-amiga500-line-a-bit-kind48-run1/capture`; packet SHA-256
`d56881c4b62fd708ec94e71adccb862a35814eb9185621dc702ef6605f631961`.

### Minterm and non-wrap controls

The 2,048-row truth-control archive has source fingerprint
`feb59528cda4d4797efdb5776d9d385b6d34948360074698c5748b892f489621`.
It crosses eight A patterns, minterms 00/FF/F0/CC/33/AA/40/CA, zero/all-one
register B data, B shifts 0/1 and eight repeats. C DMA remains enabled from
zeroed memory; B DMA is disabled. The original observer is unchanged.

Every beam observation and both destination words match on all three
platforms, and every repeat is identical. At BSHIFT=0 with B=0, hardware
sets BZERO for minterms 40 and CA only when A is 2000, 0800 or 2800; adding
bit 0 (A=2801) clears it. Direct-A minterm F0 clears BZERO for all nonzero A,
so the hardware does detect those bits in the ordinary pixel calculation.
Minterm CC clears BZERO on hardware, as in Copperline; WinUAE keeps it set.
At BSHIFT=1, the disputed zero-output cases keep BZERO set on all platforms.
These controls narrow the exception to the inter-pixel wrap evaluation.
They still do not independently measure the intermediate B word or establish
why the effective A-mask has these exceptions.

Capture: `rom-amiga500-line-a-bit-truth-control-run1/capture`; packet SHA-256
`d767ef553a221a0ace394153d993fb59f1cc0369a11a43f292d4c20d91c20304`.
For both kind48 modes, raw offsets +20..+34 are eight DMACONR words, including
offset +32; the generic decoder's `status` field is therefore a flag sample,
not a timeout status. Completion is verified by the packet's complete flag,
checksum and descriptor count.

## Scope qualification

The MHO98 accepts 50M acquisition depth at 2 ms/div and exports 12.5M digital
samples, about 25 ms at a CPU-clock-derived sample interval near 2 ns. The
RG03 LA time-increment header still reports 0.5 ns and cannot be trusted.
This time calibration uses nominal PAL CPU frequency, not an independent
frequency reference. Some deep captures have no qualified custom accesses or
the selected trigger edge and are excluded from timing evidence. One overlaps
the live-ROM helper's startup/reset interval; a later normal Kickstart deep
capture also lacks the selected edge, so reset does not explain every case.
A subsequent normal Kickstart capture has varied address/control signals
and REGEN activity. Require actual qualified bus windows before interpreting
any future scope recording; a reported STOP state alone is insufficient.
