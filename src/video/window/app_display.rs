// SPDX-License-Identifier: GPL-3.0-or-later

//! Presentation and display: surface sizing, canvas following, bezel, overlays, shaders, render pipeline.

use super::*;

impl App {
    /// Re-plan the presentation after the canvas height changed, and resize
    /// every buffer that indexes by it. False when the texture could not be
    /// resized, in which case the caller has to put its flag back: the draw
    /// helpers size themselves from the flag, so a taller canvas over a
    /// shorter buffer would index past it.
    ///
    /// Shared by every strip that takes a slice of the canvas -- the MT-32
    /// panel and the on-screen keyboard -- since what has to follow the
    /// change is the same in each case.
    pub(super) fn resync_canvas_height(&mut self) -> bool {
        if let Some(r) = self.render.as_mut() {
            let surface = r.window.inner_size();
            if let Err(e) = sync_main_present_scaling(r, (surface.width, surface.height)) {
                warn!("resize texture buffer for a canvas-height change failed: {e}");
                return false;
            }
        }
        // Tool windows draw through the same canvas height, so their buffers
        // follow too. Buffer only: their own window sizes are their business.
        for kind in ToolPanelKind::ALL {
            if let Some(tool) = self.tool_window_mut(kind) {
                if let Err(e) = tool.pixels.resize_buffer(
                    texture_width(tool.texture_scale) as u32,
                    texture_height(tool.texture_scale) as u32,
                ) {
                    warn!("resize tool texture buffer for a canvas-height change failed: {e}");
                }
                tool.window.request_redraw();
            }
        }
        true
    }

    /// Resize the presentation surface to a new window size. Shared by the
    /// Resized event and by the synchronous path of request_inner_size (see
    /// snap_window_to_canvas), which on some backends returns the applied
    /// size instead of delivering an event.
    pub(super) fn apply_surface_size(&mut self, size: PhysicalSize<u32>) {
        if let Some(r) = self.render.as_mut() {
            // A zero-sized resize is minimization (Windows reports the
            // minimized client area as 0x0). Leave the surface untouched and
            // stop rendering until the restore delivers a nonzero size (see
            // Render::minimized).
            r.minimized = size.width == 0 || size.height == 0;
            if r.minimized {
                return;
            }
            // The integer fit (and with it the supersample factor) follows
            // the surface size, so re-plan for the new one; the surface
            // resize below is what recomputes the scaling matrix and clip
            // rect from it.
            if let Err(e) = sync_main_present_scaling(r, (size.width, size.height)) {
                warn!("resize texture buffer for new surface size failed: {e}");
            }
            if let Err(e) = r.resize_surface(size) {
                warn!("resize surface failed: {e}");
            }
        }
        // Resizing the surface discards its contents, leaving it blank (white)
        // until the next present. When the machine is powered off (or paused)
        // the event loop is in Wait mode and produces no frames, so without an
        // explicit repaint here the window can sit white after the
        // scale-factor/resize event that macOS delivers right after window
        // creation.
        self.request_redraw();
    }

    /// Bring the surface up to the host window's current size before drawing,
    /// when a resize has not reached us as a Resized event yet.
    ///
    /// `pixels` reconfigures its swapchain from the size the last
    /// `resize_surface` gave it, and its render retries the acquire in an
    /// unbounded loop: on a driver that rejects a swapchain whose extent
    /// disagrees with the window (Mesa's X11 Vulkan WSI returns
    /// VK_ERROR_OUT_OF_DATE_KHR), a stale size makes that loop rebuild the
    /// swapchain forever instead of hanging on to a wrongly-scaled frame.
    /// Rendering runs inside the event callback, so the loop also starves the
    /// Resized event that would have corrected the size: the window never
    /// comes back, and the churn goes on until the display server runs the
    /// client out of resource ids. Entering or leaving fullscreen is the
    /// common way in, the window manager resizing the window a moment before
    /// the event reaches us (issue #362, upstream parasyte/pixels#460).
    pub(super) fn resync_surface_size(&mut self) {
        let Some(r) = self.render.as_ref() else {
            return;
        };
        let Some(size) = surface_resize_for_draw(r.surface_size, r.window.inner_size()) else {
            return;
        };
        self.apply_surface_size(size);
    }

    /// Tool-window counterpart of `resync_surface_size`, for the same reason:
    /// these windows are freely resizable too.
    pub(super) fn resync_tool_surface_size(&mut self, kind: ToolPanelKind) {
        let Some(tool) = self.tool_window(kind) else {
            return;
        };
        let Some(size) = surface_resize_for_draw(tool.surface_size, tool.window.inner_size())
        else {
            return;
        };
        self.apply_tool_surface_size(kind, size);
    }

