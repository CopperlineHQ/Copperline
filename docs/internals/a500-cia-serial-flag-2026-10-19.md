# CIA serial output event and flag visibility

Physical A500 kind64 probes bracket serial ICR reads with direct Timer-A
counter values and Timer-B cascade counts. Single and pending bytes, CIA-A/B,
Timer-A latches15/31/63/127 and multiple read phases separate the output event
from timer-period-dependent byte completion. Hardware references are in sibling
`amiga-hardware-probes/references/serial-phase-v{1,2}/run{1,2}`.

An immediate flag on half-clock15 repaired old differences but created10 early
reads; it was rejected. Independent counter-bracket screening supports a fixed
four-E-clock flag-visibility pipeline after the half-clock15 event. The byte still
uses16 underflows; pending SDR promotion stays on half-clock16. The existing
additional one-E-clock external IRQ-pin delay remains separate. This is not a
change to the motherboard/CPU IRQ latency setting or to serial input timing.

`sdr_output_flag_delay` advances on E-clocks even if Timer-A stops. Explicit
armed state prevents a second event at byte handoff and allows old snapshots
without the new fields to retain an in-flight flag. New states preserve the
pipeline across save/restore; edge stepping prevents batching from moving flag
or IRQ-pin edges earlier.

Compiled replay:1533/1533 qualified extended cases match (3 excluded), and
369/369 qualified shorter-probe cases match (15 excluded). The earlier buffering
probe matches320/320; the first-boundary probe matches38 stable cases with2
unstable repeats excluded. Old CIA output controls still match all2063 qualified
observations; older CIA IRQ and IRQ-stage packets are unchanged.3637 core tests
pass,37 ignored,548 window tests excluded; strict Clippy and formatting pass.
No transmitted-payload or isolated electrical-pin timing claim follows.

The investigation record and screening assumptions are in sibling probes
`docs/serial-phase-investigation-2026-10-19.md`; full logs and frozen candidate
are under `captures/serial-phase-20261019/acceptance-v2`.
