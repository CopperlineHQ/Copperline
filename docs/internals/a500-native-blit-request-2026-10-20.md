# Native BLIT request readback after the terminal E cycle

A status-first A500 probe (kind71) reads DMACONR before its first beam sample,
then a second status and INTREQR, retaining beam brackets and settled data.
Its1536-case matrix covers AD widths1/2/16, normal/hog priority, display-DMA,
no-start controls and neighboring instruction offsets. Both physical runs
restore the previous OneROM state and verify reset release.

Baseline b8a97ac6 matches1328 cases;208 differ. Two hundred have different
observation beams, so their busy-status differences do not isolate an event
edge. Eight sixteen-word, normal-priority/no-display-DMA cases have matching
observation beams but expose INTREQ.BLIT one colour clock late. The no-start
controls match. This evidence separates the native request-readback boundary
from the still-unresolved CPU grant/handoff family.

The native countdown now exposes the request three CCK after E starts, two
CCK after its slot finishes. The E slot consumes the initial countdown clock.
Reducing the native source pre-age by the same one CCK keeps the absolute CPU
recognition deadline fixed. Software/Copper INTREQ writes and
DEFAULT_IRQ_LATENCY_CCK are unchanged. Delayed terminal D writes remain
independent of the source countdown; fixed-DMA tests exercise this ordering.

The candidate repairs all eight fields without new field or qualification
regressions. The remaining200 arbitration differences are retained. Neighbor
checks cover operand/CPU-DMA, onset/completion/pressure and handoff read/write
matrices. Two older kind50 request snapshots also become matches at equal
beam brackets; their incomplete restoration provenance makes them historical
corroboration, not the primary evidence.

Versioned packets: `amiga-hardware-probes/references/handoff-edge-v2/run{1,2}`.
ROM SHA-256: `50a54642b7a32579e55895460fd8cc0e557022848a1304e1f76a9cc73e2ab22f`.
Source fingerprint: `3f8596f74d419cb1185867d62154bf020c7d39b7071c25fccd3080bcb17b3fef`.
The probe repository's `docs/accuracy-round-2026-10-20.md` records contracts,
remaining families and validation artifacts.
