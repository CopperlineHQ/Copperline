//! Per-line bitplane DDF sequencer tracking: walks the
//! [`crate::chipset::ddf_sequencer`] flop model once per scanline and serves
//! the resulting fetch table to the slot arbiter and the DMA capture loop.
//!
//! The walked table replaces the value-range window logic for FMODE=0
//! fetches: missed or invalid DDFSTRT/DDFSTOP comparators, stop drains
//! through the final fetch unit, and runs carried across line boundaries all
//! fall out of the flop walk. Wide-FMODE (AGA quantum > 1) fetches keep the
//! value-window plan; vAmiga (the flop model's hardware-verified source) has
//! no AGA counterpart to transcribe.

use super::*;
use crate::chipset::ddf_sequencer::{self as seq, DdfSignal, DdfState};

/// Widest line the fetch table covers (PAL 227, NTSC long 228).
pub(super) const DDF_SEQ_MAX_LINE_CCKS: usize = 232;

/// A DDFSTRT/DDFSTOP/BPLCON0/DMACON write that reached the sequencer during
/// the current line, at the colour clock where it takes effect.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct DdfSeqWrite {
    pub effect_cck: u16,
    pub kind: DdfSeqWriteKind,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(super) enum DdfSeqWriteKind {
    Ddfstrt(u16),
    Ddfstop(u16),
    Bplcon0(u16),
    BmapenSet,
    BmapenClr,
}

/// One line's walked bitplane fetch table.
#[derive(Clone)]
pub(super) struct DdfSeqLine {
    pub vpos: u32,
    /// Plane index + 1 fetching at each colour clock; 0 = no bitplane slot.
    pub plane_at: [u8; DDF_SEQ_MAX_LINE_CCKS],
    /// The plane's modulo applies after the fetch at this colour clock
    /// (final-unit slot).
    pub modulo_at: [bool; DDF_SEQ_MAX_LINE_CCKS],
    /// Words each plane fetches over the whole line.
    pub words_per_plane: [u16; 8],
    /// The fetching plane's word ordinal at each slot colour clock (holes
    /// keep their position when DMA enables mid-line: the ordinal counts
    /// table slots, and unfetched earlier slots stay zero words).
    pub word_idx_at: [u16; DDF_SEQ_MAX_LINE_CCKS],
    /// First fetch colour clock of the line, if any.
    pub first_fetch_cck: Option<u16>,
    /// The run's first fetch-unit boundary (first fetch minus its unit
    /// offset): the position that anchors word 0 on the display. `None`
    /// when the line began inside a run carried across the line wrap: the
    /// continuation tail is not a comparator-anchored origin (its unit
    /// counter carries over mid-word), so the renderer keeps the register
    /// view for such lines.
    pub run_origin_cck: Option<u16>,
    /// Sequencer state after the line's walk (becomes the next line's
    /// initial state).
    pub end_state: DdfState,
}

impl Bus {
    /// Whether the flop-walked fetch table drives bitplane DMA for the
    /// current display settings (FMODE=0-style single-word fetches).
    pub(super) fn ddf_seq_active(&self) -> bool {
        crate::chipset::agnus::bitplane_fetch_quantum(self.agnus.fmode()) == 1
    }

    fn ddf_seq_ecs_rules(&self) -> bool {
        !matches!(self.agnus.revision(), AgnusRevision::Ocs)
    }

