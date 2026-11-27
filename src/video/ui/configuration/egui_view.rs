// SPDX-License-Identifier: GPL-3.0-or-later

//! The configuration screen and the menu on egui: the canvas the window
//! draws them with.
//!
//! The panel is drawn by the same function the tests and the documentation
//! previews draw into a frame buffer ([`draw_launcher`] on a [`Canvas`]);
//! here [`EguiCanvas`] lays those calls down as egui shapes, at the
//! window's own resolution, and src/video/window/egui_overlay.rs paints
//! them over the presented frame. Hit-testing and keyboard navigation are
//! the panel's own, unchanged (`launcher_control_at`, `nav::map`).
//!
//! What egui adds is typing. While a value box is being typed in
//! (`LauncherState::editing`), egui's own text field stands in for the edit
//! line, with its caret, selection, clipboard and IME. The launcher still
//! decides which box is being typed in and what the typing means: the text
//! is handed back to `LauncherState::edit_commit`, the parser every box
//! already had.
//!
//! Text is drawn from a TrueType font built at runtime from the 8x8 bitmap
//! ([`pixel_font_ttf`]): egui only loads outline fonts, and this one's
//! outlines are the bitmap's pixels as squares, so at a whole multiple of
//! 8 px it renders the bitmap exactly.

use super::*;
use egui::{pos2, vec2, Color32, FontFamily, FontId};
use std::collections::HashMap;

/// Name the pixel font is registered under.
const PIXEL_FONT: &str = "copperline-8x8";

/// Font units per bitmap pixel; eight of them make the em.
const UNITS_PER_PIXEL: i16 = 128;
const UNITS_PER_EM: u16 = 8 * UNITS_PER_PIXEL as u16;

/// Where the keyboard's focus stands and what the pointer is over, as
/// `ui::set_nav_light` takes them.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::video) struct Light {
    pub(in crate::video) hover: Option<UiControl>,
    pub(in crate::video) focus: Option<UiControl>,
    pub(in crate::video) mix: f32,
    pub(in crate::video) open: bool,
    pub(in crate::video) elsewhere: bool,
}

/// What a value box being typed in did when it let go of the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::video) enum Release {
    /// Return, Tab, or a click elsewhere: take what was typed.
    Commit,
    /// Escape: throw it away.
    Cancel,
}

/// egui's side of typing into a value box: the text while it is being
/// typed, which box it is in, and what became of it. Kept by the window
/// from frame to frame; the box itself is the launcher's to choose.
#[derive(Debug, Default)]
pub(in crate::video) struct Typing {
    target: Option<EditTarget>,
    text: String,
    /// Hand the box the keyboard on the next frame.
    focus: bool,
    /// The box held the keyboard at the end of the last frame.
    had_focus: bool,
    /// The box was on the page this frame, and where: a click there is
    /// the text field's (placing the caret), not the panel's.
    drawn: Option<Rect>,
    /// The field asked for the keyboard, took it or let it go this frame:
    /// what it shows catches up on the next.
    unsettled: bool,
    release: Option<Release>,
}

impl Typing {
    /// Follow the box the launcher is typing in, starting over from its
    /// text whenever that box changes.
    pub(in crate::video) fn follow(&mut self, state: &LauncherState) {
        if state.editing() == self.target {
            return;
        }
        *self = match state.editing() {
            Some(target) => Self {
                target: Some(target),
                text: state.edit_buffer().to_string(),
                focus: true,
                ..Self::default()
            },
            None => Self::default(),
        };
    }

    /// Whether a box is being typed in: the keys are its, not the focus's.
    pub(in crate::video) fn active(&self) -> bool {
        self.target.is_some()
    }

    /// Give the box the keyboard back: what was typed was refused, and is
    /// there to be put right.
    fn refocus(&mut self) {
        self.focus = true;
        self.had_focus = false;
    }

    /// Whether canvas point `pos` is on the box being typed in, as drawn
    /// this frame.
    pub(in crate::video) fn contains(&self, pos: egui::Pos2) -> bool {
        self.drawn
            .is_some_and(|rect| rect.contains((pos.x.floor() as i32, pos.y.floor() as i32)))
    }
}

