# Cannon Fodder FMV interface notes

Source observed: AROS pull request 1089, device commit `45e3cb9` plus CDXL
ordering commit `ebfc7d9`, dated 2026-08-29 to 2026-08-30.
These notes record public interface and hardware behavior; no Commodore ROM
code or CL450 microcode is included.

## Implemented device surface

The AROS `cd32mpeg.device` implements the minimum request subset used by the
CD32 Cannon Fodder introduction:

| Command | Value | Observed behavior |
|---|---:|---|
| `MPEGCMD_GETDEVINFO` | 15 | Returns type `0x006f0000` and `CD32 MPEG Module`. |
| `MPEGCMD_SETVIDEOPARAMS` | 19 | Accepts a four-byte parameter pair. |
| `MPEGCMD_PLAYLSN` | 21 | Streams the requested LSN range asynchronously. |
| `AbortIO` | device vector 6 | Marks an active stream aborted. |

`IOMPEGReq` is 86 bytes. The MPEG-specific tail contains a signed MPEG error,
stream-type word, flags, seven 32-bit arguments, and one reserved word.

## CD and decoder path

- `cd.device` is configured for 2328-byte Mode-2 transfers, `CD_READXL` at
  speed 75, and no XL ECC.
- A 32-node continuous `CD_READXL` ring feeds sectors to the MPEG worker.
- Audio PES/system data is written to the L64111 port at board `+0x050000`.
- Video PES payload bytes are written to the CL450 port at board `+0x060000`.
- The CL450 is initialized with threshold, border, PAL video format, interrupt
  mask, unblank, and play commands; the L64111 is configured and unmuted.
- The cartridge is found through `FindConfigDev(514, 106)` and normally maps
  at the configured board address rather than assuming `0x200000`.

The standalone cartridge resident implements the same request surface but
uses standard `CD_READ` one 2328-byte Mode-2 sector at a time. This works with
Kickstart's host `cd.device` and avoids shipping a replacement CD driver. AROS
PR 1089 uses its continuous `CD_READXL` implementation instead; commit
`ebfc7d9` drains PBX snapshots chronologically by raw-sector MSF.

## Firmware dependency and open-image boundary

The AROS driver checks a big-endian `0xC3C301FD` signature at cartridge offset
`0x761C`, reads the entry/base/chunk count at `0x7622`, `0x7624`, and `0x762A`,
zeros CL450 IMEM/TMEM and cartridge RAM, uploads each described chunk, then
starts the CL450 CPU. Copperline does not execute uploaded CL450 code and
signals firmware readiness when `CPU_CONTROL` is enabled. Consequently an
empty container is sufficient under Copperline while remaining clearly
incapable of driving real CL450 hardware.
