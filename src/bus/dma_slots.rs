// SPDX-License-Identifier: GPL-3.0-or-later

//! Chip-bus slot arbitration: the per-colour-clock quantum stepper and
//! the refresh/disk/audio/sprite/bitplane/Copper/blitter/CPU DMA slot
//! scheduling it arbitrates. Split out of `bus.rs` for size; this is
//! the same `Bus`, with full access to its private state.

use super::*;

/// One-shot env flag for the Copper write-landing trace
/// (`COPPERLINE_DIAG_COP_WRITES=1`): logs every Copper MOVE's landing color
/// clock (beam position, register, value) to stderr for cross-emulator
/// comparison against vAmiga's `VAMIGA_COP_PROBE` trace.
fn diag_cop_writes_on() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| crate::envcfg::flag("COPPERLINE_DIAG_COP_WRITES"))
}

impl Bus {
    pub(super) fn advance_one_chip_bus_quantum(
        &mut self,
        forced_owner: Option<ChipBusOwner>,
    ) -> (u32, AgnusTick) {
        self.advance_one_chip_bus_quantum_limited(forced_owner, self.next_chip_bus_quantum())
    }

    pub(super) fn advance_one_chip_bus_quantum_limited(
        &mut self,
        forced_owner: Option<ChipBusOwner>,
        max_cck: u32,
    ) -> (u32, AgnusTick) {
        self.advance_one_chip_bus_quantum_limited_inner(forced_owner, max_cck, false)
    }

    /// Shared quantum step. `copper_invariant_before_deadline` means either a
    /// steady WAIT comparator or a pending vertical-blank COP1LC restart has
    /// an exact deadline still strictly ahead of this quantum. The Copper
    /// state cannot change before that point, so the slot remains free without
    /// calling into its state machine; instruction tails and wake-up cycles
    /// never select this path.
    pub(super) fn advance_one_chip_bus_quantum_limited_inner(
        &mut self,
        forced_owner: Option<ChipBusOwner>,
        max_cck: u32,
        copper_invariant_before_deadline: bool,
    ) -> (u32, AgnusTick) {
        let cck = self.next_chip_bus_quantum().min(max_cck).max(1);
        self.flush_audio_before_audio_dma_slot();

        // Advance the Copper's two-cycle cadence on every Copper-eligible color
        // clock: it fetches on every other one and yields the idle half (and
        // any sleeping WAIT cycle) to the blitter/CPU. `allow_fetch` is false
        // when a forced owner (a granted CPU access) already holds this cycle.
        let hpos = self.agnus.hpos;
        let fixed_dma_owner = if matches!(forced_owner, Some(ChipBusOwner::Cpu)) {
            // A forced CPU owner is only used after `cpu_can_use_current_slot`
            // has already proved that fixed DMA does not own this color clock.
            // The Copper comparator still advances below with `allow_fetch=false`.
            None
        } else {
            self.fixed_dma_owner_at(self.agnus.vpos, hpos)
        };
        let copper_runs = cck >= CHIP_BUS_SLOT_CCK && self.copper_comparator_runs_at(hpos);
        // The line-end refresh access stalls the CPU and blitter but does
        // not block a Copper fetch: the refresh RGA strobe overlaps a
        // concurrent Copper transfer on the real chip (see
        // `refresh_slot_active_at`). The Copper therefore treats that one
        // fixed-DMA slot as free; if it declines the cycle, the slot stays
        // with refresh below.
        let copper_sees_free_slot = match fixed_dma_owner {
            None => true,
            Some(ChipBusOwner::Refresh) => Self::line_end_refresh_slot(hpos),
            Some(_) => false,
        };
        let eligible = copper_runs && copper_sees_free_slot;
        let copper_took_bus = eligible
            && !copper_invariant_before_deadline
            && self.step_copper_eligible_slot(forced_owner.is_none(), true);
        if !eligible && copper_runs && !copper_invariant_before_deadline {
            // A fixed DMA owner (bitplane/sprite/disk/audio/refresh) holds
            // this color clock, but the Copper's WAIT/SKIP comparator is
            // combinational and keeps running: only instruction fetches need
            // a bus slot. Without this, a wait whose only releasable color
            // clock sits under display fetch (e.g. hpos $DE inside the last
            // DDFSTOP=$D8 fetch unit of an overscan screen) never wakes: the
            // line-end blackout covers the following ccks and an 8-bit
            // vertical target like WAIT vp=$FF goes false again after the
            // line-255 rollover. With allow_fetch=false a Running Copper
            // cannot fetch here, so the slot is never taken from its owner;
            // copper_cycle_free=false also keeps the post-WAIT wake-up off
            // this owned color clock.
            let _ = self.step_copper_eligible_slot(false, false);
        }

        let mut owner = match forced_owner {
            Some(owner) => owner,
            None if copper_took_bus => ChipBusOwner::Copper,
            // A Copper-declined line-end refresh slot stays with refresh:
            // only the Copper rides the pipelined RGA cycle, never the
            // blitter or CPU.
            None if eligible && fixed_dma_owner.is_some() => {
                self.scheduled_dma_owner_after_fixed(false, fixed_dma_owner)
            }
            None if eligible => self.free_chip_bus_slot_owner(),
            None => self.scheduled_dma_owner_after_fixed(false, fixed_dma_owner),
        };
        if self.cpu_posted_write_debt > 0
            && forced_owner.is_none()
            && matches!(owner, ChipBusOwner::Idle)
            && cck >= CHIP_BUS_SLOT_CCK
            && self.emulated_cck >= self.cpu_chip_port_free_at
        {
            // A posted CPU chip write retires into this otherwise-idle slot.
            // The port turns around in 2 colour clocks, so drains (and any
            // following CPU chip access) pace to every other colour clock;
            // see `cpu_posted_write_debt`.
            owner = ChipBusOwner::Cpu;
            self.cpu_posted_write_debt -= 1;
            self.cpu_chip_port_free_at = self.emulated_cck + 2;
            self.note_cpu_granted_chip_bus_cycle();
        }
        if diag_blt_slots() && self.blitter.busy {
            eprintln!(
                "BLTP {} {} {} OWNER {owner:?} pending={} needs_bus={}",
                self.emulated_frames,
                self.agnus.vpos,
                self.agnus.hpos,
                self.blitter.current_slot_label(),
                self.blitter.current_slot_needs_bus()
            );
        }
        self.last_chip_bus_owner = owner;
        if self.chip_bus_observers_on {
            self.observe_chip_bus_quantum(owner, cck, hpos);
        }
        // The Copper was already stepped above (or is held without fetching at
        // the end-of-line lockout); only drive the other owners here.
        if !matches!(owner, ChipBusOwner::Copper) {
            self.process_chip_bus_owner(owner);
        }
        // A busy blitter's non-bus pipeline cycles leave the chip bus free,
        // but they still elapse in real time, per their arbitration class
        // (see BlitSlotClass):
        //
        // - Internal cycles (register commit, micro-program begin, terminal
        //   flush/D-less BLTDONE) advance on EVERY colour clock, even under
        //   fixed DMA (vAmiga NOTHING cycles have no bus check).
        // - Bus-free micro-cycles (the D pipeline bubble, fill's extra idle
        //   cycle, the BLT_STRT startup cycles, line Bresenham cycles)
        //   advance only on colour clocks the blitter could have won: they
        //   stall while the Copper or fixed DMA owns the clock (this is
        //   what makes display DMA slow area fills) and on the clock a
        //   starved CPU is granted (the BLS line blocks busIsFree).
        if !matches!(owner, ChipBusOwner::Blitter)
            && self.blitter.busy
            && self.blitter_dma_enabled()
        {
            let ticks = match self.blitter.current_slot_class() {
                crate::chipset::blitter::BlitSlotClass::Bus => false,
                crate::chipset::blitter::BlitSlotClass::Internal => true,
                crate::chipset::blitter::BlitSlotClass::BusFree => match owner {
                    ChipBusOwner::Idle => true,
                    ChipBusOwner::Cpu => {
                        // A starvation grant is the cck where BLS denies the
                        // blitter; an ordinary CPU access on a free clock
                        // does not stall the bus-free cycle.
                        !(matches!(forced_owner, Some(ChipBusOwner::Cpu))
                            && self.blitter_yields_to_waiting_cpu())
                    }
                    _ => false,
                },
            };
            if ticks {
                if diag_blt_slots() {
                    eprintln!(
                        "BLTP {} {} {} TICK {} bus=0 owner={owner:?}",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        self.blitter.current_slot_label()
                    );
                }
                if self.blitter.tick_scheduled_slot(&mut self.mem.chip_ram) {
                    self.latch_blitter_completion("idle_pipeline");
                }
                self.note_blitter_slot_ticked();
            }
        }
        let tick = self.advance_beam(cck);
        self.audio_pending_cck = self.audio_pending_cck.saturating_add(cck);
        if tick.new_lines > 0 {
            // Line end: sample the audio state machines' DMA requests into
            // the Agnus-side latches (real Paula transfers them once per
            // line; the fixed slots service them on the following line).
            self.flush_audio();
            for _ in 0..tick.new_lines {
                self.paula.transfer_audio_dma_requests();
            }
        }
        (cck, tick)
    }

