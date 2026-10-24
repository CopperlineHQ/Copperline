# A500 BZERO bus validation, 2026-10-16

## Scope and evidence

Measurements use the same physical PAL A500 (512 KiB chip, 512 KiB slow,
ECS Agnus, OCS Denise), One ROM Fire 40 A and Rigol MHO98 with PLA2216.
CH1 is disconnected; these are digital captures. Capture files and ROM
images remain local in `amiga-hardware-probes/captures/` and `build/`.
No additional Copperline behavior change is justified by the results below.

The broader kind33 sweep still differs: its two new 1,024-record captures
have 36/37 BZERO-word differences from current Copperline and 48/49 from
WinUAE. Every other recorded field matches. Targeted controls are required
before changing the existing line-wrap model; see below.

The earlier [kind42 result](a500-line-c-input-2026-10-15.md) remains valid
as a description of that capture, but its mismatch has **not reproduced
on the current powered-on setup**. Do not treat its 896 differing BZERO
samples as a currently reproducible acceptance target.

## Reproduction and observer control

The exact archived kind42 ROM was loaded again, with source ID
`6817153ba97b5fa6ec4f9768f0e267b854a8d6e84d4a5ce2188ebcb88987a0e1`
and raw ROM SHA-256
`f8eacd9ccea036e869f233a971a10b91580371dc857e0da297e53d4cc1c89c3f`.
The new hardware packet in `captures/kind42-a500-la-attached/` has SHA-256
`8a1cfb795b06a733802de3a9656462eeb2caae2dd1e1c4892ccfc2e1e60b4cf2`:
160 completed records, no skips or timeouts. All 1,280 sampled BZERO bits
are set, including both observers and every first-C/A combination. The
older packet differed despite the same ROM. Cold-start state, retained
chip state, firmware/setup changes and attached-probe loading have not
yet been separated.

Further controls narrow that list. After the broad kind33 workload and
continuous line tests, kind42 still produced the same trial fields.
The normal Kickstart firmware rebuilt with USB/host-control plugins is
byte-identical to the prior 720,896-byte backup from 15 October (SHA-256
`7faf644f3d33d97075f07d7ffc4c5ca89037e17238cc012ec07e825f1b85638f`).
Loading that firmware and repeating the older procedure—hold reset,
patch the live ROM, verify all 512 KiB, release reset—also produced
identical kind42 trial fields and 1,280 set BZERO samples. That packet,
`captures/kind42-a500-live-poke/results.bin`, has SHA-256
`f266485091270df01b2821919096a1dc55e0a2e95bdb74518e2bc17ef5ba6979`;
its header's beam bit differs, while every trial field matches.
The One ROM firmware image and the two loading procedures therefore do
not explain the discrepancy in these controls. The owner confirms a full
A500 power-off occurred between the older and current capture sets;
power-on state and physical probe loading remain confounded.

The exact final kind39 archive also completed 344 records without skips
or timeouts. Packet `captures/kind39-a500-la-attached/results.bin` has
SHA-256 `b288b6701f2e1c9b15aac3ff875996e3981b3fc46ce8dcc38a0a44dd8708d070`.
It matches all recorded fields from both existing emulator preflights.
Compared with its previous hardware packet, exactly 40 DMACONR BZERO bits
changed from clear to set; every other field is unchanged. Thus the loss
of the older discrepancy is not specific to the revised observer kernel.

Kind44 removes the pre-sample beam-memory store and saves the eight
DMACONR reads consecutively. It crosses ten C/A presets, unprimed versus
an eight-word normal-mode all-zero A/B/C/D prime, two observers and eight
repeats: 320 records and 2,560 DMACONR words. Direct reads now observe
BBUSY on the first sample; the other seven follow completion. The polling
observer samples after busy clears. Every BZERO sample is set.