    /// The DDF comparator strobe positions for one register over the line,
    /// honouring mid-line rewrites: within each written-value's reign, the
    /// comparator fires if its position falls inside that span. The edge
    /// semantics mirror vAmiga's sequencer pokes: a rewritten value only
    /// fires strictly after its commit colour clock, and an old DDFSTOP
    /// value still fires ON the commit clock (`invalidate(posh + 1)`) while
    /// an old DDFSTRT does not (`invalidate(posh)`).
    fn comparator_strobes(
        line_start_value: u16,
        writes: &[(u16, u16)],
        line_ccks: u16,
        old_fires_on_commit_cck: bool,
        out: &mut Vec<u16>,
    ) {
        let mut active = line_start_value;
        let mut span_start = 0u16;
        for &(effect_cck, value) in writes {
            let span_end = if old_fires_on_commit_cck {
                effect_cck.saturating_add(1)
            } else {
                effect_cck
            }
            .min(line_ccks);
            if active >= span_start && active < span_end {
                out.push(active);
            }
            span_start = span_start.max(effect_cck.saturating_add(1));
            active = value;
        }
        if active >= span_start && active < line_ccks {
            out.push(active);
        }
    }

    /// Build (or rebuild) the fetch table for the current line from the
    /// carried sequencer state and this line's register-write log.
    pub(super) fn ddf_seq_build_line(&self) -> DdfSeqLine {
        let vpos = self.agnus.vpos;
        let line_ccks = self.agnus.current_line_cck() as u16;
        let revision = self.agnus.revision();
        let mask = crate::chipset::agnus::ddf_register_mask(revision);

        let mut state = self.ddf_seq_line_initial.get();

        // Line-granular vertical flop and DMA/control refresh: the vertical
        // display window opens at DIWSTRT.V and closes at DIWSTOP.V.
        state.bpv = display_window_contains_vpos(
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
            vpos,
        );

        let writes = self.ddf_seq_writes.borrow();
        // Runtime writes always land in the log, so when the log carries no
        // DMACON/BPLCON0 strobes the live values equal the line-start values;
        // reading them live also keeps direct register pokes in unit tests
        // coherent without a rollover.
        let has_bmapen_write = writes.iter().any(|w| {
            matches!(
                w.kind,
                DdfSeqWriteKind::BmapenSet | DdfSeqWriteKind::BmapenClr
            )
        });
        let has_con_write = writes
            .iter()
            .any(|w| matches!(w.kind, DdfSeqWriteKind::Bplcon0(_)));
        let start_ctl = self.ddf_seq_line_start_ctl.get();
        state.bmapen = if has_bmapen_write {
            start_ctl.0
        } else {
            self.agnus.dmacon & (DMACON_DMAEN | DMACON_BPLEN) == (DMACON_DMAEN | DMACON_BPLEN)
        };
        state.bplcon0 = if has_con_write {
            start_ctl.1
        } else {
            self.denise.bplcon0
        };
        let mut strt_writes: Vec<(u16, u16)> = Vec::new();
        let mut stop_writes: Vec<(u16, u16)> = Vec::new();
        let mut extra: Vec<DdfSignal> = Vec::new();
        for w in writes.iter() {
            match w.kind {
                DdfSeqWriteKind::Ddfstrt(v) => strt_writes.push((w.effect_cck, v & mask)),
                DdfSeqWriteKind::Ddfstop(v) => stop_writes.push((w.effect_cck, v & mask)),
                DdfSeqWriteKind::Bplcon0(v) => extra.push(DdfSignal {
                    cck: w.effect_cck.min(line_ccks.saturating_sub(1)),
                    bits: seq::sig::CON,
                    bplcon0: v,
                }),
                DdfSeqWriteKind::BmapenSet => extra.push(DdfSignal {
                    cck: w.effect_cck.min(line_ccks.saturating_sub(1)),
                    bits: seq::sig::BMAPEN_SET,
                    bplcon0: 0,
                }),
                DdfSeqWriteKind::BmapenClr => extra.push(DdfSignal {
                    cck: w.effect_cck.min(line_ccks.saturating_sub(1)),
                    bits: seq::sig::BMAPEN_CLR,
                    bplcon0: 0,
                }),
            }
        }
        // Runtime writes always land in the log (custom_regs hooks), so a
        // register with no logged write is unchanged since line start; use
        // the live value then. This also keeps direct register pokes in
        // unit tests coherent without a line rollover. First writes snapshot
        // the pre-write value into ddf_seq_line_start_regs.
        let logged = self.ddf_seq_line_start_regs.get();
        let start_regs = (
            if strt_writes.is_empty() {
                self.denise.ddfstrt
            } else {
                logged.0
            },
            if stop_writes.is_empty() {
                self.denise.ddfstop
            } else {
                logged.1
            },
        );
        let mut strt_strobes = Vec::new();
        let mut stop_strobes = Vec::new();
        Self::comparator_strobes(
            start_regs.0 & mask,
            &strt_writes,
            line_ccks,
            false,
            &mut strt_strobes,
        );
        Self::comparator_strobes(
            start_regs.1 & mask,
            &stop_writes,
            line_ccks,
            true,
            &mut stop_strobes,
        );
        for cck in strt_strobes {
            extra.push(DdfSignal {
                cck,
                bits: seq::sig::BPHSTART,
                bplcon0: 0,
            });
        }
        for cck in stop_strobes {
            extra.push(DdfSignal {
                cck,
                bits: seq::sig::BPHSTOP,
                bplcon0: 0,
            });
        }
        drop(writes);

        // The static strt/stop strobes are already covered by the log-based
        // reconstruction above, so pass never-matching values to the default
        // list builder and merge everything through `extra`. BEAMCON0.HARDDIS
        // relaxes the hardwired stop position.
        let (_, hard_stop) = crate::chipset::agnus::ddf_hard_bounds(self.harddis_active());
        let signals =
            seq::line_signals_with_hard_stop(0xFFFF, 0xFFFF, hard_stop, line_ccks, &extra);
        // A line that begins with BPRUN already up continues a run carried
        // across the line wrap; its first fetches are a mid-unit tail, not
        // a comparator-anchored run origin.
        let run_carried_in = state.bprun;
        let fetches = seq::walk_line(
            self.aga_enabled(),
            self.ddf_seq_ecs_rules(),
            &signals,
            &mut state,
        );

        let mut line = DdfSeqLine {
            vpos,
            plane_at: [0; DDF_SEQ_MAX_LINE_CCKS],
            modulo_at: [false; DDF_SEQ_MAX_LINE_CCKS],
            words_per_plane: [0; 8],
            word_idx_at: [0; DDF_SEQ_MAX_LINE_CCKS],
            first_fetch_cck: None,
            run_origin_cck: None,
            end_state: state,
        };
        let shres = crate::chipset::agnus::bitplane_shres(state.bplcon0);
        let hires = crate::chipset::agnus::bitplane_hires(state.bplcon0);
        for f in &fetches {
            let idx = usize::from(f.cck);
            if idx >= DDF_SEQ_MAX_LINE_CCKS {
                continue;
            }
            let plane = usize::from(f.plane).min(7);
            line.plane_at[idx] = f.plane + 1;
            line.modulo_at[idx] = f.apply_modulo;
            // Word addressing is unit-based: a plane enabled mid-line keeps
            // fetching into its unit's word position, leaving earlier words
            // zero (matching the hardware's per-unit pointer cadence).
            line.word_idx_at[idx] = if shres {
                f.unit_ord * 4 + u16::from(f.counter >> 1)
            } else if hires {
                f.unit_ord * 2 + u16::from(f.counter >= 4)
            } else {
                f.unit_ord
            };
            line.words_per_plane[plane] =
                line.words_per_plane[plane].max(line.word_idx_at[idx] + 1);
            if line.first_fetch_cck.is_none() {
                line.first_fetch_cck = Some(f.cck);
                if !run_carried_in {
                    line.run_origin_cck = Some(f.cck.saturating_sub(u16::from(f.counter)));
                }
            }
        }
        line
    }

