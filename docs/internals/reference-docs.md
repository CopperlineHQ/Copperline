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

## Debugger ABI reference data

The live ROM symbol resolver uses only public ABI names from the AROS module
configuration files, generated into `assets/symbols/amigaos-lvo.tsv` by
`tools/generate-amigaos-lvos.py`. The current table is from AROS commit
`d13e9e537f9e6f53e5fc255899c0e234be5d5ee2`, also pinned in its header;
`assets/symbols/LICENSE.AROS` carries the AROS Public License 1.1. It contains
module names, LVO numbers, and public function names, never ROM addresses.

At runtime, Copperline obtains addresses from Exec's active library/device
lists and their negative `JMP abs.l` vectors, and obtains other ROM module
names and bounds from the live resident-tag list. This is intentionally the
entire address model: no Kickstart checksum/address map or undocumented
private-entry byte signature is accepted. A documented private signature can
be added later as a guarded pattern, but public `RawDoFmt` and `RawPutChar`
already resolve through their Exec LVOs.
