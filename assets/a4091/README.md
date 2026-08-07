# Bundled A4091 ROM

Copperline serves this autoboot ROM when a config fits an A4091 SCSI
controller (`[scsi] controller = "a4091"`) without naming a `rom` of its own
(see `src/romsearch.rs`). It is only needed to autoboot under AmigaOS; Linux
and NetBSD drive the 53C710 directly and never execute it.

## Files

| File | Size | Role |
|---|---:|---|
| `a4091_cdfs.rom` | 64 KiB | A4091 autoboot ROM (`a4091.device` + CDFS) |
| `THIRD_PARTY_NOTICES.txt` | 12 KiB | Exact source inventory and redistribution notices |

## Provenance

This file is the unmodified `a4091_cdfs.rom` release asset from A4091 software
v42.39 (commit `ce6d62cf2db5c6bf990e4d675d633f2dedcc84a6`):

https://github.com/A4091/a4091-software/releases/tag/v42.39

SHA-256:
`01ab100153e2faf2bc653e57e27db1672ceca81dd7c3d1c0b6d63ebe58dbe24b`.

The upstream repository does not declare one project-wide GPL license. The ROM
combines code under several redistribution notices, including A4091 driver
code, NetBSD-derived SCSI code, the mounter, ODFileSystem, and a ZX0
decompressor.
See `THIRD_PARTY_NOTICES.txt` for the exact component inventory, source
revisions, copyright notices, and redistribution terms shipped with every
packaged copy of the ROM.
