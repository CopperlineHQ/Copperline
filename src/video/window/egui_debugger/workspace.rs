// SPDX-License-Identifier: GPL-3.0-or-later

//! Main-window layout, presentation preparation, and guest/UI input ownership.

use super::*;
use crate::video::window::{
    host_shortcut_modifier_pressed, window_title, HOST_SHORTCUT_MODIFIER_LABEL,
};
use winit::{
    dpi::{LogicalSize, PhysicalSize},
    event::ElementState,
    keyboard::PhysicalKey,
};

pub(in crate::video::window) struct PlayGeometry {
    size: PhysicalSize<u32>,
    position: Option<winit::dpi::PhysicalPosition<i32>>,
}

impl Layout {
    pub(super) fn display_pane(&mut self, root: &mut egui::Ui, actions: &mut Vec<Action>) {
        let maximum = (root.available_width() - 480.0).max(240.0);
        let pane = egui::Panel::left("debug_display")
            .resizable(true)
            .default_size(self.preferences.display_width.min(maximum))
            .size_range(240.0..=maximum)
            .frame(egui::Frame::NONE.inner_margin(10))
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Amiga display").strong().color(Color32::WHITE));
                    ui.label(RichText::new(if self.guest_input { "Input: Amiga" } else { "Input: debugger" })
                        .color(Color32::LIGHT_GRAY));
                });
                ui.add(egui::Label::new(RichText::new(format!(
                    "Click the display to control the Amiga · {HOST_SHORTCUT_MODIFIER_LABEL}+G releases input"
                )).small().color(Color32::LIGHT_GRAY)).wrap());
                ui.add_space(8.0);
                let rect = ui.available_rect_before_wrap();
                self.display_rect = Some(rect);
                // Leave the picture transparent: the existing GPU passes paint
                // it before egui adds the surrounding controls on this surface.
                if ui.allocate_rect(rect, egui::Sense::click()).clicked() {
                    actions.push(Action::CaptureDisplay);
                }
            });
        self.preferences.display_width = pane.response.rect.width();
    }
}

impl App {
    pub(in crate::video::window) fn enter_debug_workspace(&mut self) {
        self.suspend_mouse_capture_for_ui();
        self.release_debug_guest_input();
        let entering = !self.debug_layout_active;
        self.debug_layout_active = true;
        self.ensure_debug_workspace();
        if entering {
            let size = self.egui_layout_preferences().workspace_size;
            if let Some(r) = &self.render {
                let windowed = r.window.fullscreen().is_none() && !r.window.is_maximized();
                // Some window managers enforce a new minimum immediately.
                // Save Play's geometry before any operation can resize it.
                if windowed {
                    self.debug_play_geometry
                        .get_or_insert_with(|| PlayGeometry {
                            size: r.window.inner_size(),
                            position: r.window.outer_position().ok(),
                        });
                }
                r.window.set_title("Copperline · Debug");
                r.window
                    .set_min_inner_size(Some(LogicalSize::new(900.0, 600.0)));
                if windowed {
                    let available = r
                        .window
                        .current_monitor()
                        .map(|monitor| monitor.size().to_logical::<f64>(r.window.scale_factor()));
                    let size = LogicalSize::new(
                        size[0].min(available.map_or(size[0], |s| (s.width - 80.0).max(900.0))),
                        size[1].min(available.map_or(size[1], |s| (s.height - 100.0).max(600.0))),
                    );
                    let _ = r.window.request_inner_size(size);
                    // Growing the Play window can push its right/bottom edge
                    // off the monitor. Keep the expanded workspace reachable.
                    if let (Some(monitor), Ok(position)) =
                        (r.window.current_monitor(), r.window.outer_position())
                    {
                        let scale = r.window.scale_factor();
                        let wanted = size.to_physical::<u32>(scale);
                        let origin = monitor.position();
                        let extent = monitor.size();
                        let margin = (16.0 * scale) as i64;
                        let left = i64::from(origin.x) + margin;
                        let top = i64::from(origin.y) + (48.0 * scale) as i64;
                        let right = i64::from(origin.x) + i64::from(extent.width)
                            - i64::from(wanted.width)
                            - margin;
                        let bottom = i64::from(origin.y) + i64::from(extent.height)
                            - i64::from(wanted.height)
                            - (48.0 * scale) as i64;
                        r.window
                            .set_outer_position(winit::dpi::PhysicalPosition::new(
                                i64::from(position.x).clamp(left, right.max(left)) as i32,
                                i64::from(position.y).clamp(top, bottom.max(top)) as i32,
                            ));
                    }
                }
                r.window.focus_window();
            }
        }
        self.request_redraw();
    }

