// SPDX-License-Identifier: GPL-3.0-or-later

//! The window's half of the surfaces egui draws: the configuration screen
//! and the menu (src/video/ui/configuration/egui_view.rs), and the status
//! bar.
//!
//! Their points are canvas pixels -- the texture's logical pixels, which the
//! hit-test, the keyboard focus and every layout function measure in -- and
//! egui renders them at the window's own scale, so text is rasterised at the
//! surface's resolution rather than scaled up from the texture. The main
//! thread lays them out and tessellates; the meshes go to whichever thread
//! presents the frame (egui_paint.rs), which paints them over the passes.
//!
//! The window keeps the controller: clicks, held steppers, the pointer
//! taking the focus and every rule in app_nav.rs go through the surfaces'
//! own hit-tests, exactly as they always have. egui takes input directly
//! only while a value box is being typed in.

use super::*;
use crate::video::ui::egui_view;
use winit::dpi::PhysicalPosition;

/// Where the canvas sits on the surface: the physical position of its
/// origin, and how many physical pixels one canvas pixel covers across and
/// down. Read off the same layout the cursor mapping inverts, axis by axis,
/// so the two agree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Placement {
    pub(super) origin: (f32, f32),
    /// Across: the scale egui renders at.
    pub(super) scale: f32,
    /// Down. A layout that fits each axis to a whole multiple of its own
    /// (PAL TV integer scaling) makes this differ from `scale`, and the
    /// meshes are stretched to it.
    pub(super) scale_y: f32,
}

impl Placement {
    pub(super) fn of(layout: &PresentLayout) -> Option<Self> {
        let (dx, dy, dw, dh) = layout.display_dst;
        let (sx, sy, sw, sh) = layout.src_canvas;
        Self::fitting((dx, dy, dw, dh), (sx, sy, sw, sh))
    }

    /// The canvas rect `src` (canvas pixels) shown at `dst` (physical).
    fn fitting(dst: (u32, u32, u32, u32), src: (usize, usize, usize, usize)) -> Option<Self> {
        let (dx, dy, dw, dh) = dst;
        let (sx, sy, sw, sh) = src;
        if dw == 0 || dh == 0 || sw == 0 || sh == 0 {
            return None;
        }
        let scale = dw as f32 / sw as f32;
        let scale_y = dh as f32 / sh as f32;
        Some(Self {
            origin: (
                dx as f32 - sx as f32 * scale,
                dy as f32 - sy as f32 * scale_y,
            ),
            scale,
            scale_y,
        })
    }

    /// A physical surface position, in canvas pixels.
    fn canvas(&self, phys: PhysicalPosition<f64>) -> egui::Pos2 {
        egui::pos2(
            (phys.x as f32 - self.origin.0) / self.scale,
            (phys.y as f32 - self.origin.1) / self.scale_y,
        )
    }

    /// A canvas rect in the window's logical points (`pixels_per_point`
    /// physical pixels each), for what egui-winit hands the window.
    fn window_rect(&self, rect: egui::Rect, pixels_per_point: f32) -> egui::Rect {
        let at = |p: egui::Pos2| {
            egui::pos2(
                (self.origin.0 + p.x * self.scale) / pixels_per_point,
                (self.origin.1 + p.y * self.scale_y) / pixels_per_point,
            )
        };
        egui::Rect::from_min_max(at(rect.min), at(rect.max))
    }
}

/// The status bar for one frame, as egui draws it.
pub(super) struct BarFrame<'a> {
    pub(super) placement: Placement,
    pub(super) view: &'a StatusBarView,
    /// Where the keyboard's focus stands on the bar, as
    /// `statusbar::set_nav_light` takes it.
    pub(super) focus: Option<BarControl>,
    pub(super) mix: f32,
    pub(super) elsewhere: bool,
}

/// What the bar was last drawn from: the same again draws the same meshes.
#[derive(Clone, PartialEq)]
struct BarKey {
    view: StatusBarView,
    focus: Option<BarControl>,
    mix: u32,
    elsewhere: bool,
    placement: Placement,
    surface: (u32, u32),
    /// The floppy counter's format: read by the bar beside the view.
    track: crate::config::TrackDisplay,
}

pub(super) struct EguiOverlay {
    /// The display canvas's context: the configuration screen and the menu.
    display: egui::Context,
    /// The chrome band's context: the status bar, which a sub-rect layout
    /// places at a scale of its own.
    chrome: egui::Context,
    /// The bar's last frame, reused while nothing it shows has changed:
    /// it is on screen all the time, and mostly still.
    bar_last: Option<(BarKey, egui_paint::EguiSurfaceJob)>,
    /// Keyboard and IME for the display canvas, while a box is typed in.
    input: egui_winit::State,
    /// Copy and paste through Copperline's Wayland data device there.
    clipboard: wayland_data::EguiClipboard,
    typing: egui_view::Typing,
    images: egui_view::Images,
    placement: Option<Placement>,
    /// The last frame's focus and pointer lighting, for a frame run between
    /// redraws.
    light: egui_view::Light,
    pointer: Option<egui::Pos2>,
    /// Texture updates from frames the present worker turned away, owed to
    /// the next frame that goes.
    unsent: [egui::TexturesDelta; egui_paint::SURFACES],
    /// egui asked to be drawn again straight away (a caret, a selection).
    repaint: bool,
}

