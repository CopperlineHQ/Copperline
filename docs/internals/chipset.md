# Chipset modules

Each custom chip is a module under `src/chipset/`, owned by the `Bus` and
stepped in emulated time. Unit tests live inline in each module's
`#[cfg(test)] mod tests` block; the suites are large and are the best
specification of the modelled behaviour.

## Agnus (`agnus.rs`)

Agnus owns the beam: `vpos`/`hpos` counters advanced per colour clock, PAL
(313 lines, 227 CCK/line) and NTSC (263 lines with long/short line
alternation) geometry, the long-field flag for interlace, and VPOSR/VHPOSR.
It also owns DMACON and the display-fetch machinery: for FMODE=0 fetches
the per-line fetch table comes from the DDF sequencer flop model
(`src/chipset/ddf_sequencer.rs`; see the [arbitration model](timing) for
the flop semantics - comparator edges, stop drain through a final
modulo-applying unit, cross-line run carry, OCS/ECS rule differences).
Each fetch unit uses the BPLCON0 value the sequencer sees at that point,
so a mid-row plane-count change cannot retroactively fetch earlier words,
but it can add or remove planes for later units in the same row; word
addressing stays unit-based. The DDF register value is masked to the Agnus
revision's comparator precision (OCS keeps 4-CCK precision; ECS/AGA keep
2-CCK precision), and a DDFSTOP landing mid-unit extends the fetch through
the unit starting at-or-after it plus the drain unit (the CDTV trademark
screen's hi-res $64/$A8 window fetches 20 words per row, not the truncated
18). In lo-res OCS, bit 2 of DDFSTRT and DDFSTOP remains visible
to the 8-CCK fetch-unit count: $34/$D4 fetches 21 words, $28/$D4 fetches
23, and $4A/$B6 fetches 15. Wide-FMODE units (16/32 CCK) use the same rule
rather than moving DDFSTRT down to an absolute grid. In
lo-res, the plane-order slots for a wide unit are packed into the unit's
first eight CCKs; the remaining CCKs are free for other bus users. If a
bitplane fetch block that started before sprite 7's late DMA slot is still
active at $30, sprite 7 DMA is blocked for that line; a DDFSTRT value of
$30 itself matches on the following odd cycle and does not steal the already
decided sprite slot. The condition is derived from the fetch-block sequence,
not from a single DDFSTRT value. SANITY Roots II's AGA 256-colour effects are
regression examples for both sides of this: the hi-res FMODE=3 pictures need
raw-DDFSTRT unit rounding to preserve their 40-word rows, and the lo-res
FMODE=3 landscape needs packed first-eight CCK plane slots instead of
spreading those slots across the 32-CCK unit.

Agnus revisions are modelled independently of Denise (machines shipped
mixed): OCS (8370/8371), ECS 8372A (1M chip RAM reach), ECS 8375 (2M), and
AGA Alice (2M, HRM IDs $23/$33). The ECS Agnus adds DIWHIGH and the
implemented subset of BEAMCON0 (PAL/VARBEAMEN/LOLDIS/HARDDIS and friends);
Alice adds the FMODE wide-fetch latch, which scales the bitplane and
sprite fetch quanta (FMODE=0 stays byte-identical to the OCS/ECS slot
timing).

Sprite DMA is modelled at the register level, the way the chips work:
there is no separate "descriptor" concept. Each channel keeps Agnus-side
copies of its SPRxPOS/SPRxCTL words and the vertical comparator values
derived from them, updated identically by DMA fetches and by CPU/Copper
pokes (POS supplies the vertical start's low bits, CTL the high bit, the
whole vertical stop, and disarms the display latches). At each line start
the comparators run: passing vstart sets the channel's DMA flip-flop,
passing vstop clears it -- even when SPREN is off, which is what leaves a
sprite dead until the next field when software disables DMA across its
vstop line. Writes landing on the matching line re-evaluate immediately.

