// SPDX-License-Identifier: GPL-3.0-or-later

//! Frame capture: per-frame beam bookkeeping (begin_new_beam_frame),
//! sprite/bitplane DMA word capture, render-event recording, and the
//! palette snapshots the renderer replays by beam position. Split out
//! of `bus.rs` for size; this is the same `Bus`, with full access to
//! its private state.

use super::*;

impl Bus {
    pub(super) fn begin_new_beam_frame(&mut self) {
        self.diag_log_frame_start();
        // Only maintain the collision latch across the frame boundary once
        // software has shown it reads CLXDAT. Until then the full-frame scan is
        // unobservable work; see `collision_tracking_active`.
        if self.collision_tracking_active {
            self.accumulate_live_collisions_to_frame_end();
        }
        if let Some(uaelib) = self.uaelib.as_mut() {
            uaelib.note_frame_start(self.emulated_cck);
        }
        self.log_bus_accounting_frame();
        self.finish_frame_bus_trace();
        let promote_render_frame = !self.current_frame_render_blocked;
        if promote_render_frame {
            self.last_frame_render_base = Some(self.current_frame_render_base);
            self.last_frame_visible_start_vpos = self.current_frame_visible_start_vpos;
            self.last_frame_geometry = self.current_frame_geometry;
            self.last_frame_presentation_h_window = self.current_frame_presentation_h_window;
            self.last_frame_presentation_v_window = self.current_frame_presentation_v_window;
            self.last_frame_render_events = std::mem::take(&mut self.current_frame_render_events);
        } else {
            self.last_frame_render_base = None;
            self.last_frame_render_events.clear();
            self.current_frame_render_events.clear();
        }
        self.current_frame_collision_events.clear();
        self.current_frame_collision_control_events.clear();
        self.current_frame_collision_bpldat_events.clear();
        self.current_frame_collision_sprite_events.clear();
        self.current_frame_collision_control_index = None;
        self.current_frame_collision_bpldat_index = None;
        self.current_frame_collision_sprite_index = None;
        if promote_render_frame {
            self.last_frame_chip_ram_writes =
                std::mem::take(&mut self.current_frame_chip_ram_writes);
            self.last_frame_beam_top_palette = self.current_frame_beam_top_palette;
            self.last_frame_beam_top_palette_end = self.beam_top_palette;
            self.last_frame_beam_bottom_palette = self.beam_bottom_palette;
            self.last_frame_beam_bottom_palette_valid = self.beam_bottom_palette_valid;
            self.last_frame_beam_bottom_palette_events = self.beam_bottom_palette_events.clone();
        } else {
            self.last_frame_chip_ram_writes.clear();
            self.current_frame_chip_ram_writes.clear();
            self.last_frame_beam_bottom_palette_events.clear();
        }
        // Promote the just-finished frame's chip-RAM snapshot to immutable
        // shared ownership without copying 2 MB. `capture_current_frame_
        // display_start` already filled `current_frame_chip_ram` for any frame
        // that reached its display window. The old shared buffer becomes the
        // next mutable capture buffer once the renderer has released it. A
        // frame that never displayed has no meaningful snapshot, so fall back
        // to a live copy for the renderer's blank/border output.
        let completed_chip_ram = if promote_render_frame
            && self.current_frame_display_snapshot_taken
            && self.current_frame_chip_ram.len() == self.mem.chip_ram.len()
        {
            std::mem::take(&mut self.current_frame_chip_ram)
        } else if promote_render_frame {
            self.mem.chip_ram.clone()
        } else {
            Vec::new()
        };
        let old_chip_ram = std::mem::replace(
            &mut self.last_frame_chip_ram,
            std::sync::Arc::new(completed_chip_ram),
        );
        // The renderer normally drops its shared reference before the next
        // frame wrap, letting the old allocation return to the capture side.
        // If it is still in flight, correctness wins: start with a fresh Vec.
        self.current_frame_chip_ram = std::sync::Arc::try_unwrap(old_chip_ram).unwrap_or_default();
        let current_bitplane_rows = std::mem::replace(
            &mut self.current_frame_bitplane_rows,
            empty_captured_bitplane_rows(),
        );
        let next_bitplane_rows = if promote_render_frame {
            std::sync::Arc::new(current_bitplane_rows)
        } else {
            self.recycle_captured_bitplane_rows(current_bitplane_rows);
            std::sync::Arc::new(empty_captured_bitplane_rows())
        };
        let old_bitplane_rows =
            std::mem::replace(&mut self.last_frame_bitplane_rows, next_bitplane_rows);
        if let Ok(rows) = std::sync::Arc::try_unwrap(old_bitplane_rows) {
            self.recycle_captured_bitplane_rows(rows);
        }
        self.last_frame_sprite_lines = if promote_render_frame {
            std::mem::take(&mut self.current_frame_sprite_lines)
        } else {
            self.current_frame_sprite_lines.clear();
            Vec::new()
        };
        self.last_frame_held_sprites = if promote_render_frame {
            std::mem::take(&mut self.current_frame_held_sprites)
        } else {
            self.current_frame_held_sprites = [None; 8];
            [None; 8]
        };
        clear_captured_sprite_lines_by_y(&mut self.current_frame_sprite_lines_by_y);
        self.current_frame_sprite_collision_sources = empty_sprite_collision_sources();
        let current_sprite_display_enable_x_by_y = std::mem::replace(
            &mut self.current_frame_sprite_display_enable_x_by_y,
            empty_sprite_display_enable_x_by_y(),
        );
        self.last_frame_sprite_display_enable_x_by_y = if promote_render_frame {
            current_sprite_display_enable_x_by_y
        } else {
            empty_sprite_display_enable_x_by_y()
        };
        self.last_frame_sprite_dma_observed =
            promote_render_frame && self.current_frame_sprite_dma_observed;
        self.current_frame_sprite_dma_observed = false;
        // The next frame's snapshot is taken lazily at its display start
        // (`capture_current_frame_display_start`), which clears and refills
        // this buffer. Eagerly copying chip RAM here would just be overwritten,
        // so only clear it; a frame that never displays falls back to a live
        // copy at the next wrap (see the swap/extend above).
        self.current_frame_chip_ram.clear();
        self.current_frame_beam_top_palette = self.beam_top_palette;
        self.current_frame_display_snapshot_taken = false;
        self.ocs_same_line_diw_start_blocked_vpos = None;
        self.current_frame_render_blocked = false;
        self.current_frame_visible_start_vpos = RENDER_MIN_OVERSCAN_START_VPOS;
        self.current_frame_render_base = self.capture_render_snapshot();
        // Carry each sprite channel's DMA pointer across the frame boundary the
        // way real Agnus does: SPRxPT advances only on fetched words, so the
        // live pointer already sits at the channel's DMA frontier. It does NOT
        // snap back to the last value the Copper/CPU wrote into `denise.sprpt`,
        // so a reused control-word buffer that software rewrites every field is
        // not re-read from its previous, now-overwritten address before the
        // Copper reloads SPRxPT.
        self.sprite_dma_frame_start_ptr = self.display_dma_sprpt;
        self.current_frame_collision_may_have_dual_playfield =
            self.current_frame_render_base.bplcon0 & 0x0400 != 0;
        // BPLxPT carries across the field boundary the same way: real Agnus
        // never reloads bitplane pointers at vertical blank -- software
        // rewrites them (fully, or half by half relying on the DMA-advanced
        // other half) before the next display window starts. The register
        // write path merges half writes into `display_dma_bplpt` directly.
        self.display_dma_sprpt = self.denise.sprpt;
        self.reset_display_sprite_dma_states();
        self.sprite_dma_replay = SpriteDmaReplayCursor::default();
        self.display_dma_clipped_rows_advanced = false;
        self.lazy_collision_vpos = self.current_frame_visible_start_vpos;
        self.lazy_collision_hpos = RENDER_COPPER_WAIT_HPOS_FB0;
        self.agnus
            .update_interlace_long_frame(self.denise.bplcon0 & 0x0004 != 0);
        // The snapshot above was captured before the frame wrap toggled
        // LOF; record the settled value for the field about to render.
        self.current_frame_render_base.long_field = self.agnus.lof;
        // The frame's last raster line reset the vertical display flop, so
        // a window whose DIWSTOP the beam never reaches ended with its own
        // frame; line zero's comparator matches apply like any other line
        // start's.
        self.reevaluate_diw_vertical_flop();
        self.current_frame_geometry = self.compute_frame_geometry();
        self.current_frame_presentation_h_window = self.compute_presentation_h_window();
        self.current_frame_presentation_v_window = self.compute_presentation_v_window();
        if self.current_frame_geometry.programmable {
            self.current_frame_visible_start_vpos = self.current_frame_geometry.visible_start_vpos;
            self.lazy_collision_vpos = self.current_frame_visible_start_vpos;
        }
        self.pending_copper_frame_start = Some(self.agnus.cop1lc);
        self.copper.stop();
        self.reset_current_frame_bus_trace(false);
    }

    pub(crate) fn record_cpu_chip_ram_write(&mut self, offset: usize, size: usize, value: u32) {
        self.current_frame_chip_ram_writes
            .push(BeamChipRamWrite::from_cpu_write(
                self.agnus.vpos,
                self.agnus.hpos,
                offset,
                size,
                value,
            ));
    }

