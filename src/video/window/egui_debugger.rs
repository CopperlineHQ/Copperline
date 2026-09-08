// SPDX-License-Identifier: GPL-3.0-or-later

//! Opt-in debugger frontend. All machine reads arrive as a snapshot and all
//! commands leave as actions, after egui's (potentially repeated) layout pass.
//! `pixels` still owns the surface; its unused backing texture is just 1x1.

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
    SubmitEntry,
    MemoryScroll(i32),
    IoMapScroll(i32),
}

enum Content<'a> {
    Debugger(&'a mut ui::DebuggerPanel, &'a ui::DebuggerView),
    Analyzer(&'a mut ui::FrameAnalyzerPanel, &'a ui::FrameAnalyzerView),
}

pub(super) struct DebuggerUi {
    context: egui::Context,
    input: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    layout: Layout,
    repaint_at: Option<Instant>,
}

#[derive(Default)]
struct Layout {
    images: HashMap<String, (egui::ColorImage, egui::TextureHandle)>,
}

impl DebuggerUi {
    pub(super) fn new(
        window: &Window,
        pixels: &mut pixels::Pixels<'_>,
    ) -> Result<Self, pixels::TextureError> {
        pixels.resize_buffer(1, 1)?;
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
        Ok(Self {
            context,
            input,
            renderer,
            layout: Layout::default(),
            repaint_at: None,
        })
    }

    pub(super) fn on_event(&mut self, window: &Window, event: &WindowEvent) {
        if self.input.on_window_event(window, event).repaint {
            window.request_redraw();
        }
    }

    pub(super) fn repaint_due(&self) -> bool {
        self.repaint_at.is_some_and(|at| Instant::now() >= at)
    }