/// Hand what the text field holds back to the launcher -- filtered as the
/// box would have filtered a key at a time -- and act on the field letting
/// go of the keyboard, or on the box having left the page. Returns whether
/// the panel needs drawing again to show what came of it; an idle field
/// asks for nothing.
pub(in crate::video) fn settle(typing: &mut Typing, state: &mut LauncherState) -> bool {
    if typing.target.is_none() || typing.target != state.editing() {
        return false;
    }
    let mut redraw = std::mem::take(&mut typing.unsettled);
    let kept = state.edit_filtered(&typing.text);
    if kept != typing.text {
        typing.text = kept.clone();
        redraw = true;
    }
    // Kept in step, so anything on the panel that commits first (as a
    // click elsewhere does) takes what is in the box now.
    state.set_edit_buffer(kept);
    if typing.drawn.is_none() {
        // The box left the page with the keyboard still in it (its row
        // scrolled out of view): nothing can be typed into a field that is
        // not there, so it is let go of as a click elsewhere would be. A
        // value the launcher refuses is dropped, the refusal on the status
        // line, since there is no box left to put it right in.
        state.edit_commit();
        if state.editing().is_some() {
            state.edit_cancel();
        }
        typing.follow(state);
        return true;
    }
    match typing.release.take() {
        Some(Release::Commit) => {
            state.edit_commit();
            // Refused: the reason is in the status line, and the text stays
            // in the box, with the keyboard, to be put right.
            if state.editing().is_some() {
                typing.refocus();
            }
        }
        Some(Release::Cancel) => state.edit_cancel(),
        None => return redraw,
    }
    typing.follow(state);
    true
}

/// Textures for pictures drawn on the panel (cover art), by what they
/// show and the size they are drawn at, kept while they are drawn.
#[derive(Default)]
pub(in crate::video) struct Images {
    textures: HashMap<u64, egui::TextureHandle>,
    used: Vec<u64>,
}

impl Images {
    /// Forget every texture the last frame did not draw.
    pub(in crate::video) fn end_frame(&mut self) {
        let used = std::mem::take(&mut self.used);
        self.textures.retain(|key, _| used.contains(key));
    }
}

fn colour(c: u32) -> Color32 {
    let [r, g, b, _] = c.to_le_bytes();
    Color32::from_rgb(r, g, b)
}

fn points(r: Rect) -> egui::Rect {
    egui::Rect::from_min_size(pos2(r.x as f32, r.y as f32), vec2(r.w as f32, r.h as f32))
}

/// A [`Canvas`] laying its calls down as egui shapes in `ui`, whose points
/// are canvas pixels.
pub(in crate::video) struct EguiCanvas<'a> {
    ui: &'a mut egui::Ui,
    painter: egui::Painter,
    /// Pixels per point: device pixels per canvas pixel.
    scale: f32,
    typing: &'a mut Typing,
    images: &'a mut Images,
}

impl<'a> EguiCanvas<'a> {
    pub(in crate::video) fn new(
        ui: &'a mut egui::Ui,
        typing: &'a mut Typing,
        images: &'a mut Images,
    ) -> Self {
        let painter = ui.painter().clone();
        let scale = ui.ctx().pixels_per_point();
        Self {
            ui,
            painter,
            scale,
            typing,
            images,
        }
    }

    /// The device-pixel grid the texture would draw on at this scale, when
    /// it is a whole one: there the canvas follows the texture's per-pixel
    /// drawing exactly.
    fn whole_scale(&self) -> Option<usize> {
        let rounded = self.scale.round();
        ((self.scale - rounded).abs() < 0.001 && rounded >= 1.0).then_some(rounded as usize)
    }

    /// The device-pixel grid the status bar draws on: egui's pixels per
    /// point to the nearest whole number. At a whole scale it is the
    /// surface's own pixels; at a fractional one each grid pixel lands on
    /// slightly more or less than one.
    fn grid(&self) -> usize {
        (self.scale.round() as usize).max(1)
    }

    fn device_rect(&self, x: usize, y: usize, w: usize, h: usize) -> egui::Rect {
        let g = self.grid() as f32;
        egui::Rect::from_min_size(
            pos2(x as f32 / g, y as f32 / g),
            vec2(w as f32 / g, h as f32 / g),
        )
    }
}