impl EguiOverlay {
    pub(super) fn new(
        window: &Window,
        max_texture_side: usize,
        clipboard: Option<wayland_data::Clipboard>,
    ) -> Self {
        let display = egui::Context::default();
        egui_view::configure(&display);
        let chrome = egui::Context::default();
        egui_view::configure(&chrome);
        let input = egui_winit::State::new(
            display.clone(),
            egui::ViewportId::ROOT,
            &wayland_data::EguiDisplay::new(window, clipboard.as_ref()),
            Some(window.scale_factor() as f32),
            None,
            Some(max_texture_side),
        );
        Self {
            display,
            chrome,
            bar_last: None,
            input,
            clipboard: wayland_data::EguiClipboard::new(clipboard),
            typing: egui_view::Typing::default(),
            images: egui_view::Images::default(),
            placement: None,
            light: egui_view::Light::default(),
            pointer: None,
            unsent: Default::default(),
            repaint: false,
        }
    }

    /// Hand the keyboard-side window events egui needs. Returns whether it
    /// took the event: while a box is typed in, the keys are the box's.
    fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                if !self.typing.active() {
                    return false;
                }
                let _ = self
                    .clipboard
                    .on_window_event(&mut self.input, window, event);
                true
            }
            WindowEvent::ModifiersChanged(_) | WindowEvent::Focused(_) => {
                let _ = self
                    .clipboard
                    .on_window_event(&mut self.input, window, event);
                false
            }
            _ => false,
        }
    }

    fn pointer_moved(&mut self, phys: PhysicalPosition<f64>) {
        let Some(placement) = self.placement else {
            return;
        };
        let pos = placement.canvas(phys);
        self.pointer = Some(pos);
        self.input
            .egui_input_mut()
            .events
            .push(egui::Event::PointerMoved(pos));
    }

    fn pointer_button(&mut self, button: MouseButton, pressed: bool) {
        let button = match button {
            MouseButton::Left => egui::PointerButton::Primary,
            MouseButton::Right => egui::PointerButton::Secondary,
            MouseButton::Middle => egui::PointerButton::Middle,
            _ => return,
        };
        let Some(pos) = self.pointer else {
            return;
        };
        let input = self.input.egui_input_mut();
        let modifiers = input.modifiers;
        input.events.push(egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        });
    }

    fn pointer_left(&mut self) {
        self.pointer = None;
        self.input
            .egui_input_mut()
            .events
            .push(egui::Event::PointerGone);
    }

    /// Run one surface's frame over the canvas, placed by `placement`, and
    /// turn it into what the GPU side draws: meshes in surface pixels and
    /// the texture updates they need.
    fn surface_job(
        context: &egui::Context,
        mut input: egui::RawInput,
        placement: Placement,
        surface: (u32, u32),
        draw: impl FnMut(&mut egui::Ui),
    ) -> (egui_paint::EguiSurfaceJob, egui::PlatformOutput, bool) {
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(FB_WIDTH as f32, window_present_height() as f32),
        ));
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(placement.scale);
        let output = context.run_ui(input, draw);
        let repaint = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|viewport| viewport.repaint_delay.is_zero());
        let mut jobs = context.tessellate(output.shapes, output.pixels_per_point);
        // To where the surface shows the canvas: moved by whole physical
        // pixels, so every edge stays on the pixel grid egui laid it on, and
        // stretched down where the layout's vertical scale is not egui's.
        let shift = egui::vec2(
            placement.origin.0.round() / placement.scale,
            placement.origin.1.round() / placement.scale,
        );
        let stretch = placement.scale_y / placement.scale;
        let place = |p: egui::Pos2| egui::pos2(p.x + shift.x, p.y * stretch + shift.y);
        for job in &mut jobs {
            job.clip_rect =
                egui::Rect::from_min_max(place(job.clip_rect.min), place(job.clip_rect.max));
            if let egui::epaint::Primitive::Mesh(mesh) = &mut job.primitive {
                for vertex in &mut mesh.vertices {
                    vertex.pos = place(vertex.pos);
                }
            }
        }
        (
            egui_paint::EguiSurfaceJob {
                jobs: std::sync::Arc::new(jobs),
                textures: output.textures_delta,
                size_in_pixels: [surface.0.max(1), surface.1.max(1)],
                pixels_per_point: output.pixels_per_point,
            },
            output.platform_output,
            repaint,
        )
    }

    /// Run the display canvas's frame: the configuration screen and the
    /// menu, with the keys and pointer egui has queued since the last one.
    fn display_job(
        &mut self,
        window: &Window,
        surface: (u32, u32),
        placement: Placement,
        state: &UiState,
        light: egui_view::Light,
    ) -> egui_paint::EguiSurfaceJob {
        self.placement = Some(placement);
        self.light = light;
        let input = self.input.take_egui_input(window);
        let (typing, images) = (&mut self.typing, &mut self.images);
        let (surface_job, mut platform, repaint) =
            Self::surface_job(&self.display, input, placement, surface, |ui| {
                egui_view::show_display(ui, state, light, typing, images);
            });
        self.images.end_frame();
        // egui reports the text field in canvas points, and egui-winit
        // takes them for the window's: where the canvas sits, and at what
        // scale, is the overlay's to add, or the IME's candidates open
        // away from the field.
        if let Some(ime) = platform.ime.as_mut() {
            let pixels_per_point = egui_winit::pixels_per_point(&self.display, window);
            ime.rect = placement.window_rect(ime.rect, pixels_per_point);
            ime.cursor_rect = placement.window_rect(ime.cursor_rect, pixels_per_point);
        }
        self.clipboard.take_copies(&mut platform);
        self.input.handle_platform_output(window, platform);
        self.repaint |= repaint;
        surface_job
    }

    /// Lay out what egui draws over the frame about to be presented.
    fn prepare(
        &mut self,
        window: &Window,
        surface: (u32, u32),
        display: Option<(Placement, &UiState, egui_view::Light)>,
        bar: Option<BarFrame<'_>>,
    ) -> egui_paint::EguiJob {
        let mut job = egui_paint::EguiJob::default();
        self.repaint = false;
        if let Some((placement, state, light)) = display {
            let surface_job = self.display_job(window, surface, placement, state, light);
            job.surfaces[0] = Some(Box::new(surface_job));
        } else {
            // Nothing takes it: input for a canvas not shown is dropped.
            let _ = self.input.take_egui_input(window);
            self.placement = None;
        }
        if let Some(bar) = bar {
            job.surfaces[1] = Some(Box::new(self.bar_job(surface, bar)));
        } else {
            self.bar_last = None;
        }
        // Texture updates a turned-away frame never delivered go first.
        for (owed, surface) in self.unsent.iter_mut().zip(job.surfaces.iter_mut()) {
            if owed.is_empty() {
                continue;
            }
            let mut textures = std::mem::take(owed);
            match surface {
                Some(surface) => {
                    textures.append(std::mem::take(&mut surface.textures));
                    surface.textures = textures;
                }
                None => *owed = textures,
            }
        }
        job
    }

    /// The status bar's meshes: the last frame's while nothing it shows has
    /// changed, otherwise drawn afresh.
    fn bar_job(&mut self, surface: (u32, u32), bar: BarFrame<'_>) -> egui_paint::EguiSurfaceJob {
        let key = BarKey {
            view: bar.view.clone(),
            focus: bar.focus,
            mix: bar.mix.to_bits(),
            elsewhere: bar.elsewhere,
            placement: bar.placement,
            surface,
            track: crate::video::track_display(),
        };
        if let Some((last, job)) = &self.bar_last {
            if *last == key {
                // The same meshes, and no texture updates: those went with
                // the frame that first drew them.
                return egui_paint::EguiSurfaceJob {
                    textures: egui::TexturesDelta::default(),
                    ..job.clone()
                };
            }
        }
        let view = bar.view;
        let (job, _, _) = Self::surface_job(
            &self.chrome,
            egui::RawInput::default(),
            bar.placement,
            surface,
            |ui| {
                statusbar::set_nav_light(bar.focus, bar.mix, bar.elsewhere);
                let mut typing = egui_view::Typing::default();
                let mut images = egui_view::Images::default();
                let mut canvas = egui_view::EguiCanvas::new(ui, &mut typing, &mut images);
                draw_status_bar(&mut canvas, view);
                statusbar::set_nav_light(None, 0.0, false);
            },
        );
        self.bar_last = Some((
            key,
            egui_paint::EguiSurfaceJob {
                textures: egui::TexturesDelta::default(),
                ..job.clone()
            },
        ));
        job
    }

    /// Whether the pointer is over the box being typed in.
    fn pointer_in_typing_box(&self) -> bool {
        self.pointer.is_some_and(|pos| self.typing.contains(pos))
    }

    /// A frame that never went -- turned away by the present worker, or
    /// with no buffer to go in -- owes its texture updates to the next one.
    pub(super) fn unsent(&mut self, job: egui_paint::EguiJob) {
        for (owed, surface) in self.unsent.iter_mut().zip(job.surfaces) {
            if let Some(surface) = surface {
                owed.append(surface.textures);
            }
        }
    }
}

