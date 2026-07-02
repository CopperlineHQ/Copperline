// SPDX-License-Identifier: GPL-3.0-or-later

//! Bitplane fetch geometry: DDF-derived fetch order, per-word fetch
//! hpos, and the per-line fetch plans that map DMA words to output
//! pixels. Split out of `bitplane.rs` for size; same module family,
//! full access to the parent's private items.

use super::*;

pub(super) fn fetch_word_index_active_at_hpos(
    control: ControlState,
    word_idx: usize,
    hpos: u32,
) -> bool {
    if !control.bitplane_dma_enabled() || !control.has_valid_ddf_window() {
        return false;
    }
    let native_w = native_frame_width_for_control(control);
    if word_idx >= control.words_per_row(native_w) {
        return false;
    }
    let Some((start, _stop)) = effective_ddf_window(
        control.agnus_revision,
        control.hires() || control.shres(),
        control.ddfstrt,
        control.ddfstop,
        control.harddis,
    ) else {
        return false;
    };
    let start = u32::from(start);
    if hpos < start {
        return false;
    }
    let rel = hpos - start;
    let step = control.fetch_cck_per_word();
    rel.is_multiple_of(step) && (rel / step) == word_idx as u32
}

pub(super) fn bitplane_fetch_order(control: ControlState, plane: usize) -> u32 {
    if control.hires() || control.shres() {
        return plane as u32;
    }

    let plane_num = plane + 1;
    OCS_LORES_BPL_SEQUENCE
        .iter()
        .position(|&candidate| candidate == plane_num)
        .unwrap_or(7) as u32
}

pub(super) fn native_frame_width_for_control(control: ControlState) -> usize {
    if control.shres() {
        FB_WIDTH * 2
    } else if control.hires() {
        FB_WIDTH
    } else {
        FB_WIDTH / 2
    }
}

pub(super) fn bitplane_fetch_hpos_for_plane(
    control: ControlState,
    word_idx: usize,
    plane: usize,
) -> u32 {
    let start = u32::from(effective_ddf_start_hpos(
        control.agnus_revision,
        control.hires() || control.shres(),
        control.ddfstrt,
    ));
    let group = word_idx as u32 / control.fetch_quantum();
    if control.hires() || control.shres() {
        return start + group.saturating_mul(control.fetch_period());
    }

    let unit = control.fetch_unit();
    start + group.saturating_mul(unit) + bitplane_fetch_order(control, plane)
}

pub(super) fn fetch_plane_word_active_at_hpos(
    control: ControlState,
    word_idx: usize,
    plane: usize,
    hpos: u32,
) -> bool {
    if plane >= control.dma_planes().min(8) || !control.bitplane_dma_enabled() {
        return false;
    }
    let native_w = native_frame_width_for_control(control);
    if !control.has_valid_ddf_window() || word_idx >= control.words_per_row(native_w) {
        return false;
    }
    bitplane_fetch_hpos_for_plane(control, word_idx, plane) == hpos
}

#[derive(Clone, Copy)]
pub(super) struct LineFetchPlan {
    pub(super) word_fetch_hpos: Option<u32>,
    pub(super) fetches: [(u32, usize); 8],
    pub(super) len: usize,
}

impl LineFetchPlan {
    pub(super) fn empty() -> Self {
        Self {
            word_fetch_hpos: None,
            fetches: [(0, 0); 8],
            len: 0,
        }
    }

    pub(super) fn push(&mut self, hpos: u32, plane: usize) {
        debug_assert!(self.len < self.fetches.len());
        self.fetches[self.len] = (hpos, plane);
        self.len += 1;
    }

    pub(super) fn sort_fetches(&mut self) {
        self.fetches[..self.len].sort_unstable();
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (u32, usize)> + '_ {
        self.fetches[..self.len].iter().copied()
    }

    pub(super) fn latched_plane_sample_hpos(&self) -> Option<u32> {
        self.fetches[..self.len]
            .iter()
            .map(|(hpos, _)| *hpos)
            .max()
            .or(self.word_fetch_hpos)
    }
}

