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

## Patreon sponsors

Copperline's development is supported by its patrons on
[Patreon](https://www.patreon.com/cw/Copperline). Monthly support pays for
development time, real hardware to measure the emulator against, and the
code-signing subscriptions - see [FUNDING.md](FUNDING.md) for what the money
does.

Thank you to:

- Lee Hobson

## Bundled third-party code

- **[A4091 software](https://github.com/A4091/a4091-software)** provides the
  v42.39 autoboot ROM bundled as Copperline's default for an A4091. Thanks to
  Stefan Reinauer, Chris Hooper, Toni Wilen, Matt Harlum, and the upstream
  NetBSD, Berkeley, OSF, ODFileSystem, and ZX0 contributors. The exact source
  revisions, component inventory, and redistribution notices are kept beside
  the ROM in `assets/a4091/THIRD_PARTY_NOTICES.txt`.
- **[FluxBridge](https://github.com/CopperlineHQ/FluxBridge)**, CopperlineHQ's
  own pure-Rust library, is what lets a floppy bay drive a physical 3.5" drive
  over a Greaseweazle. It grew from a port of Rob Smith's
  [FloppyDriveBridge](https://github.com/RobSmithDev/FloppyDriveBridge) and
  records that provenance in its `NOTICE.md`; `Cargo.toml` tracks its `main`
  branch and `Cargo.lock` pins the exact revision under
  `LGPL-3.0-or-later AND MPL-2.0`. Thanks to Rob Smith for the architecture and
  protocol knowledge it began from, and to Keir Fraser for the Greaseweazle
  itself.