    pub(super) fn capture_current_frame_display_start(&mut self) {
        if self.current_frame_display_snapshot_taken {
            return;
        }
        self.lazy_collision_vpos = self.current_frame_visible_start_vpos;
        self.current_frame_chip_ram.clear();
        self.current_frame_chip_ram
            .extend_from_slice(&self.mem.chip_ram);
        self.current_frame_beam_top_palette = self.beam_top_palette;
        self.current_frame_display_snapshot_taken = true;
        if !self.current_frame_render_blocked {
            self.advance_display_dma_for_clipped_rows();
            self.advance_sprite_dma_to_display_start();
            self.capture_held_sprites_for_visible_window();
        }
    }

    /// Reset the per-frame transients of every sprite channel's DMA state.
    /// The POS/CTL register copies and the vertical comparator values are
    /// chip registers and carry across the field; the DMA flip-flop is
    /// cleared in the frame's last line, and the display latches restart
    /// with the field's replay.
    pub(super) fn reset_display_sprite_dma_states(&mut self) {
        for state in &mut self.display_dma_sprite_state {
            *state = DisplaySpriteDmaState {
                pos: state.pos,
                ctl: state.ctl,
                vstrt: state.vstrt,
                vstop: state.vstop,
                ..DisplaySpriteDmaState::default()
            };
        }
    }

    /// After the offscreen sprite-DMA replay, snapshot any sprite that has
    /// fetched data but whose DMA is now disabled (SPREN cleared): it is being
    /// "held" and will be repainted by Copper SPRxPOS repositioning across the
    /// visible window. The renderer's manual-sprite path consumes these (it can
    /// clip each repositioned segment); the bus stale-latch path is suppressed
    /// for them.
    pub(super) fn capture_held_sprites_for_visible_window(&mut self) {
        self.current_frame_held_sprites = [None; 8];
        if self.agnus.dmacon & (DMACON_DMAEN | DMACON_SPREN) == (DMACON_DMAEN | DMACON_SPREN) {
            // Sprite DMA is still active: the normal capture path handles it.
            return;
        }
        for sprite in 0..8 {
            let state = self.display_dma_sprite_state[sprite];
            let Some(line_data) = state.last_line else {
                continue;
            };
            let vstop = if state.vstop <= state.vstrt {
                self.agnus.current_frame_lines() as i32
            } else {
                state.vstop
            };
            self.current_frame_held_sprites[sprite] = Some(HeldSpriteLine {
                line: CapturedSpriteLine {
                    sprite,
                    hstart: line_data.hstart,
                    hsub_70ns: line_data.hsub_70ns,
                    beam_y: 0,
                    data: line_data.data,
                    datb: line_data.datb,
                    data_ext: line_data.data_ext,
                    datb_ext: line_data.datb_ext,
                    width_words: line_data.width_words,
                    attached: line_data.attached,
                },
                vstart: state.vstrt,
                vstop,
            });
        }
    }

    /// A CPU/Copper write to SPRxPOS or SPRxCTL pokes Agnus's channel
    /// register copies the same way a DMA control-word fetch does: POS
    /// retimes the horizontal/vertical-start bits without touching the data
    /// stream, CTL sets both vertical comparators and disarms the display
    /// latches, and the comparators fire immediately when the write lands
    /// on a matching line.
    pub(super) fn latch_display_sprite_dma_control_from_registers(
        &mut self,
        sprite: usize,
        write: SpriteControlRegisterWrite,
    ) {
        if sprite >= 8 {
            return;
        }
        self.poke_display_sprite_control_at(
            sprite,
            match write {
                SpriteControlRegisterWrite::Pos => self.denise.sprpos[sprite],
                SpriteControlRegisterWrite::Ctl => self.denise.sprctl[sprite],
            },
            write,
            self.agnus.vpos,
        );
    }

    pub(super) fn poke_display_sprite_control_at(
        &mut self,
        sprite: usize,
        value: u16,
        write: SpriteControlRegisterWrite,
        vpos: u32,
    ) {
        if sprite >= 8 {
            return;
        }
        let beam_y = vpos as i32;
        let state = &mut self.display_dma_sprite_state[sprite];
        match write {
            SpriteControlRegisterWrite::Pos => state.poke_pos(value),
            SpriteControlRegisterWrite::Ctl => state.poke_ctl(value),
        }
        // TODO: near the end of a line the hardware comparator already sees
        // the next line's counter (vAmiga applies pos.v+1 from cck $E1).
        if !self.sprite_dma_inhibited_by_vertical_blank_at(vpos) {
            self.display_dma_sprite_state[sprite].reevaluate_comparators_at(beam_y);
        }
    }

    pub(super) fn capture_same_line_display_start_if_due(&mut self) {
        if self.current_frame_display_snapshot_taken
            || matches!(self.agnus.revision(), AgnusRevision::Ocs)
            || display_window_unprogrammed(self.denise.diwstrt, self.denise.diwstop)
        {
            return;
        }
        let display_start = self.display_start_vpos_for_current_control();
        if display_start != self.agnus.vpos || !self.diw_vertical_open_at(self.agnus.vpos) {
            return;
        }
        self.capture_current_frame_display_start();
    }

    pub(super) fn advance_display_dma_for_clipped_rows(&mut self) {
        if self.display_dma_clipped_rows_advanced {
            return;
        }
        self.display_dma_clipped_rows_advanced = true;
        let visible_start = self.current_frame_visible_start_vpos;
        let rows = clipped_display_rows_before_visible(
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
            visible_start,
        );
        if rows == 0 {
            return;
        }
        // Bitplane DMA only fetched on the clipped lines where it was
        // actually enabled at the time: replay this frame's BPLCON0/DMACON
        // writes across the span rather than sampling the registers at the
        // visible start. Regression example: the CDTV extended-ROM boot
        // screen opens DIW at line 5 but raises BPLCON0 from 0 to 6 planes
        // only at line 24; advancing every clipped row ran the pointers 19
        // rows ahead, walking off the end of the image (and into the next
        // plane's data) near the bottom of the frame.
        let base = self.current_frame_render_base;
        let mut bplcon0 = base.bplcon0;
        let mut dmacon = base.dmacon;
        let first_vpos = visible_start.saturating_sub(rows as u32);
        // Writes landing before the line's hard fetch start still govern
        // that line's fetch; later ones take effect from the next line.
        let fetch_gate_hpos = u32::from(BITPLANE_DDF_HARD_START);
        let writes: Vec<(u32, u32, u16, u16)> = self
            .current_render_events()
            .iter()
            .filter(|w| matches!(w.offset, 0x096 | 0x100) && w.vpos < visible_start)
            .map(|w| (w.vpos, w.hpos, w.offset, w.value))
            .collect();
        let mut idx = 0;
        for vpos in first_vpos..visible_start {
            while idx < writes.len()
                && (writes[idx].0 < vpos
                    || (writes[idx].0 == vpos && writes[idx].1 < fetch_gate_hpos))
            {
                let (_, _, offset, value) = writes[idx];
                match offset {
                    0x096 => {
                        if value & 0x8000 != 0 {
                            dmacon |= value & 0x7FFF;
                        } else {
                            dmacon &= !value;
                        }
                    }
                    0x100 => bplcon0 = value,
                    _ => {}
                }
                idx += 1;
            }
            if dmacon & (DMACON_DMAEN | DMACON_BPLEN) != (DMACON_DMAEN | DMACON_BPLEN) {
                continue;
            }
            let nplanes =
                bitplane_dma_planes_for_fmode(bplcon0, self.agnus.fmode(), self.aga_enabled());
            if nplanes == 0 {
                continue;
            }
            if effective_ddf_window(
                self.agnus.revision(),
                bplcon0,
                self.denise.ddfstrt,
                self.denise.ddfstop,
                self.harddis_active(),
            )
            .is_none()
            {
                continue;
            }
            let words_per_row = bitplane_words_per_row(
                self.agnus.revision(),
                bplcon0,
                self.agnus.fmode(),
                self.denise.ddfstrt,
                self.denise.ddfstop,
                self.harddis_active(),
            );
            self.advance_display_dma_ptrs(1, nplanes, words_per_row, vpos);
        }
    }

    pub(super) fn advance_sprite_dma_to_display_start(&mut self) {
        self.advance_sprite_dma_replay_to(self.display_start_vpos_for_current_control(), 0);
    }

