# Graffity [Zorro II]/[Zorro III] model

Ateo Concepts' Graffity boards reuse the same CL-GD5428 core as
[Picasso II+](picasso2) (`CirrusGd5426::new_gd5428`), so their register,
VRAM, and BitBLT behaviour is identical to that model. What differs is the
autoconfig identity and window layout -- Graffity has its own registered
manufacturer ID (2092) and wires the switch-strobe and VGA-register ranges
differently from Picasso II. See [](../zorro) for the autoconfig chain and
[](video.md) for presentation.

## Zorro II layout

Like Picasso II, the physical board exposes two chained Zorro II identities:
a linear VRAM aperture (product 34, window 1) and a register aperture
(product 33, window 0). Graffity's register aperture is 128 KB rather than
Picasso II's 64 KB, though only the low VGA port range within it is live.

Unlike Picasso II, Graffity's register window addresses VGA ports directly at
the window offset (no odd/even port-mirroring quirk): offset `$3B0`-`$3DF`
reaches the corresponding VGA port 1:1. The monitor switch strobe lives at
offset bit 15 (`$8000`+): bits `$60` show the RTG screen, `$40` restores the
native Amiga display, matching Picasso II's encoding.

## Zorro III layout

The Zorro III board is a single 16 MB autoconfig window (product 33, no
window tag) with three fixed sub-apertures:

- `+$400000`, 64 KB: pure monitor-switch strobe trap. Writes here never reach
  the VGA core; only the `$60`/`$40` low-bit pattern toggles the switch.
- `+$800000`, 64 KB: the real VGA-register window, same direct port
  addressing as the Zorro II variant.
- `+$C00000`: linear VRAM, sized to the configured 1 or 2 MB.

## INT2 wiring

Graffity has no board-level interrupt-enable latch of its own (unlike
Picasso II+'s register-window `$1000`/`$1001` gate). INT2 follows the
CL-GD5428 core's own vertical-blank state directly
(`CirrusGd5426::vblank_pending`), which is itself gated by the guest arming
VGA CRTC `$11`.

## Reference

Board-level facts (autoconfig IDs, window layout, no-ROM autoconfig
synthesis) were taken from amiberry's `src/gfxboard.cpp` `GFXBOARD_ID_GRAFFITY_Z2`/
`GFXBOARD_ID_GRAFFITY_Z3` board table entries and their `init_board`/
`special_pcem_get`/`special_pcem_put` handlers, cross-checked against the
classic Aminet `Picasso96Install` package, which ships `Graffity.card` as a
first-class Picasso96 board driver. The chip register model itself is
unchanged from [Picasso II/II+](picasso2).
