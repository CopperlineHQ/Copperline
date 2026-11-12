# Video Toaster and Flyer: working specs

Notes recovered from NewTek's released sources (switcher, CG, project
editor, Flyer firmware and flyer.library) while emulating the Video
Toaster 4000 and the Video Toaster Flyer in Copperline. Each claim carries
a source reference and a confidence tag. Source paths use these roots:

- `<vt-src>/`: the Video Toaster source release (AmigaVideoToasterComplete:
  `full/switcher`, `full/CG`, `old/...`), from
  [Open Video Toaster](http://discreetfx.com/openvideotoaster.html).
- `<ovt-src>/`: the open-video-toaster-src archives (`flyer/` firmware,
  `amiflyer/` flyer.library, `Edit/` project editor, `DSP_Code/`), from the
  same place.
- `<installed>/`: an installed Toaster 4.2 system (the `NewTek` tree the
  emulator boots), not included here.

- `toaster-protocol.md`: how the card talks to the Amiga (header lines,
  extended headers, the parallel port read-back).
- `video-path-spec.md`: the video path: muxes, fader, keys, framestores,
  which Amiga bits each per-pixel mode reads.
- `dve-spec.md`: the digital effects (framestore addressed per line and
  per quad by the Amiga picture).
- `key-align.md`: why key pages sat 16 samples off (the AA zipper write
  and EH0 LUST).
- `cg-titles.md`: making ToasterCG title pages from outside and keying
  them over Program.
- `flyer-spec.md`: the Flyer board, its command mailbox and the plan for
  high-level emulation.
- `tutorial-map.md`: the functions, panels and tabs a NewTek tutorial
  covers, what each needs, and how far the emulation supports it.

The Flyer clip format is in `crates/flyerclip/FORMAT.md`. The emulation
itself is described in `docs/internals/video.md` (Toaster section) and
configured as in `docs/guide/configuration.md`.

## What you need

Neither the Amiga ROMs nor NewTek's software are included. The Toaster
needs an A4000 with Kickstart 3.1 (40.68) and Workbench 3.1, available
legally from [Amiga Forever](https://www.amigaforever.com/) (Cloanto). The
Video Toaster 4.x and Flyer software, and the effects, sources and other
files around them, are on
[Open Video Toaster](http://discreetfx.com/openvideotoaster.html). The
`[video_slot]` section of `docs/guide/configuration.md` describes the
media folder (the card's inputs, `Output` and the Flyer's drives) and
the machine to fit the card into; `flyer-convert` turns video, stills and
picture sequences into Flyer clips in that folder.

## Credits

NewTek built the Video Toaster and the Flyer, and released their
software's source code in 2004; the Open Video Toaster project, run by
DiscreetFX (Bill Panagouleas and team), hosts it and the files around it.
This emulation was written from those sources and would not exist
without them.
