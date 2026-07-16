# The timing model

Copperline's compatibility with cycle-sensitive software comes from one
idea applied consistently: **the chip bus is a single resource arbitrated
per colour clock (CCK), and everyone pays for their slots.** Reference
numbers come from real hardware via the `timing-test/` disk. The Copper
and blitter timing models are documented in full below; the 68000 prefetch
model is in [](cpu). Every rule here is backed by named regression tests in
the inline suites (`src/chipset/copper.rs`, `blitter.rs`, `src/bus.rs`).

## Chip-bus arbitration

Every colour clock of every scanline has an owner, decided in priority
order. The fixed-DMA levels (steps 1-5) are decided by
`fixed_dma_owner_at` (`src/bus/dma_slots.rs`); Copper, blitter, and CPU
contend for the slots it leaves free. (The function tests audio before
disk, which is equivalent because their fixed slots never overlap.)

1. **Memory refresh** -- fixed odd slots 1/3/5 plus the line-end refresh
   slot (E2, or E3 on NTSC long lines). Refresh only ever uses odd slots,
   which is why it never collides with the Copper's even-slot cadence.
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
DDFSTRT and DDFSTOP are comparator EDGES that set/clear flip-flops, not a
value range. A stop request drains through one final fetch unit (which
applies the modulos per plane), the hardwired window ($18/$D8, HARDDIS
relaxes the stop to $E0) gates starts and forces stops, and the flop state
carries across line boundaries. Missed comparators therefore produce the
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
paints nothing (vAmigaTS Agnus/DDF/DDF/oldhwstop3/4 A500 photos). The
per-line fetch table is rebuilt when DDFSTRT/DDFSTOP/BPLCON0/DMACON/DIW
writes land (DDF writes commit to the comparators four colour clocks after
the write slot; an old DDFSTOP still fires on its commit clock, an old
DDFSTRT does not - vAmiga's sequencer semantics, hardware-verified in
aggregate by the vAmigaTS Agnus/DDF/DDF/oldhwstop1-4 A500 photos). A
mid-row BPLCON0 change switches the fetch-unit slot layout from its commit
clock; word addressing is unit-based, so late-enabled planes keep their
word positions and earlier words stay zero.
Wide-FMODE (quantum > 1) fetches keep the memoized value-window plan: the
effective DDF window, fetch cadence, and per-plane fetch-order mask live in
a `BitplaneSlotPlan` keyed on the register inputs (`BitplaneSlotKey`,
`src/bus.rs`).
Wide-FMODE lo-res slots are packed into the first eight CCKs of each
16/32-CCK fetch unit; the rest of the unit remains available to later
arbitration priorities.

Slow RAM at `$C00000` is arbitrated through Agnus *like chip RAM*: a CPU
access to slow RAM contends with DMA even though the RAM is outside the
chip range. (Getting this wrong is observable: too-fast slow RAM can race a
guest DMACON clear against the vertical-blank interrupt.) Fast RAM
and ROM are external-bus accesses billed at the CPU clock without chip-bus
arbitration -- so an accelerated CPU speeds up exactly what a real
accelerator would.

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

