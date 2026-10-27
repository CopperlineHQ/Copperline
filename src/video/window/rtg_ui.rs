// SPDX-License-Identifier: GPL-3.0-or-later

//! UI sprites for the native RTG compositor. The host UI keeps its own
//! canvas fit; corner sprites can sit outside that fit at board corners.

use super::Rect;
use zerocopy::{Immutable, IntoBytes};

pub(super) const MAX_LAYERS: usize = 5; // guest, REC, performance, OSD, host UI

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, IntoBytes, Immutable)]
pub(super) struct Layer {
    pub(super) src: [f32; 4],
    pub(super) dst: [f32; 4],
    pub(super) backdrop: [f32; 4],
}

pub(super) const EMPTY_LAYER: Layer = Layer {
    src: [0.0; 4],
    dst: [0.0; 4],
    backdrop: [0.0; 4],
};

#[derive(Default)]
pub(super) struct Frame {
    pub(super) width: usize,
    pub(super) pixels: Vec<u32>,
    pub(super) layers: Vec<Layer>,
}

impl Frame {
    /// Capture painted pixels, then clear them before the next layer is
    /// drawn. An empty drawer creates no atlas, upload or compositor work.
    pub(super) fn capture(
        &mut self,
        frame: &mut [u8],
        stride: usize,
        rect: Rect,
        mut dst: [f32; 4],
        backdrop: u32,
    ) {
        let w = rect.w.min(stride.saturating_sub(rect.x));
        let h = rect
            .h
            .min((frame.len() / 4 / stride).saturating_sub(rect.y));
        if w == 0 || h == 0 || dst[2] <= 0.0 || dst[3] <= 0.0 {
            return;
        }
        let ink = (rect.y..rect.y + h).any(|y| {
            frame[(y * stride + rect.x) * 4..][..w * 4]
                .chunks_exact(4)
                .any(|p| p[3] != 0)
        });
        if !ink && backdrop >> 24 == 0 {
            return;
        }
        assert!(self.layers.len() < MAX_LAYERS);
        dst[2] *= w as f32 / rect.w as f32;
        dst[3] *= h as f32 / rect.h as f32;
        self.width = stride;
        let top = self.pixels.len() / stride;
        self.pixels.resize((top + h) * stride, 0);
        for y in 0..h {
            let row = &mut frame[((rect.y + y) * stride + rect.x) * 4..][..w * 4];
            for (out, p) in self.pixels[(top + y) * stride..][..w]
                .iter_mut()
                .zip(row.chunks_exact(4))
            {
                *out = u32::from_le_bytes(p.try_into().expect("RGBA pixel"));
            }
            row.fill(0);
        }
        self.layers.push(Layer {
            src: [0.0, top as f32, w as f32, h as f32],
            dst,
            backdrop: backdrop.to_le_bytes().map(|c| c as f32 / 255.0),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_drawers_need_no_atlas_or_compositor_layers() {
        let mut ui = Frame::default();
        let mut source = vec![0; 16];
        ui.capture(
            &mut source,
            2,
            Rect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            },
            [0.0, 0.0, 2.0, 2.0],
            0,
        );
        assert!(ui.layers.is_empty());
        assert!(ui.pixels.is_empty());
        assert_eq!(ui.width, 0);
    }
    #[test]
    fn captured_layers_are_independent_and_clear_their_source() {
        let mut ui = Frame::default();
        let mut source = vec![0; 16];
        for i in 0..MAX_LAYERS {
            let colour = 0xFF00_0000 | i as u32;
            source[..4].copy_from_slice(&colour.to_le_bytes());
            ui.capture(
                &mut source,
                2,
                Rect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                },
                [i as f32, 0.0, 1.0, 1.0],
                0,
            );
            assert_eq!(source, [0; 16]);
            assert_eq!(ui.pixels[i * 2], colour);
            assert_eq!(ui.layers[i].src, [0.0, i as f32, 1.0, 1.0]);
        }
        assert_eq!(ui.layers.len(), MAX_LAYERS);
    }
}
