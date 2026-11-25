// SPDX-License-Identifier: GPL-3.0-or-later

//! What the interface's own surfaces draw on.
//!
//! The configuration screen, the menu and the status bar each have one
//! drawing function, written against [`Canvas`]. The window draws them on
//! screen with egui (`egui_view::EguiCanvas`). [`FrameCanvas`] draws the
//! same functions into a frame buffer: the panels still drawn into the
//! presentation texture use it for the pieces they share, and the tests
//! and the documentation previews draw every surface with it.
//!
//! Positions are canvas pixels: the presentation texture's logical pixels,
//! which the pointer mapping, the keyboard focus and every layout function
//! measure in. A backend renders them at its own resolution.

use super::*;

pub(in crate::video) trait Canvas {
    /// Device pixels per canvas pixel, as a whole number: the texture's
    /// supersampling factor, or egui's pixels per point rounded. What is
    /// drawn finer than a canvas pixel (the status bar's glyphs) is laid out
    /// on this grid.
    fn device_scale(&self) -> usize;

    fn fill(&mut self, r: Rect, colour: u32);

    /// `colour` over what is already there, `alpha` of the way.
    fn blend(&mut self, r: Rect, colour: u32, alpha: f32);

    /// A raised or sunken edge one canvas pixel wide: `light` along the top
    /// and left, then `dark` along the bottom and right.
    fn bevel(&mut self, r: Rect, light: u32, dark: u32);

    /// Bitmap-font text at `px` canvas pixels per font pixel.
    fn text(&mut self, x: usize, y: usize, text: &str, colour: u32, px: usize);

    /// An RGBA picture (`width` by `height`) drawn opaque into `at`, each
    /// canvas pixel taking the nearest source pixel.
    fn image(&mut self, at: Rect, pixels: &[u8], width: usize, height: usize);

    /// A rectangle in device pixels (`x`, `y`, `w`, `h` already multiplied
    /// by [`Canvas::device_scale`]), for detail finer than a canvas pixel.
    fn fill_device(&mut self, x: usize, y: usize, w: usize, h: usize, colour: u32);

    /// One device pixel, `alpha` of the way to `colour`.
    fn blend_device(&mut self, x: usize, y: usize, colour: u32, alpha: f32);

    /// The value box `box_rect`, being typed in (`target`), its text at
    /// (`x`, `y`) and no wider than `avail`. A canvas that types in its own
    /// boxes (egui) puts its text field there and returns true; any other
    /// returns false, and the caller draws the edit line itself.
    #[allow(clippy::too_many_arguments)]
    fn typing_box(
        &mut self,
        _box_rect: Rect,
        _x: usize,
        _y: usize,
        _avail: usize,
        _colour: u32,
        _target: launcher::EditTarget,
    ) -> bool {
        false
    }
}

/// A canvas outline: one canvas pixel round the inside of `rect`.
pub(in crate::video) fn outline(canvas: &mut dyn Canvas, rect: Rect, colour: u32) {
    if rect.w == 0 || rect.h == 0 {
        return;
    }
    for edge in [
        Rect { h: 1, ..rect },
        Rect {
            y: rect.y + rect.h.saturating_sub(1),
            h: 1,
            ..rect
        },
        Rect { w: 1, ..rect },
        Rect {
            x: rect.x + rect.w.saturating_sub(1),
            w: 1,
            ..rect
        },
    ] {
        canvas.fill(edge, colour);
    }
}

/// The presentation texture, or any RGBA frame of its layout, at `scale`.
pub(in crate::video) struct FrameCanvas<'a> {
    frame: &'a mut [u8],
    scale: usize,
}

impl<'a> FrameCanvas<'a> {
    pub(in crate::video) fn new(frame: &'a mut [u8], scale: usize) -> Self {
        Self { frame, scale }
    }
}

impl Canvas for FrameCanvas<'_> {
    fn device_scale(&self) -> usize {
        self.scale
    }

    fn fill(&mut self, r: Rect, colour: u32) {
        fill_rect(self.frame, scale_rect(r, self.scale), colour, self.scale);
    }

    fn blend(&mut self, r: Rect, colour: u32, alpha: f32) {
        fill_rect_blend(
            self.frame,
            scale_rect(r, self.scale),
            colour,
            alpha,
            self.scale,
        );
    }

    fn bevel(&mut self, r: Rect, light: u32, dark: u32) {
        draw_rect_bevel(
            self.frame,
            scale_rect(r, self.scale),
            light,
            dark,
            self.scale,
        );
    }

    fn text(&mut self, x: usize, y: usize, text: &str, colour: u32, px: usize) {
        draw_panel_text(self.frame, x, y, text, colour, px, self.scale);
    }

    fn image(&mut self, at: Rect, pixels: &[u8], width: usize, height: usize) {
        for y in 0..at.h {
            let from_y = y * height / at.h;
            for x in 0..at.w {
                let from = (from_y * width + x * width / at.w) * 4;
                let Some(px) = pixels.get(from..from + 4) else {
                    continue;
                };
                let colour = rgba(px[0] as u32, px[1] as u32, px[2] as u32);
                self.fill(
                    Rect {
                        x: at.x + x,
                        y: at.y + y,
                        w: 1,
                        h: 1,
                    },
                    colour,
                );
            }
        }
    }

    fn fill_device(&mut self, x: usize, y: usize, w: usize, h: usize, colour: u32) {
        fill_rect(self.frame, Rect { x, y, w, h }, colour, self.scale);
    }

    fn blend_device(&mut self, x: usize, y: usize, colour: u32, alpha: f32) {
        super::super::window::statusbar::blend_pixel(self.frame, x, y, colour, alpha, self.scale);
    }
}
