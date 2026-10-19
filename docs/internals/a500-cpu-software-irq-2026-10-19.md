# A500 CPU-written request recognition

Kind58 hardware controls compare CPU INTREQ writes and INTENA unmask of pending
software requests for PORTS, EXTER and COPER. Two independent physical runs agree
on all 1152 measured cases: NOP/DBRA, STOP, masked INTREQR; four phase selectors,
eight prelaunch pads, two repeats. Exact launch/read brackets only; no shifts.
ROM SHA256: edcd6716965ebb9ec486264fd488c82b37c22ded14081f28f75803a095b35cf0.

Before: all 384 masked readbacks agree, all 768 recognition records differ.
PORTS uses an immediate bypass (early); EXTER/COPER retain the source deadline
(late). The CPU-write reference phase is two CCK earlier than that deadline.
Tracking software origin also gives PORTS/EXTER the internal instruction-poll lead.

After: zero timing/recognition differences across all 1152 qualified cases;
1147 complete matches, five diagnostic CIA serial-flag differences. Do not hide
those diagnostic differences or treat handler beam as an electrical IPL timestamp.
The asynchronous default remains six CCK; CIA pin-fed behavior is unchanged.
The CPU-write correction is limited to measured PORTS/EXTER/COPER bits; BLIT
and other unmeasured CPU-written bits retain their prior deadlines.
The two-CCK CPU-write offset is applied only on stock 68000 bus timing.
Snapshots accept both the preceding Copper-only field name and missing origin.

Hardware references and scripts live in the sibling amiga-hardware-probes repo:
`references/cpu-irq-v5-run{1,2}`, `build_cpu_irq_rom.py`, `compare_irq_stage.py`,
`docs/cpu-irq-investigation-2026-10-19.md`. Detailed logs and packet comparisons:
`captures/cpu-irq-20261019/`. CIA phase neighbor (1920 cases) is byte-identical
before/after; focused IRQ2212 and divider IRQ2560 match hardware. Broader
validation: 3632 optimized core/network tests pass,37 ignored,548 window tests
excluded; strict all-feature/all-target Clippy and formatting pass. Held-out
STORE/TST controls repair640 additional timing records; two hardware runs agree
on all768 observations. Across main+held-out1920 cases: zero timing differences,
1910 complete matches and ten diagnostic serial-flag differences. Ten prior
regression packets are byte-identical; source-stage IRQ results are unchanged.

The CIA fresh-underflow request remains separately early; the matching mask-enable
readbacks do not justify a global CIA delay. CPU operand/prefetch controls under
DMA remain the next instruction-boundary investigation.
