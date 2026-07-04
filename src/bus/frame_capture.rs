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
        self.log_bus_accounting_frame();
        self.finish_frame_bus_trace();
        let promote_render_frame = !self.current_frame_render_blocked;
        if promote_render_frame {
            self.last_frame_render_base = Some(self.current_frame_render_base);
            self.last_frame_visible_start_vpos = self.current_frame_visible_start_vpos;
            self.last_frame_geometry = self.current_frame_geometry;
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
        // Promote the just-finished frame's chip-RAM snapshot to `last` by
        // swapping buffers instead of copying 2 MB. `capture_current_frame_
        // display_start` already filled `current_frame_chip_ram` for any frame
        // that reached its display window, so move that buffer across and
        // recycle the old `last` buffer as the next `current`. A frame that
        // never displayed (no capture taken) has no meaningful snapshot, so
        // fall back to a live copy for the renderer's blank/border output.
        if promote_render_frame
            && self.current_frame_display_snapshot_taken
            && self.current_frame_chip_ram.len() == self.mem.chip_ram.len()
        {
            std::mem::swap(
                &mut self.last_frame_chip_ram,
                &mut self.current_frame_chip_ram,
            );
        } else if promote_render_frame {
            self.last_frame_chip_ram.clear();
            self.last_frame_chip_ram
                .extend_from_slice(&self.mem.chip_ram);
        } else {
            self.last_frame_chip_ram.clear();
        }
        let current_bitplane_rows = std::mem::replace(
            &mut self.current_frame_bitplane_rows,
            empty_captured_bitplane_rows(),
        );
        self.last_frame_bitplane_rows = if promote_render_frame {
            current_bitplane_rows
        } else {
            empty_captured_bitplane_rows()
        };
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
        // way real Agnus does. A channel that finished the field (read its
        // terminating descriptor, so `control` is cleared) leaves SPRxPT parked
        // past the consumed list at its DMA frontier; it does NOT snap back to
        // the last value the Copper/CPU wrote into `denise.sprpt`. Seed the next
        // frame's replay from that frontier so a reused descriptor buffer that
        // software rewrites every field is not re-armed from its previous,
        // now-overwritten address before the Copper reloads SPRxPT. Channels
        // still mid-descriptor at frame end keep the written pointer.
        for sprite in 0..8 {
            let state = &self.display_dma_sprite_state[sprite];
            self.sprite_dma_frame_start_ptr[sprite] = match (state.control, state.next_ptr) {
                (None, Some(frontier)) => frontier,
                _ => self.denise.sprpt[sprite],
            };
        }
        self.current_frame_collision_may_have_dual_playfield =
            self.current_frame_render_base.bplcon0 & 0x0400 != 0;
        self.display_dma_bplpt = self.denise.bplpt;
        self.display_dma_sprpt = self.denise.sprpt;
        self.display_dma_sprite_state = [DisplaySpriteDmaState::default(); 8];
        self.display_dma_clipped_rows_advanced = false;
        self.lazy_collision_vpos = self.current_frame_visible_start_vpos;
        self.lazy_collision_hpos = RENDER_COPPER_WAIT_HPOS_FB0;
        self.agnus
            .update_interlace_long_frame(self.denise.bplcon0 & 0x0004 != 0);
        // The snapshot above was captured before the frame wrap toggled
        // LOF; record the settled value for the field about to render.
        self.current_frame_render_base.long_field = self.agnus.lof;
        self.current_frame_geometry = self.compute_frame_geometry();
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

    /// After the offscreen sprite-DMA replay, snapshot any sprite that has
    /// fetched data but whose DMA is now disabled (SPREN cleared): it is being
    /// "held" and will be repainted by Copper SPRxPOS repositioning across the
    /// visible window. The renderer's manual-sprite path consumes these (it can
    /// clip each repositioned segment); the bus bar path is suppressed for them.
    pub(super) fn capture_held_sprites_for_visible_window(&mut self) {
        self.current_frame_held_sprites = [None; 8];
        if self.agnus.dmacon & (DMACON_DMAEN | DMACON_SPREN) == (DMACON_DMAEN | DMACON_SPREN) {
            // Sprite DMA is still active: the normal capture path handles it.
            return;
        }
        for sprite in 0..8 {
            let state = self.display_dma_sprite_state[sprite];
            if !state.data_dma_active {
                continue;
            }
            let Some(line_data) = state.last_line else {
                continue;
            };
            let Some(control) = state.control else {
                continue;
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
                vstart: control.vstart,
                vstop: control.vstop,
            });
        }
    }

    pub(super) fn apply_display_sprite_pointer_low_write(&mut self, sprite: usize) {
        self.apply_display_sprite_pointer_low_write_at(sprite, self.agnus.vpos, self.agnus.hpos);
    }

    pub(super) fn apply_display_sprite_pointer_low_write_at(
        &mut self,
        sprite: usize,
        vpos: u32,
        hpos: u32,
    ) {
        self.apply_display_sprite_pointer_low_write_at_with_dmacon(
            sprite,
            vpos,
            hpos,
            self.agnus.dmacon,
        );
    }

    pub(super) fn apply_display_sprite_pointer_low_write_at_with_dmacon(
        &mut self,
        sprite: usize,
        vpos: u32,
        hpos: u32,
        dmacon: u16,
    ) {
        if sprite >= 8 {
            return;
        }
        let state = self.display_dma_sprite_state[sprite];
        if let Some(control) = state.control {
            let pending_descriptor_not_loaded = state.control_loaded_vpos >= vpos as i32
                || (state.control_loaded_vpos == unset_sprite_control_loaded_vpos()
                    && (vpos as i32) < control.vstart);
            if !state.data_dma_active
                && !control.data_origin_is_register_stream()
                // The descriptor must have loaded earlier in this field before
                // SPRxPT can retarget its data stream. Equal/later loads or an
                // unknown save-state value before VSTART restart POS/CTL.
                && pending_descriptor_not_loaded
            {
                self.display_dma_sprite_state[sprite] = DisplaySpriteDmaState::default();
                return;
            }
        } else if self
            .armed_sprite_pointer_write_can_seed_register_data_stream(sprite, vpos, hpos, dmacon)
        {
            self.latch_display_sprite_register_data_stream_at(sprite, vpos, hpos);
            return;
        }
        self.retarget_display_sprite_dma_pointer_at(sprite, vpos, hpos);
    }

    pub(super) fn armed_sprite_pointer_write_can_seed_register_data_stream(
        &self,
        sprite: usize,
        vpos: u32,
        hpos: u32,
        dmacon: u16,
    ) -> bool {
        if !self.denise.spr_armed[sprite]
            || dmacon & (DMACON_DMAEN | DMACON_SPREN) != (DMACON_DMAEN | DMACON_SPREN)
        {
            return false;
        }

        // The transient Agnus descriptor latch is not always available to this
        // replay path (notably after save-state load), but Denise may still
        // expose an armed sprite word plus retained POS/CTL comparators. Only
        // use that as a data-stream fallback once the beam is in the rendered
        // display area and the channel's descriptor fetch slot for this line
        // has already passed. Frame-start/top-border SPRxPT reloads are normal
        // descriptor setup and must not resurrect stale armed register data.
        if !display_window_contains_vpos(
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
            vpos,
        ) {
            return false;
        }

        let beam_y = vpos as i32;
        let vstart =
            sprite_vstart_from_words(self.denise.sprpos[sprite], self.denise.sprctl[sprite]);
        let raw_vstop = sprite_vstop_from_ctl(self.denise.sprctl[sprite]);
        let vstop = if raw_vstop < vstart {
            self.agnus.current_frame_lines() as i32
        } else {
            raw_vstop
        };
        vpos >= self.current_frame_visible_start_vpos
            && beam_y >= vstart
            && beam_y < vstop
            && hpos >= SPRITE_DMA_PAIR_CAPTURE_HPOS[sprite / 2]
    }

    pub(super) fn latch_display_sprite_register_data_stream_at(
        &mut self,
        sprite: usize,
        vpos: u32,
        hpos: u32,
    ) {
        let frame_lines = self.agnus.current_frame_lines() as i32;
        let beam_y = vpos as i32;
        if beam_y >= frame_lines {
            return;
        }
        let fetch_slot = SPRITE_DMA_PAIR_CAPTURE_HPOS[sprite / 2];
        let stream_start = if hpos >= fetch_slot {
            beam_y.saturating_add(1)
        } else {
            beam_y
        };
        if stream_start >= frame_lines {
            return;
        }

        let quantum = sprite_fetch_quantum(self.agnus.fmode());
        let line_bytes = 4 * quantum;
        let data_lines = (frame_lines - stream_start).max(0) as u32;
        let data_base = self.display_dma_sprpt[sprite] & self.chip_dma_mask & !1;
        let control = DisplaySpriteControl {
            vstart: stream_start,
            vstop: frame_lines,
            hstart: sprite_hstart_from_words(
                self.denise.sprpos[sprite],
                self.denise.sprctl[sprite],
            ),
            hsub_70ns: bitplane_shres(self.denise.bplcon0)
                && sprite_hsub_70ns_from_ctl(self.denise.sprctl[sprite]),
            data_vstart: register_sprite_data_vstart(),
            data_base,
            next_ptr: data_base.wrapping_add(data_lines.saturating_mul(line_bytes))
                & self.chip_dma_mask
                & !1,
            attached: self.denise.sprctl[sprite] & 0x0080 != 0,
        };
        self.display_dma_sprite_state[sprite] = DisplaySpriteDmaState {
            control: Some(control),
            control_loaded_vpos: stream_start,
            next_ptr: Some(control.next_ptr),
            terminated: false,
            data_dma_active: false,
            last_line: None,
        };
    }

    pub(super) fn latch_display_sprite_dma_control_from_registers(
        &mut self,
        sprite: usize,
        write: SpriteControlRegisterWrite,
    ) {
        if sprite >= 8 {
            return;
        }
        self.latch_display_sprite_dma_control_from_words_at(
            sprite,
            self.denise.sprpos[sprite],
            self.denise.sprctl[sprite],
            self.agnus.vpos,
            self.agnus.hpos,
            self.agnus.dmacon,
            write,
        );
    }

    pub(super) fn latch_display_sprite_dma_control_from_words_at(
        &mut self,
        sprite: usize,
        pos: u16,
        ctl: u16,
        vpos: u32,
        hpos: u32,
        dmacon: u16,
        write: SpriteControlRegisterWrite,
    ) {
        if sprite >= 8 {
            return;
        }

        let mut state = self.display_dma_sprite_state[sprite];
        let previous_control = state.control;
        let beam_y = vpos as i32;

        if matches!(write, SpriteControlRegisterWrite::Pos)
            && state.data_dma_active
            && previous_control
                .map(|previous| beam_y < previous.vstop)
                .unwrap_or(false)
        {
            if let Some(mut control) = previous_control {
                // SPRxPOS retimes the horizontal comparator, but it does not
                // re-fetch POS/CTL or cancel an already-enabled sprite DMA
                // stream. Keep the DMA descriptor's stop/data origin latched;
                // the HSTART low bit still comes from the previously latched
                // CTL word.
                control.hstart = (((pos & 0x00FF) << 1) as i32) | (control.hstart & 1);
                state.control = Some(control);
                state.next_ptr = Some(control.next_ptr);
                state.terminated = false;
                state.data_dma_active = true;
                self.display_dma_sprite_state[sprite] = state;
                return;
            }
        }

        let vstart = sprite_vstart_from_words(pos, ctl);
        let raw_vstop = sprite_vstop_from_ctl(ctl);
        let vstop = if raw_vstop < vstart {
            self.agnus.current_frame_lines() as i32
        } else {
            raw_vstop
        };
        let height = vstop - vstart;
        if height <= 0 {
            self.display_dma_sprite_state[sprite] = DisplaySpriteDmaState::default();
            return;
        }

        let quantum = sprite_fetch_quantum(self.agnus.fmode());
        let line_bytes = 4 * quantum;
        let data_lines = if sprite_scan_doubled(self.agnus.fmode()) {
            (height as u32).div_ceil(2)
        } else {
            height as u32
        };
        let data_base = self.display_dma_sprpt[sprite] & self.chip_dma_mask & !1;
        let mut control = DisplaySpriteControl {
            vstart,
            vstop,
            hstart: sprite_hstart_from_words(pos, ctl),
            hsub_70ns: bitplane_shres(self.denise.bplcon0) && sprite_hsub_70ns_from_ctl(ctl),
            data_vstart: register_sprite_data_vstart(),
            data_base,
            next_ptr: data_base.wrapping_add(data_lines.saturating_mul(line_bytes))
                & self.chip_dma_mask
                & !1,
            attached: ctl & 0x0080 != 0,
        };

        let in_window = beam_y >= control.vstart && beam_y < control.vstop;
        let sprite_dma_enabled =
            dmacon & (DMACON_DMAEN | DMACON_SPREN) == (DMACON_DMAEN | DMACON_SPREN);
        let reaches_current_fetch_slot = beam_y == control.vstart
            && hpos <= SPRITE_DMA_PAIR_CAPTURE_HPOS[sprite / 2]
            && sprite_dma_enabled;
        let keep_held_line =
            !sprite_dma_enabled && in_window && state.data_dma_active && state.last_line.is_some();
        let keep_active_dma_line =
            sprite_dma_enabled && in_window && state.data_dma_active && state.last_line.is_some();
        let keep_pending_dma_origin = sprite_dma_enabled
            && !state.data_dma_active
            && state.last_line.is_none()
            && previous_control
                .map(|previous| beam_y < previous.vstop)
                .unwrap_or(false);

        if keep_held_line || keep_active_dma_line {
            if let Some(previous_control) = previous_control {
                control.data_vstart = previous_control.effective_data_vstart();
                control.data_base = previous_control.data_base;
                control.next_ptr = previous_control.next_ptr;
            }
        } else if keep_pending_dma_origin {
            if let Some(previous_control) = previous_control {
                // A pending descriptor has already consumed POS/CTL; direct
                // control writes retime the comparators, not the data stream.
                control.data_vstart = previous_control.data_vstart;
                control.data_base = previous_control.data_base;
                control.next_ptr = control
                    .data_base
                    .wrapping_add(data_lines.saturating_mul(line_bytes))
                    & self.chip_dma_mask
                    & !1;
            }
        }

        state.control = Some(control);
        state.control_loaded_vpos = beam_y;
        state.next_ptr = Some(control.next_ptr);
        state.terminated = false;
        state.data_dma_active =
            in_window && (reaches_current_fetch_slot || keep_held_line || keep_active_dma_line);
        if !keep_held_line && !keep_active_dma_line {
            state.last_line = None;
        }
        self.display_dma_sprite_state[sprite] = state;
    }

    pub(super) fn retarget_display_sprite_dma_pointer_at(
        &mut self,
        sprite: usize,
        vpos: u32,
        hpos: u32,
    ) {
        if sprite >= 8 {
            return;
        }

        let mut state = self.display_dma_sprite_state[sprite];
        let Some(mut control) = state.control else {
            self.display_dma_sprite_state[sprite] = DisplaySpriteDmaState::default();
            return;
        };

        let beam_y = vpos as i32;
        if beam_y >= control.vstop {
            self.display_dma_sprite_state[sprite] = DisplaySpriteDmaState::default();
            return;
        }

        let quantum = sprite_fetch_quantum(self.agnus.fmode());
        let line_bytes = 4 * quantum;
        let mut line = if beam_y <= control.vstart {
            0
        } else {
            (beam_y - control.vstart) as u32
        };
        if beam_y >= control.vstart && hpos > SPRITE_DMA_PAIR_CAPTURE_HPOS[sprite / 2] {
            line = line.saturating_add(1);
        }
        let line = if sprite_scan_doubled(self.agnus.fmode()) {
            line.div_ceil(2)
        } else {
            line
        };

        let ptr = self.display_dma_sprpt[sprite] & self.chip_dma_mask & !1;
        control.data_base =
            ptr.wrapping_sub(line.saturating_mul(line_bytes)) & self.chip_dma_mask & !1;
        control.data_vstart = if control.data_origin_is_register_stream() {
            register_sprite_data_vstart()
        } else {
            control.vstart
        };
        let height = (control.vstop - control.vstart).max(0) as u32;
        let data_lines = if sprite_scan_doubled(self.agnus.fmode()) {
            height.div_ceil(2)
        } else {
            height
        };
        control.next_ptr = control
            .data_base
            .wrapping_add(data_lines.saturating_mul(line_bytes))
            & self.chip_dma_mask
            & !1;
        state.control = Some(control);
        state.next_ptr = Some(control.next_ptr);
        state.terminated = false;
        state.data_dma_active = beam_y >= control.vstart && beam_y < control.vstop;
        state.last_line = None;
        self.display_dma_sprite_state[sprite] = state;

        if diag_sprcap().is_some() {
            log::info!(
                "sprptr f={} v={} h={} s{} ptr={:06X} vstart={} vstop={} hstart={} line={} data_base={:06X} next={:06X}",
                self.emulated_frames,
                vpos,
                hpos,
                sprite,
                ptr,
                control.vstart,
                control.vstop,
                control.hstart,
                line,
                control.data_base,
                control.next_ptr
            );
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
        if display_start != self.agnus.vpos
            || !display_window_contains_vpos(
                self.denise.diwstrt,
                self.denise.diwstop,
                self.effective_diwhigh(),
                self.agnus.vpos,
            )
        {
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
            let nplanes = BitplaneMode::from_bplcon0(bplcon0, self.aga_enabled()).dma_planes();
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
        let display_start = self.display_start_vpos_for_current_control();
        if display_start == 0 {
            return;
        }

        // Sprite DMA runs from the top of the frame, independent of the bitplane
        // display window. The frame snapshot is taken at the DIW-derived display
        // start, which may be below the fixed standard-frame overscan render top,
        // so replay sprite DMA up to that snapshot line and capture any top-border
        // sprite lines along the way. Crucially, SPREN can be toggled within the
        // frame -- software may enable sprite DMA only briefly off-screen to load
        // reused sprites, then clear it before the visible window and reposition
        // the held sprites per line. Replay this frame's DMACON and SPRxPT writes
        // across the span and run the sprite fetch only on lines where SPREN was
        // actually enabled, rather than sampling registers at the display start.
        let base = self.current_frame_render_base;
        // Seed from the previous field's carried SPRxPT frontier rather than the
        // last Copper/CPU write captured in `base.sprpt`. See
        // `sprite_dma_frame_start_ptr` for why finished channels must not snap
        // back to their stale descriptor address.
        self.current_frame_sprite_lines
            .retain(|line| line.beam_y >= display_start as i32);
        for lines in &mut self.current_frame_sprite_lines_by_y {
            lines.retain(|line| line.beam_y >= display_start as i32);
        }
        self.current_frame_sprite_collision_sources = empty_sprite_collision_sources();
        self.current_frame_sprite_display_enable_x_by_y = empty_sprite_display_enable_x_by_y();
        self.current_frame_sprite_dma_observed = !self.current_frame_sprite_lines.is_empty();
        self.display_dma_sprpt = self.sprite_dma_frame_start_ptr;
        self.display_dma_sprite_state = [DisplaySpriteDmaState::default(); 8];
        let mut dmacon = base.dmacon;
        let writes: Vec<(u32, u32, u16, u16)> = self
            .current_render_events()
            .iter()
            .filter(|w| {
                let off = w.offset & 0x01FE;
                w.vpos < display_start && (off == 0x096 || (0x120..=0x13F).contains(&off))
            })
            .map(|w| (w.vpos, w.hpos, w.offset & 0x01FE, w.value))
            .collect();
        let mut idx = 0;
        for vpos in 0..display_start {
            for (pair, &capture_hpos) in SPRITE_DMA_PAIR_CAPTURE_HPOS.iter().enumerate() {
                while idx < writes.len()
                    && (writes[idx].0 < vpos
                        || (writes[idx].0 == vpos && writes[idx].1 < capture_hpos))
                {
                    let (event_vpos, event_hpos, offset, value) = writes[idx];
                    self.apply_sprite_dma_replay_write(
                        offset,
                        value,
                        event_vpos,
                        event_hpos,
                        &mut dmacon,
                    );
                    idx += 1;
                }

                if dmacon & (DMACON_DMAEN | DMACON_SPREN) != (DMACON_DMAEN | DMACON_SPREN) {
                    continue;
                }
                if self.sprite_dma_inhibited_by_vertical_blank_at(vpos) {
                    continue;
                }
                for sprite in pair * 2..pair * 2 + 2 {
                    if sprite_dma_disabled_by_bitplane_ddf(
                        sprite,
                        self.agnus.revision(),
                        self.effective_bitplane_bplcon0(),
                        self.agnus.fmode(),
                        self.effective_bitplane_dmacon(),
                        self.denise.ddfstrt,
                        self.denise.ddfstop,
                        self.harddis_active(),
                    ) {
                        continue;
                    }
                    let _ = self.captured_sprite_line_at(sprite, vpos);
                }
            }
            while idx < writes.len() && writes[idx].0 == vpos {
                let (event_vpos, event_hpos, offset, value) = writes[idx];
                self.apply_sprite_dma_replay_write(
                    offset,
                    value,
                    event_vpos,
                    event_hpos,
                    &mut dmacon,
                );
                idx += 1;
            }
        }
    }

    pub(super) fn apply_sprite_dma_replay_write(
        &mut self,
        offset: u16,
        value: u16,
        vpos: u32,
        hpos: u32,
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
            self.apply_display_sprite_pointer_low_write_at_with_dmacon(idx, vpos, hpos, *dmacon);
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
    ) {
        // No sprite DMA pair slot lies in [old_hpos, new_hpos): nothing below
        // can run (the per-pair loop checks the same window), so skip the
        // sprite-state scan on the vast majority of beam advances.
        if old_hpos > SPRITE_DMA_PAIR_CAPTURE_HPOS[3] || new_hpos <= SPRITE_DMA_PAIR_CAPTURE_HPOS[0]
        {
            return;
        }
        if self.sprite_dma_inhibited_by_vertical_blank_at(vpos) {
            return;
        }
        let sprite_dma_enabled =
            self.agnus.dmacon & (DMACON_DMAEN | DMACON_SPREN) == (DMACON_DMAEN | DMACON_SPREN);
        let sprite_vertical_bar_active = self
            .display_dma_sprite_state
            .iter()
            .any(|state| state.data_dma_active && state.last_line.is_some());
        if !sprite_dma_enabled && !sprite_vertical_bar_active {
            return;
        }
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
        for (pair, &capture_hpos) in SPRITE_DMA_PAIR_CAPTURE_HPOS.iter().enumerate() {
            if old_hpos > capture_hpos || new_hpos <= capture_hpos {
                continue;
            }
            if sprite_dma_enabled {
                pair_slots += 1;
            }
            let mut captured_line = false;
            for sprite in pair * 2..pair * 2 + 2 {
                if sprite_dma_disabled_by_bitplane_ddf(
                    sprite,
                    self.agnus.revision(),
                    bitplane_bplcon0,
                    self.agnus.fmode(),
                    bitplane_dmacon,
                    self.denise.ddfstrt,
                    self.denise.ddfstop,
                    self.harddis_active(),
                ) {
                    continue;
                }
                let line = if sprite_dma_enabled {
                    self.captured_sprite_line_at(sprite, vpos)
                } else {
                    self.captured_sprite_vertical_bar_line_at(sprite, vpos)
                };
                if let Some(line) = line {
                    // COPPERLINE_DIAG_SPRCAP=BEAMY|all: log captured sprite
                    // lines on that beam line (frame, channel, position, words,
                    // and the chip-RAM descriptor/data addresses they fetched).
                    if let Some(want) = diag_sprcap() {
                        if diag_sprcap_matches(want, line.beam_y) {
                            let st = &self.display_dma_sprite_state[sprite];
                            let ctl = st.control;
                            log::info!(
                                "sprcap f={} s{} y={} hstart={} hsub={} att={} w={} A={:04X} {:04X?} B={:04X} {:04X?} ctl=({},{},{},{:06X}) data_base={:06X} next={:06X?}",
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
                                ctl.map(|c| c.vstart).unwrap_or(-1),
                                ctl.map(|c| c.vstop).unwrap_or(-1),
                                ctl.map(|c| c.effective_data_vstart()).unwrap_or(-1),
                                ctl.map(|c| c.next_ptr).unwrap_or(0),
                                st.control.map(|c| c.data_base).unwrap_or(0),
                                st.next_ptr
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
                ));
        }
    }

    pub(super) fn captured_sprite_line_at(
        &mut self,
        sprite: usize,
        vpos: u32,
    ) -> Option<CapturedSpriteLine> {
        let ram_len = self.mem.chip_ram.len();
        if ram_len == 0 {
            return None;
        }
        let ram_mask = self.chip_dma_mask;
        let beam_y = vpos as i32;
        let mut state = self.display_dma_sprite_state[sprite];
        let mut descriptor_can_match_current_vstart =
            state.control.is_some() || state.next_ptr.is_none();
        let mut descriptor_loaded_after_stop_this_line = false;

        // Loop-detection scratch for the descriptor chain below. A plain
        // Vec with a linear `contains` check is used instead of a HashSet:
        // chains are almost always 1-2 descriptors long in practice (this
        // runs per active sprite per DMA-pair capture point per scanline),
        // so a linear scan avoids both the heap allocation pattern and the
        // hashing overhead a HashSet would pay for such a tiny set, while
        // keeping the exact same "any previously-visited pointer" cycle
        // check (not just a fixed lookback window).
        let mut visited_descriptor_ptrs: Vec<u32> = Vec::new();
        loop {
            if state.terminated {
                self.display_dma_sprite_state[sprite] = state;
                return None;
            }

            if let Some(control) = state.control {
                if beam_y >= control.vstop {
                    state.next_ptr = Some(control.next_ptr);
                    state.control = None;
                    state.data_dma_active = false;
                    state.last_line = None;
                    descriptor_can_match_current_vstart = false;
                    descriptor_loaded_after_stop_this_line = true;
                } else if !state.data_dma_active {
                    if beam_y == control.vstart {
                        state.data_dma_active = true;
                    } else {
                        self.display_dma_sprite_state[sprite] = state;
                        return None;
                    }
                }

                if let Some(control) = state.control {
                    if !state.data_dma_active {
                        self.display_dma_sprite_state[sprite] = state;
                        return None;
                    }
                    let quantum = sprite_fetch_quantum(self.agnus.fmode());
                    // SSCAN2 fetches sprite data only on every second display
                    // line; the in-between line redisplays the same data.
                    let mut line = (beam_y - control.effective_data_vstart()) as u32;
                    if sprite_scan_doubled(self.agnus.fmode()) {
                        line /= 2;
                    }
                    let line_bytes = 4 * quantum;
                    let data_ptr = control
                        .data_base
                        .wrapping_add(line.saturating_mul(line_bytes))
                        & ram_mask
                        & !1;
                    let datb_ptr = data_ptr.wrapping_add(2 * quantum);
                    let mut data_ext = [0u16; 3];
                    let mut datb_ext = [0u16; 3];
                    for w in 1..quantum as usize {
                        data_ext[w - 1] = read_chip_word_wrapping(
                            &self.mem.chip_ram,
                            data_ptr.wrapping_add(2 * w as u32),
                        );
                        datb_ext[w - 1] = read_chip_word_wrapping(
                            &self.mem.chip_ram,
                            datb_ptr.wrapping_add(2 * w as u32),
                        );
                    }
                    let line_data = DisplaySpriteLineData {
                        hstart: control.hstart,
                        hsub_70ns: control.hsub_70ns,
                        data: read_chip_word_wrapping(&self.mem.chip_ram, data_ptr),
                        datb: read_chip_word_wrapping(&self.mem.chip_ram, datb_ptr),
                        data_ext,
                        datb_ext,
                        width_words: quantum as u8,
                        attached: control.attached,
                    };
                    state.last_line = Some(line_data);
                    self.display_dma_sprite_state[sprite] = state;
                    return Some(CapturedSpriteLine {
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
                    });
                }
            }

            let descriptor_ptr =
                state.next_ptr.unwrap_or(self.display_dma_sprpt[sprite]) & ram_mask & !1;
            if visited_descriptor_ptrs.contains(&descriptor_ptr) {
                state.terminated = true;
                state.data_dma_active = false;
                state.last_line = None;
                self.display_dma_sprite_state[sprite] = state;
                return None;
            }
            visited_descriptor_ptrs.push(descriptor_ptr);

            // AGA wide fetches also widen the control-word slots: POS is the
            // first word of the first fetch, CTL the first word of the second.
            let quantum = sprite_fetch_quantum(self.agnus.fmode());
            let mut ptr = descriptor_ptr;
            let pos = read_chip_word_wrapping(&self.mem.chip_ram, ptr);
            let ctl = read_chip_word_wrapping(&self.mem.chip_ram, ptr.wrapping_add(2 * quantum));
            ptr = ptr.wrapping_add(4 * quantum) & ram_mask & !1;
            if pos == 0 && ctl == 0 {
                state.terminated = true;
                state.data_dma_active = false;
                state.last_line = None;
                self.display_dma_sprite_state[sprite] = state;
                return None;
            }

            let vstart = sprite_vstart_from_words(pos, ctl);
            let raw_vstop = sprite_vstop_from_ctl(ctl);
            // An inverted vertical pair (vstop < vstart) does not disable the
            // sprite. Agnus arms it at vstart; its vstop comparator already
            // passed for this field, so it does not fire again until the field
            // wraps, and the sprite keeps fetching data to the bottom of the
            // frame. Clamp the effective vstop to the frame bottom -- the
            // per-field VBLANK reset re-fetches this descriptor, which covers
            // the 0..vstop wrap tail on the next field. Treating vstop<vstart
            // as "off" drops full-height strips that are intentionally reused
            // and repositioned every line by SPRxPOS writes.
            let vstop = if raw_vstop < vstart {
                self.agnus.current_frame_lines() as i32
            } else {
                raw_vstop
            };
            let height = vstop - vstart;
            if height <= 0 {
                // Equal start/stop descriptors idle the sprite stream for
                // this field. Do not scan onward into the following words:
                // they are often bitmap data for a later rearmed sprite.
                state.terminated = true;
                state.control = None;
                state.data_dma_active = false;
                state.last_line = None;
                self.display_dma_sprite_state[sprite] = state;
                return None;
            }

            // With SSCAN2 each fetched data line covers two display lines,
            // so the descriptor consumes only ceil(height/2) data lines.
            let data_lines = if sprite_scan_doubled(self.agnus.fmode()) {
                (height as u32).div_ceil(2)
            } else {
                height as u32
            };
            let mut control = DisplaySpriteControl {
                vstart,
                vstop,
                hstart: sprite_hstart_from_words(pos, ctl),
                hsub_70ns: bitplane_shres(self.denise.bplcon0) && sprite_hsub_70ns_from_ctl(ctl),
                data_vstart: vstart,
                data_base: ptr,
                next_ptr: ptr.wrapping_add(data_lines.saturating_mul(4 * quantum)) & ram_mask & !1,
                attached: ctl & 0x0080 != 0,
            };
            let defer_start_until_next_line =
                descriptor_loaded_after_stop_this_line && beam_y == control.vstart;
            if defer_start_until_next_line {
                control.data_vstart = beam_y + 1;
                let remaining_height = (control.vstop - control.data_vstart).max(0) as u32;
                let remaining_data_lines = if sprite_scan_doubled(self.agnus.fmode()) {
                    remaining_height.div_ceil(2)
                } else {
                    remaining_height
                };
                control.next_ptr = control
                    .data_base
                    .wrapping_add(remaining_data_lines.saturating_mul(4 * quantum))
                    & ram_mask
                    & !1;
            }
            if let Some(want) = diag_sprcap() {
                if diag_sprcap_matches(want, beam_y) {
                    log::info!(
                        "sprdesc f={} s{} y={} ptr={:06X} pos={:04X} ctl={:04X} vstart={} vstop={} raw_vstop={} hstart={} att={} data_base={:06X} data_vstart={} next={:06X} can_match={} start_now={} defer_start={}",
                        self.emulated_frames,
                        sprite,
                        beam_y,
                        descriptor_ptr,
                        pos,
                        ctl,
                        control.vstart,
                        control.vstop,
                        raw_vstop,
                        control.hstart,
                        u8::from(control.attached),
                        control.data_base,
                        control.data_vstart,
                        control.next_ptr,
                        u8::from(descriptor_can_match_current_vstart),
                        u8::from(beam_y == control.vstart && descriptor_can_match_current_vstart),
                        u8::from(defer_start_until_next_line),
                    );
                }
            }

            state.control = Some(control);
            state.control_loaded_vpos = beam_y;
            state.data_dma_active = false;
            state.last_line = None;
            if beam_y < control.vstart {
                self.display_dma_sprite_state[sprite] = state;
                return None;
            }
            if defer_start_until_next_line {
                state.data_dma_active = true;
                self.display_dma_sprite_state[sprite] = state;
                return None;
            }
            if beam_y == control.vstart && descriptor_can_match_current_vstart {
                state.data_dma_active = true;
                continue;
            }
            if beam_y < control.vstop {
                self.display_dma_sprite_state[sprite] = state;
                return None;
            }
        }
    }

    pub(super) fn captured_sprite_vertical_bar_line_at(
        &mut self,
        sprite: usize,
        vpos: u32,
    ) -> Option<CapturedSpriteLine> {
        // Sprites captured as "held" at the visible start are repainted by the
        // renderer's manual-sprite path (which clips each Copper-repositioned
        // segment), so do not also emit a full-width bar for them here.
        if self.current_frame_held_sprites[sprite].is_some() {
            return None;
        }
        let beam_y = vpos as i32;
        let mut state = self.display_dma_sprite_state[sprite];
        let control = state.control?;
        if beam_y >= control.vstop {
            state.next_ptr = Some(control.next_ptr);
            state.control = None;
            state.data_dma_active = false;
            state.last_line = None;
            self.display_dma_sprite_state[sprite] = state;
            return None;
        }
        if beam_y < control.vstart || !state.data_dma_active {
            self.display_dma_sprite_state[sprite] = state;
            return None;
        }
        let line_data = state.last_line?;
        self.display_dma_sprite_state[sprite] = state;
        // Position the held strip at the sprite's *current* SPRxPOS/CTL, not
        // the fetch-time hstart: with sprite DMA off the Copper (or CPU) can
        // reposition a reused sprite by rewriting SPRxPOS, so the held data
        // must follow it. For a sprite left where the DMA fetched it this is
        // the same value.
        let pos = self.denise.sprpos[sprite];
        let ctl = self.denise.sprctl[sprite];
        Some(CapturedSpriteLine {
            sprite,
            hstart: sprite_hstart_from_words(pos, ctl),
            hsub_70ns: bitplane_shres(self.denise.bplcon0) && sprite_hsub_70ns_from_ctl(ctl),
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
        if !display_window_contains_vpos(
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
            vpos,
        ) {
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
        let quantum = bitplane_fetch_quantum(fmode) as usize;
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
        let anchor_mode = BitplaneMode::from_bplcon0(anchor_bplcon0, self.aga_enabled());
        let anchor_dma_planes = anchor_mode.dma_planes();
        let period = bitplane_fetch_period(anchor_bplcon0, fmode);
        let unit = bitplane_fetch_unit(anchor_bplcon0, fmode);
        let started = VideoPipelineStats::probe_timing_sample(
            &mut self.video_pipeline_stats.bitplane_fetch_probes,
            VIDEO_FETCH_TIMING_SAMPLE_RATE,
        );
        let words_per_row = bitplane_words_per_row(
            self.agnus.revision(),
            anchor_bplcon0,
            self.agnus.fmode(),
            self.denise.ddfstrt,
            self.denise.ddfstop,
            self.harddis_active(),
        );
        let mut rows_started = 0usize;
        let mut slots = 0usize;
        let mut line_complete = false;
        let mut line_complete_plane_mask = 0u16;
        let addr_mask = self.chip_dma_mask;
        let hires_like = bitplane_hires(anchor_bplcon0) || bitplane_shres(anchor_bplcon0);
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
                let block_dma_planes = block_mode.dma_planes();
                if block_dma_planes == 0 {
                    continue;
                }
                let block_display_planes = block_mode.display_planes();
                for plane in 0..block_dma_planes.min(8) {
                    if plane == 0 {
                        self.record_sprite_display_enable_for_bitplane_dma(vpos);
                    }
                    for w in 0..quantum.min(words_per_row - word_base) {
                        let word_idx = word_base + w;
                        let addr = self.display_dma_bplpt[plane] & addr_mask;
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
                        self.display_dma_bplpt[plane] =
                            self.display_dma_bplpt[plane].wrapping_add(2) & addr_mask;
                        if word_idx == last_word_idx {
                            line_complete = true;
                            line_complete_plane_mask = plane_mask_for_count(block_dma_planes);
                        }
                    }
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
                let block_dma_planes = block_mode.dma_planes();
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
                    for w in 0..quantum.min(words_per_row - word_base) {
                        let word_idx = word_base + w;
                        let addr = self.display_dma_bplpt[plane] & addr_mask;
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
                        self.display_dma_bplpt[plane] =
                            self.display_dma_bplpt[plane].wrapping_add(2) & addr_mask;
                        if word_idx == last_word_idx && order == block_last_order {
                            line_complete = true;
                            line_complete_plane_mask = plane_mask_for_count(block_dma_planes);
                        }
                    }
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
            let mut row = CapturedBitplaneRow {
                nplanes: display_planes,
                words_per_row,
                planes: std::array::from_fn(|_| vec![0; words_per_row]),
            };
            for plane in dma_planes..display_planes {
                row.planes[plane].fill(self.denise.bpldat[plane]);
            }
            if let Some(old_row) = old_row {
                let copy_planes = old_row.nplanes.min(display_planes).min(8);
                let copy_words = old_row.words_per_row.min(words_per_row);
                for plane in 0..copy_planes {
                    row.planes[plane][..copy_words]
                        .copy_from_slice(&old_row.planes[plane][..copy_words]);
                }
            }
            self.current_frame_bitplane_rows[fb_y] = Some(row);
        }
        if let Some(row) = self.current_frame_bitplane_rows[fb_y].as_mut() {
            row.planes[plane][word_idx] = fetched;
        }
        row_needs_init
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
        let (vpos, hpos) = (self.agnus.vpos, self.agnus.hpos);
        let event = BeamRegisterWrite {
            vpos,
            hpos,
            offset,
            value,
            source,
        };
        if matches!(source, BeamWriteSource::CpuCopperIrq)
            && matches!(offset & 0x01FE, 0x180..=0x1BE)
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
        if is_live_collision_control_custom_write(offset) {
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
            bplpt: self.denise.bplpt,
            bpldat: self.denise.bpldat,
            sprpt: self.denise.sprpt,
            sprpos: self.denise.sprpos,
            sprctl: self.denise.sprctl,
            sprdata: self.denise.sprdata,
            sprdatb: self.denise.sprdatb,
            spr_armed: self.denise.spr_armed,
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

    pub(super) fn write_cpu_palette_snapshot(&mut self, idx: usize, color: u16) {
        let target = self.cpu_palette_target;
        match target {
            CpuPaletteTarget::Top => {
                self.beam_top_palette.write_ocs(idx, color);
            }
            CpuPaletteTarget::Bottom => {
                self.beam_top_palette.write_ocs(idx, color);
                let target_vpos = self
                    .cpu_palette_target_beam
                    .map(|(vpos, _)| vpos)
                    .unwrap_or(self.agnus.vpos);
                if target_vpos >= CPU_COPPER_BOTTOM_PALETTE_MIN_VPOS {
                    self.beam_bottom_palette.write_ocs(idx, color);
                    self.beam_bottom_palette_valid = true;
                }
                self.cpu_palette_target_writes = self.cpu_palette_target_writes.saturating_add(1);
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