/// What egui drew over a frame, which the texture then leaves out.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct OverlayDrawn {
    pub(super) launcher: bool,
    pub(super) menu: bool,
    pub(super) bar: bool,
}

impl App {
    /// Whether egui draws the configuration screen this frame.
    pub(super) fn launcher_on_egui(&self) -> bool {
        self.render.is_some() && matches!(&self.ui.panel, Some(Panel::Launcher(_)))
    }

    /// Whether egui draws the menu this frame.
    pub(super) fn menu_on_egui(&self) -> bool {
        self.render.is_some() && self.ui.menu_open
    }

    /// Whether egui draws the status bar this frame: it is shown at all.
    pub(super) fn bar_on_egui(&self) -> bool {
        self.render.is_some() && !crate::video::status_bar_hidden()
    }

    /// Feed egui the window events it takes. Returns whether the event is
    /// spent: keys while a box is being typed in.
    pub(super) fn route_egui_overlay_event(&mut self, event: &WindowEvent) -> bool {
        if !self.launcher_on_egui() {
            return false;
        }
        let (Some(egui), Some(r)) = (self.egui_overlay.as_mut(), self.render.as_ref()) else {
            return false;
        };
        let typing = egui.typing.active();
        let mut flush = false;
        let taken = match event {
            WindowEvent::CursorMoved { position, .. } => {
                egui.pointer_moved(*position);
                false
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = *state == ElementState::Pressed;
                egui.pointer_button(*button, pressed);
                // A click in the box being typed in is the text field's: it
                // places the caret or selects. The panel never sees it, or
                // its hit-test would commit the box and open it again.
                let inside = typing && egui.pointer_in_typing_box();
                flush = typing && pressed && !inside;
                inside
            }
            WindowEvent::CursorLeft { .. } => {
                egui.pointer_left();
                false
            }
            other => egui.on_window_event(&r.window, other),
        };
        // A click anywhere else acts on the panel straight away, and may
        // commit the box: what egui still holds for it goes in first.
        if flush {
            self.flush_egui_typing();
        }
        // While a box is typed in, everything the pointer and keys do can
        // move its caret or selection.
        if taken || typing {
            self.request_redraw();
        }
        taken
    }

    /// Run the display canvas's frame now, between redraws, so the box
    /// being typed in holds everything egui has queued for it (the keys
    /// since the last frame, and the click taking the keyboard from it)
    /// before the click acts on the panel. The frame is not shown: its
    /// texture updates go with the next one.
    fn flush_egui_typing(&mut self) {
        let display_src = self.display_canvas_src();
        let rtg = self.rtg_present_dims;
        let Some(r) = self.render.as_ref() else {
            return;
        };
        let surface = r.surface_size;
        let window = r.window.clone();
        let Some(placement) = Placement::of(&main_ui_present_layout(r, display_src, rtg)) else {
            return;
        };
        let Some(egui) = self.egui_overlay.as_mut() else {
            return;
        };
        let light = egui.light;
        let job = egui.display_job(&window, surface, placement, &self.ui, light);
        egui.unsent(egui_paint::EguiJob {
            surfaces: [Some(Box::new(job)), None],
        });
        self.settle_egui_typing();
    }

