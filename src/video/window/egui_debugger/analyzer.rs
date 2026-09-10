// SPDX-License-Identifier: GPL-3.0-or-later

//! Frame Analyzer views on the shared egui surface. Captures, selection,
//! stepping and exports use the same handlers as the classic tool window.

use super::*;

fn command(
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
    label: &str,
    control: UiControl,
    enabled: bool,
) {
    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        actions.push(Action::Analyzer(control));
    }
}

fn selectable(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.add(
        egui::Label::new(RichText::new(text).monospace())
            .selectable(true)
            .wrap_mode(egui::TextWrapMode::Extend),
    );
}

fn argb(pixel: u32) -> Color32 {
    Color32::from_rgb((pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8)
}

/// Both native picking and tests use the protocol's 0..1023 beam fraction.
fn beam_fraction(rect: egui::Rect, point: egui::Pos2) -> [u16; 2] {
    let fraction = (point - rect.min) / rect.size();
    [
        (fraction.x * 1024.0).clamp(0.0, 1023.0) as u16,
        (fraction.y * 1024.0).clamp(0.0, 1023.0) as u16,
    ]
}

impl Layout {
    pub(super) fn analyzer(
        &mut self,
        root: &mut egui::Ui,
        panel: &mut ui::FrameAnalyzerPanel,
        view: &ui::FrameAnalyzerView,
        actions: &mut Vec<Action>,
    ) {
        if root.ctx().current_pass_index() == 0 {
            analyzer_shortcuts(root, actions);
        }
        egui::Panel::top("analyzer_tabs").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for tab in ui::ANALYZER_TABS {
                    if sub_tab(ui, ui::analyzer_tab_label(tab), panel.tab == tab).clicked() {
                        actions.push(Action::Analyzer(UiControl::AnalyzerTab(tab)));
                    }
                }
            });
        });
        egui::Panel::bottom("analyzer_transport").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                command(
                    ui,
                    actions,
                    if view.running { "Pause (R)" } else { "Run (R)" },
                    UiControl::AnalyzerRun,
                    true,
                );
                command(
                    ui,
                    actions,
                    "Capture frame (F)",
                    UiControl::AnalyzerFrame,
                    true,
                );
                if panel.tab == ui::AnalyzerTab::Beam {
                    command(
                        ui,
                        actions,
                        "Run to beam (T)",
                        UiControl::AnalyzerRunTo,
                        view.trace.is_some(),
                    );
                }
                if panel.tab == ui::AnalyzerTab::Resources {
                    command(
                        ui,
                        actions,
                        "Save resource…",
                        UiControl::AnalyzerResourceSave,
                        view.resources
                            .as_ref()
                            .is_some_and(|resources| resources.exportable),
                    );
                }
            });
        });
        egui::CentralPanel::default().show(root, |ui| {
            ScrollArea::both()
                .id_salt(format!("analyzer_{:?}", panel.tab))
                .auto_shrink([false, false])
                .show(ui, |ui| match panel.tab {
                    ui::AnalyzerTab::Beam => self.analyzer_beam(ui, panel, view, actions),
                    ui::AnalyzerTab::Memory => self.analyzer_memory(ui, panel, view, actions),
                    ui::AnalyzerTab::Blits => self.analyzer_blits(ui, view, actions),
                    ui::AnalyzerTab::Resources => self.analyzer_resources(ui, view, actions),
                });
        });
    }

    fn analyzer_beam(
        &mut self,
        ui: &mut egui::Ui,
        panel: &ui::FrameAnalyzerPanel,
        view: &ui::FrameAnalyzerView,
        actions: &mut Vec<Action>,
    ) {
        ui.horizontal_wrapped(|ui| {
            for (label, mut checked, control) in [
                (
                    "Picture (U)",
                    panel.show_underlay,
                    UiControl::AnalyzerUnderlay,
                ),
                ("Beam scrub (B)", panel.show_scrub, UiControl::AnalyzerScrub),
                (
                    "CPU waits (W)",
                    panel.show_cpu_wait,
                    UiControl::AnalyzerCpuWait,
                ),
            ] {
                if ui.checkbox(&mut checked, label).changed() {
                    actions.push(Action::Analyzer(control));
                }
            }
        });
        let Some(trace) = &view.trace else {
            ui.label(
                "Capture a frame to inspect chip-bus ownership, register writes and CPU waits.",
            );
            return;
        };
        if trace.rows == 0 || trace.cols == 0 {
            return;
        }
        selectable(
            ui,
            format!(
                "Frame {} · {:.3}s · {} lines × {} colour clocks{}",
                trace.frame,
                trace.seconds,
                trace.rows,
                trace.line_cck,
                if trace.partial { " · partial" } else { "" }
            ),
        );
        ui.label(
            "White: captured display · orange: DIW · cyan: DDF · left gutter: CPU stall share",
        );
        // A bidirectional scroll area's content can retain its old width
        // after a window resize. Size diagrams from the visible viewport.
        let width = (ui.clip_rect().width() - 250.0).clamp(320.0, 1200.0);
        let size = [trace.cols * 4, trace.rows];
        let pixels = (0..size[1])
            .flat_map(|y| {
                (0..size[0]).map(move |x| {
                    color(trace.raster_pixel(
                        view.underlay.as_ref(),
                        view.scrub,
                        panel.show_cpu_wait,
                        size,
                        [x, y],
                    ))
                })
            })
            .collect();
        let texture = self.texture(ui, "analyzer_beam".into(), size, pixels);
        let mut probe = (trace.selected_vpos, trace.selected_hpos);
        ui.horizontal_top(|ui| {
            ui.add_space(22.0);
            ui.vertical(|ui| {
                let display = egui::vec2(
                    width,
                    (width * analyzer_field_rows(trace.rows) as f32 / (trace.cols * 2) as f32)
                        .clamp(220.0, 420.0),
                );
                let response =
                    ui.add(egui::Image::new((texture, display)).sense(egui::Sense::hover()));
                let response = ui.interact(
                    response.rect,
                    egui::Id::new("analyzer_beam_pick"),
                    egui::Sense::click_and_drag(),
                );
                beam_overlays(ui, response.rect, trace);
                if let Some(point) = response
                    .hover_pos()
                    .or_else(|| response.interact_pointer_pos())
                {
                    let [x, y] = beam_fraction(response.rect, point);
                    probe = (
                        (usize::from(y) * analyzer_field_rows(trace.rows) / 1024)
                            .min(trace.rows.saturating_sub(1)),
                        usize::from(x) * trace.cols / 1024,
                    );
                    if response.clicked() || response.dragged() {
                        actions.push(Action::Analyzer(UiControl::AnalyzerPick {
                            x,
                            y,
                            scanline: false,
                        }));
                    }
                }
                ui.label(format!(
                    "Scanline {} · click or drag to select a colour clock",
                    trace.selected_vpos
                ));
                let pixels = (0..trace.cols)
                    .map(|x| {
                        color(trace.raster_pixel(
                            None,
                            false,
                            panel.show_cpu_wait,
                            [trace.cols, trace.rows],
                            [x, trace.selected_vpos],
                        ))
                    })
                    .collect();
                let strip = self.texture(ui, "analyzer_scanline".into(), [trace.cols, 1], pixels);
                let response = ui.add(
                    egui::Image::new((strip, egui::vec2(width, 28.0))).sense(egui::Sense::hover()),
                );
                let response = ui.interact(
                    response.rect,
                    egui::Id::new("analyzer_scanline_pick"),
                    egui::Sense::click_and_drag(),
                );
                let selected_x =
                    response.rect.left() + trace.selected_hpos as f32 / trace.cols as f32 * width;
                ui.painter().vline(
                    selected_x,
                    response.rect.y_range(),
                    Stroke::new(1.5, Color32::from_rgb(255, 160, 40)),
                );
                if response.clicked() || response.dragged() {
                    if let Some(point) = response.interact_pointer_pos() {
                        let [x, y] = beam_fraction(response.rect, point);
                        actions.push(Action::Analyzer(UiControl::AnalyzerPick {
                            x,
                            y,
                            scanline: true,
                        }));
                    }
                }
            });
            ui.vertical(|ui| {
                beam_counters(ui, trace, panel.show_cpu_wait, actions);
            });
        });
        ui.separator();
        selectable(
            ui,
            format!(
                "Selected v={:03} h={:03} · {} · CPU wait: {}{}",
                trace.selected_vpos,
                trace.selected_hpos,
                trace.selected_owner,
                ui::cpu_wait_name_for_code(trace.selected_cpu_wait_code),
                trace
                    .selected_blit
                    .as_ref()
                    .map(|s| format!(" · {s}"))
                    .unwrap_or_default()
            ),
        );
        selectable(ui, format!("Pointer v={:03} h={:03}", probe.0, probe.1));
        beam_detail(ui, trace, trace.selected_vpos, trace.selected_hpos, actions);
    }

    fn analyzer_memory(
        &mut self,
        ui: &mut egui::Ui,
        panel: &ui::FrameAnalyzerPanel,
        view: &ui::FrameAnalyzerView,
        actions: &mut Vec<Action>,
    ) {
        ui.horizontal_wrapped(|ui| {
            for (index, preset) in panel.heat_presets.iter().enumerate() {
                let selected = view
                    .heat
                    .as_ref()
                    .is_some_and(|heat| (heat.base, heat.span) == (preset.base, preset.span));
                if ui
                    .selectable_label(
                        selected,
                        RichText::new(&preset.label).color(if selected {
                            Color32::WHITE
                        } else {
                            INK
                        }),
                    )
                    .clicked()
                {
                    actions.push(Action::Analyzer(UiControl::AnalyzerHeatPreset(index as u8)));
                }
            }
        });
        let Some(heat) = &view.heat else {
            ui.label("The memory heat map is not armed.");
            return;
        };
        selectable(
            ui,
            format!(
                "Frame {} · ${:08X}–${:08X} · {} bytes/cell",
                heat.frame,
                heat.base,
                heat.base.saturating_add(heat.span.saturating_sub(1)),
                heat.bytes_per_cell
            ),
        );
        ui.label(format!(
            "Last access to each block, fading over {} frames",
            crate::heatmap::DECAY_FRAMES
        ));
        let height = (ui.clip_rect().bottom() - ui.cursor().top() - 80.0).max(256.0);
        let width = (ui.clip_rect().width() - 275.0)
            .clamp(256.0, 620.0)
            .min(height);
        let texture = self.texture(
            ui,
            "analyzer_memory".into(),
            [crate::heatmap::GRID; 2],
            heat.image.iter().map(|&p| argb(p)).collect(),
        );
        let mut hovered = None;
        ui.horizontal_top(|ui| {
            let response = ui.add(
                egui::Image::new((texture, egui::vec2(width, width))).sense(egui::Sense::hover()),
            );
            let response = ui.interact(
                response.rect,
                egui::Id::new("analyzer_heat_pick"),
                egui::Sense::click_and_drag(),
            );
            if let Some(cell) = panel.heat_selected {
                let pos = response.rect.min
                    + egui::vec2((cell % 256) as f32 + 0.5, (cell / 256) as f32 + 0.5)
                        * (width / 256.0);
                ui.painter().rect_stroke(
                    egui::Rect::from_center_size(pos, egui::vec2(7.0, 7.0)),
                    0.0,
                    Stroke::new(1.5, Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
            if let Some(point) = response
                .hover_pos()
                .or_else(|| response.interact_pointer_pos())
            {
                let [x, y] = beam_fraction(response.rect, point);
                let (x, y) = ((x / 4) as u8, (y / 4) as u8);
                hovered = Some(usize::from(y) * 256 + usize::from(x));
                if response.clicked() || response.dragged() {
                    actions.push(Action::Analyzer(UiControl::AnalyzerHeatPick { x, y }));
                }
            }
            ui.vertical(|ui| {
                ui.strong("Last access");
                egui::Grid::new("analyzer_census")
                    .striped(true)
                    .show(ui, |ui| {
                        for row in &heat.census {
                            swatch(ui, argb(row.colour));
                            ui.label(row.name);
                            ui.monospace(format!("{} cells", row.cells));
                            ui.monospace(format!("{} B", row.bytes));
                            ui.end_row();
                        }
                    });
            });
        });
        if let Some(cell) = hovered.or(panel.heat_selected) {
            selectable(
                ui,
                format!(
                    "{}{}",
                    ui::heat_cell_range(heat.base, heat.bytes_per_cell, cell),
                    ui::heat_resource_suffix(heat, cell)
                ),
            );
        } else {
            ui.label("Click a cell to inspect its address and last access.");
        }
        if let Some(selected) = &heat.selected {
            let address = heat
                .base
                .wrapping_add(selected.cell as u32 * heat.bytes_per_cell);
            address_link(
                ui,
                actions,
                ui::DebugTab::Memory,
                address,
                format!("Inspect memory at ${address:08X}"),
            );
            selectable(
                ui,
                format!(
                    "Pinned: {} · {}{}",
                    ui::heat_cell_range(heat.base, heat.bytes_per_cell, selected.cell),
                    selected.toucher.unwrap_or("untouched"),
                    selected
                        .age_frames
                        .map(|age| format!(" · {age} frames ago"))
                        .unwrap_or_default()
                ),
            );
        }
    }

    fn analyzer_blits(
        &mut self,
        ui: &mut egui::Ui,
        view: &ui::FrameAnalyzerView,
        actions: &mut Vec<Action>,
    ) {
        let Some(blits) = &view.blits else {
            ui.label(
                "No blits captured in this frame. Capture a frame containing blitter activity.",
            );
            return;
        };
        ui.horizontal(|ui| {
            if ui.button("Previous blit").clicked() {
                actions.push(Action::BlitScroll(-1));
            }
            if ui.button("Next blit").clicked() {
                actions.push(Action::BlitScroll(1));
            }
            ui.label(format!(
                "{} above · {} below",
                blits.hidden_above, blits.hidden_below
            ));
        });
        selectable(
            ui,
            "#  beam span        mode/channels    size       used/stall   destination",
        );
        for (i, row) in blits.rows.iter().enumerate() {
            if ui
                .selectable_label(
                    row.selected,
                    RichText::new(&row.text).monospace().color(if row.selected {
                        Color32::WHITE
                    } else {
                        INK
                    }),
                )
                .clicked()
            {
                actions.push(Action::Analyzer(UiControl::AnalyzerBlitRow(i as u8)));
            }
        }
        ui.separator();
        selectable(ui, &blits.formula);
        selectable(ui, &blits.detail);
        let width = (ui.clip_rect().width() - 16.0).max(400.0) / 2.0;
        ui.horizontal_top(|ui| {
            for (key, label, preview) in [
                ("blit_source", blits.source_label, &blits.source),
                ("blit_result", "Result / D", &blits.destination),
            ] {
                ui.vertical(|ui| {
                    ui.set_min_width(width);
                    ui.strong(label);
                    if let Some(preview) = preview {
                        self.bitmap_preview(ui, key, preview, width);
                    }
                });
            }
        });
    }

    fn analyzer_resources(
        &mut self,
        ui: &mut egui::Ui,
        view: &ui::FrameAnalyzerView,
        actions: &mut Vec<Action>,
    ) {
        let Some(resources) = &view.resources else {
            return;
        };
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    resources.hidden_above > 0,
                    egui::Button::new("Previous page"),
                )
                .clicked()
            {
                actions.push(Action::ResourceScroll(
                    -(ui::ANALYZER_RESOURCE_ROWS_MAX as isize),
                ));
            }
            if ui
                .add_enabled(resources.hidden_below > 0, egui::Button::new("Next page"))
                .clicked()
            {
                actions.push(Action::ResourceScroll(
                    ui::ANALYZER_RESOURCE_ROWS_MAX as isize,
                ));
            }
            ui.label(format!(
                "{} above · {} below",
                resources.hidden_above, resources.hidden_below
            ));
        });
        if resources.rows.is_empty() {
            ui.label("The guest has not registered any debug resources.");
        }
        for (i, row) in resources.rows.iter().enumerate() {
            if ui
                .selectable_label(
                    row.selected,
                    RichText::new(&row.text).monospace().color(if row.selected {
                        Color32::WHITE
                    } else {
                        INK
                    }),
                )
                .clicked()
            {
                actions.push(Action::Analyzer(UiControl::AnalyzerResourceRow(i as u8)));
            }
        }
        ui.separator();
        match &resources.detail {
            Some(ui::AnalyzerResourceDetail::Bitmap(preview)) => {
                self.bitmap_preview(ui, "resource_bitmap", preview, ui.clip_rect().width())
            }
            Some(ui::AnalyzerResourceDetail::Palette { colours }) => {
                ui.horizontal_wrapped(|ui| {
                    for (index, &pixel) in colours.iter().enumerate() {
                        ui.vertical(|ui| {
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(42.0, 26.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 0.0, color(pixel));
                            response.on_hover_text(format!(
                                "{index}: #{:02X}{:02X}{:02X}",
                                color(pixel).r(),
                                color(pixel).g(),
                                color(pixel).b()
                            ));
                            ui.monospace(index.to_string());
                        });
                    }
                });
            }
            Some(ui::AnalyzerResourceDetail::Copperlist { lines }) => {
                for line in lines {
                    selectable(ui, line);
                }
            }
            None if !resources.rows.is_empty() => {
                ui.label("Select a resource to inspect its contents.");
            }
            None => {}
        }
    }

    fn bitmap_preview(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        preview: &crate::video::resource_preview::BitmapPreview,
        width: f32,
    ) {
        let scale = (width / preview.width.max(1) as f32).clamp(0.1, 4.0);
        self.image(
            ui,
            key.into(),
            [preview.width, preview.height],
            preview.pixels.iter().map(|&p| color(p)).collect(),
            scale,
        );
        if let Some(note) = &preview.note {
            ui.label(note);
        }
    }
}

