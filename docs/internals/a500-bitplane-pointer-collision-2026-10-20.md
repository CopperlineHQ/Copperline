# Bitplane pointer writes during the Agnus DMA counter update

A500 kinds72/73 use PF1/PF2 CLXDAT matching as a digital witness for retargeting
a zero-filled bitplane to ones. Kind73 samples again after the following full
scanline, distinguishing a genuinely lost pointer write from early collision
visibility. Both live captures restore OneROM and verify reset release; all
constant positive/negative controls pass. CPU/Copper writers, both BPL2 pointer
halves and BPL1PTL provide neighboring controls.

Copper BPL2 writes lose at B2,BA,C2,CA,D2 CCK; BPL1's holes shift four CCK.
Both BPL2 halves have the same holes. These positions precede the selected
plane's data fetch by one CCK, supporting a counter read/modify/write window
rather than a register-half-specific rule or global Copper render delay.

The OCS/ECS write path now discards a pointer half-write in that same-plane
window, preserving the pointer advanced by DMA. The attempted bus write still
reaches debugger/watch diagnostics, but neither the live pointer nor render
replay receives the lost value. Other planes and neighboring clocks are
unaffected. Alice's wider DMA pipeline is outside the A500 evidence and keeps
its existing behavior. No serialized state is added.

CPU register application happens after its two-CCK bus-cycle tail. The guard
therefore uses `cpu_custom_access_slot`, as render recording already does;
Copper uses the current slot. A rejected current-beam candidate repaired20
cases but regressed7 CPU/BPL1 cases. Its byte-identical trace identified that
phase error. The grant-anchored candidate repairs all24 qualified persistent
losses (71 fields), with zero field/qualification regressions. Thirteen early
collision-read differences remain a separate family.

The480-case matrix has281 qualifying rows and199 unchanged exclusions under
exact setup, CPU-write and both early/settled read brackets. Of these,111
candidate rows have independent repeated exact cohorts; zero conflicts does
not make all281 independently repeated. Four of the24 repaired rows have
independently repeated exact cohorts; the raw Copper hole pattern recurs across
both physical runs with differing later read brackets. Never count exclusions as matches.
The earlier800-case sweep retains its separate362-exclusion budget.

Versioned reference: `amiga-hardware-probes/references/pointer-collision-v2/run{1,2}`.
ROM SHA-256: `89e928cb35641a5e0d5519d6103ebcfc97a2d7ec206fab8feb9ac8a64c013aaf`.
Source fingerprint: `6386f8702df804478280c5f0639af82f9a4301d4817512fcfe02d6f1fde28aaf`.
The probe repository's `docs/accuracy-round-2026-10-20.md` links the observer,
rejected experiment and final validation artifacts.