    pub(super) fn refresh_chip_bus_observers(&mut self) {
        self.chip_bus_observers_on = self.bus_accounting.enabled
            || self.dbg_slotmap_on
            || self.frame_analyzer_enabled
            || self.wave_on;
    }

    /// Observer fan-out is outlined from the ordinary arbitration path. It is
    /// deliberately cold relative to emulation: even an interactive debugger
    /// spends most of its lifetime with no bus trace or waveform armed.
    #[cold]
    fn observe_chip_bus_quantum(&mut self, owner: ChipBusOwner, cck: u32, hpos: u32) {
        if self.bus_accounting.enabled {
            self.bus_accounting
                .record_cck(owner, cck, self.blitter.busy);
            if matches!(owner, ChipBusOwner::Bitplane) {
                let v = self.agnus.vpos as usize;
                if v < self.dbg_bpl_cck.len() {
                    self.dbg_bpl_cck[v] += cck;
                }
            }
        }
        if self.dbg_slotmap_on {
            let v = self.agnus.vpos as usize;
            let h = self.agnus.hpos as usize;
            if self.dbg_slotmap.is_empty() {
                self.dbg_slotmap = vec![vec![b'.'; 256]; 320];
            }
            if v < self.dbg_slotmap.len() {
                let code = chip_bus_owner_code(owner);
                let row = &mut self.dbg_slotmap[v];
                let end = (h + cck as usize).min(row.len());
                for slot in row.iter_mut().take(end).skip(h) {
                    *slot = code;
                }
            }
        }
        if self.frame_analyzer_enabled {
            self.current_frame_bus_trace.record(
                self.agnus.vpos,
                hpos,
                cck,
                owner,
                self.blitter.busy,
            );
        }
        if self.wave_on {
            self.wave_tap_quantum(owner);
        }
    }

    /// Step the Copper through one eligible color clock and apply any register
    /// write it produced. Returns whether the Copper used the bus this cycle.
    /// `copper_cycle_free` mirrors `Copper::step_eligible_slot`: false when
    /// fixed DMA owns this color clock (comparator-only advance).
    pub(super) fn step_copper_eligible_slot(
        &mut self,
        allow_fetch: bool,
        copper_cycle_free: bool,
    ) -> bool {
        let cop1lc = self.agnus.cop1lc;
        let cop2lc = self.agnus.cop2lc;
        let vpos = self.agnus.vpos;
        let hpos = self.agnus.hpos;
        let blitter_busy = self.blitter.busy;
        let line_cck = self.agnus.current_line_cck();
        // The Copper's PC before the slot, for attribution below.
        let fetch_pc = self.mem_watches_armed().then(|| self.copper.pc());
        let mut copper = std::mem::take(&mut self.copper);
        let action = copper.step_eligible_slot(
            &self.mem.chip_ram,
            vpos,
            hpos,
            blitter_busy,
            cop1lc,
            cop2lc,
            allow_fetch,
            line_cck,
            copper_cycle_free,
        );
        self.copper = copper;
        // Attributed only when the Copper actually took the slot. A
        // WAITing or stopped Copper leaves its PC pointing at the next
        // instruction and touches nothing, so noting the read before the
        // step reported a fetch on every eligible colour clock of the
        // wait -- which re-latched a watch hit as fast as the debugger
        // could clear it.
        if let Some(pc) = fetch_pc {
            if !matches!(action, CopperSlotAction::Idle) {
                self.note_dma_read(crate::debugger::WatchSource::Copper, pc, 4);
            }
        }
        if !self.ui_copper_breaks.is_empty() {
            self.check_ui_copper_breaks();
        }
        match action {
            CopperSlotAction::Idle => false,
            CopperSlotAction::BusUsed => true,
            CopperSlotAction::Move { register, value } => {
                if diag_cop_writes_on() {
                    eprintln!(
                        "COPPROBE MOVE   v={:03x} h={:02x} reg={:03x} val={:04x}",
                        vpos, hpos, register, value
                    );
                }
                if self.copper_can_write_custom(register) {
                    let _ = self.write_custom_word_from(register, value, BeamWriteSource::Copper);
                } else {
                    self.copper.stop();
                }
                true
            }
            CopperSlotAction::SkippedMove { register } => {
                // The SKIP suppresses the write, not the illegal-register
                // decode: a forbidden MOVE stops the Copper even when skipped.
                if !self.copper_can_write_custom(register) {
                    self.copper.stop();
                }
                true
            }
        }
    }

    /// Owner of a Copper-eligible free color clock that the Copper did not take
    /// (its idle half, a sleeping WAIT, or a stopped Copper): the blitter if it
    /// is running and its current pipeline cycle accesses the bus, otherwise
    /// idle/CPU.
    pub(super) fn free_chip_bus_slot_owner(&self) -> ChipBusOwner {
        if self.blitter.busy && self.blitter_dma_enabled() && self.blitter.current_slot_needs_bus()
        {
            ChipBusOwner::Blitter
        } else {
            ChipBusOwner::Idle
        }
    }

