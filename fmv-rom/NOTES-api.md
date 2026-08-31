# CD32 FMV interface notes

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

## Video CD library surface

The following binary surface was recovered by tracing the original public
library calls with the Philips Media Retail Sampler '95 inserted. Function
names are descriptive names used by the open implementation; the LVO offsets,
register arguments, lifetime pairs, and return values are the observed ABI.
No original implementation code was copied.

| LVO | Open name | Register arguments | Result / behavior |
|---:|---|---|---|
| -30 | Reserved | none | Returns zero. |
| -36 | ClassifyDisc | none | Inspects `cd.device`; returns 4 for a Video CD. |
| -42 | OpenDisc | A0 source (normally zero), A1 tags | Returns an opaque parsed-disc handle. |
| -48 | CloseDisc | A0 disc handle | Releases the handle and its metadata. |
| -54 | DescribeItem | D0 item, A0 disc handle, A1 tags | Returns an allocated `TagItem` description. Item zero is the disc; items one onward are video tracks. |
| -60 | FreeDescription | A0 description | Releases a description returned by -54. |

The original disc description exposed tag values `0x80001065` (item kind),
`0x8000100a` (album/title), `0x80001067`/`0x80001068` (volume count/number),
`0x8000100c` (video-track count), and `0x80001070`/`0x80001071` (entry count
and LSN array). The open library preserves those values and adds documented
track-number, start/end-LSN, duration, and entry-range tags in
`src/videocd.h`.

The independent parser reads the White Book metadata sectors through public
`cd.device` commands: INFO.VCD at LSN 150 begins with `VIDEO_CD`; ENTRIES.VCD
at LSN 151 begins with `ENTRYVCD` and carries a big-endian entry count followed
by BCD track/MSF records. The implementation obtains track boundaries from
`CD_TOCLSN`, restores the drive's prior sector-size/read-speed configuration
on every exit path, and allocates no large static buffers in cartridge RAM.

The real-media probe observed classifier 4, album `3106906332`, two video
tracks, and 45 entry points. Its first video track spans LSN 3450 through
4052, matching the original player's eight-second menu entry.

## Video CD boot and player contract

The ROM's version 41 `cdstrap` replaces the CD32 extended ROM's lower-version
resident through the same name/version/priority rule used by an expansion ROM.
It claims only media classified as a Video CD. For every other disc it invokes
the displaced version 40 init entry, preserving normal CD32 game boot.

For Video CDs the strap starts a task which opens the library, builds a track
list from the allocated descriptions, and opens `cd32mpeg.device`. Up/down
select a track, Red submits one asynchronous `MPEGCMD_PLAYLSN`, and Blue calls
`AbortIO`, waits for completion, hides the decoder output, and redraws the
menu. `iomr_Arg1` is 2328 and `iomr_Arg2` repeats the sequence's starting LSN,
matching the public CD32 MPEG request contract. The player task runs above the
decoder worker so controller input remains serviceable while ready CD sectors
stream without host-time throttling.

## Firmware dependency and open-image boundary

The AROS driver checks a big-endian `0xC3C301FD` signature at cartridge offset
`0x761C`, reads the entry/base/chunk count at `0x7622`, `0x7624`, and `0x762A`,
zeros CL450 IMEM/TMEM and cartridge RAM, uploads each described chunk, then
starts the CL450 CPU. Copperline does not execute uploaded CL450 code and
signals firmware readiness when `CPU_CONTROL` is enabled. Consequently an
empty container is sufficient under Copperline while remaining clearly
incapable of driving real CL450 hardware.