impl Canvas for EguiCanvas<'_> {
    fn device_scale(&self) -> usize {
        self.grid()
    }

    fn fill(&mut self, r: Rect, c: u32) {
        if r.w > 0 && r.h > 0 {
            self.painter.rect_filled(points(r), 0, colour(c));
        }
    }

    fn blend(&mut self, r: Rect, c: u32, alpha: f32) {
        if r.w > 0 && r.h > 0 {
            let [red, green, blue, _] = c.to_le_bytes();
            let alpha = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            self.painter.rect_filled(
                points(r),
                0,
                Color32::from_rgba_unmultiplied(red, green, blue, alpha),
            );
        }
    }

    fn bevel(&mut self, r: Rect, light: u32, dark: u32) {
        if r.w == 0 || r.h == 0 {
            return;
        }
        let Some(s) = self.whole_scale() else {
            // A fractional scale has no device grid to follow: one canvas
            // pixel each side, light first so dark wins the corners.
            let (x, y, w, h) = (r.x, r.y, r.w, r.h);
            self.fill(Rect { x, y, w, h: 1 }, light);
            self.fill(Rect { x, y, w: 1, h }, light);
            self.fill(
                Rect {
                    y: y + h - 1,
                    h: 1,
                    ..r
                },
                dark,
            );
            self.fill(
                Rect {
                    x: x + w - 1,
                    w: 1,
                    ..r
                },
                dark,
            );
            return;
        };
        // `draw_rect_bevel`, span for span: at a whole scale each inset is
        // one device pixel, and the later spans win the corners they share.
        let (x, y, w, h) = (r.x * s, r.y * s, r.w * s, r.h * s);
        for inset in 0..s {
            for (rect, c) in [
                (self.device_rect(x, y + inset, w, 1), light),
                (self.device_rect(x + inset, y, 1, h), light),
                (self.device_rect(x, y + h - 1 - inset, w, 1), dark),
                (self.device_rect(x + w - 1 - inset, y, 1, h), dark),
            ] {
                self.painter.rect_filled(rect, 0, colour(c));
            }
        }
    }

    fn text(&mut self, x: usize, y: usize, text: &str, c: u32, px: usize) {
        let text = font::renderable(text).into_owned();
        let galley =
            self.painter
                .layout_no_wrap(text, FontId::proportional(8.0 * px as f32), colour(c));
        self.painter
            .galley(pos2(x as f32, y as f32), galley, colour(c));
    }

    fn image(&mut self, at: Rect, pixels: &[u8], width: usize, height: usize) {
        if at.w == 0 || at.h == 0 || width == 0 || height == 0 {
            return;
        }
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (
                pixels.as_ptr() as usize,
                pixels.len(),
                width,
                height,
                at.w,
                at.h,
            )
                .hash(&mut hasher);
            hasher.finish()
        };
        self.images.used.push(key);
        let ctx = self.ui.ctx().clone();
        let texture = self.images.textures.entry(key).or_insert_with(|| {
            // Sampled to the size it is drawn at, a canvas pixel per source
            // pixel taken nearest, as the texture draws it; the GPU then
            // only ever magnifies it by whole device pixels.
            let mut sampled = Vec::with_capacity(at.w * at.h * 4);
            for y in 0..at.h {
                let from_y = y * height / at.h;
                for x in 0..at.w {
                    let from = (from_y * width + x * width / at.w) * 4;
                    match pixels.get(from..from + 3) {
                        Some(px) => sampled.extend_from_slice(&[px[0], px[1], px[2], 255]),
                        None => sampled.extend_from_slice(&[0, 0, 0, 0]),
                    }
                }
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([at.w, at.h], &sampled);
            ctx.load_texture(
                format!("launcher-image-{key:x}"),
                image,
                egui::TextureOptions::NEAREST,
            )
        });
        self.painter.image(
            texture.id(),
            points(at),
            egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    fn fill_device(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32) {
        if w > 0 && h > 0 {
            let rect = self.device_rect(x, y, w, h);
            self.painter.rect_filled(rect, 0, colour(c));
        }
    }

    fn blend_device(&mut self, x: usize, y: usize, c: u32, alpha: f32) {
        let [red, green, blue, _] = c.to_le_bytes();
        let alpha = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        let rect = self.device_rect(x, y, 1, 1);
        self.painter.rect_filled(
            rect,
            0,
            Color32::from_rgba_unmultiplied(red, green, blue, alpha),
        );
    }

    fn typing_box(
        &mut self,
        box_rect: Rect,
        x: usize,
        y: usize,
        avail: usize,
        c: u32,
        target: EditTarget,
    ) -> bool {
        if self.typing.target != Some(target) {
            return false;
        }
        self.typing.drawn = Some(box_rect);
        let left = x.saturating_sub(box_rect.x).min(i8::MAX as usize) as i8;
        let top = y.saturating_sub(box_rect.y).min(i8::MAX as usize) as i8;
        let edit = egui::TextEdit::singleline(&mut self.typing.text)
            .id(egui::Id::unique(("launcher_typing", format!("{target:?}"))))
            // A custom frame replaces TextEdit's own margin, so the text's
            // inset rides on the frame: where the edit line would start.
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left,
                right: 0,
                top,
                bottom: 0,
            }))
            .text_color(colour(c))
            .align(egui::Align2::LEFT_TOP)
            .desired_width(avail as f32);
        // In a child ui of its own: `put` in the parent would move the
        // parent's cursor, which nothing here lays out by but egui tracks.
        let response = self
            .ui
            .new_child(egui::UiBuilder::new().max_rect(points(box_rect)))
            .put(points(box_rect), edit);
        let asked = std::mem::take(&mut self.typing.focus);
        if asked {
            response.request_focus();
        }
        let focused = response.has_focus();
        if self.typing.had_focus && !focused {
            let escape = self.ui.input(|i| i.key_pressed(egui::Key::Escape));
            self.typing.release = Some(if escape {
                Release::Cancel
            } else {
                Release::Commit
            });
        } else if !focused && !asked {
            // It never took the keyboard: ask again.
            self.typing.focus = true;
        }
        // The keyboard asked for, taken or let go: the caret shows (or
        // goes) on the next frame, and a request still out needs one to be
        // granted in.
        if asked || self.typing.focus || focused != self.typing.had_focus {
            self.typing.unsettled = true;
        }
        self.typing.had_focus = focused;
        true
    }
}

