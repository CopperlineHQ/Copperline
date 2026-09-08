// SPDX-License-Identifier: GPL-3.0-or-later

//! Debug workspace frontend. All machine reads arrive as a snapshot and all
//! commands leave as actions, after egui's (potentially repeated) layout pass.
//! The Amiga display and egui share the main window and its GPU surface.

use super::{ui, App, KeyCode, ToolPanelKind, UiControl};
use egui::{Color32, FontId, RichText, ScrollArea, Stroke};
use pixels::wgpu;
use std::collections::HashMap;
use std::time::Instant;
use winit::{event::WindowEvent, window::Window};

const BLUE: Color32 = Color32::from_rgb(35, 75, 164);
const INK: Color32 = Color32::from_rgb(28, 32, 40);
const PAPER: Color32 = Color32::from_rgb(238, 240, 242);

#[derive(Debug, PartialEq)]
pub(super) enum Action {
    Control(UiControl),
    Analyzer(UiControl),
    AnalyzerKey(KeyCode),
    ResourceScroll(isize),
    BlitScroll(isize),
    SelectTool(ToolPanelKind),
    CloseWorkspace,
    CaptureDisplay,
    CloseTool(ToolPanelKind),
    Navigate(ui::DebugTab, u32),
    ConsoleSubmit(String),
    SubmitEntry,
    FollowPc,
    FollowCopper,
    MemoryScroll(i32),
    IoMapScroll(i32),
}

enum Content<'a> {
    Debugger(&'a mut ui::DebuggerPanel, &'a ui::DebuggerView),
    Analyzer(&'a mut ui::FrameAnalyzerPanel, &'a ui::FrameAnalyzerView),
    Console(&'a mut ui::ConsolePanel, &'a str),
}

pub(super) struct DebuggerUi {
    context: egui::Context,
    input: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    layout: Layout,
    repaint_at: Option<Instant>,
    prepared: Option<PreparedFrame>,
    snapshot: Option<Snapshot>,
    input_dirty: bool,
}

#[derive(Default)]
struct Layout {
    images: HashMap<String, (egui::ColorImage, egui::TextureHandle)>,
    preferences: preferences::Preferences,
    last_tool: Option<ToolPanelKind>,
    console_to_end: bool,
    navigation: Option<ui::DebugTab>,
    workspace: bool,
    guest_input: bool,
    display_rect: Option<egui::Rect>,
}

impl DebuggerUi {
    pub(super) fn new(
        window: &Window,
        pixels: &pixels::Pixels<'_>,
        preferences: preferences::Preferences,
    ) -> Self {
        let context = egui::Context::default();
        configure_style(&context);
        let input = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            None,
            Some(pixels.device().limits().max_texture_dimension_2d as usize),
        );
        let renderer = egui_wgpu::Renderer::new(
            pixels.device(),
            pixels.surface_texture_format(),
            egui_wgpu::RendererOptions {
                dithering: false,
                ..Default::default()
            },
        );
        Self {
            context,
            input,
            renderer,
            layout: Layout {
                preferences,
                ..Default::default()
            },
            repaint_at: None,
            prepared: None,
            snapshot: None,
            input_dirty: true,
        }
    }

    pub(super) fn on_event(&mut self, window: &Window, event: &WindowEvent) {
        if !window.is_maximized() && window.fullscreen().is_none() {
            if let WindowEvent::Resized(size) = event {
                if size.width > 0 && size.height > 0 {
                    let size = size.to_logical::<f64>(window.scale_factor());
                    self.layout.preferences.workspace_size = [size.width, size.height];
                }
            }
        }
        if self.input.on_window_event(window, event).repaint {
            self.input_dirty = true;
            window.request_redraw();
        }
    }

    pub(super) fn repaint_due(&self) -> bool {
        self.repaint_at.is_some_and(|at| Instant::now() >= at)
    }

    fn prepare(
        &mut self,
        window: &Window,
        pixels: &pixels::Pixels<'_>,
        content: Content<'_>,
        mouse_captured: bool,
    ) -> Vec<Action> {
        let input = self.input.take_egui_input(window);
        let (mut output, actions) =
            run_content_frame(&self.context, &mut self.layout, input, content);
        if mouse_captured {
            output.platform_output.cursor_icon = egui::CursorIcon::None;
            output.platform_output.cursor_image = None;
        }
        self.input
            .handle_platform_output(window, output.platform_output);
        self.repaint_at = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .and_then(|viewport| Instant::now().checked_add(viewport.repaint_delay));
        for (id, delta) in &output.textures_delta.set {
            self.renderer
                .update_texture(pixels.device(), pixels.queue(), *id, delta);
        }
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        let size = window.inner_size();
        self.prepared = Some(PreparedFrame {
            jobs: self
                .context
                .tessellate(output.shapes, output.pixels_per_point),
            screen: egui_wgpu::ScreenDescriptor {
                size_in_pixels: [size.width.max(1), size.height.max(1)],
                pixels_per_point: output.pixels_per_point,
            },
        });
        self.input_dirty = false;
        actions
    }

    pub(super) fn paint_prepared(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
    ) {
        if let Some(frame) = &self.prepared {
            paint(
                &mut self.renderer,
                device,
                queue,
                encoder,
                target,
                &frame.jobs,
                &frame.screen,
                false,
            );
        }
    }
}