    /// Tool-window counterpart of `apply_surface_size`, shared by that window's
    /// Resized event and the synchronous `request_inner_size` path.
    pub(super) fn apply_tool_surface_size(&mut self, kind: ToolPanelKind, size: PhysicalSize<u32>) {
        if let Some(tool) = self.tool_window_mut(kind) {
            // Same minimized-present deadlock guard as the main window.
            tool.minimized = size.width == 0 || size.height == 0;
            if tool.minimized {
                return;
            }
            let _ = tool.resize_surface(size);
        }
        self.request_redraw();
    }

    /// Size the window to the presentation canvas, unless it is fullscreen: the
    /// request resizes nothing there and instead shrinks the drawable into a
    /// corner (macOS and Windows; Linux window managers ignore it), so leave the
    /// display-sized surface alone and let the presentation scale into it.
    ///
    /// Only the two things that change the canvas height -- the pixel aspect
    /// and the status bar -- call this, and only for a window still at the old
    /// canvas size. Nothing else may take a window the user has sized.
    ///
    /// `request_inner_size` is only asynchronous when it returns `None`. Wayland
    /// applies the resize client-side and returns the new size with no `Resized`
    /// event to follow, so the surface must be resized here or the stale extent
    /// misplaces every click through `cursor_texture_position`.
    pub(super) fn snap_window_to_canvas(&mut self) {
        let Some(window) = self.render.as_ref().map(|r| r.window.clone()) else {
            return;
        };
        if window.fullscreen().is_some() {
            return;
        }
        let size = LogicalSize::new(FB_WIDTH as f64, window_present_height() as f64);
        if let Some(applied) = window.request_inner_size(size) {
            self.apply_surface_size(applied);
            // This backend applied the request synchronously, so no Resized
            // event remains to consume it.
            self.snap_request_deadline = None;
            self.window_manually_sized = false;
        } else {
            self.snap_request_deadline = Some(Instant::now() + CANVAS_SNAP_RESPONSE_TIMEOUT);
        }
    }

    /// Follow a canvas-height change with the window, or arrange to when
    /// fullscreen gives the window back.
    ///
    /// `was_canvas_sized` is the verdict taken before the change and
    /// `canvas_before` the canvas height it was taken at. Three cases:
    ///
    /// - The window was the canvas's: put it on the new canvas size.
    /// - Fullscreen is holding it: nothing can be resized now, and on the
    ///   way out the window returns at the size the *old* canvas gave it.
    ///   Remember what is owed and take it when the window comes back.
    /// - The window is the user's own size: their size is not ours to
    ///   take, but the strip the canvas just gained or lost is theirs to
    ///   gain or lose with it. Move the height by exactly that and leave
    ///   the width alone. Doing nothing here -- as this used to -- leaves
    ///   the canvas a different shape inside an unchanged window, and the
    ///   picture letterboxes on whichever axis has come up short.
    pub(super) fn follow_canvas_change(&mut self, was_canvas_sized: bool, canvas_before: usize) {
        if was_canvas_sized {
            self.snap_window_to_canvas();
            return;
        }
        let delta = window_present_height() as i32 - canvas_before as i32;
        let fullscreen = self
            .render
            .as_ref()
            .is_some_and(|r| r.window.fullscreen().is_some());
        if fullscreen {
            // A second change while still fullscreen adds to the first.
            self.pending_canvas_follow = Some(if self.window_manually_sized {
                let owed = match self.pending_canvas_follow {
                    Some(CanvasFollow::Nudge(d)) => d,
                    _ => 0,
                };
                CanvasFollow::Nudge(owed + delta)
            } else {
                CanvasFollow::Snap
            });
            return;
        }
        self.nudge_window_height(delta);
    }

    /// Move the main window's height by `delta` logical pixels, keeping the
    /// width. Unlike a snap this leaves the window the user's -- it is
    /// still their size, less or plus the strip the canvas changed by -- so
    /// the resize it provokes is classified as any other.
    pub(super) fn nudge_window_height(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let Some(window) = self.render.as_ref().map(|r| r.window.clone()) else {
            return;
        };
        if window.fullscreen().is_some() {
            return;
        }
        let scale = window.scale_factor();
        let size = window.inner_size();
        let logical_w = f64::from(size.width) / scale;
        let logical_h = f64::from(size.height) / scale;
        let want = (logical_h + f64::from(delta)).max(1.0);
        if let Some(applied) = window.request_inner_size(LogicalSize::new(logical_w, want)) {
            // Applied client-side, with no Resized event to follow.
            self.apply_surface_size(applied);
        }
    }

