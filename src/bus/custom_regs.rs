// SPDX-License-Identifier: GPL-3.0-or-later

//! Custom-chip register file: the OCS/ECS/AGA custom-register read and
//! write dispatch ($DFF000 word offsets), including the byte-write latch
//! and POT pin sense. Split out of `bus.rs` for size; this is the same
//! `Bus`, with full access to its private state.

use super::*;

impl Bus {
    pub(super) fn read_custom_word(&mut self, off: u16) -> u16 {
        match off & 0xFFE {
            0x002 => {
                // DMACONR. Bit 14 = BBUSY (blitter busy), bit 13 = BZERO
                // (last blit's D was all zero).
                let mut r = self.agnus.dmacon & 0x07FF;
                if self.blitter.busy {
                    r |= 1 << 14;
                }
                if self.blitter.bzero {
                    r |= 1 << 13;
                }
                self.trace_dmaconr_read(r);
                r
            }
            0x004 => self.agnus.read_vposr(), // VPOSR
            0x006 => self.agnus.read_vhposr(),
            0x00A => self.input.joy0dat(), // JOY0DAT (mouse port 1 counters)
            0x00C => self.input.joy1dat(), // JOY1DAT (mouse port 2 counters)
            0x00E => {
                // Software is observing collisions: arm the per-frame flush from
                // now on so the latch is maintained exactly as hardware does.
                self.collision_tracking_active = true;
                self.accumulate_live_collisions_until_current_beam();
                self.denise.read_clxdat()
            } // CLXDAT
            0x008 => self.floppy.read_dskdatr(), // DSKDATR
            0x010 => self.paula.adkcon,    // ADKCONR
            0x012 => self.paula.read_potdat(0), // POT0DAT
            0x014 => self.paula.read_potdat(1), // POT1DAT
            0x016 => {
                // POTGOR
                let mut v = self.paula.read_potgor(self.pot_pins());
                if self.cd32_pad_serial_mode() {
                    // P5 reads low (driven), P9 carries the serial bit.
                    v &= !(1 << 12);
                    if self.cd32_pad_serial_bit() && !self.input.rmb_port2 {
                        v |= 1 << 14;
                    } else {
                        v &= !(1 << 14);
                    }
                } else if self.input.cd32_pad_port2 {
                    // Leaving serial mode reloads the shift register.
                    // (Interior mutability not needed: POTGOR reads come
                    // through custom_read with &mut self.)
                    self.cd32_pad_shifter = 8;
                }
                v
            }
            0x018 => self.paula.read_serdatr(), // SERDATR
            0x01A => {
                let r = self
                    .floppy
                    .read_dskbytr(self.agnus.dmacon, self.paula.adkcon);
                if self.floppy.take_sync_irq() {
                    self.paula.intreq |= INT_DSKSYNC;
                }
                r
            }
            0x01C => self.paula.intena, // INTENAR
            0x01E => {
                self.flush_audio();
                self.paula.intreq // INTREQR
            }
            off @ 0x0A0..=0x0DF => {
                self.flush_audio();
                self.paula.read_audio_reg(off - 0x0A0)
            }
            // DENISEID: ECS Denise (8373) drives 0xFFFC; software detects ECS
            // via the low byte (0xFC). OCS Denise (8362) has no such register,
            // so it falls through to the undriven-bus fallback below.
            // HHPOSR (ECS Agnus): UHRES dual-mode H counter readback. The
            // counter is not emulated, so this reads the HHPOSW latch.
            0x1DA if self.agnus.revision().is_ecs() => self.agnus.hhpos(),
            0x07C if self.denise_revision.id().is_some() => self.denise_revision.id().unwrap_or(0),
            _ => {
                // Real write-only custom registers leave the CPU reading an
                // undriven custom bus. Copperline does not model the previous
                // bus owner yet, so write-only and unmapped custom reads use
                // deterministic zero. Byte writes still consult private
                // latches through `custom_byte_write_latch`.
                0
            }
        }
    }

    pub(super) fn pot_pins(&self) -> PotPins {
        PotPins {
            left_x_released: !self.input.mmb_port1,
            left_y_released: !self.input.rmb_port1,
            right_x_released: !self.input.mmb_port2,
            right_y_released: !self.input.rmb_port2,
        }
    }