fn swatch(ui: &mut egui::Ui, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, colour);
}

fn beam_counters(
    ui: &mut egui::Ui,
    trace: &ui::AnalyzerTraceView,
    waits: bool,
    actions: &mut Vec<Action>,
) {
    ui.strong(if waits { "CPU waits" } else { "Bus ownership" });
    let total = trace.owner_cck.iter().sum::<u64>().max(1);
    let names = if waits {
        &crate::bus::CPU_WAIT_CLASS_NAMES
    } else {
        &crate::bus::CHIP_BUS_OWNER_NAMES
    };
    let counts = if waits {
        &trace.cpu_wait_by_class
    } else {
        &trace.owner_cck
    };
    let codes = if waits { *b"RBSDACLNp" } else { *b"RBSDACLP." };
    egui::Grid::new("analyzer_bus_counters")
        .striped(true)
        .show(ui, |ui| {
            for ((name, count), code) in names.iter().zip(counts).zip(codes) {
                swatch(
                    ui,
                    color(if waits {
                        ui::cpu_wait_color(code)
                    } else {
                        ui::owner_color(code)
                    }),
                );
                ui.label(*name);
                ui.monospace(format!("{count}"));
                ui.monospace(format!(
                    "{:.1}%",
                    *count as f64 * 100.0
                        / if waits {
                            trace.cpu_wait_cck.max(1)
                        } else {
                            total
                        } as f64
                ));
                ui.end_row();
            }
        });
    if waits {
        ui.label(format!(
            "{} clocks waiting ({:.1}%)",
            trace.cpu_wait_cck,
            trace.cpu_wait_percent()
        ));
        for (name, count) in crate::bus::CPU_BUS_ACCESS_KIND_NAMES
            .iter()
            .zip(trace.cpu_wait_by_kind)
        {
            selectable(ui, format!("{name}: {count}"));
        }
        ui.strong("Most stalled PCs");
        for (pc, count, symbol) in &trace.top_stalled_pcs {
            address_link(
                ui,
                actions,
                ui::DebugTab::Cpu,
                *pc,
                format!("${pc:08X}  {count} cck"),
            );
            if let Some(symbol) = symbol {
                ui.label(symbol);
            }
        }
    } else if trace.blitter_busy_cck > 0 {
        ui.label(format!(
            "Blitter grant: {:.1}%",
            trace.owner_cck[6] as f64 * 100.0 / trace.blitter_busy_cck as f64
        ));
        ui.strong("Blitter waits");
        for (name, count) in crate::bus::CHIP_BUS_OWNER_NAMES
            .iter()
            .zip(trace.blitter_starve_cck)
        {
            if count > 0 {
                selectable(ui, format!("{name}: {count} cck"));
            }
        }
    }
}

