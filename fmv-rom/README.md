# Open CD32 FMV ROM

This directory builds Copperline's deterministic, freely redistributable
256 KiB replacement ROM for the CD32 Full Motion Video cartridge. The image
contains a valid expansion DiagArea, a clean-room `cd32mpeg.device`, and an
empty CL450 firmware container. It contains no Commodore code and no
proprietary CL450 microcode.

The resident implements the Cannon Fodder request subset recovered from
[AROS pull request 1089](https://github.com/aros-development-team/AROS/pull/1089):
device discovery/open, device information, video parameters, asynchronous
`PLAYLSN`, and abort. It initializes the CL450 and L64111 through their public
register interfaces and streams 2328-byte Mode-2 sectors through the host
system's standard `cd.device` `CD_READ` command. Consequently the same image
runs Cannon Fodder under CD32 Kickstart 3.1 without containing a replacement
`cd.device` or `cdstrap`.

AROS PR 1089 instead carries a matching system-ROM `cd32mpeg.device` and
deliberately skips the cartridge diagnostic to prevent Commodore's legacy ROM
from displacing AROS's own `cd.device`. It still reads this image's empty CL450
container. Copperline marks its command-level CL450 model ready when
`CPU_CONTROL` is enabled and never executes uploaded IMEM/TMEM, which is the
documented emulator-only boundary that makes the empty container sufficient.

## Build, test, and refresh the bundle

```sh
make check
make
make bundle
```

The pinned Amiga GCC toolchain builds `build/cd32mpeg.device`; `hunk.py`
relocates its code/data into the cartridge ROM and its BSS into the module's
reserved RAM. `build_rom.py` writes `build/copperline-fmv.rom`, checks the
fixed layout, and pads unused space with `0xFF`. `make bundle` copies the exact
output to `assets/fmv/copperline-fmv.rom`.

The build takes no proprietary ROM or media as input. Current bundled image:

- Size: 262,144 bytes.
- SHA-256: `44378167fa07f5260e9f0baea9d737672e9fe395b8fa6cd25cd9654d04649aa7`.
- DiagArea: offset `0x80`, name `config_mpeg`; its diagnostic entry installs
  the cartridge's autoinit resident on systems which execute it.
- Resident: `cd32mpeg.device` 41.0, located from the configured board with
  `FindConfigDev(514, 106)`.
- CL450 container signature: `0xC3C301FD` at `0x761C`, with zero entry, base,
  and chunks.

The source and generated ROM are GPL-3.0-or-later. See the repository root
`LICENSE` and `assets/fmv/README.md`.

## AROS draft-PR integration

PR 1089's first `CD_READXL` implementation drains ready Akiko PBX slots by
slot number. Akiko fills the highest armed slot first, so a re-armed high slot
can contain a newer sector while an older low slot is still pending. The AROS
author incorporated Copperline's fix as PR commit
[`ebfc7d9`](https://github.com/aros-development-team/AROS/commit/ebfc7d9d9c263f1b7accf046957149927bc6dfb8):
each PBX snapshot is drained by the raw sector's absolute MSF. This preserves
the hardware's high-bit-first arbitration while delivering chronological CDXL
data. [AROS-CDXL-ORDERING.md](AROS-CDXL-ORDERING.md) records the race timeline
and before/after validation evidence.

## Validation and scope

On 2026-08-30, deterministic 60-second release runs against the local Cannon
Fodder disc passed on both supported boot environments:

- CD32 Kickstart 3.1 r40.60 plus its extended ROM used this cartridge's own
  resident and standard Mode-2 reader: 735 decoded 352x288 frames, no malformed
  MPEG reset, full-colour output, and non-silent stereo before gameplay.
- AROS PR 1089 through ordering commit `ebfc7d9` used AROS's system resident:
  more than 1,000 decoded 352x288 frames, no malformed reset, full-colour FMV,
  and non-silent stereo.

Video Creator v1.1 reaches the same application screen as the proprietary ROM
and opens the decoder. A Philips Video CD reaches only the stock CD32 boot
screen with this ROM; the proprietary cartridge's `videocd.library`, player,
and strap are intentionally outside the game-ROM milestone. They remain the
optional Video CD phase documented in `FMV-ROM-REPLACEMENT-PLAN.md`.

See [NOTES-api.md](NOTES-api.md) for the recovered request ABI and the root
[FMV-ROM-REPLACEMENT-PLAN.md](../FMV-ROM-REPLACEMENT-PLAN.md) for design and
remaining optional work.
