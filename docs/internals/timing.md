# The timing model

Copperline's compatibility with cycle-sensitive software comes from one
idea applied consistently: **the chip bus is a single resource arbitrated
per colour clock (CCK), and everyone pays for their slots.** Reference
numbers come from real hardware via the `timing-test/` disk. The Copper
and blitter timing models are documented in full below; the 68000 prefetch
model is in [](cpu.md). Every rule here is backed by named regression tests
(the inline suites in `src/chipset/copper.rs` and `src/chipset/blitter.rs`,
plus `src/bus/tests.rs` and `src/video/bitplane/tests.rs`), and many by a
golden probe render (see [](#cross-checking-against-hardware)).

## Custom-register read sampling

On the arbitrated 68000 path, CLXDAT is sampled and cleared at the start
of its granted word transfer. Pixels clocked during that word or its trailing
CPU clock belong to the next read. Paula INTREQR retains its independently
calibrated end-of-slot sample. Byte reads select their lane; long reads sample
each custom-bus word separately. Other registers retain their existing phases.

Live collision scans use the absolute pixel-comparator conversion
`x = 4 * H - 2 * DIW_HSTART_FB0`, matching the window and sprite coordinates.
The render-write anchor is a different domain and must not advance the
collision cursor. At H=$40 the pixel cursor is x=60; DIWSTRT=$81 opens at
x=62 during that clock. Reading CLXDAT at the transfer's start must not include
that later pixel. DMA grants, rendered positions and register-write events
are unchanged.

Saved A500 kind72/73 sweeps in `amiga-hardware-probes` now match all719
qualified cases:29 early observations repaired in total, including the final
eight. The561 unmatched timing cohorts remain excluded, not counted as matches.
This is scoped validation, not a claim of complete collision accuracy. Named
regressions in `src/bus/tests.rs` check the absolute DIW boundary and retention
of bus-tail collisions across byte/word/long reads. Graphical fixture
observations use the pixel clock without changing their expected bit values.

## Chip-bus arbitration

Every colour clock of every scanline has an owner, decided in priority
order. The fixed-DMA levels (steps 1-5) are decided by
`fixed_dma_owner_at` (`src/bus/dma_slots.rs`); Copper, blitter, and CPU
contend for the slots it leaves free. (The function tests audio before
disk, which is equivalent because their fixed slots never overlap.)

1. **Memory refresh** -- fixed odd slots 1/3/5 plus the line-end refresh
   slot (E2, or E3 on NTSC long lines). The 1/3/5 slots sit on the odd
   parity the Copper never fetches from; the line-end slot lands on the
   Copper's even-slot cadence but stalls only the CPU and blitter -- the
   refresh RGA strobe overlaps a concurrent Copper transfer on the real
   chip, so the arbiter lets a Copper fetch through it
   (`line_end_refresh_slot` in `src/bus/dma_slots.rs`). A Copper stream
   that saturates the line needs that clock: Nexus 7's plasma-zoom text
   reloads 128+3 palette entries per 3-line band between two beam WAITs,
   and blocking E2 slips the chain ~6 cck per band until its BPLCON4
   sprite-bank flip drifts into the sprite window.
2. **Disk DMA** -- fixed slots 7/9/B, when DSKEN is set and a transfer is
   live.
3. **Audio DMA** -- one fixed slot per Paula channel (D/F/11/13), claimed
   only when that channel actually has a fetch pending.
4. **Sprite DMA** -- the fixed per-sprite slot pairs starting at 15/17.
5. **Bitplane DMA** -- the fetch pattern determined by DDFSTRT/DDFSTOP and
   the plane count; at high plane counts this is what starves everyone
   else, exactly as on hardware.
6. **Copper** -- even-CCK fetch cadence when not waiting.
7. **Blitter** -- any remaining slots its schedule claims.
8. **CPU** -- whatever is left.

For FMODE=0 fetches (OCS/ECS, and AGA with a 1-word fetch quantum) the
bitplane decision (step 5) is driven by the Agnus DDF sequencer flop model
(`src/chipset/ddf_sequencer.rs`, walked per line by `src/bus/ddf_line.rs`):
DDFSTRT and DDFSTOP are comparator *edges* that set/clear flip-flops, not a
value range. A stop request drains through one final fetch unit (which
applies the modulos per plane), the hardwired window ($18/$D8) gates
starts and forces stops, and the flop state carries across line
boundaries. Missed comparators therefore produce the
hardware's "old stop" behaviours: a rewritten-too-late DDFSTOP lets the run
continue to the hardware-stop drain, a DDFSTRT match past $D8 starts a run
that wraps through horizontal blanking into the next line, and an
early-blanked DDFSTRT ($10 with SHW still down) never starts on OCS on a
fresh line. Because OCS only clears SHW when a fetch run completes, the
latch survives a run-less line: the next line's below-$18 DDFSTRT match
then does arm a run, anchored at its raw comparator position, so such
lines fetch on alternating rasters with the picture sitting linearly left
of the standard grid (its early words run through the left border). The
renderer honours this through the captured run geometry: a below-$18 run
origin keeps its raw fetch grid, and a line without a captured fetch
paints nothing (vAmigaTS Agnus/DDF/DDF/oldhwstop3/4 A500 photos).

The $D8 stop holds on Alice exactly as on an ECS Agnus. Either relaxes it
to $E0 (the canvas edge) while BEAMCON0 selects HARDDIS or VARBEAMEN, or
BPLCON0 selects SHRES or UHRES (WinUAE's `check_harddis` rule); OCS has no
such controls. A lo-res $28/$E0 window therefore fetches 23 words per line
on an A1200 as on an A500, and software whose modulo assumes that count
displays straight (Pacmania's WHDLoad install; the `ddfprobe-agahwstop`
golden probe).

For a line with no mid-line sequencer writes, the walked fetch plan is
keyed on the complete carried flop state, masked DDF registers, line
geometry, chip revision and hard-stop mode. An identical key reuses the
preceding line's immutable slots and end state; the ordinary static walk
is allocation-free. DDFSTRT/DDFSTOP/BPLCON0/DMACON/DIW writes invalidate that
plan and rebuild the affected line (DDF writes commit to the comparators
four colour clocks after the write slot; an old DDFSTOP still fires on its
commit clock, an old DDFSTRT does not -- vAmiga's sequencer semantics,
hardware-verified in aggregate by the vAmigaTS
Agnus/DDF/DDF/oldhwstop1-4 A500 photos). Because the complete initial state
is in the key, a run carried across horizontal blanking or a vertical-window
transition cannot accidentally reuse an ordinary interior line. A
mid-row BPLCON0 change switches the fetch-unit slot layout from its commit
clock; word addressing is unit-based, so late-enabled planes keep their
word positions and earlier words stay zero.

The lo-res fetch unit is eight colour clocks with eight usable DMA slots.
OCS/ECS Agnus drives six of them (slot order 4,6,2,3,5,1 at unit offsets
1,2,3,5,6,7), leaving offsets 0 and 4 free for the Copper/blitter/CPU --
which is why lo-res tops out at six bitplanes there. Alice drives those two
remaining slots once BPLCON0 asks for more than six planes (plane 8 at
offset 0, plane 7 at offset 4, giving the full order 8,4,6,2,7,3,5,1), so an
eight-bitplane AGA lo-res screen leaves no spare bitplane slot in the unit.

Wide-FMODE (quantum > 1) fetches keep the memoized value-window plan: the
effective DDF window, fetch cadence, and per-plane fetch-order mask live in
a `BitplaneSlotPlan` keyed on the register inputs (`BitplaneSlotKey`,
`src/bus.rs`). On a line without a fetch-affecting write, that plan is
published as a complete per-line slot mask and is also shared with the DMA
capture path for its stable cadence and row width. The colour-clock arbiter
therefore performs a bit test, while capture does not re-derive the same DDF
geometry on every quantum. A mid-line DDF, BPLCON0, DMACON, DIW or FMODE write
invalidates the publication and selects the existing block-delay-aware
calculation for the rest of that line. If a delayed BPLCON0 or DMACON change
crosses the line boundary, the following line also remains dynamic until the
delayed value has taken effect. A restored save state likewise keeps the
remainder of its partial line dynamic, then republishes from the next
unchanged line. Wide-FMODE lo-res slots are packed into the first eight
CCKs of each 16/32-CCK fetch unit; the rest of the unit remains available
to later arbitration priorities.

The value-window model still honours the DDF start comparator's single-cycle
match. A DDFSTRT write that moves the match position to the current colour
clock or behind the beam, before the position it replaces has fired, leaves
that line with no start at all: the counter never returns to the new value
before the horizontal wrap, so the line fetches nothing and no bitplane
pointer advances. A write landing after the flop has set cannot un-start the
run -- only DDFSTOP ends it. Restarting the fetch from the moved comparator
instead truncates the first unit, so only the planes whose lo-res slot number
survives it fetch, and those pointers stay one wide-fetch word ahead of the
rest for the remainder of the display. Microcosm's CD32 status panel is the
regression example: its Copper repoints seven bitplanes and drops DDFSTRT
`$2C` to `$18` in one burst that overruns the line, committing the new value
at hpos ~`$1E` on the panel's first line, which desynchronised BPL5 and BPL1
(lo-res slots 6 and 7) from the other five planes and speckled the whole
panel. Covered by the `ddfprobe-ddfmiss` golden probe, verified against
vAmiga 5.0b1's `A1200_2MB` AGA setup.

Slow RAM at `$C00000` is arbitrated through Agnus *like chip RAM*: a CPU
access to slow RAM contends with DMA even though the RAM is outside the
chip range. (Getting this wrong is observable: too-fast slow RAM can race a
guest DMACON clear against the vertical-blank interrupt.) Fast RAM
and ROM are external-bus accesses billed at the CPU clock without chip-bus
arbitration -- so an accelerated CPU speeds up exactly what a real
accelerator would.

The A1200's 32-bit Alice path moves an aligned longword in one granted CPU
slot, but the slot is not the complete latency of a read. A 68020 chip-RAM
read, instruction-cache fill, or custom-register read waits one more CCK for
the chipset data-return phase; a write can be posted after the 020's shorter
three-clock local-bus cycle. Sequential instruction words from the same
aligned longword use the AGA fetch latch and do not request or wait for the
same value twice. This read/write asymmetry is covered by
`aga_68020_chip_reads_wait_for_data_but_writes_are_posted`.

### Deferred timed-device ticks

The chipset and beam advance every colour clock, but the *timed devices*
the CPU can only observe indirectly -- the CIAs, serial port, pots, audio,
floppy, Akiko -- are not ticked per CPU bus access (that dominated the host
profile). Instead their colour clocks accumulate in `pending_device_tick`
and `flush_timed_devices` applies them in one batch at the next observation
boundary: a CIA/custom/peripheral register read or write, or an instruction
boundary for interrupt recognition. Read-only custom registers
(INTREQR/DSKBYTR/SERDATR/POTxDAT) and writes that change device state
(INTREQ/INTENA/ADKCON/DSKLEN/AUDxxx/SERDAT) flush first, so a device always
reflects time right up to the moment it is read or written. The CIA E-clock
divider and every device tick are exact under batching, so observable timing
is unchanged -- the accumulator is a host-CPU optimisation only, and it is
flushed to zero at every frame boundary (so it is never serialized into a
save state).

### CPU vs blitter

BLTPRI affects ownership only on colour clocks when the blitter actually
requests the chip bus. A requested slot goes to the blitter ahead of the CPU;
startup ladder cycles, the empty first-D bubble, line-mode Bresenham cycles,
fill idle cycles, and disabled-channel gaps do not create a separate warm-up
fence. The CPU can use those idle slots even when BLTPRI is set. With BLTPRI
clear, the blitter still holds requested slots until the CPU has missed three
consecutive chip-bus clocks, then yields one slot; fixed-DMA denials count
among those misses. The starvation threshold and reset rule are described in
[](#cpu-contention) below.

This model is supported by chip-bus regressions and aligned One ROM captures,
not title-specific timing assumptions. An independent colour-clock sequence
model that gives BLTPRI precedence only when a blitter bus request is asserted
reproduces every one of the 3,456 hardware priority-on busy samples. Across
busy, IRQ and destination-memory observers, 3,456 aligned trials for each of
the two BLTPRI settings match both hardware and WinUAE in masked state and
elapsed colour clocks: 20,736 hardware trials total, with no mismatches. The
priority-on busy and IRQ ROM source IDs are `93adfec44fe58ea38aba0e07e81f7949d1eaf519dc6bcc5914d10f2f5d2ccf1f`
and `18786132832d8aab99388ce1a1c52fbd12ff01465f40a73d21c71799f07edd53`.
The full capture provenance and scope are documented in the probe repository
at `docs/rom-timing-isolation-2026-10-14.md`.

### Sprite DMA control rewrites

Sprite DMA fetches POS/CTL at a channel's fixed pair slots on its vstop
line, and DATA/DATB on the lines between vstart and vstop; the
register-level channel model is described in [](chipset.md). Standard hard
vertical blank suppresses those fetches until PAL line $19 or NTSC line
$14, so SPRxPT writes made before that boundary set where the field's first
control-word fetch reads rather than retargeting words that could not yet
have been fetched. Software can rewrite SPRxPOS/SPRxCTL while a sprite is
pending, or before a later pair slot, to reposition an already active
sprite on that scanline. Those writes update the channel's register copies
and vertical comparators (re-evaluated at once when they land on the
matching line), but they leave SPRxPT alone: the data stream continues
from the live pointer, and only the rewritten HSTART applies from that
line. An SPRxPT write while a sprite is pending or armed retargets the
next fetched data words, including for a sprite armed directly by CPU
POS/CTL writes. The whole per-channel state is chip state and is
serialized in save states. Tests:
`pending_sprite_control_rewrite_preserves_descriptor_data_origin`,
`active_sprite_control_rewrite_preserves_descriptor_data_origin`,
`pending_descriptor_sprite_pointer_write_retargets_data_stream`,
`after_slot_armed_sprite_pointer_write_seeds_dma_data_stream`.

Beam-timed SPRxPOS writes are replayed in Denise's horizontal-comparator
domain, seven colour clocks ahead of the normal register-output position
(`SPRITE_REGISTER_WRITE_PIPELINE_CCK` in `src/video/bitplane.rs`). Copper
writes use three, which takes back out the four-clock WAIT-comparator
lookahead already in the Copper's landings.
This matters for manual sprite reuse with sprite DMA disabled: Copper lists
can write consecutive SPRxPOS values whose HSTARTs exactly abut. SPRxDATA
and SPRxDATB writes update Denise's data latches at their ordinary beam
position, but the sprite serializer copies those latches only when the
horizontal comparator fires. A same-line DATA/DATB write after that compare
therefore waits for a later compare or scanline instead of replacing the
word already shifting. Same-line POS/CTL writes also do not truncate a
manual sprite word that has already started shifting; they re-arm a later
compare while the active word completes. Tests:
`manual_sprite_position_write_before_hstart_uses_sprite_compare_domain`,
`manual_sprite_position_writes_use_denise_compare_lag`,
`manual_sprite_position_write_does_not_truncate_started_word`,
`manual_sprite_data_write_after_compare_waits_for_next_scanline`,
`sprite_register_data_write_after_compare_preserves_dma_latch_on_same_beam_line`.

## The Copper

The Copper (`src/chipset/copper.rs`) is a two-cycle processor that
accesses chip RAM on every *other* colour clock. The HRM describes MOVE as
a two-word, two-memory-cycle instruction; Copperline models the cadence
and its edge cases in detail.

### The fetch cadence and the MOVE write boundary

The two-CCK cadence is locked to the beam's horizontal parity, not a
free-running internal phase: the Copper fetches on even in-line colour
clocks and idles on the odd ones (`Copper::hpos_is_access_cycle`). So:

- A MOVE spans **four colour clocks** (fetch, idle, fetch, idle), leaving
  the alternate clocks free for the blitter/CPU. Modelling the Copper as
  owning every clock while running would starve a chip-bus-bound CPU
  during dense Copper effects such as horizontal colour gradients.
- WAIT and SKIP span **eight** colour clocks, spending a two-Copper-cycle
  tail after their two word fetches (the Minimig
  FETCH1/FETCH2/WAITSKIP1/WAITSKIP2 sequence; vAmiga COP_WAIT1/COP_WAIT2
  at fetch2+2 and fetch2+4): an immediately-true WAIT or a SKIP resumes
  fetching at fetch2+6.
- The custom-register side effect occurs on the second word fetch, i.e.
  the third of the four colour clocks: three back-to-back MOVEs starting
  at beam `hpos` write at `hpos + 2`, `hpos + 6`, and `hpos + 10`.
- The WAIT comparator's horizontal input runs **two colour clocks ahead**
  of the beam (`CopperWait::comparator_is_satisfied`), wrapping through 0
  over the last three clocks of a line. A sleeping WAIT therefore wakes at
  target-2 -- the match colour clock is the bus-free wake-up cycle -- and
  the next instruction's first fetch lands exactly on the masked target,
  its write two clocks later. Cross-verified against vAmiga with the
  two-sided landing probes (`COPPERLINE_DIAG_COP_WRITES` /
  `VAMIGA_COP_PROBE`): a `WAIT $4721` + `MOVE SPR0CTL` lands the write at
  hpos $22 in both emulators (vAmigaTS spritedma/interfere2, matching the
  real-A500 photo).
- The wake-up itself is a real Copper cycle: it must land on an
  access-parity colour clock that fixed DMA (bitplane/sprite/disk/audio/
  refresh) leaves free (vAmiga's `COP_REQ_DMA` reschedules until the bus
  is free). On a quiet bus this is invisible -- the match clock doubles as
  the wake-up. Inside a 5/6-plane lores or hires fetch window, where
  bitplanes own even colour clocks too, the wake-up is pushed to the next
  free slot and the woken MOVE's fetches slip one free slot further (a
  6-plane lores group leaves only offsets 0 and 4 free, so the write
  lands a full slot later than the quiet-bus spacing). Shadow of the
  Beast's title relies on this: its per-line `WAIT (v,$40)` /
  `MOVE COLOR00` toggles tuck against the display window edge at colour
  clock $48 only because the wake-up consumes the free slot at $40
  (`ddfprobe-sotb`/`-sotb2` pin the quiet-bus and starved landings against
  vAmiga; `copper_wait_wakeup_spends_a_free_slot_under_lores6_fetch`).

Anchoring the cadence to the beam rather than to a carried-over flip-flop
is what makes a back-to-back colour MOVE list land its writes at the
*same* hpos on every line. With a free-running phase, fixed-DMA
interference drifts the phase line-to-line and a Copper-driven horizontal
gradient shimmers instead of showing clean vertical bands. The parity
check is applied by `Copper::step_eligible_slot`, the single primitive
shared by the live bus path and the blitter-deadline predictor's cloned
simulation, so prediction and execution cannot drift apart.

When the 68k is halted in STOP, its idle fast-forward path may encounter a
Copper in the steady comparator phase of WAIT. It computes that WAIT's exact
beam/blitter deadline once and leaves the comparator dormant strictly before
the deadline while the ordinary DMA arbiter continues to own every colour
clock. The shortcut is capped at the field wrap because the vertical-blank
COP1LC strobe supersedes a wait that would otherwise extend into the next
field. Instruction-tail and wake-up cycles remain individually stepped.

Ordinary CPU-driven advances use the same invariant through a cached bound
(`Bus::copper_sleeping_before_wake_bound`, consulted by every bus quantum):
once a WAIT is in its steady comparator phase, the earliest colour clock at
which it could release -- the nearest of its position, a pending frame
restart and the field wrap, with the end-of-list WAIT bounded by the wrap
alone -- is computed once and kept as an absolute clock. Quanta strictly
before it leave the comparator dormant; at it, the comparator runs every
eligible slot again and wakes the Copper at the slot it always would. A
WAIT already at its position, including one held by a running blitter,
gets no bound: the wake slot and the blitter-finished condition stay with
the per-clock path. The bound is dropped by whatever can move the wake --
a COPJMP strobe, VPOSW/VHPOSW, a DMACON write that changes the Copper's
own DMA gate, a sleep/wake transition, reset -- and is never saved.

Register writes take effect a fixed number of colour clocks after the
chip-bus slot that carried them, and the delay is a property of the
register pipeline, not of the bus master. Denise-boundary registers apply
to the pixel pipeline about four colour clocks after the slot
(`DENISE_WRITE_EFFECT_DELAY_CCK`; vAmiga: a register-change delay plus
pixel-domain application offsets). Agnus's two-cycle register class
(DMACON, BPLxPT, BPLxMOD, SPRxPT; vAmiga `recordRegisterChange(DMA_CYCLES(2))`)
applies two colour clocks after the slot (`AGNUS_WRITE_EFFECT_DELAY_CCK`);
the bitplane/sprite DMA-gating replay is calibrated against events
recorded at that position. Render events are recorded at the effective
position for *every* writer: a Copper MOVE executes at its bus slot, so
its event records at the current beam position plus the delay; a CPU
write is applied once its whole bus cycle has been billed (the beam sits
past the granted slot by then), so its event is referenced from the
granted slot itself (`Bus::cpu_custom_access_slot`) plus the same delay
(`Bus::record_render_write`). Copper-sourced events currently record the
Denise delay for the Agnus two-cycle class as well -- the copper-driven
DMA-gating replay was calibrated with that offset when the copper
landings became bus-exact (a documented TODO). The Denise delay was
verified two-sided with the `COPPERLINE_DIAG_CPU_WRITES` /
`VAMIGA_CPU_PROBE` landing traces on the vAmigaTS DMACON sprena dense CPU
`COLOR00` stream: with landings matched line-for-line, the rendered rows
sat exactly two colour clocks left of vAmiga until the CPU side carried
the same slot-referenced delay; the Agnus delay is pinned by the DMACON
bplon bitplane-gating bars, which sit 8 px right of vAmiga when those
events carry the Denise delay instead.

For the low-res renderer, a same-line `COLORxx` event recorded at `hpos`
starts affecting pixels at `(hpos - $35) * 4` (`COLOR_WRITE_HPOS_FB0` in
`src/video/bitplane.rs`); beam-timed placement is anchored at
`COPPER_WAIT_HPOS_FB0` ($28), and bitplane-control writes add the
fetch-to-display pipeline offset (`BITPLANE_CONTROL_PIPELINE_FB`). This
models COLORxx on Denise's final palette/output phase, one lores pixel
ahead of writes that feed delayed shifter/control paths. OCS Denise (8362)
and ECS Denise (8373) share this timing; the only OCS/ECS colour-path
difference is the OCS 12-bit value mask. (AGA Lisa delays colour changes by
one hires pixel relative to OCS/ECS; the AGA replay adds that one
framebuffer sample after the common COLOR write anchor.) AGA BPLCON4's
sprite palette-base byte uses Lisa's earlier sprite colour-lookup path at
`(hpos - $36) * 4` (`SPRITE_PALETTE_CONTROL_HPOS_FB0`), one lores pixel
ahead of ordinary COLORxx replay. Tests:
`copper_move_writes_visible_registers_on_second_dma_slot`,
`copper_move_spends_four_color_clocks_leaving_alternate_cycles_free`,
`color_register_writes_use_final_output_position`.

BPLCON0's HAM select rides that same colour-selection phase: it does not
feed the bitplane shifter, it picks how the already-serialised index
becomes a colour. A HAM change and a `COLORxx` write carried by the same
chip-bus slot therefore reach the picture at the same pixel, so the
renderer samples the HAM bit `DENISE_HAM_SELECT_PIPELINE_FB` framebuffer
pixels left of the generic register domain the rest of BPLCON0 (plane
count, resolution -- the fetch/serialiser side) is sampled in. vAmiga
models the same relation from the other end: `Denise::setBPLCON0` records
the change one colour clock after the bus slot and then backs it up by one
colour clock, landing on the `pos.pixel()` that `Denise::pokeCOLORxx`
uses. A saturated segment position (a write recorded past the display
window) keeps the register-domain position so it cannot be dragged back
into the visible line. Regression example: Hollywood Poker Pro draws a HAM
photo and an EHB (6-bitplane) scoreboard on the same scanlines and
clears HAM at `WAIT hp=$A2`; in the generic domain the switch landed 26
lo-res pixels late and the scoreboard's left 24 columns decoded as HAM
modify-blue. Tests: `ham_select_lands_in_the_colour_write_domain`, and the
vAmiga-verified `hamprobe-select` golden render.

### WAIT/SKIP edge cases

The Copper compares its masked beam position against the Agnus beam
counters; the BFD bit controls whether a satisfied position wait must also
wait for the blitter to go idle.

- Full-mask waits are satisfied once the beam reaches or passes the target
  in the current 8-bit vertical phase. Near the end of PAL line 255,
  low-half targets wait for the short post-rollover tail while high-half
  targets (e.g. `$fc`) stay satisfied because that phase has passed;
  partial-mask waits with the high vertical bit set likewise stay
  satisfied across the line-255-to-256 rollover.
- With BFD clear a position-satisfied wait parks while the blitter is
  busy and resumes once the scheduled blit consumes its slots; with BFD
  set it ignores a busy blitter.
- The WAIT comparator cannot release in the last 4 colour clocks of a line
  (`WAIT_RELEASE_LINE_END_BLACKOUT_CCK`); a wait satisfied there releases
  at its target on a following line, unless its compare position lies
  *inside* the blackout (the only clock where it is ever true), which
  still releases there.
- The comparator is combinational and runs on **every** colour clock,
  including ones owned by fixed DMA (bitplane/sprite/disk/audio/refresh);
  bus contention only delays the next *fetch*, never the release decision.
  The CDTV extended-ROM boot list depends on this: its `WAIT vp=$FF
  hp=$DE` is only releasable at hpos $DE of line 255, which sits inside
  the last DDFSTOP=$D8 overscan fetch unit -- evaluating the comparator
  only on Copper-eligible slots lost the release and the follow-up
  display-off MOVE. Tests:
  `wait_release_is_blocked_in_line_end_blackout`,
  `copper_wait_comparator_runs_under_fixed_bitplane_dma`.

### COPJMP and frame reload

- A CPU write to COPJMP1/COPJMP2 loads the Copper program counter
  immediately, but the target list has no visible effect until the Copper
  gets DMA slots to fetch it.
- A CPU *read* of COPJMP1/COPJMP2 fires the same strobe: the register
  decode acts on the address alone (reading a write-only register performs
  a bus write of the floating data-bus residue into it -- the UAE/vAmiga
  model), while the CPU reads back the undriven bus. Vertical-blank
  handlers rely on this (`move.l #list,COP1LC` + `tst.w COPJMP1`) to make
  a double-buffered list swap take effect in the same frame, before
  display fetch begins (regression example: the Sleepwalker CD32
  logo/intro double-buffer flip, which without the read strobe shows each
  new pose one frame early in the still-displayed buffer). Test:
  `copjmp_strobe_fires_on_read_access`; golden probe:
  `timing-test/copprobe-jmpread.asm` (vAmiga-verified).
- A Copper MOVE can update COP1LC/COP2LC; a later COPJMP strobe branches
  through the *current* value. A Copper MOVE to COPJMP1/COPJMP2 spends its
  second word fetch on the strobe, then two more bus-free Copper cycles
  (vAmiga COP_JMP1/COP_JMP2); the program counter reloads on the second of
  those, so the first fetch from the new list lands three Copper cycles
  after the strobe (verified against the vAmiga copper trace: a COPJMP2
  MOVE at hpos $04 fetches the target list's first MOVE at $0A, its write
  landing at $0C).
- The automatic frame reload latches the current COP1LC at end of frame
  and restarts the Copper at the top of the next frame (vpos 0) through
  the vertical-blank lines -- it branches through a Copper-programmed
  COP1LC value, so a MOVE to COP1LC changes where the next frame restarts.
  The `tests/README.md` manual chipset workflow records the falling-man
  handoff regression example (`t=165s`/`t=180s` frame dumps).

Copper writes to "dangerous" registers are gated by COPCON's CDANG bit.
References: HRM [Coprocessor Hardware, chapter
2](https://erikarn.github.io/amiga/docs/Amiga_Hardware_Reference_Manual_3rd_edition.pdf).

A 68000 byte write drives the byte onto both halves of the data bus, and
the custom chips latch the full 16-bit word (they have no byte lanes), so
`move.b v,COLOR00+1` lands `$vvvv` in the register and a byte bit
operation such as `bset #1,COPCON` sets CDANG. The vAmigaTS `CIA/oldcnt`
cnt1/cnt3/cnt5 ramps (which paint `move.b TALO,COLOR00+1` mid-count)
photograph the mirrored high byte on real hardware. Test:
`custom_byte_write_drives_the_byte_onto_both_bus_halves`.

## The blitter

The blitter (`src/chipset/blitter.rs`) is not a do-it-all-at-once engine
with a delay: it is **scheduled per DMA slot**. Each word issues its
hardware channel bus sequence, so DMACONR's BBUSY reflects genuine
in-flight state and BBUSY-polling loops see hardware timing. Because the
renderer and the CPU both need to know when a blit finishes,
`cck_until_blitter_completes` (`src/bus/dma_slots.rs`) predicts completion by walking
the same slot-eligibility primitive the live engine uses.

### Per-slot FSM

Scheduled blits use explicit phases for the hardware controller. ROM-resident
One ROM probes on a PAL A500 (ECS Agnus, OCS Denise, 512 KB chip + 512 KB slow)
separately observe busy, interrupt and destination-memory reads. Launch phases
are aligned before starting the blit; the observer's first chip-bus access is
the measured read, followed by a raw beam timestamp. Across all normal USE
masks, eight D-enabled fill masks and two line modes, the busy, IRQ and memory
observers cover 20,736 trials across BLTPRI clear and set. Hardware, WinUAE
Best Compatibility and Copperline match in observed state and elapsed read
time. DMACONR.BZERO is measured separately: three clocks of normal propagation,
plus one for fill's USE-D/no-USE-C idle phase, match the complete 4,608-row
packet byte-for-byte. Broader line-mode flag semantics remain unresolved.
The [measurement report and remaining plan](a500-timing-measurements-2026-10-14.md)
records the scope and comparison baselines. The measurements refine the earlier
cross-emulator slot traces:

- **Startup**: CPU-launched normal-mode StartDelay and Init each consume a
  fixed-DMA-free colour clock after the BLTSIZE write completes. They do not
  claim the bus and can advance alongside Copper transfers. A500
  source-overwrite races distinguish the first-stage stall under six-plane DMA
  from the second-stage stall under four-plane DMA. Copper-launched blits retain
  the separately calibrated deferred register pipeline and internal startup;
  line startup also retains its internal timing. Adding extra register-commit
  clocks here double-counted time already
  advanced by the CPU register-write bus cycle.
- **Body**: the source cadence follows the enabled-channel speed table: A
  is always visited, B and C only when enabled, and D when D is enabled
  or no C next-word state exists. D output is delayed through the hold
  register: the first D phase of every program is the HRM "-" bubble and
  does not claim the chip bus -- including D-only clears, which write
  "-- D0 -- D1 | -- D2" on hardware. The first destination word is
  written on the next D slot and the final word in the F flush slot.
- **Termination**: the last body cycle is followed by E (internal flush) and
  F (the final D write when enabled). BBUSY clears when E completes; the engine
  remains active until F completes. The interrupt is armed at E's completion
  and asserted three colour clocks later, independently of a stalled F write.
  The bus countdown starts at four because the arming clock itself decrements
  it once. A chip-RAM CPU read samples its own granted slot before the trailing
  bus-cycle clock can perform a blitter write. Reading RAM after that tail
  incorrectly returned a destination word which hardware had not yet exposed
  to the CPU. Custom-register readback retains its separate timing path.
  The register file is released when BBUSY drops, so terminal cycles
  retire the queued D word without overwriting registers programmed for
  the next blit.

Normal-mode A/B barrel-shifter carry is cleared at the first word of a new
BLTSIZE, then carries from the last source word of one row into the first
source word of the next inside that blit; masks, modulos, and fill carry
still observe row boundaries.

Normal A/B sources are read from live chip RAM in their granted DMA slots,
just like C. BLTSIZE latches registers, not the contents of future source
addresses: A500 kind77 copies a CPU overwrite made after launch when the CPU
wins before the source fetch. An already fetched word remains latched for
the shifter/minterm pipeline. D-to-A/B feedback and physical aliases therefore
follow live RAM too. Legacy snapshot/overlay fields remain loadable in saved
pending blits, but do not determine source data; new blits allocate no source
snapshot buffers.

Line blits follow vAmiga's four line
micro-programs, indexed by the USEB/USEC pair: with USEB clear each pixel
is four cycles (L1-L4: L2 fetches C when USEC is set, L3 propagates, L4
stores), and with USEB set each pixel is six (an LB cycle fetches B --
adding only BLTBMOD to BLTBPT -- and a bare LBus cycle allocates the bus
without a transfer). With USEC set the line D cycle allocates the bus
even when SING suppresses the store (unlike copy mode, where a locked D
slot is bus-idle); with USEC clear no line cycle touches the bus, and the
terminal BLTDONE cycle of a USEB program is itself a bus-idle cycle.
A SING-suppressed dot only locks the store: the A shifter, minterm and
BZERO update still run on the full inputs.

One ROM measurements on the PAL A500 additionally show a register-texture
(USEB clear) BZERO evaluation after the texture counter wraps from BSH=0
to 15 between pixels. The model evaluates the preceding pixel's shifted A
and C holds with B=$ffff; it does not write a D word. That B input fits the
earlier selected cases, but is not a universal hardware rule: the
[one-hot and minterm controls](a500-copper-blitter-followup-2026-10-17.md)
find repeatable exceptions at effective A bits 13 and 11, despite matching
destination data and direct-A flag controls.
L2 evaluates those old holds before fetching the next C word, and L3
exposes the queued clear. A blit starting at BSH=15 has no preceding pixel
and does not trigger this check. Save states preserve the prior A hold and
pending clear. The wrap-stage input/reduction exceptions, B-DMA and other
non-wrap effects remain unresolved; see the
[One ROM measurement report](a500-line-bzero-2026-10-15.md).

The Bresenham error
accumulator (BLTAPT's low word) advances only while USEA is enabled, the
line texture register is BLTBDAT rotated by the LIVE BSH each pixel (no
write-time latch, unlike the USEB-off copy hold word), and C DMA fetches
load the BLTCDAT register itself, so a later USEC-off blit consumes the
last fetched C word. At completion the hardware-visible ASH, BSH, SIGN,
and low-word BLTAPT accumulator state is written back. Verified
cycle-for-cycle against the vAmiga line traces (bususage1l/5l/15l): both
emulators run the pixel micro-cycles, bus grants and stalls on the same
colour clocks relative to the BLTSIZE poke. Tests:
`scheduled_normal_mode_bbusy_start_delay_precedes_first_source_slot`,
`blit_pipeline_identifies_idle_cycles_per_hrm_diagrams`,
`scheduled_normal_clear_writes_progressively`,
`scheduled_line_mode_latches_c_source_before_store_phase`,
`scheduled_shift_carry_crosses_normal_mode_row_boundary`,
`scheduled_a_shift_zero_fills_first_word_of_new_blit`.

B DMA also updates raw BLTBDAT and the retained B shifter hold at each fetch,
so a later operation with USEB clear consumes the last fetched/shifted data.
One ROM A500 controls distinguish the line texture used to select a pixel
from the hold retained for a subsequent normal-mode blit. With B DMA disabled,
the first pixel retains a zero-filled shift of the raw word; subsequent
pixels retain the pre-rotation texture. B-DMA lines retain the post-rotation
texture instead. Line completion preserves the previous raw B word as carry
for a later CPU BLTBDAT write using the then-current BSH. These updates occur
on fetch/pixel events, so terminal drain does not overwrite a newer CPU write.
The [follow-up measurements](a500-copper-blitter-followup-2026-10-17.md)
record the tested patterns, shifts, lengths and write orders. This corrects
retained data; the separate inter-pixel BZERO exceptions remain unresolved.

### Mid-operation register writes

The HRM documents the blitter as an asynchronous DMA engine, says BLTSIZE
starts a blit and must be written last, and tells software to check
BLTDONE before touching blitter registers. Copperline's deterministic
classification of writes while BBUSY is set:

| Register group | Classification | Notes |
| --- | --- | --- |
| `BLTCON0` | Immediate D disable | Clears the current blit's remaining D output path; the register value still updates immediately. |
| `BLTCON1` | Immediate / snapshot-protected | The public register updates immediately, but the in-flight blit keeps its normal/line snapshot so a transient line-mode bit does not reinterpret the active pipeline. |
| `BLTCON0L` (ECS) | Deferred | The minterm-only write drains the current blit before latching. |
| `BLTAFWM`, `BLTALWM` | Deferred | First/last-word masks captured by the scheduled state at BLTSIZE. |
| `BLT[ABCD]PTH/PTL` | Deferred | Pointer writes do not retarget the already-scheduled transfer. |
| `BLT[ABCD]MOD` | Deferred | Modulos captured at BLTSIZE. |
| `BLTBDAT` | Immediate, write-time shifter | The write runs the B barrel shifter with the BSH/DESC current at write time (unlike BLTADAT/BLTCDAT); USEB-off blits consume that latched hold word. Normal-mode completion resets the next write's old-word input; line completion preserves the raw B carry. B DMA also updates raw data and the hold. |
| `BLTSIZE` | Deferred restart | A second start strobe drains the current blit first, then starts the replacement from the post-completion pointer state. |
| `DMACON.DMAEN/BLTEN` | Immediate | Gate blitter bus grants immediately; clearing leaves BBUSY set and preserves the pending blit until re-enabled. |
| `DMACON.BLTPRI` | Immediate | Bus arbitration observes the priority bit directly. |

No covered mid-operation write is modelled as ignored; less common writes
are treated as deferred by draining first. Tests:
`busy_bltcon0_write_disables_remaining_d_output_without_draining_blit`,
`busy_bltsize_write_finishes_current_blit_then_starts_replacement`.

Writes after BBUSY has dropped, while the terminal cycles are still
pending, program the next blit. They never reach the finishing one:
`BLTCON0` latches without disabling the queued final D word, the data
and control registers keep the values software wrote, and the deferred
groups above still drain the terminal cycles before latching. A 68000
cannot land a register write in that two-cycle window after a BBUSY poll,
but the 020+ chip-bus timing can. Treating such a write as mid-blit
dropped the final D word, which for a one-word, one-row blit is its whole
output: graphics.library's per-plane `BltBitMap` loop on a fast A4000
lost entire bitplanes of small copies.
Tests: `bltcon0_write_after_bbusy_poll_keeps_pending_final_d_write`,
`register_writes_after_bbusy_drop_survive_blit_completion`.

### Micro-cycle stall classes

Non-bus blitter cycles come in two hardware classes, mirrored from
vAmiga's micro-instructions and encoded as `BlitSlotClass::BusFree` and
`BlitSlotClass::Internal` (bus-access cycles are `BlitSlotClass::Bus`):

- **Bus-free** cycles (vAmiga BUSIDLE: the D pipeline bubble, area
  fill's extra idle cycle, the BLT_STRT startup cycles, a line blit's
  internal Bresenham cycles) advance only on colour clocks the blitter
  could have won: they stall while the Copper or fixed DMA owns the
  clock, and on the clock a starved CPU is granted (the BLS line blocks
  `busIsFree`). They never allocate the bus, so the CPU can use the same
  colour clock on an uncontended access.
- **Internal** cycles (vAmiga NOTHING: the BLTSIZE register commit, the
  micro-program begin cycle, the terminal D-hold flush and a D-less
  BLTDONE) elapse on every colour clock regardless of bus ownership.

Verified cycle-for-cycle against the vAmiga slot traces: a Copper-poked
blit on a 6-plane display line stalls its BLT_STRT cycles through the
display fetches and lands its first A fetch on the same colour clock in
both emulators.

(cpu-contention)=
### CPU contention

With BLTPRI clear, Copperline models the Agnus blitter-slowdown counter
(the Minimig RTL `bls_cnt`). A busy "nice" blitter holds every slot in which
its pipeline requests the chip bus; the CPU receives no regular alternate
slot. Each consecutive chip-bus clock that denies the waiting CPU advances
the counter, including fixed-DMA denials during a disabled-channel gap or
pipeline bubble. An idle blitter phase does not itself reset the counter; a
CPU grant does. After `BLITTER_SLOWDOWN_CPU_MISS_LIMIT` (3) misses, the
blitter yields one slot, matching the HRM "one bus cycle in four" rule and
the Minimig RTL. BLTPRI disables this starvation yield.

With BLTPRI set, the blitter wins only a slot where its current micro-cycle
requests a bus access. Idle startup and pipeline cycles remain available to
the CPU, including startup-ladder cycles and the empty first-D bubble. Once
running, fill's explicit idle cycle and line-mode BUSIDLE likewise do not
request the bus. The aligned One ROM busy, IRQ and memory observer captures
match hardware, WinUAE and Copperline for both BLTPRI settings, across 3,456
trials per observer and setting. The separate kind-22 BZERO matrix also matches
the tested normal/fill readback; line-texture controls expose additional flag
behavior that remains under investigation. The historical game-specific
examples are removed as evidence for the rule.

A 68000 `TAS` read-modify-write is unsafe on chip RAM (the HRM warns
against it); the `m68k` backend exposes it as a byte read then a byte
write, so Copperline models `TAS` against chip RAM as two separately
Agnus-arbitrated accesses, each subject to the same back-pressure rule.
Tests: `blithog_clear_busy_blitter_yields_to_cpu_only_after_starvation`,
`bltpri_cpu_uses_first_d_bubble_but_waits_for_bus_accesses`,
`bltpri_line_blit_bresenham_cycles_stay_cpu_available`,
`bltpri_leaves_startup_and_first_d_bubble_available_to_cpu`.

The Frame Analyzer's CPU wait view (and the `cpu` record of a
`profile.start` export) attributes each denied colour clock to the actual
owner: fixed DMA (`refresh`, `bitplane`, `sprite`, `disk`, `audio`), the
Copper's access clock (`copper`), a BLTPRI-clear blitter hold (`blitter`), or
a BLTPRI-set blitter bus access (`blitter_nasty`). An idle pipeline cycle is
not attributed to the blitter. The `port` class describes the 020+ chip
port's two-clock turnaround, a wait on the CPU's own bus unit rather than a
denial. A synchronous grant sees the CPU's arbitration view, including the
three-miss yield; a posted 020+ write drains through ordinary arbitration,
where a busy blitter keeps every requested access cycle. Tests:
`frame_analyzer_attributes_cpu_wait_to_nice_blitter`,
`frame_analyzer_attributes_cpu_wait_to_bltpri_bus_access`.
The analyzer has two recording levels. The cheap level keeps the one-byte
owner and CPU-wait grids used by the live heatmap. The full level adds one
24-byte record per raster colour clock and is armed only for an open Frame
Analyzer or `profile.start {"slots": true}`. Producers annotate the arbitration
slot where the signal already occurs: the DDF sequencer's comparator strobes,
the blitter pipeline's current A/B/C/D access and terminal D write, Copper's
fixed WAIT wake and SKIP result, the CPU's current fetch/data/custom access,
CIA E-clock phase, DMA source pointers/data, beam flops, and interrupt/STOP
edges. The recorders observe existing state and do not advance a device or
perform a side-effecting memory read. Tests:
`full_bus_record_is_exactly_24_bytes_and_owner_only_trace_stays_cheap`,
`full_bus_events_match_ddf_blitter_and_copper_probe_cadence`.

### Area fill

Area fill is applied only when BLTCON1.DESC is set (the HRM requires
descending mode because the fill carry propagates in descending bit order
across each row); IFE/EFE in ascending mode is treated as ordinary minterm
output. With USEC clear, fill adds one extra **idle cycle** (no bus
access) per word, placed AFTER the D slot -- the trailing "-" in vAmiga's
fill micro-programs for USE masks 1/5/9/D ("A0 -- -- A1 D0 -- A2 D1 --").
So an A->D area fill costs **3 CCK/word** vs 2 for an A->D copy. The fill
carry datapath (`apply_fill` in `finish_source_word`) is not what adds the
cycle -- the FillIdle slot in the controller sequence is. Cross-emulator
validated: FS-UAE and vAmiga both time the `bltcon0=0x09F0` A->D fill at 3
CCK/word (timing-test rows 23/24/26). USEC-carrying fills reuse their real
C cycle and D-less fills match their copy timing (vAmiga's fill programs
change only masks 1/5/9/D). The idle fill phase advances on
CPU/Copper/idle arbitration slots but not through fixed DMA
(bitplane/sprite/disk/audio/refresh) slots. (An earlier experiment dropped
the extra cycle to improve one capture, but that made the blitter faster
than hardware and broke a separate blitter-heavy regression.) Test:
`descending_area_fill_costs_one_extra_idle_cycle_per_word`.

### ECS registers

BLTCON0L, BLTSIZV, and BLTSIZH are active only on an ECS Agnus. BLTSIZV
latches the 15-bit vertical size but does not start the blit; BLTSIZH
latches the 11-bit horizontal size and is the ECS start strobe; zero
decodes to the documented maxima (32768 lines, 2048 words). BLTCON1.DOFF
suppresses destination writes while still running the D channel timing,
advancing the D pointer, and updating BZERO from the generated D data.
Tests: `ecs_bltsizv_bltsizh_start_extended_blit`,
`ecs_bltcon1_doff_suppresses_destination_writes_but_advances_pointer`.

### Known residuals

Cross-emulator timing-test comparisons (corroborated in
`timing-test/README.md`) now put the D-only clear and the active-display
fill/copper phase within a few colour clocks of the FS-UAE/real reference
after the BLS pressure-counter distinction above:

- **D-only clear**: row 23 measures `0x2712` vs the reference `0x2714`.
- **A->D fill under 3-plane display**: row 26 measures `0x6203` vs the
  reference `0x61FF`.
- **Copper-vs-CPU phase**: row 27 measures `0x6426` vs the reference
  `0x6426`.
- **Line blits**: a 64-pixel line now measures `0x015A`, matching the
  vAmiga reference and leaving the calibrated normal/fill rows unchanged.
  The line program enters its first BUSIDLE/HOLD_A micro-instruction one
  colour clock earlier than the normal A/B/C/D pipeline setup; a two-slot
  reduction overshoots row 25 and shifts row 26, so the remaining FS-UAE
  row-25 difference is not another startup slot.

The previous row-23 D-only clear residual was caused by feeding BLS pressure
from normal-mode disabled-channel idle phases blocked by fixed DMA. Those
phases still stall as micro-cycles, but they no longer make the nice blitter
yield earlier to the CPU.

References: HRM [Blitter Hardware, chapter
6](https://erikarn.github.io/amiga/docs/Amiga_Hardware_Reference_Manual_3rd_edition.pdf).

## Interrupt-recognition latency

A 68000 does not enter an exception the moment INTREQ rises. Copperline
separates source recognition, instruction poll qualification and register
readback:

1. Each newly asserted source receives an independent recognition deadline
   (`DEFAULT_IRQ_LATENCY_CCK = 6`, `src/bus.rs`). The default is calibrated
   against the combined A500 board/CPU STOP path, rather than a direct
   measurement of Paula's IPL pins. Later sources cannot postpone an earlier
   deadline. CPU mask/acknowledge writes update the bookkeeping; unmasking a
   latched PORTS level retains its existing immediate behavior. A new
   Copper-written PORTS request is a source assertion, not an INTENA unmask,
   and receives a recognition deadline.
2. A running stock 68000 samples candidates at bus-access starts. Its ordinary
   qualified prefetch poll uses the deadline minus two CCK; a core-marked
   unqualified operand poll uses the deadline minus four CCK. The core holds
   the selected sample through subsequent accesses. Memory TST polls on its
   operand word (the low word for TST.L); DIVS can mark an internal poll before
   its final prefetch. Other CPU models and accelerated bus ratios retain
   their previous qualification. STOP uses the unchanged deadline directly
   and preserves its two-CCK polling grid. A cleared source is not delivered
   from a stale sample. `COPPERLINE_IRQ_LATENCY_CCK=0` selects the raw model.
3. INTREQR is the raw Paula latch, independent of the recognition deadline.
   A 68000 CPU read captures it during its granted transfer, before the bus
   cycle's trailing CCK. Reading after the tail incorrectly included Copper
   writes that occurred after the data transfer. Long accesses sample the
   INTREQR word in its own slot. Other custom registers retain their separately
   calibrated phases, and the shorter 020+ data-return path is unchanged.

The [focused One ROM measurements](a500-irq-focus-2026-10-15.md) match all
2,212 A500 observations after these corrections; the earlier 2,560-divider
and 896-STOP result packets also match hardware byte-for-byte. WinUAE Best
Compatibility agrees on the same measured fields. This closes the observed
poll/readback differences in those suites, not every instruction, source,
DMA or priority transition. The previous full six-CCK qualification at a
prefetch's start counted its trailing transfer twice, making ordinary
instruction recognition two CCK late. Globally shortening the deadline
fixed those instructions but broke the already accurate STOP path.

Copper-generated PORTS/EXTER requests use the same running-instruction poll
qualification as internal Paula requests. The request bit alone cannot identify
an external CIA edge: a Copper INTREQ write can set either bit. A serialized
origin mask preserves that distinction until acknowledgement. Pin-fed CIA and
unmeasured CPU-written external request paths keep their previous qualification.
Paired A500 STOP/running/readback controls and remaining CIA request-visibility
differences are recorded in the sibling probe repository at
`docs/irq-stage-investigation-2026-10-19.md`. These measurements do not justify
removing the six-clock default or claiming an isolated IPL-pin delay.

The six-CCK default is calibrated against `amiga-hardware-probes` captures from a
PAL A500 with 512 KB chip RAM, 512 KB slow RAM, ECS Agnus and OCS Denise.
With only Copper DMA enabled, a WAIT at line 64 followed by a MOVE that
sets SOFTINT wakes a STOPped 68000: the handler's first instruction records
VHPOSR 37 CCK after the WAIT target at all sixteen horizontal phases,
twice each. The previous five-CCK default recorded 36. This validates the
combined Copper/interrupt/CPU/readback path, not an isolated measurement of
the propagation delay at Paula's pins.

A subsequent RAM-resident ROM probe on the same A500 measured both 37 and
38 CCK with DMA disabled. A fixed WAIT at line 96, horizontal 64, and a
NOP sweep before STOP separated the CPU phase from the Copper target.
Across 256 trials, the parity of the logged entry beam predicted the
result exactly; inserting EXG (six CPU clocks) reversed the relationship.
The stock 68000 STOP path therefore retains a four-CPU-clock (two-CCK) polling
grid from STOP entry and recognizes a newly visible IPL at the next poll.
This phase is saved with the machine. The boundary-recognition setting remains
six CCK; changing that constant cannot reproduce both results.

On the 68000/68010 motherboard bus, interrupt acknowledge reads the low
byte of the ROM word at `$FFFFF0 + 2 * level`. Normal Kickstart's final
eight words contain vector numbers 24 through 31. A custom ROM must supply
these words; Copperline does not replace a missing word with an autovector
or suppress delivery because a RAM handler is zero. The freezer cartridge
retains its own acknowledge behavior. The accelerated CPU bus path retains
autovector delivery. This ROM-data behavior was checked on the One ROM A500
by changing the level-1 word to select the installed vector-30 handler.
The zero-footer measurement ROM now reproduces the physical machine's
illegal-instruction fault at `$003020`, rather than completing its trials.

### CIA register transfers and timer startup

A CIA access uses the 68000's synchronous VPA/E bus cycle, not an ordinary
four-clock DTACK cycle ([MC68000 User's Manual, appendix B](https://www.nxp.com/docs/en/reference-manual/MC68000UM.pdf)). The E period is
ten CPU clocks, six low and four high. `cpu_cia_access` synchronizes the
transfer to the E falling edge. The register transfer occurs before that
edge's CIA update; `cpu_cia_access_finish` applies the edge and then advances
the final six CPU clocks. Applying a START write after its transfer edge
would miss the first timer pipeline stage and postpone underflow by an
entire E-clock. The total access cost is unchanged.

Timers A and B propagate START through two count-enable stages on PHI2.
Clearing START reads back immediately, but counting continues for the two
edges while those stages drain. A STOP-only delay sweep measured two extra
decrements on both timers of both physical CIAs (320 trials). One-shot
underflow stops counting directly. START+LOAD, including a stopped one-shot
timer's high-byte auto-start, holds counting for three E-clock edges.
FORCE LOAD with START still set applies the same three-edge hold to an
already-running timer; it does not require a stopped-to-running transition.
A One ROM sweep on the physical A500 measured this on both timers of both
CIAs: 640 reload trials, five launch phases and sixteen read delays. Before
the correction every live sample was three counts too low; afterwards both
the live and fully stopped counts matched all 640 hardware samples exactly.
This also removes the three-E-clock bias in repeated `timing-test` stopwatch
rows, whose one-shot high-byte write starts the timer before CRA FORCE LOAD.
On a CPU VPA write the transfer edge feeds the first stage. The timer's START bit is readable
during startup; one-shot underflow still clears it. Startup advances on
PHI2 even for CNT/cascade modes, and underflow predictions include the
remaining stages so a stopped CPU cannot nap past an interrupt. The
LOAD holds and count-enable stages travel in snapshots. Older snapshots
reconstruct the stages from their running flag and startup delay; legacy
USS imports restore a steady running or stopped timer.

The clock-source and one-shot qualifiers also pass through two PHI2 stages.
Changing a running timer B from PHI2 to timer-A underflows retains two final
PHI2 decrements; changing back delays the first PHI2 decrement by two edges.
Clearing one-shot together with START does not cancel a one-shot underflow
already in those stages. The qualifier history travels in snapshots.

Timer-A underflows reach timer B's cascade input one E-clock later. The
pending final pulse is saved and included in STOP wakeup predictions, even
when timer A has already stopped. Batched updates retain that final pulse
and deliver all preceding ones; latch zero produces one underflow per
enabled input tick. One-shot batches normally stop at their first underflow;
the zero-latch high-byte auto-start exception is described below. A zero
START+LOAD also produces carry on the last LOAD stage, before the first
decrement, and can stop a one-shot there.

CIA batches step individual PHI2 edges while timer PB outputs, serial output,
an ICR-read race, or control pipelines need edge ordering. This preserves PB
toggle parity and one-edge pulse widths, and prevents elapsed batch time from
consuming the IRQ-pin delay of a serial completion on the final edge. Once
these effects settle, ordinary timer counting remains batched. Regression
tests compare complete states after identical clock sequences partitioned
into one batch or individual edges. This is a batching consistency correction,
not a new measurement of physical IRQ propagation or a latency recalibration.

Physical A500 captures from 19 October 2026 additionally constrain the
Timer-A PB6 and serial output behavior on both CIAs. Pulse mode idles low
and drives high for the underflow pulse, including when DDRB selects input.
START initializes the toggle latch high. Adjacent zero-latch underflows
merge into one asserted pulse and therefore produce only one toggle edge;
a gap rearms the next edge. High-byte one-shot auto-start also initializes
the toggle latch. With a zero latch it produces two adjacent Timer-A carries
before stopping, observable through timer B's cascade input.

Serial output advances one bit per two Timer-A underflows and asserts SP
after sixteen underflows for a clean byte. Serial input still shifts eight
external CNT edges. The output half-clock phase and auto-start carry are
saved, with defaults for older states. These measurements do not establish
serial buffering semantics, Timer-B zero-latch auto-start, electrical IRQ
latency, or CPU IPL sampling. Captures, provenance and comparison rules are
in the sibling `amiga-hardware-probes` repository's
`docs/cia-hardware-verification-2026-10-19.md`.

Synchronous CPU timer reads sample the reload data mux before the falling
edge commits the counter. At an eligible PHI2 reload slot, internal count
zero therefore reads as the latch value. This changes readout, not the
internal countdown or the continuous period of `latch + 1`. Counter
inspection and fully stopped reads still expose the actual stored count.

These details were isolated with One ROM on the real 512K-chip/512K-slow PAL
A500 on 13–14 October 2026: independent reference timers were frozen before
reading, live reads were bracketed by a reference STOP, and timer B counted
timer-A underflows. A dense 6240-trial live-read sweep fits the same
E-clock-calibrated read curve on hardware and Copperline, including 60
hardware reload-boundary samples at latch 15. The calibration offset belongs
to the probe's instruction sequence and is not an emulator timing setting.

The earlier sparse period probe's reference interval includes setup and
reads after the first target sample, so matching that whole interval does
not establish matching target-read phase. Use the bracketed reference and
large-latch controls for this comparison; raw trial indices and matching
NOP counts alone are insufficient.

ICR's IR bit is a latch: clearing an interrupt mask does not clear a
previously recorded enabled interrupt. Reading ICR acknowledges the latch.
Re-enabling a still-pending source arms the IRQ pin even when IR was already
set. The mask-clear captures constrain this readback behavior, rather than
the electrical pin's precise deassertion edge.

The hardware probe's CIA measurements include register programming, the
timer, interrupt dispatch and VHPOSR readback. Its launch instruction/E-clock
phase varies between trials and between boots, so whole-record equality is
not an accuracy metric. Compare phase-matched samples or elapsed-time
distributions, and exclude ambiguous VHPOSR reads at horizontal wrap.

## Blitter slowdown and fixed DMA

With BLTPRI clear, the slowdown counter follows consecutive denied CPU
access slots. A refresh or other fixed-DMA slot still counts while the
blitter is in a disabled-channel gap or destination pipeline bubble. A CPU
grant resets the counter; an idle blitter phase does not. Fixed DMA retains
its priority, and BLTPRI disables this CPU starvation yield.

One ROM measurements on the same A500 compared D-only, descending-fill and
line blits with a stopped CPU, normal polling and BLTPRI polling (540 trials).
For a 220-by-22-word D-only clear, normal polling measured 9923–9934 colour
clocks. Ignoring fixed-DMA misses during idle blitter phases produced
9876–9890; counting them produces 9925–9935. The stopped-CPU and BLTPRI
controls are unchanged. These ranges include launch phase, register access
and readback time; they are not a constant to add to a blit.

### Division bus phase and probe clock conversion

The MC68000 divider computes before its final instruction prefetch and IPL
sample. Both DIVU and DIVS therefore flush their internal clocks before topping
up the prefetch queue; operand reads still precede computation. MULU/MULS keep
their separate early-prefetch sequence. One ROM division sweeps vary operands,
addressing modes, bitplane count and loop length. A Copper interrupt during the
divide confirms that hardware takes the exception before the following marker
instruction; the previous early-prefetch sequence could execute that marker
first. WinUAE Best Compatibility agrees with the hardware ordering.

The earlier claim that 64 six-plane DIVU operations were 191 colour clocks
early was derived from a defective test clock. The horizontal VHPOSR byte
already counts colour clocks; multiplying it by two made the computed time
jump backwards at a scanline boundary. The corrected test uses
`227 * vertical + horizontal`. A NOP replaces the redundant doubling so the
probe's own instruction size and cycle cost do not change. Raw beam endpoints
and an independent CIA timer cross-check elapsed measurements. Launch phase
still matters under display DMA; an isolated table value must not be treated
as an operand-independent divider penalty.

The aligned sweep includes seven bitplane counts, eight operand/control
kernels, eight launch phases and 1/2/16/64 iterations. All 1,732 unambiguous
Copperline samples with the same actual launch beam as hardware have identical
elapsed colour clocks. WinUAE has 1,640 such pairs, also identical. In the
six-plane, 64-iteration kernels, normal DIVU measures 4763--4767 clocks,
worst-case DIVU measures 5102, and DIVS measures 5249--5257 on all three.
These totals include the shared loop and observation overhead.

The stopped-CPU event horizon also includes the pending BLIT assertion:
the engine can finish its final write while the interrupt countdown still has
two clocks left. Bounding idle advances only by engine completion would then
sleep past the source assertion even though running-CPU readback was correct.

The native BLIT source phase is modeled three CCK before its INTREQR readback
edge, rather than starting the full recognition interval after readback.
The synchronized 896-row A500/WinUAE IRQ probe isolates this from identical
Copper-generated BLIT/COPER/SOFT controls and RAM/ROM handler placement.
After correction the native NOP-phase cases match; EXG-phase cases remain
two CCK late. This is an inferred timing model and a documented remaining
scheduler/poll-phase question, not a direct capture of Paula's IPL pins.

The Copper polling row's fixed beam values overlap after sweeping polling
phase; they do not establish an additional fixed Copper interrupt delay.

## Beam-register readback

Live VHPOSR reads expose a pipelined beam position, not the exact internal
counter. Copperline reports the horizontal byte three colour clocks
(`VHPOSR_LOOKAHEAD_CCK`) ahead of the internal Agnus counter at the
CPU-visible register-read point, while the first two reported positions of a
new line still carry the previous vertical line number. vAmiga models the
same hardware quirk as a five-cycle lead at its peek point; Copperline's
smaller residual lead accounts for the chip-bus grant already advancing the
beam before `read_vhposr` samples it. The low byte is raw colour clocks, not
half clocks. This is visible in timing-test rows 19/20/22/27 and in
line-wrap polling loops that read VHPOSR immediately after INTREQR bits
become visible.

## Real-time pacing

Pacing never changes emulated behaviour -- it only decides how much
emulated time to advance per host frame and when to sleep. (The one
opt-in exception is the fastest-possible JIT speed described below.) Real mode
debits a per-frame instruction budget one of two ways, selected by
`[emulation] pacing_budget`:

- `cycles` (default): the budget is debited by the chip-bus (device) time
  each slice actually consumed. The cycle-exact core advances the chipset
  through every CPU cycle as it executes -- internal cycles, bus-cycle
  tails, chip-bus grants and contention waits -- so the slice's elapsed bus
  CCK is the true hardware cost (`real_slice_accounting` in
  `src/emulator.rs`). Because the m68k core's 68000 cycle totals are exact
  across its [SingleStepTests validation corpus](https://github.com/benletchford/m68k-rs/tree/m68k-v0.13.0#validation--testing),
  this matches a stock PAL 68000.
- `instructions`: a flat cycles-per-instruction quota
  (`COPPERLINE_REAL_CPU_CPI`, default 4.0), debited by retired
  instructions -- cheaper and pacing-robust, but runs the CPU faster than
  hardware for instruction mixes above the flat cost.

`COPPERLINE_REAL_PACING_BUDGET=cycles|instructions` overrides the config
for one run; the config overrides the built-in `cycles` default.
`COPPERLINE_REAL_PACING_PROFILE=1` emits a one-second pacing log (see
[](peripherals.md)). Accelerated CPUs scale the budget by
`cpu_clocks_per_cck`; fast RAM/ROM access costs are scaled with sub-CCK
carry accumulation so fractional costs are not lost.

Warp (`Emulator::set_paced(false)`) skips the sleep entirely. While the
window sees the operator holding a host key or a button on the guest's
mouse, and for a short grace period after (`WARP_INPUT_YIELD_GRACE`), it
engages `Emulator::set_warp_input_yield`, which re-anchors the pacing clock
and keeps the sleep in force underneath warp until released, so a physical
hold or the gap between two clicks lasts as long in emulated time as it did
in the hand. `paced()` keeps reporting warp as engaged throughout; only
the pacer's sleep decision (`paces_realtime`) changes. Scheduled and
control-protocol input never engages it.

`cycles` became the default once the core's 68000 cycle counts were made
accurate: under flat instruction pacing, or with the old static cycle
counts, a blitter-bound chip-RAM scene had the CPU over-issuing chip-bus
accesses and starving the line blitter. With
accurate per-instruction costs the CPU's chip-bus slot ratio settles at a
physically valid value naturally, which (together with the area-fill C-slot
fix) resolved that blanking regression.

(jit-fastest-pacing)=
### Fastest-possible JIT speed

`[cpu] speed = "fastest"` (JIT only) is the one setting where pacing does
change emulated behaviour, and it is opt-in and documented as not
reproducible. WinUAE's *Fastest possible* keeps the chipset in real time
and gives the CPU whatever host time each frame leaves; Copperline does
the same by moving the JIT's effective clock
([](#jit-fastest-speed)), driven by
`SpeedGovernor` in `src/emulator/fastest.rs`:

- After every real-time quantum `step_real_until` reports the host time it
  spent emulating (pacing sleep excluded), the host time since the previous
  quantum ended (the frontend's share of the main thread), the emulated time
  advanced, the instructions retired, and whether the JIT batch path ran.
  Time the window spends presenting a frame on the main thread (the RTG,
  inspector and screenshot paths, which can block on vsync) is reported
  through `Emulator::note_frontend_wait` and left out of the frontend
  share: it is a wait, not work, and frames the present worker takes never
  touch the main thread at all.
- Every 0.1 s of emulated *or* host time, whichever comes first, the
  governor computes the load: host time over emulated time, 1.0 when the
  host only just keeps up. Closing on host time as well bounds how long an
  overloaded clock survives in wall time.
- Above 0.93 it lowers the clock and below 0.80 it raises it, each time by
  the square root of `0.85 / load`: at most a 4x cut or a 1.25x rise per
  window. A full proportional step overshoots near saturation, where frame
  cost climbs faster than the clock, and made the clock hunt between two
  values in measurements; half the log-gain settles. It only raises the
  clock when the CPU is clock-bound: it retired at least a quarter of an
  instruction per billed clock. A CPU idling in STOP or
  starved by chip-bus waits gains nothing from a faster clock, and ramping
  it would only cause a hiccup when the workload changes. Between the two
  thresholds it holds, so measurement noise does not make it hunt. Host
  cost grows less than in proportion to the clock (the chipset's work per
  emulated second is fixed), so the steps approach the band from below.
- The clock stays between `clock_mhz` and 2000 MHz.
- A gap of more than 100 ms between quanta (a paused machine, a modal
  dialog, a debugger break) discards the window, as does any quantum
  stepped on the precise core (debug hooks armed, a run-to-PC request).
  `reanchor_realtime_clock` discards it too.
- A save-state load rebases the floor to the state's clock. A governor
  that has never moved the clock follows the new floor exactly, so an
  unpaced run that loads a state still matches `speed = "fixed"`; one
  that has adapted keeps its clock within the new bounds.
- Unpaced quanta (warp, run-ahead bursts, headless runs) discard the
  window and leave the clock where it is. A headless run therefore never
  leaves `clock_mhz` and is byte-identical to `speed = "fixed"`, unless
  `COPPERLINE_HEADLESS_PACED=1` paces it like the window to measure exactly
  this.

Measuring wall time rather than CPU time makes the controller back off
when other processes compete for the core. It also copes with hosts whose
scheduler parks a lightly loaded thread on efficiency cores at a low clock
(macOS does): the paced thread looks expensive at first, moves to a
performance core as the load rises, its cost per instruction falls, and
the governor raises the clock further.

### Thread scheduling priority

Pacing only works if the host actually runs the right thread at the right
moment. Two threads are latency-critical: the **pacer** (the main thread,
which advances the core and calls `thread::sleep` in
`Emulator::sleep_until_realtime_device_time`) and the **cpal audio callback**
(which drains the sample ring buffer the pacer keeps ~150 ms ahead of the
device clock). During live-audio startup and rebuffering, the pacer treats the
unfilled prebuffer as additional required lead; the large-stall self-heal
allows for that lead so it does not cancel the refill as if it were a host
pause. The `copperline-render` and `copperline-present` workers
([](architecture.md)) are throughput threads, not latency ones, and are
left at normal priority. When the host is busy, a scheduler that preempts
the pacer shows up as frame stutter, and one that preempts the audio
callback shows up as an audible underrun.

`[emulation] realtime_priority` (off by default; `COPPERLINE_REALTIME_PRIORITY`
overrides it for one run) asks the OS to schedule those two threads above
normal. It is best effort -- `src/priority.rs` logs what it did and never fails
the run -- and, like all pacing, it never changes emulated behaviour; it only
changes when host work is scheduled. The implementation is per-platform because
"real-time priority" is portable in neither API nor semantics:

- **macOS** -- the pacer thread joins the `USER_INTERACTIVE` QoS class
  (`pthread_set_qos_class_self_np`), the idiomatic unprivileged low-latency
  request. The audio callback is left untouched: Core Audio already runs it on
  a real-time thread, and pinning a QoS class onto it would only *demote* it.
- **Windows** -- both threads are raised to `THREAD_PRIORITY_HIGHEST` via the
  `thread-priority` crate; no privilege required.
- **Linux / other Unix** -- raising priority needs privilege (an `rtprio`
  rlimit, `CAP_SYS_NICE`, or root); without it the request is logged and
  declined, and the thread keeps normal scheduling.

The pacer sleeps between work chunks rather than spinning, so even the
strongest scheduling class it can land in still yields the CPU and cannot
starve the host -- which is why elevating it is safe to offer.

(cross-checking-against-hardware)=
## Cross-checking against hardware

`timing-test/` is a bootable disk that measures CPU and chip-bus operation
timings against the CIA E-clock and reports them on screen and over
serial. The same numbers can be collected from Copperline, vAmiga, FS-UAE,
and real Amigas; several timing fixes (IRQ latency, the area-fill C slot)
were validated this way. When changing the timing model, update the
corresponding reference doc and add a named regression test for the
hardware behaviour.

Behaviours with a natural on-screen form are also pinned by golden probe
renders. Each probe is a bootblock program in `timing-test/` (the `.asm`
source committed beside its assembled `.bin`) that takes over the machine
and draws the behaviour as a static display: bars, strips, or marker
positions. `tests/probe_golden.rs` wraps each `.bin` in a bootable ADF,
boots it on the bundled AROS ROM, and compares a screenshot pixel for pixel
with `timing-test/golden/<name>.png`; the probes named in this chapter
(`ddfprobe-ddfmiss`, `ddfprobe-sotb`, `hamprobe-select`,
`copprobe-jmpread`, ...) are among them. The suite runs only in release
builds. After an intentional change, re-bless the goldens and review the
render diff:

```sh
COPPERLINE_BLESS_GOLDEN=1 cargo test --release --test probe_golden
```

The two probes that display live E-clock-referenced counts (`timing-test`
itself and `bltprobe-pace`) also move whenever the bundled AROS ROM is
refreshed, because their phase depends on how long the ROM takes to boot.
`timing-test/README.md` ("CI golden renders") has the details, including
the vAmiga cross-checks behind individual probes.

### A500 review boundary regressions

The `a500-blit-boundaries` and `a500-display-boundaries` golden probes retain
raw startup, BBUSY/BZERO, descending destination-restore, collision-clear,
pointer-write and line-end-refresh samples. The display probe pairs single
and dual playfields. They supplement the versioned numerical hardware captures;
rendered emulator goldens are not hardware measurements.

The destination-restore measurements require classic CPU RAM writes to land
at each granted word transfer, before DMA runs in the bus cycle's free tail.
Committing the complete write after that tail reverses CPU/blitter ordering
despite apparently correct grants. The same transfer boundary is used for
byte, word and longword writes, including separate classic longword halves.

CIA mask writes do not acknowledge an already-latched interrupt: IR and the
external request remain asserted until an ICR read. One-shot START readback can
show STOP during the E-high transfer before falling-edge decrement/underflow
commit; this preview does not apply during the LOAD hold. Internal carry and
timer-A-to-timer-B cascade state are preserved. These constraints come from
paired measurements of both CIAs, with explicit acknowledge, masked-source,
zero/one/nonzero-latch and timing-gap controls in `amiga-hardware-probes`.