/// Draw what egui draws over the display canvas for one frame: the
/// configuration screen when it is open (its face, title bar and
/// everything on it), then the menu's open levels when the menu is up. The
/// texture keeps what dims the display behind them: the panel's scrim and
/// the menu's veil, which egui adds over the panel it drew itself.
pub(in crate::video) fn show_display(
    ui: &mut egui::Ui,
    state: &UiState,
    light: Light,
    typing: &mut Typing,
    images: &mut Images,
) {
    typing.drawn = None;
    set_nav_light(light.focus, light.mix, light.open, light.elsewhere);
    let mut canvas = EguiCanvas::new(ui, typing, images);
    let launcher = state
        .panel
        .as_ref()
        .filter(|panel| matches!(panel, Panel::Launcher(_)));
    if let Some(panel @ Panel::Launcher(launcher_state)) = launcher {
        panel_chrome(&mut canvas, panel, light.hover);
        draw_launcher(&mut canvas, panel_rect(panel), launcher_state, light.hover);
    }
    if state.menu_open {
        if let Some(panel) = launcher {
            menu_veil_over(&mut canvas, panel);
        }
        draw_menu_columns(&mut canvas, &state.menu_rows, &state.menu_nav);
    }
    set_nav_light(None, 0.0, false, false);
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Fonts, colours and spacing for egui's own widgets (the text field), and
/// the pixel font everything is drawn in. Points are canvas pixels.
pub(in crate::video) fn configure(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        PIXEL_FONT.into(),
        egui::FontData::from_owned(pixel_font_ttf())
            .tweak(egui::FontTweak {
                hinting: Some(false),
                subpixel_binning: Some(false),
                ..Default::default()
            })
            .into(),
    );
    // First choice for both families; egui's own faces stay behind it as
    // the fallback for anything beyond ASCII.
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, PIXEL_FONT.into());
    }
    ctx.set_fonts(fonts);
    // Square-edged fills: anti-aliased rectangle edges would smear the
    // one-pixel bevels at fractional scale factors.
    ctx.tessellation_options_mut(|o| o.feathering = false);
    let mut style = egui::Style {
        visuals: egui::Visuals::dark(),
        ..Default::default()
    };
    let v = &mut style.visuals;
    v.panel_fill = colour(PANEL_BG);
    v.window_fill = colour(PANEL_BG);
    v.extreme_bg_color = colour(ENTRY_BG);
    v.selection.bg_fill = colour(PANEL_TITLE_BG);
    v.selection.stroke = egui::Stroke::new(1.0, colour(PANEL_TITLE_TEXT));
    v.text_cursor.stroke = egui::Stroke::new(2.0, colour(PANEL_TEXT_HILIGHT));
    v.text_cursor.blink = false;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::ZERO;
        w.expansion = 0.0;
    }
    style.text_styles = [
        (egui::TextStyle::Small, FontId::proportional(8.0)),
        (egui::TextStyle::Body, FontId::proportional(8.0)),
        (egui::TextStyle::Button, FontId::proportional(8.0)),
        (egui::TextStyle::Monospace, FontId::monospace(8.0)),
        (egui::TextStyle::Heading, FontId::proportional(16.0)),
    ]
    .into();
    style.interaction.selectable_labels = false;
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style);
}

// ---------------------------------------------------------------------------
// The pixel font
// ---------------------------------------------------------------------------