#[cfg(test)]
fn run_frame(
    context: &egui::Context,
    layout: &mut Layout,
    input: egui::RawInput,
    panel: &mut ui::DebuggerPanel,
    view: &ui::DebuggerView,
) -> (egui::FullOutput, Vec<Action>) {
    run_content_frame(context, layout, input, Content::Debugger(panel, view))
}

fn run_content_frame(
    context: &egui::Context,
    layout: &mut Layout,
    input: egui::RawInput,
    mut content: Content<'_>,
) -> (egui::FullOutput, Vec<Action>) {
    let mut actions = Vec::new();
    let output = context.run_ui(input, |root| {
        let (selected, status) = match &content {
            Content::Debugger(_, view) => (ToolPanelKind::Debugger, view.status.as_str()),
            Content::Analyzer(_, view) => (ToolPanelKind::FrameAnalyzer, view.status.as_str()),
            Content::Console(_, status) => (ToolPanelKind::Console, *status),
        };
        if layout.workspace {
            egui::Panel::top("workspace_title")
                .frame(egui::Frame::new().fill(BLUE).inner_margin(8))
                .show(root, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Copperline · Debug")
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Return to Play").clicked() {
                                actions.push(Action::CloseWorkspace);
                            }
                            ui.add(
                                egui::Label::new(RichText::new(status).color(Color32::WHITE))
                                    .truncate(),
                            );
                        });
                    });
                });
        } else {
            egui::Panel::top("workspace_title")
                .frame(egui::Frame::new().fill(BLUE).inner_margin(8))
                .show(root, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("Copperline Debugger")
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(RichText::new(status).color(Color32::WHITE));
                    });
                });
        }
        if layout.workspace {
            layout.display_pane(root, &mut actions);
        }
        egui::Panel::top("workspace_tools").show(root, |ui| {
            ui.horizontal(|ui| {
                for (label, kind) in [
                    ("Debugger", ToolPanelKind::Debugger),
                    ("Frame Analyzer", ToolPanelKind::FrameAnalyzer),
                    ("Console", ToolPanelKind::Console),
                ] {
                    if ui
                        .selectable_label(
                            selected == kind,
                            RichText::new(label).strong().color(if selected == kind {
                                Color32::WHITE
                            } else {
                                INK
                            }),
                        )
                        .clicked()
                    {
                        actions.push(Action::SelectTool(kind));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Close inspector").clicked() {
                        actions.push(Action::CloseTool(selected));
                    }
                });
            });
        });
        match &mut content {
            Content::Debugger(panel, view) => layout.show(root, panel, view, &mut actions),
            Content::Analyzer(panel, view) => layout.analyzer(root, panel, view, &mut actions),
            Content::Console(panel, _) => layout.console(root, panel, &mut actions),
        }
        layout.last_tool = Some(selected);
    });
    for (id, dimension) in [("debugger_registers", 0), ("debugger_cpu_memory", 1)] {
        if let Some(state) = egui::containers::panel::PanelState::load(context, egui::Id::new(id)) {
            if dimension == 0 {
                layout.preferences.register_width = state.size().x;
            } else {
                layout.preferences.memory_height = state.size().y;
            }
        }
    }
    // Input can be consumed on the first of several layout passes. Keep its
    // edits and commands, but dispatch each command only once per UI frame.
    let mut unique = Vec::new();
    for action in actions {
        if !unique.contains(&action) {
            unique.push(action);
        }
    }
    layout.navigation = None;
    (output, unique)
}

