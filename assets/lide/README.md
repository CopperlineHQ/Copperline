# Bundled lide.device ROMs

Copperline serves these ROMs when a `[lide]` board is fitted (an explicit
`board`, or a drive image) without naming a `rom`/`rom_bank2` of its own (see
`src/romsearch.rs`). Set `rom = ""` to opt back out and keep the board in
hardware-only mode -- no autoboot, drives still work under a disk-loaded
`lide.device`.

## Files

| File | Size | Role |
|---|---:|---|
| `lide.rom` | 32 KiB | Autoboot ROM for CIDER/RIPPLE/RIDE |
| `lide-atbus.rom` | 32 KiB | Autoboot ROM for AT-Bus 2008 and clones |
| `cdfs.rom` | ~31 KiB | Second flash bank: ODFileSystem, for booting from CD (RIPPLE/RIDE only) |
| `THIRD_PARTY_NOTICES.txt` | — | Exact source, license, and redistribution notices |

`lide.rom` and `lide-atbus.rom` are **not interchangeable**, despite both
being 32 KiB: upstream links them with different scripts (`bootrom/rom.ld`
vs `bootrom/atbusrom.ld`). `lide.rom` opens with a 4-byte `"LIV2"` header
before the bootloader; `lide-atbus.rom` starts the bootloader straight at
offset 0. This matches the different `diag_vec` Copperline's own
`zorro::BoardSpec::lide` already uses per personality (`0x0008` for
RIPPLE/RIDE, `0x0001` for AT-Bus 2008) -- board = "ripple"/"ride" default to
`lide.rom`, board = "atbus2008" defaults to `lide-atbus.rom`. `cdfs.rom` only
applies to boards with flash banking (RIPPLE, RIDE); it is never defaulted
for `board = "atbus2008"`, which has none.

## Provenance

All three files are the unmodified release assets from `LIV2/lide.device`
`Release-40.12` (commit `a387f65b3a0b9a206674c1a391365ea9f9d39e90`):

https://github.com/LIV2/lide.device/releases/tag/Release-40.12

- `lide.rom` SHA-256:
  `e151f10b75678537ab93144d80c26bf3dde2aabad1305d64428f95974b893b37`
- `lide-atbus.rom` SHA-256:
  `7a93a26683a50488cda25af79e248ba39c26f94c77e710bb4d70d15de77508ef`
- `cdfs.rom` SHA-256:
  `cccd66446120d274c40b15d352c94786421bb405b48c950b2e9818335e27c138`

`lide.rom` and `lide-atbus.rom` are lide.device's own code (GPL-2.0-only).
`cdfs.rom` is not LIV2's: it is fetched verbatim by their release build from
[reinauer/ODFileSystem](https://github.com/reinauer/ODFileSystem) `v0.7.0`
(BSD-2-Clause). See `THIRD_PARTY_NOTICES.txt` for the exact license texts.