    pub(super) fn advance_beam(&mut self, cck: u32) -> AgnusTick {
        let old_vpos = self.agnus.vpos;
        let old_hpos = self.agnus.hpos;
        let old_frame_lines = self.agnus.current_frame_lines();
        let old_emulated_cck = self.emulated_cck;
        self.emulated_cck = self.emulated_cck.saturating_add(cck as u64);
        self.coper_cpu_irq_delay_cck = self.coper_cpu_irq_delay_cck.saturating_sub(cck);
        if let Some(delay) = self.blit_irq_delay_cck {
            let delay = delay.saturating_sub(cck);
            if delay == 0 {
                self.blit_irq_delay_cck = None;
                self.raise_blit_irq("scheduled");
            } else {
                self.blit_irq_delay_cck = Some(delay);
            }
        }
        if self.irq_latency_cck != 0 {
            self.irq_latency_cck = self.irq_latency_cck.saturating_sub(cck);
            if self.irq_latency_cck == 0 {
                self.irq_latency_mask = 0;
            }
        }
        let tick = self.agnus.advance_by_cck(cck);
        if !self.ui_beam_traps.is_empty() {
            self.check_ui_beam_traps((old_vpos, old_hpos), old_frame_lines, tick.new_frames);
        }
        if self.wave_on {
            self.wave_note_beam((old_vpos, old_hpos), old_frame_lines, tick.new_frames);
        }
        if tick.new_frames == 0 && tick.new_lines == 0 {
            self.capture_sprite_dma_words_if_due(
                old_vpos,
                old_hpos,
                self.agnus.hpos,
                old_emulated_cck,
            );
            self.capture_bitplane_dma_words_if_due(
                old_vpos,
                old_hpos,
                self.agnus.hpos,
                old_emulated_cck,
            );
        }
        if tick.new_lines != 0 || tick.new_frames != 0 {
            self.bitplane_ddfstart_miss = None;
            self.ocs_same_line_diw_start_blocked_vpos = None;
            // Carry the DDF sequencer flops into the new line. Quanta are at
            // most a few colour clocks, so exactly one line boundary can be
            // crossed per advance.
            self.ddf_seq_on_line_rollover(old_vpos);
            if tick.new_frames == 0 {
                // The vertical display flop's comparators fire at the new
                // line's start; a frame wrap runs them for line zero in
                // begin_new_beam_frame instead (the latch itself carries
                // across the wrap).
                self.reevaluate_diw_vertical_flop();
            }
        }
        let display_start = self.display_start_vpos_for_current_control();
        if tick.new_frames == 0 && old_vpos < display_start && self.agnus.vpos >= display_start {
            self.capture_current_frame_display_start();
        }
        for _ in 0..tick.new_frames {
            self.emulated_frames = self.emulated_frames.saturating_add(1);
            self.begin_new_beam_frame();
        }
        self.start_pending_copper_frame_if_due();
        tick
    }

    pub(super) fn process_chip_bus_owner(&mut self, owner: ChipBusOwner) {
        match owner {
            // The Copper is stepped directly in advance_one_chip_bus_quantum_limited
            // via step_copper_eligible_slot (its cadence needs per-color-clock
            // gap accounting), so it never reaches here.
            ChipBusOwner::Blitter => {
                if diag_blt_slots() {
                    eprintln!(
                        "BLTP {} {} {} TICK {} bus=1",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        self.blitter.current_slot_label()
                    );
                }
                if self.blitter.tick_scheduled_slot(&mut self.mem.chip_ram) {
                    self.latch_blitter_completion("bus_slot");
                }
                self.note_blitter_slot_ticked();
            }
            ChipBusOwner::Audio => self.step_audio_dma_slot(),
            ChipBusOwner::Copper
            | ChipBusOwner::Refresh
            | ChipBusOwner::Bitplane
            | ChipBusOwner::Sprite
            | ChipBusOwner::Disk
            | ChipBusOwner::Cpu
            | ChipBusOwner::Idle => {}
        }
    }

    pub(super) fn step_audio_dma_slot(&mut self) {
        self.flush_audio();
        let Some(channel) = Self::audio_dma_channel_at(self.agnus.hpos) else {
            return;
        };
        let Some(request) = self.paula.audio_dma_request(channel) else {
            return;
        };
        if self.mem_watches_armed() {
            self.note_dma_read(
                crate::debugger::WatchSource::Audio(channel as u8),
                request.address,
                2,
            );
        }
        let word = self.read_chip_word_for_audio_dma(request.address);
        self.data_bus = word;
        let irq = self.paula.grant_audio_dma(channel, word, self.agnus.dmacon);
        self.paula.latch_interrupt_sources(irq);
    }

    pub(super) fn copper_dma_enabled(&self) -> bool {
        self.agnus.dmacon & (DMACON_DMAEN | DMACON_COPEN) == (DMACON_DMAEN | DMACON_COPEN)
    }

    pub(super) fn copper_can_write_custom(&self, off: u16) -> bool {
        let off = off & 0x01FE;
        if off <= 0x03E {
            return !matches!(self.agnus.revision(), AgnusRevision::Ocs)
                && self.agnus.copper_danger_enabled();
        }
        // COPJMP1/2 are handled as Copper control-flow strobes above.
        if (0x040..=0x07E).contains(&off) {
            return self.agnus.copper_danger_enabled();
        }
        true
    }

    pub(super) fn blitter_dma_enabled(&self) -> bool {
        self.agnus.dmacon & (DMACON_DMAEN | DMACON_BLTEN) == (DMACON_DMAEN | DMACON_BLTEN)
    }

    pub(super) fn blitter_slowdown_counter_enabled(&self) -> bool {
        self.blitter.busy && self.blitter_dma_enabled() && self.agnus.dmacon & DMACON_BLTPRI == 0
    }

    pub(super) fn blitter_yields_to_waiting_cpu(&self) -> bool {
        self.blitter_slowdown_counter_enabled()
            && self.blitter_slowdown_cpu_misses >= exp_miss_limit()
    }

    pub(super) fn cpu_can_use_current_slot(&self) -> bool {
        matches!(
            self.scheduled_dma_owner(true),
            ChipBusOwner::Cpu | ChipBusOwner::Idle
        )
    }

    pub(super) fn scheduled_dma_owner(&self, for_cpu: bool) -> ChipBusOwner {
        self.scheduled_dma_owner_after_fixed(
            for_cpu,
            self.fixed_dma_owner_at(self.agnus.vpos, self.agnus.hpos),
        )
    }

