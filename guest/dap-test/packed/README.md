# Packed Hunk DWARF probes

These freestanding programs exercise the packed trailing `HUNK_DEBUG`
container written by AmigaPorts binutils (`amiga_pack_dwarf_sections` in
`bfd/amigaos.c`). It contains fourteen fixed-order sections, each with a
big-endian byte length and longword padding; missing sections have zero
length. The DWARF references are section-relative, unlike the older
symbol-delimited container.

The committed probes use GCC 16.2.0b and binutils from
`tinic/binutils-gdb` revision `ffe2ad82e34e11206efd706338b32a696185ebb4`.
They cover DWARF 2, 4 and 5, two compilation units, initialized data, BSS,
a stack parameter and local, source lines, and call-frame information.
No SDK or runtime library is linked. `entry` loops after calling `worker`.
DWARF 2 uses a `.debug_loc` frame-base list that the producer does not
retain in this container; its parameter must be reported unsupported.
DWARF 4/5 expose the parameter value, and all three expose the direct
register-relative local value.

Rebuild with `make AMIGA_CC=/path/to/m68k-amigaos-gcc`. The asset-free
`cargo test --no-default-features --features dap --lib debuginfo` tests read
the committed probes, so running the regression does not require an Amiga
cross-compiler, ROM or disk image.