fn beam_overlays(ui: &egui::Ui, rect: egui::Rect, trace: &ui::AnalyzerTraceView) {
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    // The diagram is laid out against the long field, so the overlays have
    // to be placed against it too or they would drift by a line as the
    // fields alternate.
    let field_rows = analyzer_field_rows(trace.rows);
    let pos = |h: usize, v: usize| {
        rect.min
            + egui::vec2(
                h.min(trace.cols) as f32 / trace.cols as f32 * rect.width(),
                v.min(field_rows) as f32 / field_rows as f32 * rect.height(),
            )
    };
    let outline = |a, b, colour| {
        painter.rect_stroke(
            egui::Rect::from_min_max(a, b),
            0.0,
            Stroke::new(1.0, colour),
            egui::StrokeKind::Inside,
        );
    };
    outline(
        pos(
            trace.display_hpos_start as usize,
            trace.visible_start_vpos as usize,
        ),
        pos(
            trace.display_hpos_end as usize,
            trace.visible_start_vpos as usize + trace.visible_lines,
        ),
        Color32::WHITE,
    );
    if let Some((v0, v1)) = trace.diw_v {
        if let Some((h0, h1)) = trace.diw_h_cck {
            outline(
                pos(h0 as usize, v0 as usize),
                pos(h1 as usize, v1 as usize),
                Color32::from_rgb(245, 155, 45),
            );
        }
        if let Some((h0, h1)) = trace.ddf_cck {
            for h in [h0, h1] {
                painter.line_segment(
                    [pos(h as usize, v0 as usize), pos(h as usize, v1 as usize)],
                    Stroke::new(1.0, Color32::from_rgb(60, 210, 240)),
                );
            }
        }
    }
    for marker in &trace.markers {
        let p = pos(marker.hpos as usize, marker.vpos as usize);
        let stroke = Stroke::new(1.0, color(marker.colour));
        painter.line_segment([p - egui::vec2(2.0, 0.0), p + egui::vec2(2.0, 0.0)], stroke);
        painter.line_segment([p - egui::vec2(0.0, 2.0), p + egui::vec2(0.0, 2.0)], stroke);
    }
    painter.rect_stroke(
        egui::Rect::from_center_size(
            pos(trace.selected_hpos, trace.selected_vpos),
            egui::vec2(7.0, 7.0),
        ),
        0.0,
        Stroke::new(1.5, Color32::from_rgb(255, 160, 40)),
        egui::StrokeKind::Inside,
    );
    // One horizontal bar per captured line, beside the beam raster.
    for v in 0..trace.rows {
        let Some(row) = trace.cpu_wait_row(v) else {
            continue;
        };
        let waited = row.iter().filter(|&&code| code != b'.').count();
        if waited == 0 {
            continue;
        }
        let dominant = b"RBSDACLNp"
            .iter()
            .max_by_key(|&&code| row.iter().filter(|&&c| c == code).count())
            .copied()
            .unwrap_or(b'.');
        let y = rect.top() + v as f32 / trace.rows as f32 * rect.height();
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left() - 21.0, y),
            egui::vec2(
                19.0 * waited as f32 / trace.cols as f32,
                (rect.height() / trace.rows as f32).max(1.0),
            ),
        );
        ui.painter()
            .rect_filled(bar, 0.0, color(ui::cpu_wait_color(dominant)));
    }
}