    /// Lay out what egui draws over the frame about to be presented, and
    /// settle what the configuration screen's typing did. Returns the job
    /// for the GPU side and whether egui draws the configuration screen,
    /// the menu and the status bar.
    pub(super) fn prepare_egui_overlay(
        &mut self,
        light: egui_view::Light,
        bar_view: &StatusBarView,
        bar_light: (Option<BarControl>, f32, bool),
    ) -> (egui_paint::EguiJob, OverlayDrawn) {
        let empty = (egui_paint::EguiJob::default(), OverlayDrawn::default());
        if self.render.is_none() {
            return empty;
        }
        if self.egui_overlay.is_none() {
            let Some(r) = self.render.as_mut() else {
                return empty;
            };
            let window = r.window.clone();
            let Some(gpu) = r.gpu_mut() else {
                return empty;
            };
            let max = gpu.pixels.device().limits().max_texture_dimension_2d as usize;
            let clipboard = self.wayland_data.as_ref().map(|data| data.clipboard());
            self.egui_overlay = Some(EguiOverlay::new(&window, max, clipboard));
        }
        let launcher_on_egui = self.launcher_on_egui();
        let menu_on_egui = self.menu_on_egui();
        let display_src = self.display_canvas_src();
        let rtg = self.rtg_present_dims;
        let Some(r) = self.render.as_ref() else {
            return empty;
        };
        let surface = r.surface_size;
        let window = r.window.clone();
        let placement = Placement::of(&main_ui_present_layout(r, display_src, rtg));
        // The bar sits where the scaler draws the chrome band: its own rect
        // in a sub-rect layout, the foot of the canvas otherwise.
        let board = main_present_layout(r, display_src, rtg);
        let bar_placement = match board.chrome_dst {
            Some(chrome) => Placement::fitting(
                chrome,
                (
                    0,
                    present_height(),
                    FB_WIDTH,
                    window_present_height() - present_height(),
                ),
            ),
            None => Placement::of(&board),
        };
        let bar_on_egui = self.bar_on_egui();
        let egui = self.egui_overlay.as_mut().expect("made above");
        let bar = bar_placement
            .filter(|_| bar_on_egui)
            .map(|placement| BarFrame {
                placement,
                view: bar_view,
                focus: bar_light.0,
                mix: bar_light.1,
                elsewhere: bar_light.2,
            });
        let bar_drawn = bar.is_some();
        let display = match placement {
            Some(placement) if launcher_on_egui || menu_on_egui => {
                if let Some(Panel::Launcher(state)) = self.ui.panel.as_ref() {
                    egui.typing.follow(state);
                }
                Some((placement, &self.ui, light))
            }
            _ => None,
        };
        let drawn = display.is_some();
        let job = egui.prepare(&window, surface, display, bar);
        let repaint = egui.repaint;
        if drawn && launcher_on_egui {
            self.settle_egui_typing();
        }
        if repaint {
            self.request_redraw();
        }
        (
            job,
            OverlayDrawn {
                launcher: drawn && launcher_on_egui,
                menu: drawn && menu_on_egui,
                bar: bar_drawn,
            },
        )
    }

    /// Hand what egui's text field holds back to the launcher and act on
    /// what became of it (`egui_view::settle`), drawing again only when
    /// that changed what is shown.
    fn settle_egui_typing(&mut self) {
        let Some(egui) = self.egui_overlay.as_mut() else {
            return;
        };
        let Some(Panel::Launcher(state)) = self.ui.panel.as_mut() else {
            return;
        };
        if egui_view::settle(&mut egui.typing, state) {
            self.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MachineModel;
    use crate::video::launcher::{LauncherTab, MachineSetup};
    use LauncherField as F;

    /// The canvas is placed axis by axis, as the cursor mapping inverts
    /// it: a layout that scales across and down by different whole
    /// multiples (PAL TV integer scaling) puts each canvas pixel where the
    /// pointer finds it, and the window rect handed to the IME lands on the
    /// same physical pixels.
    #[test]
    fn placement_follows_the_cursor_mapping_on_both_axes() {
        // A 716x570 canvas at 4x across and 3.5x down, letterboxed.
        let layout = PresentLayout {
            src_canvas: (0, 0, 716, 570),
            display_dst: (488, 82, 2864, 1995),
            filter: scaler::ScaleFilter::Nearest,
            chrome_dst: None,
            factors: None,
        };
        let placement = Placement::of(&layout).expect("a placement");
        assert_eq!((placement.scale, placement.scale_y), (4.0, 3.5));
        for (x, y) in [(0usize, 0usize), (300, 380), (715, 569), (357, 12)] {
            // The middle of canvas pixel (x, y), drawn where the placement
            // says, is where the cursor mapping finds that pixel.
            let phys = PhysicalPosition::new(
                f64::from(placement.origin.0 + (x as f32 + 0.5) * placement.scale),
                f64::from(placement.origin.1 + (y as f32 + 0.5) * placement.scale_y),
            );
            assert_eq!(
                layout.cursor_position(phys),
                Some((x as i32, y as i32)),
                "canvas pixel ({x}, {y})"
            );
            let back = placement.canvas(phys);
            assert_eq!((back.x.floor() as usize, back.y.floor() as usize), (x, y));
        }
        // At a window scale of 2, a canvas rect reaches the window in
        // logical points: physical pixels halved.
        let rect = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(30.0, 40.0));
        let window = placement.window_rect(rect, 2.0);
        assert_eq!(
            window.min,
            egui::pos2((488.0 + 40.0) / 2.0, (82.0 + 70.0) / 2.0)
        );
        assert_eq!(
            window.max,
            egui::pos2((488.0 + 120.0) / 2.0, (82.0 + 140.0) / 2.0)
        );
    }

    pub(super) fn ui_state(state: LauncherState) -> UiState {
        UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: crate::video::menu::MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        }
    }

