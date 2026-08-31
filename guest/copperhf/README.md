# copperhf.device boot ROM

The 68k side of `copperhf.device` (see `COPPERHF-DEVICE-PLAN.md`'s M2):
a DiagArea + Romtag that builds an exec device against the register
protocol in `copperhf_board.h`, served read-only from the board window's
`0x0000-0x3FFF` range (`src/copperhf.rs::COPPERHF_ROM` /
`crate::zorro::BoardSpec::copperhf`'s `diag_vec`).

- `entry.s` -- entry table, DiagArea, and Romtag (adapted from
  `guest/services/entry.s` / `guest/hostsocket/entry.s`).
- `device.c` -- device construction (MakeLibrary + AddDevice, deferred to
  rt_Init) and the Open/Close/Expunge/BeginIO/AbortIO vectors.
- `int_handler.s` -- the INT2 completion-drain server (must be assembly:
  see its own header comment on the Z-flag contract).
- `copperhf_board.h` -- the register map, shared with `src/copperhf.rs`.

No partition mounter yet -- that is M3. A program that already knows the
device's name can `OpenDevice("copperhf.device", unit, ...)` and drive it,
but nothing here makes an attached unit appear as a mounted volume.

## Building

```sh
make
```

Needs Docker (`stefanreinauer/amiga-gcc:gcc-v16.1`, pulled automatically);
see `guest/toolchain.mk`. Installs `assets/copperhf/copperhf_rom.bin`. A
plain `cargo build` never needs Docker -- it embeds the committed artifact
via `include_bytes!`.

The build fails if the linked executable has any relocations or a
`.data`/`.bss` section (the ROM must be pure PC-relative code -- see
`entry.s`'s header comment for the two hard-won traps that discipline
guards against) or if it exceeds the 0x4000-byte budget below the
register block (`CHF_MAGIC` in `copperhf_board.h`).
