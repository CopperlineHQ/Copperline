# AROS PR 1089 CDXL ordering fix

Upstream commit:
[`ebfc7d9`](https://github.com/aros-development-team/AROS/commit/ebfc7d9d9c263f1b7accf046957149927bc6dfb8)
(`cd: preserve CDXL sector arrival order`).

## Problem

Akiko transfers a sector to the highest armed PBX slot and clears that slot's
bit. The first `CD32_CmdReadXL()` implementation took a PBX snapshot, copied
its cleared slots from bit 15 downwards, then re-armed the copied slots. DMA
remained active while the task copied a snapshot.

This permitted the following normal interleaving:

1. The task snapshots slot 15 as filled.
2. While it copies slot 15, the next sector arrives in slot 14.
3. The task re-arms only slot 15 from its old snapshot.
4. The following sector arrives in the newly armed slot 15.
5. The next snapshot contains the older sector in slot 14 and the newer one
   in slot 15; draining high-to-low swaps them.

Cannon Fodder reached this interleaving repeatedly during its FMV
introduction. The first observed swap was around disc sectors 27779-27780 and
made a valid MPEG-1 stream fail with `coefficient run exceeds block`.

## Fix

Commit `ebfc7d9` orders the filled slots in each PBX snapshot by the absolute
MSF stored in bytes 12-14 of the raw sector header. It does not change Akiko
arbitration, transfer pacing, or the re-arm contract; it only makes
`CD_READXL` copy the snapshot in chronological disc order.

## Validation

The same deterministic 60-second Copperline run used AROS PR 1089, the open
FMV ROM in this directory, and the Cannon Fodder CD:

| Build | Decoded 352x288 frames | Malformed MPEG resets | Rejected FMV writes |
|---|---:|---:|---:|
| Initial draft PR 1089 | 1,106 | 3 | 0 |
| PR 1089 with `ebfc7d9` | 1,134 | 0 | 0 |

The ordered run also produced a full-colour live-action screenshot and a
non-silent 44.1 kHz stereo WAV. Rolling hashes of the CL450 input matched an
offline demultiplex of the Mode-2 track beyond the first former corruption
point; the standalone MPEG-1 decoder decoded all 4,142 frames from that
offline stream without error.