    /// Machines that between them bring in every kind of row: stock, AGA,
    /// a CD32, and a 32-bit machine with every drive bay, a fixed RAM fill,
    /// chosen paths and a disk in DF0.
    fn setups() -> Vec<(&'static str, MachineSetup)> {
        let mut out = Vec::new();
        for (name, model) in [
            ("default", None),
            ("a1200", Some(MachineModel::A1200)),
            ("cd32", Some(MachineModel::Cd32)),
        ] {
            let mut setup = MachineSetup::default();
            setup.select_model(model);
            out.push((name, setup));
        }
        let mut full = MachineSetup::default();
        full.select_model(Some(MachineModel::A4000));
        while full.row_hidden(F::RamPattern) {
            full.cycle(F::RamInit, true);
        }
        full.set_path(F::Rom, "kick31.rom".into());
        full.set_path(F::PathsScreenshots, "/Users/someone/Pictures/Shots".into());
        full.cycle(F::ScsiController, true);
        full.cycle(F::LideBoard, true);
        for f in [
            F::IdeMaster,
            F::IdeSlave,
            F::ScsiUnit0,
            F::ScsiUnit1,
            F::ScsiUnit6,
        ] {
            full.set_path(f, format!("{f:?}.hdf").into());
        }
        full.set_path(F::Df0Image, "Workbench.adf".into());
        out.push(("full", full));
        out
    }

    const TABS: [LauncherTab; 31] = [
        LauncherTab::System,
        LauncherTab::Cpu,
        LauncherTab::Memory,
        LauncherTab::Rom,
        LauncherTab::Floppy,
        LauncherTab::Storage,
        LauncherTab::FluxBridge,
        LauncherTab::BootPriority,
        LauncherTab::HostFs,
        LauncherTab::Whdload,
        LauncherTab::WhdloadLibrary,
        LauncherTab::HostDisk,
        LauncherTab::Cd,
        LauncherTab::Lide,
        LauncherTab::Copperhf,
        LauncherTab::Sf2000Sd,
        LauncherTab::IoPorts,
        LauncherTab::IoParallel,
        LauncherTab::IoNetworking,
        LauncherTab::IoAudio,
        LauncherTab::Input,
        LauncherTab::Netplay,
        LauncherTab::Zorro,
        LauncherTab::AvAudio,
        LauncherTab::AvVideo,
        LauncherTab::AvDisplay,
        LauncherTab::AvEmulation,
        LauncherTab::AvPaths,
        LauncherTab::CreateFloppy,
        LauncherTab::CreateHard,
        LauncherTab::CreateGeometry,
    ];

    /// The texture as `ui::draw` paints it at `scale`, lit as the window
    /// lights it.
    fn in_texture(ui: &UiState, light: egui_view::Light, scale: usize) -> Vec<u32> {
        let (w, h) = (texture_width(scale), texture_height(scale));
        let mut frame = vec![0u8; w * h * 4];
        ui::set_nav_light(light.focus, light.mix, light.open, light.elsewhere);
        ui::draw(&mut frame, scale, ui, light.hover, None);
        ui::set_nav_light(None, 0.0, false, false);
        frame
            .chunks_exact(4)
            .map(|p| u32::from_le_bytes(p.try_into().unwrap()))
            .collect()
    }