A sprite line's two DMA slots ($15+4N and $17+4N) are evaluated at their
own colour clocks. On the channel's vstop line the slots fetch the next
control words (POS in the first slot, CTL in the second); on other lines
an enabled channel fetches DATA and DATB, so chip RAM rewritten between
the two slots is seen by DATB but not DATA. The vertical-blank reset
(PAL line $19, NTSC line $14 -- sprite DMA is inhibited above it) forces
every channel's vstop to that line, which is how each field's first
control-word fetch happens; software reloads SPRxPT each vblank to point
it at the sprite list. An "inverted" pair (vstop before vstart, or a
$0000/$0000 terminator) simply parks the comparators on values the beam
has passed or never shows: the terminator decodes to vstart=vstop=0 and
the channel stays silent until the next field's reset.

DMACON's SPREN is sampled by each DMA slot individually, so a mid-line
edge fetches exactly one word of the pair. SPRxPT advances only on words
actually fetched: a skipped slot leaves the pointer behind and the stream
shifts accordingly. The display line assembles from the fetched words
plus the stale latch on a skipped side; a missed DATA slot never arms the
sprite, and an armed channel with DMA off keeps redisplaying its latches
at the current POS/CTL decode until a CTL fetch or poke disarms it (the
vAmigaTS sprena/sprdis families' vertical bars).

Because SPRxPT only moves on fetches, a channel that consumed its list
leaves the pointer parked at the DMA frontier past it, and the next
field's replay seeds from that frontier rather than from the stale
last-written value; programs must reload SPRxPT every field (normally
from the Copper) to keep a sprite displayed. The frame-start replay path
replays off-screen DMACON, SPRxPT and SPRxPOS/CTL writes in beam order
before rendering the visible field. The whole per-channel register state
is chip state: save states serialize and restore it directly.

A modelling note that catches people out: OCS lo-res with BPU=7 is an
overprogrammed mode. Denise still decodes six BPLDAT latches, but Agnus
only schedules four DMA streams, so planes 5 and 6 display whatever was
last latched -- this is hardware behaviour, not a bug.

## Copper (`copper.rs`)

The Copper decodes MOVE/WAIT/SKIP and executes on its beam-locked fetch
cadence (see [](timing)). It runs from Agnus beam
time, is gated by DMACON's COPEN, restarts from COP1LC each frame, and its
register writes are recorded as beam events for the renderer.

## Blitter (`blitter.rs`)

A scheduled per-DMA-slot engine with the hardware per-word channel
sequences for normal, line, and fill modes; see [](timing). Normal-mode
A/B barrel-shifter carry is a datapath latch and survives BLTSIZE row
boundaries; first/last masks, area-fill state, and modulos remain row
scoped. ECS adds BLTSIZV/BLTSIZH for larger blits.

## Paula (`paula.rs`)

Paula owns the interrupt system (INTENA/INTREQ, delivered through the
modelled IPL-pin pipe and 68000 boundary sampling), serial, and audio:

- **Audio**: four channels running the HRM per-channel state machine
  (states 000/001/101/010/011): AUDxDAT arrivals, the period counter,
  and DMACON edges drive the transitions, whether the data comes from
  the channel's DMA slot or a CPU write (Paula cannot tell them apart,
  so a CPU AUDxDAT poke during DMA playback counts against the length
  counter like a fetch). DMA start-up performs two fetches -- the first
  from the stale pointer, discarded, raising the channel interrupt and
  resetting the pointer to AUDxLC -- before output begins; the length
  rollover reloads pointer/length at the final-word fetch and interrupts
  at the following word start. State-machine DMA requests transfer to
  Agnus at each line end and the fixed audio slots service them on the
  next line regardless of the DMACON bits (a request posted by a brief
  AUDxEN pulse is still fetched after the channel is switched off, which
  kicks the channel into free-running IRQ-mode output at the AUDxPER
  cadence -- the software "period timer" idiom, vAmigaTS pertimer1).
  Clearing AUDxEN while the channel is outputting is not sampled at the
  DMACON write: Paula only re-evaluates AUDxON at the word-start boundary
  (the 011 period event, which idles the channel when AUDxON is low and
  the channel interrupt is pending). A clear followed by a re-enable
  before that boundary is therefore missed entirely and playback
  continues from the live pointer rather than restarting from AUDxLC
  (the 2c1.adf regression, issue #74; vAmiga idles immediately on the
  clear and gets this case wrong). Only the DMA start-up states 001/101,
  which have not begun output, idle at the write.
  ADKCON attach modes feed the fetched words to the next channel's
  volume latch at word starts and period latch mid-word. LEN=0 plays a
  full 65536-word block, as on hardware. Output is mixed in emulated
  time to stereo with the LED filter, then resampled at the host
  boundary.