    /// The walked table for the current line, building it on first use.
    pub(super) fn ddf_seq_line_table(&self) -> std::cell::Ref<'_, DdfSeqLine> {
        {
            let cached = self.ddf_seq_line.borrow();
            if cached
                .as_ref()
                .is_some_and(|line| line.vpos == self.agnus.vpos)
            {
                return std::cell::Ref::map(cached, |line| line.as_ref().unwrap());
            }
        }
        let built = self.ddf_seq_build_line();
        *self.ddf_seq_line.borrow_mut() = Some(built);
        std::cell::Ref::map(self.ddf_seq_line.borrow(), |line| line.as_ref().unwrap())
    }

    /// Invalidate the current line's table (a register write changed the
    /// remaining signals). Already-consumed word counters are preserved by
    /// the capture loop keying on colour clocks, not indices.
    pub(super) fn ddf_seq_invalidate_line(&self) {
        *self.ddf_seq_line.borrow_mut() = None;
    }

    /// Record a register write reaching the sequencer this line.
    pub(super) fn ddf_seq_record_write(&self, kind: DdfSeqWriteKind, delay_cck: u16) {
        let effect_cck = (self.agnus.hpos as u16).saturating_add(delay_cck);
        {
            let mut writes = self.ddf_seq_writes.borrow_mut();
            // First control write of the line: snapshot the pre-write value
            // as the line-start state (the log-empty fast path reads live
            // registers, which just changed).
            match kind {
                DdfSeqWriteKind::BmapenSet | DdfSeqWriteKind::BmapenClr => {
                    if !writes.iter().any(|w| {
                        matches!(
                            w.kind,
                            DdfSeqWriteKind::BmapenSet | DdfSeqWriteKind::BmapenClr
                        )
                    }) {
                        let mut ctl = self.ddf_seq_line_start_ctl.get();
                        ctl.0 = matches!(kind, DdfSeqWriteKind::BmapenClr);
                        self.ddf_seq_line_start_ctl.set(ctl);
                    }
                }
                DdfSeqWriteKind::Bplcon0(_) => {}
                DdfSeqWriteKind::Ddfstrt(_) | DdfSeqWriteKind::Ddfstop(_) => {}
            }
            writes.push(DdfSeqWrite { effect_cck, kind });
        }
        self.ddf_seq_invalidate_line();
    }

    /// Record a BPLCON0 write, snapshotting the pre-write value on the first
    /// control write of the line.
    pub(super) fn ddf_seq_record_bplcon0_write(&self, value: u16, previous: u16, delay_cck: u16) {
        {
            let writes = self.ddf_seq_writes.borrow();
            if !writes
                .iter()
                .any(|w| matches!(w.kind, DdfSeqWriteKind::Bplcon0(_)))
            {
                let mut ctl = self.ddf_seq_line_start_ctl.get();
                ctl.1 = previous;
                self.ddf_seq_line_start_ctl.set(ctl);
            }
        }
        self.ddf_seq_record_write(DdfSeqWriteKind::Bplcon0(value), delay_cck);
    }

    /// Record a DDFSTRT/DDFSTOP write, snapshotting the pre-write values on
    /// the first DDF write of the line.
    pub(super) fn ddf_seq_record_ddf_write(
        &self,
        kind: DdfSeqWriteKind,
        previous: u16,
        delay_cck: u16,
    ) {
        {
            let writes = self.ddf_seq_writes.borrow();
            let (had_strt, had_stop) = writes.iter().fold((false, false), |acc, w| match w.kind {
                DdfSeqWriteKind::Ddfstrt(_) => (true, acc.1),
                DdfSeqWriteKind::Ddfstop(_) => (acc.0, true),
                _ => acc,
            });
            let mut regs = self.ddf_seq_line_start_regs.get();
            match kind {
                DdfSeqWriteKind::Ddfstrt(_) if !had_strt => regs.0 = previous,
                DdfSeqWriteKind::Ddfstop(_) if !had_stop => regs.1 = previous,
                _ => {}
            }
            if !had_strt && !had_stop {
                // The other register was untouched this line: its line-start
                // value is the live one.
                if matches!(kind, DdfSeqWriteKind::Ddfstrt(_)) {
                    regs.1 = self.denise.ddfstop;
                } else {
                    regs.0 = self.denise.ddfstrt;
                }
            }
            self.ddf_seq_line_start_regs.set(regs);
        }
        self.ddf_seq_record_write(kind, delay_cck);
    }

    /// FMODE=0 bitplane DMA capture driven by the walked fetch table:
    /// fetches the assigned plane's word at each table slot, feeds Denise,
    /// advances the plane pointer, and applies the plane's modulo at its
    /// final-unit fetch. Replaces the value-window capture loop wholesale
    /// when the sequencer table is active.
    pub(super) fn capture_bitplane_dma_words_fsm(
        &mut self,
        vpos: u32,
        old_hpos: u32,
        new_hpos: u32,
        old_emulated_cck: u64,
    ) {
        if self.ocs_same_line_diw_start_blocked_vpos == Some(vpos) {
            return;
        }
        if self.mem.chip_ram.is_empty() {
            return;
        }
        let display_bplcon0 = self.effective_bitplane_bplcon0_at(old_emulated_cck);
        let display_planes =
            BitplaneMode::from_bplcon0(display_bplcon0, self.aga_enabled()).display_planes();
        let Some(fb_y) = visible_framebuffer_y(
            vpos,
            self.current_frame_visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        ) else {
            // Lines outside the captured framebuffer advance no pointers,
            // matching the pre-FSM capture (and the vAmiga reference dumps:
            // diwv3/diwv4 pin this - a DIWSTRT.V inside vertical blanking
            // must not skew the visible rows' pointer progression).
            return;
        };
        let (plane_at, modulo_at, word_idx_at, words_per_row, run_origin) = {
            let table = self.ddf_seq_line_table();
            let wpr = table.words_per_plane.iter().copied().max().unwrap_or(0) as usize;
            (
                table.plane_at,
                table.modulo_at,
                table.word_idx_at,
                wpr,
                table.run_origin_cck,
            )
        };
        if words_per_row == 0 {
            return;
        }
        let addr_mask = self.chip_dma_mask;
        let end = new_hpos.min(DDF_SEQ_MAX_LINE_CCKS as u32);
        let mut slots = 0usize;
        let mut rows_started = 0usize;
        for hpos in old_hpos..end {
            let slot = plane_at[hpos as usize];
            if slot == 0 {
                continue;
            }
            let plane = usize::from(slot - 1);
            if plane == 0 {
                self.record_sprite_display_enable_for_bitplane_dma(vpos);
            }
            let word_idx = usize::from(word_idx_at[hpos as usize]);
            let addr = self.display_dma_bplpt[plane] & addr_mask;
            let fetched = read_chip_word_wrapping(&self.mem.chip_ram, addr);
            self.data_bus = fetched;
            let dma_planes = plane_count_from_table(&plane_at);
            if self.capture_bitplane_fetch_word(
                fb_y,
                display_planes,
                dma_planes,
                words_per_row,
                plane,
                word_idx.min(words_per_row.saturating_sub(1)),
                fetched,
            ) {
                rows_started += 1;
            }
            self.denise.write_bpldat(plane, fetched);
            self.display_dma_bplpt[plane] =
                self.display_dma_bplpt[plane].wrapping_add(2) & addr_mask;
            if modulo_at[hpos as usize] {
                let modulo = self.display_dma_modulo_for_plane(plane, vpos);
                self.display_dma_bplpt[plane] =
                    ((self.display_dma_bplpt[plane] as i64).wrapping_add(modulo as i64) as u32)
                        & addr_mask;
            }
            slots += 1;
        }
        if slots != 0 {
            if let Some(row) = self.current_frame_bitplane_rows[fb_y].as_mut() {
                row.fetch_origin_cck = run_origin;
            }
            self.record_bitplane_fetch_timing(slots, rows_started, 0, None);
        }
    }

    /// Line rollover: finalize the ending line's walk, carry the sequencer
    /// state, and reset the per-line write log. `ended_vpos` is the line
    /// that just finished.
    pub(super) fn ddf_seq_on_line_rollover(&mut self, ended_vpos: u32) {
        let end_state = {
            let cached = self.ddf_seq_line.borrow();
            match cached.as_ref() {
                Some(line) if line.vpos == ended_vpos => Some(line.end_state),
                _ => None,
            }
        };
        let end_state = end_state.unwrap_or_else(|| {
            // The table was never built (or was invalidated) for the ended
            // line: walk it now, against the ended line's vpos, so the
            // carried state stays exact.
            let vpos_backup = self.agnus.vpos;
            self.agnus.vpos = ended_vpos;
            let line = self.ddf_seq_build_line();
            self.agnus.vpos = vpos_backup;
            line.end_state
        });
        self.ddf_seq_line_initial.set(end_state);
        self.ddf_seq_line_start_regs
            .set((self.denise.ddfstrt, self.denise.ddfstop));
        self.ddf_seq_line_start_ctl.set((
            self.agnus.dmacon & (DMACON_DMAEN | DMACON_BPLEN) == (DMACON_DMAEN | DMACON_BPLEN),
            self.denise.bplcon0,
        ));
        self.ddf_seq_writes.borrow_mut().clear();
        self.ddf_seq_invalidate_line();
    }
}