    pub(super) fn custom_byte_write_latch(&self, off: u16) -> Option<u16> {
        match off & 0xFFE {
            0x02E => Some(self.agnus.copcon),
            0x032 => Some(self.paula.serper),
            0x034 => Some(self.paula.potgo),
            0x098 => Some(self.denise.clxcon),
            0x040 => Some(self.blitter.bltcon0),
            0x042 => Some(self.blitter.bltcon1),
            0x044 => Some(self.blitter.bltafwm),
            0x046 => Some(self.blitter.bltalwm),
            0x05A if self.blitter_ecs_registers_enabled() => Some(self.blitter.bltcon0 & 0x00FF),
            0x05C if self.blitter_ecs_registers_enabled() => Some(self.blitter.bltsizv),
            0x048 => Some(((self.blitter.bltcpt >> 16) & 0x001F) as u16),
            0x04A => Some((self.blitter.bltcpt & 0xFFFE) as u16),
            0x04C => Some(((self.blitter.bltbpt >> 16) & 0x001F) as u16),
            0x04E => Some((self.blitter.bltbpt & 0xFFFE) as u16),
            0x050 => Some(((self.blitter.bltapt >> 16) & 0x001F) as u16),
            0x052 => Some((self.blitter.bltapt & 0xFFFE) as u16),
            0x054 => Some(((self.blitter.bltdpt >> 16) & 0x001F) as u16),
            0x056 => Some((self.blitter.bltdpt & 0xFFFE) as u16),
            0x060 => Some(self.blitter.bltcmod as u16),
            0x062 => Some(self.blitter.bltbmod as u16),
            0x064 => Some(self.blitter.bltamod as u16),
            0x066 => Some(self.blitter.bltdmod as u16),
            0x070 => Some(self.blitter.bltcdat),
            0x072 => Some(self.blitter.bltbdat),
            0x074 => Some(self.blitter.bltadat),
            // Audio registers (AUDxLC/LEN/PER/VOL/DAT) deliberately fall
            // through to the mirrored-byte path below. On a real 68000 a
            // byte write drives the value onto both data-bus halves, and
            // Paula latches the full 16-bit word, so `move.b #v,AUDxVOL`
            // (an even address) lands the value in the volume bits 0..6
            // exactly as `move.w` would. Some music players use this to
            // set a channel's volume via the "set volume" effect (e.g.
            // Magic Pockets' title tune drops its echo voice to a low
            // volume this way); merging with the latched low byte would
            // leave the volume at its previous full value.
            0x08E => Some(self.denise.diwstrt),
            0x090 => Some(self.denise.diwstop),
            0x1E4 => self.denise_ecs_registers().then_some(self.denise.diwhigh),
            0x1C0 if self.blitter_ecs_registers_enabled() => Some(self.agnus.htotal()),
            0x1DC if self.blitter_ecs_registers_enabled() => Some(self.agnus.beamcon0()),
            0x1C2 if self.blitter_ecs_registers_enabled() => Some(self.agnus.hsstop()),
            0x1C4 if self.blitter_ecs_registers_enabled() => Some(self.agnus.hbstrt()),
            0x1C6 if self.blitter_ecs_registers_enabled() => Some(self.agnus.hbstop()),
            0x1C8 if self.blitter_ecs_registers_enabled() => Some(self.agnus.vtotal()),
            0x1CA if self.blitter_ecs_registers_enabled() => Some(self.agnus.vsstop()),
            0x1CC if self.blitter_ecs_registers_enabled() => Some(self.agnus.vbstrt()),
            0x1CE if self.blitter_ecs_registers_enabled() => Some(self.agnus.vbstop()),
            0x1DE if self.blitter_ecs_registers_enabled() => Some(self.agnus.hsstrt()),
            0x1E0 if self.blitter_ecs_registers_enabled() => Some(self.agnus.vsstrt()),
            0x1E2 if self.blitter_ecs_registers_enabled() => Some(self.agnus.hcenter()),
            0x078 if self.blitter_ecs_registers_enabled() => Some(self.agnus.sprhdat()),
            0x1D8 if self.blitter_ecs_registers_enabled() => Some(self.agnus.hhpos()),
            0x092 => Some(self.denise.ddfstrt),
            0x094 => Some(self.denise.ddfstop),
            0x100 => Some(self.denise.bplcon0),
            0x102 => Some(self.denise.bplcon1),
            0x104 => Some(self.denise.bplcon2),
            0x106 => Some(self.denise.bplcon3),
            0x10C if self.denise_is_lisa() => Some(self.denise.bplcon4),
            0x10E if self.denise_is_lisa() => Some(self.denise.clxcon2),
            0x1FC if matches!(self.agnus.revision(), AgnusRevision::AgaAlice) => {
                Some(self.agnus.fmode())
            }
            0x108 => Some(self.denise.bpl1mod as u16),
            0x10A => Some(self.denise.bpl2mod as u16),
            off @ 0x110..=0x11E => {
                let idx = ((off - 0x110) / 2) as usize;
                let max = if self.aga_enabled() { 8 } else { 6 };
                (idx < max).then_some(self.denise.bpldat[idx])
            }
            off @ 0x0E0..=0x0FF => {
                let idx = ((off - 0x0E0) / 4) as usize;
                let max = if self.aga_enabled() { 8 } else { 6 };
                (idx < max).then(|| {
                    if off & 2 == 0 {
                        ((self.denise.bplpt[idx] >> 16) & 0x001F) as u16
                    } else {
                        (self.denise.bplpt[idx] & 0xFFFE) as u16
                    }
                })
            }
            off @ 0x120..=0x13F => {
                let idx = ((off - 0x120) / 4) as usize;
                (idx < 8).then(|| {
                    if off & 2 == 0 {
                        ((self.denise.sprpt[idx] >> 16) & 0x001F) as u16
                    } else {
                        (self.denise.sprpt[idx] & 0xFFFE) as u16
                    }
                })
            }
            off @ 0x140..=0x17F => {
                let idx = ((off - 0x140) / 8) as usize;
                if idx >= 8 {
                    return None;
                }
                match (off - 0x140) & 0x0006 {
                    0x0 => Some(self.denise.sprpos[idx]),
                    0x2 => Some(self.denise.sprctl[idx]),
                    0x4 => Some(self.denise.sprdata[idx]),
                    0x6 => Some(self.denise.sprdatb[idx]),
                    _ => None,
                }
            }
            off @ 0x180..=0x1BE => {
                let idx = ((off - 0x180) / 2) as usize;
                (idx < 32).then_some(self.denise.palette[idx])
            }
            _ => None,
        }
    }