fn paint(
    renderer: &mut egui_wgpu::Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    jobs: &[egui::ClippedPrimitive],
    screen: &egui_wgpu::ScreenDescriptor,
    clear: bool,
) {
    let commands = renderer.update_buffers(device, queue, encoder, jobs, screen);
    queue.submit(commands);
    let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("debugger_egui"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: if clear {
                    wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    renderer.render(&mut pass.forget_lifetime(), jobs, screen);
}

fn configure_style(context: &egui::Context) {
    let mut style = egui::Style {
        visuals: egui::Visuals::light(),
        ..Default::default()
    };
    style.visuals.panel_fill = PAPER;
    style.visuals.window_fill = PAPER;
    style.visuals.override_text_color = Some(INK);
    style.visuals.selection.bg_fill = BLUE;
    style.visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(192, 208, 242);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, INK);
    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
    ] {
        widget.corner_radius = egui::CornerRadius::ZERO;
    }
    style.spacing.item_spacing = egui::vec2(6.0, 5.0);
    style.spacing.button_padding = egui::vec2(9.0, 5.0);
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, FontId::monospace(13.0));
    context.set_theme(egui::Theme::Light);
    context.set_style_of(egui::Theme::Light, style);
}

fn button(
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
    label: &str,
    control: UiControl,
    enabled: bool,
) {
    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        actions.push(Action::Control(control));
    }
}

fn lines(ui: &mut egui::Ui, lines: &[ui::DbgLine]) {
    for line in lines {
        let mut text = RichText::new(&line.text).monospace();
        if line.highlight {
            text = text.color(BLUE).strong();
        }
        ui.add(
            egui::Label::new(text)
                .selectable(true)
                .wrap_mode(egui::TextWrapMode::Extend),
        );
    }
}