DMACON's BLTPRI ("blitter nasty") is modelled as on hardware: with BLTPRI
set the blitter wins every slot it requests, and the request line stays
asserted through the blit's warm-up -- the startup ladder and, for
D-writing blits, the first word's cycles including the empty first-D
bubble -- so the CPU is fenced out of those holes too (the Copper and
fixed DMA still use them). Once the pipeline is primed, genuinely bus-free
micro-cycles (line-mode Bresenham cycles, fill's idle cycle,
disabled-channel gaps) release the request and stay CPU-available even
under BLTPRI, matching FS-UAE/vAmiga per-slot arbitration. Without
BLTPRI, the blitter yields a slot after the CPU has missed three
consecutive slots, matching the Minimig RTL. The counter and the
back-pressure rule are detailed under [](#cpu-contention) below.

### Sprite DMA control rewrites

Sprite DMA fetches POS/CTL at the fixed pair slots, then data words for
the active line. Standard hard vertical blank suppresses those fetches until
PAL line $19 or NTSC line $14, so frame-start SPRxPT writes made before that
boundary still name a memory descriptor rather than retargeting a descriptor
that could not yet have been fetched. Software can still rewrite
SPRxPOS/SPRxCTL while a descriptor is pending or before a later pair slot to
reposition an already active sprite on that scanline. Those writes update the
live horizontal and vertical comparators, but they do not restart the sprite
data stream: the
data pointer stays with the descriptor that armed the sprite, and active
row offsets remain relative to that descriptor. Copperline therefore keeps
a runtime-only data-origin VSTART alongside the live comparator VSTART,
preserving it across active POS/CTL rewrites while still using the
rewritten HSTART for the line. SPRxPT rewrites on a later beam line while a
descriptor is still pending retarget that descriptor's data stream; same-line
rewrites after the descriptor fetch restart from a memory descriptor on the
next sprite slot. Directly armed register sprites use the same runtime origin
marker when an after-slot SPRxPT write refreshes a data stream instead of a
memory descriptor. The runtime origin is skipped in save states to preserve
the fixed bincode layout; after load, retained Denise armed state and the
next after-slot SPRxPT low-word write reconstruct this case for subsequent
full frames.
Future save-state versioning should serialize it if mid-line sprite-DMA
resume accuracy is tightened. Tests:
`pending_sprite_control_rewrite_preserves_descriptor_data_origin`,
`active_sprite_control_rewrite_preserves_descriptor_data_origin`,
`pending_descriptor_sprite_pointer_write_retargets_data_stream`,
`after_slot_armed_sprite_pointer_write_seeds_dma_data_stream`.

Beam-timed SPRxPOS writes are replayed in Denise's horizontal-comparator
domain, seven colour clocks ahead of the normal register-output position.
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
one hires pixel relative to OCS/ECS per WinUAE; that sub-colour-clock offset
is not yet modelled.) AGA BPLCON4's sprite palette-base byte uses Lisa's
earlier sprite colour-lookup path at `(hpos - $36) * 4`
(`SPRITE_PALETTE_CONTROL_HPOS_FB0`), one lores pixel ahead of ordinary
COLORxx replay. Tests:
`copper_move_writes_visible_registers_on_second_dma_slot`,
`copper_move_spends_four_color_clocks_leaving_alternate_cycles_free`,
`color_register_writes_use_final_output_position`.

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
References: HRM [Coprocessor
Hardware](https://www.theflatnet.de/pub/cbm/amiga/AmigaDevDocs/hard_2.html).

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

Scheduled blits use explicit phases matching the hardware controller,
cross-checked cycle-for-cycle against vAmiga's slot-accurate blitter with
a two-sided slot-trace probe (`COPPERLINE_DIAG_BLT_SLOTS` in Copperline,
`VAMIGA_BLT_PROBE` hooks in a local vAmiga build):

- **Startup**: two internal extra slots (the BLTSIZE register-commit
  cycle plus the BLT_STRT arbitration cycles real Agnus spends before the
  micro-program begins) followed by the StartDelay/Init slots, so the
  first body cycle runs at poke+4, matching vAmiga's BLT_STRT1/BLT_STRT2
  timeline. The startup applies to normal and line blits alike.
- **Body**: the source cadence follows the enabled-channel speed table: A
  is always visited, B and C only when enabled, and D when D is enabled
  or no C next-word state exists. D output is delayed through the hold
  register: the first D phase of every program is the HRM "-" bubble and
  does not claim the chip bus -- including D-only clears, which write
  "-- D0 -- D1 | -- D2" on hardware. The first destination word is
  written on the next D slot and the final word in the F flush slot.
- **Termination**: every program ends with two terminal micro-cycles (the
  internal D-hold flush and the BLTDONE cycle -- the final D write for
  USED programs, internal otherwise). DMACONR's BBUSY drops with the
  sequencer's final body cycle, BEFORE those terminal cycles: polling
  loops see BBUSY clear two cycles before the last D word lands.
  INTREQ.BLIT rises one colour clock after the BLTDONE cycle's FIRST
  attempt (vAmiga `scheduleIrqRel(BLIT, 1)` runs before the bus
  allocation check), so a bus-blocked final D write raises the interrupt
  before the word lands; the Copper's blitter-finished gate opens when
  the BLTDONE cycle completes.

Normal-mode A/B barrel-shifter carry is cleared at the first word of a new
BLTSIZE, then carries from the last source word of one row into the first
source word of the next inside that blit; masks, modulos, and fill carry
still observe row boundaries. Line blits follow vAmiga's four line
micro-programs, indexed by the USEB/USEC pair: with USEB clear each pixel
is four cycles (L1-L4: L2 fetches C when USEC is set, L3 propagates, L4
stores), and with USEB set each pixel is six (an LB cycle fetches B --
adding only BLTBMOD to BLTBPT -- and a bare LBus cycle allocates the bus
without a transfer). With USEC set the line D cycle allocates the bus
even when SING suppresses the store (unlike copy mode, where a locked D
slot is bus-idle); with USEC clear no line cycle touches the bus, and the
terminal BLTDONE cycle of a USEB program is itself a bus-idle cycle.
A SING-suppressed dot only locks the store: the A shifter, minterm and
BZERO update still run on the full inputs. The Bresenham error
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
| `BLTBDAT` | Immediate, write-time shifter | The write runs the B barrel shifter with the BSH/DESC current at write time (unlike BLTADAT/BLTCDAT); USEB-off blits consume that latched hold word. The first B write after completion zeros the B old register; later writes shift through the latched B data. |
| `BLTSIZE` | Deferred restart | A second start strobe drains the current blit first, then starts the replacement from the post-completion pointer state. |
| `DMACON.DMAEN/BLTEN` | Immediate | Gate blitter bus grants immediately; clearing leaves BBUSY set and preserves the pending blit until re-enabled. |
| `DMACON.BLTPRI` | Immediate | Bus arbitration observes the priority bit directly. |

No covered mid-operation write is modelled as ignored; less common writes
are treated as deferred by draining first. Tests:
`busy_bltcon0_write_disables_remaining_d_output_without_draining_blit`,
`busy_bltsize_write_finishes_current_blit_then_starts_replacement`.

### Micro-cycle stall classes

Non-bus blitter cycles come in two hardware classes, mirrored from
vAmiga's micro-instructions and encoded as `BlitSlotClass`:

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
(the Minimig RTL `bls_cnt`). A busy "nice" blitter still holds the chip bus
on its access cycles -- the CPU gets no regular alternate slot. Each colour
clock the waiting CPU misses still increments the diagnostic miss count, but
the BLS pressure counter only advances when the pending blitter micro-cycle
is a real pressure source: a bus access, fill's explicit idle cycle, or
line-mode BUSIDLE. Ordinary normal-mode "-" cycles from disabled channels or
the empty D pipeline bubble stall through fixed DMA but do not make the
blitter yield earlier. After `BLITTER_SLOWDOWN_CPU_MISS_LIMIT` (3)
consecutive pressure misses the blitter yields one slot, matching the HRM
"one bus cycle in four" rule while also matching UAE's `blitter_nasty`
distinction between normal idle cycles and fill/line BUSIDLE. Idle blit
pipeline cycles never claim the bus and stay CPU-available -- with one
exception: with BLTPRI set, the blit's warm-up holes (the startup ladder
and, for D-writing blits, the first word's cycles including the empty
first-D bubble) fence the CPU, because the sequencer's bus request is held
asserted by its queued back-to-back first fetches. Software depends on the
warm-up fence: MFM-decode trackloaders (e.g. Jim Power's) save the word
below a decode blit's destination, write BLTSIZE, and restore it two
instructions later; granting the CPU the startup and first-D holes let the
restore (and its prefetches) land mid-blit and the blitter overwrite the
restored word, corrupting the last data word of every decoded sector. Once
the pipeline is primed, bus-free micro-cycles are CPU-available even under
BLTPRI -- line-heavy demo main loops (Rampage's vector parts) depend on
that CPU time, and a whole-blit fence overshoots the FS-UAE/vAmiga
references on timing-test row 26 by ~+70 cck. The counter resets when the
CPU gets the bus, when BLTPRI is set, or when blitter DMA cannot run.

A 68000 `TAS` read-modify-write is unsafe on chip RAM (the HRM warns
against it); the `m68k` backend exposes it as a byte read then a byte
write, so Copperline models `TAS` against chip RAM as two separately
Agnus-arbitrated accesses, each subject to the same back-pressure rule.
Tests: `blithog_clear_busy_blitter_yields_to_cpu_only_after_starvation`,
`bltpri_stalls_cpu_chip_access_through_blitter_access_cycles`.

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

References: HRM [Blitter
Hardware](https://www.theflatnet.de/pub/cbm/amiga/AmigaDevDocs/hard_6.html).

## Interrupt-recognition latency

A 68000 does not enter an exception the moment INTREQ rises. Two hardware
mechanisms sit between the two events, and Copperline models both:

1. **The Paula IPL pipe.** A change to the enabled-pending interrupt set
   reaches the CPU's IPL pins only after a few chip clocks of pipelining
   inside Paula (`DEFAULT_IRQ_LATENCY_CCK = 5`, `src/bus.rs`;
   `COPPERLINE_IRQ_LATENCY_CCK` overrides, `0` disables both mechanisms).
   The delay is attached to asynchronous Paula/CIA/blitter/Copper source
   assertions. A CPU write that merely changes INTENA/INTREQ masking or
   acknowledges a latch normally only updates the delayed-bit bookkeeping.
   PORTS is level-fed by CIA-A/Gayle-style INT2 sources and remains
   immediately visible when software unmasks an already-latched level; other
   newly exposed latched sources are treated as freshly-present CPU IPL
   inputs and still pass through the pipe. INTREQR reads are never delayed:
   the pipe sits between the level encoder and the pins, not on the register.
2. **Boundary sampling.** The CPU latches its IPL pins during bus cycles,
   and the take-interrupt decision at an instruction boundary uses the level
   latched at the *previous* instruction's last bus access
   (`CpuBus::sample_ipl`, `src/cpu.rs`). A level that rises during an
   instruction's trailing internal cycles is therefore recognised one
   instruction later, exactly as on silicon.

Together these reproduce the raise-to-handler-entry positions measured
against vAmiga (and the vAmigaTS real-A500 photos) across VERTB and
copper-poked INTREQ sources under a range of foreground loops; the residual
is 0..+7 CCK of per-instruction IPL poll-point detail the vendored core does
not model. An earlier revision used a blanket 65 CCK "recognition latency"
calibrated against timing-test row 19 with a mis-decoded VHPOSR (the low
byte is the CCK position, not CCK/2); that delivered every interrupt ~50 CCK
late and dominated the vAmigaTS cputim/irqtim divergence.

## Beam-register readback

Live VHPOSR reads expose a pipelined beam position, not the exact internal
counter. Copperline reports the horizontal byte three colour clocks ahead of
the internal Agnus counter at the CPU-visible register-read point, while the
first two reported positions of a new line still carry the previous vertical
line number. vAmiga models the same hardware quirk as a five-cycle lead at its
peek point; Copperline's smaller residual lead accounts for the chip-bus grant
already advancing the beam before `read_vhposr` samples it. The low byte is raw
colour clocks, not half clocks. This is visible in timing-test rows 19/20/22/27
and in line-wrap polling loops that read VHPOSR immediately after INTREQR bits
become visible.

## Real-time pacing

Pacing never changes emulated behaviour -- it only decides how much
emulated time to advance per host frame and when to sleep. Real mode
debits a per-frame instruction budget one of two ways, selected by
`[emulation] pacing_budget`:

- `cycles` (default): the budget is debited by the chip-bus (device) time
  each slice actually consumed. The cycle-exact core advances the chipset
  through every CPU cycle as it executes -- internal cycles, bus-cycle
  tails, chip-bus grants and contention waits -- so the slice's elapsed bus
  CCK is the true hardware cost (`real_slice_accounting` in
  `src/emulator.rs`). Because the vendored core's 68000 cycle totals are
  exact across the SingleStepTests corpus
  (`crates/m68k/CYCLE_TIMING_GAP.md`),
  this matches a stock PAL 68000.
- `instructions`: a flat cycles-per-instruction quota
  (`COPPERLINE_REAL_CPU_CPI`, default 4.0), debited by retired
  instructions -- cheaper and pacing-robust, but runs the CPU faster than
  hardware for instruction mixes above the flat cost.

`COPPERLINE_REAL_PACING_BUDGET=cycles|instructions` overrides the config
for one run; the config overrides the built-in `cycles` default.
`COPPERLINE_REAL_PACING_PROFILE=1` emits a one-second pacing log (see
[](peripherals)). Accelerated CPUs scale the budget by
`cpu_clocks_per_cck`; fast RAM/ROM access costs are scaled with sub-CCK
carry accumulation so fractional costs are not lost.

`cycles` became the default once the core's 68000 cycle counts were made
accurate: under flat instruction pacing, or with the old static cycle
counts, a blitter-bound chip-RAM scene had the CPU over-issuing chip-bus
accesses and starving the line blitter. With
accurate per-instruction costs the CPU's chip-bus slot ratio settles at a
physically valid value naturally, which (together with the area-fill C-slot
fix) resolved that blanking regression.

### Thread scheduling priority

Pacing only works if the host actually runs the right thread at the right
moment. Two threads are latency-critical: the **pacer** (the main thread,
which advances the core and calls `thread::sleep` in
`Emulator::sleep_until_realtime_device_time`) and the **cpal audio callback**
(which drains the sample ring buffer the pacer keeps ~150 ms ahead of the
device clock). During live-audio startup and rebuffering, the pacer treats the
unfilled prebuffer as additional required lead; the large-stall self-heal
allows for that lead so it does not cancel the refill as if it were a host
pause. The `copperline-render` worker ([](architecture)) is a
throughput thread, not a latency one, and is left at normal priority. When the
host is busy, a scheduler that preempts the pacer shows up as frame stutter,
and one that preempts the audio callback shows up as an audible underrun.

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

## Cross-checking against hardware

`timing-test/` is a bootable disk that measures CPU and chip-bus operation
timings against the CIA E-clock and reports them on screen and over
serial. The same numbers can be collected from Copperline, vAmiga, FS-UAE,
and real Amigas; several timing fixes (IRQ latency, the area-fill C slot)
were validated this way. When changing the timing model, update the
corresponding reference doc and add a named regression test for the
hardware behaviour.
