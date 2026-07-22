# The video pipeline

The renderer's central rule: **it never races the chipset.** The chipset
does not paint pixels as it runs; instead, every render-relevant event is
recorded with its beam position, and the renderer replays the completed
frame's events afterwards. The live emulation and the painting of pixels
are decoupled in time but exact in beam position. In normal windowed and
headless runs, replay happens on the default render worker; the CPU,
custom-chip model, and GPU presentation remain on the main thread.

## Recording: beam events (`video/beam.rs`)

As the core runs, Copper and CPU writes to render-relevant registers --
BPLxPT, BPLCONx, COLORxx, DIWSTRT/STOP, DDFSTRT/STOP, modulos, sprite
registers -- are recorded as `BeamRegisterWrite` events tagged with
`(vpos, hpos, source)`. Chip-RAM writes that can affect a frame already
being fetched are recorded similarly. `BeamEventIndex` buckets events per
scanline so replay does not rescan the full frame log per line.

## Replay: planar to RGBA (`video/bitplane.rs`)

At frame end the renderer starts from a snapshot of display state, then
walks each scanline applying that line's recorded events at their beam
positions: a palette write at `hpos` changes the colour of pixels to its
right, a mid-line BPLCON1 write shifts scroll mid-line, exactly as the
beam would have seen it. Bitplane data is fetched via the recorded BPLxPT
state in the hardware fetch order, shifted through beam-timed BPLCON1,
decoded through EHB / HAM / HAM8 / dual-playfield rules (the pixel
pipeline carries 24-bit colour end to end; OCS/ECS paths keep their exact
12-bit maths and expand by nibble), composited with the eight sprites
under playfield priority, and CLXDAT collisions are accumulated.
For DMA-fetched HAM playfields, the display window gates framebuffer output
and collision recording, but the low-res Denise phase can still seed the HAM
component history just before DIW: when the window opens to the right of the
fetch origin (a late DIWSTRT, or an early DDFSTRT), replay pre-advances the
hidden samples before painting the DIW edge. The standard `$81` window edge
is flush with the standard `$38` picture (both at framebuffer x 62,
hardware-verified on the sblit0 A500 photo), so a stock screen hides no
samples. Extra fetch
groups from an earlier DDFSTRT are not decoded into the HAM hold colour before
DIW opens; they are fetched by Agnus, but the first visible HAM history is
bounded to the display-phase samples. Single-word lo-res fetch placement is linear in DDFSTRT: each 8-cck fetch
period before the standard `$38` slot moves the picture exactly 16 lo-res
pixels left (hardware-verified
against the vAmigaTS `Agnus/DIW/OLDDIW/diw1` A500 photos, OCS and ECS).
Early and late single-word lo-res DDF keep the picture beam-anchored;
the renderer must not add or subtract a sample just to
align the picture to a fetch-unit boundary.
Hi-res early DDF is beam-anchored the same way: content fetched ahead of the
window edge is hidden by the window comparator alone (XSysInfo's DDFSTRT `$38`
panel clips exactly its one pre-fetch word), so when an extreme-overscan
screen opens the window early as well (KS 3.2 Overscan editor on ECS:
DDFSTRT `$28` with DIWSTRT h `$5D`), the early words are visible inside the
window rather than being snapped away (issue #186).
When DDFSTRT is late enough that DIW opens before DMA has delivered the
first BPL1DAT word for the row, playfield output remains border-colour until
that plane-0 fetch reaches Denise instead of sampling stale shifter contents.
That gate is placed in the bitplane/DIW coordinate domain, not the normal
Copper/register-write output domain, because it follows the fetch slot that
loads BPL1DAT.
Horizontal DIW clipping applies to sprites unless AGA border sprites are
enabled by BPLCON3.BRDSPRT; if BPLCON3.BRDRBLNK is asserted, the border-sprite
bypass is suppressed along with the blanked border.
Once that first DMA word is visible, the renderer samples the enabled
bitplanes from the complete latched word; it does not expose the first word
plane-by-plane according to each plane's individual DMA slot.
If a manual BPL1DAT write starts a word before a later DMA BPL1DAT load
point, replay stops the manual word where that DMA word replaces Denise's
shifter.
The OCS/ECS BPLCON1 scroll nibbles count lo-res pixels regardless of
resolution: one step shifts a hi-res playfield two hi-res samples and a
super-hi-res playfield four, and the comparison narrows with the word
cadence, so hi-res ignores nibble bit 3 and super-hi-res bits 2-3 (pinned
by the `ddfprobe-hscroll` golden probe on the Kickstart 2.05 boot-screen
constellation, vAmiga-verified). AGA's extended BPLCON1 fields feed the
same per-plane delays through `aga_bplcon1_scroll_samples`, masked to one
fetch-unit width (32-bit fetches scroll within 32 lo-res px, 64-bit within
64).
An off-grid DDFSTRT interacts with the scroll in both fetch regimes. An
FMODE=0 fetch placed off the shifter reload grid rounds UP (the data is
late for its own slot), and a scroll that covers the lateness catches the
floor slot one gulp earlier (vAmiga-verified, `ddfprobe-phase`). A wide
FMODE fetch has the opposite sense: Agnus masks DDFSTRT DOWN to the
fetch-unit grid, the data arrives early, and Denise's reload comparator
window is anchored at that early fetch start, so scroll taps folding into
the last `earliness` px of the gulp window already see the next gulp's
data and the playfield sits one full gulp left -- the display delay is
`((tap + earliness) mod gulp) - earliness`. On-grid starts never fold.
Pinned by the `ddfprobe-agafold` golden probe on the Alien Breed II AGA
playfield constellation (lo-res BPL32, DDFSTRT $24 -> earliness 8 px),
whose scroller pairs the folded taps with a one-gulp pointer step and
jumps 32 px for 4 of every 16 pan frames without the fold. FS-UAE
(WinUAE core) renders the probe's 16-band map identically, band by band
(vAmiga is OCS/ECS-only and cannot arbitrate AGA); the hi-res/SHRES
scaling of the fold is not yet externally verified.
BPLCON1-delayed samples at the left edge of a scanline do not reuse the
previous line's final bitplane word. Before the current line's shifter has a
sample for a delayed tap, replay marks playfield output active but returns
colour index 0. Block-start lines also suppress samples fetched before DIW
opened, because no earlier playfield stream was active before the visible gate.
Contiguous rows may expose same-line samples that were fetched before DIW
opened, but the scroll-in never comes from a previous scanline's tail. AGA's
extended BPLCON1 delays can exceed one 16-bit shifter word; the extra leading
gap also stays background until current-line samples reach Lisa.
A BPLCON1 write whose normal register position is already at or beyond DIW's
right edge is not pulled left into the current line's bitplane-scroll domain;
it updates following lines without retapping the visible HAM tail of the
current line.

The playfield pixel loop runs in control-run chunks: recorded control,
scroll, and palette events take effect at output-pixel boundaries, so
between two event positions everything derived from `ControlState` (the
BPLCON0 mode decode, display-window edges, fetch-origin quantization,
per-plane scroll delays) is constant and is computed once per run rather
than per pixel. The per-pixel decisions inside a run are unchanged -- the
chunking is a host-CPU optimisation, not a model change.

AGA Lisa has one known split control path in this replay: BPLCON4's
high-byte BPLAM bitplane XOR follows the normal control timeline, but the
low-byte ESPRM/OSPRM sprite palette-base fields are visible to sprite
colour lookup at Lisa's earlier sprite palette-control x position. Ordinary
COLORxx palette writes stay on the Denise palette-output timeline; sharing
the sprite path shifts copper palette gradients horizontally and
turns smooth per-line colour ramps into bands.
The render event journal therefore creates a sprite-only BPLCON4 segment
when those two x positions differ, then applies the full BPLCON4 value on
the normal control segment.

Manual and held-sprite replay has a smaller split of its own. SPRxDATA and
SPRxDATB writes update Denise's data latches in the normal register-output
domain, but the sprite serializer copies those latches only when the
horizontal comparator fires. A DATA/DATB write after that compare is for a
later compare or scanline, not the word already shifting. SPRxPOS writes
re-arm the sprite horizontal comparator: if the write occurs before the
newly programmed HSTART, the sprite can still begin at that HSTART. The
replay clips those position intervals in the sprite-comparator domain
(seven CCK ahead of the normal register-output position) so adjacent manual
sprite words can abut at their HSTARTs and staggered even/odd attached-pair
position writes do not create artificial half-pair strips. Once a manual
sprite word has started shifting, later same-line POS/CTL writes can arm a
future compare but do not truncate that active word. A POS write that lands
exactly on the HSTART compare boundary is on the already-started side of
that rule.

When sprite DMA was observed for the frame, captured DMA lines are the
authoritative data source for DMA-fetched spans. Manual replay is seeded by
beam-timed SPRx register writes, not by frame-start SPRxDATA latches alone:
the data latch can persist across frames without proving that the sprite
vertical comparators are active in the current field. A same-line SPRxPOS
write after the sprite DMA slot can re-arm the horizontal comparator and
reuse the line data DMA already loaded, so the renderer seeds those POS-only
reuse spans from the captured DMA line. Sprites whose data was established
by DMA before SPREN was cleared are carried separately as held sprites and
can still be repositioned by later SPRxPOS/CTL writes. Merely enabling
sprite DMA and crossing an empty sprite pair slot is not enough to make
captured DMA authoritative; the frame must contain actual fetched or held
sprite data.

Two manual-replay guards exist only to reconcile DMA writes the beam replay
cannot see (Agnus drives POS/CTL/DATA through the same Denise registers
without recording beam events): an early same-line SPRxPOS write hands the
line to the DMA capture, and a pre-visible SPRxDATA/DATB write seeds the
latch for later retiming instead of arming direct output. Both apply only
when sprite DMA was observed in the frame. With sprite DMA idle Denise's own
rules hold unmodified: SPRxDATA arms at any beam position (including
vertical blank), SPRxCTL disarms, SPRxPOS never disarms, and an armed sprite
serializes at HSTART on every line because Denise has no vertical
comparator. A vblank arm sequence with VSTART equal to VSTOP therefore
displays full-height columns, which is how Gen-X draws the vertical
edge-masking line sprites of its shutter transitions.

Because DMA fetches land in the same SPRxPOS/CTL/DATA/DATB registers a
CPU/Copper write hits, Denise keeps two views of them: the CPU/Copper write
shadow (`sprpos`/`sprctl`/`sprdata`/`sprdatb`/`spr_armed`), which the
manual replay above and the live collision path are calibrated against, and
the hardware-true view (`spr_hw_*`), which additionally receives every
sprite DMA fetch -- a DATA fetch arms it, the vstop control fetch
(including the 0/0 list terminator) disarms it. The DMA-idle latched
redisplay seeds from the hardware-true view: software relies on the
terminator's CTL to silence a channel for good, so a later bare
SPRxDATA arm must redisplay the DMA-written words, not the last manual
pattern (Hamazing's scene switch writes SPRxDATA=$0000 after a DMA sprite
scene and expects invisible sprites; the stale write-shadow pattern would
paint full-height bars). Only the authoritative sprite-DMA pass for a line
writes the hardware view through: pre-display lines are computed twice, and
the pre-display replay at the display start owns them (`sprprobe-latch` in
timing-test/ pins the whole sequence).

The mapping from beam coordinates to framebuffer x is anchored by
constants that encode the hardware's fetch-to-display pipeline delays --
register writes, palette writes, and bitplane data each land at their own
documented offset, and the bitplane fetch reference differs between lo-res
and hi-res. The display-window comparator maps a DIWSTRT hstart H to
framebuffer x = 2H - 196 (hardware-verified against the sblit0 A500 photo).
A standard lo-res `$81`/`$38` picture is flush with that edge; a standard
hi-res `$81`/`$3C` picture starts its 640 fetched pixels one lo-res pixel
inside the window (matching vAmiga), with no wider leading border. Wide-FMODE DMA fetches start from the revision-masked
DDFSTRT comparator value and complete whole units, but the displayed shifter
origin is still quantized by the FMODE fetch gulp; the renderer keeps those
two effects separate. Denise's output line starts at the horizontal blanking
start counter; COLORxx writes before that counter are the wrapped tail of
the previous output row, while the palette value they load is still the
base colour for the following row. These anchors were calibrated against
real-hardware captures and other emulators; `COPPERLINE_HCENTER=0` and
`COPPERLINE_OVERSCAN=full` help when re-checking them.

For FMODE=0 lo-res, the one-sample low-res phase bias is applied on both
standard and late fetch origins. If a late DDF row completes exactly at
DIWSTOP, the final visible DIW sample still includes undelayed planes; BPLCON1
delay only retaps the per-plane shifters, it does not make the undelayed planes
drop one sample before the display window closes.

The framebuffer is a 716x285 overscan field (lo-res pixels doubled
horizontally). It captures deep overscan on all sides.
For standard 15 kHz PAL/NTSC fields, row zero is anchored at Copperline's
fixed overscan top rather than the current DIWSTRT vertical value. DIW still
acts as the hardware display-window flip-flop: it decides when the frame's
chip-RAM snapshot and bitplane DMA capture begin, but changing DIWSTRT later
in the field does not recenter the already-visible top border. Programmable
VARBEAMEN scans instead use their programmed visible window as the render
origin. Under VARBEAMEN, Denise's horizontal counter restarts at 0 with the
programmable line rather than free-running at the standard 15 kHz phase, so
the DIW and sprite comparators sit later on the canvas by that origin
difference (Linux/m68k amifb and the KS3.1 DblPAL screen both program their
windows against the zero origin). A programmable frame is presented like a
multisync monitor on both axes: when the mode programs its sync pulses, the
glass shows the line from the HSYNC trailing edge to the next pulse
(VARHSYEN) and the frame from the VSYNC trailing edge to the next pulse
(VARVSYEN), so the picture sits where the mode's own porches place it, with
blanked border rows above and below the programmed vertical window. Without
a programmed horizontal sync the whole line maps onto the glass
time-linearly (each colour clock covers 227/line_cck of a standard clock's
width); without a programmed vertical sync the captured rows keep covering
the full glass height.

Super-hi-res output: Denise/Lisa resolve every 35 ns sample through the
full palette pipeline (ECS Denise carries at most two bitplanes into
SHRES; AGA Lisa runs the complete 8-bit index path, e.g. the 4-plane
FMODE=3 Linux amifb console). A programmable scan that drives SHRES
renders a double-width canvas at the 35 ns pixel pitch
(`canvas_scale_for`): each of the two per-column samples is emitted as
its own framebuffer pixel, and the presentation, screenshots, and the
browser canvas carry the doubled width through (the desktop window shows
it 1:1 on a 2x HiDPI texture). Every logical coordinate in the replay --
comparators, fetch origins, sprite positions, the collision buffers --
stays in the classic hi-res-pitch domain; only the framebuffer writes
fan out, with non-SHRES pixels and sprites doubled. Standard 15 kHz
scans keep the classic single-width canvas byte-identical; their SHRES
screens still blend each 35 ns pair into the 70 ns pixel. Sprite
positions remain at hi-res resolution on either canvas (true 35 ns
sprite placement is a remaining TODO).

Two vertical edge cases the replay honours:

- A display window can open above the captured canvas. Bitplane pointers are
  pre-advanced for those clipped rows by replaying the frame's
  BPLCON0/DMACON writes line by line, so only lines where bitplane DMA
  was actually enabled consume a row -- the CDTV boot screen opens its
  window at line 5 but raises BPLCON0 from 0 to 6 planes at line 24.
- DIWSTRT=0 is not a sentinel. If DIWSTOP is non-zero, the replay opens the
  display window at beam zero and clips the overscan rows/pixels that fall
  before the captured framebuffer; only DIWSTRT=DIWSTOP=0 falls back to the
  reset/default visible window.
- Canvas rows whose beam line lies at or past the frame wrap (the fixed
  285-row field is taller than a standard PAL scan) are forced to black:
  the beam never produces those lines, and a deep-overscan window would
  otherwise let the replay keep walking bitplane memory past the image.

## Threaded frame handoff (`RenderInput`, `video/window.rs`)

At frame end, `Bus::begin_new_beam_frame` freezes the just-finished frame:
the render-event journal, chip-RAM snapshot, captured bitplane/sprite DMA
rows, palette split, display geometry, frame line count, framebuffer start
line, and Agnus programmable blanking latches become the source for
`RenderInput::from_bus`. `render_from_input` consumes only that owned
bundle, so the main thread can start emulating frame N+1 while the worker
renders frame N.

`window.rs` starts a persistent `copperline-render` worker by default.
`COPPERLINE_THREADED_RENDER=0` (also `false`, `off`, or `no`) disables the
worker and uses the synchronous wrapper path. The default worker owns a
scratch framebuffer and the deinterlacer history, calls
`bitplane::render_from_input`, applies the same presentation post-processing
as the synchronous path, and returns a presentation framebuffer tagged with
the render generation and emulated frame number. Resets, power changes, and
save-state loads bump the generation so stale worker results are ignored
instead of being shown after the machine timeline changes.

The worker never mutates emulator-visible hardware state. `CLXDAT`
collisions are CPU-visible Denise state, so the bus completes unread live
collision replay to the end of the frame before rolling the frame buffers.
The synchronous fallback still ORs the render result's collision bits into
Denise after painting, but the threaded path treats those bits as diagnostic
render output and records only the returned render timing on the main
thread.

wgpu and winit remain main-thread-only: the worker paints CPU buffers, and
the main thread uploads the newest completed presentation buffer to the
`pixels` surface. Normal display can be one frame behind emulation; exact
capture paths call `finish_render_for_current_frame` so screenshots, frame
dumps, recordings, debugger step, and run-to-PC output use the requested
emulated frame.

## Interlace (`video/deinterlace.rs`)

Interlaced (LACE) content is presented through a motion-adaptive
deinterlacer at double height: each field lands on its parity's output
rows, and opposite-parity rows are filled by weaving the previous field
where content is static and interpolating neighbours where it moved.
Motion is detected on both parities (each field against the previous
field of its own parity, and the woven line against its own
predecessor), and the per-pixel motion mask is dilated one pixel
sideways so dithered moving art bobs as a region instead of weaving and
interpolating on alternate pixels.
Progressive content is line-doubled without history.
`COPPERLINE_DEINTERLACE=0` falls back to plain line doubling.
In the default threaded pipeline the worker owns this history; the
synchronous fallback keeps it on the window `App`.

The deinterlacer also hosts the optional CRT phosphor-persistence stage
(`[display] phosphor` / `COPPERLINE_PHOSPHOR`, off by default, clamped to
0.95): when on, `present_with_phosphor` blends each presented frame over a
retained copy of the previous one, keeping `phosphor`/256 of the old value
per channel for an exponential trail. This is what fuses field-rate flicker
(alternate-field dither transparency, flicker-dithered animation) the way a
real tube does. Like the rest of the deinterlacer it operates on the
presentation buffer only and never touches the emulated framebuffer.

## Known display gaps

- **31 kHz horizontal layout** (DblPAL / DblNTSC / Productivity): at
  doubled scan rates the bitmap lands ~16 colour clocks left of the DIW
  window edge, and fetched data draws past the short line's end instead of
  being cut by the line wrap. Pinning the per-line DIW/fetch anchoring
  needs WinUAE / real-hardware reference captures; the image-regression
  suite covers these modes structurally but does not yet assert exact pixel
  positions.
- **Programmable interlaced (FF) weaving** is implemented but untested
  against real software.

## Presentation (`video/present_common.rs`, `video/window.rs`, `video/ui.rs`)

`window.rs` owns the winit `ApplicationHandler` and the `pixels` GPU
surface: the field is presented at a TV-like 4:3 aspect plus the
44-pixel status bar, scaling continuously with the window. The GPU surface
is fed from `present_fb`, the post-processed presentation buffer produced by
either the render worker or the synchronous fallback.

The frontend-independent half of this pass lives in
`video/present_common.rs`: the post-render pipeline (vertical/horizontal
recentring, the TV bezel mask, programmable-scan presentation) plus the
standard-window and TV-aperture constants and the geometry predicates that
key on them. `window/present.rs` re-exports everything there, so the
desktop path is unchanged; headless consumers -- `cpu.rs`'s debug
screenshots and the [browser (WebAssembly) frontend](../guide/browser.md)
-- present frames through it without the winit stack.

Two presentation-only adjustments (they never alter the emulated
framebuffer):

- **Overscan mask**: `[display] overscan = "tv"` masks deep-overscan
  margins in black like a CRT bezel; `"full"` shows the entire field. The
  default TV mask is presentation-only: horizontally it keeps 24 lo-res
  pixels of consumer-visible overscan beside the standard display and blacks
  only the deeper horizontal margins. TV mode keeps the framebuffer's fixed
  horizontal source origin, matching the way vAmiga and FS-UAE crop from their
  rendered source texture instead of copying the picture sideways. Vertical
  border colour changes remain visible because they are part of the Denise
  output and are often deliberate border effects.
- **PAL TV PNG aperture**: normal screenshots and `--dump-frames` in TV mode
  crop standard horizontal content to a 692x540 aperture. The horizontal crop
  keeps a 640-pixel standard display centred with 26 pixels of visible overscan
  on both sides; vertically it keeps the PAL title-bar/top-border position
  aligned with the reference-emulator crop. The live window keeps its 716-pixel
  4:3 texture for the status bar and scaling path, but centres the same TV
  aperture inside that texture instead of showing the raw framebuffer origin.
  True horizontal overscan fetches are not cropped to this aperture: they stay
  on the full-width TV path so intentional border content remains visible.
  `COPPERLINE_SHOT_RAW=1` bypasses the PNG crop and writes the raw 716x570
  woven framebuffer. A second, narrower aperture (`TV_PAL_CAPTURED_*`,
  668x540) clips the same rect to columns the framebuffer actually
  captures: the reference aperture's right margin reaches 12 columns past
  the framebuffer's right edge, which the window and PNG paths pad with
  black bezel. Frontends whose frame should end on real pixels present the
  captured aperture instead -- the browser canvas hugs its border on every
  side -- with the margin mirrored from the captured right-overscan width
  so the standard window stays exactly centred. Its geometry invariants are
  const-evaluated beside the definitions.
- **Full-overscan horizontal recentring**: in `"full"` presentation, a standard
  (non-overscan) display is recentred because the framebuffer captures a deep
  slab of left overscan that would otherwise push the picture right of centre.
  The decision keys off the bitplane data the display actually fetches (DDF),
  not just the DIW window: a demo that opens DIW wide around a standard-width
  picture (Virtual Dreams' "Absolute Inebriation") is still recentred, while a
  display that genuinely fetches bitplane data into the overscan border is left
  exactly as rendered.

### RTG scanout (Z3660)

When a fitted `[rtg]` board's guest driver switches the display to RTG,
the presentation path swaps sources: the board's panned framebuffer
(decoded from VRAM in the scanout's pixel format, with the board's
hardware mouse sprite -- including wide and doubled sprites -- composited
over it in `z3660.rs`) replaces the chipset render. The window presents
that frame at its native resolution through a dedicated GPU texture
rather than the 716-wide chipset buffer, and the TV aperture crop is
suppressed -- it is a chipset crop rect, and applying it would show a
sub-rect of the board's screen. While a menu or panel is open the window
falls back to the CPU present path (at the cost of the downscale) so the
overlay is not overdrawn by the GPU pass. If the board claims the display
but its frame does not compose yet (mode set before the resolution
registers), presentation falls back to the chipset render rather than
freezing on a stale frame.

`compose_rtg_present` (`present_common.rs`) also keeps an
`FB_WIDTH`-stride copy of the native frame for the screenshot and CCP
capture paths, which read the shared presentation buffer: one output row
per board row at the board's native height, downsampled horizontally by
sampling each output pixel's source-span centre so the rightmost source
columns survive. Screenshots under RTG are therefore 716 wide at the
board's native row count.

`ui.rs` implements the status bar widgets, the pop-up menu, the smaller
overlay panels (About, Shortcuts, Calibration), and the shared debugger/tool
panel drawing used by the native debugger and frame-analyzer windows. The UI
uses the 8x8 `font.rs` glyphs. `COPPERLINE_UI_PREVIEW=1 cargo test
panels_render_into_their_rects` renders every panel into
`target/ui-preview-*.png` -- the screenshots in this documentation come
from there -- and the `test_app()` fixture drives the debugger window
against a real emulator instance in the unit tests.

## Headless capture (`screenshot.rs`)

`--screenshot-after` and `--dump-frames` render through the identical
pipeline with the window hidden; PNGs are scaled to the same geometry the
window would present unless `COPPERLINE_SHOT_RAW=1` requests the unscaled
woven framebuffer. The default vertical presentation scale selects whole
source rows rather than blending adjacent Amiga scanlines, matching the
normal unfiltered display path. Because the default render worker may be one
frame behind, these paths wait for the worker result matching the target
emulated frame before writing the PNG. The
[headless debugger](../debugger/headless) `COPPERLINE_DBG_SHOT` hook reuses
the same path to capture the last completed frame at a breakpoint.

## Video recording (`recorder.rs`)

The [interactive recording](../guide/ui) shortcut writes an AVI containing
lossless ZMBV video -- the DOSBox capture codec: zlib-deflated intra frames
plus XOR-delta inter frames on a
16x16-block grid, encoded entirely with the `flate2` crate -- and
16-bit stereo PCM at the 44.1 kHz mixer rate. `recorder.rs` owns both
the encoder and the AVI muxer, and its unit tests round-trip the stream
through a reference decoder.

Capture is locked to the emulated timeline, not the host clock. Paula
carries an optional capture tap that collects every mixed stereo frame
(before the master output volume); the window drains it once per
emulated frame and, when the frame loop completed a new emulated frame,
waits for the matching presentation buffer before pushing it through the same
`scale_y_into` source-row presentation scale as the live window. At finish the
AVI's video rate/scale is patched from the exact frames-to-audio-samples ratio,
so a nominal "50 fps" label never drifts against PAL's true field rate and
warp-speed captures play back at normal speed. The REC badge, status bar, OSD,
and menus are drawn into the presentation texture after capture, so they never
appear in the file.
