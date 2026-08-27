# In-repo reference documents

Timing behaviour is documented next to the code, backed by named
regression tests. Consult and update these when changing the corresponding
model. The Copper and blitter timing models (fetch cadence, MOVE write
boundary, WAIT/SKIP edge cases, the per-slot blitter FSM, mid-blit
register classification, area fill, ECS extensions, and the known
residuals) are documented in [](timing.md), which also covers the real-mode
pacing model (`cycles` vs `instructions`); the 68000 prefetch queue and the
020+ cache model are in [](cpu.md). Full ECS, the A600/A1200 machine profiles
and Gayle, and the AGA display path are implemented; their remaining gaps
are recorded next to the subsystem they belong to ([](chipset.md), [](video.md),
and [](cpu.md)). The remaining reference material lives in the repository:

`timing-test/`
: Not a document but the measurement tool behind several of them: a
  bootable disk that times CPU/chip-bus operations against the CIA
  E-clock, comparable across Copperline, vAmiga, FS-UAE, and real
  hardware.

`../index.md`
: The public project overview, including the hardware-first compatibility
  principle: model the chip behaviour instead of branching on individual
  software titles.