pub(super) fn line_fetch_plan_for_word(
    base_control: ControlState,
    control_segments: &[ControlSegment],
    word_idx: usize,
    dma_planes: usize,
) -> LineFetchPlan {
    let mut plan = LineFetchPlan::empty();
    if control_segments.is_empty() {
        let start = u32::from(effective_ddf_start_hpos(
            base_control.agnus_revision,
            base_control.hires() || base_control.shres(),
            base_control.ddfstrt,
        ));
        let word_group = word_idx as u32 / base_control.fetch_quantum();
        let word_hpos = if base_control.hires() || base_control.shres() {
            start + word_group.saturating_mul(base_control.fetch_period())
        } else {
            start + word_group.saturating_mul(base_control.fetch_unit())
        };
        if (u32::from(BITPLANE_DDF_HARD_START)..=BITPLANE_FETCH_HARD_END).contains(&word_hpos)
            && fetch_word_index_active_at_hpos(base_control, word_idx, word_hpos)
        {
            plan.word_fetch_hpos = Some(word_hpos);
        }
        for plane in 0..dma_planes.min(8) {
            let hpos = bitplane_fetch_hpos_for_plane(base_control, word_idx, plane);
            if (u32::from(BITPLANE_DDF_HARD_START)..=BITPLANE_FETCH_HARD_END).contains(&hpos)
                && fetch_plane_word_active_at_hpos(base_control, word_idx, plane, hpos)
            {
                plan.push(hpos, plane);
            }
        }
        plan.sort_fetches();
        return plan;
    }

    let mut control = base_control;
    let mut segment_idx = 0usize;
    for hpos in u32::from(BITPLANE_DDF_HARD_START)..=BITPLANE_FETCH_HARD_END {
        let x = ((hpos as i32 - COPPER_WAIT_HPOS_FB0).max(0) as usize * 4).min(FB_WIDTH);
        while segment_idx < control_segments.len() && control_segments[segment_idx].x <= x {
            control = control_segments[segment_idx].control;
            segment_idx += 1;
        }
        if plan.word_fetch_hpos.is_none()
            && fetch_word_index_active_at_hpos(control, word_idx, hpos)
        {
            plan.word_fetch_hpos = Some(hpos);
        }
        for plane in 0..dma_planes.min(8) {
            if fetch_plane_word_active_at_hpos(control, word_idx, plane, hpos) {
                plan.push(hpos, plane);
            }
        }
    }
    plan
}

pub(super) fn line_fetch_plans_for_line(
    base_control: ControlState,
    control_segments: &[ControlSegment],
    words_per_row: usize,
    dma_planes: usize,
) -> Vec<LineFetchPlan> {
    let mut plans = vec![LineFetchPlan::empty(); words_per_row];
    if words_per_row == 0 {
        return plans;
    }
    if control_segments.is_empty() {
        for (word_idx, plan) in plans.iter_mut().enumerate() {
            *plan = line_fetch_plan_for_word(base_control, control_segments, word_idx, dma_planes);
        }
        return plans;
    }

    let mut control = base_control;
    let mut segment_idx = 0usize;
    for hpos in u32::from(BITPLANE_DDF_HARD_START)..=BITPLANE_FETCH_HARD_END {
        let x = ((hpos as i32 - COPPER_WAIT_HPOS_FB0).max(0) as usize * 4).min(FB_WIDTH);
        while segment_idx < control_segments.len() && control_segments[segment_idx].x <= x {
            control = control_segments[segment_idx].control;
            segment_idx += 1;
        }
        if !control.bitplane_dma_enabled() || !control.has_valid_ddf_window() {
            continue;
        }
        let Some((start, _stop)) = effective_ddf_window(
            control.agnus_revision,
            control.hires() || control.shres(),
            control.ddfstrt,
            control.ddfstop,
            control.harddis,
        ) else {
            continue;
        };
        let start = u32::from(start);
        if hpos < start {
            continue;
        }
        let rel = hpos - start;
        let Some(word_idx) = (if control.hires() || control.shres() {
            let step = control.fetch_cck_per_word();
            (rel % step == 0).then_some((rel / step) as usize)
        } else {
            Some((rel / 8) as usize)
        }) else {
            continue;
        };
        if word_idx >= words_per_row {
            continue;
        }
        if plans[word_idx].word_fetch_hpos.is_none()
            && fetch_word_index_active_at_hpos(control, word_idx, hpos)
        {
            plans[word_idx].word_fetch_hpos = Some(hpos);
        }
        for plane in 0..dma_planes.min(8) {
            // A plane fetches a given word once; if it reads as active across
            // more than one colorclock (overlapping DDF segments), keep only
            // the first so the per-word fetch plan never exceeds dma_planes.
            if fetch_plane_word_active_at_hpos(control, word_idx, plane, hpos)
                && !plans[word_idx].iter().any(|(_, p)| p == plane)
            {
                plans[word_idx].push(hpos, plane);
            }
        }
    }
    for plan in &mut plans {
        plan.sort_fetches();
    }
    plans
}

