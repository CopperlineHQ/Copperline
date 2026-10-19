# A500 CIA serial output buffering

Two independent physical A500 runs of the drained kind63 serial probe agree
byte-for-byte across320 records. Both CIA-A and CIA-B, Timer-A latches15/31,
and repeated observation sweeps support one pending output byte. Active SDR
writes preserve the first completion, then cause one more
completion; two active writes do not add a third completion. Single-byte and
idle-write controls already matched Copperline. The previous implementation
restarted shift progress on every SDR write and failed all160 active-write cases.

`Cia::sdr_pending` holds one pending byte while `sdr_out` is active. Writes leave
the active count/half-clock intact. Completion promotes the pending byte and
starts a fresh count. Direction changes clear active and pending state. The new
field defaults to empty when reading older save states. Existing E-clock stepping
keeps serial completion and the following delayed IRQ-pin edge distinct.

Evidence is in sibling `amiga-hardware-probes/references/serial-buffer-v1-run{1,2}`;
source015b086d, ROM00875ce2, results484d0f3c. Full metadata, hardware configuration,
capture/restoration method and repeatability are in the reference manifests.
The short investigation record there links build sources, comparator and logs.

Comparison uses exact observed Timer-A-underflow brackets from Timer-B cascade,
not inferred CPU cycles or shifted traces. It compares the SP completion flag,
with CPU/CIA interrupt masks disabled. This establishes completion count/timing;
it does not establish transmitted payload, overwrite priority on the SP pin,
electrical IRQ propagation or writes exactly at a byte boundary. The pending
slot retains the latest software SDR value, but payload behavior still needs
pin-level verification; no new serial-pin output model is claimed.

The queue candidate repairs147 complete cases, with13 existing active-write
differences left at the second-completion boundary near observed underflow31;
all160 single/idle controls remain matches. A separate40-case single-byte sweep
(`serial-boundary-v1`) shows6 differences at observed underflow15, with identical
accepted-baseline/candidate packets. Hardware has SP set in all8 underflow15
samples, whereas Copperline has it in2. This isolates a pre-existing serial
flag-boundary difference; buffering alone does not resolve it. Keep byte-period,
flag-latch phase and the one-edge-delayed cascade observation distinct before
changing the nominal sixteen-underflow byte model.

Asset-free validation:3635 core/network tests pass,37 ignored,548 window tests
excluded as in the accepted baseline; strict all-target/all-feature Clippy and
format checks pass. New tests cover active-write progress, pending capacity,
save/restore including missing legacy fields, direction reset and batched versus
single-edge execution. MyST is unavailable locally; documentation builds were
not run.

All11 earlier CIA/IRQ/bus/blitter regression packets and the1920-case CPU/DMA
neighbor are byte-identical to the accepted4038ec64 baseline. Thus the buffered
write repairs do not change those established timing constraints. The13 active
boundary differences and the first-byte boundary differences remain open.