    /// Returns true if the write asserted a new INTREQ bit (caller
    /// should preempt the slice to deliver the IRQ promptly).
    pub(super) fn write_custom_word_from(
        &mut self,
        off: u16,
        val: u16,
        source: BeamWriteSource,
    ) -> bool {
        let off = off & 0xFFE;
        // Debugger-window register watch: record the first watched write
        // until the debugger polls it. The CpuCopperIrq attribution is a
        // render-pipeline nuance; the writer is the CPU.
        if !self.ui_reg_watches.is_empty()
            && self.ui_reg_hit.is_none()
            && self.ui_reg_watches.contains(&(off & 0x1FE))
        {
            self.ui_reg_hit = Some(UiRegHit {
                off: off & 0x1FE,
                value: val,
                source: match source {
                    BeamWriteSource::Copper => "copper",
                    BeamWriteSource::Cpu | BeamWriteSource::CpuCopperIrq => "cpu",
                },
                vpos: self.agnus.vpos as u16,
                hpos: self.agnus.hpos as u16,
            });
        }
        if is_audio_timing_custom_write(off) {
            self.flush_audio();
        }
        if is_render_relevant_custom_write(off)
            && !matches!(off, 0x180..=0x1BE)
            && (off != 0x1E4 || self.denise_ecs_registers())
            && (off != 0x106 || self.bplcon3_write_enabled())
            && (!matches!(off, 0x0F8..=0x0FF | 0x11C | 0x11E) || self.aga_enabled())
            && (!matches!(off, 0x10C | 0x10E) || self.denise_is_lisa())
            && (off != 0x1FC || self.aga_enabled())
        {
            self.record_render_write(off, val, source);
        }

        match off {
            0x038 | 0x03A | 0x03C | 0x03E => {
                // STREQU/STRVBL/STRHOR/STRLONG are Denise sync strobes.
                // Copperline's current video path derives sync/blanking from
                // the configured beam standard, so accepting these writes
                // as explicit no-ops documents them as outside the current
                // OCS-visible model.
                false
            }
            0x02A => {
                self.agnus.write_vposw(val);
                false
            }
            0x02C => {
                self.agnus.write_vhposw(val);
                false
            }
            0x02E => {
                if !matches!(source, BeamWriteSource::Copper)
                    || !matches!(self.agnus.revision(), AgnusRevision::Ocs)
                {
                    self.agnus.write_copcon(val);
                }
                false
            }
            0x030 => {
                let irq = self.paula.write_serdat(val);
                self.paula.intreq |= irq;
                if irq != 0 {
                    self.note_irq_source_asserted();
                }
                irq & self.paula.intena != 0
            }
            0x032 => {
                self.paula.serper = val;
                false
            }
            0x034 => {
                self.paula.write_potgo(val);
                false
            }
            0x036 => {
                self.input.write_joytest(val);
                false
            }
            0x020 => {
                self.floppy.set_dskpt_high(val);
                false
            }
            0x022 => {
                self.floppy.set_dskpt_low(val);
                false
            }
            0x024 => {
                // Debug: COPPERLINE_DBG_DSKLEN logs each DSKLEN write (disk read/write
                // arming) with its word count and beam position, to correlate disk
                // activity with scene/animation timing.
                if crate::envcfg::flag("COPPERLINE_DBG_DSKLEN") {
                    log::info!(
                        "dsklen f={} secs={:.4} v={} dskpt={:#08X} write={:#06X} dma={} wr={} words={}",
                        self.emulated_frames,
                        self.emulated_seconds(),
                        self.agnus.vpos,
                        self.floppy.dskpt(),
                        val,
                        (val >> 15) & 1,
                        (val >> 14) & 1,
                        val & 0x3FFF,
                    );
                }
                if self.floppy.write_dsklen(val, self.paula.adkcon) {
                    self.paula.intreq |= crate::chipset::paula::INT_DSKBLK;
                    self.note_irq_source_asserted();
                    return self.paula.intena & crate::chipset::paula::INT_DSKBLK != 0;
                }
                false
            }
            0x026 => {
                self.floppy.write_dskdat(val);
                false
            }
            0x08E => {
                if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") {
                    log::info!(
                        "disp f={} v={} h={} DIWSTRT={:#06X}",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        val
                    );
                }
                self.denise.diwstrt = val;
                self.ocs_same_line_diw_start_blocked_vpos = None;
                // ECS DIWHIGH only supplies the window MSBs when it is written
                // *after* DIWSTRT/DIWSTOP (HRM p.306). A later DIWSTRT/DIWSTOP
                // write reverts to implicit (OCS-complement) MSB decoding until
                // DIWHIGH is rewritten. Without this, a stale DIWHIGH left by
                // a previous ECS display shrinks the vertical window of an OCS
                // program booted afterwards (it sets DIWSTRT/DIWSTOP but never
                // DIWHIGH), so no bitplane DMA falls inside the window and the
                // display goes black.
                self.denise.diwhigh_written = false;
                if matches!(self.agnus.revision(), AgnusRevision::Ocs)
                    && !self.current_frame_display_snapshot_taken
                    && !display_window_unprogrammed(self.denise.diwstrt, self.denise.diwstop)
                    && u32::from(diw_v_start(self.denise.diwstrt, self.effective_diwhigh()))
                        == self.agnus.vpos
                {
                    self.ocs_same_line_diw_start_blocked_vpos = Some(self.agnus.vpos);
                }
                self.capture_same_line_display_start_if_due();
                false
            }
            0x090 => {
                if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") {
                    log::info!(
                        "disp f={} v={} h={} DIWSTOP={:#06X}",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        val
                    );
                }
                self.denise.diwstop = val;
                self.denise.diwhigh_written = false;
                if self.ocs_same_line_diw_start_blocked_vpos == Some(self.agnus.vpos)
                    && !display_window_contains_vpos(
                        self.denise.diwstrt,
                        self.denise.diwstop,
                        self.effective_diwhigh(),
                        self.agnus.vpos,
                    )
                {
                    self.ocs_same_line_diw_start_blocked_vpos = None;
                }
                self.capture_same_line_display_start_if_due();
                false
            }
            0x098 => {
                self.denise.clxcon = val;
                // AGA: a CLXCON write resets CLXCON2 (planes 7-8 collision
                // control returns to disabled).
                if self.denise_is_lisa() {
                    self.denise.clxcon2 = 0;
                }
                false
            }
            0x092 => {
                if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") {
                    log::info!(
                        "disp f={} v={} h={} DDFSTRT={:#06X}",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        val
                    );
                }
                self.denise.ddfstrt = val;
                self.record_ddfstrt_write_match_miss(val);
                false
            }
            0x094 => {
                if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") {
                    log::info!(
                        "disp f={} v={} h={} DDFSTOP={:#06X}",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        val
                    );
                }
                self.denise.ddfstop = val;
                false
            }
            0x080 => {
                self.agnus.set_cop1lc_high(val);
                false
            }
            0x082 => {
                self.agnus.set_cop1lc_low(val);
                false
            }
            0x084 => {
                self.agnus.set_cop2lc_high(val);
                false
            }
            0x086 => {
                self.agnus.set_cop2lc_low(val);
                false
            }
            0x088 => {
                self.pending_copper_frame_start = None;
                self.copper.jump(self.agnus.cop1lc);
                false
            }
            0x08A => {
                self.pending_copper_frame_start = None;
                self.copper.jump(self.agnus.cop2lc);
                false
            }
            0x096 => {
                let previous = self.effective_bitplane_dmacon();
                self.agnus.write_dmacon(val);
                if crate::envcfg::flag("COPPERLINE_DIAG_AUDIO_NOTES")
                    && (val & 0x000F != 0 || self.agnus.dmacon & 0x000F != previous & 0x000F)
                {
                    log::info!(
                        "audio-ctl frame={} DMACON write={:#06X} -> {:#06X}",
                        self.emulated_frames,
                        val,
                        self.agnus.dmacon
                    );
                }
                if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") && self.agnus.dmacon != previous {
                    log::info!(
                        "disp f={} v={} h={} DMACON write={:#06X} -> dmacon={:#06X} (AUD={:X} SPR={} DSK={} BLT={})",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        val,
                        self.agnus.dmacon,
                        self.agnus.dmacon & 0x000F,
                        (self.agnus.dmacon >> 5) & 1,
                        (self.agnus.dmacon >> 4) & 1,
                        (self.agnus.dmacon >> 6) & 1,
                    );
                }
                if self.agnus.dmacon != previous {
                    self.record_bitplane_dmacon_write(previous);
                }
                false
            }
            // Blitter $040-$074. BLTSIZE ($058) triggers the blit and
            // starts scheduled DMA. The CPU slice is stopped after the
            // write instruction retires so Agnus can grant early blitter
            // slots before ROM-only code reuses chip-memory sources.
            0x040 => {
                self.blitter.write_bltcon0(val);
                false
            }
            0x042 => {
                let val = if self.blitter_ecs_registers_enabled() {
                    val
                } else {
                    val & !BLTCON1_DOFF
                };
                self.blitter.write_bltcon1(val);
                false
            }
            0x044 => {
                self.finish_pending_blitter();
                self.blitter.bltafwm = val;
                false
            }
            0x046 => {
                self.finish_pending_blitter();
                self.blitter.bltalwm = val;
                false
            }
            0x048 => {
                self.finish_pending_blitter();
                self.blitter.set_cpt_high(val);
                false
            }
            0x04A => {
                self.finish_pending_blitter();
                self.blitter.set_cpt_low(val);
                false
            }
            0x04C => {
                self.finish_pending_blitter();
                self.blitter.set_bpt_high(val);
                false
            }
            0x04E => {
                self.finish_pending_blitter();
                self.blitter.set_bpt_low(val);
                false
            }
            0x050 => {
                self.finish_pending_blitter();
                self.blitter.set_apt_high(val);
                false
            }
            0x052 => {
                self.finish_pending_blitter();
                self.blitter.set_apt_low(val);
                false
            }
            0x054 => {
                self.finish_pending_blitter();
                self.blitter.set_dpt_high(val);
                false
            }
            0x056 => {
                self.finish_pending_blitter();
                self.blitter.set_dpt_low(val);
                false
            }
            0x058 => {
                self.finish_pending_blitter();
                // Starting a new blit consumes a stale pending blitter-done
                // interrupt request: INTREQ.BLIT reflects "the last started blit
                // has finished", so a BLTSIZE write while the request is still
                // set (never acknowledged) clears it, and the interrupt for this
                // blit fires only when it actually completes.
                //
                // A BLTSIZE write can follow a long run of polling-only blits
                // with INTREQ.BLIT still stale-set. Enabling INTENA.BLIT for the
                // new blit must not take an immediate interrupt before software
                // patches its handler operands; on real hardware the new
                // BLTSIZE consumes the stale request and the next request is
                // raised only when the new blit completes. This is also
                // consistent with how the OS blitter queue manages the interrupt
                // via INTENA rather than relying on INTREQ acks while the blitter
                // is idle.
                self.paula.intreq &= !crate::chipset::paula::INT_BLIT;
                self.note_irq_latches_changed();
                // A blit over the exception-vector area is useful crash context:
                // flag it so the CPU wrapper can dump the instruction history.
                if self.blitter.bltcon0 & 0x0100 != 0 && self.blitter.bltdpt < 0x1000 {
                    self.diag_lowmem_blit = true;
                }
                self.diag_blit_start(((val as u32) >> 6) & 0x3FF, (val as u32) & 0x3F);
                // COPPERLINE_DBG_BLIT="<lo_secs>:<hi_secs>": log each blit's key
                // parameters (control words, D pointer/modulo, size, mode flags)
                // with its beam position, within the time window. For
                // investigating which blit produces a given rendered region.
                if let Some(spec) = crate::envcfg::var("COPPERLINE_DBG_BLIT") {
                    let mut parts = spec.split(':');
                    let lo: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                    let hi: f64 = parts
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(f64::MAX);
                    let secs = self.emulated_seconds();
                    if (lo..hi).contains(&secs) {
                        let h = (val >> 6) & 0x3FF;
                        let w = val & 0x3F;
                        let c1 = self.blitter.bltcon1;
                        log::info!(
                            "blit t={secs:.5} f={} v={} h={} bltcon0={:#06X} bltcon1={:#06X} \
                             dpt={:#08X} dmod={} size={}x{} {}{}{}",
                            self.emulated_frames,
                            self.agnus.vpos,
                            self.agnus.hpos,
                            self.blitter.bltcon0,
                            c1,
                            self.blitter.bltdpt,
                            self.blitter.bltdmod,
                            h,
                            w,
                            if c1 & 0x0001 != 0 { "LINE " } else { "" },
                            if c1 & 0x0008 != 0 { "FILL " } else { "" },
                            if c1 & 0x0002 != 0 { "DESC " } else { "" },
                        );
                    }
                }
                self.trace_blitter_start(val, source);
                {
                    // BLTSIZE zero fields mean the maximum (1024 rows / 64 words).
                    let h = ((val as u32) >> 6) & 0x3FF;
                    let w = (val as u32) & 0x3F;
                    self.record_frame_blit_start(
                        if h == 0 { 1024 } else { h },
                        if w == 0 { 64 } else { w },
                    );
                }
                self.blitter.start_scheduled(val, &self.mem.chip_ram);
                self.record_blit_accounting();
                self.slice_preempted = true;
                false
            }
            0x05A => {
                if !self.blitter_ecs_registers_enabled() {
                    return false;
                }
                self.finish_pending_blitter();
                self.blitter.bltcon0 = (self.blitter.bltcon0 & 0xFF00) | (val & 0x00FF);
                false
            }
            0x05C => {
                if !self.blitter_ecs_registers_enabled() {
                    return false;
                }
                self.finish_pending_blitter();
                self.blitter.bltsizv = val & 0x7FFF;
                false
            }
            0x05E => {
                if !self.blitter_ecs_registers_enabled() {
                    return false;
                }
                self.finish_pending_blitter();
                // Same as BLTSIZE: starting a new blit consumes a stale pending
                // blitter-done interrupt request.
                self.paula.intreq &= !crate::chipset::paula::INT_BLIT;
                self.note_irq_latches_changed();
                self.trace_blitter_start_ecs(val, source);
                self.diag_blit_start(u32::from(self.blitter.bltsizv), (val as u32) & 0x07FF);
                {
                    let h = u32::from(self.blitter.bltsizv);
                    let w = (val as u32) & 0x07FF;
                    self.record_frame_blit_start(
                        if h == 0 { 0x8000 } else { h },
                        if w == 0 { 0x800 } else { w },
                    );
                }
                self.blitter.start_scheduled_ecs(val, &self.mem.chip_ram);
                self.record_blit_accounting();
                self.slice_preempted = true;
                false
            }
            0x060 => {
                self.finish_pending_blitter();
                self.blitter.bltcmod = (val & 0xFFFE) as i16;
                false
            }
            0x062 => {
                self.finish_pending_blitter();
                self.blitter.bltbmod = (val & 0xFFFE) as i16;
                false
            }
            0x064 => {
                self.finish_pending_blitter();
                self.blitter.bltamod = (val & 0xFFFE) as i16;
                false
            }
            0x066 => {
                self.finish_pending_blitter();
                self.blitter.bltdmod = (val & 0xFFFE) as i16;
                false
            }
            0x070 => {
                self.blitter.write_bltcdat(val);
                false
            }
            0x072 => {
                self.blitter.write_bltbdat(val);
                false
            }
            0x074 => {
                self.blitter.write_bltadat(val);
                false
            }
            0x07E => {
                if self.floppy.write_dsksync(val) {
                    self.paula.intreq |= INT_DSKSYNC;
                    self.note_irq_source_asserted();
                    return self.paula.intena & INT_DSKSYNC != 0;
                }
                false
            }
            0x09A => {
                // COPPERLINE_DBG_CIA: log INTENA writes touching EXTER or the
                // master enable, to order them against CIA-B TOD-alarm latches
                // (this ordering identified the 9 Fingers TOD-alarm guru).
                if dbg_cia_on() && val & (INT_EXTER | 0x4000) != 0 {
                    log::info!(
                        "INTENA write {:#06X} from {:?} secs={:.5} (intena was {:#06X}, intreq {:#06X})",
                        val,
                        source,
                        self.emulated_seconds(),
                        self.paula.intena,
                        self.paula.intreq,
                    );
                }
                self.paula.write_intena(val);
                self.note_irq_latches_changed();
                false
            }
            0x09C => {
                // COPPERLINE_DBG_CIA: same for INTREQ writes touching EXTER.
                if dbg_cia_on() && val & INT_EXTER != 0 {
                    log::info!(
                        "INTREQ write {:#06X} from {:?} secs={:.5} (intreq was {:#06X})",
                        val,
                        source,
                        self.emulated_seconds(),
                        self.paula.intreq,
                    );
                }
                // Crash diagnostics: log INTREQ writes that touch the blitter bit,
                // to identify which code set or acknowledged the request.
                if crate::envcfg::flag("COPPERLINE_DIAG_CRASH")
                    && val & crate::chipset::paula::INT_BLIT != 0
                {
                    log::warn!(
                        "INTREQ write {:#06X} from {:?} at t={:.4} (intreq was {:#06X})",
                        val,
                        source,
                        self.emulated_seconds(),
                        self.paula.intreq,
                    );
                }
                let coper_was_pending = self.paula.intreq & INT_COPER != 0;
                let asserted = self.paula.write_intreq(val);
                let copper_asserted = matches!(source, BeamWriteSource::Copper)
                    && val & 0x8000 != 0
                    && val & INT_COPER != 0
                    && !coper_was_pending;
                if copper_asserted {
                    self.pending_copper_irq_beam = Some((self.agnus.vpos, self.agnus.hpos));
                    self.coper_cpu_irq_delay_cck = COPER_CPU_IRQ_DELAY_CCK;
                }
                if copper_asserted && asserted {
                    self.note_irq_source_asserted();
                } else {
                    self.note_irq_latches_changed();
                }
                if val & 0x8000 == 0 && val & INT_COPER != 0 {
                    self.pending_copper_irq_beam = None;
                    self.coper_cpu_irq_delay_cck = 0;
                }
                if matches!(source, BeamWriteSource::Cpu) {
                    self.note_intreq_palette_target(val);
                }
                asserted
            }
            0x09E => {
                if crate::envcfg::flag("COPPERLINE_DIAG_AUDIO_NOTES") {
                    log::info!(
                        "audio-ctl frame={} ADKCON write={:#06X}",
                        self.emulated_frames,
                        val
                    );
                }
                self.paula.write_adkcon(val);
                false
            }
            0x1C0 => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_htotal(val);
                    self.refresh_paula_audio_min_period();
                }
                false
            }
            0x1DC => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_beamcon0(val);
                    self.refresh_paula_audio_min_period();
                    if val & BEAMCON0_DUAL != 0 && !self.uhres_dual_warned {
                        log::warn!(
                            "BEAMCON0 DUAL set: A2024/Productivity (UHRES dual-monitor) display is not emulated"
                        );
                        self.uhres_dual_warned = true;
                    }
                }
                false
            }
            // ECS Agnus programmable sync/blank latches + UHRES SPRHDAT.
            // Stored only; no scan-rate geometry derived yet (TODO section 9).
            0x1C2 => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_hsstop(val);
                }
                false
            }
            0x1C4 => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_hbstrt(val);
                }
                false
            }
            0x1C6 => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_hbstop(val);
                }
                false
            }
            0x1C8 => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_vtotal(val);
                }
                false
            }
            0x1CA => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_vsstop(val);
                }
                false
            }
            0x1CC => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_vbstrt(val);
                }
                false
            }
            0x1CE => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_vbstop(val);
                }
                false
            }
            0x1DE => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_hsstrt(val);
                }
                false
            }
            0x1E0 => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_vsstrt(val);
                }
                false
            }
            0x1E2 => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_hcenter(val);
                }
                false
            }
            0x078 => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_sprhdat(val);
                }
                false
            }
            0x1D8 => {
                if self.blitter_ecs_registers_enabled() {
                    self.agnus.write_hhposw(val);
                }
                false
            }
            // Audio block: AUD0..AUD3, 16 bytes per channel.
            off @ 0x0A0..=0x0DF => {
                // Timestamp audio register writes (AUDxLC/LEN/PER/VOL) by
                // emulated frame to trace note triggers and buffer chaining.
                if crate::envcfg::flag("COPPERLINE_DIAG_AUDIO_NOTES") {
                    let lane = (off - 0x0A0) & 0x0F;
                    if matches!(lane, 0x0 | 0x2 | 0x4 | 0x6 | 0x8) {
                        let ch = (off - 0x0A0) / 0x10;
                        let kind = match lane {
                            0x0 => "LCH",
                            0x2 => "LCL",
                            0x4 => "LEN",
                            0x6 => "PER",
                            _ => "VOL",
                        };
                        log::info!(
                            "audio-note frame={} ch={} {}={}",
                            self.emulated_frames,
                            ch,
                            kind,
                            val
                        );
                    }
                }
                self.paula.write_audio_reg(off - 0x0A0, val);
                false
            }
            0x100 => {
                // ERSY/LPEN/COLOR/GAUD side effects are external sync,
                // light-pen, colorburst, and genlock-audio controls. The
                // current renderer keeps the bits for mode/register replay
                // but intentionally does not model those board-level pins.
                let previous = self.effective_bitplane_bplcon0();
                if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") && val != previous {
                    let bpu = (val >> 12) & 0x7;
                    let hires = (val >> 15) & 1;
                    log::info!(
                        "disp f={} v={} h={} BPLCON0={:#06X} bpu={} hires={}",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        val,
                        bpu,
                        hires
                    );
                }
                self.denise.bplcon0 = val;
                // Agnus snoops BPLCON0.LPEN (bit 3) for the light-pen beam
                // latch; the comment above still holds for the board pins.
                self.agnus.set_lpen(val & 0x0008 != 0);
                // BPLCON0.ERSY (bit 1) with no genlock attached stops the
                // beam counters; the boot ROM genlock probe depends on
                // the VPOSR/VHPOSR readback freezing while it is set.
                self.agnus.set_ersy(val & 0x0002 != 0);
                if self.denise.bplcon0 != previous {
                    self.record_bitplane_bplcon0_write(previous);
                }
                false
            }
            0x102 => {
                self.denise.bplcon1 = val;
                false
            }
            0x104 => {
                self.denise.bplcon2 = val;
                false
            }
            0x106 => {
                // ECS Denise only latches BPLCON3 while BPLCON0 bit 0
                // (ENBPLCN3/ECSENA) is set; OCS Denise has no BPLCON3.
                if self.bplcon3_write_enabled() {
                    self.denise.bplcon3 = val;
                } else if !self.bplcon3_drop_warned && val != 0 {
                    self.bplcon3_drop_warned = true;
                    log::info!(
                        "BPLCON3 write {val:#06X} dropped ({}); further drops not logged",
                        if self.denise_ecs_registers() {
                            "ENBPLCN3 clear"
                        } else {
                            "OCS Denise"
                        }
                    );
                }
                false
            }
            // AGA Lisa registers: latched only until the AGA display path
            // lands (plan 3.3/3.4); unreachable from config today because
            // Lisa is not selectable.
            0x10C => {
                if self.denise_is_lisa() {
                    if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") && self.denise.bplcon4 != val
                    {
                        log::info!(
                            "disp f={} v={} h={} BPLCON4={:#06X}",
                            self.emulated_frames,
                            self.agnus.vpos,
                            self.agnus.hpos,
                            val
                        );
                    }
                    self.denise.bplcon4 = val;
                }
                false
            }
            0x10E => {
                if self.denise_is_lisa() {
                    self.denise.clxcon2 = val & 0x0FFF;
                }
                false
            }
            // AGA Alice FMODE (write_fmode gates on the revision itself).
            0x1FC => {
                self.agnus.write_fmode(val);
                if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") {
                    log::info!(
                        "disp f={} v={} h={} FMODE={:#06X}",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        self.agnus.fmode()
                    );
                }
                false
            }
            0x108 => {
                self.denise.bpl1mod = (val & 0xFFFE) as i16;
                if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") {
                    log::info!(
                        "disp f={} v={} h={} BPL1MOD={}",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        self.denise.bpl1mod
                    );
                }
                false
            }
            0x10A => {
                self.denise.bpl2mod = (val & 0xFFFE) as i16;
                if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") {
                    log::info!(
                        "disp f={} v={} h={} BPL2MOD={}",
                        self.emulated_frames,
                        self.agnus.vpos,
                        self.agnus.hpos,
                        self.denise.bpl2mod
                    );
                }
                false
            }
            off @ 0x110..=0x11E => {
                let idx = ((off - 0x110) / 2) as usize;
                if idx < if self.aga_enabled() { 8 } else { 6 } {
                    if idx == 0 {
                        self.record_sprite_display_enable_at(self.agnus.vpos, self.agnus.hpos);
                    }
                    self.denise.write_bpldat(idx, val);
                }
                false
            }
            // Sprite pointers: SPR0PTH/PTL .. SPR7PTH/PTL.
            off @ 0x120..=0x13F => {
                let idx = ((off - 0x120) / 4) as usize;
                if idx < 8 {
                    if off & 2 == 0 {
                        self.denise.set_sprpt_high(idx, val);
                    } else {
                        self.denise.set_sprpt_low(idx, val);
                    }
                    if diag_sprcap().is_some() && off & 2 != 0 {
                        log::info!(
                            "sprptw f={} v={} h={} s{idx} = {:06X}",
                            self.emulated_frames,
                            self.agnus.vpos,
                            self.agnus.hpos,
                            self.denise.sprpt[idx]
                        );
                    }
                    self.display_dma_sprpt[idx] = self.denise.sprpt[idx];
                    if off & 2 != 0 {
                        self.apply_display_sprite_pointer_low_write(idx);
                    }
                }
                false
            }
            // Sprite position/control/data registers:
            // SPRxPOS, SPRxCTL, SPRxDATA, SPRxDATB.
            off @ 0x140..=0x17F => {
                let idx = ((off - 0x140) / 8) as usize;
                let reg = (off - 0x140) & 0x0006;
                if idx < 8 {
                    match reg {
                        0x0 => {
                            self.denise.sprpos[idx] = val;
                            self.latch_display_sprite_dma_control_from_registers(
                                idx,
                                SpriteControlRegisterWrite::Pos,
                            );
                        }
                        0x2 => {
                            self.denise.write_sprctl(idx, val);
                            self.latch_display_sprite_dma_control_from_registers(
                                idx,
                                SpriteControlRegisterWrite::Ctl,
                            );
                        }
                        0x4 => self.denise.write_sprdata(idx, val),
                        0x6 => self.denise.write_sprdatb(idx, val),
                        _ => {}
                    }
                }
                false
            }
            0x1E4 => {
                if self.denise_ecs_registers() {
                    self.denise.diwhigh = val;
                    self.denise.diwhigh_written = true;
                }
                false
            }
            // Bitplane pointers: $0E0..$0F4 high, +2 low; BPL7/BPL8 at
            // $0F8/$0FC exist on AGA Alice only.
            off @ 0x0E0..=0x0FF => {
                let idx = ((off - 0x0E0) / 4) as usize;
                if idx < if self.aga_enabled() { 8 } else { 6 } {
                    if off & 2 == 0 {
                        self.denise.set_bplpt_high(idx, val);
                    } else {
                        self.denise.set_bplpt_low(idx, val);
                    }
                    self.display_dma_bplpt[idx] = self.denise.bplpt[idx];
                    if off & 2 != 0 && crate::envcfg::flag("COPPERLINE_DIAG_BPLPT") {
                        log::info!(
                            "bplpt f={} v={} h={} src={} BPL{}PT={:#08X} cop1lc={:#08X} coppc={:#08X}",
                            self.emulated_frames,
                            self.agnus.vpos,
                            self.agnus.hpos,
                            beam_write_source_name(source),
                            idx + 1,
                            self.denise.bplpt[idx],
                            self.agnus.cop1lc,
                            self.copper.pc(),
                        );
                    }
                }
                false
            }
            // Color palette $180..$1BE in pairs of two bytes.
            off @ 0x180..=0x1BE => {
                let idx = ((off - 0x180) / 2) as usize;
                if idx < 32 {
                    let color = color_register_value(val);
                    let render_source = if matches!(source, BeamWriteSource::Cpu)
                        && matches!(self.cpu_palette_target, CpuPaletteTarget::Bottom)
                    {
                        BeamWriteSource::CpuCopperIrq
                    } else {
                        source
                    };
                    self.record_render_write(off, val, render_source);
                    if self.denise_is_lisa() {
                        // AGA: BPLCON3 BANK/LOCT route the write into the
                        // 256-entry store. Bank 0 with LOCT clear is the
                        // OCS-compatible case. The replay renderer still
                        // tracks bank 0 only until plan 3.3 lands.
                        self.denise.palette.write_banked(
                            crate::chipset::denise::Palette::bank_from_bplcon3(self.denise.bplcon3),
                            idx,
                            crate::chipset::denise::Palette::loct_from_bplcon3(self.denise.bplcon3),
                            color,
                        );
                    } else {
                        self.denise.palette.write_ocs(idx, color);
                    }
                    if matches!(source, BeamWriteSource::Cpu) {
                        self.write_cpu_palette_snapshot(idx, color);
                    }
                }
                false
            }
            _ => false,
        }
    }
}