fn beam_detail(
    ui: &mut egui::Ui,
    trace: &ui::AnalyzerTraceView,
    v: usize,
    h: usize,
    actions: &mut Vec<Action>,
) {
    selectable(
        ui,
        format!(
            "Inspect v={v:03} h={h:03} · {}",
            ui::owner_name_for_code(trace.owner_code_at(v, h))
        ),
    );
    if let Some(record) = trace.record_at(v, h) {
        if record.size > 0 {
            address_link(
                ui,
                actions,
                ui::DebugTab::Memory,
                record.addr,
                format!("Inspect memory at ${:08X}", record.addr),
            );
        }
        selectable(
            ui,
            format!(
                "reg=${:04X} addr=${:08X} data=${:016X}/{} kind={}:{} IPL={} events={}",
                record.reg,
                record.addr,
                record.data,
                record.size,
                record.kind,
                record.subtype,
                record.ipl,
                crate::bus::bus_event_names(record.events).join("|")
            ),
        );
        if record.kind == crate::bus::BUS_RECORD_COPPER {
            let address = if record.flags & 1 != 0 || record.subtype != 0 {
                record.addr.saturating_sub(2)
            } else {
                record.addr
            };
            address_link(
                ui,
                actions,
                ui::DebugTab::Copper,
                address,
                format!("Copper instruction @${address:06X}"),
            );
        }
    }
    for marker in trace
        .markers
        .iter()
        .filter(|marker| marker.near(v, h))
        .take(8)
    {
        selectable(ui, marker.label());
    }
}

