# Bundled open A2091/A590 ROM

Copperline uses `copperline-a2091.rom` when `[scsi] controller = "a2091"`
does not name another ROM. The 64 KiB image contains the clean-room loader,
`scsi.device` 42.40, WD33C93/DMAC transport, and RDB automounter built from
the sources in `a2091-rom/`.

| File | Size | SHA-256 |
|---|---:|---|
| `copperline-a2091.rom` | 65,536 bytes | `4bf1ab8411ac19b360d06c59f4640ddc8f430b55e616927cd31d20b77a7fd3f4` |

The image is board-linear from A2091 offset `$2000`; its first 8 KiB are
erased because that physical range is shadowed by the board registers. For
EPROM programming, build the U13-even and U12-odd halves with
`make -C a2091-rom`; those hardware outputs are intentionally not installed
as runtime assets.

No Commodore ROM code is included. See `THIRD_PARTY_NOTICES.txt` and
`a2091-rom/NOTES-api.md` for provenance and the clean-room boundary.