/// A TrueType font made from the 8x8 bitmap: printable ASCII, monospaced
/// at one em (eight pixels) per cell, ascent one em and no descent, so a
/// line of text is exactly eight pixels tall at size 8. Each horizontal run
/// of lit pixels in a glyph row becomes one rectangular contour.
pub(in crate::video) fn pixel_font_ttf() -> Vec<u8> {
    let chars: Vec<char> = (0x20u8..=0x7E).map(char::from).collect();
    let num_glyphs = chars.len() + 1; // glyph 0 is .notdef
    let mut glyf = Vec::new();
    let mut loca = vec![0u32];
    let mut hmtx = Vec::new();
    let (mut max_points, mut max_contours) = (0u16, 0u16);
    // .notdef: empty.
    loca.push(0);
    put_u16(&mut hmtx, UNITS_PER_EM);
    put_i16(&mut hmtx, 0);
    for ch in chars {
        let runs = glyph_runs(font::glyph(ch));
        let x_min = runs.iter().map(|r| r.0).min().unwrap_or(0);
        if !runs.is_empty() {
            write_simple_glyph(&mut glyf, &runs);
            max_contours = max_contours.max(runs.len() as u16);
            max_points = max_points.max(4 * runs.len() as u16);
        }
        loca.push(glyf.len() as u32);
        put_u16(&mut hmtx, UNITS_PER_EM);
        put_i16(&mut hmtx, x_min);
    }

    let em = UNITS_PER_EM as i16;
    let mut head = Vec::new();
    put_u32(&mut head, 0x0001_0000); // version
    put_u32(&mut head, 0x0001_0000); // fontRevision
    put_u32(&mut head, 0); // checkSumAdjustment
    put_u32(&mut head, 0x5F0F_3CF5); // magicNumber
    put_u16(&mut head, 0x000B); // baseline y=0, lsb x=0, integer ppem
    put_u16(&mut head, UNITS_PER_EM);
    head.extend_from_slice(&[0; 16]); // created, modified
    for v in [0, 0, em, em] {
        put_i16(&mut head, v); // xMin yMin xMax yMax
    }
    put_u16(&mut head, 0); // macStyle
    put_u16(&mut head, 8); // lowestRecPPEM
    put_i16(&mut head, 2); // fontDirectionHint
    put_i16(&mut head, 1); // indexToLocFormat: long
    put_i16(&mut head, 0); // glyphDataFormat

    let mut hhea = Vec::new();
    put_u32(&mut hhea, 0x0001_0000);
    for v in [em, 0, 0] {
        put_i16(&mut hhea, v); // ascender descender lineGap
    }
    put_u16(&mut hhea, UNITS_PER_EM); // advanceWidthMax
    for v in [0, 0, em, 1, 0, 0, 0, 0, 0, 0, 0] {
        // minLsb minRsb xMaxExtent caretRise caretRun caretOffset
        // reserved x4 metricDataFormat
        put_i16(&mut hhea, v);
    }
    put_u16(&mut hhea, num_glyphs as u16); // numberOfHMetrics

    let mut maxp = Vec::new();
    put_u32(&mut maxp, 0x0001_0000);
    put_u16(&mut maxp, num_glyphs as u16);
    put_u16(&mut maxp, max_points);
    put_u16(&mut maxp, max_contours);
    for v in [0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0] {
        // composite points/contours, zones, twilight, storage, fdefs,
        // idefs, stack, instruction size, component elements/depth
        put_u16(&mut maxp, v);
    }

    let mut os2 = Vec::new();
    put_u16(&mut os2, 4); // version
    put_i16(&mut os2, em); // xAvgCharWidth
    put_u16(&mut os2, 400); // usWeightClass
    put_u16(&mut os2, 5); // usWidthClass
    put_u16(&mut os2, 0); // fsType: installable
    os2.extend_from_slice(&[0; 22]); // sub/superscript, strikeout, family class
    os2.extend_from_slice(&[0; 10]); // panose
    put_u32(&mut os2, 1); // ulUnicodeRange1: Basic Latin
    os2.extend_from_slice(&[0; 12]); // ulUnicodeRange2..4
    os2.extend_from_slice(b"CPLN"); // achVendID
    put_u16(&mut os2, 0x00C0); // fsSelection: REGULAR | USE_TYPO_METRICS
    put_u16(&mut os2, 0x20); // usFirstCharIndex
    put_u16(&mut os2, 0x7E); // usLastCharIndex
    for v in [em, 0, 0] {
        put_i16(&mut os2, v); // sTypoAscender sTypoDescender sTypoLineGap
    }
    put_u16(&mut os2, UNITS_PER_EM); // usWinAscent
    put_u16(&mut os2, 0); // usWinDescent
    put_u32(&mut os2, 1); // ulCodePageRange1: Latin 1
    put_u32(&mut os2, 0);
    for v in [5 * UNITS_PER_PIXEL, 7 * UNITS_PER_PIXEL, 0, 0x20, 1] {
        // sxHeight sCapHeight usDefaultChar usBreakChar usMaxContext
        put_i16(&mut os2, v);
    }

    let mut post = Vec::new();
    put_u32(&mut post, 0x0003_0000); // no glyph names
    put_u32(&mut post, 0); // italicAngle
    put_i16(&mut post, -UNITS_PER_PIXEL); // underlinePosition
    put_i16(&mut post, UNITS_PER_PIXEL); // underlineThickness
    put_u32(&mut post, 1); // isFixedPitch
    post.extend_from_slice(&[0; 16]);

    // One format 4 subtable: ' '..='~' to glyphs 1.., then the end marker.
    let mut cmap = Vec::new();
    put_u16(&mut cmap, 0); // version
    put_u16(&mut cmap, 1); // numTables
    put_u16(&mut cmap, 3); // platform: Windows
    put_u16(&mut cmap, 1); // encoding: Unicode BMP
    put_u32(&mut cmap, 12); // subtable offset
    for v in [4u16, 32, 0, 4, 4, 1, 0] {
        // format length language segCountX2 searchRange entrySelector
        // rangeShift
        put_u16(&mut cmap, v);
    }
    for v in [0x7Eu16, 0xFFFF, 0, 0x20, 0xFFFF] {
        put_u16(&mut cmap, v); // endCode[2] reservedPad startCode[2]
    }
    put_u16(&mut cmap, 1u16.wrapping_sub(0x20)); // idDelta
    put_u16(&mut cmap, 1);
    put_u16(&mut cmap, 0); // idRangeOffset
    put_u16(&mut cmap, 0);

    let mut loca_bytes = Vec::new();
    for offset in loca {
        put_u32(&mut loca_bytes, offset);
    }

    // The table directory must be sorted by tag.
    sfnt(&[
        (*b"OS/2", os2),
        (*b"cmap", cmap),
        (*b"glyf", glyf),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"loca", loca_bytes),
        (*b"maxp", maxp),
        (*b"post", post),
    ])
}