    /// Run `draw` as an egui frame over the canvas at `ppp` pixels per
    /// point and render it offscreen; the canvas is `texture_width(1)` by
    /// `texture_height(1)` points.
    pub(super) fn on_egui(
        gpu: &super::super::crt_shader::TestGpu,
        ppp: f32,
        mut draw: impl FnMut(&mut egui::Ui),
    ) -> Vec<u32> {
        let (cw, ch) = (texture_width(1) as f32, texture_height(1) as f32);
        let (w, h) = ((cw * ppp).round() as u32, (ch * ppp).round() as u32);
        let ctx = egui::Context::default();
        egui_view::configure(&ctx);
        let mut outputs = Vec::new();
        for _ in 0..3 {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(cw, ch),
                )),
                focused: true,
                ..Default::default()
            };
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .native_pixels_per_point = Some(ppp);
            outputs.push(ctx.run_ui(input, &mut draw));
        }
        let (device, queue) = (gpu.device(), gpu.queue());
        // The layer egui draws into in the window: blending on the encoded
        // values, as the texture does.
        let format = egui_paint::LAYER_FORMAT;
        let mut renderer = egui_wgpu::Renderer::new(
            device,
            format,
            egui_wgpu::RendererOptions {
                dithering: false,
                ..Default::default()
            },
        );
        for output in &outputs {
            for (id, delta) in &output.textures_delta.set {
                renderer.update_texture(device, queue, *id, delta);
            }
        }
        let last = outputs.last().unwrap();
        let jobs = ctx.tessellate(last.shapes.clone(), last.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [w, h],
            pixels_per_point: last.pixels_per_point,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui_overlay_test"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        let commands = renderer.update_buffers(device, queue, &mut encoder, &jobs, &screen);
        queue.submit(commands);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui_overlay_test"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        let padded = (w * 4).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(padded * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            target.size(),
        );
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                tx.send(result).unwrap();
            });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let pixels = buffer
            .slice(..)
            .get_mapped_range()
            .chunks_exact(padded as usize)
            .flat_map(|row| {
                row[..w as usize * 4]
                    .chunks_exact(4)
                    .map(|p| u32::from_le_bytes(p.try_into().unwrap()))
                    .collect::<Vec<_>>()
            })
            .collect();
        pixels
    }

    /// Pixels of `rect` (canvas pixels, at `scale`) whose colour differs by
    /// more than `tolerance` in any channel.
    pub(super) fn differing(
        a: &[u32],
        b: &[u32],
        rect: Rect,
        scale: usize,
        tolerance: u8,
    ) -> usize {
        let w = texture_width(scale);
        let mut n = 0;
        for y in rect.y * scale..(rect.y + rect.h) * scale {
            for x in rect.x * scale..(rect.x + rect.w) * scale {
                let i = y * w + x;
                let (pa, pb) = (a[i].to_le_bytes(), b[i].to_le_bytes());
                if (0..3).any(|c| pa[c].abs_diff(pb[c]) > tolerance) {
                    n += 1;
                }
            }
        }
        n
    }

    fn panel_of(ui: &UiState) -> Rect {
        ui::panel_bounds(ui.panel.as_ref().unwrap())
    }

    fn save_pair(name: &str, theirs: &[u32], ours: &[u32], scale: usize) {
        std::fs::create_dir_all("target/egui-launcher").unwrap();
        let (w, h) = (texture_width(scale) as u32, texture_height(scale) as u32);
        for (suffix, pixels) in [("texture", theirs), ("egui", ours)] {
            let path = format!("target/egui-launcher/{name}-{suffix}.png");
            crate::screenshot::save(std::path::Path::new(&path), pixels, w, h).unwrap();
        }
    }

    /// Every launcher page on every machine of the matrix, its dialogs, and
    /// each kind of control lit by the pointer and by the focus, drawn by
    /// egui and by the texture from the same drawing function: compared
    /// pixel for pixel over the panel at 1x, and a spread of them at 2x.
    #[test]
    #[ignore = "renders on the GPU"]
    fn egui_draws_the_launcher_as_the_texture_does() {
        let gpu = super::super::crt_shader::test_gpu("egui_overlay_pages")
            .expect("hardware GPU required");
        let none = egui_view::Light::default();
        let mut cases: Vec<(String, UiState, egui_view::Light, usize)> = Vec::new();
        for (setup_name, setup) in setups() {
            for tab in TABS {
                let mut state = LauncherState::new(setup.clone());
                state.tab = tab;
                cases.push((format!("{setup_name}-{tab:?}"), ui_state(state), none, 1));
            }
        }
        let full = setups().pop().unwrap().1;
        let page = |tab| {
            let mut state = LauncherState::new(full.clone());
            state.tab = tab;
            state
        };
        let mut save = page(LauncherTab::Memory);
        save.save_dialog = true;
        cases.push(("save-dialog".into(), ui_state(save), none, 1));
        let mut confirm = page(LauncherTab::Memory);
        confirm.confirm_reset = true;
        cases.push(("confirm-reset".into(), ui_state(confirm), none, 1));
        let lit = |hover, focus, mix, open| egui_view::Light {
            hover,
            focus,
            mix,
            open,
            elsewhere: false,
        };
        let chip = UiControl::LauncherCycle {
            field: F::ChipRam,
            forward: false,
        };
        for (name, tab, light) in [
            (
                "hover-run",
                LauncherTab::Memory,
                lit(Some(UiControl::LauncherRun), None, 1.0, false),
            ),
            (
                "hover-arrow",
                LauncherTab::Memory,
                lit(Some(chip), None, 1.0, false),
            ),
            (
                "focus-tab",
                LauncherTab::Memory,
                lit(
                    None,
                    Some(UiControl::LauncherTab(LauncherTab::Cpu)),
                    1.0,
                    false,
                ),
            ),
            (
                "focus-chosen-tab",
                LauncherTab::Memory,
                lit(
                    None,
                    Some(UiControl::LauncherTab(LauncherTab::Memory)),
                    0.45,
                    false,
                ),
            ),
            (
                "focus-model",
                LauncherTab::Memory,
                lit(
                    None,
                    Some(UiControl::LauncherModel(MachineModel::A500)),
                    0.7,
                    false,
                ),
            ),
            (
                "focus-stepper",
                LauncherTab::Memory,
                lit(None, Some(chip), 0.45, false),
            ),
            (
                "open-stepper",
                LauncherTab::Memory,
                lit(None, Some(chip), 1.0, true),
            ),
            (
                "focus-value-box",
                LauncherTab::Memory,
                lit(None, Some(UiControl::LauncherRamPatternEdit), 0.6, false),
            ),
            (
                "focus-close",
                LauncherTab::Memory,
                lit(None, Some(UiControl::PanelClose), 1.0, false),
            ),
            (
                "focus-browse",
                LauncherTab::Rom,
                lit(None, Some(UiControl::LauncherBrowse(F::Rom)), 0.8, false),
            ),
            (
                "focus-toggle",
                LauncherTab::Floppy,
                lit(
                    None,
                    Some(UiControl::LauncherToggle(F::Df0WriteProtect)),
                    0.8,
                    false,
                ),
            ),
            (
                "focus-boot",
                LauncherTab::BootPriority,
                lit(
                    None,
                    Some(UiControl::LauncherDriveBootToggle(F::IdeMasterBoot)),
                    0.8,
                    false,
                ),
            ),
            (
                "focus-nav-tab",
                LauncherTab::AvVideo,
                lit(
                    None,
                    Some(UiControl::LauncherNavTab(LauncherTab::AvDisplay)),
                    1.0,
                    false,
                ),
            ),
        ] {
            cases.push((name.into(), ui_state(page(tab)), light, 1));
        }
        for tab in [
            LauncherTab::Memory,
            LauncherTab::Floppy,
            LauncherTab::Storage,
            LauncherTab::BootPriority,
        ] {
            cases.push((format!("2x-{tab:?}"), ui_state(page(tab)), none, 2));
        }
        cases.push((
            "2x-open-stepper".into(),
            ui_state(page(LauncherTab::Memory)),
            lit(None, Some(chip), 1.0, true),
            2,
        ));
        let mut report = Vec::new();
        for (name, ui, light, scale) in &cases {
            let theirs = in_texture(ui, *light, *scale);
            let panel = ui.panel.as_ref().unwrap();
            let ours = on_egui(&gpu, *scale as f32, |root| {
                let mut typing = egui_view::Typing::default();
                let mut images = egui_view::Images::default();
                egui_view::show_display(root, ui, *light, &mut typing, &mut images);
            });
            let rect = panel_of(ui);
            // A dialog dims the page under it with a translucent scrim,
            // whose alpha egui carries in eight bits where the texture
            // blends with the exact fraction: a level apart at most.
            // Everything else is exact.
            let dimmed = matches!(
                panel,
                Panel::Launcher(state) if state.save_dialog || state.confirm_reset
            );
            let n = differing(&theirs, &ours, rect, *scale, u8::from(dimmed));
            if n != 0 {
                save_pair(name, &theirs, &ours, *scale);
            }
            report.push((name.clone(), n));
        }
        let bad: Vec<_> = report.iter().filter(|(_, n)| *n != 0).collect();
        eprintln!("{} cases, {} differ: {bad:?}", report.len(), bad.len());
        assert!(bad.is_empty(), "egui draws differently: {bad:?}");
    }

    fn menu_fixture(over: Option<LauncherState>) -> UiState {
        use crate::video::menu::{MenuAction, MenuNav, MenuRow, MenuRowKind};
        let row = |label: &str, kind| MenuRow {
            label: label.into(),
            value: None,
            enabled: true,
            kind,
        };
        let child = vec![
            row("Slots", MenuRowKind::Caption),
            MenuRow {
                value: Some("PAL".into()),
                ..row("Video standard", MenuRowKind::Action(MenuAction::Quit))
            },
            row(
                "Scanlines",
                MenuRowKind::Toggle {
                    action: MenuAction::Quit,
                    on: true,
                },
            ),
            row(
                "Smooth",
                MenuRowKind::Choice {
                    action: MenuAction::Quit,
                    selected: true,
                },
            ),
            row(
                "Sharp",
                MenuRowKind::Choice {
                    action: MenuAction::Quit,
                    selected: false,
                },
            ),
            MenuRow {
                enabled: false,
                ..row("Freeze (HRTMon)", MenuRowKind::Action(MenuAction::Quit))
            },
        ];
        let rows = vec![
            row(
                "Machine Configuration...",
                MenuRowKind::Action(MenuAction::Quit),
            ),
            row("Video Settings", MenuRowKind::Submenu(child)),
            row("About...", MenuRowKind::Action(MenuAction::Quit)),
            row("Quit", MenuRowKind::Action(MenuAction::Quit)),
        ];
        let mut nav = MenuNav::default();
        nav.point_at(1);
        nav.descend(&rows);
        nav.point_at(3);
        UiState {
            menu_open: true,
            menu_rows: rows,
            menu_nav: nav,
            panel: over.map(|state| Panel::Launcher(Box::new(state))),
        }
    }

    /// The menu's levels drawn by egui and by the texture from the same
    /// function, at both menu sizes and two scales, and over the
    /// configuration screen, whose own veil egui lays over the panel.
    #[test]
    #[ignore = "renders on the GPU"]
    fn egui_draws_the_menu_as_the_texture_does() {
        let gpu = super::super::crt_shader::test_gpu("egui_menu").expect("hardware GPU required");
        let full = Rect {
            x: 0,
            y: 0,
            w: texture_width(1),
            h: crate::video::present_height(),
        };
        let mut report = Vec::new();
        for menu_scale in [
            crate::config::MenuScale::Normal,
            crate::config::MenuScale::Large,
        ] {
            crate::video::set_menu_scale(menu_scale);
            for scale in [1usize, 2] {
                // On its own: the texture's veil aside, black either side.
                let ui = menu_fixture(None);
                let (w, h) = (texture_width(scale), texture_height(scale));
                let mut frame = vec![0u8; w * h * 4];
                ui::draw_menu_columns(
                    &mut ui::FrameCanvas::new(&mut frame, scale),
                    &ui.menu_rows,
                    &ui.menu_nav,
                );
                let theirs: Vec<u32> = frame
                    .chunks_exact(4)
                    .map(|p| u32::from_le_bytes(p.try_into().unwrap()) | 0xFF00_0000)
                    .collect();
                let ours = on_egui(&gpu, scale as f32, |root| {
                    let mut typing = egui_view::Typing::default();
                    let mut images = egui_view::Images::default();
                    egui_view::show_display(
                        root,
                        &ui,
                        egui_view::Light::default(),
                        &mut typing,
                        &mut images,
                    );
                });
                let name = format!("menu-{menu_scale:?}-{scale}x");
                let n = differing(&theirs, &ours, full, scale, 0);
                if n != 0 {
                    save_pair(&name, &theirs, &ours, scale);
                }
                report.push((name, n));
                // Over the configuration screen: the texture veils the panel
                // it drew; egui veils the panel it drew, in its own layer.
                let mut state = LauncherState::new(MachineSetup::default());
                state.tab = LauncherTab::Memory;
                let ui = menu_fixture(Some(state));
                let theirs = in_texture(&ui, egui_view::Light::default(), scale);
                let ours = on_egui(&gpu, scale as f32, |root| {
                    let mut typing = egui_view::Typing::default();
                    let mut images = egui_view::Images::default();
                    egui_view::show_display(
                        root,
                        &ui,
                        egui_view::Light::default(),
                        &mut typing,
                        &mut images,
                    );
                });
                let name = format!("menu-over-launcher-{menu_scale:?}-{scale}x");
                let n = differing(&theirs, &ours, panel_of(&ui), scale, 1);
                if n != 0 {
                    save_pair(&name, &theirs, &ours, scale);
                }
                report.push((name, n));
            }
        }
        crate::video::set_menu_scale(crate::config::MenuScale::Normal);
        let bad: Vec<_> = report.iter().filter(|(_, n)| *n != 0).collect();
        eprintln!("{} cases, {} differ: {bad:?}", report.len(), bad.len());
        assert!(bad.is_empty(), "egui draws the menu differently: {bad:?}");
    }

    /// The status bar drawn by egui and by the texture from the same
    /// function, at the two whole scales the texture draws it at, in every
    /// state the bar tells apart and with each control lit by the pointer
    /// and by the focus. The glyphs' anti-aliased edges are built of
    /// translucent fills, stacked where strokes cross (the power ring's
    /// thirty-two segments overlap each other), each with an alpha egui
    /// carries in eight bits: three levels apart at most.
    #[test]
    #[ignore = "renders on the GPU"]
    fn egui_draws_the_status_bar_as_the_texture_does() {
        use crate::bus::FrontPanelStatus;
        use crate::config::JoystickInputMode;
        let gpu = super::super::crt_shader::test_gpu("egui_bar").expect("hardware GPU required");
        let drive = |inserted, multi, bridged| DriveBar {
            connected: true,
            inserted,
            multi,
            bridged,
        };
        let none = DriveBar::default();
        let medias = [
            MediaBar {
                drives: [drive(true, false, false), none, none, none],
                cd: None,
            },
            MediaBar {
                drives: [
                    drive(true, true, false),
                    drive(false, false, true),
                    none,
                    none,
                ],
                cd: None,
            },
            MediaBar {
                drives: [
                    drive(true, true, false),
                    drive(true, false, false),
                    drive(false, false, false),
                    drive(true, false, true),
                ],
                cd: Some(true),
            },
        ];
        let busy = FrontPanelStatus {
            power_led_bright: false,
            fdd_led_on: true,
            fdd_track: Some(79),
            hdd_led: Some(true),
            cd_led: Some(true),
            cd_track: Some(12),
            output_volume_percent: 35,
        };
        let mut views = Vec::new();
        for (m, media) in medias.iter().enumerate() {
            for (status, powered_on, paused) in [
                (FrontPanelStatus::default(), true, false),
                (busy, false, false),
                (busy, true, true),
            ] {
                views.push((
                    format!("bar-{m}-{powered_on}-{paused}"),
                    StatusBarView {
                        status,
                        powered_on,
                        paused,
                        media: *media,
                        joystick_input_mode: if paused {
                            JoystickInputMode::Keyboard
                        } else {
                            JoystickInputMode::Gamepad
                        },
                        keyboard_panel_shown: paused,
                        hover: None,
                        control_connected: !powered_on,
                    },
                    None,
                ));
            }
        }
        for control in [
            BarControl::Power,
            BarControl::Pause,
            BarControl::Reboot,
            BarControl::Screenshot,
            BarControl::Menu,
            BarControl::Joystick,
            BarControl::Keyboard,
            BarControl::Volume,
            BarControl::DriveLoad(0),
            BarControl::DriveSwap(0),
            BarControl::DriveEject(0),
            BarControl::CdLoad,
            BarControl::CdEject,
        ] {
            for (how, hover, focus) in [
                ("hover", Some(control), None),
                ("focus", None, Some(control)),
            ] {
                views.push((
                    format!("bar-{how}-{control:?}"),
                    StatusBarView {
                        status: busy,
                        powered_on: true,
                        paused: false,
                        media: medias[2],
                        joystick_input_mode: JoystickInputMode::Gamepad,
                        keyboard_panel_shown: false,
                        hover,
                        control_connected: false,
                    },
                    focus,
                ));
            }
        }
        let bar = status_bar_rect();
        let mut report = Vec::new();
        for scale in [1usize, 2] {
            for (name, view, focus) in &views {
                let (w, h) = (texture_width(scale), texture_height(scale));
                let mut frame = vec![0u8; w * h * 4];
                statusbar::set_nav_light(*focus, 0.7, false);
                draw_status_bar(&mut ui::FrameCanvas::new(&mut frame, scale), view);
                statusbar::set_nav_light(None, 0.0, false);
                let theirs: Vec<u32> = frame
                    .chunks_exact(4)
                    .map(|p| u32::from_le_bytes(p.try_into().unwrap()))
                    .collect();
                let ours = on_egui(&gpu, scale as f32, |root| {
                    statusbar::set_nav_light(*focus, 0.7, false);
                    let mut typing = egui_view::Typing::default();
                    let mut images = egui_view::Images::default();
                    let mut canvas = egui_view::EguiCanvas::new(root, &mut typing, &mut images);
                    draw_status_bar(&mut canvas, view);
                    statusbar::set_nav_light(None, 0.0, false);
                });
                let name = format!("{name}-{scale}x");
                let n = differing(&theirs, &ours, bar, scale, 3);
                if n != 0 {
                    save_pair(&name, &theirs, &ours, scale);
                }
                report.push((name, n));
            }
        }
        let bad: Vec<_> = report.iter().filter(|(_, n)| *n != 0).collect();
        eprintln!("{} cases, {} differ: {bad:?}", report.len(), bad.len());
        assert!(bad.is_empty(), "egui draws the bar differently: {bad:?}");
    }
}