    /// Replay pre-display sprite DMA up to (not including) beam position
    /// `limit_vpos`/`limit_hpos`, continuing from wherever the frame's replay
    /// last stopped.
    ///
    /// This runs in step with the beam -- driven slot by slot as the beam
    /// passes them -- rather than in one batch at the display start, because
    /// `fetch_sprite_words` reads chip RAM as it is *now*. The vertical-blank
    /// control-word fetch happens on PAL line $19, while the display start is
    /// typically line $2C, and software routinely rewrites its sprite
    /// descriptors from the vertical-blank interrupt in between: a batched
    /// replay read the rewritten descriptor instead of the one the hardware
    /// latched, which left a channel's comparators pointing at the wrong lines
    /// for the rest of the field (Ghostown's Spooky Town scrolls its credits
    /// that way, and lost one of four Copper-multiplexed sprite channels on
    /// alternate frames).
    pub(super) fn advance_sprite_dma_replay_to(&mut self, limit_vpos: u32, limit_hpos: u32) {
        let display_start = self.display_start_vpos_for_current_control();
        if display_start == 0 {
            return;
        }
        let (limit_vpos, limit_hpos) = if limit_vpos >= display_start {
            (display_start, 0)
        } else {
            (limit_vpos, limit_hpos)
        };

        // Sprite DMA runs from the top of the frame, independent of the bitplane
        // display window. The frame snapshot is taken at the DIW-derived display
        // start, which may be below the fixed standard-frame overscan render top,
        // so replay sprite DMA up to that snapshot line and capture any top-border
        // sprite lines along the way. Crucially, SPREN can be toggled within the
        // frame -- software may enable sprite DMA only briefly off-screen to load
        // reused sprites, then clear it before the visible window and reposition
        // the held sprites per line. Replay this frame's DMACON, SPRxPT and
        // SPRxPOS/CTL writes across the span and run the sprite slots only on
        // lines where SPREN was actually enabled, rather than sampling registers
        // at the display start.
        if !self.sprite_dma_replay.started {
            // Seed from the previous field's carried SPRxPT frontier rather
            // than the last Copper/CPU write captured in `base.sprpt`. See
            // `sprite_dma_frame_start_ptr` for why channels must not snap back
            // to their stale control-word address.
            self.current_frame_sprite_lines
                .retain(|line| line.beam_y >= display_start as i32);
            for lines in &mut self.current_frame_sprite_lines_by_y {
                lines.retain(|line| line.beam_y >= display_start as i32);
            }
            self.current_frame_sprite_collision_sources = empty_sprite_collision_sources();
            self.current_frame_sprite_display_enable_x_by_y = empty_sprite_display_enable_x_by_y();
            self.current_frame_sprite_dma_observed = !self.current_frame_sprite_lines.is_empty();
            self.display_dma_sprpt = self.sprite_dma_frame_start_ptr;
            self.reset_display_sprite_dma_states();
            self.sprite_dma_replay = SpriteDmaReplayCursor {
                started: true,
                vpos: 0,
                slot_idx: 0,
                event_idx: 0,
                dmacon: self.current_frame_render_base.dmacon,
            };
        }
        // The event cursor indexes this frame's render-event log directly and
        // only moves forward, so driving the replay from the beam costs the
        // events it actually consumes rather than a fresh filtered copy of the
        // whole log on every call.
        let events_len = self.current_render_events().len();
        let mut dmacon = self.sprite_dma_replay.dmacon;
        let mut idx = self.sprite_dma_replay.event_idx.min(events_len);
        let mut vpos = self.sprite_dma_replay.vpos;
        let mut slot_idx = self.sprite_dma_replay.slot_idx;
        // Walk the line's sixteen sprite DMA slots in beam order, stopping at
        // the requested beam position so a slot is replayed while chip RAM
        // still holds what that slot would have read.
        loop {
            if vpos > limit_vpos || (vpos == limit_vpos && limit_hpos == 0) {
                break;
            }
            if slot_idx >= 2 * SPRITE_DMA_SLOT1_HPOS.len() {
                // End of the line: apply whatever is left of its writes and
                // step to the next line.
                while let Some((event_vpos, event_hpos, offset, value)) =
                    self.sprite_dma_replay_event_at(idx, events_len)
                {
                    if event_vpos != vpos {
                        break;
                    }
                    idx += 1;
                    if Self::is_sprite_dma_replay_offset(offset) {
                        self.apply_sprite_dma_replay_write(
                            offset,
                            value,
                            event_vpos,
                            event_hpos,
                            &mut dmacon,
                        );
                    }
                }
                vpos += 1;
                slot_idx = 0;
                continue;
            }
            let sprite = slot_idx / 2;
            let is_slot1 = slot_idx.is_multiple_of(2);
            let slot1_hpos = SPRITE_DMA_SLOT1_HPOS[sprite];
            let slot_hpos = if is_slot1 { slot1_hpos } else { slot1_hpos + 2 };
            if vpos == limit_vpos && slot_hpos >= limit_hpos {
                break;
            }
            while let Some((event_vpos, event_hpos, offset, value)) =
                self.sprite_dma_replay_event_at(idx, events_len)
            {
                if event_vpos > vpos || (event_vpos == vpos && event_hpos >= slot_hpos) {
                    break;
                }
                idx += 1;
                if Self::is_sprite_dma_replay_offset(offset) {
                    self.apply_sprite_dma_replay_write(
                        offset,
                        value,
                        event_vpos,
                        event_hpos,
                        &mut dmacon,
                    );
                }
            }

            // The start-of-line comparator update runs on every line,
            // even where the vertical blank or DDF conflicts inhibit
            // the DMA slots themselves.
            if is_slot1 {
                self.sprite_comparators_catch_up(sprite, vpos, dmacon);
            }
            if !self.sprite_dma_inhibited_by_vertical_blank_at(vpos) {
                let spren = dmacon & (DMACON_DMAEN | DMACON_SPREN) == (DMACON_DMAEN | DMACON_SPREN);
                let ddf_blocked = sprite_dma_disabled_by_bitplane_ddf(
                    sprite,
                    self.agnus.revision(),
                    self.effective_bitplane_bplcon0(),
                    self.agnus.fmode(),
                    self.effective_bitplane_dmacon(),
                    self.denise.ddfstrt,
                    self.denise.ddfstop,
                    self.harddis_active(),
                );
                if is_slot1 {
                    self.sprite_slot1(sprite, vpos, spren, ddf_blocked, true);
                } else {
                    // Off-screen lines evolve the channel state only;
                    // the visible field's capture starts at the display
                    // start.
                    let _ = self.sprite_slot2(sprite, vpos, spren, ddf_blocked, true);
                }
            }
            slot_idx += 1;
        }
        self.sprite_dma_replay.vpos = vpos;
        self.sprite_dma_replay.slot_idx = slot_idx;
        self.sprite_dma_replay.event_idx = idx;
        self.sprite_dma_replay.dmacon = dmacon;
    }

    /// The registers the pre-display sprite-DMA replay tracks: DMACON, the
    /// SPRxPT halves, and the SPRxPOS/CTL/DATA/DATB block.
    fn is_sprite_dma_replay_offset(offset: u16) -> bool {
        offset == 0x096 || (0x120..=0x13F).contains(&offset) || (0x140..=0x17F).contains(&offset)
    }

    /// One render event as `(vpos, hpos, masked offset, value)`, or `None`
    /// past the end of the log.
    fn sprite_dma_replay_event_at(
        &self,
        idx: usize,
        events_len: usize,
    ) -> Option<(u32, u32, u16, u16)> {
        if idx >= events_len {
            return None;
        }
        let w = &self.current_render_events()[idx];
        Some((w.vpos, w.hpos, w.offset & 0x01FE, w.value))
    }

    pub(super) fn apply_sprite_dma_replay_write(
        &mut self,
        offset: u16,
        value: u16,
        vpos: u32,
        _hpos: u32,
        dmacon: &mut u16,
    ) {
        if offset == 0x096 {
            if value & 0x8000 != 0 {
                *dmacon |= value & 0x7FFF;
            } else {
                *dmacon &= !value;
            }
            return;
        }

        if (0x140..=0x17F).contains(&offset) {
            let idx = ((offset - 0x140) / 8) as usize;
            match (offset - 0x140) & 0x0006 {
                0x0 => self.poke_display_sprite_control_at(
                    idx,
                    value,
                    SpriteControlRegisterWrite::Pos,
                    vpos,
                ),
                0x2 => self.poke_display_sprite_control_at(
                    idx,
                    value,
                    SpriteControlRegisterWrite::Ctl,
                    vpos,
                ),
                _ => {}
            }
            return;
        }

        let idx = ((offset - 0x120) / 4) as usize;
        if idx >= 8 {
            return;
        }
        if offset & 2 == 0 {
            let cur = self.display_dma_sprpt[idx];
            self.display_dma_sprpt[idx] = (cur & 0x0000_FFFF) | ((value as u32 & 0x001F) << 16);
        } else {
            let cur = self.display_dma_sprpt[idx];
            self.display_dma_sprpt[idx] = (cur & 0x00FF_0000) | (value as u32 & 0xFFFE);
        }
    }

    pub(super) fn sprite_dma_inhibited_by_vertical_blank_at(&self, vpos: u32) -> bool {
        vpos < sprite_dma_first_active_vpos(self.agnus.video_standard())
    }

