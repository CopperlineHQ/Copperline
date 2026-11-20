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
- Tinic Uro
- Seppo Tomperi
- TronMetatron

## Patreon sponsors

Copperline's development is supported by its patrons on
[Patreon](https://www.patreon.com/cw/Copperline). Monthly support pays for
development time, real hardware to measure the emulator against, and the
code-signing subscriptions - see [FUNDING.md](FUNDING.md) for what the money
does.

Thank you to:

- Lee Hobson
- Sphair
- Karlos_Fandango

## Bundled third-party code

- **[egui](https://github.com/emilk/egui)** provides the shared desktop
  inspector UI. Its bundled Hack (including the slashed-zero build in
  `assets/egui/hack-slash/`), Noto Emoji, Ubuntu Light, and emoji-icon-font
  typefaces retain their original licenses; the font notices and license texts
  are in `assets/egui/THIRD_PARTY_FONTS.txt` and `assets/egui/hack-slash/LICENSE.txt`
  and accompany desktop packages.
- **[AROS](https://github.com/aros-development-team/AROS)** public module
  configuration files provide the library/device LVO names used by the live
  ROM symbol resolver. The compact generated ABI table records its exact
  source revision and is distributed under the AROS Public License 1.1; the
  notice and license are kept in `assets/symbols/`.
- **[plmpeg](https://github.com/CopperlineHQ/plmpeg-rs)** provides the safe,
  pure-Rust incremental MPEG-1 video decoder used by the CD32 Full Motion
  Video module. Its reconstruction core descends from Native32Emu's
  BSD-3-Clause Rust port of Dominic Szablewski's MIT-licensed PL_MPEG; exact
  source revisions and both license notices are recorded in that repository.
- **[A4091 software](https://github.com/A4091/a4091-software)** provides the
  v42.39 autoboot ROM bundled as Copperline's default for an A4091, and
  contributes source components to Copperline's open A2091 autoboot ROM
  (`assets/a2091/copperline-a2091.rom`). Thanks to
  Stefan Reinauer, Chris Hooper, Toni Wilen, Matt Harlum, and the upstream
  NetBSD, Berkeley, OSF, ODFileSystem, and ZX0 contributors. Exact source
  revisions, component inventories, and redistribution notices are kept beside
  the ROMs in `assets/a4091/THIRD_PARTY_NOTICES.txt` and
  `assets/a2091/THIRD_PARTY_NOTICES.txt`.
- **[HRTMon](https://github.com/wepl/hrtmon)** by Alain Malek, maintained by
  Bert Jahn (wepl) and contributors, is the freezer-cartridge monitor bundled
  as `assets/hrtmon/hrtmon.rom` and fitted by `[cartridge] model = "hrtmon"`.
  It is assembled from the upstream source in its UAE cartridge
  configuration (`hrtmon-rom/`) under the GNU GPL, version 2 or later; the
  notice and license text are kept beside the image in
  `assets/hrtmon/LICENSE`.
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
- **[mt32-rs](https://github.com/CopperlineHQ/mt32-rs)** and
  **[Coppersynth](https://github.com/CopperlineHQ/coppersynth)** power the
  built-in Roland MT-32 emulation and General MIDI SoundFont synthesizer.
- **[Symphonia](https://github.com/pdeljanov/Symphonia)** provides pure-Rust
  MPEG audio decoding for the MHI board, MP3 cue-sheet tracks, and the CD32
  FMV module's Layer II path.
- **[Open Video Toaster](http://discreetfx.com/openvideotoaster.html)** (NewTek's
  2004 source release, hosted by DiscreetFX) provided the technical reference
  behind the Video Toaster 4000, Video Flyer, and `crates/flyerclip` VTASC
  codec implementations.
- **[MNT ZZ9000 SDK](https://source.mnt.re/amiga/zz9000-sdk)** defines the
  SDK v2 service contract modelled by the `[zz9k]` crypto accelerator board.