    /// Classify a resize of the main window: the user's own drag, or the
    /// window following a canvas change.
    ///
    /// A snap asks for the canvas size; what comes back may be clamped by
    /// the platform or rounded by the scale factor, and that near miss must
    /// not read as a drag or the window stops following the canvas for the
    /// rest of the run. A drag onto the canvas size hands it back.
    pub(super) fn note_window_resize(&mut self, size: PhysicalSize<u32>) {
        // Read what is needed and let the borrow go: a drag delivers these
        // continuously, so this takes nothing it has to hold on to.
        let Some((fullscreen, scale)) = self
            .render
            .as_ref()
            .map(|r| (r.window.fullscreen().is_some(), r.window.scale_factor()))
        else {
            return;
        };
        // Fullscreen sizes the window itself; leave the standing verdict.
        if fullscreen {
            return;
        }
        // The window is back from fullscreen with a canvas change owing:
        // this size is the old canvas's, not a drag. Settle up instead of
        // classifying it, or the stale size is what gets remembered.
        if let Some(follow) = self.pending_canvas_follow.take() {
            match follow {
                CanvasFollow::Snap => self.snap_window_to_canvas(),
                CanvasFollow::Nudge(delta) => self.nudge_window_height(delta),
            }
            return;
        }
        let logical_w = f64::from(size.width) / scale;
        let logical_h = f64::from(size.height) / scale;
        if resize_is_canvas_owned(
            &mut self.snap_request_deadline,
            Instant::now(),
            logical_w,
            logical_h,
            window_present_height(),
        ) {
            self.window_manually_sized = false;
            return;
        }
        self.window_manually_sized = true;
    }

    /// Whether the main window still belongs to the canvas rather than to the
    /// user -- i.e. it has not been manually resized (fullscreen counts as
    /// resized). Lets a canvas change snap an untouched window to the new
    /// size while leaving a resized one alone.
    pub(super) fn window_is_canvas_sized(&self) -> bool {
        let Some(window) = self.render.as_ref().map(|r| r.window.clone()) else {
            return false;
        };
        if window.fullscreen().is_some() {
            return false;
        }
        !self.window_manually_sized
    }

    /// Cmd/Alt+M: turn the monitor bezel off, or back on to whichever
    /// front was last chosen. Picking a style is the menu's job; this is
    /// the on-off for the one already picked, so it never changes which.
    pub(super) fn toggle_bezel(&mut self) {
        let style = if self.bezel.is_on() {
            BezelStyle::None
        } else {
            self.bezel_last
        };
        self.set_bezel(style);
    }

    /// Draw a given monitor front for the rest of the run (the config file
    /// default is unchanged; set `[display] bezel` to make it stick).
    pub(super) fn set_bezel(&mut self, style: BezelStyle) {
        self.bezel = style;
        if style.is_on() {
            self.bezel_last = style;
        }
        info!("monitor bezel: {}", style.label());
        self.show_osd(format!("Monitor bezel: {}", style.menu_label()));
        self.request_redraw();
    }

    /// Cmd/Alt+P: toggle the performance overlay for the rest of the run
    /// (the config file default is unchanged; set `[display] perf_overlay`
    /// to make it stick).
    pub(super) fn toggle_perf_overlay(&mut self) {
        self.perf_overlay = !self.perf_overlay;
        self.perf = PerfOverlay::default();
        let label = if self.perf_overlay { "on" } else { "off" };
        info!("performance overlay: {label}");
        self.show_osd(format!("Performance overlay: {label}"));
        self.request_redraw();
    }

    /// Resample the performance overlay counters and reformat its lines
    /// when the interval has elapsed. A run-state flip (pause, power-off,
    /// halt) publishes the idle readout immediately and re-baselines, so
    /// rates are never computed across the boundary.
    pub(super) fn update_perf_overlay(&mut self, running: bool) {
        if !self.perf_overlay {
            return;
        }
        let now = Instant::now();
        if let Some(base) = &self.perf.baseline {
            if base.running == running && now.duration_since(base.at) < PERF_SAMPLE_INTERVAL {
                return;
            }
        }
        let audio = self.emu.bus().live_audio_status();
        let counters = self.emu.perf_counters();
        let current = PerfBaseline {
            at: now,
            running,
            emulated_frames: self.emu.bus().emulated_frames(),
            emulated_seconds: self.emu.bus().emulated_seconds(),
            busy: counters.busy,
            audio_underrun_frames: audio.callback_underrun_frames,
        };
        let audio_lead_ms = audio.output_lead_seconds * 1000.0;
        let readout = match &self.perf.baseline {
            Some(base) if base.running == running && running => {
                perf_readout(base, &current, audio_lead_ms, counters.pacer_slips)
            }
            // First sample after enabling, a run-state flip, or an idle
            // machine: rates are zero by definition, only the levels show.
            _ => PerfReadout {
                audio_lead_ms,
                pacer_slips: counters.pacer_slips,
                ..Default::default()
            },
        };
        self.perf.baseline = Some(current);
        let lines = perf_overlay_lines(&readout);
        if lines != self.perf.lines {
            self.perf.lines = lines;
            self.perf.revision = self.perf.revision.wrapping_add(1);
        }
    }

    /// Install a screen tint and its presentation table together.
    pub(super) fn set_tint(&mut self, tint: crate::config::Tint) {
        self.tint = tint;
        self.tint_lut = tint_lut(tint);
    }