/// The lit runs of a bitmap glyph as `(x_min, x_max, y_min, y_max)` in
/// font units, y up with the baseline at the cell's bottom. Within a row
/// the least-significant bit is the leftmost pixel.
fn glyph_runs(bitmap: &[u8; 8]) -> Vec<(i16, i16, i16, i16)> {
    let mut runs = Vec::new();
    for (row, bits) in bitmap.iter().enumerate() {
        let y_min = (7 - row as i16) * UNITS_PER_PIXEL;
        let mut x = 0;
        while x < 8 {
            if (bits >> x) & 1 == 0 {
                x += 1;
                continue;
            }
            let start = x;
            while x < 8 && (bits >> x) & 1 == 1 {
                x += 1;
            }
            runs.push((
                start as i16 * UNITS_PER_PIXEL,
                x as i16 * UNITS_PER_PIXEL,
                y_min,
                y_min + UNITS_PER_PIXEL,
            ));
        }
    }
    runs
}

/// A simple glyph of rectangular contours, each wound clockwise, padded to
/// four bytes for the long `loca` format.
fn write_simple_glyph(out: &mut Vec<u8>, runs: &[(i16, i16, i16, i16)]) {
    put_i16(out, runs.len() as i16);
    put_i16(out, runs.iter().map(|r| r.0).min().unwrap());
    put_i16(out, runs.iter().map(|r| r.2).min().unwrap());
    put_i16(out, runs.iter().map(|r| r.1).max().unwrap());
    put_i16(out, runs.iter().map(|r| r.3).max().unwrap());
    for i in 0..runs.len() {
        put_u16(out, (4 * i + 3) as u16); // endPtsOfContours
    }
    put_u16(out, 0); // instructionLength
    out.extend(std::iter::repeat_n(0x01u8, 4 * runs.len())); // on-curve
    let points: Vec<(i16, i16)> = runs
        .iter()
        .flat_map(|&(x0, x1, y0, y1)| [(x0, y0), (x0, y1), (x1, y1), (x1, y0)])
        .collect();
    let mut last = 0;
    for &(x, _) in &points {
        put_i16(out, x - last);
        last = x;
    }
    last = 0;
    for &(_, y) in &points {
        put_i16(out, y - last);
        last = y;
    }
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// Wrap tables (already sorted by tag) in an sfnt container.
fn sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let count = tables.len() as u16;
    let pow2 = 1u16 << (15 - count.leading_zeros());
    let mut out = Vec::new();
    put_u32(&mut out, 0x0001_0000);
    put_u16(&mut out, count);
    put_u16(&mut out, pow2 * 16); // searchRange
    put_u16(&mut out, pow2.trailing_zeros() as u16); // entrySelector
    put_u16(&mut out, count * 16 - pow2 * 16); // rangeShift
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, data) in tables {
        out.extend_from_slice(tag);
        put_u32(&mut out, table_checksum(data));
        put_u32(&mut out, offset as u32);
        put_u32(&mut out, data.len() as u32);
        body.extend_from_slice(data);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        offset = 12 + 16 * tables.len() + body.len();
    }
    out.extend_from_slice(&body);
    out
}