Reviewed archive `build/kind44-v3/` has source ID
`2f6909df2c458b5bacd31972a27b9c03482c3126f3b7a1556680e4e949a73e98`.
Hardware packet SHA-256:
`9016441dbc7452ce1cde59b551a7b8eb34ccf44ac0cdbfa59f35075bd0ee2f2a`.
Both Copperline and WinUAE Best Compatibility match all 2,560 complete
DMACONR words, not just their BZERO/BBUSY bits. This is a focused status
comparison, not a claim that all emulator behavior or timing matches.
Copperline executable SHA-256:
`9986d14a16a5b05de60f7641c069d61a73684e6e881aa1cbfe15745b94fae54a`.

Kind45 extends this to six normal-mode histories: no prime, all-zero,
A-only all-one, B-only all-one, C-only all-one and all-one A/B/C sources.
The 960 records completed without skips/timeouts. Both emulators match
all **7,680 complete DMACONR words**; BZERO remains set for every sample.
The first direct sample has BBUSY set, and later samples have it clear.
These selected normal-mode histories do not restore the old discrepancy;
this does not rule out every possible retained-state mechanism.

Archive `build/line-c-history-controllednonzero-v2/` has source ID
`9128d07c6c2c7dba7093a277f278950d24979f938879141751162c08e404dd73`.
Hardware packet `captures/kind45-a500-v2/results.bin` SHA-256:
`68213e27c31971e6b0f2c23bfd4f103f68c656a7dd4f4766b59af75324c1cb84`.
The emulator preflight checks actual A/B/C source memory at collection
completion against the final descriptor's literal pattern, including
nonzero A and B words. The new suite reuses the reviewed kind44 observer
instructions. Probe unit tests: 94 passed after adding these controls.

## Independent digital wiring check

A continuous diagnostic ROM reads custom offsets `002,004,008,010,020,
040,080,100` in order, with three NOPs after each read. It disables DMA
and masks interrupts. This independently exercises every probed address
bit A1 through A8 instead of relying on Kickstart reads whose addresses
share several high bits.

`build/scope_address_a1_a8_20261016/` raw ROM SHA-256:
`a6302bc421abb8534bb6f379efe476b4c7fa1af7a8e5444437a850fcea6fe99d`.
`captures/bzero-scope-2026-10-16/address-walking-full.bin` contains 70
qualified read windows, all in the expected repeating order. Qualification
requires AS, Gary REGEN, UDS and DTACK active together, with stable R/W
and address. The wiring and decoder pass this check.

With analog channels disabled, 20 microseconds/division and a requested
one-million-point acquisition, the exported LA record contains 250,000
packed digital samples. D0's rising-edge period averages 140.969 samples,
consistent with about 1 ns per digital sample using the nominal PAL CPU
clock as reference. The file's 250 ps increment is unsuitable for LA timing;
the declared payload size is also overstated. Analysis uses actual payload
length and captured clock edges. This reference-derived scale is not an
independent absolute timebase calibration.

## Continuous line captures and positive control

Three small diagnostic ROMs repeatedly restore the same two-pixel line
configuration, launch it, and read DMACONR eight times consecutively from
RAM code. Unlike kinds44/45, they do not synchronize to a frame phase.
The archive names are `build/scope_bzero_c0000_b0000_v4_20261016/`,
`scope_bzero_cffff_b0000_v4_20261016/`, and
`scope_bzero_c0000_bffff_v4_20261016/`. B DMA is disabled in all three;
the last sets BLTBDAT to ffff as a nonzero-result positive control.

Qualified DMACONR windows show BZERO high for the zero-B controls with
first C either zero or ffff. With B=ffff, D13 settles low on subsequent
reads. This rules out a D13 input stuck high and shows actual bus values,
rather than relying only on the ROM's exported status words.

Several windows contain a transition after DTACK is already active,
consistent with data settling during a read. The decoder records first
and last levels plus every transition; it does not label such a window
stable or silently pick its midpoint. These observations alone do not
establish the exact 68000 data-sampling instant or an internal flag edge.
The first read observes busy, subsequent reads observe completion.