    /// Load the configured sticker folder into the decal pass, or clear it
    /// when none is configured. The full message goes to the log; the
    /// returned one-line summary is for the caller's overlay, exactly as
    /// [`Self::reload_custom_shader`] reports. A failure leaves no sheet,
    /// so the front falls back to bare plastic rather than a stale set.
    pub(super) fn reload_bezel_stickers(&mut self) -> Result<(), String> {
        let path = self.bezel_stickers_path.clone();
        let Some(r) = self.render.as_mut() else {
            return Ok(());
        };
        match path {
            None => {
                r.sticker_pass.set_sheet(None);
                Ok(())
            }
            Some(dir) => match stickers::load_sheet(&dir) {
                Ok(sheet) => {
                    r.sticker_pass.set_sheet(Some(sheet));
                    Ok(())
                }
                Err(msg) => {
                    r.sticker_pass.set_sheet(None);
                    error!("[display] bezel_stickers: {msg}");
                    Err(msg.lines().next().unwrap_or_default().to_string())
                }
            },
        }
    }

    /// Compile the configured user shader against the live device. The full
    /// message goes to the log; the returned one-line summary is for the
    /// caller to fold into whatever overlay it is already showing. A failure
    /// leaves no pipeline, so the caller falls back to no shader rather than
    /// to a stale one.
    pub(super) fn reload_custom_shader(&mut self) -> Result<(), String> {
        let fail = |msg: String| {
            error!("[display] shader: {msg}");
            Err(msg.lines().next().unwrap_or_default().to_string())
        };
        let Some(path) = self.custom_shader_path.clone() else {
            return fail("no custom shader configured".to_string());
        };
        let Some(r) = self.render.as_mut() else {
            return fail(format!(
                "cannot load shader {} before the window exists",
                path.display()
            ));
        };
        let format = r.pixels.render_texture_format();
        match r.crt_shader.load_custom(r.pixels.device(), format, &path) {
            Ok(()) => Ok(()),
            Err(msg) => fail(msg),
        }
    }

    /// Switch the presentation pixel aspect live: the canvas height (and
    /// with it the backing texture and the window) changes between the
    /// 4:3 and the square-pixel size, so the texture must be rebuilt like
    /// a DPI change (see resync_render_scale) and the window re-sized.
    pub(super) fn apply_pixel_aspect(&mut self, aspect: PixelAspect) {
        if aspect == crate::video::pixel_aspect() {
            return;
        }
        // A video recording's frame size is fixed when the encoder is
        // created; refuse to change the presentation under it.
        if self.recorder.is_some() {
            self.show_osd("Stop the video recording before changing pixel aspect");
            return;
        }
        // Decide before the change (it feeds window_present_height) whether the
        // window is still canvas-sized, so a manual resize survives, and
        // note the height that verdict is measured against.
        let was_canvas_sized = self.window_is_canvas_sized();
        let canvas_before = window_present_height();
        crate::video::set_pixel_aspect(aspect);
        if let Some(r) = self.render.as_mut() {
            // The canvas height changes with the aspect, so re-plan: the
            // integer fit (and its supersample factor) is re-decided for the
            // new canvas, and the texture resized to it.
            let surface = r.window.inner_size();
            if let Err(e) = sync_main_present_scaling(r, (surface.width, surface.height)) {
                warn!("resize texture buffer for pixel aspect failed: {e}");
            }
        }
        // Tool windows share the canvas-sized texture layout (panel
        // centring reads the live canvas height), so their buffers and
        // windows must follow the new size too.
        let size = LogicalSize::new(FB_WIDTH as f64, window_present_height() as f64);
        for kind in ToolPanelKind::ALL {
            let mut applied = None;
            if let Some(tool) = self.tool_window_mut(kind) {
                if let Err(e) = tool.pixels.resize_buffer(
                    texture_width(tool.texture_scale) as u32,
                    texture_height(tool.texture_scale) as u32,
                ) {
                    warn!("resize tool texture buffer for pixel aspect failed: {e}");
                }
                applied = tool.window.request_inner_size(size);
            }
            // Synchronous on Wayland, with no Resized event to follow.
            if let Some(applied) = applied {
                self.apply_tool_surface_size(kind, applied);
            }
        }
        self.follow_canvas_change(was_canvas_sized, canvas_before);
        self.request_redraw();
    }

