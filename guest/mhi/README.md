# Guest-side MHI decoder library

`mhi_copperline.library` is the guest (Amiga-side) driver for Copperline's
virtual MHI (Music Hardware Interface) MPEG audio decoder board, built for
AmigaAMP and other MHI-aware players. It is an ordinary disk-loaded AmigaOS
shared library -- unlike `guest/hostsocket` or `guest/services`, this board
has no boot ROM at all (`docs/internals/mhi.md`'s own "Zorro identity": "no
autoboot ROM"), so there is no DiagArea/Romtag-in-ROM dance here. AmigaAMP
finds it by scanning `LIBS:mhi/` for `mhi#?.library` (see `MHI-PLAN.md`'s WP1
notes) and `OpenLibrary()`s it exactly like any other library file.

## Provenance

The MHI front-end (library-structure boilerplate, the 10 `i_MHI*` entry
points, the semaphore-guarded single-decoder model) is ported from
[BlitterStudio/host-tools](https://github.com/BlitterStudio/host-tools),
commit `c14cf8c1be881d7157a0a051e3f6f4ed695c57d3`,
`drivers/mhi/src/{mhi_abi.h,mhiuae.h,mhiuae.c,mhiuae_startup.c}`
(`mhiuae.library`, GPL-3.0-or-later, Copyright 2020-2026 Dimitris
Panokostas). Every source file ported from there keeps its original
`SPDX-FileCopyrightText`/`SPDX-License-Identifier` header plus a comment
naming the exact upstream commit; see each file's own header for details.
The one substantive change throughout: every `UaeMHI*` trap call (a
`calltrap()` into Amiberry's `uae.resource`, the host resource mhiuae.library
was written against) is replaced by a call into `board.c`, which speaks
Copperline's own register protocol instead.

The MHI API constants (`mhi_abi.h`) are extended past the subset mhiuae.c
needed with the remainder of the official MHI developer kit's
`Include/libraries/mhi.h` (Aminet `driver/audio/mhi_dev.lha`, Paul Qureshi &
Thomas Wenzel) -- see `test-assets/mhi/NOTES.md` for how that dev kit was
fetched (WP1).

## Files

- `board.h`/`board.c` -- the entire hardware-specific surface: everything
  that knows the register offsets, bit layouts, and command values from
  `docs/internals/mhi.md` (THE CONTRACT) lives here and nowhere else, per
  MHI-PLAN.md WP4's "published-spec portability story". Deliberately free
  of any MHI-API vocabulary (`MHIF_*`/`MHIP_*`/`MHIQ_*`, a decoder handle, a
  signal mask) -- see `mhi.md`'s "The MHI-API/board split".
- `int_handler.s` -- the INT2 (`INTB_PORTS`) server's entry point. Hand-written
  assembly, not C: `AddIntServer`'s own Autodoc warns that a plain C
  function cannot reliably control the 68k Z flag on return, and a shared
  chain (real hardware shares `INTB_PORTS` with CIA-A) depends on getting
  that exactly right.
- `mhi_abi.h` -- the MHI API's own constants (`MHIF_*`/`MHIQ_*`/`MHIP_*`).
- `mhi_copperline.h`/`mhi_copperline.c` -- the 10 `i_MHI*` entry points:
  translates the MHI API (handles, signal masks, `MHIF_*`/`MHIP_*`
  constants) to and from `board.c` calls. `MHIQuery` answers per
  `mhi.md`'s "MHI-API/board split" table -- decoder identity and most
  capability flags are compile-time constants here; MPEG format/layer/
  bitrate-mode support reads the board's own `CAPS` register.
- `startup.c` -- `RTF_AUTOINIT` library boilerplate (`InitTab`/`FuncTab`,
  `Open`/`Close`/`Expunge`). Refuses to open if no MHI board is found (or
  its `VERSION` is older than this driver understands), mirroring
  mhiuae.c's own "no host resource, no library" gate.
- `test/` -- `mhitest`, a small CLI probe for WP5's M1 integration harness
  (see its own header comment for the exact "MHITEST: ..." output lines the
  harness greps for).

## Building

Needs Docker (the same image every guest build under `guest/` uses -- see
`guest/toolchain.mk`):

```sh
make          # -> mhi_copperline.library
make check    # objdump sanity check
make -C test  # -> test/mhitest
```

`mhi_copperline.library` (and `test/mhitest`) are committed artifacts,
rebuilt by hand when the source changes -- referenced directly by path from
`tests/mhi.rs` (WP5), the same convention `guest/hostfs-test/mkfile` already
uses (`tests/image_regression.rs`). There is no `assets/mhi/` counterpart:
unlike `guest/hostsocket`'s ROM or `guest/services`'s handler, nothing in
`src/` embeds this library via `include_bytes!` -- it is guest software a
test harness stages onto a boot volume, not something the host board itself
serves, so the committed artifact belongs in `guest/mhi/` directly.

## 68000 floor

Built with `-m68000`, the repo-wide floor (`AGENTS.md`'s `--cpu
68000..68060`; also what `guest/hostsocket`, `guest/services`, and
`guest/hostfs-test` all target) -- deliberately narrower than upstream
mhiuae.library's own `-mcpu=68020`, since nothing in the board-access layer
needs anything past plain 68000 word moves.