Raw captures under `captures/bzero-scope-2026-10-16/`:

| File | SHA-256 |
| --- | --- |
| `address-walking-full.bin` | `59c5320b75c96ba0b3814e099c4b40847e367867216d54de7700b6d5da24edb8` |
| `bzero-c0000-b0000.bin` | `c1beec8580d6e54e36e03620059aff53eb7a5e2ae5f3d20ad6270ae4e0b1e9b4` |
| `bzero-cffff-b0000.bin` | `02c4411c4bb0c84d8a3e5d8ace68c079f6374d4a19c77a16db48aad6264459dc` |
| `bzero-c0000-bffff.bin` | `23188bcff22110d4f00aa947446e6b2f692f4b42f14371bf4362d4a5a694bc7c` |

## Remaining shift/B-DMA discrepancies

The exact kind33 ROM (source ID
`5f619a48e43447d851236bcbfd5276595c8e71267f62204a685613900b3ed83d`)
was run twice. First packet SHA-256:
`c5f33adb9c740a60c8c75626c46123a19c1ce022a997e0c1b2ca30b8c79f8221`;
repeat: `854a3ba77817f967f64fb24ffa85e30919af766c0aa31332436067ff41386597`.
Only target 57's BZERO changes between runs, from set to clear.

| Comparison | First run | Repeat | Other recorded field differences |
| --- | ---: | ---: | ---: |
| Current Copperline vs A500 | 36 BZERO words | 37 | 0 |
| WinUAE Best Compatibility vs A500 | 48 BZERO words | 49 | 0 |

Copperline has two false clears, targets 129 and257, in both captures:
A shifts 2/4 respectively, B shift0, A=8000, B=C=0, LF=ca, B DMA off,
two pixels. The other differences are hardware-clear/emulator-set.
The two older hardware runs differ from the first current run on 115/117
BZERO words respectively, with no other field differences. The older runs
already differed from each other on two BZERO words. No single packet is
a sufficient oracle for every line-mode flag case.

Continuous RAM-code diagnostic captures independently reproduce the
current distinction, using A=8000, B=C=0, B shift0, LF=ca and two pixels:

| Capture stem | A shift | B DMA | BZERO after completion |
| --- | ---: | --- | --- |
| `bzero-ash0-bsh0-ca-bdmaoff` | 0 | off | clear |
| `bzero-ash2-bsh0-ca-bdmaoff` | 2 | off | set |
| `bzero-ash4-bsh0-ca-bdmaoff` | 4 | off | set |
| `bzero-ash0-bsh0-ca-bdmaon` | 0 | on | clear |

The raw `.bin`, decoded `.json` and SCPI logs are in the same scope
capture directory. The v5 diagnostic archives record all literal controls.
These controls remove dependence on the old observer's ROM execution and
its single late status sample, but not every launch-phase/history effect.
The accepted extra wrap evaluation with effective B=ffff does not predict
all these new results. Replacing that value with another fitted constant
would not establish a hardware mechanism and is not accepted.

## Controlled CPU bus write

Kind46 tests a narrower hypothesis: a CPU data transfer immediately after
launch influences the extra line-mode BZERO evaluation. It crosses all 16
A shifts with six write words (`0000,ffff,2000,0800,2800,d7ff`), chip RAM
`18000` versus slow RAM `c01000`, and eight repeats (1,536 records).
Each trial uses A=8000, B=C=0, B shift0, LF=ca, B DMA off and two pixels.
The observer writes BLTSIZE, executes `MOVE.W D3,(A3)` to the selected
scratch address, then performs eight consecutive DMACONR reads. The opcode
and destination remain identical when comparing values within one region.
All source words are restored explicitly, including the second C word.
After completion, both destination words are recorded separately.

The region index temporarily occupies the descriptor's phase field.
That field is held at zero through both beam alignment stages, then
restored before the record is finalized. Preflight confirms identical
launch phase for both regions. The controller saves/restores A3, which
the outer measurement loop uses as its descriptor pointer.