    pub(super) fn scheduled_dma_owner_after_fixed(
        &self,
        for_cpu: bool,
        fixed_owner: Option<ChipBusOwner>,
    ) -> ChipBusOwner {
        if let Some(owner) = fixed_owner {
            return owner;
        }
        if self.agnus.dmacon & DMACON_DMAEN == 0 {
            return ChipBusOwner::Idle;
        }
        // The Copper claims the slot only on its access-parity color clock; on
        // the odd (idle-half) color clocks it yields to the blitter/CPU, which
        // is how the OCS Copper's 4-color-clock MOVE leaves alternate cycles
        // free. The cadence is locked to the beam, so a dense MOVE list lands at
        // the same hpos on every line.
        if self.copper_ready_for_slot() && Copper::hpos_is_access_cycle(self.agnus.hpos) {
            return ChipBusOwner::Copper;
        }
        if self.blitter.busy && self.blitter_dma_enabled() {
            // With BLTPRI set, BLS fences the CPU (not the Copper or fixed
            // DMA) during the blit's warm-up: the startup ladder and, for
            // D-writing blits, the first word's cycles including the empty
            // first-D bubble, while the sequencer's bus request is held
            // asserted by the queued back-to-back first fetches. Regression
            // example: the Jim Power trackloader saves the word below a
            // descending MFM-decode blit's destination, writes BLTSIZE, and
            // restores the word two instructions later; the fence keeps the
            // restore's prefetches out of the startup holes so the CPU write
            // lands only after the blit completes. Once the pipeline is
            // primed the request line drops on genuine bus-free micro-cycles
            // (line-mode Bresenham, fill idle, disabled-channel gaps) and
            // the CPU uses them even under BLTPRI -- line-heavy demo loops
            // (Rampage's vector parts) rely on that CPU time, and FS-UAE and
            // vAmiga agree (timing-test row 26: 25095/25097; a whole-blit
            // fence overshoots to 25161).
            if for_cpu
                && self.agnus.dmacon & DMACON_BLTPRI != 0
                && self.blitter.bltpri_warmup_fences_cpu()
            {
                return ChipBusOwner::Blitter;
            }
            // Idle blit pipeline cycles (the "-" slots in the HRM cycle diagrams,
            // e.g. the first empty D phase after source fetches or a line blit's
            // internal Bresenham cycles) never claim the bus: per the HRM they
            // are available to the other DMA channels or the 68000, and MiniMig
            // only asserts the blitter's dma_req on channel-access states. The
            // pipeline still advances through them -- see
            // advance_one_chip_bus_quantum_limited.
            if !self.blitter.current_slot_needs_bus() {
                return ChipBusOwner::Idle;
            }
            // With BLTPRI=0 the blitter is "nice" but still holds the chip bus:
            // it yields to the CPU only once the CPU has been starved for
            // BLITTER_SLOWDOWN_CPU_MISS_LIMIT cycles, not on every even slot-pair.
            // Granting the CPU a regular alternate slot here used to split the
            // bus ~1:1, but real OCS gives a busy blitter ~2:1 over a BLITWAIT-ing
            // CPU (cross-emulator DMA accounting on a blitter-heavy frame:
            // blitter 34892, CPU 17882). The old even/odd grant starved the
            // blitter so big fills overran the frame and flickered.
            if for_cpu && self.blitter_yields_to_waiting_cpu() {
                return ChipBusOwner::Idle;
            }
            return ChipBusOwner::Blitter;
        }
        ChipBusOwner::Idle
    }

    pub(super) fn fixed_dma_owner_at(&self, vpos: u32, hpos: u32) -> Option<ChipBusOwner> {
        if Self::refresh_slot_active_at(hpos) {
            return Some(ChipBusOwner::Refresh);
        }
        // Audio requests latched in Agnus are serviced even while the
        // DMACON bits -- the master enable included -- are off (the DAS
        // slot table keeps the audio slots in every DMACON variant).
        if self.audio_slot_active_at(hpos) {
            return Some(ChipBusOwner::Audio);
        }
        if self.agnus.dmacon & DMACON_DMAEN == 0 {
            return self
                .bitplane_slot_active_at(vpos, hpos)
                .then_some(ChipBusOwner::Bitplane);
        }
        if self.disk_slot_active_at(hpos) {
            return Some(ChipBusOwner::Disk);
        }
        if self.sprite_slot_active_at(hpos) {
            return Some(ChipBusOwner::Sprite);
        }
        if self.bitplane_slot_active_at(vpos, hpos) {
            return Some(ChipBusOwner::Bitplane);
        }
        None
    }

    /// Predict the color clocks until the pending blit completes by walking its
    /// remaining slot access pattern against the beam. Eligible-consuming slots
    /// (mask bit set: bus accesses AND bus-free micro-cycles) consume the next
    /// color clock the blitter can win (not fixed DMA, not Copper); internal
    /// cycles (mask bit clear) consume exactly one color clock unconditionally,
    /// matching the live arbitration where they elapse regardless of bus
    /// ownership.
    pub(super) fn cck_until_blitter_completes(
        &self,
        access_mask: u64,
        slot_count: u32,
    ) -> Option<u32> {
        if slot_count == 0 || slot_count > BLITTER_DEADLINE_SLOT_SCAN_LIMIT {
            return None;
        }

        let mut copper = self.copper.clone();
        let mut slot_idx = 0u32;
        let mut elapsed = 0u32;
        let mut hpos = self.agnus.hpos;
        let mut vpos = self.agnus.vpos;
        let mut lol = self.agnus.lol;
        let mut pending_copper_frame_start = self.pending_copper_frame_start;
        let frame_lines = self.agnus.current_frame_lines();
        let max_scan_cck = frame_lines.saturating_mul(NTSC_LONG_COLORCLOCKS_PER_LINE);

        while elapsed < max_scan_cck {
            if let Some(cop1lc) = pending_copper_frame_start
                .filter(|_| vpos >= copper_frame_start_vpos(self.agnus.video_standard()))
            {
                copper.frame_start(cop1lc);
                pending_copper_frame_start = None;
            }
            let line_cck = self.agnus.line_cck_for(lol);
            let quantum = next_chip_bus_quantum_at(hpos, line_cck);

            // Mirror the live path's per-color-clock Copper cadence on the
            // clone (stepped on every non-fixed-DMA color clock) so the
            // blitter only claims the color clocks the Copper leaves free
            // (its idle halves, sleeping WAITs, gaps). The shared
            // step_eligible_slot keeps prediction and execution from
            // drifting apart.
            let fixed_owner = self.fixed_dma_owner_at(vpos, hpos);
            let slot_grantable = quantum >= CHIP_BUS_SLOT_CCK && fixed_owner.is_none();
            // The live arbiter's line-end refresh carve-out, mirrored: the
            // Copper fetches through the E2/E3 refresh access, so the clone
            // must fetch there too or it falls a word behind the live
            // Copper. The slot stays non-grantable, so the blitter still
            // never consumes it.
            let copper_may_fetch = slot_grantable
                || (quantum >= CHIP_BUS_SLOT_CCK
                    && matches!(fixed_owner, Some(ChipBusOwner::Refresh))
                    && Self::line_end_refresh_slot(hpos));
            let copper_blocks = if !copper_may_fetch {
                // Fixed DMA owns this color clock, but the Copper's WAIT/SKIP
                // comparator keeps running (mirrors the live path's
                // comparator-only advance with allow_fetch=false).
                if quantum >= CHIP_BUS_SLOT_CCK
                    && pending_copper_frame_start.is_none()
                    && self.copper_dma_enabled()
                    && !self.copper_bus_lockout_active_at(hpos)
                {
                    let _ = copper.step_eligible_slot(
                        &self.mem.chip_ram,
                        vpos,
                        hpos,
                        self.blitter.busy,
                        self.agnus.cop1lc,
                        self.agnus.cop2lc,
                        false,
                        line_cck,
                        false,
                    );
                }
                false
            } else if pending_copper_frame_start.is_some() {
                true
            } else if !self.copper_dma_enabled() {
                false
            } else if self.copper_bus_lockout_active_at(hpos) {
                copper.is_running()
            } else {
                !matches!(
                    copper.step_eligible_slot(
                        &self.mem.chip_ram,
                        vpos,
                        hpos,
                        self.blitter.busy,
                        self.agnus.cop1lc,
                        self.agnus.cop2lc,
                        true,
                        line_cck,
                        true,
                    ),
                    CopperSlotAction::Idle
                )
            };

            let slot_needs_bus = access_mask & (1u64 << slot_idx) != 0;
            let slot_consumed = if slot_needs_bus {
                // Bus accesses and bus-free micro-cycles both need a colour
                // clock the blitter could have won.
                slot_grantable && !copper_blocks
            } else {
                // Internal cycle: elapses unconditionally.
                true
            };
            if slot_consumed {
                slot_idx += 1;
                if slot_idx == slot_count {
                    return Some(elapsed.saturating_add(quantum).max(1));
                }
            }

            elapsed = elapsed.saturating_add(quantum);
            hpos = hpos.saturating_add(quantum);
            if hpos >= line_cck {
                hpos = 0;
                vpos = vpos.saturating_add(1);
                if self.agnus.long_line_toggles() {
                    lol = !lol;
                }
                if vpos >= frame_lines {
                    vpos = 0;
                }
            }
        }

        None
    }

