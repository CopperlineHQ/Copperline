// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure helpers for the Frame Analyzer's blitter visualisations.
//!
//! The inputs are the exact per-blit DMA words recorded by the bus. Nothing
//! here reads or mutates the emulated machine, so CCP image export, the tool
//! window, and headless screenshot overlays all share one deterministic path.

use crate::bus::{FrameBlitRecord, RenderRegisterSnapshot};
use crate::chipset::denise::{rgb24_to_rgba8, BitplaneMode};
use crate::uaelib::{DebugResource, ResourceKind, RESOURCE_FLAG_INTERLEAVED};
use crate::video::resource_preview::{BitmapPreview, PREVIEW_MAX_PIXELS, PREVIEW_MAX_WIDTH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlitChannel {
    A,
    B,
    C,
    D,
    Result,
}

impl BlitChannel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_uppercase().as_str() {
            "A" => Some(Self::A),
            "B" => Some(Self::B),
            "C" => Some(Self::C),
            "D" => Some(Self::D),
            "RESULT" => Some(Self::Result),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::Result => "result",
        }
    }

    fn index(self) -> Option<usize> {
        match self {
            Self::A => Some(0),
            Self::B => Some(1),
            Self::C => Some(2),
            Self::D => Some(3),
            Self::Result => None,
        }
    }
}

