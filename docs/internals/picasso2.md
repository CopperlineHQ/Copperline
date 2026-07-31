# Picasso II/II+ and CL-GD5426/5428 model

Copperline's Picasso II family is a hardware model of the Village Tronic Zorro
II boards, not a Picasso96-aware display API. The guest enumerates the real
manufacturer/product/serial identities, programs VGA and Cirrus registers,
writes the linear VRAM aperture, starts BitBLT operations, and controls the
physical monitor pass-through switch. See [](../zorro) for the two-window
autoconfig layout and [](video) for presentation.

## Implemented controller surface

`CirrusGd5426` owns all serializable CL-GD5426/5428 chip state: controller
revision, 1 or 2 MB of VRAM, VGA and extended register files, DAC palettes,
hidden-DAC access phase, scan position, vertical-interrupt latch, hardware
cursor, and BitBLT state. The board wrapper adds the two Zorro window decoders,
revision-specific INT2 wiring, and monitor relay state.

The register model covers the packed-pixel driver path:

- standard sequencer, graphics-controller, CRTC, attribute, miscellaneous
  output and DAC ports, including mono CRTC/status aliases;
- SR6 extension locking, extended VCLK registers, extended pitch/start bits,
  the four-read hidden-DAC protocol, and the Cirrus cursor registers;
- direct linear writes plus VGA write modes 0-3;
- 8-bit CLUT, 15-bit RGB 5:5:5, 16-bit RGB 5:6:5, and 24-bit BGR scanout,
  including pitch, panning start, doublescan, progressive presentation of
  interlaced modes, palette mask, and cursor composition;
- CL-GD5426/5428 BitBLT video/video and system/video sources, forward and backward
  overlap-safe copy, 8x8 pattern and solid fill, colour expansion, source
  transparency, 8/16/24-bit pixel widths, and all sixteen documented Cirrus
  raster operations.

VGA text rendering is intentionally absent. The card powers up behind native
Amiga pass-through, and Amiga RTG drivers program a packed-pixel mode before
selecting the VGA output.

## Timing and determinism

The selected VCLK numerator, denominator, and post-divider produce the pixel
clock from the Cirrus controller's 14.318184 MHz reference. CRTC totals derive a frame
period in Amiga colour clocks. `$3DA` vertical-retrace and display-enable status
therefore advance only through device `tick` calls; no host or wall clock enters
the model.

On the II+, crossing the programmed vertical-retrace start latches an interrupt
when VGA CRTC `$11` enables it. Register-window offset `$1001` gates the latch
onto the Zorro INT2 line; `$1000` removes it. Writing CRTC `$11` with bit 4 clear
acknowledges the latch. The original Picasso II follows the same deterministic
scan clock but has no physical interrupt connection and always reports INT2
inactive.

BitBLT work is applied deterministically when started (or when the last byte of
a system-source transfer arrives). GR31 remains busy for 16 colour clocks after
completion. This short fixed observation window lets polling software see the
busy transition without making output depend on host execution speed.

Direct CPU access keeps Copperline's existing functional-board timing of two
colour clocks per word. A board-specific Zorro II bus timing refinement would
affect broader emulator timing and is deliberately separate work.

## Diagnostics and known gaps

Set `COPPERLINE_DIAG_PICASSO=1` before startup to log VGA port writes, decoded
mode changes, blit descriptors, hidden-DAC changes, and monitor-switch changes.
The value is read through Copperline's startup configuration cache and does not
introduce wall-clock or dynamic environment state.

The 1 MB configuration advertises and maps a 1 MB memory-space aperture, so
there is no guest-visible upper-half alias or hole. The II+ model supplies its
CL-GD5428 part ID, different autoconfig serial, and INT2 vertical blank. The
physical product-13 segmented configuration, Pablo encoder, VGA text modes,
and native-resolution headless RTG screenshots remain outside the model.

The implementation was checked against the Cirrus Logic CL-GD542X Technical
Reference Manual and the independent Cirrus models in 86Box and QEMU. Those
remain the reference when a guest register trace exposes a missing detail.