    pub(super) fn refresh_slot_active_at(hpos: u32) -> bool {
        // The OCS Agnus does 4 memory-refresh cycles per line, on ODD color
        // clocks in the fixed-DMA row (HRM DMA time-slot chart:
        // refresh/disk/audio/sprite all sit on the alternate slots). The
        // parity matters: the Copper's bus fetches use the EVEN color clocks
        // (WinUAE COPPER_CYCLE_POLARITY), so on real hardware refresh NEVER
        // blocks a Copper fetch. Putting refresh on even slots (a misreading
        // of MiniMig's 2x-hpos numbering) delayed Copper MOVE streams at the
        // start of every line by ~8 cck, which broke demos that rely on a
        // post-WAIT register burst completing before DDFSTRT; if a BPLCON0
        // plane-count switch lands after the line's fetches begin, the planes
        // are misaligned.
        //
        // Positions 1/3/5 plus the line-end slot mirror the Agnus DAS table:
        // refresh takes the first event and marks 1/3/5 plus EOL (vAmiga:
        // E2 on normal lines, E3 on NTSC long lines). The following odd slots
        // are disk (7/9/B), audio (D/F/11/13), then sprites (15...33).
        //
        // The line-end slot has one carve-out, applied in the arbiter (see
        // `line_end_refresh_slot`): it stalls the CPU and blitter but does
        // NOT block a Copper fetch. E2 sits on the Copper's even-cck fetch
        // grid, and stealing it starves a Copper stream that saturates the
        // line by one fetch per line: Nexus 7's plasma-zoom text reloads
        // 128+3 palette entries per 3-line band between two beam WAITs, a
        // chain that fits on real hardware but slips ~6 cck per band when
        // E2 is blocked, drifting the demo's BPLCON4 sprite-bank flip into
        // the sprite window and painting plasma streaks through the text.
        // On the real chip the line-end refresh RGA strobe overlaps a
        // concurrent Copper transfer (RGA pipelining); the CPU-visible
        // stall stays, which the real-A1200 timing-test column was
        // calibrated against.
        matches!(hpos, 0x001 | 0x003 | 0x005 | 0x0E2 | 0x0E3)
    }

    /// The line-end refresh access (E2 short lines, E3 long ones): the one
    /// refresh slot whose RGA strobe overlaps a concurrent Copper fetch on
    /// the real chip, so the arbiter lets the Copper through it (see
    /// `refresh_slot_active_at`).
    pub(super) fn line_end_refresh_slot(hpos: u32) -> bool {
        matches!(hpos, 0x0E2 | 0x0E3)
    }

    pub(super) fn disk_slot_active_at(&self, hpos: u32) -> bool {
        // Standard OCS disk DMA reserves three slots per line (the actual
        // floppy->chip-RAM transfer is rate-based in `floppy.tick`, so this
        // reservation only models the CPU/blitter stall). The previous code
        // reserved a six-slot band (0x009-0x00E), double the hardware count,
        // which over-stalled the CPU during disk loading. Copperline does not model
        // the ECS "fast disk" slot expansion, so three is correct here.
        // Diagnostic builds can remove disk DMA CPU/blitter stalls entirely
        // for timing experiments. Normal builds always reserve the slots.
        if no_disk_stall() {
            return false;
        }
        self.agnus.dmacon & DMACON_DSKEN != 0
            && self.floppy.dma_active(self.agnus.dmacon)
            && matches!(hpos, 0x007 | 0x009 | 0x00B)
    }

    pub(super) fn audio_slot_active_at(&self, hpos: u32) -> bool {
        // Each of the four audio channels has one fixed DMA slot (hpos 0x00D,
        // 0x00F, 0x011, 0x013). The slot is used only on lines where a DMA
        // request was latched into Agnus at the previous line end -- roughly
        // once per 2*AUDxPER cck at music periods, well under once per line.
        // The DMACON audio bits do NOT gate the slot: a request posted while
        // DMA was on is still serviced after software turns the channel off
        // (that latched-request service is what lets a brief DMACON pulse
        // kick a channel into IRQ-mode free-run, vAmigaTS pertimer1).
        match Self::audio_dma_channel_at(hpos) {
            Some(channel) => self.paula.audio_dma_request(channel).is_some(),
            None => false,
        }
    }

    pub(super) fn audio_dma_channel_at(hpos: u32) -> Option<usize> {
        match hpos {
            0x00D => Some(0),
            0x00F => Some(1),
            0x011 => Some(2),
            0x013 => Some(3),
            _ => None,
        }
    }

    pub(super) fn flush_audio_before_audio_dma_slot(&mut self) {
        if Self::audio_dma_channel_at(self.agnus.hpos).is_some() {
            self.flush_audio();
        }
    }

    pub(super) fn read_chip_word_for_audio_dma(&self, address: u32) -> u16 {
        if self.mem.chip_ram.is_empty() {
            return 0;
        }
        let off = (address as usize) % self.mem.chip_ram.len();
        let hi = self.mem.chip_ram[off] as u16;
        let lo = self.mem.chip_ram[(off + 1) % self.mem.chip_ram.len()] as u16;
        (hi << 8) | lo
    }

    pub(super) fn sprite_slot_active_at(&self, hpos: u32) -> bool {
        // Real OCS sprite DMA fetches only on lines where a sprite is actually
        // active (within its vstart..vstop), not on every line. Sprite N owns
        // the two odd slots $15+4N and $17+4N (the hardware slot chart /
        // vAmiga's DAS table), so reserve them only when that sprite is
        // fetching data this line -- gating on the same `data_dma_active` the
        // renderer uses, so the bus model and the captured image agree.
        // Parked/off-screen sprites free their slots for the CPU/blitter.
        if self.agnus.dmacon & DMACON_SPREN == 0 {
            return false;
        }
        if self.sprite_dma_inhibited_by_vertical_blank_at(self.agnus.vpos) {
            return false;
        }
        // Sprite DMA slots sit on ODD color clocks (same parity as refresh/
        // disk/audio -- the HRM chart's fixed-DMA band), so they never block
        // the Copper's even-clock fetches.
        if !(0x015..=0x033).contains(&hpos) || hpos & 1 == 0 {
            return false;
        }
        let sprite = ((hpos - 0x015) / 4) as usize;
        if sprite >= 8 {
            return false;
        }
        // A channel uses its slots when it is fetching data this line, or on
        // its vstop line where the slots fetch the next POS/CTL control word.
        let state = &self.display_dma_sprite_state[sprite];
        state.dma_enabled || self.agnus.vpos as i32 == state.vstop
    }

    pub(super) fn record_bitplane_dmacon_write(&mut self, previous: u16) {
        self.bitplane_dmacon_delay = Some(BitplaneDmaconDelay {
            previous,
            changed_at_cck: self.emulated_cck,
        });
    }

    pub(super) fn effective_bitplane_dmacon(&self) -> u16 {
        self.effective_bitplane_dmacon_at(self.emulated_cck)
    }