/// DMA plane count implied by the walked table (highest plane with a slot).
fn plane_count_from_table(plane_at: &[u8; DDF_SEQ_MAX_LINE_CCKS]) -> usize {
    plane_at.iter().copied().max().unwrap_or(0) as usize
}

#[cfg(test)]
mod tests {
    use super::super::tests::empty_bus;
    use super::*;

    #[test]
    fn standard_window_table_matches_value_model() {
        let mut bus = empty_bus();
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
        bus.denise.diwstrt = 0x2C81;
        bus.denise.diwstop = 0x2CC1;
        bus.denise.ddfstrt = 0x0038;
        bus.denise.ddfstop = 0x00D0;
        bus.denise.bplcon0 = 0x4200; // 4 planes lores
        bus.agnus.vpos = 0x50;
        bus.ddf_seq_on_line_rollover(0x4F);

        let table = bus.ddf_seq_line_table();
        assert_eq!(table.first_fetch_cck, Some(0x39));
        assert_eq!(table.words_per_plane[0], 20);
        assert_eq!(table.words_per_plane[3], 20);
        assert_eq!(table.words_per_plane[4], 0);
        // Plane 1 fetches at the end of each unit ($3F, $47, ...).
        assert_eq!(table.plane_at[0x3F], 1);
        assert_eq!(table.plane_at[0x38], 0);
    }