impl Layout {
    fn show(
        &mut self,
        root: &mut egui::Ui,
        panel: &mut ui::DebuggerPanel,
        view: &ui::DebuggerView,
        actions: &mut Vec<Action>,
    ) {
        if root.ctx().current_pass_index() == 0 {
            shortcuts(root, panel, actions);
        }
        egui::Panel::top("debugger_tabs").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                for tab in ui::DEBUG_TABS {
                    if ui
                        .selectable_label(
                            panel.tab == tab,
                            RichText::new(ui::debug_tab_label(tab)).color(if panel.tab == tab {
                                Color32::WHITE
                            } else {
                                INK
                            }),
                        )
                        .clicked()
                    {
                        actions.push(Action::Control(UiControl::DebugTab(tab)));
                    }
                }
            });
        });
        egui::Panel::bottom("debugger_transport").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (label, control) in [
                    (
                        if view.running { "Pause (R)" } else { "Run (R)" },
                        UiControl::DebugRun,
                    ),
                    ("Step (S)", UiControl::DebugStep),
                    ("Over (O)", UiControl::DebugStepOver),
                    ("Out (U)", UiControl::DebugStepOut),
                    ("Frame (F)", UiControl::DebugStepFrame),
                    ("Line (L)", UiControl::DebugRunLine),
                ] {
                    button(ui, actions, label, control, true);
                }
                for (label, control) in [
                    ("< Frame", UiControl::DebugReverseFrame),
                    ("< Step", UiControl::DebugReverseStep),
                    ("< Run", UiControl::DebugReverseRun),
                ] {
                    button(ui, actions, label, control, view.reverse_available);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Address / command");
                let entry = ui.add(
                    egui::TextEdit::singleline(&mut panel.entry)
                        .id(egui::Id::new("debugger_entry"))
                        .font(egui::TextStyle::Monospace)
                        .char_limit(512)
                        .desired_width(280.0),
                );
                panel.entry_active = entry.has_focus();
                if entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    actions.push(Action::SubmitEntry);
                }
                button(
                    ui,
                    actions,
                    "Run to",
                    UiControl::DebugRunTo,
                    panel.entry_addr().is_some(),
                );
                let can_poke = match panel.tab {
                    ui::DebugTab::Cpu => panel.reg_poke().is_some(),
                    ui::DebugTab::Memory => panel.poke_target().is_some(),
                    _ => false,
                };
                button(
                    ui,
                    actions,
                    if panel.tab == ui::DebugTab::Cpu {
                        "Set Reg"
                    } else {
                        "Poke"
                    },
                    UiControl::DebugPoke,
                    can_poke,
                );
            });
        });
        if panel.tab == ui::DebugTab::Cpu {
            if let Some(cpu) = &view.cpu {
                self.cpu(root, panel, cpu, actions);
                return;
            }
        }
        egui::CentralPanel::default().show(root, |ui| {
            self.tab_controls(ui, panel, view, actions);
            ui.separator();
            let mut scroll = ScrollArea::both()
                .id_salt(format!("debugger_{:?}", panel.tab))
                .auto_shrink([false, false]);
            if self.navigation == Some(panel.tab) {
                scroll = scroll.scroll_offset(egui::Vec2::ZERO);
            }
            scroll.show_viewport(ui, |ui, viewport| match panel.tab {
                ui::DebugTab::Video => {
                    if let Some(video) = &view.video {
                        self.video(ui, video, actions);
                    }
                }
                ui::DebugTab::Audio => {
                    if let Some(audio) = &view.audio {
                        self.audio(ui, audio, viewport.width(), actions);
                    }
                }
                _ => {
                    lines(ui, &view.lines);
                    if let Some(bitmap) = &view.bitmap {
                        let size = [bitmap.stride * 8, bitmap.rows];
                        let colors = bitmap
                            .data
                            .iter()
                            .flat_map(|byte| {
                                (0..8).map(move |bit| {
                                    if byte & (0x80 >> bit) == 0 {
                                        Color32::from_gray(20)
                                    } else {
                                        PAPER
                                    }
                                })
                            })
                            .collect();
                        self.image(ui, "memory_bits".into(), size, colors, 2.0);
                    }
                }
            });
        });
    }

    fn cpu(
        &mut self,
        root: &mut egui::Ui,
        panel: &mut ui::DebuggerPanel,
        cpu: &ui::CpuView,
        actions: &mut Vec<Action>,
    ) {
        egui::Panel::left("debugger_registers")
            .resizable(true)
            .default_size(self.preferences.register_width)
            .size_range(180.0..=480.0)
            .show(root, |ui| {
                ScrollArea::both()
                    .id_salt("cpu_registers_scroll")
                    .show(ui, |ui| {
                        ui.heading("Registers");
                        egui::Grid::new("registers").striped(true).show(ui, |ui| {
                            for (name, value) in [
                                ("PC".to_string(), cpu.pc),
                                ("SR".to_string(), u32::from(cpu.sr)),
                            ]
                            .into_iter()
                            .chain(cpu.d.iter().enumerate().map(|(i, v)| (format!("D{i}"), *v)))
                            .chain(cpu.a.iter().enumerate().map(|(i, v)| (format!("A{i}"), *v)))
                            {
                                ui.monospace(&name);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("{value:08X}")).monospace(),
                                    )
                                    .selectable(true),
                                );
                                if ui
                                    .small_button("Edit")
                                    .on_hover_text("Prepare a register edit; Set Reg applies it")
                                    .clicked()
                                {
                                    panel.entry = format!("{name} {value:08X}");
                                    ui.memory_mut(|m| {
                                        m.request_focus(egui::Id::new("debugger_entry"))
                                    });
                                }
                                ui.end_row();
                            }
                        });
                        ui.monospace(ui::sr_flags(cpu.sr));
                        if cpu.stopped {
                            ui.colored_label(BLUE, "CPU stopped");
                        }
                        ui.separator();
                        ui.strong("Recent PCs");
                        for pc in &cpu.history {
                            ui.monospace(format!("{pc:08X}"));
                        }
                    });
            });
        egui::Panel::bottom("debugger_cpu_memory")
            .resizable(true)
            .default_size(self.preferences.memory_height)
            .size_range(100.0..=480.0)
            .show(root, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Memory");
                    ui.add(
                        egui::DragValue::new(&mut panel.mem_addr)
                            .hexadecimal(8, false, true)
                            .speed(16.0),
                    );
                    panel.mem_addr &= !0xF;
                    if ui.button("Previous page").clicked() {
                        actions.push(Action::MemoryScroll(-16));
                    }
                    if ui.button("Next page").clicked() {
                        actions.push(Action::MemoryScroll(16));
                    }
                });
                ScrollArea::both()
                    .id_salt("cpu_memory_scroll")
                    .show(ui, |ui| {
                        lines(ui, &cpu.memory);
                    });
            });
        egui::CentralPanel::default().show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("Disassembly");
                if panel.disasm_addr.is_some() && ui.button("Follow PC").clicked() {
                    actions.push(Action::FollowPc);
                }
            });
            let mut scroll = ScrollArea::both()
                .id_salt("cpu_disassembly_scroll")
                .auto_shrink([false, false]);
            if self.navigation == Some(ui::DebugTab::Cpu) {
                scroll = scroll.scroll_offset(egui::Vec2::ZERO);
            }
            scroll.show(ui, |ui| {
                lines(ui, &cpu.disassembly);
            });
        });
    }

    fn tab_controls(
        &self,
        ui: &mut egui::Ui,
        panel: &mut ui::DebuggerPanel,
        view: &ui::DebuggerView,
        actions: &mut Vec<Action>,
    ) {
        ui.horizontal_wrapped(|ui| match panel.tab {
            ui::DebugTab::Break => {
                for (label, control, enabled) in [
                    (
                        "Break +/-",
                        UiControl::DebugBreakToggle,
                        panel.entry_addr().is_some(),
                    ),
                    (
                        "Watch +/-",
                        UiControl::DebugWatchToggle,
                        panel.entry_addr().is_some(),
                    ),
                    (
                        "Reg +/-",
                        UiControl::DebugRegToggle,
                        panel.entry_addr().is_some(),
                    ),
                    (
                        "Beam +/-",
                        UiControl::DebugBeamToggle,
                        ui::parse_beam_spec(&panel.entry).is_some(),
                    ),
                    (
                        "Catch +/-",
                        UiControl::DebugCatchToggle,
                        ui::parse_catch_spec(&panel.entry).is_some(),
                    ),
                    ("Clear all", UiControl::DebugBreaksClear, true),
                ] {
                    button(ui, actions, label, control, enabled);
                }
            }
            ui::DebugTab::Copper => {
                button(
                    ui,
                    actions,
                    "CBreak +/-",
                    UiControl::DebugCopperBreakToggle,
                    panel.entry_addr().is_some(),
                );
                button(ui, actions, "CStep (C)", UiControl::DebugCopperStep, true);
                if panel.copper_addr.is_some() && ui.button("Follow Copper").clicked() {
                    actions.push(Action::FollowCopper);
                }
            }
            ui::DebugTab::Memory => {
                for (label, control, enabled) in [
                    (
                        "Find",
                        UiControl::DebugMemFind,
                        panel.find_pattern().is_some(),
                    ),
                    (
                        "Save...",
                        UiControl::DebugMemSave,
                        panel.region_spec().is_some(),
                    ),
                    (
                        "Writer?",
                        UiControl::DebugMemWriter,
                        panel.entry_addr().is_some(),
                    ),
                    (
                        if panel.mem_view_bits { "Hex" } else { "Bits" },
                        UiControl::DebugMemBits,
                        true,
                    ),
                    ("Previous page", UiControl::DebugMemPrev, true),
                    ("Next page", UiControl::DebugMemNext, true),
                ] {
                    button(ui, actions, label, control, enabled);
                }
            }
            ui::DebugTab::IoMap => {
                if ui.button("Previous register").clicked() {
                    actions.push(Action::IoMapScroll(-1));
                }
                if ui.button("Next register").clicked() {
                    actions.push(Action::IoMapScroll(1));
                }
            }
            ui::DebugTab::Waveform => {
                button(
                    ui,
                    actions,
                    "Arm",
                    UiControl::DebugWaveArm,
                    crate::waveform::parse_wave_args(panel.entry.split_whitespace()).is_ok(),
                );
                button(ui, actions, "Stop", UiControl::DebugWaveStop, true);
            }
            _ => {
                ui.label(&view.status);
            }
        });
    }

    fn image(
        &mut self,
        ui: &mut egui::Ui,
        key: String,
        size: [usize; 2],
        colors: Vec<Color32>,
        scale: f32,
    ) {
        if size.contains(&0) {
            return;
        }
        let texture = self.texture(ui, key, size, colors);
        ui.image((
            texture,
            egui::vec2(size[0] as f32 * scale, size[1] as f32 * scale),
        ));
    }

    fn texture(
        &mut self,
        ui: &egui::Ui,
        key: String,
        size: [usize; 2],
        colors: Vec<Color32>,
    ) -> egui::TextureId {
        let image = egui::ColorImage::new(size, colors);
        let (_, texture) = self
            .images
            .entry(key.clone())
            .and_modify(|(old, texture)| {
                if *old != image {
                    texture.set(image.clone(), egui::TextureOptions::NEAREST);
                    *old = image.clone();
                }
            })
            .or_insert_with(|| {
                let texture =
                    ui.ctx()
                        .load_texture(key, image.clone(), egui::TextureOptions::NEAREST);
                (image, texture)
            });
        texture.id()
    }

    fn video(&mut self, ui: &mut egui::Ui, video: &ui::VideoView, actions: &mut Vec<Action>) {
        ui.monospace(&video.header);
        ui.horizontal_wrapped(|ui| {
            ui.strong("Bitplanes");
            for i in 0..8 {
                let mut on = video.plane_mask & (1 << i) != 0;
                if ui
                    .add_enabled(
                        i < video.nplanes,
                        egui::Checkbox::new(&mut on, format!("{}", i + 1)),
                    )
                    .changed()
                {
                    actions.push(Action::Control(UiControl::DebugPlaneToggle(i)));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.strong("Sprites");
            for i in 0..8 {
                let mut on = video.sprite_mask & (1 << i) != 0;
                if ui.checkbox(&mut on, format!("{i}")).changed() {
                    actions.push(Action::Control(UiControl::DebugSpriteToggle(i)));
                }
            }
        });
        for (i, sprite) in video.sprites.iter().enumerate() {
            ui.monospace(&sprite.text);
            self.image(
                ui,
                format!("sprite_{i}"),
                [16, sprite.thumb_rows],
                sprite.thumb.iter().map(|&p| color(p)).collect(),
                2.0,
            );
        }
        ui.strong("Palette");
        ui.horizontal_wrapped(|ui| {
            for (i, &rgb) in video.palette.iter().enumerate() {
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 0.0, color(rgb));
                response.on_hover_text(format!(
                    "{i}: #{:02X}{:02X}{:02X}",
                    color(rgb).r(),
                    color(rgb).g(),
                    color(rgb).b()
                ));
            }
        });
    }

    fn audio(
        &self,
        ui: &mut egui::Ui,
        audio: &ui::AudioScopeView,
        viewport_width: f32,
        actions: &mut Vec<Action>,
    ) {
        ui.add(
            egui::Label::new(RichText::new(&audio.header).monospace())
                .selectable(true)
                .wrap_mode(egui::TextWrapMode::Extend),
        );
        // Paula has three permanent lines and an optional pending-flags line.
        // Reserve all four even while idle. Size columns from the viewport,
        // not changing text extents, so the scopes and following rows stay put.
        let row_height =
            (ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y) * 4.0
                + ui.spacing().scroll.bar_width;
        let row_width = viewport_width.max(560.0);
        let scope_width = (row_width * 0.28).clamp(220.0, 420.0);
        for (i, row) in audio
            .channels
            .iter()
            .chain(audio.extras.iter().map(|extra| &extra.row))
            .enumerate()
        {
            ui.separator();
            let (row_rect, _) =
                ui.allocate_exact_size(egui::vec2(row_width, row_height), egui::Sense::hover());
            let rect = egui::Rect::from_min_max(
                egui::pos2(row_rect.right() - scope_width, row_rect.top()),
                row_rect.max,
            );
            let mute_rect = egui::Rect::from_min_size(row_rect.min, egui::vec2(64.0, row_height));
            let mut controls = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("audio_mute", i))
                    .max_rect(mute_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            let mut muted = row.muted;
            if controls.checkbox(&mut muted, "Mute").changed() {
                actions.push(Action::Control(UiControl::DebugAudioMute(i)));
            }
            let text_rect = egui::Rect::from_min_max(
                egui::pos2(mute_rect.right(), row_rect.top()),
                egui::pos2(rect.left() - ui.spacing().item_spacing.x, row_rect.bottom()),
            );
            let mut details = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("audio_details", i))
                    .max_rect(text_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            details.set_clip_rect(text_rect.intersect(ui.clip_rect()));
            ScrollArea::horizontal()
                .id_salt("text")
                .auto_shrink([false, false])
                .show(&mut details, |ui| lines(ui, &row.text));
            ui.painter().rect_filled(rect, 0.0, Color32::from_gray(25));
            ui.painter().line_segment(
                [rect.left_center(), rect.right_center()],
                Stroke::new(1.0, Color32::from_gray(55)),
            );
            if row.scope.len() >= 2 {
                let points = row
                    .scope
                    .iter()
                    .enumerate()
                    .map(|(i, &sample)| {
                        egui::pos2(
                            rect.left() + i as f32 / (row.scope.len() - 1) as f32 * rect.width(),
                            rect.center().y - sample as f32 / 128.0 * rect.height() * 0.45,
                        )
                    })
                    .collect();
                ui.painter().add(egui::Shape::line(
                    points,
                    Stroke::new(1.5, Color32::from_rgb(110, 220, 170)),
                ));
            }
        }
    }
}

