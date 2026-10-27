# CIA physical output and timer readback stages

The A500 kind66 and address-coded scope probes distinguish PB output, timer
readback, ICR flags, physical CIA /IRQ, Paula INTREQR and physical CPU IPL.
CPU interrupts are masked in the scope kernels. These are distinct observables;
a PB discrepancy does not justify shifting the timer or CPU recognition.

## Physical PB stage

Four-channel-qualified CIA-B captures show PHI2 PB6/PB7 output one E-clock
(10 CPU clocks) before the timer flag and ordinary /IRQ assertion. Copperline
commits this physical output stage; PRB transfer reads observe the same pins.
This replaces the previous transfer-only PRB preview. Pulse expiry and adjacent
pulse merging remain explicit, and the event scheduler visits the PB boundary.
CNT/cascade timer-B output retains its existing stage: these captures directly
qualify the PHI2 path, not all timer input modes or CIA revisions.

## Automatic reload and old-counter carry

A one-shot high-byte write preserves a carry from the old zero counter
independently of the newly loaded timer. Timer B now retains this history,
matching the existing Timer-A distinction. An old carry does not stop the newly
loaded one-shot. Old3/new0 must not create the earlier old-zero carry.

A separate old0/3, new15 test proves event count: after a destructive ICR read,
old0 produces two separated physical IRQ assertions near 20 and 180 CPU clocks;
old3 produces only the later assertion. This rules out explaining the extended
zero-latch PB pulse solely as an output-width artifact. New serialized history
fields default to false when absent in older states.

## Validation and remaining uncertainty

References in amiga-hardware-probes:
`references/cia-b-four-channel-scope-20261024-v1` (28 captures),
`references/cia-tb-event-count-20261024-v1` (4 captures), and
`docs/cia-pin-boundaries-2026-10-24.md` (qualification and baseline deltas).
The prior kind66 reference remains `references/cia-source-v3/run{1,2}`.

The combined change repairs 82 qualified scope read differences without a new
qualified difference. The earlier 288-case pulse reference repaired 36 cases,
but retained a historical difference and unstable rows. Subsequent probes
verified stopped-counter history and starting phase: the controlled 576-case
matrix matches, and five independently phase-selected refresh observations
reproduce the model's extra E tick and resulting pulse inversion. The old
unqualified comparison is not evidence for another PB retiming.

The later ICR/underflow sweep matches all 2,048 selected-source flag and control
cases. Eight four-channel CIA-B captures provide 52 valid one-shot boundary
observations: ordinary IRQ uses falling E, while a raced request uses rising E.
The measured race-minus-control interval is approximately 5.90 CPU clocks,
consistent with the modeled six-clock difference. These controls do not
establish every external-input or continuous/cascade boundary.

Physical /IRQ precedes IPL6 by about eight CPU clocks. The implemented
output/input/CPU stages are described in [the timing model](timing.md).
Request and aperture comparisons match 640/640 and 1,280/1,280 cases; conditioned
keyboard startup controls retain their qualified matches. The unchanged
`DEFAULT_IRQ_LATENCY_CCK=6` remains separate from source propagation.

The software ICR sweep's elapsed-E witness is a separate unresolved observation:
1,898 matches, 36 stable differences and 114 unstable cases. It includes a
custom beam read before arm; exact starting-phase coverage is incomplete.
Flag agreement and physical boundary checks do not waive those timing results.
Historical broad-suite exclusions also remain exclusions, not hardware matches.

Current evidence in the sibling probe repository:
`docs/cia-pb-phase-2026-10-25.md`, `docs/cia-icr-race-2026-10-25.md`,
`references/cia-pb-phase-chip-20261025-v1/`,
`references/cia-pb-gated-scope-20261025-v1/`,
`references/cia-pb-tb1-scope-20261025-v1/`, and
`references/cia-icr-race-scope-20261025-v2/`. Captures retain exact source/ROM
fingerprints, units and qualification rules; no trace fitting is used.

## Non-timer request regression controls

Two independent 64-case A500 captures with keyboard and DF0 attached match
serial-output and clocked-TOD controls on both CIAs. They distinguish a masked
pending source, enable after acknowledgement, acknowledgement of Paula while
the CIA source remains asserted, and acknowledgement of the CIA while Paula
still retains its latch. The selected-source flag must be present; unrelated
raw ICR flags are retained separately. No timing shifts or exclusions are used.
The native bus regression repeats these orders and checks a fresh reassertion.

Two qualified runs each match all 72 cases, including eight real CIA-B
disk-index FLAG controls. These cover masked
pending flags, enable after acknowledgement, held-source reassertion after
Paula acknowledgement, and final source/request clearing. Positive source
witnesses are mandatory; no exclusions or timing shifts are used.

The native FLAG test injects a synthetic input; the hardware reference verifies
the settled real-index path.
Neither establishes electrical FLAG-to-IRQ latency or the CIA-A parallel-ACK
path. Complete packets, source fingerprints and verified restoration are in
`references/cia-external-pr-complete-20261026-v1/` in the sibling probe
repository; see `docs/cia-floppy-flag-2026-10-26.md` there for the before/after
fixture evidence. No emulation change was required by the measurements.