- **Serial**: SERDAT through a one-word transmit buffer and a timed shift
  register to stdout; SERDATR reports TBE/TSRE/RBF. DiagROM's diagnostic
  stream arrives this way.
- **Disk registers**: DSKLEN/DSKBYTR/DSKSYNC/DSKDAT and the disk-block
  interrupt, fed by the floppy controller below.
- **Pots**: POTGO/POTGOR counters at the hardware 512-CCK rate (the second
  mouse/joystick button path). A pin driven HIGH as an output (its OUTxx and
  DATxx bits both set) holds the matching POTxDAT counter at 0, modelling the
  cap charging instantly through the driver; floating/driven-low pins charge
  up as before. Software that writes POTGO=$FFFF to read POTxDAT back as ~0
  (e.g. the Bitmap Brothers input poll, which keys a no-second-button test on
  POT0DAT) depends on this.

## Denise (`denise.rs`)

Palette (32 12-bit entries as seen by OCS/ECS; the store is the AGA
256-entry layout of high/low nibble-plane pairs giving 24-bit colour plus
the genlock transparency (T) bit, with Lisa COLORxx writes routed through
BPLCON3 BANK/LOCT banking), BPLCON0-4,
display window (DIWSTRT/DIWSTOP, ECS DIWHIGH), sprite
position/control/data registers, and CLXCON/CLXDAT collision detection
(CLXCON2 extends it to planes 7-8 on Lisa). Denise revisions: OCS 8362,
ECS 8373, AGA Lisa (DENISEID $00F8). The AGA decode adds 8 bitplanes,
HAM8, the BPLCON4 BPLAM pixel-index XOR mask, and the OSPRM/ESPRM sprite
palette banks. The two BPLCON4 fields are on different Lisa timing paths:
the low byte that selects sprite palette bases (ESPRM/OSPRM) reaches sprite
colour lookup on an earlier sprite palette-control path than ordinary COLORxx
palette writes, while the high-byte BPLAM XOR continues on the normal
bitplane/control path. AGA also widens dual
playfield: OCS/ECS split six bitplanes into two three-bit fields (PF1 =
planes 1/3/5, PF2 = planes 2/4/6), while Lisa extends each field to four
bits by feeding bitplane 7 into PF1 and bitplane 8 into PF2, so a 7-8
plane dual playfield addresses palette entries 8..15 per field. The extra
bits are gated on the AGA revision; pre-AGA chips never carry bitplanes
7/8 and keep the exact three-bit decode. Denise state is not rendered live
-- writes become beam events that the [video pipeline](video) replays.

The ECS DIWHIGH high bits only stay in force until the next DIWSTRT or
DIWSTOP write, which re-arms the OCS-implicit high bits derived from the
low DIWSTRT/DIWSTOP values. Software that programmed a wide window through
DIWHIGH and then touches DIWSTRT/DIWSTOP falls back to the implicit
window, so the replay must drop the stale DIWHIGH on those writes rather
than hold it.