    #[test]
    fn invalid_stop_extends_the_run_to_the_hard_stop() {
        let mut bus = empty_bus();
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
        bus.denise.diwstrt = 0x2C81;
        bus.denise.diwstop = 0x2CC1;
        bus.denise.ddfstrt = 0x0060;
        bus.denise.ddfstop = 0x00FF; // never matches ($FC-masked to $FC, still past the line's RHW drain)
        bus.denise.bplcon0 = 0x4200;
        bus.agnus.vpos = 0x50;
        bus.ddf_seq_on_line_rollover(0x4F);

        let table = bus.ddf_seq_line_table();
        // Run from $60 to the hard-stop drain: ($D8 - $60) / 8 = 15 units,
        // then the $D8 unit drains as the final unit: 16 words.
        assert_eq!(table.words_per_plane[0], 16);
        assert!(table.plane_at[0xDF] != 0);
    }

    #[test]
    fn vertical_window_gates_the_walk() {
        let mut bus = empty_bus();
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
        bus.denise.diwstrt = 0x2C81;
        bus.denise.diwstop = 0x2CC1;
        bus.denise.ddfstrt = 0x0038;
        bus.denise.ddfstop = 0x00D0;
        bus.denise.bplcon0 = 0x4200;
        bus.agnus.vpos = 0x10; // above DIWSTRT.V
        bus.ddf_seq_on_line_rollover(0x0F);

        let table = bus.ddf_seq_line_table();
        assert_eq!(table.first_fetch_cck, None);
    }

