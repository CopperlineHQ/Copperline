# HRTMon cartridge image

This directory builds Copperline's bundled HRTMon freezer-cartridge image,
`assets/hrtmon/hrtmon.rom`, from the maintained upstream source. HRTMon is
an Action-Replay-style system monitor by Alain Malek, maintained by Bert
Jahn (wepl) and contributors; Copperline fits it as `[cartridge] model =
"hrtmon"` (see `docs/guide/configuration.md`) and enters it with a level-7
interrupt on a freeze (`docs/internals/peripherals.md`, "Freezer
cartridge").

## Provenance

- Upstream: <https://github.com/wepl/hrtmon>, commit
  `3af8d5105f1e01ecc7961475568826e564995068` (HRTMon 2.39, 19 February
  2021).
- License: GNU General Public License, version 2 or (at your option) any
  later version, as stated at the top of `src/HRTmonV2.s`. The full text is
  kept beside the image in `assets/hrtmon/LICENSE`.
- Nothing from any other emulator is used: the image is assembled from the
  upstream source alone. WinUAE ships the same program built the same way,
  which is why its cartridge layout (bank at `$A10000`, entry at `+12`,
  custom-register snapshot at `$A9F000`) is what the source's `UAE`,
  `CARTRIDGE` and `SAVE_CUSTOM` switches produce.

## Build

Needs [vasm](http://sun.hasenbraten.de/vasm/) (`vasmm68k_mot`, the Motorola
syntax module) and the NDK assembler includes (`exec/*.i`,
`hardware/custom.i`, `devices/hardblocks.i`), plus `git` to fetch the pinned
source:

```sh
./hrtmon-rom/build.sh            # -> hrtmon-rom/build/hrtmon.rom
./hrtmon-rom/build.sh bundle     # ... and copy it to assets/hrtmon/hrtmon.rom
```

`VASM`, `NDK_INCLUDE` (default `/opt/amiga/m68k-amigaos/ndk-include`) and
`HRTMON_SRC` (an existing checkout at the pinned commit, instead of cloning)
override the tool locations.

The script never edits the checkout. It copies `src/HRTmonV2.s` into
`build/` and patches five lines with `sed`:

- `UAE = 1`: `ORG $A10000`, the UAE cartridge address.
- `CARTRIDGE = 1`: the cartridge header (the RTE address first, `HRT!` at
  `+4`, `bra.w mon_install` at `+8`, `bra.w monitor` at `+12`), the
  self-installing first entry, and the exit through the header's RTE.
- `SAVE_CUSTOM = 1`: the monitor reads the emulator's custom-register
  snapshot at `PTR_CUSTOM` (`$A9F000`) instead of the live registers.
- `OPT a-` after the `OPT o1+` of the source's vasm block: vasm's
  automatic absolute-to-PC-relative optimisation is switched off. The
  block opens with `MC68030`, which lets the optimiser rewrite the entry
  code's absolute operands as `(d16,PC)`; for `TST` that is a 68020+ form,
  and a 68000 or 68010 takes an illegal-instruction exception at the
  first freeze. The source raises and lowers the CPU itself around every
  privileged or 020+ instruction it means to use (`MC68010` ... `movec`
  ... `MC68000`) and spells out the PC-relative operands it wants, so
  with the optimiser off the shared code assembles for every CPU exactly
  as written. Upstream's own vasm build carries the same optimisation.
- HRTMon's own `BITDEF AF,68060,7` is wrapped in `IFND AFB_68060`: NDK 3.2
  and later define the flag in `exec/execbase.i` and vasm refuses the
  redefinition. The NDK 3.1 and 3.9 includes upstream targets do not have
  it, and the guard changes nothing there.

The `$VER` string embeds `.date`; the script writes the pinned commit's
date (`(19.02.2021)`) rather than the build day so the image is
reproducible. Everything else is upstream's own vasm flag set (the
`ASMBASE` line of its Makefile) with the binary output module.

Current bundled image:

- Size: 199,300 bytes (loaded at the start of the 1 MiB bank, the rest of
  the bank reads `$FF`).
- SHA-256: `184c9b12e7e83749d817b1c2692e5a86dffb84dc1bcda0d8c752650dfafe3d74`
  (vasm 2.0b, M68k backend 2.7c, NDK 3.2 includes).
- Header: `HRT!` at `+4`, version words `0002 0027` (2.39) at `+56`.