fn analyzer_shortcuts(ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        actions.push(Action::CloseWorkspace);
        return;
    }
    if ui.ctx().text_edit_focused() || ui.input(|i| !i.modifiers.is_none()) {
        return;
    }
    for (key, code) in [
        (egui::Key::F, KeyCode::KeyF),
        (egui::Key::R, KeyCode::KeyR),
        (egui::Key::U, KeyCode::KeyU),
        (egui::Key::B, KeyCode::KeyB),
        (egui::Key::W, KeyCode::KeyW),
        (egui::Key::T, KeyCode::KeyT),
        (egui::Key::M, KeyCode::KeyM),
        (egui::Key::ArrowUp, KeyCode::ArrowUp),
        (egui::Key::ArrowDown, KeyCode::ArrowDown),
        (egui::Key::ArrowLeft, KeyCode::ArrowLeft),
        (egui::Key::ArrowRight, KeyCode::ArrowRight),
        (egui::Key::PageUp, KeyCode::PageUp),
        (egui::Key::PageDown, KeyCode::PageDown),
    ] {
        let navigation = matches!(
            key,
            egui::Key::ArrowUp
                | egui::Key::ArrowDown
                | egui::Key::ArrowLeft
                | egui::Key::ArrowRight
                | egui::Key::PageUp
                | egui::Key::PageDown
        );
        if ui.input(|i| i.events.iter().any(|event| matches!(event, egui::Event::Key { key: k, pressed: true, repeat, .. } if *k == key && (navigation || !repeat)))) { actions.push(Action::AnalyzerKey(code)); }
    }
}

/// Address navigation never runs the guest or alters a captured frame.
fn address_link(
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
    tab: ui::DebugTab,
    address: u32,
    label: String,
) {
    let destination = match tab {
        ui::DebugTab::Cpu => "CPU disassembly",
        ui::DebugTab::Copper => "Copper list",
        _ => "memory",
    };
    ui.push_id(("analyzer_address", destination, address), |ui| {
        if ui.link(RichText::new(label).monospace().color(BLUE)).on_hover_text(format!("Open {destination} at this address in the current machine; the captured frame stays selected")).clicked() {
            actions.push(Action::Navigate(tab, address));
        }
    });
}