fn color(rgba: u32) -> Color32 {
    let [r, g, b, a] = rgba.to_le_bytes();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

fn shortcuts(ui: &mut egui::Ui, panel: &ui::DebuggerPanel, actions: &mut Vec<Action>) {
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        if let Some(id) = ui
            .memory(|m| m.focused())
            .filter(|_| ui.ctx().text_edit_focused())
        {
            ui.memory_mut(|m| m.surrender_focus(id));
        } else {
            actions.push(Action::CloseWorkspace);
        }
        return;
    }
    // Text fields and standard clipboard shortcuts own their keys. In
    // particular, typing an S or Ctrl/Cmd+C must never step the machine.
    if ui.ctx().text_edit_focused() || ui.input(|i| !i.modifiers.is_none()) {
        return;
    }
    for (key, control) in [
        (egui::Key::S, UiControl::DebugStep),
        (egui::Key::O, UiControl::DebugStepOver),
        (egui::Key::U, UiControl::DebugStepOut),
        (egui::Key::F, UiControl::DebugStepFrame),
        (egui::Key::L, UiControl::DebugRunLine),
        (egui::Key::C, UiControl::DebugCopperStep),
        (egui::Key::R, UiControl::DebugRun),
    ] {
        if ui.input(|i| i.events.iter().any(|event| matches!(event, egui::Event::Key { key: k, pressed: true, repeat: false, .. } if *k == key))) {
            actions.push(Action::Control(control));
        }
    }
    for (key, delta) in [
        (egui::Key::ArrowUp, -1),
        (egui::Key::ArrowDown, 1),
        (egui::Key::PageUp, -16),
        (egui::Key::PageDown, 16),
    ] {
        if ui.input(|i| i.key_pressed(key)) {
            match panel.tab {
                ui::DebugTab::Memory => actions.push(Action::MemoryScroll(delta)),
                ui::DebugTab::IoMap => actions.push(Action::IoMapScroll(if delta.abs() == 16 {
                    delta.signum() * 78
                } else {
                    delta
                })),
                _ => {}
            }
        }
    }
    if panel.tab == ui::DebugTab::IoMap {
        for (key, delta) in [(egui::Key::ArrowLeft, -26), (egui::Key::ArrowRight, 26)] {
            if ui.input(|i| i.key_pressed(key)) {
                actions.push(Action::IoMapScroll(delta));
            }
        }
    }
}

