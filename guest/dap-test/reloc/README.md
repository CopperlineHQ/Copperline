# Relocatable ELF DAP probes

These tiny programs exercise two C compilation units, separate code hunks,
initialized data, BSS, and an intentional infinite loop after a function
call. `main.c` is linked first so AmigaDOS enters `entry` directly. The
source breakpoint in `worker.c` must stop before the loop.

The committed Hunk/ELF pairs were built with Bartman's
`m68k-amiga-elf-gcc` 15.1.0 and `elf2hunk`:

- `program-dwarf4`: DWARF 4, `-r -nostdlib`.
- `program-dwarf5`: DWARF 5, `-r -nostdlib`.
- `program-linked`: DWARF 5, fully linked with `--emit-relocs`.

Rebuild with `make` here, setting `ELF_CC` and `ELF2HUNK` if the tools are
not on PATH. Debug paths are relative to this directory. No SDK, ROM, or
cross-compiler is needed to run the tests against the committed fixtures
from the repository root:

```sh
cargo test --lib debuginfo
cargo test --release --test dap_stdio dap_binds_ -- --ignored
```

The unit tests check source/function/global hunk addresses and call-frame
information, including the second compilation unit. The DAP tests boot
the bundled AROS ROM and verify initial unbound breakpoints, LoadSeg
binding events, source-aware stack traces, and stops carrying the same
breakpoint ID. The linked fixture guards against relocating already
resolved debug bytes again.