    pub(super) fn capture_sprite_dma_words_if_due(
        &mut self,
        vpos: u32,
        old_hpos: u32,
        new_hpos: u32,
        old_emulated_cck: u64,
    ) {
        // No sprite DMA slot lies in [old_hpos, new_hpos): nothing below can
        // run (the per-sprite loop checks the same window), so skip the
        // sprite-state scan on the vast majority of beam advances.
        if old_hpos > SPRITE_DMA_SLOT1_HPOS[7] + 2 || new_hpos <= SPRITE_DMA_SLOT1_HPOS[0] {
            return;
        }
        let vblank_inhibited = self.sprite_dma_inhibited_by_vertical_blank_at(vpos);
        let Some(fb_y) = visible_framebuffer_y(
            vpos,
            self.current_frame_visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        ) else {
            return;
        };
        let started = VideoPipelineStats::probe_timing_sample(
            &mut self.video_pipeline_stats.sprite_fetch_probes,
            VIDEO_FETCH_TIMING_SAMPLE_RATE,
        );
        let mut pair_slots = 0usize;
        let mut fetched_lines = 0usize;
        let bitplane_bplcon0 = self.effective_bitplane_bplcon0();
        let bitplane_dmacon = self.effective_bitplane_dmacon();
        // Lines above the display start are provisional here: the
        // pre-display replay re-runs them at the display start with the
        // frame's final DMACON/SPRxPT event timeline and owns their latch
        // write-through (sprite_slot1 doc). TODO: a mid-frame DIWSTRT
        // rewrite that moves the display start re-runs the overlap; the
        // accurate model writes the latch once, at the single hardware
        // fetch time of each slot.
        // Pre-display lines belong to the beam-paced replay
        // (`advance_sprite_dma_replay_to`), which owns their event timeline and
        // runs each line once its slots have passed. Running them here as well
        // would fetch every pre-display line twice.
        let latch_write_through = vpos >= self.display_start_vpos_for_current_control();
        if !latch_write_through {
            return;
        }
        for (sprite, &slot1_hpos) in SPRITE_DMA_SLOT1_HPOS.iter().enumerate() {
            // Each sprite line uses two hardware DMA slots: $15+4N fetches
            // POS or DATA, $17+4N fetches CTL or DATB. Both crossings are
            // evaluated at their own beam time, so a mid-line DMACON edge
            // between the two slots fetches exactly one of the pair, and
            // memory rewritten between them is sampled per slot.
            let slot2_hpos = slot1_hpos + 2;
            if old_hpos > slot2_hpos || new_hpos <= slot1_hpos {
                continue;
            }
            // SPREN is sampled by each DMA slot individually (the sprena/
            // sprdis vAmigaTS sweeps step DMACON writes in two-colour-clock
            // increments around individual slots), honouring the DMACON
            // write commit delay.
            let slot_cck = |hpos: u32| {
                old_emulated_cck.saturating_add(u64::from(hpos.saturating_sub(old_hpos)))
            };
            let spren_at = |dmacon: u16| {
                dmacon & (DMACON_DMAEN | DMACON_SPREN) == (DMACON_DMAEN | DMACON_SPREN)
            };
            let ddf_blocked = sprite_dma_disabled_by_bitplane_ddf(
                sprite,
                self.agnus.revision(),
                bitplane_bplcon0,
                self.agnus.fmode(),
                bitplane_dmacon,
                self.denise.ddfstrt,
                self.denise.ddfstop,
                self.harddis_active(),
            );
            if (old_hpos..new_hpos).contains(&slot1_hpos) {
                let dmacon_now = self.effective_bitplane_dmacon_at(slot_cck(slot1_hpos));
                // The start-of-line comparator update runs on every line,
                // even where vertical blank inhibits the DMA slots.
                self.sprite_comparators_catch_up(sprite, vpos, dmacon_now);
                if !vblank_inhibited {
                    self.sprite_slot1(
                        sprite,
                        vpos,
                        spren_at(dmacon_now),
                        ddf_blocked,
                        latch_write_through,
                    );
                }
            }
            if !(old_hpos..new_hpos).contains(&slot2_hpos) {
                continue;
            }
            let dmacon_now = self.effective_bitplane_dmacon_at(slot_cck(slot2_hpos));
            self.sprite_comparators_catch_up(sprite, vpos, dmacon_now);
            if vblank_inhibited {
                continue;
            }
            let slot2_enabled = spren_at(dmacon_now);
            if slot2_enabled || self.display_dma_sprite_state[sprite].entry_line_vpos == vpos as i32
            {
                pair_slots += 1;
            }
            let mut captured_line = false;
            {
                let line = self.sprite_slot2(
                    sprite,
                    vpos,
                    slot2_enabled,
                    ddf_blocked,
                    latch_write_through,
                );
                if let Some(line) = line {
                    // COPPERLINE_DIAG_SPRCAP=BEAMY|all: log captured sprite
                    // lines on that beam line (frame, channel, position, words,
                    // and the chip-RAM data addresses they fetched).
                    if let Some(want) = diag_sprcap() {
                        if diag_sprcap_matches(want, line.beam_y) {
                            let st = &self.display_dma_sprite_state[sprite];
                            log::info!(
                                "sprcap f={} s{} y={} hstart={} hsub={} att={} w={} A={:04X} {:04X?} B={:04X} {:04X?} vstrt={} vstop={} ena={} ptr={:06X}",
                                self.emulated_frames,
                                line.sprite,
                                line.beam_y,
                                line.hstart,
                                u8::from(line.hsub_70ns),
                                u8::from(line.attached),
                                line.width_words,
                                line.data,
                                line.data_ext,
                                line.datb,
                                line.datb_ext,
                                st.vstrt,
                                st.vstop,
                                u8::from(st.dma_enabled),
                                self.display_dma_sprpt[sprite],
                            );
                        }
                    }
                    self.current_frame_sprite_lines.push(line);
                    self.current_frame_sprite_lines_by_y[fb_y].push(line);
                    self.current_frame_sprite_dma_observed = true;
                    captured_line = true;
                    fetched_lines += 1;
                }
            }
            if captured_line {
                self.current_frame_sprite_collision_sources[fb_y] = None;
            }
        }
        self.record_sprite_fetch_timing(
            pair_slots,
            fetched_lines,
            started.map(|started| (started.elapsed(), VIDEO_FETCH_TIMING_SAMPLE_RATE)),
        );
    }

    pub(super) fn ensure_current_frame_sprite_collision_sources_for_y(
        &mut self,
        fb_y: usize,
        vpos: u32,
    ) {
        if self.current_frame_sprite_collision_sources[fb_y].is_none() {
            self.current_frame_sprite_collision_sources[fb_y] =
                Some(live_sprite_collision_sources_with_beam_gated_odd(
                    &self.current_frame_sprite_lines_by_y[fb_y],
                    vpos as i32,
                    self.agnus.fmode(),
                ));
        }
    }

    /// Run one full sprite line (both slots, DMA on) at a line the beam is
    /// not sweeping: the tests' stand-in for the pre-display replay.
    #[cfg(test)]
    pub(super) fn captured_sprite_line_at(
        &mut self,
        sprite: usize,
        vpos: u32,
    ) -> Option<CapturedSpriteLine> {
        self.sprite_comparators_catch_up(sprite, vpos, self.agnus.dmacon);
        if self.sprite_dma_inhibited_by_vertical_blank_at(vpos) {
            return None;
        }
        self.sprite_slot1(sprite, vpos, true, false, true);
        self.sprite_slot2(sprite, vpos, true, false, true)
    }

    /// Lazily run the start-of-line comparator update for every line since
    /// the last evaluated one (vAmiga updateSpriteDMA): the vertical-blank
    /// reset forces vstop to the first post-blank line -- which is what
    /// schedules each field's first POS/CTL control-word fetch -- the DMA
    /// flip-flop clears in the frame's last line, and outside the blank the
    /// vstrt/vstop comparators set/clear it (clear winning on a tie).
    pub(super) fn sprite_comparators_catch_up(&mut self, sprite: usize, vpos: u32, dmacon: u16) {
        let beam_y = vpos as i32;
        let mut state = self.display_dma_sprite_state[sprite];
        if state.comparator_vpos >= beam_y {
            return;
        }
        let reset_line = sprite_dma_first_active_vpos(self.agnus.video_standard()) as i32;
        let last_line = self.agnus.current_frame_lines() as i32 - 1;
        // TODO: SPREN is sampled once for the whole caught-up span; only the
        // vertical-blank reset line consumes it, and the live/replay paths
        // call this every line, so the approximation only matters when a
        // DMACON edge lands inside a multi-line catch-up across the blank.
        let spren = dmacon & (DMACON_DMAEN | DMACON_SPREN) == (DMACON_DMAEN | DMACON_SPREN);
        for v in (state.comparator_vpos.max(-1) + 1)..=beam_y {
            if v == reset_line && spren {
                state.vstop = v;
                continue;
            }
            if v == last_line {
                state.dma_enabled = false;
                continue;
            }
            if v > reset_line {
                if v == state.vstrt {
                    state.dma_enabled = true;
                }
                if v == state.vstop {
                    state.dma_enabled = false;
                }
            }
        }
        state.comparator_vpos = beam_y;
        self.display_dma_sprite_state[sprite] = state;
    }

    /// Fetch one sprite DMA word group (1/2/4 words by FMODE) from the
    /// channel's live pointer, advancing SPRxPT the way the chip does:
    /// only words actually fetched move the pointer.
    fn fetch_sprite_words(&mut self, sprite: usize) -> Option<[u16; 4]> {
        if self.mem.chip_ram.is_empty() {
            return None;
        }
        let fmode = self.agnus.fmode();
        let mode = (fmode >> 2) & 0x0003;
        let quantum = sprite_fetch_quantum(fmode);
        let ptr = self.display_dma_sprpt[sprite] & self.chip_dma_mask & !1;
        let mut words = [0u16; 4];
        for (w, word) in words.iter_mut().enumerate().take(quantum as usize) {
            let addr = wide_fetch_word_address(ptr, mode, w);
            if self.mem_watches_armed() {
                self.note_dma_read(crate::debugger::WatchSource::Sprite(sprite as u8), addr, 2);
            }
            *word = read_chip_word_wrapping(&self.mem.chip_ram, addr);
        }
        self.display_dma_sprpt[sprite] = ptr.wrapping_add(2 * quantum) & self.chip_dma_mask & !1;
        Some(words)
    }

