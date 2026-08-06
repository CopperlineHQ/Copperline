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

## Patreon sponsors

Copperline's development is supported by its patrons on
[Patreon](https://www.patreon.com/cw/Copperline). Monthly support pays for
development time, real hardware to measure the emulator against, and the
code-signing subscriptions - see [FUNDING.md](FUNDING.md) for what the money
does.

Thank you to:

- Lee Hobson

## Bundled third-party code

- **[FluxBridge](https://github.com/CopperlineHQ/FluxBridge)**, CopperlineHQ's
  own pure-Rust library, is what lets a floppy bay drive a physical 3.5" drive
  over a Greaseweazle. It grew from a port of Rob Smith's
  [FloppyDriveBridge](https://github.com/RobSmithDev/FloppyDriveBridge) and
  records that provenance in its `NOTICE.md`; it is pinned by revision in
  `Cargo.toml` under `LGPL-3.0-or-later AND MPL-2.0`. Thanks to Rob Smith for
  the architecture and protocol knowledge it began from, and to Keir Fraser
  for the Greaseweazle itself.