    /// Switch how the presentation canvas is scaled into the window live.
    ///
    /// The canvas itself never changes -- integer mode may re-render it at a
    /// different supersample factor, but its pixel content, the window size
    /// and a video recording (whose frames are the 1x canvas, averaged down
    /// like any supersample) all carry on -- so unlike a pixel-aspect switch
    /// there is no recording to refuse and no window to re-size.
    pub(super) fn apply_display_scaling(&mut self, scaling: DisplayScaling) {
        if scaling == crate::video::display_scaling() {
            return;
        }
        crate::video::set_display_scaling(scaling);
        if let Some(r) = self.render.as_mut() {
            // A minimized window has no surface to re-plan against; the
            // Resized event that restores it re-plans itself.
            if !r.minimized {
                let size = r.window.inner_size();
                if let Err(e) = sync_main_present_scaling(r, (size.width, size.height)) {
                    warn!("resize texture buffer for display scaling failed: {e}");
                }
                // The sync only stores the mode (and resizes the texture), so
                // re-apply the current surface size: that is what recomputes
                // the scaling matrix and the clip rect the cursor mapping,
                // the shader passes and the RTG pass all read.
                if let Err(e) = r.resize_surface(size) {
                    warn!("resize surface for display scaling failed: {e}");
                }
            }
        }
        self.show_osd(format!("Scaling: {}", scaling.label()));
        self.request_redraw();
    }

    /// The rect the autocrop presentation shows of the display region,
    /// in canvas pixels -- or `None` whenever the classic whole-canvas
    /// layout must present instead: autocrop off, or a frame the mode
    /// does not apply to (the bezel and the CRT presets frame the whole
    /// glass, so the tube look wins while one is on; RTG board frames
    /// and programmable scans present their own geometry).
    ///
    /// While the mode is on, the layout never falls back to the classic
    /// letterbox: an open menu or panel, or a session with no content
    /// yet, widens the rect to the full display region instead. The
    /// overlays draw into the display region against the full canvas
    /// mapping, so this keeps them entirely visible -- and it keeps the
    /// status-bar band pinned to the window bottom, rather than hopping
    /// between the bottom-anchored band and the letterbox's centred bar
    /// every time a menu opens.
    pub(super) fn autocrop_canvas_src(&self) -> Option<(usize, usize, usize, usize)> {
        if !crate::video::autocrop()
            || self.rtg_present_dims.is_some()
            || self.present_programmable
            || self.present_width != FB_WIDTH
            || self.bezel.is_on()
            || self.crt_shader_kind != crate::config::ShaderKind::None
        {
            return None;
        }
        let full = (0, 0, FB_WIDTH, present_height());
        if self.ui.active() {
            return Some(full);
        }
        let Some(content) = self.present_content_rect else {
            return Some(full);
        };
        Some(
            canvas_content_rect(
                content,
                self.present_rows,
                self.overscan,
                self.tv_centre,
                self.present_tv_aperture_rows,
                present_height(),
            )
            .unwrap_or(full),
        )
    }

    /// Switch the autocrop presentation live. Purely a scaler-pass
    /// input -- the canvas, texture and window are untouched -- so
    /// nothing needs rebuilding; the next redraw draws the other layout.
    pub(super) fn apply_autocrop(&mut self, autocrop: bool) {
        if autocrop == crate::video::autocrop() {
            return;
        }
        crate::video::set_autocrop(autocrop);
        // Turning it on under a bezel or CRT preset changes nothing on
        // screen (the tube look owns the glass and the crop suspends
        // itself); say so, or the toggle looks broken.
        let suspended =
            self.bezel.is_on() || self.crt_shader_kind != crate::config::ShaderKind::None;
        self.show_osd(match (autocrop, suspended) {
            (true, true) => "Autocrop: on (suspended while a bezel or CRT shader is on)",
            (true, false) => "Autocrop: on",
            (false, _) => "Autocrop: off",
        });
        self.main_presentation_dirty = true;
        self.request_redraw();
    }

    /// Nudge the TV-presentation centring (Video Settings -> Screen
    /// Centring), the front-panel H-CENTER/V-CENTER knobs of a real
    /// monitor. A live presentation change like the bezel toggle: captures
    /// follow it, the configured start-up value is untouched.
    pub(super) fn step_tv_centre(&mut self, dh: i32, dv: i32) {
        use crate::config::{TV_H_CENTRE_RANGE, TV_V_CENTRE_RANGE};
        let centre = &mut self.tv_centre;
        centre.h = (centre.h + dh).clamp(-TV_H_CENTRE_RANGE, TV_H_CENTRE_RANGE);
        centre.v = (centre.v + dv).clamp(-TV_V_CENTRE_RANGE, TV_V_CENTRE_RANGE);
        let centre = *centre;
        self.show_osd(format!("Centring: H {:+}, V {:+}", centre.h, centre.v));
        self.main_presentation_dirty = true;
        self.request_redraw();
    }