fn table_checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_i16(out: &mut Vec<u8>, v: i16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::menu::MenuNav;

    /// The configuration screen on the Display page, where the window
    /// position is typed, typing nothing yet.
    fn launcher() -> UiState {
        let mut state = LauncherState::new(launcher::MachineSetup::default());
        state.tab = LauncherTab::AvDisplay;
        UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        }
    }

    fn state(ui: &mut UiState) -> &mut LauncherState {
        match ui.panel.as_mut() {
            Some(Panel::Launcher(state)) => state,
            _ => unreachable!("the launcher is up"),
        }
    }

    /// One frame of the display surface, a point to a canvas pixel, with
    /// `events` for its input. Typing follows the launcher first, as the
    /// window has it do.
    fn frame(
        ctx: &egui::Context,
        ui: &mut UiState,
        typing: &mut Typing,
        images: &mut Images,
        events: Vec<egui::Event>,
    ) {
        typing.follow(state(ui));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                vec2(
                    crate::video::window::texture_width(1) as f32,
                    crate::video::window::texture_height(1) as f32,
                ),
            )),
            events,
            ..Default::default()
        };
        // Nothing draws these frames, so their texture updates go
        // explicitly (egui asserts that none is dropped unseen).
        ctx.run_ui(input, |root| {
            show_display(root, ui, Light::default(), typing, images);
        })
        .textures_delta
        .clear();
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// The text field stands in for a value box only while the launcher is
    /// typing in it, starts from what the box held, and takes the keyboard
    /// on its own.
    #[test]
    fn the_text_field_follows_the_box_being_typed_in() {
        let ctx = egui::Context::default();
        configure(&ctx);
        let (mut typing, mut images) = (Typing::default(), Images::default());
        let mut ui = launcher();
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        assert!(!typing.active(), "nothing is being typed in");

        state(&mut ui).begin_edit_window_position();
        state(&mut ui).set_edit_buffer("10, 20".into());
        // The first frame asks for the keyboard, the next has it.
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        assert_eq!(typing.target, Some(EditTarget::WindowPosition));
        assert!(typing.drawn.is_some(), "the box is on the page");
        assert_eq!(typing.text, "10, 20");
        frame(
            &ctx,
            &mut ui,
            &mut typing,
            &mut images,
            vec![egui::Event::Text("5".into())],
        );
        assert_eq!(typing.text, "10, 205", "typed at the end");
        assert_eq!(typing.release.take(), None, "and still typing");

        // The launcher letting go of the box lets go of the field.
        state(&mut ui).edit_cancel();
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        assert!(!typing.active());
    }

    /// Return and Tab take what was typed and Escape throws it away; either
    /// way the field gives the keyboard back, and a refused value can have
    /// it again to be put right.
    #[test]
    fn return_commits_and_escape_cancels() {
        let ctx = egui::Context::default();
        configure(&ctx);
        for (pressed, release) in [
            (egui::Key::Enter, Release::Commit),
            (egui::Key::Tab, Release::Commit),
            (egui::Key::Escape, Release::Cancel),
        ] {
            let (mut typing, mut images) = (Typing::default(), Images::default());
            let mut ui = launcher();
            state(&mut ui).begin_edit_window_position();
            frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
            frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
            assert_eq!(typing.release.take(), None);
            frame(&ctx, &mut ui, &mut typing, &mut images, vec![key(pressed)]);
            assert_eq!(typing.release.take(), Some(release), "{pressed:?}");
            assert_eq!(typing.release.take(), None, "told once");

            // Asked back, the field has the keyboard again next frame and
            // reports nothing until it is let go once more.
            typing.refocus();
            frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
            frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
            assert_eq!(typing.release.take(), None, "{pressed:?}");
            frame(&ctx, &mut ui, &mut typing, &mut images, vec![key(pressed)]);
            assert_eq!(typing.release.take(), Some(release), "{pressed:?}");
        }
    }

    /// Settling hands the field's outcome to the launcher and asks for a
    /// frame only when that changes what is shown: an open field left alone
    /// asks for none, Return closes the box, and a click lands on the box
    /// itself only inside it.
    #[test]
    fn settling_draws_again_only_when_something_changed() {
        let ctx = egui::Context::default();
        configure(&ctx);
        let (mut typing, mut images) = (Typing::default(), Images::default());
        let mut ui = launcher();
        state(&mut ui).begin_edit_window_position();
        state(&mut ui).set_edit_buffer("10, 20".into());
        // The frame that asks for the keyboard needs one more to show the
        // caret; after that an idle field asks for nothing.
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        assert!(settle(&mut typing, state(&mut ui)), "the caret is to come");
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        assert!(!settle(&mut typing, state(&mut ui)), "an idle field");
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        assert!(!settle(&mut typing, state(&mut ui)), "and still idle");

        // The box answers for clicks inside it and nowhere else.
        let rect = typing.drawn.expect("the box is on the page");
        let middle = egui::pos2((rect.x + rect.w / 2) as f32, (rect.y + rect.h / 2) as f32);
        assert!(typing.contains(middle));
        assert!(!typing.contains(egui::pos2(rect.x as f32 - 1.0, middle.y)));
        assert!(!typing.contains(egui::pos2(middle.x, (rect.y + rect.h) as f32)));

        // Typing reaches the launcher's buffer; Return commits it there.
        frame(
            &ctx,
            &mut ui,
            &mut typing,
            &mut images,
            vec![egui::Event::Text("5".into())],
        );
        settle(&mut typing, state(&mut ui));
        assert_eq!(state(&mut ui).edit_buffer(), "10, 205");
        frame(
            &ctx,
            &mut ui,
            &mut typing,
            &mut images,
            vec![key(egui::Key::Enter)],
        );
        assert!(settle(&mut typing, state(&mut ui)), "the box closed");
        assert_eq!(state(&mut ui).editing(), None);
        assert!(!typing.active(), "and the keys are the panel's again");
        assert_eq!(
            state(&mut ui).row_value(launcher::LauncherField::WindowPosition),
            "10, 205"
        );
    }

    /// A box that leaves the page with the keyboard in it -- a boot
    /// priority scrolled out of view -- lets go of it, as a click
    /// elsewhere would, rather than holding every key with no field to
    /// type them into.
    #[test]
    fn a_box_scrolled_out_of_view_lets_go_of_the_keyboard() {
        use launcher::LauncherField as F;
        let ctx = egui::Context::default();
        configure(&ctx);
        let (mut typing, mut images) = (Typing::default(), Images::default());
        let mut ui = launcher();
        let launcher_state = state(&mut ui);
        launcher_state
            .setup
            .select_model(Some(crate::config::MachineModel::A1200));
        launcher_state.setup.cycle(F::ScsiController, true);
        launcher_state.setup.cycle(F::LideBoard, true);
        // Ten drives: one more than the page shows.
        for f in [
            F::IdeMaster,
            F::LideDrive0,
            F::ScsiUnit0,
            F::ScsiUnit1,
            F::ScsiUnit2,
            F::ScsiUnit3,
            F::ScsiUnit4,
            F::ScsiUnit5,
            F::ScsiUnit6,
        ] {
            launcher_state
                .setup
                .set_path(f, std::path::PathBuf::from("disk.hdf"));
        }
        launcher_state.tab = LauncherTab::BootPriority;
        assert!(launcher_state.setup.boot_order_scrolls());
        launcher_state.begin_edit_drive_bootpri(F::IdeMasterBoot);
        assert_eq!(
            launcher_state.editing(),
            Some(EditTarget::DriveBootpri(F::IdeMasterBoot))
        );
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        settle(&mut typing, state(&mut ui));
        assert!(typing.drawn.is_some(), "the box is on the page");

        // The wheel takes its row out of view.
        state(&mut ui).setup.scroll_boot_order(3);
        frame(&ctx, &mut ui, &mut typing, &mut images, Vec::new());
        assert!(typing.drawn.is_none(), "the box has gone");
        assert!(settle(&mut typing, state(&mut ui)));
        assert_eq!(state(&mut ui).editing(), None, "the edit is over");
        assert!(!typing.active(), "and the keys are the panel's again");
    }

    /// The font made from the bitmap lays text out in the bitmap's own
    /// cells: eight pixels a character and eight a line at size 8.
    #[test]
    fn the_pixel_font_keeps_the_bitmap_cell() {
        let ctx = egui::Context::default();
        configure(&ctx);
        // Fonts set now are the next frame's.
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        let size = ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    "Copperline".into(),
                    FontId::proportional(8.0),
                    Color32::WHITE,
                )
                .size()
        });
        assert_eq!(size, vec2(80.0, 8.0));
    }
}