Archive `build/line-c-bus-data-kind46-v2/` source ID:
`5a9cfc28ee9c17a620d259ec73a8b05ce9f0b5f45c72dc26a8f694747dce6317`;
raw ROM SHA-256:
`3d6fb876bac15bebdac4e6725524ac38bf939f925dd1571d887d62078de37256`.
Both emulator preflights completed all records, with identical launch and
subsequent beam fields, zero destination readbacks throughout, and no
timeouts or skips. Copperline clears all eight BZERO samples in every
trial; WinUAE sets all of them. Every sample pair differs only by 2000.
CCP also verifies zero A/B/C source memory and the final d7ff word at
both scratch addresses. Those final scratch checks do not independently
verify every preceding write value on hardware.

The A500 completed all 1,536 records with no skips/timeouts. Packet SHA-256:
`303a2776216993689ef8c95c5f32afab05abd20f56c898bee44a1fb5332cee3f`.
Every destination readback is zero, and all recorded beam fields match
both emulator preflights. BZERO is set only for A shifts 2/4: 192 records,
or 1,536 sample words. It is clear for the other 1,344 records. Each case
is invariant across all six CPU write values, both RAM regions, all eight
repeats and all eight samples. Copperline therefore falsely clears 1,536
sample words; WinUAE falsely sets 10,752. This specific CPU write-value/
destination control does not explain the mask. Other CPU traffic phases
have not thereby been ruled out.

Two further continuous diagnostics hold the A shift field at zero and
write ADAT=2000 or 0800 instead. Both leave BZERO set, matching ADAT=8000
shifted by 2 or 4 respectively. Captures are
`bzero-a2000-ash0-bsh0-ca.bin` and `bzero-a0800-ash0-bsh0-ca.bin`.
This supports dependence on the effective A bit position rather than
only on the A-shift register field. The next matrix should include
nonzero-B output-word controls to verify the alternative ADAT inputs
independently; do not turn this incidence map into an unexplained constant.

## Next controls

1. Repeat any new flag differences before inferring a rule. The selected
   normal-mode zero/nonzero A/B/C histories have been tested and matched;
   further state controls need a distinct hypothesis.
2. Cross all 16 one-hot ADAT words with A shifts 0/2/4, B data 0/ffff,
   one/two pixels and eight repeats in a checksum-protected One ROM suite.
   Keep C zero and B DMA off, record both actual output words and the
   eight early DMACONR reads. B=ffff controls must prove the intended
   shifted A words reach memory. This separates effective bit position,
   counter state, nonzero data and the inter-pixel wrap evaluation.
3. Cross a reduced discriminating set with A-data/control-register write
   order, controlled prior line operations and early/postbusy observers.
   Only then extend full minterm closures and the B-DMA path. Retain the
   existing within-case repeat and exact timing/memory checks.
4. If the old C-dependent mismatch remains absent, distinguish power-on
   state and probe loading with a controlled physical comparison. The
   owner confirms a power-off occurred, but both factors changed between
   sessions. Plan the reset/loading sequence before asking for wiring
   changes; do not fit the older non-reproducing packet.
5. Accept a core correction only when a repeatable state/datapath rule
   predicts held-out inputs and preserves existing normal, fill, IRQ and
   accepted line-wrap results.

## Checkpoint

Probe tests: 99 passed. Current isolated legacy builds still fit their
focused payload limits (kind29: 62,380 bytes; kind30: 60,220 bytes).
No new Copperline execution behavior was changed in this session.
The original live Kickstart was restored and read back byte-for-byte
(SHA-256 `9797dfe08af1e48271528411a41eda20fb03ee3cf387e610ce50eedfb0c457eb`),
and SEL_C was released. The normal firmware retains USB and host-control
plugins and is identical to the prior 15 October backup noted above.

Probe implementation and measurement notes are committed in
`amiga-hardware-probes` at `ea36f4c` (with whitespace cleanup in the next commit).
