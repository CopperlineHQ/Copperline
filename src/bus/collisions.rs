// SPDX-License-Identifier: GPL-3.0-or-later

//! Live (beam-timed) collision accumulation: replays sprite/playfield
//! pixels between beam positions to latch CLXDAT bits at the colour
//! clock they occur on real Denise. Split out of `bus.rs` for size;
//! this is the same `Bus`, with full access to its private state.

use super::*;

impl Bus {
    pub(super) fn accumulate_live_collisions_until_current_beam(&mut self) {
        const VISIBLE_END_HPOS: u32 =
            RENDER_COPPER_WAIT_HPOS_FB0 + (RENDER_FRAMEBUFFER_WIDTH as u32 / 4);
        let visible_start_vpos = self.current_frame_visible_start_vpos;
        let visible_end_vpos =
            visible_start_vpos + self.current_frame_geometry.visible_lines as u32;
        let mut end_vpos = self.agnus.vpos;
        let mut end_hpos = self.agnus.hpos.min(VISIBLE_END_HPOS);

        if end_vpos < visible_start_vpos {
            return;
        }
        if end_vpos >= visible_end_vpos {
            end_vpos = visible_end_vpos - 1;
            end_hpos = VISIBLE_END_HPOS;
        }
        if end_hpos <= RENDER_COPPER_WAIT_HPOS_FB0 && end_vpos == visible_start_vpos {
            return;
        }

        let start_vpos = self.lazy_collision_vpos.max(visible_start_vpos);
        if start_vpos > end_vpos {
            return;
        }
        let start_hpos = if self.lazy_collision_vpos < visible_start_vpos {
            RENDER_COPPER_WAIT_HPOS_FB0
        } else {
            self.lazy_collision_hpos.min(VISIBLE_END_HPOS)
        };
        if start_vpos == end_vpos && start_hpos >= end_hpos {
            return;
        }

        for vpos in start_vpos..=end_vpos {
            let old_hpos = if vpos == start_vpos {
                start_hpos
            } else {
                RENDER_COPPER_WAIT_HPOS_FB0
            };
            let new_hpos = if vpos == end_vpos {
                end_hpos
            } else {
                VISIBLE_END_HPOS
            };
            if new_hpos <= old_hpos {
                continue;
            }
            self.accumulate_live_playfield_collisions_if_due(vpos, old_hpos, new_hpos);
            self.accumulate_live_manual_bpl_collisions_if_due(vpos, old_hpos, new_hpos);
            self.accumulate_live_sprite_sprite_collisions_if_due(vpos, old_hpos, new_hpos);
            self.accumulate_live_manual_sprite_collisions_if_due(vpos, old_hpos, new_hpos);
        }

        self.lazy_collision_vpos = end_vpos;
        self.lazy_collision_hpos = end_hpos;
    }

    pub(super) fn accumulate_live_collisions_to_frame_end(&mut self) {
        const VISIBLE_END_HPOS: u32 =
            RENDER_COPPER_WAIT_HPOS_FB0 + (RENDER_FRAMEBUFFER_WIDTH as u32 / 4);
        let visible_start_vpos = self.current_frame_visible_start_vpos;
        let visible_end_vpos =
            visible_start_vpos + self.current_frame_geometry.visible_lines as u32;
        if visible_end_vpos <= visible_start_vpos {
            return;
        }

        let end_vpos = visible_end_vpos - 1;
        let start_vpos = self.lazy_collision_vpos.max(visible_start_vpos);
        if start_vpos > end_vpos {
            return;
        }
        let start_hpos = if self.lazy_collision_vpos < visible_start_vpos {
            RENDER_COPPER_WAIT_HPOS_FB0
        } else {
            self.lazy_collision_hpos.min(VISIBLE_END_HPOS)
        };
        if start_vpos == end_vpos && start_hpos >= VISIBLE_END_HPOS {
            return;
        }

        for vpos in start_vpos..=end_vpos {
            let old_hpos = if vpos == start_vpos {
                start_hpos
            } else {
                RENDER_COPPER_WAIT_HPOS_FB0
            };
            let new_hpos = VISIBLE_END_HPOS;
            if new_hpos <= old_hpos {
                continue;
            }
            self.accumulate_live_playfield_collisions_if_due(vpos, old_hpos, new_hpos);
            self.accumulate_live_manual_bpl_collisions_if_due(vpos, old_hpos, new_hpos);
            self.accumulate_live_sprite_sprite_collisions_if_due(vpos, old_hpos, new_hpos);
            self.accumulate_live_manual_sprite_collisions_if_due(vpos, old_hpos, new_hpos);
        }

        self.lazy_collision_vpos = end_vpos;
        self.lazy_collision_hpos = VISIBLE_END_HPOS;
    }