    /// Show or hide the status bar. An untouched window resizes to gain or lose
    /// the bar's strip; a window the user has manually resized keeps its size
    /// (and fullscreen keeps its size too), with the display reflowing to fit --
    /// the presentation already letterboxes any window shape. Only the display
    /// is recorded, so a recording is unaffected. Bound to the shortcut and menu.
    pub(super) fn toggle_status_bar(&mut self) {
        // Decide before the flag flips (it feeds window_present_height) whether
        // the window is still canvas-sized, so a manual resize survives.
        let was_canvas_sized = self.window_is_canvas_sized();
        let canvas_before = window_present_height();
        let hidden = !crate::video::status_bar_hidden();
        crate::video::set_status_bar_hidden(hidden);
        if let Some(r) = self.render.as_mut() {
            // The canvas gains or loses the bar's strip, so re-plan: the
            // integer fit is re-decided for the new canvas height and the
            // texture resized to it (see apply_pixel_aspect).
            let surface = r.window.inner_size();
            if let Err(e) = sync_main_present_scaling(r, (surface.width, surface.height)) {
                // The draw helpers size themselves from the hidden flag, so a
                // failed resize must not commit the toggle: a taller canvas over
                // an unchanged, shorter buffer would index past it. Revert and
                // leave the flag and buffer consistent.
                warn!("resize texture buffer for status bar toggle failed: {e}");
                crate::video::set_status_bar_hidden(!hidden);
                // The plan above was made for a canvas that never
                // materialised; re-plan for the one the flag went back to.
                let _ = sync_main_present_scaling(r, (surface.width, surface.height));
                return;
            }
        }
        // Every tool window (Debugger, Frame Analyzer, Console) draws through
        // draw_panel_layer, which indexes its buffer by the same canvas height
        // (window_present_height), so resize all their buffers to match too, or
        // a later tool draw could index past a now-too-small buffer. Buffer
        // only: unlike a pixel-aspect switch, leave a tool window's own size
        // alone.
        for kind in ToolPanelKind::ALL {
            if let Some(tool) = self.tool_window_mut(kind) {
                if let Err(e) = tool.pixels.resize_buffer(
                    texture_width(tool.texture_scale) as u32,
                    texture_height(tool.texture_scale) as u32,
                ) {
                    warn!("resize tool texture buffer for status bar toggle failed: {e}");
                }
                tool.window.request_redraw();
            }
        }
        // An unresized window goes on the new canvas size; a resized one
        // keeps the width the user chose and moves by the bar's height.
        self.follow_canvas_change(was_canvas_sized, canvas_before);
        self.request_redraw();
        if hidden {
            self.show_osd(format!(
                "Status bar hidden ({HOST_SHORTCUT_MODIFIER_LABEL}+Shift+F restores)"
            ));
        } else {
            self.show_osd("Status bar restored");
        }
    }

    pub(super) fn request_main_redraw(&self) {
        if let Some(render) = self.render.as_ref() {
            if !render.minimized {
                render.window.request_redraw();
            }
        }
    }

    pub(super) fn request_redraw(&self) {
        self.request_main_redraw();
        for kind in ToolPanelKind::ALL {
            if let Some(tool) = self.tool_window(kind) {
                if !tool.minimized {
                    tool.window.request_redraw();
                }
            }
        }
    }

    pub(super) fn refresh_present_from_deinterlacer(&mut self) {
        let rows = self.deinterlacer.output_rows();
        let width = self.deinterlacer.output_width();
        let active = rows * width;
        self.present_fb.resize(active, 0);
        self.present_fb
            .copy_from_slice(&self.deinterlacer.output()[..active]);
        self.present_rows = rows;
        self.present_width = width;
    }

    pub(super) fn reset_render_pipeline(&mut self) {
        self.render_generation = self.render_generation.wrapping_add(1);
        self.last_rendered_emulated_frame = None;
        self.last_submitted_render_frame = None;
        self.presentation_latch.reset();
        self.autocrop_latch.reset();
        self.present_content_rect = None;
        self.last_main_redraw_state = None;
        self.main_presentation_dirty = true;
        let _ = self.collect_threaded_render_results(false);
    }

    pub(super) fn apply_threaded_render_result(&mut self, result: RenderWorkerResult) -> bool {
        // Only one job is in flight at a time, so the returned snapshot is
        // always the freshest one to recycle.
        let mut input = result.input;
        input.release_shared_frame_data();
        self.render_recycle_input = Some(input);
        if result.generation != self.render_generation {
            if self.render_recycle_fb.is_empty() {
                self.render_recycle_fb = result.presentation_fb;
            }
            return false;
        }

        // Advance the autocrop smoothing on every rendered frame, reused
        // ones included: a static screen is exactly what lets a smaller
        // envelope prove itself stable. A change to the *presented* crop
        // must repaint even when the pixels themselves are unchanged
        // (the shrink adoption fires on the Nth identical frame).
        let smoothed = self.autocrop_latch.resolve(result.content_rect);
        if smoothed != self.present_content_rect {
            self.present_content_rect = smoothed;
            self.main_presentation_dirty = true;
        }

        if result.reused_previous {
            self.render_recycle_fb = result.presentation_fb;
            self.last_rendered_emulated_frame = Some(result.emulated_frame);
            return true;
        }

        self.emu.bus_mut().record_video_render_frame(result.timing);
        let next_tv_aperture_rows = self
            .presentation_latch
            .resolve_tv_aperture(result.tv_aperture);
        let unchanged = self.rtg_present_dims.is_none()
            && self.present_tv_aperture_rows == next_tv_aperture_rows
            && self.present_programmable == result.programmable
            && presentation_pixels_equal(
                &self.present_fb,
                self.present_rows,
                self.present_width,
                &result.presentation_fb,
                result.present_rows,
                result.present_width,
            );
        if unchanged {
            self.render_recycle_fb = result.presentation_fb;
            self.last_rendered_emulated_frame = Some(result.emulated_frame);
            return true;
        }

        self.main_presentation_dirty = true;
        let old = std::mem::replace(&mut self.present_fb, result.presentation_fb);
        self.render_recycle_fb = old;
        self.present_rows = result.present_rows;
        self.present_width = result.present_width;
        self.present_tv_aperture_rows = next_tv_aperture_rows;
        self.present_programmable = result.programmable;
        self.rtg_present_dims = None;
        self.last_rendered_emulated_frame = Some(result.emulated_frame);
        true
    }