DIWSTRT value zero is still a real Denise comparator position. The emulator
only treats the display window as unprogrammed when DIWSTRT and DIWSTOP are
both zero; a zero start paired with a non-zero stop opens the window at beam
zero and can expose deep overscan.

## CIA (`cia.rs`)

A small 8520 model used for both CIAs: I/O ports, the
interval timers with cascading and underflow pulses, the 24-bit TOD
counters (VSYNC-clocked on CIA-A, HSYNC on CIA-B) with latch and alarm
semantics (including the hardware quirk that a reset alarm is $000000),
and the ICR with its read-clears behaviour.

CIA-A carries /OVL (the reset-time ROM overlay at `$0`), the keyboard
serial port (SDR/ICR with the KDAT handshake and an emulated
keyboard-controller pacing delay), and the fire-button lines. CIA-B
carries the floppy control lines (motor, select, side, step) and the FLAG
input pulsed by the disk index.

The 68000 `RESET` instruction asserts the external reset line without
resetting the CPU core or clearing RAM. Copperline resets the CIA port
state on that line, so CIA-A releases `/OVL` and the boot ROM overlay is
visible again before Kickstart reads the reset vectors.

## Floppy (`floppy.rs`)

The floppy subsystem is track-timed: a drive has a rotational position,
and data under the head right now is what disk DMA sees. Track stepping
pays settle time, direction reversals cost more, and the index pulse fires
once per revolution into CIA-B FLAG. Reads assemble MFM bitstreams from
the 11-sector AmigaDOS track layout; DSKSYNC matching, word-at-a-time
DSKDAT, and DMA into chip RAM behave as Paula documents. Non-WORDSYNC read
DMA drains Paula's recovered 16-bit disk word phase even when DSKLEN is
armed between disk-word boundaries; WORDSYNC is the explicit mode that
realigns framing to a matched sync word before transfer. Supported image
formats: ADF (read/write), gzip ADZ, single file ZIP, DMS (decompressed by
 `dms.rs`), UAE extended ADF, and read-only SCP flux images.
Connected mechanisms with no media keep the active-low disk-change line
asserted; a step pulse only clears that latch once media is actually
present, so guest software sees a no-disk condition rather than unreadable
track data.

Standard ADF and AmigaDOS tracks are synthesized as one PAL-sized
revolution: 11 sectors occupy 5984 MFM words, and the generated revolution
is 6334 16-bit MFM words so the index gap matches normal Amiga floppy
timing. This matters for raw loaders that DMA a fixed-size window and make
their own assumptions about the post-sector gap. UAE extended raw tracks
and SCP flux captures keep their stored track length and per-revolution
timing instead of using this synthetic geometry.

The synthesized drive sounds ([](../guide/configuration)) are driven by
this model's real state transitions -- motor spin-up, seeks, the
empty-drive poll click.

## Known AGA/ECS gaps and non-goals

Most ECS and AGA behaviour is implemented (the register notes above and
[](video)); the chipset gaps that remain are:

- **Sub-unit AGA DDF stop effects** beyond whole-unit completion are not
  modelled; the current model starts from DDFSTRT and rounds DDFSTOP
  through complete FMODE units.
- **Live (beam-timed) collisions** stay on the 6-plane decode: CLXCON2 is
  interpreted in the rendered collision path but not yet in the beam-timed
  `COLLISIONS_AGA_DECODE` path.
- **True 35 ns SuperHires sprite** output is not modelled -- SPRES upgrades
  sprite resolution, but the compositor does not place sprites on the SHRES
  pixel grid.
- The vAmigaTS ECS register-readback sweep has not been run against a local
  checkout; readback is pinned by unit tests meanwhile.

Deliberate non-goals, recorded so they are not re-investigated: A2024 /
UHRES dual-scan display (a one-time "not emulated" warning is kept),
genlock ZD output beyond register storage, and AGA "double CAS"
memory-timing fidelity beyond what `timing-test/` measurements justify.