    pub(super) fn accumulate_live_sprite_sprite_collisions_if_due(
        &mut self,
        vpos: u32,
        old_hpos: u32,
        new_hpos: u32,
    ) {
        let visible_start_vpos = self.current_frame_visible_start_vpos;
        if new_hpos <= RENDER_COPPER_WAIT_HPOS_FB0 {
            return;
        }
        let Some(fb_y) = visible_framebuffer_y(
            vpos,
            visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        ) else {
            return;
        };
        let x_start = old_hpos
            .saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)
            .saturating_mul(4)
            .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32;
        let x_stop = new_hpos
            .saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)
            .saturating_mul(4)
            .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32;
        if x_start >= x_stop {
            return;
        }
        self.ensure_current_frame_sprite_collision_sources_for_y(fb_y, vpos);
        let has_overlapping_source_pair = {
            let sources = self.current_frame_sprite_collision_sources[fb_y]
                .as_deref()
                .unwrap_or(&[]);
            live_sprite_sources_have_group_pair_overlap(sources, x_start, x_stop)
        };
        if !has_overlapping_source_pair {
            return;
        }
        self.ensure_current_collision_control_index();
        let sources = self.current_frame_sprite_collision_sources[fb_y]
            .as_deref()
            .unwrap_or(&[]);
        let started = VideoPipelineStats::probe_timing_sample(
            &mut self.video_pipeline_stats.collision_probes,
            VIDEO_COLLISION_TIMING_SAMPLE_RATE,
        );
        let current_control = LiveCollisionControl::from_current(
            self.agnus.revision(),
            self.denise.bplcon0,
            self.denise.bplcon1,
            self.denise.bplcon3,
            self.denise.clxcon,
            self.denise.clxcon2,
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
            self.denise.ddfstrt,
            self.denise.bpldat,
        );
        let frame_base = self.current_frame_render_base;
        let control_index = self.current_frame_collision_control_index.as_ref().unwrap();
        let control_replay = LiveCollisionLineReplay::from_index(
            current_control,
            frame_base,
            control_index,
            vpos as i32,
        );
        let sprite_display_enable_x =
            self.current_frame_sprite_display_enable_x_by_y[fb_y].map(|x| x as i32);
        let clxdat = live_sprite_sprite_collision_bits(
            sources,
            &control_replay,
            vpos as i32,
            x_start,
            x_stop,
            sprite_display_enable_x,
            self.denise.clxdat,
        );
        self.denise.or_clxdat(clxdat);
        self.record_live_collision_timing(
            (x_stop - x_start) as u64,
            control_replay.segment_count(),
            false,
            started.map(|started| (started.elapsed(), VIDEO_COLLISION_TIMING_SAMPLE_RATE)),
        );
    }

    pub(super) fn accumulate_live_playfield_collisions_if_due(
        &mut self,
        vpos: u32,
        old_hpos: u32,
        new_hpos: u32,
    ) {
        let visible_start_vpos = self.current_frame_visible_start_vpos;
        if new_hpos <= RENDER_COPPER_WAIT_HPOS_FB0 {
            return;
        }
        let Some(fb_y) = visible_framebuffer_y(
            vpos,
            visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        ) else {
            return;
        };
        let x_start = old_hpos
            .saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)
            .saturating_mul(4)
            .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32;
        let x_stop = new_hpos
            .saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)
            .saturating_mul(4)
            .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32;
        if x_start >= x_stop {
            return;
        }
        if self.current_frame_bitplane_rows[fb_y].is_none() {
            return;
        }
        self.ensure_current_frame_sprite_collision_sources_for_y(fb_y, vpos);
        let has_overlapping_sprite_source = {
            let sprite_sources = self.current_frame_sprite_collision_sources[fb_y]
                .as_deref()
                .unwrap_or(&[]);
            sprite_sources
                .iter()
                .any(|source| live_sprite_source_may_overlap_x_range(source, x_start, x_stop))
        };
        let needs_dual_playfield_collision =
            self.live_playfield_collision_may_have_dual_playfield() && self.denise.clxdat & 1 == 0;
        if !has_overlapping_sprite_source && !needs_dual_playfield_collision {
            return;
        }
        self.ensure_current_collision_control_index();
        let sprite_sources = self.current_frame_sprite_collision_sources[fb_y]
            .as_deref()
            .unwrap_or(&[]);
        let started = VideoPipelineStats::probe_timing_sample(
            &mut self.video_pipeline_stats.collision_probes,
            VIDEO_COLLISION_TIMING_SAMPLE_RATE,
        );
        let current_control = LiveCollisionControl::from_current(
            self.agnus.revision(),
            self.denise.bplcon0,
            self.denise.bplcon1,
            self.denise.bplcon3,
            self.denise.clxcon,
            self.denise.clxcon2,
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
            self.denise.ddfstrt,
            self.denise.bpldat,
        );
        let frame_base = self.current_frame_render_base;
        let control_index = self.current_frame_collision_control_index.as_ref().unwrap();
        let control_replay = LiveCollisionLineReplay::from_index(
            current_control,
            frame_base,
            control_index,
            vpos as i32,
        );
        let needs_dual_playfield_collision = needs_dual_playfield_collision
            && control_replay.dual_playfield_in_range(x_start, x_stop);
        if !has_overlapping_sprite_source && !needs_dual_playfield_collision {
            return;
        }
        let sprite_display_enable_x =
            self.current_frame_sprite_display_enable_x_by_y[fb_y].map(|x| x as i32);
        let clxdat = {
            let row = self.current_frame_bitplane_rows[fb_y].as_ref().unwrap();
            let mut bits = 0;
            if needs_dual_playfield_collision {
                bits |= live_bitplane_collision_bits_in_range(
                    row,
                    &control_replay,
                    vpos as i32,
                    x_start,
                    x_stop,
                );
            }
            if has_overlapping_sprite_source {
                bits |= live_sprite_playfield_collision_bits_in_range(
                    row,
                    sprite_sources,
                    &control_replay,
                    &control_replay,
                    vpos as i32,
                    x_start,
                    x_stop,
                    sprite_display_enable_x,
                    self.denise.clxdat,
                );
            }
            bits
        };
        self.denise.or_clxdat(clxdat);
        self.record_live_collision_timing(
            (x_stop - x_start) as u64,
            control_replay.segment_count(),
            false,
            started.map(|started| (started.elapsed(), VIDEO_COLLISION_TIMING_SAMPLE_RATE)),
        );
    }

    pub(super) fn accumulate_live_manual_bpl_collisions_if_due(
        &mut self,
        vpos: u32,
        old_hpos: u32,
        new_hpos: u32,
    ) {
        let visible_start_vpos = self.current_frame_visible_start_vpos;
        if new_hpos <= RENDER_COPPER_WAIT_HPOS_FB0
            || self.current_frame_collision_bpldat_events.is_empty()
        {
            return;
        }
        let Some(fb_y) = visible_framebuffer_y(
            vpos,
            visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        ) else {
            return;
        };
        let x_start = old_hpos
            .saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)
            .saturating_mul(4)
            .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32;
        let x_stop = new_hpos
            .saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)
            .saturating_mul(4)
            .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32;
        if x_start >= x_stop {
            return;
        }
        let current_line_sprite_lines_empty = self.current_frame_sprite_lines_by_y[fb_y].is_empty();
        if current_line_sprite_lines_empty
            && self.current_frame_collision_sprite_events.is_empty()
            && (self.denise.clxdat & 1 != 0
                || !self.live_playfield_collision_may_have_dual_playfield())
        {
            return;
        }
        let started = VideoPipelineStats::probe_timing_sample(
            &mut self.video_pipeline_stats.collision_probes,
            VIDEO_COLLISION_TIMING_SAMPLE_RATE,
        );
        let current_control = LiveCollisionControl::from_current(
            self.agnus.revision(),
            self.denise.bplcon0,
            self.denise.bplcon1,
            self.denise.bplcon3,
            self.denise.clxcon,
            self.denise.clxcon2,
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
            self.denise.ddfstrt,
            self.denise.bpldat,
        );
        let frame_base = self.current_frame_render_base;
        self.ensure_current_collision_control_index();
        let control_index = self.current_frame_collision_control_index.as_ref().unwrap();
        let control_replay = LiveCollisionLineReplay::from_index(
            current_control,
            frame_base,
            control_index,
            vpos as i32,
        );
        if current_line_sprite_lines_empty
            && self.current_frame_collision_sprite_events.is_empty()
            && !control_replay.dual_playfield_in_range(x_start, x_stop)
        {
            return;
        }
        self.ensure_current_collision_bpldat_index();
        self.ensure_current_collision_sprite_index();
        let bpldat_index = self.current_frame_collision_bpldat_index.as_ref().unwrap();
        let sprite_index = self.current_frame_collision_sprite_index.as_ref().unwrap();
        let sprite_display_enable_x =
            self.current_frame_sprite_display_enable_x_by_y[fb_y].map(|x| x as i32);
        let clxdat = live_manual_bpl_collision_bits_in_range(
            frame_base,
            bpldat_index,
            sprite_index,
            &control_replay,
            &self.current_frame_sprite_lines_by_y[fb_y],
            vpos as i32,
            x_start,
            x_stop,
            sprite_display_enable_x,
        );
        self.denise.or_clxdat(clxdat);
        self.record_live_collision_timing(
            (x_stop - x_start) as u64,
            control_replay.segment_count(),
            false,
            started.map(|started| (started.elapsed(), VIDEO_COLLISION_TIMING_SAMPLE_RATE)),
        );
    }

    pub(super) fn accumulate_live_manual_sprite_collisions_if_due(
        &mut self,
        vpos: u32,
        old_hpos: u32,
        new_hpos: u32,
    ) {
        let visible_start_vpos = self.current_frame_visible_start_vpos;
        if new_hpos <= RENDER_COPPER_WAIT_HPOS_FB0
            || self.current_frame_collision_sprite_events.is_empty()
        {
            return;
        }
        let Some(fb_y) = visible_framebuffer_y(
            vpos,
            visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        ) else {
            return;
        };
        let x_start = old_hpos
            .saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)
            .saturating_mul(4)
            .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32;
        let x_stop = new_hpos
            .saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)
            .saturating_mul(4)
            .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32;
        if x_start >= x_stop {
            return;
        }
        let started = VideoPipelineStats::probe_timing_sample(
            &mut self.video_pipeline_stats.collision_probes,
            VIDEO_COLLISION_TIMING_SAMPLE_RATE,
        );
        self.ensure_current_collision_control_index();
        self.ensure_current_collision_sprite_index();
        let row = self.current_frame_bitplane_rows[fb_y].as_ref();
        let current_control = LiveCollisionControl::from_current(
            self.agnus.revision(),
            self.denise.bplcon0,
            self.denise.bplcon1,
            self.denise.bplcon3,
            self.denise.clxcon,
            self.denise.clxcon2,
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
            self.denise.ddfstrt,
            self.denise.bpldat,
        );
        let frame_base = self.current_frame_render_base;
        let control_index = self.current_frame_collision_control_index.as_ref().unwrap();
        let control_replay = LiveCollisionLineReplay::from_index(
            current_control,
            frame_base,
            control_index,
            vpos as i32,
        );
        let sprite_index = self.current_frame_collision_sprite_index.as_ref().unwrap();
        let sprite_display_enable_x =
            self.current_frame_sprite_display_enable_x_by_y[fb_y].map(|x| x as i32);
        let mut clxdat = live_manual_sprite_sprite_collision_bits_in_range(
            frame_base,
            sprite_index,
            &control_replay,
            vpos as i32,
            x_start,
            x_stop,
            sprite_display_enable_x,
            self.denise.clxdat,
        );
        if let Some(row) = row {
            clxdat |= live_manual_sprite_playfield_collision_bits_in_range(
                row,
                frame_base,
                sprite_index,
                &control_replay,
                &control_replay,
                vpos as i32,
                x_start,
                x_stop,
                sprite_display_enable_x,
                self.denise.clxdat | clxdat,
            );
        }
        self.denise.or_clxdat(clxdat);
        self.record_live_collision_timing(
            (x_stop - x_start) as u64,
            control_replay.segment_count(),
            false,
            started.map(|started| (started.elapsed(), VIDEO_COLLISION_TIMING_SAMPLE_RATE)),
        );
    }
}