    /// With SSCAN2 the hardware fetches sprite data on every other display
    /// line and redisplays the previous fetch in between.
    fn sprite_scan_repeat_line(&self, state: &DisplaySpriteDmaState, beam_y: i32) -> bool {
        sprite_scan_doubled(self.agnus.fmode())
            && state.last_data_fetch_vpos != unset_sprite_line_marker()
            && beam_y == state.last_data_fetch_vpos + 1
    }

    /// First hardware sprite slot ($15+4N). On the channel's vstop line the
    /// DMA flip-flop is cleared -- even when SPREN is off, which is what
    /// leaves a sprite dead until the next field when software disables DMA
    /// across its vstop line -- and, when the slot is enabled, the next
    /// control word's POS part is fetched. On other lines an enabled
    /// channel fetches the line's DATA word(s), sampled at this slot's beam
    /// time; the second slot assembles and emits the line.
    /// `latch_write_through` says whether this pass is the authoritative
    /// sprite-DMA pass for the line: pre-display lines are computed twice
    /// (a provisional live pass, then the pre-display replay at the display
    /// start re-runs them with the frame's final DMACON/SPRxPT event
    /// timeline and owns the result), and only the authoritative pass may
    /// land its fetches in the Denise display latches.
    pub(super) fn sprite_slot1(
        &mut self,
        sprite: usize,
        vpos: u32,
        spren: bool,
        ddf_blocked: bool,
        latch_write_through: bool,
    ) {
        let beam_y = vpos as i32;
        let mut state = self.display_dma_sprite_state[sprite];
        state.entry_line_vpos = beam_y;
        state.pending_data = None;
        state.pending_line_vpos = unset_sprite_line_marker();
        if beam_y == state.vstop {
            state.dma_enabled = false;
            if spren && !ddf_blocked {
                if let Some(words) = self.fetch_sprite_words(sprite) {
                    state.poke_pos(words[0]);
                    state.reevaluate_comparators_at(beam_y);
                    if latch_write_through {
                        self.denise.dma_write_sprpos(sprite, words[0]);
                    }
                }
            }
        } else if state.dma_enabled
            && spren
            && !ddf_blocked
            && !self.sprite_scan_repeat_line(&state, beam_y)
        {
            if let Some(words) = self.fetch_sprite_words(sprite) {
                state.pending_data = Some((words[0], [words[1], words[2], words[3]]));
                state.pending_line_vpos = beam_y;
                state.last_data_fetch_vpos = beam_y;
                // The DATA fetch arms the display latch exactly like a
                // manual SPRxDATA write.
                if latch_write_through {
                    self.denise.dma_write_sprdata(sprite, words[0]);
                }
            }
        }
        self.display_dma_sprite_state[sprite] = state;
    }

    /// Second hardware sprite slot ($17+4N): on the channel's vstop line it
    /// fetches the control word's CTL part (loading the next vertical
    /// comparators and disarming the display latches); on other lines it
    /// fetches DATB at its own beam time and assembles/emits the display
    /// line. A skipped slot reuses the stale latch on that side, a missed
    /// DATA slot never arms the sprite, and an armed channel with DMA off
    /// keeps displaying its latches until a CTL fetch or poke disarms it.
    pub(super) fn sprite_slot2(
        &mut self,
        sprite: usize,
        vpos: u32,
        spren: bool,
        ddf_blocked: bool,
        latch_write_through: bool,
    ) -> Option<CapturedSpriteLine> {
        let beam_y = vpos as i32;
        let mut state = self.display_dma_sprite_state[sprite];
        let pending = if state.pending_line_vpos == beam_y {
            state.pending_data.take()
        } else {
            None
        };
        state.pending_data = None;
        if beam_y == state.vstop {
            state.dma_enabled = false;
            if spren && !ddf_blocked {
                if let Some(words) = self.fetch_sprite_words(sprite) {
                    state.poke_ctl(words[0]);
                    state.reevaluate_comparators_at(beam_y);
                    // The CTL fetch is a SPRxCTL write: it disarms the live
                    // display latch. Software relies on the null terminator's
                    // CTL to silence a channel for good (arming it later with
                    // SPRxDATA redisplays whatever the registers hold, so the
                    // latch has to track DMA truth, not the last manual write).
                    if latch_write_through {
                        self.denise.dma_write_sprctl(sprite, words[0]);
                    }
                }
            }
            self.display_dma_sprite_state[sprite] = state;
            return None;
        }
        let mut datb_fetched = None;
        if state.dma_enabled
            && spren
            && !ddf_blocked
            && !self.sprite_scan_repeat_line(&state, beam_y)
        {
            if let Some(words) = self.fetch_sprite_words(sprite) {
                datb_fetched = Some((words[0], [words[1], words[2], words[3]]));
                // The DATB fetch lands in SPRxDATB like a manual write.
                if latch_write_through {
                    self.denise.dma_write_sprdatb(sprite, words[0]);
                }
            }
        }
        // Sprites captured as "held" at the visible start are repainted by
        // the renderer's manual-sprite path (which clips each
        // Copper-repositioned segment), so do not also emit stale-latch
        // redisplay lines for them here.
        if pending.is_none()
            && datb_fetched.is_none()
            && self.current_frame_held_sprites[sprite].is_some()
        {
            self.display_dma_sprite_state[sprite] = state;
            return None;
        }
        let stale = state.last_line;
        let assembled = match (pending, datb_fetched) {
            (Some(data), Some(datb)) => Some((data, datb)),
            // DATB missed its slot: the sprite displays the new DATA with
            // the stale DATB latch.
            (Some(data), None) => Some((data, stale.map_or((0, [0; 3]), |l| (l.datb, l.datb_ext)))),
            // DATA missed its slot: DATB alone does not arm the sprite; an
            // already-armed sprite keeps displaying with the stale DATA.
            (None, Some(datb)) => stale.map(|l| ((l.data, l.data_ext), datb)),
            // No fetch at all (DMA off or an SSCAN2 repeat line): an armed
            // channel redisplays its latches at the current POS/CTL decode.
            (None, None) => stale.map(|l| ((l.data, l.data_ext), (l.datb, l.datb_ext))),
        };
        let Some(((data, data_ext), (datb, datb_ext))) = assembled else {
            self.display_dma_sprite_state[sprite] = state;
            return None;
        };
        let quantum = sprite_fetch_quantum(self.agnus.fmode());
        let line_data = DisplaySpriteLineData {
            hstart: sprite_hstart_from_words(state.pos, state.ctl),
            hsub_70ns: bitplane_shres(self.denise.bplcon0) && sprite_hsub_70ns_from_ctl(state.ctl),
            data,
            datb,
            data_ext,
            datb_ext,
            width_words: quantum as u8,
            attached: state.ctl & 0x0080 != 0,
        };
        state.last_line = Some(line_data);
        self.display_dma_sprite_state[sprite] = state;
        Some(CapturedSpriteLine {
            sprite,
            hstart: line_data.hstart,
            hsub_70ns: line_data.hsub_70ns,
            beam_y,
            data: line_data.data,
            datb: line_data.datb,
            data_ext: line_data.data_ext,
            datb_ext: line_data.datb_ext,
            width_words: line_data.width_words,
            attached: line_data.attached,
        })
    }