impl App {
    pub(super) fn egui_workspace_open(&self) -> bool {
        self.debugger_panel.is_some()
            || self.frame_analyzer_panel.is_some()
            || self.console_panel.is_some()
    }

    pub(super) fn egui_other_tool_pause(&self, kind: ToolPanelKind) -> Option<(bool, bool)> {
        ToolPanelKind::ALL.into_iter().find_map(|other| {
            if other == kind || !self.tool_panel_is_open(other) {
                return None;
            }
            let restore = match other {
                ToolPanelKind::Debugger => self.paused_before_debugger,
                ToolPanelKind::FrameAnalyzer => self.paused_before_analyzer,
                ToolPanelKind::Console => self.paused_before_console,
            };
            Some((self.paused, restore))
        })
    }

    pub(super) fn egui_remember_run_state(&mut self) {
        self.paused_before_debugger = self.paused;
        self.paused_before_analyzer = self.paused;
        self.paused_before_console = self.paused;
    }

    pub(super) fn egui_did_open_tool(
        &mut self,
        kind: ToolPanelKind,
        shared_pause: Option<(bool, bool)>,
    ) {
        if let Some((paused, resume_paused)) = shared_pause {
            self.paused = paused;
            self.paused_before_debugger = resume_paused;
            self.paused_before_analyzer = resume_paused;
            self.paused_before_console = resume_paused;
            self.sync_live_audio_suspension();
        }
        self.egui_selected_tool = kind;
        self.tool_window_front = Some(kind);
        self.enter_debug_workspace();
        self.request_redraw();
    }