    pub(super) fn collect_threaded_render_results(&mut self, wait: bool) -> bool {
        let mut rendered = false;
        loop {
            let result = match self.render_worker.as_ref() {
                Some(worker) if wait => match worker.recv() {
                    Ok(result) => result,
                    Err(_) => {
                        self.render_worker = None;
                        return rendered;
                    }
                },
                Some(worker) => match worker.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => return rendered,
                    Err(TryRecvError::Disconnected) => {
                        self.render_worker = None;
                        return rendered;
                    }
                },
                None => return rendered,
            };
            rendered |= self.apply_threaded_render_result(result);
            if wait {
                return rendered;
            }
        }
    }

    pub(super) fn render_emulated_frame_threaded(&mut self) -> bool {
        let mut rendered = self.collect_threaded_render_results(false);
        let emulated_frame = self.emu.bus().emulated_frames();
        if !should_render_emulated_frame(self.last_submitted_render_frame, emulated_frame) {
            return rendered;
        }

        let input = match self.render_recycle_input.take() {
            Some(mut input) => {
                input.refill_from_bus(self.emu.bus());
                input
            }
            None => bitplane::RenderInput::from_bus(self.emu.bus()),
        };
        let h_shift = if self.hcenter {
            self.presentation_latch
                .presentation_h_shift(&input.render_base(), self.overscan)
        } else {
            0
        };
        let job = RenderJob {
            generation: self.render_generation,
            input,
            h_shift,
            overscan: self.overscan,
            deinterlace: self.deinterlace,
            phosphor: self.phosphor,
            presentation_fb: std::mem::take(&mut self.render_recycle_fb),
        };
        let send_result = self
            .render_worker
            .as_ref()
            .expect("threaded render path without worker")
            .send(job);
        match send_result {
            Ok(()) => {
                self.last_submitted_render_frame = Some(emulated_frame);
            }
            Err(job) => {
                warn!("render worker stopped; falling back to synchronous rendering");
                self.render_recycle_fb = job.presentation_fb;
                self.render_recycle_input = Some(job.input);
                self.render_worker = None;
                rendered |= self.render_emulated_frame_sync();
            }
        }
        rendered | self.collect_threaded_render_results(false)
    }

    pub(super) fn finish_render_for_current_frame(&mut self) -> bool {
        if !self.powered_on {
            return false;
        }
        if !self.emu.bus().frame_render_available() {
            return false;
        }
        let target = self.emu.bus().emulated_frames();
        let mut rendered = self.render_emulated_frame_if_needed();
        while self.render_worker.is_some() && self.last_rendered_emulated_frame != Some(target) {
            rendered |= self.collect_threaded_render_results(true);
        }
        rendered
    }

    /// Present the RTG board frame when one is driving the display: the
    /// board frame (own resolution) is scaled horizontally into the
    /// FB_WIDTH-stride presentation buffer, and the shared vertical scaling
    /// maps its rows to the output height. Returns `None` when no RTG board
    /// is active (native chipset presentation as usual).
    pub(super) fn render_rtg_frame_if_active(&mut self) -> Option<bool> {
        if !self.emu.bus().rtg_active() {
            return None;
        }
        let emulated_frame = self.emu.bus().emulated_frames();
        if !should_render_emulated_frame(self.last_rendered_emulated_frame, emulated_frame) {
            return Some(false);
        }
        let mut rtg = std::mem::take(&mut self.rtg_fb);
        let mut present = std::mem::take(&mut self.present_fb);
        let composed = compose_rtg_present(self.emu.bus(), &mut rtg, &mut present);
        self.rtg_fb = rtg;
        self.present_fb = present;
        let Some((rows, native_w, native_h)) = composed else {
            // rtg_active() is true but the frame did not compose (e.g. MODE
            // set before ORIG_RES): fall back to the chipset render rather
            // than freezing on the stale frame.
            self.rtg_present_dims = None;
            return None;
        };
        // The native frame stays in `rtg_fb`; the window presents it at full
        // resolution through the RTG texture, while `present_fb` keeps the
        // FB_WIDTH version the screenshot path reads.
        if self.rtg_present_dims.is_none() {
            // Entering RTG is a presentation discontinuity. Advance the
            // generation so the render worker clears its chipset repeated-
            // frame and deinterlace history before native output resumes;
            // otherwise an exact pre-RTG input match could retain this RTG
            // buffer instead of producing the first returning chipset frame.
            self.render_generation = self.render_generation.wrapping_add(1);
            self.presentation_latch.reset();
            self.autocrop_latch.reset();
            self.present_content_rect = None;
        }
        self.rtg_present_dims = Some((native_w, native_h));
        self.main_presentation_dirty = true;
        self.present_rows = rows;
        self.present_width = FB_WIDTH;
        self.present_tv_aperture_rows = None;
        self.present_programmable = false;
        self.last_rendered_emulated_frame = Some(emulated_frame);
        self.last_submitted_render_frame = Some(emulated_frame);
        Some(true)
    }

    pub(super) fn render_emulated_frame_if_needed(&mut self) -> bool {
        if !self.emu.bus().frame_render_available() {
            return false;
        }
        // Drain in-flight chipset render results first (recycling their
        // buffers) so a stale result cannot land on top of an RTG frame.
        // Their "new frame applied" outcome must propagate to the caller,
        // which uses it to schedule the window redraw.
        let mut rendered = false;
        if self.render_worker.is_some() {
            rendered = self.collect_threaded_render_results(false);
        }
        if let Some(rtg_rendered) = self.render_rtg_frame_if_active() {
            return rendered | rtg_rendered;
        }
        if self.render_worker.is_some() {
            return rendered | self.render_emulated_frame_threaded();
        }
        rendered | self.render_emulated_frame_sync()
    }

    pub(super) fn render_emulated_frame_sync(&mut self) -> bool {
        let emulated_frame = self.emu.bus().emulated_frames();
        if !should_render_emulated_frame(self.last_rendered_emulated_frame, emulated_frame) {
            return false;
        }

        let visible_start_vpos = self.emu.bus().frame_visible_start_vpos();
        let h_shift = if self.hcenter {
            self.presentation_latch
                .presentation_h_shift(&self.emu.bus().frame_render_base(), self.overscan)
        } else {
            0
        };
        let field_content = bitplane::render(self.emu.bus_mut(), &mut self.fb);
        let geometry = self.emu.bus().frame_geometry();
        let canvas_scale = self.emu.bus().frame_canvas_scale();
        let field_rows = post_process_rendered_field(
            &mut self.fb,
            geometry,
            canvas_scale,
            self.emu.bus().frame_presentation_h_window(),
            self.emu.bus().frame_presentation_v_window(),
            visible_start_vpos,
            h_shift,
            self.overscan,
        );
        let base = self.emu.bus().frame_render_base();
        // Standard 15 kHz fields line-double / weave to 2x rows; a
        // programmable progressive scan already carries every line.
        let mut next_present_fb = std::mem::take(&mut self.render_recycle_fb);
        let (rows, width) = self.deinterlacer.present_field_into(
            &self.fb,
            field_rows,
            FB_WIDTH * canvas_scale,
            base.bplcon0 & 0x0004 != 0,
            base.long_field,
            !geometry.programmable,
            &mut next_present_fb,
        );
        let smoothed = self.autocrop_latch.resolve(woven_content_rect(
            field_content,
            geometry.programmable,
            visible_start_vpos,
            h_shift,
            FB_WIDTH * canvas_scale,
            rows,
        ));
        if smoothed != self.present_content_rect {
            self.present_content_rect = smoothed;
            self.main_presentation_dirty = true;
        }
        let next_tv_aperture_rows = self
            .presentation_latch
            .resolve_tv_aperture(standard_tv_aperture_frame(geometry, rows, &base));
        let unchanged = self.rtg_present_dims.is_none()
            && self.present_tv_aperture_rows == next_tv_aperture_rows
            && self.present_programmable == geometry.programmable
            && presentation_pixels_equal(
                &self.present_fb,
                self.present_rows,
                self.present_width,
                &next_present_fb,
                rows,
                width,
            );
        if unchanged {
            self.render_recycle_fb = next_present_fb;
        } else {
            self.main_presentation_dirty = true;
            let old = std::mem::replace(&mut self.present_fb, next_present_fb);
            self.render_recycle_fb = old;
            self.present_rows = rows;
            self.present_width = width;
            self.present_tv_aperture_rows = next_tv_aperture_rows;
            self.present_programmable = geometry.programmable;
        }
        self.rtg_present_dims = None;
        self.last_rendered_emulated_frame = Some(emulated_frame);
        self.last_submitted_render_frame = Some(emulated_frame);
        true
    }
}
