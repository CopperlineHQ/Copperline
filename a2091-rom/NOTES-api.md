# Clean-room A2091/A590 interface notes

This file records interface observations used to implement the open ROM. It
contains no Commodore implementation code. The proprietary A2091 7.0 dump was
used only to confirm public OS/board conventions; it is not part of the tree.

## Device and unit ABI

- The Exec device is `scsi.device`. If that name already exists, the driver
  chooses `2nd.scsi.device`, `3rd.scsi.device`, and so on.
- Unit numbering is `target + 10 * lun + 100 * board`; target 7 is the host.
- The retained DiagArea contains one cold-start `NT_DEVICE` Resident at
  priority 10. The driver obtains its board through `GetCurrentBinding()`.
- Kickstart V36+ mounts RDB partitions through `AddBootNode`. V34 uses the
  mounter's hand-built `BootNode`/`Enqueue` path and the standard BootPoint
  that starts the `dos.library` resident.

## DiagArea facts

The local 7.0 interface reference has its DiagArea at image/board offset
`$2000`: config `$90` (`DAC_WORDWIDE | DAC_CONFIGTIME`), size `$88`,
BootPoint `+$0E`, name `+$24`, DiagPoint `+$40`, and Resident `+$6E`.
Kickstart copies the complete DiagArea before calling DiagPoint, so every
header pointer and the Resident are self-contained in `da_Size`.

The replacement uses the same public Expansion Library contract but a new
loader and driver. Its payload is board-linear from `$2000`; the build tool
rotates the bytes for 16/32/64 KiB physical images and writes U13-even then
U12-odd split halves.

## Board contract

- Autoconfig identity is supplied by the DMAC: manufacturer 514, product 3
  (product 2 is accepted on hardware), Zorro II 64 KiB, DiagArea `$2000`.
- WD33C93 ports are SASR/ASR `$91` and SCMD `$93`. Host ID is 7, asynchronous
  transfer mode, and the selection timeout register is kept small.
- DMAC registers used are ISTR `$40`, CNTR `$42`, ACR `$84/$86`, DAWR `$8E`,
  and ST_DMA/SP_DMA/CINT/FLUSH at `$E0/$E2/$E4/$E8`.
- The DMAC masters an even word stream in the 24-bit address space. Buffers
  that are odd, odd-sized, wrap, or end above `$01000000` must use PIO or a
  Chip RAM bounce buffer.
- A WD combination command posts command-complete CSR `$16`, followed closely
  by disconnect CSR `$85`. The interrupt bridge therefore queues status causes
  rather than storing a single byte.

## Sources used as specifications

- Copperline `src/a2091.rs` and `src/scsi.rs`, with their hardware regression
  tests and the WD33C93/DMAC register behavior exercised by the original ROM;
- WD33C93A data sheet and Commodore A2091/A590 schematics;
- NetBSD `sys/arch/amiga/dev/atzsc.c` and `sys/dev/ic/wd33c93*`;
- AROS expansion/romboot code for the Kickstart-compatible DiagArea lifecycle;
- A4091 software v42.39 and its mounter for the open device/relocator/RDB base.

Linux A2091 sources were read only to cross-check public register constants
and the bounce-buffer rule. No GPL-2.0-only Linux code was copied into this
BSD source tree.