    pub(in crate::video::window) fn leave_debug_workspace(&mut self) {
        if !self.debug_layout_active {
            return;
        }
        self.save_egui_preferences();
        self.release_debug_guest_input();
        self.debug_layout_active = false;
        if let Some(r) = &mut self.render {
            r.debug_viewport = None;
            r.window.set_title(window_title());
            r.window.set_min_inner_size(Some(LogicalSize::new(
                crate::video::FB_WIDTH as f64 / 2.0,
                crate::video::window::window_present_height() as f64 / 2.0,
            )));
        }
        self.restore_play_geometry();
        self.restore_mouse_capture_after_ui();
        self.apply_auto_mouse_capture();
        self.request_redraw();
    }

    // Fullscreen/maximized windows defer restoration until the desktop size
    // returns. Keep the saved Play geometry across that asynchronous resize.
    pub(in crate::video::window) fn restore_play_geometry(&mut self) -> bool {
        if self.debug_layout_active {
            return false;
        }
        if let Some(r) = &self.render {
            if r.window.fullscreen().is_none() && !r.window.is_maximized() {
                if let Some(geometry) = self.debug_play_geometry.take() {
                    let applied = r.window.request_inner_size(geometry.size);
                    if let Some(position) = geometry.position {
                        if r.window.available_monitors().any(|monitor| {
                            let p = monitor.position();
                            let size = monitor.size();
                            title_bar_visible(
                                [position.x, position.y],
                                [p.x, p.y],
                                [size.width, size.height],
                            )
                        }) {
                            r.window.set_outer_position(position);
                        }
                    }
                    if let Some(applied) = applied {
                        self.apply_surface_size(applied);
                    }
                    // Preserve canvas ownership across the layout's resize.
                    if !self.window_manually_sized {
                        self.snap_request_deadline = Some(
                            Instant::now() + crate::video::window::CANVAS_SNAP_RESPONSE_TIMEOUT,
                        );
                    }
                    return true;
                }
            }
        }
        false
    }

    pub(in crate::video::window) fn ensure_debug_workspace(&mut self) {
        if !self.debug_layout_active || self.debugger_ui.is_some() {
            return;
        }
        let preferences = self.egui_layout_preferences().clone();
        let Some(r) = &mut self.render else {
            return;
        };
        let mut ui = DebuggerUi::new(&r.window, &r.pixels, preferences);
        ui.layout.workspace = true;
        self.debugger_ui = Some(ui);
        self.request_redraw();
    }

    pub(in crate::video::window) fn capture_debug_guest_input(&mut self) {
        self.debug_guest_input = true;
        self.nav.clear();
        if let Some(ui) = &mut self.debugger_ui {
            ui.context.memory_mut(|memory| {
                if let Some(id) = memory.focused() {
                    memory.surrender_focus(id);
                }
            });
        }
        if self.mouse_port().is_some() {
            self.set_mouse_captured(true);
        }
        self.request_redraw();
    }