    pub(super) fn effective_bitplane_dmacon_at(&self, emulated_cck: u64) -> u16 {
        if let Some(delay) = self.bitplane_dmacon_delay {
            if emulated_cck.saturating_sub(delay.changed_at_cck) < 2 {
                return delay.previous;
            }
        }
        self.agnus.dmacon
    }

    pub(super) fn record_bitplane_bplcon0_write(&mut self, previous: u16) {
        self.bitplane_bplcon0_delay = Some(BitplaneBplcon0Delay {
            previous,
            changed_at_cck: self.emulated_cck,
        });
    }

    pub(super) fn effective_bitplane_bplcon0(&self) -> u16 {
        self.effective_bitplane_bplcon0_at(self.emulated_cck)
    }

    pub(super) fn effective_bitplane_bplcon0_at(&self, emulated_cck: u64) -> u16 {
        if let Some(delay) = self.bitplane_bplcon0_delay {
            if emulated_cck.saturating_sub(delay.changed_at_cck) < 3 {
                return delay.previous;
            }
        }
        self.denise.bplcon0
    }

    // Agnus latches the bitplane plane count / resolution at the start of each
    // DDF fetch block rather than continuously. A BPLCON0 write at or before a
    // block's first cycle configures that block's fetch; a write that lands
    // mid-block only affects the next block. This is the cycle-accurate version
    // of the coarse three-CCK `effective_bitplane_bplcon0_at` delay: it lets a
    // write exactly at DDFSTRT enable the earliest-slot plane on the same line
    // (e.g. lores plane 4, which fetches first), while still deferring a write
    // that arrives after the block has begun.
    pub(super) fn bitplane_bplcon0_for_block(&self, block_start_cck: i128) -> u16 {
        if let Some(delay) = self.bitplane_bplcon0_delay {
            if i128::from(delay.changed_at_cck) > block_start_cck {
                return delay.previous;
            }
        }
        self.denise.bplcon0
    }

    /// Agnus starts a line's bitplane fetch from a horizontal comparator: the
    /// DDF flop sets on the single colour clock where the counter equals
    /// DDFSTRT. Both the old and the new value are judged against the write's
    /// effect clock ([`super::ddf_line::DDF_WRITE_COMMIT_CCK`] after the write slot), with the
    /// sequencer's edge semantics for DDFSTRT: the outgoing value fires
    /// strictly before that clock, the incoming value strictly after it. When
    /// neither can fire, the line has no start at all - the counter never
    /// returns to the new value before the horizontal wrap - so it fetches
    /// nothing. A value that reaches the comparator after the flop has already
    /// set cannot un-start the run: only DDFSTOP ends it.
    ///
    /// Regression example: Microcosm's CD32 status panel repoints all seven
    /// bitplanes and drops DDFSTRT from $2C to $18 in one copper burst that
    /// overruns the line, so the new DDFSTRT commits at hpos ~$1E on the
    /// panel's first line - too late for $2C, too early for nothing, and $18
    /// is long gone. Restarting the fetch mid-block there hands the planes
    /// whose lo-res slot number survives the truncated block (BPL5 and BPL1,
    /// slots 6 and 7) one fetch more than the rest, and the two pointer
    /// groups stay 8 bytes apart for the whole panel.
    ///
    /// TODO: when the outgoing value does fire, the run belongs to it for the
    /// rest of the line, but the wide-FMODE value-window path has no run state
    /// and keeps deriving the fetch grid from the live DDFSTRT, so the
    /// remainder of such a line is placed on the incoming value's grid. Giving
    /// wide FMODE the FMODE=0 path's latched start (`ddf_line.rs`) would model
    /// it properly.
    pub(super) fn record_ddfstrt_write_match_miss(&mut self, previous: u16, ddfstrt: u16) {
        let bplcon0 = self.effective_bitplane_bplcon0();
        let revision = self.agnus.revision();
        let effect = u32::from(self.agnus.hpos as u16 + super::ddf_line::DDF_WRITE_COMMIT_CCK);
        let ddfstart = u32::from(effective_ddf_hpos(revision, bplcon0, ddfstrt));
        if ddfstart == 0 || ddfstart > effect {
            // The incoming value reaches the comparator in time to match.
            return;
        }
        let previous_start = u32::from(effective_ddf_hpos(revision, bplcon0, previous));
        if previous_start != 0
            && previous_start < effect
            && !self.bitplane_ddfstart_missed_on_line(self.agnus.vpos, previous_start)
        {
            // The outgoing value fired before the write took effect: the fetch
            // sequence is running and the new DDFSTRT applies from the next
            // line.
            return;
        }
        self.bitplane_ddfstart_miss = Some(BitplaneDdfStartMiss {
            vpos: self.agnus.vpos,
            ddfstart,
        });
    }

    pub(super) fn bitplane_ddfstart_missed_on_line(&self, vpos: u32, ddfstart: u32) -> bool {
        self.bitplane_ddfstart_miss
            .is_some_and(|miss| miss.vpos == vpos && miss.ddfstart == ddfstart)
    }

    pub(super) fn bitplane_slot_active_at(&self, vpos: u32, hpos: u32) -> bool {
        if self.ddf_seq_active() {
            // FMODE=0: the walked DDF sequencer table owns the decision
            // (vertical window, comparator flops, stop drains, carried runs).
            let _ = vpos;
            return self.ddf_seq_slot_active_at(hpos);
        }
        if hpos < SLOT_MASK_BITS && self.wide_bitplane_dynamic_vpos.get() != Some(vpos) {
            if !self.wide_bitplane_hot_line.is_current(vpos) {
                let plan = if self.diw_vertical_open_at(vpos) {
                    let bplcon0 = self.effective_bitplane_bplcon0();
                    self.bitplane_slot_plan_for_bplcon0(bplcon0)
                        .filter(|plan| !self.bitplane_ddfstart_missed_on_line(vpos, plan.start))
                } else {
                    None
                };
                self.wide_bitplane_hot_line.publish(vpos, plan);
            }
            return self.wide_bitplane_hot_line.slot_mask[(hpos / 64) as usize].get()
                & (1u64 << (hpos % 64))
                != 0;
        }
        self.dynamic_bitplane_slot_active_at(vpos, hpos)
    }