    #[test]
    fn run_carried_across_the_line_wrap_reports_no_origin() {
        let mut bus = empty_bus();
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
        bus.denise.diwstrt = 0x2C81;
        bus.denise.diwstop = 0x2CC1;
        // A start past the hardware stop arms a run the missed RHW cannot
        // stop: it wraps through horizontal blanking into the next line.
        bus.denise.ddfstrt = 0x00E0;
        bus.denise.ddfstop = 0x00FF;
        bus.denise.bplcon0 = 0x4200;
        // Roll over from a line above the vertical window so the armed
        // line starts from a clean (not carried) state.
        bus.agnus.vpos = 0x2C;
        bus.ddf_seq_on_line_rollover(0x2B);
        {
            let table = bus.ddf_seq_line_table();
            assert_eq!(table.run_origin_cck, Some(0xE0));
            assert!(table.end_state.bprun, "run carries across the wrap");
        }
        bus.agnus.vpos = 0x2D;
        bus.ddf_seq_on_line_rollover(0x2C);
        let table = bus.ddf_seq_line_table();
        // The wrapped line fetches from its start, but the tail is not a
        // comparator-anchored origin: the renderer keeps the register view.
        assert!(table.first_fetch_cck.is_some());
        assert_eq!(table.run_origin_cck, None);
    }