    pub(super) fn dispatch_egui_frame(
        &mut self,
        actions: Vec<Action>,
        result: Result<(), pixels::Error>,
    ) {
        if let Err(error) = result {
            log::error!("debugger render: {error}");
        }
        // Input has already been consumed and the edited panels stored. A
        // presentation failure must not discard submitted commands or require
        // the user to repeat an action whose input is no longer in the queue.
        for action in actions {
            self.apply_egui_debugger_action(action);
        }
    }

    fn apply_egui_debugger_action(&mut self, action: Action) {
        match action {
            Action::SelectTool(ToolPanelKind::Debugger) => self.open_debugger(),
            Action::SelectTool(ToolPanelKind::FrameAnalyzer) => self.open_frame_analyzer(),
            Action::SelectTool(ToolPanelKind::Console) => self.open_console(),
            Action::CloseTool(kind) => self.close_tool_panel(kind),
            Action::FollowPc => {
                if let Some(panel) = &mut self.debugger_panel {
                    panel.disasm_addr = None;
                }
            }
            Action::FollowCopper => {
                if let Some(panel) = &mut self.debugger_panel {
                    panel.copper_addr = None;
                }
            }
            Action::Navigate(tab, address) => {
                self.open_debugger();
                let address = address & self.emu.machine.ui_addr_mask();
                let panel = self.debugger_panel.as_mut().unwrap();
                panel.tab = tab;
                panel.entry = format!("{address:08X}");
                panel.entry_active = false;
                if let Some(egui) = self.debugger_ui.as_mut() {
                    egui.layout.navigation = Some(tab);
                    egui.context.memory_mut(|memory| {
                        if let Some(id) = memory.focused() {
                            memory.surrender_focus(id);
                        }
                    });
                }
                match tab {
                    ui::DebugTab::Cpu => panel.disasm_addr = Some(address & !1),
                    ui::DebugTab::Memory => {
                        panel.mem_addr = address & !0xF;
                        panel.mem_view_bits = false;
                        panel.mem_last_find = None;
                    }
                    ui::DebugTab::Copper => panel.copper_addr = Some(address & !1),
                    _ => {}
                }
            }
            Action::ConsoleSubmit(text) => {
                for line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
                    let Some(panel) = &mut self.console_panel else {
                        break;
                    };
                    panel.input = line.to_owned();
                    self.console_submit();
                }
            }
            Action::CloseWorkspace => self.leave_debug_workspace(),
            Action::CaptureDisplay => self.capture_debug_guest_input(),
            Action::Analyzer(control) => {
                self.activate_tool_control(ToolPanelKind::FrameAnalyzer, control)
            }
            Action::AnalyzerKey(code) => {
                self.ui_handle_frame_analyzer_key(code);
            }
            Action::ResourceScroll(rows) => self.frame_analyzer_scroll_resources(rows),
            Action::BlitScroll(rows) => self.frame_analyzer_move_blit_selection(rows),
            Action::Control(control) => {
                self.activate_tool_control(ToolPanelKind::Debugger, control)
            }
            Action::SubmitEntry => {
                if let Some(panel) = &mut self.debugger_panel {
                    panel.entry_active = true;
                }
                self.ui_handle_debugger_key(KeyCode::Enter);
            }
            Action::MemoryScroll(rows) => {
                // The CPU's memory pane uses the same address/scroll rules as
                // the Memory tab; switch only for the shared action dispatch.
                if let Some(panel) = &mut self.debugger_panel {
                    let tab = panel.tab;
                    let bits = panel.mem_view_bits;
                    if tab == ui::DebugTab::Cpu {
                        panel.mem_view_bits = false;
                    }
                    panel.tab = ui::DebugTab::Memory;
                    self.debugger_mem_scroll(rows);
                    if let Some(panel) = &mut self.debugger_panel {
                        panel.tab = tab;
                        panel.mem_view_bits = bits;
                    }
                }
            }
            Action::IoMapScroll(rows) => self.debugger_iomap_move(rows),
        }
        self.request_redraw();
    }
}

mod analyzer;
mod console;
pub(super) mod preferences;

#[cfg(test)]
mod tests;

struct PreparedFrame {
    jobs: Vec<egui::ClippedPrimitive>,
    screen: egui_wgpu::ScreenDescriptor,
}

enum Snapshot {
    Debugger(Box<ui::DebuggerView>),
    Analyzer(Box<ui::FrameAnalyzerView>),
    Console,
}

pub(super) mod workspace;
