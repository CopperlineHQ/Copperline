# A500 CPU read turnaround before refresh

Kind65 in `amiga-hardware-probes` compares TST, MOVE and CMP operands in chip,
slow and ROM memory, with STOP/masked controls and four DMA modes. Two restored,
independent A500 captures agree on all 960 raw records. References are
`references/cpu-operand-v2/run{1,2}` (ROM SHA-256 `ead12d28a322a0ad9827d8b608a25c0d0e605a30d660ae0dc23cb85f59e25d3c`).

The previous emulator was eight colour clocks early at the handler in 96 cases:
chip/slow operands under six-plane DMA plus Copper. ROM operands, STOP and masked
controls match. This separates the error from a common IRQ-recognition delay.

The corrected stock OCS/ECS CPU grant waits when a DRAM read/fetch immediately
following Copper ownership would cross into line-end refresh. Register cycles
and writes retain their grant phase; 020 short-cycle and AGA scheduling are
unchanged. This is an arbitration condition, independent of instruction identity
or guest address. Qualification uses exact launch beams, without trace alignment.

Broader proposed guards were rejected: globally reserving the preceding slot,
delaying writes, and delaying reads without preceding Copper ownership all lost
valid launch controls or introduced new differences. Byte-identical diagnostic
replays locate one such recording write at hE1 and explain why delaying writes
invalidated the following source release. Rejected patches and reports remain in
the probe repository. The narrower rule repairs all 96 differences, with all 960
cases still qualified; existing CPU/DMA, bus marker and handoff contracts check
individual fields as well as whole-case results.

The captures constrain this PAL A500 transition, rather than establishing every
Gary/Agnus bus phase. Other video standards and chipset revisions require their
own boundary measurements. Investigation details and trace retrieval paths:
`amiga-hardware-probes/docs/cpu-operand-investigation-2026-10-19.md`.