    #[test]
    fn start_below_hard_window_reports_its_raw_origin_on_the_armed_line() {
        let mut bus = empty_bus();
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
        bus.denise.diwstrt = 0x2C81;
        bus.denise.diwstop = 0x2CC1;
        bus.denise.ddfstrt = 0x0010;
        bus.denise.ddfstop = 0x0010;
        bus.denise.bplcon0 = 0x4200;
        // The rolled-over line (above the vertical window) fires the $10
        // comparator with SHW still down, so nothing fetches; SHW armed at
        // $18 survives into this line.
        bus.agnus.vpos = 0x2C;
        bus.ddf_seq_on_line_rollover(0x2B);
        {
            // The surviving SHW latch arms the run at the raw $10 grid and
            // the missed stop drains through the hardware stop.
            let table = bus.ddf_seq_line_table();
            assert_eq!(table.run_origin_cck, Some(0x10));
            assert_eq!(table.words_per_plane[0], 26);
        }
        bus.agnus.vpos = 0x2D;
        bus.ddf_seq_on_line_rollover(0x2C);
        // The completed run cleared SHW: the next line starts nothing.
        let table = bus.ddf_seq_line_table();
        assert_eq!(table.first_fetch_cck, None);
    }

    #[test]
    fn mid_line_stop_rewrite_reaches_the_walk() {
        let mut bus = empty_bus();
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
        bus.denise.diwstrt = 0x2C81;
        bus.denise.diwstop = 0x2CC1;
        bus.denise.ddfstrt = 0x0038;
        bus.denise.ddfstop = 0x00D0;
        bus.denise.bplcon0 = 0x4200;
        bus.agnus.vpos = 0x50;
        bus.ddf_seq_on_line_rollover(0x4F);
        // Beam early in the line: rewrite DDFSTOP to $60.
        bus.agnus.hpos = 0x20;
        bus.denise.ddfstop = 0x0060;
        bus.ddf_seq_record_write(DdfSeqWriteKind::Ddfstop(0x0060), 4);

        let table = bus.ddf_seq_line_table();
        // Stop at $60: units $38..$60 = 5, plus the $60 drain unit: 6 words.
        assert_eq!(table.words_per_plane[0], 6);
    }
}