    /// Block-delay-aware fallback for a wide-FMODE line changed by a
    /// fetch-affecting register write, and for programmable lines beyond the
    /// precomputed 256-colour-clock mask.
    pub(super) fn dynamic_bitplane_slot_active_at(&self, vpos: u32, hpos: u32) -> bool {
        // Bitplane DMA only runs inside the vertical display window (the
        // flop set at DIWSTRT.V, cleared at DIWSTOP.V), so the top-border
        // and vertical-blank lines are free for the blitter/CPU. Rejecting
        // this before the DDF/BPLCON0 plan lookup avoids per-color-clock
        // cache probes on lines that cannot fetch bitplanes.
        if !self.diw_vertical_open_at(vpos) {
            return false;
        }

        let mut bplcon0 = self.effective_bitplane_bplcon0();
        let mut plan = self.bitplane_slot_plan_for_bplcon0(bplcon0);
        if plan.is_none() {
            if let Some(delay) = self.bitplane_bplcon0_delay {
                bplcon0 = delay.previous;
                plan = self.bitplane_slot_plan_for_bplcon0(bplcon0);
            }
        }
        let Some(mut plan) = plan else {
            return false;
        };
        if self.bitplane_ddfstart_missed_on_line(vpos, plan.start) {
            return false;
        }
        if hpos >= plan.start {
            for _ in 0..2 {
                let block_span = if plan.hires_like {
                    plan.period
                } else {
                    plan.unit
                }
                .max(1);
                let rel = hpos - plan.start;
                let block_start_hpos = plan.start + (rel / block_span) * block_span;
                let block_start_cck = i128::from(self.emulated_cck)
                    - i128::from(hpos.saturating_sub(block_start_hpos));
                let block_bplcon0 = self.bitplane_bplcon0_for_block(block_start_cck);
                if block_bplcon0 == bplcon0 {
                    break;
                }
                bplcon0 = block_bplcon0;
                let Some(block_plan) = self.bitplane_slot_plan_for_bplcon0(bplcon0) else {
                    return false;
                };
                plan = block_plan;
                if hpos < plan.start || self.bitplane_ddfstart_missed_on_line(vpos, plan.start) {
                    return false;
                }
            }
        }
        // Cheap hpos rejection first via the memoized slot bitmask (which also
        // encodes the start/last_fetch_hpos bounds). The vpos gates below only
        // matter on color clocks that are actually bitplane slots, so testing
        // the pattern first lets the off-slot majority skip them entirely.
        let is_slot = if hpos < SLOT_MASK_BITS {
            plan.slot_mask[(hpos / 64) as usize] & (1u64 << (hpos % 64)) != 0
        } else {
            // Programmable line wider than the bitmask: fall back to the math.
            Self::plan_slot_at(&plan, hpos)
        };
        if !is_slot {
            return false;
        }
        true
    }

    /// Whether `hpos` is a bitplane fetch slot for `plan`, from the fetch
    /// cadence alone (vpos-independent). This is the exact per-color-clock math
    /// that `bitplane_slot_active_at` used inline; it is now memoized into
    /// `BitplaneSlotPlan::slot_mask` and kept here for that precompute and for
    /// the wide-programmable-line fallback.
    pub(super) fn plan_slot_at(plan: &BitplaneSlotPlan, hpos: u32) -> bool {
        if hpos < plan.start || hpos > plan.last_fetch_hpos {
            return false;
        }
        let rel = hpos - plan.start;
        if plan.hires_like {
            return rel.is_multiple_of(plan.period)
                && (rel / plan.period) * plan.quantum < plan.words_per_row;
        }
        if (rel / plan.unit) * plan.quantum >= plan.words_per_row {
            return false;
        }
        let unit_off = rel % plan.unit;
        if unit_off >= 8 {
            return false;
        }
        let order = unit_off;
        plan.order_mask & (1u8 << order) != 0
    }

    pub(super) fn bitplane_slot_plan_for_bplcon0(&self, bplcon0: u16) -> Option<BitplaneSlotPlan> {
        let dmacon = self.effective_bitplane_dmacon();
        let key = BitplaneSlotKey {
            bplen: dmacon & (DMACON_DMAEN | DMACON_BPLEN) == (DMACON_DMAEN | DMACON_BPLEN),
            bplcon0: bitplane_slot_plan_bplcon0_key(bplcon0, self.aga_enabled()),
            ddfstrt: self.denise.ddfstrt,
            ddfstop: self.denise.ddfstop,
            fmode: self.agnus.fmode(),
            harddis: self.harddis_active(),
        };
        if let Some(plan) = self.bitplane_slot_plan_cache.lookup(key) {
            return plan;
        }
        let plan = self.compute_bitplane_slot_plan(&key);
        self.bitplane_slot_plan_cache.insert(key, plan);
        plan
    }

    pub(super) fn compute_bitplane_slot_plan(
        &self,
        key: &BitplaneSlotKey,
    ) -> Option<BitplaneSlotPlan> {
        if !key.bplen {
            return None;
        }
        let bplcon0 = key.bplcon0;
        let nplanes = bitplane_dma_planes_for_fmode(bplcon0, key.fmode, self.aga_enabled());
        if nplanes == 0 {
            return None;
        }
        let (start, stop) = effective_ddf_window(
            self.agnus.revision(),
            bplcon0,
            key.ddfstrt,
            key.ddfstop,
            key.harddis,
        )?;
        let start = u32::from(start);
        // Mirrors the capture loop's FMODE cadence so arbitration and
        // capture cannot drift: wider fetches reserve fewer slots.
        let fmode = key.fmode;
        let quantum = bitplane_fetch_quantum(fmode);
        let period = bitplane_fetch_period(bplcon0, fmode);
        let unit = bitplane_fetch_unit(bplcon0, fmode);
        // The DDFSTRT comparator starts the sequencer. Wide FMODE increases
        // the unit length between fetch groups; it does not move the first
        // group back to an absolute unit boundary.
        let start = u32::from(crate::chipset::agnus::anchor_bitplane_fetch_start(
            start as u16,
            unit,
        ));
        // The sequencer completes whole units from the DDF start:
        // a DDFSTOP inside a unit extends the fetch to the end of the unit
        // starting at-or-after it (see agnus::bitplane_fetch_blocks), so the
        // last slot can land past DDFSTOP.
        let blocks =
            crate::chipset::agnus::bitplane_fetch_blocks(u32::from(stop) - start, unit) as u32;
        let last_fetch_hpos = start + blocks * unit - 1;
        let words_per_row = bitplane_words_per_row(
            self.agnus.revision(),
            bplcon0,
            fmode,
            key.ddfstrt,
            key.ddfstop,
            key.harddis,
        ) as u32;
        let mut order_mask = 0u8;
        for plane in 0..nplanes.min(8) {
            order_mask |= 1u8 << bitplane_fetch_order(bplcon0, plane);
        }
        let mut plan = BitplaneSlotPlan {
            start,
            last_fetch_hpos,
            period,
            unit,
            quantum,
            words_per_row,
            hires_like: bitplane_hires(bplcon0) || bitplane_shres(bplcon0),
            order_mask,
            slot_mask: [0u64; 4],
        };
        // Memoize the vpos-independent fetch pattern so the per-color-clock
        // arbiter does a bit test instead of the div/mod in `plan_slot_at`.
        for hpos in plan.start..=plan.last_fetch_hpos.min(SLOT_MASK_BITS - 1) {
            if Self::plan_slot_at(&plan, hpos) {
                plan.slot_mask[(hpos / 64) as usize] |= 1u64 << (hpos % 64);
            }
        }
        Some(plan)
    }

    pub(super) fn copper_ready_for_slot(&self) -> bool {
        if self.pending_copper_frame_start.is_some() {
            return false;
        }
        if !self.copper_dma_enabled() {
            return false;
        }
        self.copper.is_running()
    }

    /// Whether the Copper's WAIT/SKIP comparator advances this color clock.
    /// Unlike a bus slot, the comparator does not arbitrate against fixed DMA:
    /// it keeps evaluating while bitplane/sprite/disk/audio DMA owns the bus.
    pub(super) fn copper_comparator_runs_at(&self, hpos: u32) -> bool {
        self.pending_copper_frame_start.is_none()
            && self.copper_dma_enabled()
            && !self.copper_bus_lockout_active_at(hpos)
    }

    pub(super) fn copper_bus_lockout_active_at(&self, hpos: u32) -> bool {
        hpos == self.copper_bus_lockout_hpos()
    }

    pub(super) fn copper_bus_lockout_hpos(&self) -> u32 {
        if self.agnus.lol {
            COPPER_BUS_LOCKOUT_HPOS_LONG_LINE
        } else {
            COPPER_BUS_LOCKOUT_HPOS_SHORT_LINE
        }
    }