    pub(in crate::video::window) fn release_debug_guest_input(&mut self) {
        let release_buttons = self.debug_guest_input && !self.mouse_captured;
        self.debug_guest_input = false;
        self.set_mouse_captured(false);
        if release_buttons {
            self.release_mouse_buttons();
        }
        // Releases cross the same input/recording boundary as ordinary keys.
        // A focus change must not leave a guest qualifier held indefinitely.
        for rawkey in 0..128 {
            self.release_amiga_rawkey_if_held(rawkey);
        }
        self.raw_device_held_rawkeys.fill(false);
        if self
            .keyboard_joy_held
            .iter()
            .any(crate::keymap::HeldKeys::any_held)
        {
            self.keyboard_joy_held = Default::default();
            self.pump_joystick_input();
        }
        self.last_display_cursor_pos = None;
        self.request_redraw();
    }

    /// True when the workspace consumed the event. Window lifecycle events
    /// still reach the main window's resize, focus, and minimized guards.
    pub(in crate::video::window) fn route_debug_workspace_event(
        &mut self,
        event: &WindowEvent,
    ) -> bool {
        if !self.debug_layout_active || self.ui.active() {
            return false;
        }
        if let WindowEvent::ModifiersChanged(modifiers) = event {
            self.update_host_modifiers(modifiers.state());
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if host_shortcut_modifier_pressed(self.modifiers) {
                if key.physical_key == PhysicalKey::Code(KeyCode::KeyG) {
                    if key.state == ElementState::Pressed && !key.repeat {
                        if self.debug_guest_input {
                            self.release_debug_guest_input();
                        } else {
                            self.capture_debug_guest_input();
                        }
                    }
                    return true;
                }
                if let PhysicalKey::Code(code) = key.physical_key {
                    let editing = self
                        .debugger_ui
                        .as_ref()
                        .is_some_and(|ui| ui.context.text_edit_focused());
                    if host_shortcut_reaches_main(code, self.modifiers, editing) {
                        return false;
                    }
                }
            }
        }
        if self.debug_guest_input && !self.mouse_captured {
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } = event
            {
                let inside = self
                    .debugger_ui
                    .as_ref()
                    .and_then(|ui| {
                        ui.layout
                            .display_rect
                            .map(|rect| (rect, ui.context.pixels_per_point()))
                    })
                    .zip(self.last_cursor_phys)
                    .is_some_and(|((rect, scale), position)| {
                        rect.contains(egui::pos2(
                            position.x as f32 / scale,
                            position.y as f32 / scale,
                        ))
                    });
                if !inside {
                    self.release_debug_guest_input();
                }
            }
        }
        let input_event = matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::Ime(_)
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::Touch(_)
        );
        if self.debug_guest_input && input_event {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => self.last_cursor_phys = Some(*position),
            WindowEvent::CursorLeft { .. } => self.last_cursor_phys = None,
            _ => {}
        }
        if let (Some(ui), Some(r)) = (&mut self.debugger_ui, &self.render) {
            ui.on_event(&r.window, event);
        }
        input_event
    }

    pub(in crate::video::window) fn schedule_egui_debugger_repaint(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        if !self.debug_layout_active || self.ui.active() {
            return;
        }
        let (Some(ui), Some(r)) = (&mut self.debugger_ui, &self.render) else {
            return;
        };
        if r.minimized {
            return;
        }
        let Some(at) = ui.repaint_at else {
            return;
        };
        if at <= Instant::now() {
            ui.repaint_at = None;
            ui.input_dirty = true;
            r.window.request_redraw();
        } else if matches!(
            event_loop.control_flow(),
            winit::event_loop::ControlFlow::Wait
        ) {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(at));
        }
    }

    pub(in crate::video::window) fn prepare_debug_workspace(&mut self) -> Vec<Action> {
        if !self.debug_layout_active || self.ui.active() {
            if let Some(r) = &mut self.render {
                r.debug_viewport = None;
            }
            return Vec::new();
        }
        self.ensure_debug_workspace();
        let Some(mut ui) = self.debugger_ui.take() else {
            return Vec::new();
        };
        let refresh = self.debug_snapshot_dirty.replace(false) || ui.snapshot.is_none();
        if !refresh && !ui.input_dirty && !ui.repaint_due() {
            self.debugger_ui = Some(ui);
            return Vec::new();
        }
        if refresh {
            ui.snapshot = match self.egui_selected_tool {
                ToolPanelKind::Debugger => self.debugger_panel.as_ref().map(|panel| {
                    Snapshot::Debugger(Box::new(
                        self.build_debugger_view_with_clipping(panel, false),
                    ))
                }),
                ToolPanelKind::FrameAnalyzer => {
                    self.ensure_analyzer_underlay();
                    self.frame_analyzer_panel.as_ref().map(|panel| {
                        Snapshot::Analyzer(Box::new(self.build_frame_analyzer_view(panel)))
                    })
                }
                ToolPanelKind::Console => Some(Snapshot::Console),
            };
        }
        ui.layout.guest_input = self.debug_guest_input;
        let snapshot = ui.snapshot.take();
        let actions = if let Some(r) = &self.render {
            match &snapshot {
                Some(Snapshot::Debugger(view)) => {
                    self.debugger_panel.as_mut().map_or_else(Vec::new, |panel| {
                        ui.prepare(
                            &r.window,
                            &r.pixels,
                            Content::Debugger(panel, view),
                            self.mouse_captured,
                        )
                    })
                }
                Some(Snapshot::Analyzer(view)) => {
                    self.frame_analyzer_panel
                        .as_mut()
                        .map_or_else(Vec::new, |panel| {
                            ui.prepare(
                                &r.window,
                                &r.pixels,
                                Content::Analyzer(panel, view),
                                self.mouse_captured,
                            )
                        })
                }
                Some(Snapshot::Console) => {
                    self.console_panel.as_mut().map_or_else(Vec::new, |panel| {
                        ui.prepare(
                            &r.window,
                            &r.pixels,
                            Content::Console(panel, if self.paused { "Paused" } else { "Running" }),
                            self.mouse_captured,
                        )
                    })
                }
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        ui.snapshot = snapshot;
        if let Some(r) = &mut self.render {
            r.debug_viewport = ui.layout.display_rect.map(|rect| {
                let scale = ui.context.pixels_per_point();
                let min = (rect.min * scale).round();
                let max = (rect.max * scale).round();
                (
                    min.x.max(0.0) as u32,
                    min.y.max(0.0) as u32,
                    (max.x - min.x).max(1.0) as u32,
                    (max.y - min.y).max(1.0) as u32,
                )
            });
        }
        self.debugger_ui = Some(ui);
        actions
    }
}

pub(super) fn host_shortcut_reaches_main(
    code: KeyCode,
    modifiers: winit::keyboard::ModifiersState,
    editing: bool,
) -> bool {
    if !host_shortcut_modifier_pressed(modifiers) {
        return false;
    }
    // On macOS the app's Command+A/Z overlap Select All and Undo/Redo.
    // Linux/Windows use Alt for app shortcuts, leaving Ctrl edits to egui.
    if editing && cfg!(target_os = "macos") && matches!(code, KeyCode::KeyA | KeyCode::KeyZ) {
        return false;
    }
    matches!(
        code,
        KeyCode::KeyA
            | KeyCode::KeyB
            | KeyCode::KeyD
            | KeyCode::KeyE
            | KeyCode::KeyF
            | KeyCode::KeyJ
            | KeyCode::KeyK
            | KeyCode::KeyM
            | KeyCode::KeyP
            | KeyCode::KeyQ
            | KeyCode::KeyR
            | KeyCode::KeyS
            | KeyCode::KeyW
            | KeyCode::KeyZ
    ) || super::super::save_slot_for_key(code).is_some()
        || (modifiers.shift_key()
            && matches!(
                code,
                KeyCode::KeyL | KeyCode::Equal | KeyCode::Minus | KeyCode::Period | KeyCode::Comma
            ))
}

pub(super) fn title_bar_visible(position: [i32; 2], monitor: [i32; 2], size: [u32; 2]) -> bool {
    let [x, y] = position.map(i64::from);
    let [left, top] = monitor.map(i64::from);
    x >= left
        && x + 160 <= left + i64::from(size[0])
        && y >= top
        && y + 64 <= top + i64::from(size[1])
}
