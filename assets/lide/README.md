# Bundled lide.device ROM

Copperline serves these ROMs when a `[lide]` board is fitted (an explicit
`board`, or a drive image) without naming a `rom`/`rom_bank2` of its own (see
`src/romsearch.rs`). Set `rom = ""` to opt back out and keep the board in
hardware-only mode -- no autoboot, drives still work under a disk-loaded
`lide.device`.

## Files

| File | Size | Role |
|---|---:|---|
| `lide.rom` | 32 KiB | Autoboot ROM: AutoConfig DiagArea + `lide.device` |
| `cdfs.rom` | ~31 KiB | Second flash bank: ODFileSystem, for booting from CD |
| `THIRD_PARTY_NOTICES.txt` | — | Exact source, license, and redistribution notices |

`lide.rom` is the CIDER/RIPPLE/RIDE build from upstream's release table; it
also serves as Copperline's default for `board = "atbus2008"` (AT-Bus 2008
and clones), whose own upstream build differs only in how the same ROM
content is padded and repeated to fill a larger physical flash chip, not in
driver code -- a distinction that only matters when writing to real
hardware. `cdfs.rom` only applies to boards with flash banking (RIPPLE,
RIDE); it is never defaulted for `board = "atbus2008"`, which has none.

## Provenance

Both files are the unmodified release assets from `LIV2/lide.device`
`Release-40.12` (commit `a387f65b3a0b9a206674c1a391365ea9f9d39e90`):

https://github.com/LIV2/lide.device/releases/tag/Release-40.12

- `lide.rom` SHA-256:
  `e151f10b75678537ab93144d80c26bf3dde2aabad1305d64428f95974b893b`
- `cdfs.rom` SHA-256:
  `cccd66446120d274c40b15d352c94786421bb405b48c950b2e9818335e27c1`

`lide.rom` is lide.device's own code (GPL-2.0-only). `cdfs.rom` is not
LIV2's: it is fetched verbatim by their release build from
[reinauer/ODFileSystem](https://github.com/reinauer/ODFileSystem) `v0.7.0`
(BSD-2-Clause). See `THIRD_PARTY_NOTICES.txt` for the exact license texts.