    fn draw(
        &mut self,
        window: &Window,
        pixels: &pixels::Pixels<'_>,
        content: Content<'_>,
    ) -> Result<Vec<Action>, pixels::Error> {
        let input = self.input.take_egui_input(window);
        let (output, actions) = run_content_frame(&self.context, &mut self.layout, input, content);
        self.input
            .handle_platform_output(window, output.platform_output);
        self.repaint_at = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .and_then(|viewport| Instant::now().checked_add(viewport.repaint_delay));
        let jobs = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        let size = window.inner_size();
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size.width.max(1), size.height.max(1)],
            pixels_per_point: output.pixels_per_point,
        };
        for (id, delta) in &output.textures_delta.set {
            self.renderer
                .update_texture(pixels.device(), pixels.queue(), *id, delta);
        }
        let result = pixels.render_with(|encoder, target, gpu| {
            paint(
                &mut self.renderer,
                &gpu.device,
                &gpu.queue,
                encoder,
                target,
                &jobs,
                &screen,
            );
            Ok(())
        });
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        result.map(|()| actions)
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
            Content::Debugger(_, view) => (ToolPanelKind::Debugger, &view.status),
            Content::Analyzer(_, view) => (ToolPanelKind::FrameAnalyzer, &view.status),
        };
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
        egui::Panel::top("workspace_tools").show(root, |ui| {
            ui.horizontal(|ui| {
                for (label, kind) in [
                    ("Debugger", ToolPanelKind::Debugger),
                    ("Frame Analyzer", ToolPanelKind::FrameAnalyzer),
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
                        actions.push(if selected == ToolPanelKind::FrameAnalyzer {
                            Action::Analyzer(UiControl::PanelClose)
                        } else {
                            Action::Control(UiControl::PanelClose)
                        });
                    }
                });
            });
        });
        match &mut content {
            Content::Debugger(panel, view) => layout.show(root, panel, view, &mut actions),
            Content::Analyzer(panel, view) => layout.analyzer(root, panel, view, &mut actions),
        }
    });
    // Input can be consumed on the first of several layout passes. Keep its
    // edits and commands, but dispatch each command only once per UI frame.
    let mut unique = Vec::new();
    for action in actions {
        if !unique.contains(&action) {
            unique.push(action);
        }
    }
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
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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
            ScrollArea::both()
                .id_salt(format!("debugger_{:?}", panel.tab))
                .auto_shrink([false, false])
                .show(ui, |ui| match panel.tab {
                    ui::DebugTab::Video => {
                        if let Some(video) = &view.video {
                            self.video(ui, video, actions);
                        }
                    }
                    ui::DebugTab::Audio => {
                        if let Some(audio) = &view.audio {
                            self.audio(ui, audio, actions);
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
            .default_size(235.0)
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
            .default_size(190.0)
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
                    panel.disasm_addr = None;
                }
            });
            ScrollArea::both()
                .id_salt("cpu_disassembly_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    lines(ui, &cpu.disassembly);
                });
        });
    }

    fn tab_controls(
        &self,
        ui: &mut egui::Ui,
        panel: &ui::DebuggerPanel,
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

    fn audio(&self, ui: &mut egui::Ui, audio: &ui::AudioScopeView, actions: &mut Vec<Action>) {
        ui.monospace(&audio.header);
        for (i, row) in audio
            .channels
            .iter()
            .chain(audio.extras.iter().map(|extra| &extra.row))
            .enumerate()
        {
            ui.separator();
            ui.horizontal(|ui| {
                let mut muted = row.muted;
                if ui.checkbox(&mut muted, "Mute").changed() {
                    actions.push(Action::Control(UiControl::DebugAudioMute(i)));
                }
                ui.vertical(|ui| {
                    lines(ui, &row.text);
                });
            });
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width().clamp(200.0, 1200.0), 70.0),
                egui::Sense::hover(),
            );
            ui.painter().rect_filled(rect, 0.0, Color32::from_gray(25));
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
        self.debugger_panel.is_some() || self.frame_analyzer_panel.is_some()
    }

    pub(super) fn egui_other_tool_pause(&self, kind: ToolPanelKind) -> Option<(bool, bool)> {
        match kind {
            ToolPanelKind::Debugger if self.frame_analyzer_panel.is_some() => {
                Some((self.paused, self.paused_before_analyzer))
            }
            ToolPanelKind::FrameAnalyzer if self.debugger_panel.is_some() => {
                Some((self.paused, self.paused_before_debugger))
            }
            _ => None,
        }
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
            self.sync_live_audio_suspension();
        }
        self.egui_selected_tool = kind;
        self.tool_window_front = Some(kind);
        if let Some(tool) = &self.debugger_tool_window {
            tool.window.focus_window();
        }
        self.request_redraw();
    }

    fn close_egui_workspace(&mut self) {
        self.close_tool_panel(ToolPanelKind::FrameAnalyzer);
        self.close_tool_panel(ToolPanelKind::Debugger);
    }

    pub(super) fn schedule_egui_debugger_repaint(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) {
        let Some(tool) = &mut self.debugger_tool_window else {
            return;
        };
        if tool.minimized {
            return;
        }
        let Some(egui) = &mut tool.egui else {
            return;
        };
        let Some(at) = egui.repaint_at else {
            return;
        };
        if at <= Instant::now() {
            egui.repaint_at = None;
            tool.window.request_redraw();
        } else if matches!(
            event_loop.control_flow(),
            winit::event_loop::ControlFlow::Wait
        ) {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(at));
        }
    }

    pub(super) fn handle_egui_debugger_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        event: WindowEvent,
    ) {
        if matches!(
            event,
            WindowEvent::Focused(true) | WindowEvent::KeyboardInput { .. }
        ) {
            self.tool_window_front = Some(self.egui_selected_tool);
        }
        if let Some(tool) = &mut self.debugger_tool_window {
            if let Some(egui) = &mut tool.egui {
                egui.on_event(&tool.window, &event);
            }
        }
        match event {
            WindowEvent::CloseRequested => self.close_egui_workspace(),
            WindowEvent::RedrawRequested => self.draw_egui_debugger(),
            WindowEvent::Resized(size) => {
                self.apply_tool_surface_size(ToolPanelKind::Debugger, size)
            }
            WindowEvent::ScaleFactorChanged { .. } => self.request_redraw(),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.update_host_modifiers(modifiers.state())
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == winit::event::ElementState::Pressed
                    && event.physical_key == winit::keyboard::PhysicalKey::Code(KeyCode::KeyQ)
                    && super::host_shortcut_modifier_pressed(self.modifiers) =>
            {
                event_loop.exit()
            }
            _ => {}
        }
    }

    pub(super) fn draw_egui_debugger(&mut self) {
        self.resync_tool_surface_size(ToolPanelKind::Debugger);
        if self
            .debugger_tool_window
            .as_ref()
            .is_none_or(|tool| tool.minimized)
        {
            return;
        }
        let result = if self.egui_selected_tool == ToolPanelKind::FrameAnalyzer {
            self.ensure_analyzer_underlay();
            let Some(mut panel) = self.frame_analyzer_panel.clone() else {
                return;
            };
            let view = self.build_frame_analyzer_view(&panel);
            let tool = self.debugger_tool_window.as_mut().unwrap();
            let Some(egui) = &mut tool.egui else {
                return;
            };
            let result = egui.draw(
                &tool.window,
                &tool.pixels,
                Content::Analyzer(&mut panel, &view),
            );
            self.frame_analyzer_panel = Some(panel);
            result
        } else {
            let Some(mut panel) = self.debugger_panel.clone() else {
                return;
            };
            let view = self.build_debugger_view_with_clipping(&panel, false);
            let tool = self.debugger_tool_window.as_mut().unwrap();
            let Some(egui) = &mut tool.egui else {
                return;
            };
            let result = egui.draw(
                &tool.window,
                &tool.pixels,
                Content::Debugger(&mut panel, &view),
            );
            self.debugger_panel = Some(panel);
            result
        };
        match result {
            Ok(actions) => {
                for action in actions {
                    self.apply_egui_debugger_action(action);
                }
            }
            Err(error) => log::error!("debugger render: {error}"),
        }
    }

    fn apply_egui_debugger_action(&mut self, action: Action) {
        match action {
            Action::SelectTool(ToolPanelKind::Debugger) => self.open_debugger(),
            Action::SelectTool(ToolPanelKind::FrameAnalyzer) => self.open_frame_analyzer(),
            Action::SelectTool(ToolPanelKind::Console) => {}
            Action::CloseWorkspace => self.close_egui_workspace(),
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

#[cfg(test)]
mod tests;