    pub(super) fn capture_bitplane_dma_words_if_due(
        &mut self,
        vpos: u32,
        old_hpos: u32,
        new_hpos: u32,
        old_emulated_cck: u64,
    ) {
        if self.ocs_same_line_diw_start_blocked_vpos == Some(vpos) {
            return;
        }
        if self.ddf_seq_active() {
            self.capture_bitplane_dma_words_fsm(vpos, old_hpos, new_hpos, old_emulated_cck);
            return;
        }
        let display_bplcon0 = self.effective_bitplane_bplcon0_at(old_emulated_cck);
        let mode = BitplaneMode::from_bplcon0(display_bplcon0, self.aga_enabled());
        let display_planes = mode.display_planes();
        if !self.diw_vertical_open_at(vpos) {
            return;
        }
        let Some(fb_y) = visible_framebuffer_y(
            vpos,
            self.current_frame_visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        ) else {
            return;
        };

        let ram_len = self.mem.chip_ram.len();
        if ram_len == 0 {
            return;
        }
        let Some((effective_ddfstart, effective_ddfstop)) = effective_ddf_window(
            self.agnus.revision(),
            display_bplcon0,
            self.denise.ddfstrt,
            self.denise.ddfstop,
            self.harddis_active(),
        ) else {
            return;
        };
        let effective_ddfstart = u32::from(effective_ddfstart);
        let effective_ddfstop = u32::from(effective_ddfstop);
        // AGA FMODE: each fetch slot moves `quantum` consecutive words per
        // plane; the per-plane cadence stretches to `period` colour clocks
        // and the lores slot sequence spreads across the `unit`-cck block.
        let fmode = self.agnus.fmode();
        let static_plan = (self.wide_bitplane_dynamic_vpos.get() != Some(vpos)
            && self.wide_bitplane_hot_line.is_current(vpos))
        .then(|| self.wide_bitplane_hot_line.plan.get())
        .flatten();
        let quantum = static_plan.map_or_else(
            || bitplane_fetch_quantum(fmode) as usize,
            |plan| plan.quantum as usize,
        );
        // Wide-FMODE units lengthen the gap between groups of fetched words,
        // but the sequencer is still armed by the DDFSTRT comparator itself.
        // Lores plane-order slots are packed into the first eight cycles of
        // that unit; the remaining cycles are free for the blitter/CPU.
        let ddfstart = effective_ddfstart;
        if self.bitplane_ddfstart_missed_on_line(vpos, ddfstart) {
            return;
        }
        if new_hpos <= ddfstart {
            return;
        }
        let ddfstart_cck = if old_hpos <= ddfstart {
            Some(i128::from(
                old_emulated_cck.saturating_add(u64::from(ddfstart - old_hpos)),
            ))
        } else {
            old_emulated_cck
                .checked_sub(u64::from(old_hpos - ddfstart))
                .map(i128::from)
        };
        let anchor_bplcon0 = ddfstart_cck
            .map(|cck| self.bitplane_bplcon0_for_block(cck))
            .unwrap_or(display_bplcon0);
        let anchor_dma_planes =
            bitplane_dma_planes_for_fmode(anchor_bplcon0, fmode, self.aga_enabled());
        let period = static_plan.map_or_else(
            || bitplane_fetch_period(anchor_bplcon0, fmode),
            |plan| plan.period,
        );
        let unit = static_plan.map_or_else(
            || bitplane_fetch_unit(anchor_bplcon0, fmode),
            |plan| plan.unit,
        );
        let started = VideoPipelineStats::probe_timing_sample(
            &mut self.video_pipeline_stats.bitplane_fetch_probes,
            VIDEO_FETCH_TIMING_SAMPLE_RATE,
        );
        let words_per_row = static_plan.map_or_else(
            || {
                bitplane_words_per_row(
                    self.agnus.revision(),
                    anchor_bplcon0,
                    self.agnus.fmode(),
                    self.denise.ddfstrt,
                    self.denise.ddfstop,
                    self.harddis_active(),
                )
            },
            |plan| plan.words_per_row as usize,
        );
        let mut rows_started = 0usize;
        let mut slots = 0usize;
        let mut line_complete = false;
        let mut line_complete_plane_mask = 0u16;
        let addr_mask = self.chip_dma_mask;
        let hires_like = static_plan.map_or_else(
            || bitplane_hires(anchor_bplcon0) || bitplane_shres(anchor_bplcon0),
            |plan| plan.hires_like,
        );
        let last_word_idx = words_per_row.saturating_sub(1);
        if diag_caprow().is_some_and(|spec| spec.contains(vpos))
            && old_hpos <= ddfstart
            && new_hpos > ddfstart
        {
            log::info!(
                "caprow f={} v={} h={} dmacon={:#06X} bplcon0={:#06X} dma_bplcon0={:#06X} bplcon1={:#06X} bplcon2={:#06X} bplcon4={:#06X} fmode={:#06X} diw={:#06X}/{:#06X}/{:?} ddf={:#04X}/{:#04X} eff={:#04X}-{:#04X} anchor={:#04X} unit={} period={} quantum={} wpr={} display_planes={} dma_planes={} mod={}/{} bplpt={:#08X},{:#08X},{:#08X},{:#08X},{:#08X},{:#08X},{:#08X},{:#08X}",
                self.emulated_frames,
                vpos,
                self.agnus.hpos,
                self.effective_bitplane_dmacon(),
                display_bplcon0,
                anchor_bplcon0,
                self.denise.bplcon1,
                self.denise.bplcon2,
                self.denise.bplcon4,
                fmode,
                self.denise.diwstrt,
                self.denise.diwstop,
                self.effective_diwhigh(),
                self.denise.ddfstrt,
                self.denise.ddfstop,
                effective_ddfstart,
                effective_ddfstop,
                ddfstart,
                unit,
                period,
                quantum,
                words_per_row,
                display_planes,
                anchor_dma_planes,
                self.denise.bpl1mod,
                self.denise.bpl2mod,
                self.display_dma_bplpt[0],
                self.display_dma_bplpt[1],
                self.display_dma_bplpt[2],
                self.display_dma_bplpt[3],
                self.display_dma_bplpt[4],
                self.display_dma_bplpt[5],
                self.display_dma_bplpt[6],
                self.display_dma_bplpt[7],
            );
        }
        for hpos in old_hpos..new_hpos {
            let hpos_emulated_cck =
                old_emulated_cck.saturating_add(u64::from(hpos.saturating_sub(old_hpos)));
            if self.effective_bitplane_dmacon_at(hpos_emulated_cck) & (DMACON_DMAEN | DMACON_BPLEN)
                != (DMACON_DMAEN | DMACON_BPLEN)
            {
                continue;
            }
            if hpos < ddfstart {
                continue;
            }
            let rel = hpos - ddfstart;
            if hires_like {
                if rel % period != 0 {
                    continue;
                }
                let word_base = (rel / period) as usize * quantum;
                if word_base >= words_per_row {
                    continue;
                }
                let block_start_cck = i128::from(hpos_emulated_cck);
                let block_bplcon0 = self.bitplane_bplcon0_for_block(block_start_cck);
                let block_mode = BitplaneMode::from_bplcon0(block_bplcon0, self.aga_enabled());
                let block_dma_planes =
                    bitplane_dma_planes_for_fmode(block_bplcon0, fmode, self.aga_enabled());
                if block_dma_planes == 0 {
                    continue;
                }
                let block_display_planes = block_mode.display_planes();
                for plane in 0..block_dma_planes.min(8) {
                    if plane == 0 {
                        self.record_sprite_display_enable_for_bitplane_dma(vpos);
                    }
                    let fetch_ptr = self.display_dma_bplpt[plane];
                    let fetch_words = quantum.min(words_per_row - word_base);
                    for w in 0..fetch_words {
                        let word_idx = word_base + w;
                        let addr = wide_fetch_word_address(fetch_ptr, fmode, w) & addr_mask;
                        if self.mem_watches_armed() {
                            self.note_dma_read(
                                crate::debugger::WatchSource::Bitplane(plane as u8),
                                addr,
                                2,
                            );
                        }
                        let fetched = read_chip_word_wrapping(&self.mem.chip_ram, addr);
                        self.data_bus = fetched;
                        if self.capture_bitplane_fetch_word(
                            fb_y,
                            block_display_planes,
                            block_dma_planes,
                            words_per_row,
                            plane,
                            word_idx,
                            fetched,
                        ) {
                            rows_started += 1;
                        }
                        self.denise.write_bpldat(plane, fetched);
                        if word_idx == last_word_idx {
                            line_complete = true;
                            line_complete_plane_mask = plane_mask_for_count(block_dma_planes);
                        }
                    }
                    self.display_dma_bplpt[plane] = self.display_dma_bplpt[plane]
                        .wrapping_add((2 * fetch_words) as u32)
                        & addr_mask;
                    slots += 1;
                }
            } else {
                let word_base = (rel / unit) as usize * quantum;
                if word_base >= words_per_row {
                    continue;
                }
                let unit_off = rel % unit;
                if unit_off >= 8 {
                    continue;
                }
                let order = unit_off;
                let block_start_cck = i128::from(hpos_emulated_cck) - i128::from(unit_off);
                let block_bplcon0 = self.bitplane_bplcon0_for_block(block_start_cck);
                let block_mode = BitplaneMode::from_bplcon0(block_bplcon0, self.aga_enabled());
                let block_dma_planes =
                    bitplane_dma_planes_for_fmode(block_bplcon0, fmode, self.aga_enabled());
                if block_dma_planes == 0 {
                    continue;
                }
                let block_display_planes = block_mode.display_planes();
                let block_last_order = (0..block_dma_planes.min(8))
                    .map(|plane| bitplane_fetch_order(block_bplcon0, plane))
                    .max()
                    .unwrap_or(0);
                for plane in 0..block_dma_planes.min(8) {
                    if bitplane_fetch_order(block_bplcon0, plane) != order {
                        continue;
                    }
                    if plane == 0 {
                        self.record_sprite_display_enable_for_bitplane_dma(vpos);
                    }
                    let fetch_ptr = self.display_dma_bplpt[plane];
                    let fetch_words = quantum.min(words_per_row - word_base);
                    for w in 0..fetch_words {
                        let word_idx = word_base + w;
                        let addr = wide_fetch_word_address(fetch_ptr, fmode, w) & addr_mask;
                        if self.mem_watches_armed() {
                            self.note_dma_read(
                                crate::debugger::WatchSource::Bitplane(plane as u8),
                                addr,
                                2,
                            );
                        }
                        let fetched = read_chip_word_wrapping(&self.mem.chip_ram, addr);
                        self.data_bus = fetched;
                        if self.capture_bitplane_fetch_word(
                            fb_y,
                            block_display_planes,
                            block_dma_planes,
                            words_per_row,
                            plane,
                            word_idx,
                            fetched,
                        ) {
                            rows_started += 1;
                        }
                        self.denise.write_bpldat(plane, fetched);
                        if word_idx == last_word_idx && order == block_last_order {
                            line_complete = true;
                            line_complete_plane_mask = plane_mask_for_count(block_dma_planes);
                        }
                    }
                    self.display_dma_bplpt[plane] = self.display_dma_bplpt[plane]
                        .wrapping_add((2 * fetch_words) as u32)
                        & addr_mask;
                    slots += 1;
                }
            }
        }

        if slots == 0 {
            return;
        }
        if line_complete {
            self.advance_display_dma_modulos_for_mask(line_complete_plane_mask, self.agnus.vpos);
        }

        self.record_bitplane_fetch_timing(
            slots,
            rows_started,
            usize::from(line_complete),
            started.map(|started| (started.elapsed(), VIDEO_FETCH_TIMING_SAMPLE_RATE)),
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn capture_bitplane_fetch_word(
        &mut self,
        fb_y: usize,
        display_planes: usize,
        dma_planes: usize,
        words_per_row: usize,
        plane: usize,
        word_idx: usize,
        fetched: u16,
    ) -> bool {
        let row_needs_init = match &self.current_frame_bitplane_rows[fb_y] {
            Some(row) => row.nplanes != display_planes || row.words_per_row != words_per_row,
            None => true,
        };
        if row_needs_init {
            let old_row = self.current_frame_bitplane_rows[fb_y].take();
            let mut row = self
                .bitplane_row_pool
                .pop()
                .unwrap_or_else(|| CapturedBitplaneRow {
                    nplanes: 0,
                    words_per_row: 0,
                    planes: std::array::from_fn(|_| Vec::new()),
                    fetch_origin_cck: None,
                });
            row.nplanes = display_planes;
            row.words_per_row = words_per_row;
            row.fetch_origin_cck = None;
            for plane in &mut row.planes {
                plane.resize(words_per_row, 0);
                plane.fill(0);
            }
            for plane in dma_planes..display_planes {
                row.planes[plane].fill(self.denise.bpldat[plane]);
            }
            if let Some(old_row) = old_row.as_ref() {
                let copy_planes = old_row.nplanes.min(display_planes).min(8);
                let copy_words = old_row.words_per_row.min(words_per_row);
                for plane in 0..copy_planes {
                    row.planes[plane][..copy_words]
                        .copy_from_slice(&old_row.planes[plane][..copy_words]);
                }
            }
            self.current_frame_bitplane_rows[fb_y] = Some(row);
            if let Some(old_row) =
                old_row.filter(|_| self.bitplane_row_pool.len() < MAX_VISIBLE_LINES)
            {
                self.bitplane_row_pool.push(old_row);
            }
        }
        if let Some(row) = self.current_frame_bitplane_rows[fb_y].as_mut() {
            row.planes[plane][word_idx] = fetched;
        }
        row_needs_init
    }

    fn recycle_captured_bitplane_rows(&mut self, rows: Vec<Option<CapturedBitplaneRow>>) {
        for row in rows.into_iter().flatten() {
            if self.bitplane_row_pool.len() == MAX_VISIBLE_LINES {
                break;
            }
            self.bitplane_row_pool.push(row);
        }
    }

    pub(super) fn advance_display_dma_ptrs(
        &mut self,
        rows: usize,
        nplanes: usize,
        words_per_row: usize,
        first_vpos: u32,
    ) {
        for row in 0..rows {
            for plane in 0..nplanes.min(8) {
                self.display_dma_bplpt[plane] =
                    self.display_dma_bplpt[plane].wrapping_add((words_per_row * 2) as u32);
            }
            self.advance_display_dma_modulos(nplanes, words_per_row, first_vpos + row as u32);
        }
    }

    /// FMODE BSCAN2 (bit 14, Alice only) scan-doubles bitplanes: both plane
    /// groups share one end-of-line modulo, selected by the line parity
    /// relative to DIWSTRT's vertical start - the matching-parity line adds
    /// BPL1MOD, the doubled line BPL2MOD (WinUAE model). Software doubles
    /// each fetched row by rewinding with BPL1MOD = -(row bytes) and
    /// advancing with BPL2MOD.
    pub(super) fn display_dma_modulo_for_plane(&self, plane: usize, vpos: u32) -> i16 {
        if self.agnus.fmode() & 0x4000 != 0 {
            return if (u32::from(self.denise.diwstrt >> 8) ^ vpos) & 1 != 0 {
                self.denise.bpl2mod
            } else {
                self.denise.bpl1mod
            };
        }
        if plane & 1 == 0 {
            self.denise.bpl1mod
        } else {
            self.denise.bpl2mod
        }
    }

    pub(super) fn advance_display_dma_modulos(
        &mut self,
        nplanes: usize,
        _words_per_row: usize,
        vpos: u32,
    ) {
        self.advance_display_dma_modulos_for_mask(plane_mask_for_count(nplanes), vpos);
    }

    pub(super) fn advance_display_dma_modulos_for_mask(&mut self, plane_mask: u16, vpos: u32) {
        for plane in 0..8 {
            if plane_mask & (1u16 << plane) == 0 {
                continue;
            }
            let modulo = self.display_dma_modulo_for_plane(plane, vpos);
            self.display_dma_bplpt[plane] = ((self.display_dma_bplpt[plane] as i64)
                .wrapping_add(modulo as i64) as u32)
                & self.chip_dma_mask;
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_FETCH") && (66..76).contains(&self.agnus.vpos) {
            log::info!(
                "fetch v={} plane_mask={:#04X} bplpt0={:#08X} (expect 0x03E606+{}*352={:#08X})",
                self.agnus.vpos,
                plane_mask,
                self.display_dma_bplpt[0],
                self.agnus.vpos - 66,
                0x03E606u32 + (self.agnus.vpos - 66 + 1) * 352,
            );
        }
    }

    pub(super) fn record_render_write(&mut self, offset: u16, value: u16, source: BeamWriteSource) {
        // Register writes take effect a fixed number of colour clocks after
        // the chip-bus slot that carried them, and the delay is a property
        // of the register pipeline, not of the writer:
        //
        // - Denise-boundary registers apply to the pixel pipeline about
        //   four colour clocks after the slot (DENISE_WRITE_EFFECT_DELAY_
        //   CCK; vAmiga models this as a register-change delay plus
        //   pixel-domain application offsets inside Denise). Verified
        //   two-sided against vAmiga's VAMIGA_CPU_PROBE landing trace:
        //   landing-matched dense CPU COLOR00 lines rendered exactly two
        //   colour clocks left of vAmiga until the CPU side carried the
        //   same slot-referenced delay the Copper side already had.
        // - Agnus's two-cycle register class (DMACON, BPLxPT, BPLxMOD,
        //   SPRxPT) applies two colour clocks after the slot (AGNUS_WRITE_
        //   EFFECT_DELAY_CCK; vAmiga `recordRegisterChange(DMA_CYCLES(2))`).
        //   The bitplane/sprite DMA-gating replay is calibrated against
        //   events recorded at that position (vAmigaTS DMACON bplon bars
        //   sit 8 px right of vAmiga when these events carry the Denise
        //   delay instead).
        //
        // The render-side anchors (COLOR_WRITE_HPOS_FB0, COPPER_WAIT_HPOS_
        // FB0, the sprite write pipeline) are photo-calibrated against
        // events recorded at the Denise-effective position. A Copper MOVE
        // executes at its bus slot (the current beam position); a CPU write
        // is applied once its whole bus cycle has been billed, past its
        // granted slot, so the slot is taken from `cpu_custom_access_slot`
        // (a direct call without a granted slot treats the current beam
        // position as the slot).
        //
        // Copper-sourced events currently record the Denise delay for the
        // Agnus two-cycle class as well: the DMA-gating replay of copper
        // writes was calibrated with that offset when the copper landings
        // became bus-exact. TODO: model the Agnus boundary for copper
        // writes too and recalibrate the copper-driven DMA-gating replay.
        let agnus_two_cycle = matches!(
            offset & 0x01FE,
            0x096 | 0x0E0..=0x0FE | 0x108 | 0x10A | 0x120..=0x13E
        );
        let (mut vpos, mut hpos, delay) = match source {
            BeamWriteSource::Copper => (
                self.agnus.vpos,
                self.agnus.hpos,
                DENISE_WRITE_EFFECT_DELAY_CCK,
            ),
            BeamWriteSource::Cpu | BeamWriteSource::CpuCopperIrq => {
                let (v, h) = self
                    .cpu_custom_access_slot
                    .unwrap_or((self.agnus.vpos, self.agnus.hpos));
                let delay = if agnus_two_cycle {
                    AGNUS_WRITE_EFFECT_DELAY_CCK
                } else {
                    DENISE_WRITE_EFFECT_DELAY_CCK
                };
                (v, h, delay)
            }
        };
        hpos += delay;
        let line_cck = self.agnus.current_line_cck();
        if hpos >= line_cck {
            hpos -= line_cck;
            vpos += 1;
            if vpos >= self.agnus.current_frame_lines() {
                vpos = 0;
            }
        }
        let event = BeamRegisterWrite {
            vpos,
            hpos,
            offset,
            value,
            source,
        };
        if matches!(source, BeamWriteSource::CpuCopperIrq)
            && matches!(offset & 0x01FE, 0x180..=0x1BE)
            // Same scoping as write_cpu_palette_snapshot: only bank-0
            // LOCT-clear writes belong to the split-palette pattern the
            // bottom-palette replay reconstructs.
            && self.cpu_palette_write_bank_loct() == (0, false)
        {
            let (target_vpos, target_hpos) = self.cpu_palette_target_beam.unwrap_or((vpos, hpos));
            if target_vpos >= CPU_COPPER_BOTTOM_PALETTE_MIN_VPOS {
                if self.cpu_palette_target_writes == 0 {
                    self.pending_beam_bottom_palette_events.clear();
                }
                self.pending_beam_bottom_palette_events
                    .push(BeamRegisterWrite {
                        vpos: target_vpos,
                        hpos: target_hpos,
                        offset,
                        value,
                        source,
                    });
            }
        }
        self.current_frame_render_events.push(event);
        if is_live_collision_relevant_custom_write(offset) {
            self.current_frame_collision_events.push(event);
        }
        // CLXCON2 joins the control replay only where the register exists:
        // Lisa latches $10E, so pre-AGA machines keep today's event stream.
        if is_live_collision_control_custom_write(offset)
            || ((offset & 0x01FE) == 0x10E && self.denise_is_lisa())
        {
            self.current_frame_collision_control_events.push(event);
            self.current_frame_collision_control_index = None;
        }
        if is_live_collision_bpldat_custom_write(offset) {
            self.current_frame_collision_bpldat_events.push(event);
            self.current_frame_collision_bpldat_index = None;
        }
        if is_live_collision_sprite_custom_write(offset) {
            self.current_frame_collision_sprite_events.push(event);
            self.current_frame_collision_sprite_index = None;
        }
        if (offset & 0x01FE) == 0x100 && value & 0x0400 != 0 {
            self.current_frame_collision_may_have_dual_playfield = true;
        }
    }

    pub(super) fn record_sprite_display_enable_at(&mut self, vpos: u32, hpos: u32) {
        let Some(fb_y) = visible_framebuffer_y(
            vpos,
            self.current_frame_visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        ) else {
            return;
        };
        let denise_hpos = hpos.saturating_sub(DENISE_HPOS_LAG_CCK);
        let x = framebuffer_x_for_live_collision_hpos(denise_hpos) as usize;
        self.record_sprite_display_enable_x(fb_y, x);
    }

    pub(super) fn record_sprite_display_enable_for_bitplane_dma(&mut self, vpos: u32) {
        let Some(fb_y) = visible_framebuffer_y(
            vpos,
            self.current_frame_visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        ) else {
            return;
        };
        let (window_x_start, _) = live_display_window_x(
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
        );
        let x = window_x_start.max(0) as usize;
        self.record_sprite_display_enable_x(fb_y, x);
    }

    pub(super) fn record_sprite_display_enable_x(&mut self, fb_y: usize, x: usize) {
        let enable_x = &mut self.current_frame_sprite_display_enable_x_by_y[fb_y];
        *enable_x = Some(enable_x.map_or(x, |old| old.min(x)));
    }

    pub(super) fn commit_pending_bottom_palette_events(&mut self) {
        if self.pending_beam_bottom_palette_events.is_empty() {
            return;
        }
        if palette_event_sequences_equivalent(
            &self.beam_bottom_palette_events,
            &self.pending_beam_bottom_palette_events,
        ) {
            let current_vpos = self
                .beam_bottom_palette_events
                .first()
                .map(|event| event.vpos)
                .unwrap_or(u32::MAX);
            let pending_vpos = self
                .pending_beam_bottom_palette_events
                .first()
                .map(|event| event.vpos)
                .unwrap_or(u32::MAX);
            if pending_vpos < current_vpos {
                self.beam_bottom_palette_events =
                    std::mem::take(&mut self.pending_beam_bottom_palette_events);
            } else {
                self.pending_beam_bottom_palette_events.clear();
            }
        } else {
            self.beam_bottom_palette_events =
                std::mem::take(&mut self.pending_beam_bottom_palette_events);
        }
    }

    pub(super) fn capture_render_snapshot(&self) -> RenderRegisterSnapshot {
        RenderRegisterSnapshot {
            agnus_revision: self.agnus.revision(),
            harddis: self.harddis_active(),
            dmacon: self.agnus.dmacon,
            bplcon0: self.denise.bplcon0,
            bplcon1: self.denise.bplcon1,
            bplcon2: self.denise.bplcon2,
            bplcon3: self.denise.bplcon3,
            bplcon4: self.denise.bplcon4,
            fmode: self.agnus.fmode(),
            clxcon: self.denise.clxcon,
            clxcon2: self.denise.clxcon2,
            // The render replay models the live BPLxPT counters, which DMA
            // and modulo adds keep advancing; `denise.bplpt` is only the
            // last-written latch for the debugger's register view.
            bplpt: self.display_dma_bplpt,
            bpldat: self.denise.bpldat,
            sprpt: self.denise.sprpt,
            sprpos: self.denise.sprpos,
            sprctl: self.denise.sprctl,
            sprdata: self.denise.sprdata,
            sprdatb: self.denise.sprdatb,
            spr_armed: self.denise.spr_armed,
            spr_hw_pos: self.denise.spr_hw_pos,
            spr_hw_ctl: self.denise.spr_hw_ctl,
            spr_hw_data: self.denise.spr_hw_data,
            spr_hw_datb: self.denise.spr_hw_datb,
            spr_hw_armed: self.denise.spr_hw_armed,
            bpl1mod: self.denise.bpl1mod,
            bpl2mod: self.denise.bpl2mod,
            palette: self.denise.palette,
            diwstrt: self.denise.diwstrt,
            diwstop: self.denise.diwstop,
            diwhigh: self.effective_diwhigh(),
            ddfstrt: self.denise.ddfstrt,
            ddfstop: self.denise.ddfstop,
            // LOF for this frame is settled by update_interlace_long_frame
            // after the wrap; the caller patches it in (see new_frame).
            long_field: self.agnus.lof,
        }
    }

    pub(super) fn note_intreq_palette_target(&mut self, val: u16) {
        if val & 0x8000 != 0 {
            return;
        }
        let clears_coper = val & crate::chipset::paula::INT_COPER != 0;
        let clears_vertb = val & crate::chipset::paula::INT_VERTB != 0;
        let handling_coper = self.delivered_irq_pending & crate::chipset::paula::INT_COPER != 0;
        if clears_coper && handling_coper {
            self.cpu_palette_target = CpuPaletteTarget::Bottom;
            self.cpu_palette_target_writes = 0;
            self.cpu_palette_target_beam = self.delivered_copper_irq_beam;
        } else if clears_vertb {
            self.cpu_palette_target = CpuPaletteTarget::Top;
            self.cpu_palette_target_writes = 0;
            self.cpu_palette_target_beam = None;
        }
        self.delivered_irq_pending &= !(val & 0x7FFF);
        if clears_coper {
            self.delivered_copper_irq_beam = None;
        }
    }

    /// BPLCON3 BANK/LOCT routing for a CPU COLORxx write, as Lisa decodes
    /// it. Pre-AGA Denise has no banking: everything lands on bank 0 with
    /// both nibble planes written.
    pub(super) fn cpu_palette_write_bank_loct(&self) -> (usize, bool) {
        if self.denise_is_lisa() {
            (
                crate::chipset::denise::Palette::bank_from_bplcon3(self.denise.bplcon3),
                crate::chipset::denise::Palette::loct_from_bplcon3(self.denise.bplcon3),
            )
        } else {
            (0, false)
        }
    }

    pub(super) fn write_cpu_palette_snapshot(&mut self, idx: usize, color: u16) {
        // Lisa decodes every COLORxx write against the BPLCON3 latch standing
        // at the write (BANK selects the 32-entry block, LOCT the nibble
        // half), so the CPU-write shadow must decode identically: resolving
        // bank-blind would collapse a banked palette upload onto entries
        // 0..31, and a frame later seeded from this shadow would show bank 0
        // holding the last-written bank with every other bank black.
        let (bank, loct) = self.cpu_palette_write_bank_loct();
        let target = self.cpu_palette_target;
        match target {
            CpuPaletteTarget::Top => {
                self.beam_top_palette.write_banked(bank, idx, loct, color);
            }
            CpuPaletteTarget::Bottom => {
                self.beam_top_palette.write_banked(bank, idx, loct, color);
                // The bottom-palette reconstruction models the classic
                // split-palette pattern: a copper interrupt near the display
                // bottom whose handler rewrites the OCS-visible palette
                // (bank 0, LOCT clear). A banked or low-nibble AGA write is
                // not that pattern; letting one latch the bottom palette
                // would replay wrong values into carry-forward frames.
                if bank == 0 && !loct {
                    let target_vpos = self
                        .cpu_palette_target_beam
                        .map(|(vpos, _)| vpos)
                        .unwrap_or(self.agnus.vpos);
                    if target_vpos >= CPU_COPPER_BOTTOM_PALETTE_MIN_VPOS {
                        self.beam_bottom_palette.write_ocs(idx, color);
                        self.beam_bottom_palette_valid = true;
                    }
                    self.cpu_palette_target_writes =
                        self.cpu_palette_target_writes.saturating_add(1);
                    if idx == 15 || idx == 31 || self.cpu_palette_target_writes >= 16 {
                        self.commit_pending_bottom_palette_events();
                        self.cpu_palette_target = CpuPaletteTarget::Top;
                        self.cpu_palette_target_writes = 0;
                        self.cpu_palette_target_beam = None;
                    }
                }
            }
        }
    }
}
