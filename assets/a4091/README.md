# Bundled A4091 ROM

Copperline serves this autoboot ROM when a config fits an A4091 SCSI
controller (`[scsi] controller = "a4091"`) without naming a `rom` of its own
(see `src/romsearch.rs`). It is only needed to autoboot under AmigaOS; Linux
and NetBSD drive the 53C710 directly and never execute it.

## Files

| File              | Size    | Role                              |
|-------------------|---------|-----------------------------------|
| `a4091_cdfs.rom`  | 64 KiB  | A4091 autoboot ROM (`a4091.device` + CDFS) |

## Provenance

`a4091.device` / `A4091 scsidisk` 42.39 (30.7.2026), from the open-source
A4091 software project: https://github.com/A4091/a4091-software

The project is GPL-licensed and freely redistributable, so unlike a real
Kickstart the ROM can legally ship with Copperline.
