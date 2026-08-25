# Credits

Copperline is written and maintained by Andrew "LinuxJedi" Hutchings.

## Contributors

Code contributions from (the full list is on
[GitHub](https://github.com/CopperlineHQ/Copperline/graphs/contributors)):

- Bernie Innocenti
- Lee Hobson
- jbl007
- Simon Dick
- Nicolas Ramz
- Ben Letchford
- Volker Schwaberow
- Matt Harlum

## Patreon sponsors

Copperline's development is supported by its patrons on
[Patreon](https://www.patreon.com/cw/Copperline). Monthly support pays for
development time, real hardware to measure the emulator against, and the
code-signing subscriptions - see [FUNDING.md](FUNDING.md) for what the money
does.

Thank you to:

- Lee Hobson

## Bundled third-party code

- **[plmpeg](https://github.com/CopperlineHQ/plmpeg-rs)** provides the safe,
  pure-Rust incremental MPEG-1 video decoder used by the CD32 Full Motion
  Video module. Its reconstruction core descends from Native32Emu's
  BSD-3-Clause Rust port of Dominic Szablewski's MIT-licensed PL_MPEG; exact
  source revisions and both license notices are recorded in that repository.
- **[A4091 software](https://github.com/A4091/a4091-software)** provides the
  v42.39 autoboot ROM bundled as Copperline's default for an A4091. Thanks to
  Stefan Reinauer, Chris Hooper, Toni Wilen, Matt Harlum, and the upstream
  NetBSD, Berkeley, OSF, ODFileSystem, and ZX0 contributors. The exact source
  revisions, component inventory, and redistribution notices are kept beside
  the ROM in `assets/a4091/THIRD_PARTY_NOTICES.txt`.
- **[lide.device](https://github.com/LIV2/lide.device)** by Matt Harlum
  (LIV2) provides the autoboot ROM and CD-filesystem bank bundled as
  Copperline's default for a fitted `[lide]` board. `cdfs.rom` is Stefan
  Reinauer's [ODFileSystem](https://github.com/reinauer/ODFileSystem),
  fetched by lide.device's own release build. Exact source revisions and
  redistribution notices are kept beside the ROMs in
  `assets/lide/THIRD_PARTY_NOTICES.txt`.
- **[FluxBridge](https://github.com/CopperlineHQ/FluxBridge)**, CopperlineHQ's
  own pure-Rust library, is what lets a floppy bay drive a physical 3.5" drive
  over a Greaseweazle. It grew from a port of Rob Smith's
  [FloppyDriveBridge](https://github.com/RobSmithDev/FloppyDriveBridge) and
  records that provenance in its `NOTICE.md`; `Cargo.toml` tracks its `main`
  branch and `Cargo.lock` pins the exact revision under
  `LGPL-3.0-or-later AND MPL-2.0`. Thanks to Rob Smith for the architecture and
  protocol knowledge it began from, and to Keir Fraser for the Greaseweazle
  itself.