#[cfg(test)]
pub(super) fn bitplane_output_start_x(
    base_control: ControlState,
    control_segments: &[ControlSegment],
    display_start_x: usize,
    words_per_row: usize,
    dma_planes: usize,
) -> usize {
    bitplane_dma_output_start_x(
        base_control,
        control_segments,
        display_start_x,
        words_per_row,
        dma_planes,
    )
    .unwrap_or(0)
}

pub(super) fn bitplane_dma_output_start_x(
    base_control: ControlState,
    control_segments: &[ControlSegment],
    display_start_x: usize,
    words_per_row: usize,
    dma_planes: usize,
) -> Option<usize> {
    if dma_planes == 0 || words_per_row == 0 {
        return None;
    }
    let mut display_control = base_control;
    for segment in control_segments {
        if segment.x <= display_start_x {
            display_control = segment.control;
        }
    }
    let pixel_repeat = display_control.framebuffer_pixel_repeat();
    if display_control.fetch_start_native_x(display_control.diw_h_start(), pixel_repeat) == 0 {
        return Some(display_start_x);
    }
    let plan = line_fetch_plan_for_word(base_control, control_segments, 0, dma_planes);
    plan.word_fetch_hpos
        .or_else(|| {
            plan.iter()
                .find_map(|(hpos, plane)| (plane == 0).then_some(hpos))
        })
        .map(bitplane_fetch_framebuffer_x)
}

pub(super) fn manual_bpl_dma_clip_x(
    seg: &ManualBplSegment,
    base_control: ControlState,
    control_segments: &[ControlSegment],
    dma_output_start_x: Option<usize>,
) -> Option<usize> {
    let mut clip_x = dma_output_start_x.filter(|&x| seg.x < x as i32);
    let dma_planes = line_max_dma_planes(base_control, control_segments);
    if dma_planes == 0 || !line_has_valid_ddf_window(base_control, control_segments) {
        return clip_x;
    }

    let words_per_row = line_words_per_row(base_control, control_segments);
    for word_idx in 0..words_per_row {
        let plan = line_fetch_plan_for_word(base_control, control_segments, word_idx, dma_planes);
        let Some(next_bpl1dat_hpos) = plan
            .iter()
            .find_map(|(hpos, plane)| (plane == 0 && hpos > seg.hpos).then_some(hpos))
        else {
            continue;
        };
        let next_bpl1dat_x = bitplane_fetch_framebuffer_x(next_bpl1dat_hpos);
        if next_bpl1dat_x as i32 > seg.x {
            clip_x = Some(clip_x.map_or(next_bpl1dat_x, |old| old.min(next_bpl1dat_x)));
        }
        break;
    }

    clip_x
}

pub(super) fn bitplane_carry_words_for_line(
    block_start: bool,
    display_start_x: usize,
    dma_output_start_x: Option<usize>,
    previous_playfield_tail_words: [Option<u16>; 8],
) -> [Option<u16>; 8] {
    if block_start || dma_output_start_x.is_some_and(|start| start > display_start_x) {
        [None; 8]
    } else {
        previous_playfield_tail_words
    }
}

#[cfg(test)]
pub(super) fn line_fetch_hpos_for_word(
    base_control: ControlState,
    control_segments: &[ControlSegment],
    word_idx: usize,
) -> Option<u32> {
    line_fetch_plan_for_word(base_control, control_segments, word_idx, 0).word_fetch_hpos
}
