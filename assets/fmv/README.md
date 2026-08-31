# Bundled open CD32 FMV ROM

`copperline-fmv.rom` is Copperline's freely redistributable 256 KiB ROM for
the CD32 Full Motion Video cartridge. It contains a clean-room GPLv3+
`cd32mpeg.device`, a clean-room `videocd.library`, a Video CD autoboot/player
resident, a valid expansion DiagArea, and an empty CL450 firmware container.
Copperline models the CL450 command interface but does not execute the
cartridge's proprietary microcode, so no Commodore code or firmware is
included.

The preferred source is the adjacent repository directory `fmv-rom/`; rebuild
and refresh this artifact with:

```sh
make -C fmv-rom check
make -C fmv-rom bundle
```

The CD32 machine profile fits this ROM by default. An explicit `fmv_rom` path
still wins; `fmv_rom = ""` leaves the module unfitted. The ROM is licensed
under GNU GPL v3.0 or later, the same `LICENSE` shipped at Copperline's root.

Compatibility validated on 2026-08-30:

- CD32 Kickstart 3.1 r40.60: Cannon Fodder streams its 352x288 MPEG intro
  through the resident device using the host `cd.device`'s standard Mode-2
  reads, with decoded video and non-silent stereo audio.
- AROS PR 1089 through CDXL-ordering commit `ebfc7d9`: Cannon Fodder streams
  through AROS's system-ROM device. PR 1089 intentionally skips the cartridge
  diagnostic to prevent the legacy Commodore ROM replacing AROS's
  `cd.device`.

Under CD32 Kickstart, the library classifies and parses Video CD metadata; the
Philips Media Retail Sampler '95 probe returns two video tracks and 45 entry
points. Its version 41 `cdstrap` cold-boots that disc to a controller-driven
track menu; Red plays a track and Blue aborts playback and returns to the
menu. Non-Video-CD media chain to the displaced stock strap. AROS PR 1089
skips cartridge diagnostics, so it does not install these residents.
