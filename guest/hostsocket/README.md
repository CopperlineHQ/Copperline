# Guest-side bsdsocket.library stub

`hostsocket_rom.bin` (committed under `assets/hostsocket/`) is the m68k code
the bundled HostSocket board serves as its `diag_vec` autoboot ROM. It is
deliberately tiny: all socket semantics live in the plugin
(`crates/hostsocket-plugin/`); this stub only installs itself as
`bsdsocket.library` and stages LVO calls through the board window to the
plugin's `write()` doorbell.

`entry.s` holds everything -- the entry table, DiagArea, `rt_Init`-deferred
`MakeLibrary()`/`AddLibrary()` library-build logic, and all 38
bsdsocket.library LVO trampolines, plus the blocking-call `Wait()`/`Signal()`
path and the interrupt handler that drives it, all hand-written assembly.
`stub.c` holds `hs_get_board_base()`, the one routine plain C is actually
safe for here (see its own header comment for why: each LVO's arguments
arrive in specific, call-number-dependent registers per the real
`bsdsocket_lib.fd`, not gcc's C calling convention, so the LVO trampolines
themselves still can't be plain C without their own new class of bug --
`rt_Init`'s calling context has no such per-call-number register contract to
get wrong). The entry-table/DiagArea structure is adapted from
`guest/services/entry.s`, not hand-written from scratch, because that file
documents a real, easy-to-reintroduce PC-relative miscompilation bug it took
real debugging to find (`entry.s`'s own header comment explains it). This
library's `ln_Type` is `NT_LIBRARY` (a plain Exec library via
`MakeLibrary()`/`AddLibrary()`), not `NT_DEVICE` like the hostfs handler's
DOS-mount path -- simpler in that respect, since there's no DOS-list surgery
involved (a real boot-safety deferral *is* still needed here, though: see
`entry.s`'s own header comment on why `da_DiagPoint` can't build the library
synchronously).

## Building

Needs Docker (same image every guest build under `guest/` uses -- see
`guest/toolchain.mk`):

```sh
make        # -> ../../assets/hostsocket/hostsocket_rom.bin
```

The ROM must be position-independent and self-contained, same rules as the
other guest ROMs: `-mpcrel` for PC-relative code/data, and the Makefile
fails the build if the linked executable carries relocations or a data/bss
hunk.