    pub(super) fn cck_until_copper_wait_position(&self, wait: CopperWait) -> Option<u32> {
        if wait.is_end_of_list() {
            return None;
        }
        if wait.comparator_is_satisfied(self.agnus.vpos, self.agnus.hpos) {
            return Some(0);
        }

        let line_cck = self.agnus.current_line_cck();
        if wait.compare_mask() == 0xFFFE {
            return self.cck_until_full_mask_copper_wait(wait);
        }

        let mut vpos = self.agnus.vpos;
        let mut hpos = self.agnus.hpos;
        let frame_lines = self.agnus.current_frame_lines();
        let frame_cck = frame_lines.saturating_mul(line_cck);
        for delta in 1..=frame_cck {
            hpos += 1;
            if hpos >= line_cck {
                hpos = 0;
                vpos += 1;
                if vpos >= frame_lines {
                    vpos = 0;
                }
            }
            if wait.comparator_is_satisfied(vpos, hpos) {
                return Some(delta);
            }
        }
        None
    }

    pub(super) fn cck_until_full_mask_copper_wait(&self, wait: CopperWait) -> Option<u32> {
        // The comparator's horizontal input runs two color clocks ahead of
        // the beam, so a sleeping full-mask wait releases two color clocks
        // before its masked horizontal target (see
        // `CopperWait::comparator_is_satisfied`).
        let target_h = (wait.position_bits() & 0x00FE) as u32;
        let release_h = target_h.saturating_sub(2);
        let frame_lines = self.agnus.current_frame_lines();

        for line_delta in 0..=frame_lines {
            let vpos = (self.agnus.vpos + line_delta) % frame_lines;
            let line_start_delta = if line_delta == 0 {
                0
            } else {
                self.agnus.cck_until_line_ticks(line_delta)?
            };
            let target_line_cck = self.line_cck_after_lines(line_delta);

            if line_delta == 0 {
                if release_h < target_line_cck
                    && self.agnus.hpos <= release_h
                    && wait.comparator_is_satisfied(vpos, release_h)
                {
                    return Some(release_h - self.agnus.hpos);
                }
            } else if wait.comparator_is_satisfied(vpos, 0) {
                return Some(line_start_delta);
            } else if release_h < target_line_cck && wait.comparator_is_satisfied(vpos, release_h) {
                return Some(line_start_delta + release_h);
            }
        }

        None
    }

    pub(super) fn line_cck_after_lines(&self, line_delta: u32) -> u32 {
        if !self.agnus.long_line_toggles() {
            // PAL, LOLDIS, or programmable VARBEAMEN: every line is the same.
            return self.agnus.current_line_cck();
        }
        let target_lol = if line_delta.is_multiple_of(2) {
            self.agnus.lol
        } else {
            !self.agnus.lol
        };
        self.agnus.line_cck_for(target_lol)
    }

    pub(super) fn next_chip_bus_quantum(&self) -> u32 {
        next_chip_bus_quantum_at(self.agnus.hpos, self.agnus.current_line_cck())
    }

    /// Horizontal position on the restart line where the vertical-blank
    /// COP1LC strobe wakes the Copper. Calibrated against the vAmigaTS
    /// Copper/Skip/copstrt1+copstrt2 real-A500 captures, which bracket the
    /// first instruction's comparator decision to beam $0B..$0C: the Copper
    /// does not start fetching at the very first color clock of the line.
    pub(super) fn cck_until_pending_copper_frame_start(&self) -> Option<u32> {
        self.pending_copper_frame_start?;
        let target_vpos = copper_frame_start_vpos(self.agnus.video_standard());
        if self.agnus.vpos > target_vpos {
            return Some(0);
        }
        if self.agnus.vpos == target_vpos {
            return Some(COPPER_FRAME_START_HPOS.saturating_sub(self.agnus.hpos));
        }
        self.agnus
            .cck_until_line_start(target_vpos)
            .map(|cck| cck.saturating_add(COPPER_FRAME_START_HPOS))
    }

    pub(super) fn start_pending_copper_frame_if_due(&mut self) {
        let Some(cop1lc) = self.pending_copper_frame_start else {
            return;
        };
        let target_vpos = copper_frame_start_vpos(self.agnus.video_standard());
        if self.agnus.vpos < target_vpos
            || (self.agnus.vpos == target_vpos && self.agnus.hpos < COPPER_FRAME_START_HPOS)
        {
            return;
        }
        self.pending_copper_frame_start = None;
        self.copper.frame_start(cop1lc);
        // The vertical-blank strobe selects COP1LC and records whether the
        // Copper is live this field; a dormant Copper (DMA off here) has its
        // PC retargeted by later COPxLC writes (copper_lc_written).
        self.copper_current_list = 1;
        self.copper_active_in_frame = self.copper_dma_enabled();
    }

    /// A COPxLC location register was rewritten. While the Copper has not
    /// been active in the current field, a write to the location register it
    /// was last strobed from retargets its program counter directly (real
    /// Agnus behaviour, photographed by the vAmigaTS Copper/lc family);
    /// otherwise the write only loads the latch for the next strobe. A
    /// rewrite in the wrap-to-strobe window refreshes the pending strobe's
    /// address so the restart uses the live COP1LC value.
    pub(super) fn copper_lc_written(&mut self, list: u8) {
        let lc = if list == 1 {
            self.agnus.cop1lc
        } else {
            self.agnus.cop2lc
        };
        if self.pending_copper_frame_start.is_some() {
            if list == 1 {
                self.pending_copper_frame_start = Some(lc);
            }
            return;
        }
        // A dormant Copper necessarily has its DMA off: any COPEN edge sets
        // copper_active_in_frame. The explicit DMA check keeps directly
        // constructed test states (DMACON preset without a register write)
        // on the latch-only path a live Copper uses.
        if !self.copper_active_in_frame
            && !self.copper_dma_enabled()
            && self.copper_current_list == list
        {
            self.copper.jump(lc);
        }
    }

    pub(super) fn record_slice_bus_advance(&mut self, cck: u32, tick: AgnusTick) {
        self.slice_bus_advanced_cck = self.slice_bus_advanced_cck.saturating_add(cck);
        add_agnus_tick(&mut self.slice_bus_tick, tick);
        if self.device_clock.realtime_enabled {
            self.device_clock.note_realtime_device_advance(cck);
        }
        // Defer the timed-device tick: accumulate these color clocks and apply
        // them in one batch at the next device observation or instruction
        // boundary (see `flush_timed_devices`). The chipset/beam advance above
        // already happened per color clock; only the CIA/serial/pots/audio/
        // floppy/Akiko devices, whose state the CPU can only observe through a
        // register read or an interrupt, are batched.
        self.pending_device_cck = self.pending_device_cck.saturating_add(cck);
        add_agnus_tick(&mut self.pending_device_tick, tick);
    }

    /// Apply any deferred timed-device color clocks (see `record_slice_bus_
    /// advance`). Called before every device-register observation (CIA, custom,
    /// and other peripheral reads/writes) and at each instruction boundary, so
    /// the CPU never sees a stale device or a late interrupt. Batching is exact:
    /// the CIA E-clock divider carries its remainder and every device tick is
    /// linear in the color-clock count.
    pub fn flush_timed_devices(&mut self) {
        let cck = std::mem::take(&mut self.pending_device_cck);
        if cck == 0 {
            return;
        }
        let tick = std::mem::take(&mut self.pending_device_tick);
        self.tick_timed_devices(cck, tick);
    }
}