/// Smallest sum-of-products formula for the blitter's three-input truth
/// table. With only eight minterms an exhaustive implicant cover is smaller
/// and easier to audit than a general-purpose Quine-McCluskey implementation,
/// while producing the same minimum term/literal ordering.
pub fn minterm_formula(minterm: u8) -> String {
    if minterm == 0 {
        return "0".to_string();
    }
    if minterm == u8::MAX {
        return "1".to_string();
    }

    #[derive(Clone, Copy)]
    struct Implicant {
        value: u8,
        mask: u8,
        covers: u8,
        literals: u8,
    }

    let mut implicants = Vec::new();
    // mask bit set = that variable is significant. Enumerating all ternary
    // cubes (0/1/don't-care) is exactly the prime-implicant search for three
    // variables; cubes touching a false minterm are rejected.
    for mask in 0u8..8 {
        for value in 0u8..8 {
            if value & !mask != 0 {
                continue;
            }
            let mut covers = 0u8;
            for abc in 0u8..8 {
                if abc & mask == value {
                    covers |= 1 << abc;
                }
            }
            if covers != 0 && covers & !minterm == 0 {
                implicants.push(Implicant {
                    value,
                    mask,
                    covers,
                    literals: mask.count_ones() as u8,
                });
            }
        }
    }
    // Remove cubes wholly covered by a less-specific cube.
    let all_implicants = implicants.clone();
    implicants.retain(|candidate| {
        !all_implicants.iter().any(|other| {
            other.covers != candidate.covers
                && other.covers | candidate.covers == other.covers
                && other.literals <= candidate.literals
        })
    });

    let mut best: Option<(u32, u32, u64)> = None;
    let count = implicants.len();
    for subset in 1u64..(1u64 << count) {
        let mut cover = 0u8;
        let mut terms = 0u32;
        let mut literals = 0u32;
        for (index, implicant) in implicants.iter().enumerate() {
            if subset & (1u64 << index) != 0 {
                cover |= implicant.covers;
                terms += 1;
                literals += u32::from(implicant.literals);
            }
        }
        if cover == minterm {
            let score = (terms, literals, subset);
            if best.is_none_or(|old| score < old) {
                best = Some(score);
            }
        }
    }
    let subset = best
        .expect("every non-zero truth table has a minterm cover")
        .2;
    let names = ['A', 'B', 'C'];
    implicants
        .iter()
        .enumerate()
        .filter(|(index, _)| subset & (1u64 << index) != 0)
        .map(|(_, implicant)| {
            let terms: Vec<String> = (0..3)
                .filter(|bit| implicant.mask & (1 << bit) != 0)
                .map(|bit| {
                    let name = names[2 - bit];
                    if implicant.value & (1 << bit) != 0 {
                        name.to_string()
                    } else {
                        format!("!{name}")
                    }
                })
                .collect();
            terms.join(" & ")
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn minterm_word(minterm: u8, a: u16, b: u16, c: u16) -> u16 {
    let mut out = 0u16;
    for abc in 0..8 {
        if minterm & (1 << abc) == 0 {
            continue;
        }
        let av = if abc & 4 != 0 { a } else { !a };
        let bv = if abc & 2 != 0 { b } else { !b };
        let cv = if abc & 1 != 0 { c } else { !c };
        out |= av & bv & cv;
    }
    out
}

/// Plane count used by the visualiser: a registered bitmap containing the
/// destination wins, otherwise the frame-start Denise BPU decode does.
pub fn plane_count_for_blit(
    blit: &FrameBlitRecord,
    resources: &[DebugResource],
    base: RenderRegisterSnapshot,
) -> (usize, &'static str) {
    if let Some(planes) = resources.iter().find_map(|resource| {
        let end = resource.address.saturating_add(resource.size);
        if !(resource.address..end).contains(&blit.dpt) {
            return None;
        }
        match resource.kind {
            ResourceKind::Bitmap { planes, .. } => Some(usize::from(planes).clamp(1, 8)),
            _ => None,
        }
    }) {
        return (planes, "resource");
    }
    let aga = matches!(
        base.agnus_revision,
        crate::chipset::agnus::AgnusRevision::AgaAlice
    );
    (
        BitplaneMode::from_bplcon0(base.bplcon0, aga)
            .display_planes()
            .max(1),
        "BPLCON0",
    )
}

fn channel_words(blit: &FrameBlitRecord, channel: BlitChannel, count: usize) -> Vec<u16> {
    if let Some(index) = channel.index() {
        if blit.channels[index] {
            return blit.channel_words[index].clone();
        }
        if index < 3 {
            return vec![blit.data[index]; count];
        }
        return Vec::new();
    }
    if !blit.channel_words[3].is_empty() {
        return blit.channel_words[3].clone();
    }
    let inputs: [Vec<u16>; 3] = std::array::from_fn(|index| {
        if blit.channels[index] {
            blit.channel_words[index].clone()
        } else {
            vec![blit.data[index]; count]
        }
    });
    (0..count)
        .map(|index| {
            minterm_word(
                blit.minterm,
                inputs[0].get(index).copied().unwrap_or(blit.data[0]),
                inputs[1].get(index).copied().unwrap_or(blit.data[1]),
                inputs[2].get(index).copied().unwrap_or(blit.data[2]),
            )
        })
        .collect()
}

fn indexed_colour(index: usize, planes: usize) -> u32 {
    if index == 0 {
        return rgb24_to_rgba8(0x101418);
    }
    let max = (1usize << planes.min(8)).saturating_sub(1).max(1);
    let level = (index * 255 / max) as u32;
    let rgb = if planes == 1 {
        (level << 16) | (level << 8) | level
    } else {
        let r = ((index.wrapping_mul(97) & 0xFF) as u32 + level) / 2;
        let g = ((index.wrapping_mul(57) & 0xFF) as u32 + level) / 2;
        let b = ((index.wrapping_mul(23) & 0xFF) as u32 + level) / 2;
        (r << 16) | (g << 8) | b
    };
    rgb24_to_rgba8(rgb)
}

pub fn render_blit(
    blit: &FrameBlitRecord,
    channel: BlitChannel,
    planes: usize,
) -> Result<BitmapPreview, String> {
    if blit.line_mode
        && !matches!(
            channel,
            BlitChannel::C | BlitChannel::D | BlitChannel::Result
        )
    {
        // Line mode's A/B values are Bresenham state rather than a rectangle;
        // still render them when DMA produced a stream, but say so below.
    }
    let width_words = usize::try_from(blit.width_words).unwrap_or(usize::MAX);
    let transfer_rows = usize::try_from(blit.height).unwrap_or(usize::MAX);
    let width = width_words
        .checked_mul(16)
        .ok_or_else(|| "blit width overflows the preview".to_string())?;
    if width == 0 || transfer_rows == 0 || width > PREVIEW_MAX_WIDTH {
        return Err(format!(
            "blit geometry {}x{} words is outside the preview limit",
            blit.width_words, blit.height
        ));
    }
    let planes = planes.clamp(1, 8);
    let height = transfer_rows.div_ceil(planes);
    if width
        .checked_mul(height)
        .is_none_or(|pixels| pixels > PREVIEW_MAX_PIXELS)
    {
        return Err(format!(
            "blit preview {}x{} exceeds the pixel limit",
            width, height
        ));
    }
    let word_count = width_words.saturating_mul(transfer_rows);
    let words = channel_words(blit, channel, word_count);
    let mut plane_words = vec![0u16; word_count];
    for (sequence, word) in words.iter().copied().take(word_count).enumerate() {
        let transfer_row = sequence / width_words;
        let transfer_col = sequence % width_words;
        let (row, col) = if blit.descending {
            (
                transfer_rows - 1 - transfer_row,
                width_words - 1 - transfer_col,
            )
        } else {
            (transfer_row, transfer_col)
        };
        plane_words[row * width_words + col] = word;
    }
    let mut pixels = vec![rgb24_to_rgba8(0x101418); width * height];
    for y in 0..height {
        for x in 0..width {
            let mut index = 0usize;
            for plane in 0..planes {
                let transfer_row = y * planes + plane;
                if transfer_row >= transfer_rows {
                    continue;
                }
                let word = plane_words[transfer_row * width_words + x / 16];
                index |= usize::from((word >> (15 - x % 16)) & 1) << plane;
            }
            pixels[y * width + x] = indexed_colour(index, planes);
        }
    }
    let expected = if matches!(channel, BlitChannel::D | BlitChannel::Result) && !blit.channels[3] {
        word_count
    } else if let Some(index) = channel.index() {
        if blit.channels[index] {
            word_count
        } else {
            words.len()
        }
    } else {
        word_count
    };
    let mut notes = vec![format!("{} plane(s)", planes)];
    if words.len() < expected {
        notes.push(format!(
            "{} of {} DMA words captured",
            words.len(),
            expected
        ));
    }
    if blit.render_truncated {
        notes.push("transfer record truncated".to_string());
    }
    if blit.line_mode {
        notes.push("line-mode transfer".to_string());
    }
    Ok(BitmapPreview {
        width,
        height,
        pixels,
        note: Some(notes.join("; ")),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DestinationPixel {
    pub x: usize,
    pub y: usize,
}

/// Map a D-channel word address into a registered bitmap's first pixel.
/// This is the same planar/interleaved layout used by the Resources tab.
pub fn destination_word_pixel(
    address: u32,
    resources: &[DebugResource],
) -> Option<(DestinationPixel, usize, usize)> {
    resources.iter().find_map(|resource| {
        let ResourceKind::Bitmap {
            width,
            height,
            planes,
        } = resource.kind
        else {
            return None;
        };
        let end = resource.address.saturating_add(resource.size);
        if !(resource.address..end).contains(&address) || width == 0 || height == 0 || planes == 0 {
            return None;
        }
        let row_bytes = usize::from(width).div_ceil(16) * 2;
        let offset = usize::try_from(address - resource.address).ok()?;
        let (row, plane_offset) = if resource.flags & RESOURCE_FLAG_INTERLEAVED != 0 {
            let row_stride = row_bytes * usize::from(planes);
            (offset / row_stride, offset % row_stride % row_bytes)
        } else {
            let plane_size = row_bytes * usize::from(height);
            (offset % plane_size / row_bytes, offset % row_bytes)
        };
        (row < usize::from(height)).then_some((
            DestinationPixel {
                x: plane_offset / 2 * 16,
                y: row,
            },
            usize::from(width),
            usize::from(height),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minterm_simplifier_handles_copy_cookie_cut_and_constants() {
        assert_eq!(minterm_formula(0x00), "0");
        assert_eq!(minterm_formula(0xFF), "1");
        assert_eq!(minterm_formula(0xF0), "A");
        assert_eq!(minterm_formula(0xCC), "B");
        assert_eq!(minterm_formula(0xAA), "C");
        let cookie = minterm_formula(0xCA);
        assert!(cookie.contains('A') && cookie.contains('B') && cookie.contains('C'));
    }

    fn recorded_blit() -> FrameBlitRecord {
        FrameBlitRecord {
            id: 7,
            bltcon0: 0x01F0,
            bltcon1: 0,
            width_words: 1,
            height: 2,
            descending: false,
            line_mode: false,
            fill_mode: false,
            channels: [false, false, false, true],
            apt: 0,
            bpt: 0,
            cpt: 0,
            dpt: 0x1000,
            modulos: [0; 4],
            shifts: [0; 2],
            masks: [0xFFFF; 2],
            minterm: 0xF0,
            data: [0; 3],
            start_frame: 2,
            end_frame: Some(2),
            start: (10, 20),
            end: Some((10, 30)),
            cycles_used: 8,
            cycles_stalled: 2,
            channel_words: [Vec::new(), Vec::new(), Vec::new(), vec![0x8000, 0x0001]],
            channel_addrs: [Vec::new(), Vec::new(), Vec::new(), vec![0x1000, 0x1002]],
            render_truncated: false,
        }
    }

    #[test]
    fn recorded_dma_words_render_without_live_memory() {
        let preview = render_blit(&recorded_blit(), BlitChannel::D, 1).unwrap();
        assert_eq!((preview.width, preview.height), (16, 2));
        assert_ne!(preview.pixels[0], preview.pixels[1]);
        assert_ne!(preview.pixels[31], preview.pixels[30]);
    }

    #[test]
    fn destination_addresses_map_through_registered_bitmap_layout() {
        let resource = DebugResource {
            address: 0x1000,
            size: 80,
            flags: 0,
            kind: ResourceKind::Bitmap {
                width: 32,
                height: 10,
                planes: 2,
            },
            name: "bitmap".to_string(),
            registered_frame: 0,
        };
        let (pixel, width, height) = destination_word_pixel(0x1006, &[resource]).unwrap();
        assert_eq!((pixel.x, pixel.y, width, height), (16, 1, 32, 10));
    }
}
