// SPDX-License-Identifier: GPL-3.0-or-later

//! Debugger and frame-analyzer tool windows: transport, breakpoints, memory editing, view data.

use super::*;

impl App {
    pub(super) fn ui_handle_debugger_key(&mut self, code: KeyCode) -> bool {
        let Some(panel) = self.debugger_panel.as_mut() else {
            return false;
        };
        if panel.entry_active {
            if let Some(ch) = entry_char_for_key(code) {
                panel.push_entry_char(ch);
                self.request_redraw();
                return true;
            }
            match code {
                KeyCode::Backspace => {
                    panel.backspace_entry();
                    self.request_redraw();
                    return true;
                }
                KeyCode::Enter | KeyCode::NumpadEnter => {
                    match panel.tab {
                        ui::DebugTab::Memory => {
                            if let Some(addr) = panel.entry_addr() {
                                panel.mem_addr = addr & !0xF;
                            }
                        }
                        // The IO Map takes an offset (96) or address (DFF096).
                        ui::DebugTab::IoMap => {
                            if let Some(addr) = panel.entry_addr() {
                                panel.iomap_sel = (addr as u16) & 0x1FE;
                            }
                        }
                        // On the CPU tab, Enter pins the disassembly to
                        // the typed address; an empty box follows the PC again.
                        ui::DebugTab::Cpu => panel.disasm_addr = panel.entry_addr(),
                        _ => {}
                    }
                    panel.entry_active = false;
                    self.request_redraw();
                    return true;
                }
                _ => {}
            }
        }
        if panel.entry_active {
            return false;
        }
        let control = match code {
            KeyCode::KeyS => Some(UiControl::DebugStep),
            KeyCode::KeyO => Some(UiControl::DebugStepOver),
            KeyCode::KeyU => Some(UiControl::DebugStepOut),
            KeyCode::KeyF => Some(UiControl::DebugStepFrame),
            KeyCode::KeyL => Some(UiControl::DebugRunLine),
            KeyCode::KeyC => Some(UiControl::DebugCopperStep),
            KeyCode::KeyR => Some(UiControl::DebugRun),
            _ => None,
        };
        if let Some(control) = control {
            self.activate_tool_control(ToolPanelKind::Debugger, control);
            return true;
        }
        // Memory tab: cursor/page keys scroll the hex or bitmap view.
        if self
            .debugger_panel
            .as_ref()
            .is_some_and(|panel| panel.tab == ui::DebugTab::Memory)
        {
            let rows = match code {
                KeyCode::ArrowUp => Some(-1),
                KeyCode::ArrowDown => Some(1),
                KeyCode::PageUp => Some(-16),
                KeyCode::PageDown => Some(16),
                _ => None,
            };
            if let Some(rows) = rows {
                self.debugger_mem_scroll(rows);
                return true;
            }
        }
        // IO Map tab: arrows move the register selection (left/right by
        // a display column), PageUp/Down by a page.
        if self
            .debugger_panel
            .as_ref()
            .is_some_and(|panel| panel.tab == ui::DebugTab::IoMap)
        {
            let delta = match code {
                KeyCode::ArrowUp => Some(-1i32),
                KeyCode::ArrowDown => Some(1),
                KeyCode::ArrowLeft => Some(-26),
                KeyCode::ArrowRight => Some(26),
                KeyCode::PageUp => Some(-78),
                KeyCode::PageDown => Some(78),
                _ => None,
            };
            if let Some(delta) = delta {
                self.debugger_iomap_move(delta);
                return true;
            }
        }
        false
    }

    /// Move the IO Map selection by `delta` registers, clamped to the
    /// custom bank.
    pub(super) fn debugger_iomap_move(&mut self, delta: i32) {
        if let Some(panel) = self.debugger_panel.as_mut() {
            let idx = i32::from(panel.iomap_sel >> 1) + delta;
            panel.iomap_sel = (idx.clamp(0, 255) as u16) << 1;
            self.request_redraw();
        }
    }

    pub(super) fn ui_handle_frame_analyzer_key(&mut self, code: KeyCode) -> bool {
        if self.frame_analyzer_panel.is_none() {
            return false;
        }
        let memory_tab = self
            .frame_analyzer_panel
            .as_ref()
            .is_some_and(|panel| panel.tab == ui::AnalyzerTab::Memory);
        let control = match code {
            KeyCode::KeyF => Some(UiControl::AnalyzerFrame),
            KeyCode::KeyR => Some(UiControl::AnalyzerRun),
            KeyCode::KeyU => Some(UiControl::AnalyzerUnderlay),
            KeyCode::KeyB => Some(UiControl::AnalyzerScrub),
            KeyCode::KeyW => Some(UiControl::AnalyzerCpuWait),
            KeyCode::KeyT => Some(UiControl::AnalyzerRunTo),
            // One key flips between the two views of the traced machine.
            KeyCode::KeyM => Some(UiControl::AnalyzerTab(if memory_tab {
                ui::AnalyzerTab::Beam
            } else {
                ui::AnalyzerTab::Memory
            })),
            _ => None,
        };
        if let Some(control) = control {
            self.activate_tool_control(ToolPanelKind::FrameAnalyzer, control);
            return true;
        }
        // Resources tab: cursor/page keys scroll the table, so every
        // registry entry stays reachable however many are registered.
        if self
            .frame_analyzer_panel
            .as_ref()
            .is_some_and(|panel| panel.tab == ui::AnalyzerTab::Resources)
        {
            let rows = match code {
                KeyCode::ArrowUp => Some(-1isize),
                KeyCode::ArrowDown => Some(1),
                KeyCode::PageUp => Some(-(ui::ANALYZER_RESOURCE_ROWS_MAX as isize)),
                KeyCode::PageDown => Some(ui::ANALYZER_RESOURCE_ROWS_MAX as isize),
                _ => None,
            };
            if let Some(rows) = rows {
                self.frame_analyzer_scroll_resources(rows);
                return true;
            }
        }
        let delta = match code {
            KeyCode::ArrowLeft => Some((-1, 0)),
            KeyCode::ArrowRight => Some((1, 0)),
            KeyCode::ArrowUp => Some((0, -1)),
            KeyCode::ArrowDown => Some((0, 1)),
            _ => None,
        };
        if let Some((dx, dy)) = delta {
            // The arrows nudge whichever selection the visible tab has: a
            // beam slot on the Beam tab, a grid cell on the Memory tab.
            if memory_tab {
                self.frame_analyzer_move_heat_selection(dx, dy);
            } else {
                self.frame_analyzer_move_selection(dx, dy);
            }
            return true;
        }
        false
    }

    /// Open the debugger window (pausing the machine), or close it again
    /// if it is already open (the host shortcut toggle).
    pub(super) fn toggle_debugger(&mut self) {
        if self.debugger_panel.is_some() {
            self.close_tool_panel(ToolPanelKind::Debugger);
        } else {
            self.ui.menu_open = false;
            self.open_debugger();
            self.request_redraw();
        }
    }

    pub(super) fn open_debugger(&mut self) {
        if self.debugger_panel.is_none() {
            // The debugger shortcut can arrive while the mouse is captured;
            // release it so the window's controls are reachable, and note
            // it so closing the panel gives the capture back.
            self.suspend_mouse_capture_for_ui();
            self.ui.panel = None;
            self.paused_before_debugger = self.paused;
            self.paused = true;
            self.sync_live_audio_suspension();
            let mut panel = ui::DebuggerPanel::new();
            // Start the memory view at the current program counter's
            // neighbourhood; it is usually what you came to look at.
            panel.mem_addr = self.emu.machine.pc() & self.emu.machine.ui_addr_mask() & !0xF;
            self.debugger_panel = Some(panel);
            self.emu.machine.ui_set_pc_history_enabled(true);
            // Arm reverse debugging so the < Step / < Run controls work. A
            // conservative interval keeps the per-snapshot serialize off the
            // critical path; captures only accrue while the machine advances
            // (Run / Step Frame inside the debugger), not while paused.
            if !self.emu.time_travel_enabled() {
                self.emu.enable_time_travel(
                    crate::debugger::RR_DEFAULT_BUDGET_MB,
                    DEBUGGER_REVERSE_INTERVAL_FRAMES,
                );
            }
        }
    }

    /// Open the console window (pausing the machine), or close it again
    /// if it is already open (the host shortcut toggle).
    pub(super) fn toggle_console(&mut self) {
        if self.console_panel.is_some() {
            self.close_tool_panel(ToolPanelKind::Console);
        } else {
            self.ui.menu_open = false;
            self.open_console();
            self.request_redraw();
        }
    }

    pub(super) fn open_console(&mut self) {
        if self.console_panel.is_none() {
            self.suspend_mouse_capture_for_ui();
            self.ui.panel = None;
            self.paused_before_console = self.paused;
            self.paused = true;
            self.sync_live_audio_suspension();
            let mut panel = ui::ConsolePanel::default();
            panel.push_output("Copperline debugger console. Type HELP for commands.");
            self.console_panel = Some(panel);
            self.emu.machine.ui_set_pc_history_enabled(true);
            // Arm reverse debugging so the reverse commands work, exactly
            // like opening the debugger window does.
            if !self.emu.time_travel_enabled() {
                self.emu.enable_time_travel(
                    crate::debugger::RR_DEFAULT_BUDGET_MB,
                    DEBUGGER_REVERSE_INTERVAL_FRAMES,
                );
            }
        }
    }

    pub(super) fn open_frame_analyzer(&mut self) {
        if self.frame_analyzer_panel.is_none() {
            self.suspend_mouse_capture_for_ui();
            self.ui.panel = None;
            self.paused_before_analyzer = self.paused;
            self.paused = true;
            self.sync_live_audio_suspension();
            self.emu.bus_mut().set_frame_analyzer_full(true);
            self.frame_analyzer_panel = Some(ui::FrameAnalyzerPanel::new());
        }
    }

    pub(super) fn frame_analyzer_toggle_run(&mut self) {
        self.paused = !self.paused;
        self.paused_before_analyzer = self.paused;
        self.sync_live_audio_suspension();
        if !self.paused {
            self.emu.bus_mut().set_frame_analyzer_full(true);
        }
    }

    pub(super) fn frame_analyzer_step_frame(&mut self) {
        self.emu.bus_mut().set_frame_analyzer_full(true);
        self.debugger_step_frame();
    }

    /// Switch the analyzer to `tab`. Entering the Memory tab rebuilds the
    /// window presets from the machine's memory map and arms the heat map
    /// over chip RAM if nothing has armed it yet, so the tab always opens
    /// on a map that is recording. Leaving the tab deliberately does not
    /// disarm it: flipping tabs would otherwise wipe the recording.
    pub(super) fn frame_analyzer_set_tab(&mut self, tab: ui::AnalyzerTab) {
        if self.frame_analyzer_panel.is_none() {
            return;
        }
        let presets = (tab == ui::AnalyzerTab::Memory).then(|| {
            if self.emu.bus().heat_map().is_none() {
                let window = analyzer_default_heat_window(self.emu.bus());
                self.emu.bus_mut().set_heat_map(Some(window));
                self.heatmap_armed_by_panel = true;
            }
            analyzer_heat_presets(self.emu.bus())
        });
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.tab = tab;
            if let Some(presets) = presets {
                panel.heat_presets = presets;
            }
        }
        self.request_redraw();
    }

    /// Select a Resources-tab row. The index maps through the live
    /// registry (the same order the rows were drawn from); one past the
    /// end does nothing, and the selection is kept by address so registry
    /// churn cannot re-point it at a different resource.
    pub(super) fn frame_analyzer_select_resource(&mut self, index: u8) {
        let resources = self.emu.uaelib_resources();
        let scroll = self
            .frame_analyzer_panel
            .as_ref()
            .map(|panel| {
                panel.resource_scroll.min(
                    resources
                        .len()
                        .saturating_sub(ui::ANALYZER_RESOURCE_ROWS_MAX),
                )
            })
            .unwrap_or(0);
        let Some(address) = resources
            .get(scroll + usize::from(index))
            .map(|resource| resource.address)
        else {
            return;
        };
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.resource_selected = Some(address);
        }
        self.request_redraw();
    }

    /// Scroll the Resources tab's table window by `rows`, clamped to the
    /// registry.
    pub(super) fn frame_analyzer_scroll_resources(&mut self, rows: isize) {
        let max_scroll = self
            .emu
            .uaelib_resources()
            .len()
            .saturating_sub(ui::ANALYZER_RESOURCE_ROWS_MAX) as isize;
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            let scroll = (panel.resource_scroll as isize)
                .saturating_add(rows)
                .clamp(0, max_scroll);
            panel.resource_scroll = scroll as usize;
        }
        self.request_redraw();
    }

    /// The Resources tab's table and the selected resource's preview,
    /// decoded fresh from guest memory each build so the pane tracks what
    /// the program draws (crate::video::resource_preview).
    pub(super) fn build_analyzer_resources_view(
        &self,
        panel: &ui::FrameAnalyzerPanel,
    ) -> ui::AnalyzerResourcesView {
        use crate::uaelib::{
            ResourceKind, RESOURCE_FLAG_HAM, RESOURCE_FLAG_INTERLEAVED, RESOURCE_FLAG_MASKED,
        };
        use crate::video::resource_preview;

        let resources = self.emu.uaelib_resources();
        // The scroll clamps to the registry as it is NOW, so a shrunk
        // registry cannot leave the window past the end.
        let scroll = panel.resource_scroll.min(
            resources
                .len()
                .saturating_sub(ui::ANALYZER_RESOURCE_ROWS_MAX),
        );
        let rows = resources
            .iter()
            .skip(scroll)
            .take(ui::ANALYZER_RESOURCE_ROWS_MAX)
            .map(|r| {
                // Character-wise: a byte truncate can panic inside a
                // multibyte character of a lossily-decoded guest name.
                let name: String = r.name.chars().take(12).collect();
                let geometry = match r.kind {
                    ResourceKind::Bitmap {
                        width,
                        height,
                        planes,
                    } => {
                        let mut text = format!("{width}x{height}x{planes}");
                        if r.flags & RESOURCE_FLAG_INTERLEAVED != 0 {
                            text.push_str(" ilv");
                        }
                        if r.flags & RESOURCE_FLAG_MASKED != 0 {
                            text.push_str(" msk");
                        }
                        if r.flags & RESOURCE_FLAG_HAM != 0 {
                            text.push_str(" ham");
                        }
                        text
                    }
                    ResourceKind::Palette { entries } => format!("{entries} entries"),
                    ResourceKind::Copperlist => String::new(),
                    ResourceKind::Unknown(code) => format!("type {code}"),
                };
                ui::AnalyzerResourceRowView {
                    text: format!(
                        "{name:<12} {kind:<10} ${addr:06X}  {size:>8}  {geometry}",
                        kind = r.kind_name(),
                        addr = r.address,
                        size = r.size,
                    ),
                    selected: panel.resource_selected == Some(r.address),
                }
            })
            .collect();
        let hidden_above = scroll;
        let hidden_below = resources
            .len()
            .saturating_sub(scroll + ui::ANALYZER_RESOURCE_ROWS_MAX);

        let detail = panel
            .resource_selected
            .and_then(|address| resources.iter().find(|r| r.address == address))
            .and_then(|r| match r.kind {
                ResourceKind::Bitmap {
                    width,
                    height,
                    planes,
                } => {
                    let len = (r.size.max(1) as usize).min(512 * 1024);
                    let data = self.emu.machine.debug_read_memory(r.address, len);
                    let palette = self.analyzer_resource_palette();
                    Some(ui::AnalyzerResourceDetail::Bitmap(
                        resource_preview::decode_bitmap(
                            &data,
                            width,
                            height,
                            planes,
                            r.flags & RESOURCE_FLAG_INTERLEAVED != 0,
                            r.flags & RESOURCE_FLAG_MASKED != 0,
                            r.flags & RESOURCE_FLAG_HAM != 0,
                            &palette,
                        ),
                    ))
                }
                ResourceKind::Palette { entries } => {
                    let data = self
                        .emu
                        .machine
                        .debug_read_memory(r.address, usize::from(entries) * 2);
                    Some(ui::AnalyzerResourceDetail::Palette {
                        colours: resource_preview::decode_palette_words(&data, entries),
                    })
                }
                ResourceKind::Copperlist => {
                    let bus = self.emu.bus();
                    let lines =
                        crate::disasm::dump_copper_list(|a| bus.peek_word_any(a), r.address, 12)
                            .into_iter()
                            .map(|(addr, text)| format!("{addr:06X}: {text}"))
                            .collect();
                    Some(ui::AnalyzerResourceDetail::Copperlist { lines })
                }
                ResourceKind::Unknown(_) => None,
            });
        let exportable = panel.resource_selected.is_some_and(|address| {
            resources.iter().any(|resource| {
                resource.address == address
                    && matches!(
                        resource.kind,
                        ResourceKind::Bitmap { .. } | ResourceKind::Palette { .. }
                    )
            })
        });

        ui::AnalyzerResourcesView {
            rows,
            hidden_above,
            hidden_below,
            detail,
            exportable,
        }
    }

    /// Save the selected guest bitmap/palette with the protocol export path.
    pub(super) fn frame_analyzer_save_resource(&mut self) {
        let Some(resource) = self
            .frame_analyzer_panel
            .as_ref()
            .and_then(|panel| panel.resource_selected)
            .and_then(|address| {
                self.emu
                    .uaelib_resources()
                    .iter()
                    .find(|resource| resource.address == address)
                    .cloned()
            })
        else {
            self.show_osd("Select a bitmap or palette resource first");
            return;
        };
        if !matches!(
            resource.kind,
            crate::uaelib::ResourceKind::Bitmap { .. }
                | crate::uaelib::ResourceKind::Palette { .. }
        ) {
            self.show_osd("Only bitmap and palette resources export as PNG");
            return;
        }
        let name: String = resource
            .name
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Save debug resource")
            .set_file_name(format!(
                "{}.png",
                if name.is_empty() { "resource" } else { &name }
            ))
            .add_filter("PNG image", &["png"])
            .save_file();
        if let Some(path) = picked {
            match self.emu.export_uaelib_resource(resource.address, &path) {
                Ok(_) => self.show_osd(format!("Saved {}", display_file_name(&path))),
                Err(error) => {
                    warn!("resource export failed ({}): {error:#}", path.display());
                    self.show_osd("Resource export failed (see log)");
                }
            }
        }
        self.finish_host_io_pause();
    }

    /// The palette a bitmap preview is decoded with: the first palette
    /// the guest registered (read from guest memory), else the live
    /// Denise palette -- the same source the Video tab shows.
    fn analyzer_resource_palette(&self) -> Vec<u32> {
        if let Some((address, entries)) =
            self.emu
                .uaelib_resources()
                .iter()
                .find_map(|r| match r.kind {
                    crate::uaelib::ResourceKind::Palette { entries } => Some((r.address, entries)),
                    _ => None,
                })
        {
            let data = self
                .emu
                .machine
                .debug_read_memory(address, usize::from(entries) * 2);
            return crate::video::resource_preview::decode_palette_words(&data, entries);
        }
        let palette = &self.emu.bus().denise.palette;
        (0..32)
            .map(|entry| crate::chipset::denise::rgb24_to_rgba8(palette.rgb24(entry)))
            .collect()
    }

    /// Point the heat map at preset `index`. An index past the end does
    /// nothing: a click can land after the preset list was rebuilt from a
    /// machine with fewer banks.
    pub(super) fn frame_analyzer_heat_preset(&mut self, index: u8) {
        let Some(window) = self
            .frame_analyzer_panel
            .as_ref()
            .and_then(|panel| panel.heat_presets.get(usize::from(index)))
            .map(|preset| (preset.base, preset.span))
        else {
            return;
        };
        // Ownership follows the arming, not the window. Re-windowing an
        // already-armed map is shared control (the last window request
        // wins, and the map goes cold either way), but a map the control
        // protocol armed is not the pane's to release on close just
        // because a preset was clicked; only a click that arms an unarmed
        // map makes the pane the owner.
        if self.emu.bus().heat_map().is_none() {
            self.heatmap_armed_by_panel = true;
        }
        self.emu.bus_mut().set_heat_map(Some(window));
        self.request_redraw();
    }

    /// Pin grid cell (`x`, `y`) so the readout under the map names what
    /// last touched it.
    pub(super) fn frame_analyzer_heat_pick(&mut self, x: u8, y: u8) {
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.heat_selected = Some(usize::from(y) * heatmap::GRID + usize::from(x));
        }
        self.request_redraw();
    }

    /// Move the Memory tab's pinned cell by one grid cell, clamped to the
    /// grid's edges. With nothing pinned the arrow starts from the centre
    /// cell, so the keyboard can reach the map without a click first.
    pub(super) fn frame_analyzer_move_heat_selection(&mut self, dx: i16, dy: i16) {
        let grid = heatmap::GRID as i32;
        let Some(panel) = self.frame_analyzer_panel.as_mut() else {
            return;
        };
        let centre = heatmap::CELLS / 2 + heatmap::GRID / 2;
        let cell = panel
            .heat_selected
            .unwrap_or(centre)
            .min(heatmap::CELLS - 1);
        let x = (cell % heatmap::GRID) as i32 + i32::from(dx);
        let y = (cell / heatmap::GRID) as i32 + i32::from(dy);
        panel.heat_selected =
            Some(y.clamp(0, grid - 1) as usize * heatmap::GRID + x.clamp(0, grid - 1) as usize);
        self.request_redraw();
    }

    pub(super) fn frame_analyzer_toggle_underlay(&mut self) {
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.show_underlay = !panel.show_underlay;
            // Dropping the underlay also ends a scrub riding on it.
            if !panel.show_underlay {
                panel.show_scrub = false;
            }
            self.request_redraw();
        }
    }

    pub(super) fn frame_analyzer_toggle_cpu_wait(&mut self) {
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.show_cpu_wait = !panel.show_cpu_wait;
            self.request_redraw();
        }
    }

    pub(super) fn frame_analyzer_toggle_scrub(&mut self) {
        // Snap inputs for enabling scrub: the traced frame's last slot and
        // the frame-start DIW top-left corner in (vpos, cck) beam units
        // (same decode as build_frame_analyzer_view's DIW overlay).
        let trace_end = self.emu.bus().frame_bus_trace().map(|trace| {
            (
                trace.rows.saturating_sub(1).min(u16::MAX as usize) as u16,
                trace.cols.saturating_sub(1).min(u16::MAX as usize) as u16,
            )
        });
        let base = self.emu.bus().frame_render_base();
        let diw_top_left = (!(base.diwstrt == 0 && base.diwstop == 0)).then(|| {
            (
                base.diwhigh.v_start(base.diwstrt),
                base.diwhigh.h_start(base.diwstrt) / 2,
            )
        });
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.show_scrub = !panel.show_scrub;
            // Enabling scrub with the selection at or before the display
            // window's top-left corner would ghost the whole picture (the
            // CRT has drawn none of it at that beam position), which reads
            // as the underlay switching off. Snap the selection to the end
            // of the traced frame instead: the picture starts fully drawn
            // and scrubbing backward peels it away.
            if panel.show_scrub {
                if let (Some((end_v, end_h)), Some(diw)) = (trace_end, diw_top_left) {
                    if (panel.selected_vpos, panel.selected_hpos) <= diw {
                        panel.selected_vpos = end_v;
                        panel.selected_hpos = end_h;
                    }
                }
            }
            self.request_redraw();
        }
    }

    /// Re-render the picture underlay when the analyzer's traced frame has
    /// changed. The render is a pure function of a `RenderInput` snapshot
    /// (`render_from_input`), so unlike the `bitplane::render` wrapper it
    /// never feeds collision bits or timing stats back into the machine:
    /// inspecting a frame cannot perturb the emulation. The result stays in
    /// beam coordinates (no presentation post-processing), matching the DMA
    /// trace's grid.
    pub(super) fn ensure_analyzer_underlay(&mut self) {
        let want = self
            .frame_analyzer_panel
            .as_ref()
            .is_some_and(|panel| panel.underlay_active());
        if !want {
            return;
        }
        let Some(frame) = self.emu.bus().frame_bus_trace().map(|trace| trace.frame) else {
            return;
        };
        if self.analyzer_underlay_frame == Some(frame) && self.analyzer_underlay_rows != 0 {
            return;
        }
        match &mut self.analyzer_underlay_input {
            Some(input) => input.refill_from_bus(self.emu.bus()),
            slot @ None => *slot = Some(bitplane::RenderInput::from_bus(self.emu.bus())),
        }
        let input = self
            .analyzer_underlay_input
            .as_ref()
            .expect("underlay render input just filled");
        let underlay_width = FB_WIDTH * input.canvas_scale();
        let fb = std::rc::Rc::make_mut(&mut self.analyzer_underlay_fb);
        fb.resize(MAX_CANVAS_PIXELS, 0);
        fb.fill(0);
        let _ = bitplane::render_from_input(input, fb.as_mut_slice());
        self.analyzer_underlay_width = underlay_width;
        self.analyzer_underlay_rows = self
            .emu
            .bus()
            .frame_geometry()
            .visible_lines
            .min(MAX_VISIBLE_LINES);
        self.analyzer_underlay_frame = Some(frame);
    }

    pub(super) fn frame_analyzer_select(&mut self, x: u16, y: u16, scanline: bool) {
        let Some(trace) = self.emu.bus().frame_bus_trace() else {
            return;
        };
        let hpos = (usize::from(x) * trace.cols / 1024).min(trace.cols.saturating_sub(1));
        let vpos = if scanline {
            self.frame_analyzer_panel
                .as_ref()
                .map(|panel| panel.selected_vpos as usize)
                .unwrap_or(trace.visible_start_vpos as usize)
        } else {
            (usize::from(y) * trace.rows / 1024).min(trace.rows.saturating_sub(1))
        };
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            panel.selected_hpos = hpos.min(u16::MAX as usize) as u16;
            panel.selected_vpos = vpos.min(u16::MAX as usize) as u16;
        }
    }

    pub(super) fn frame_analyzer_move_selection(&mut self, dhpos: i16, dvpos: i16) {
        let Some((max_hpos, max_vpos)) = self.emu.bus().frame_bus_trace().map(|trace| {
            (
                trace.cols.saturating_sub(1).min(u16::MAX as usize) as i32,
                trace.rows.saturating_sub(1).min(u16::MAX as usize) as i32,
            )
        }) else {
            return;
        };
        if let Some(panel) = self.frame_analyzer_panel.as_mut() {
            let hpos =
                (i32::from(panel.selected_hpos) + i32::from(dhpos)).clamp(0, max_hpos) as u16;
            let vpos =
                (i32::from(panel.selected_vpos) + i32::from(dvpos)).clamp(0, max_vpos) as u16;
            if panel.selected_hpos != hpos || panel.selected_vpos != vpos {
                panel.selected_hpos = hpos;
                panel.selected_vpos = vpos;
                self.request_redraw();
            }
        }
    }

    pub(super) fn debugger_toggle_run(&mut self) {
        self.paused = !self.paused;
        self.last_debug_stop = None;
        // Run/Pause inside the debugger is an explicit choice; closing the
        // window must not revert it.
        self.paused_before_debugger = self.paused;
        self.sync_live_audio_suspension();
    }

    /// Execute a single instruction while paused in the debugger.
    pub(super) fn debugger_step(&mut self) {
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        if let Err(e) = self.emu.debug_step_instructions(1) {
            error!("debugger step halted: {e:?}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        }
        self.surface_debug_stop();
    }

    /// Step over a subroutine call while paused: run a BSR/JSR/TRAP callee to
    /// completion and stop at the following instruction (a plain single step
    /// otherwise). Bounded so a call that never returns cannot wedge the UI.
    pub(super) fn debugger_step_over(&mut self) {
        const STEP_OVER_BUDGET: usize = 5_000_000;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        if let Err(e) = self.emu.debug_step_over(STEP_OVER_BUDGET) {
            error!("debugger step-over halted: {e:?}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        }
        self.surface_debug_stop();
        self.finish_render_for_current_frame();
    }

    /// Step out of the current subroutine while paused: run until it returns to
    /// its caller. Bounded so a routine that never returns cannot wedge the UI.
    pub(super) fn debugger_step_out(&mut self) {
        const STEP_OUT_BUDGET: usize = 5_000_000;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        if let Err(e) = self.emu.debug_step_out(STEP_OUT_BUDGET) {
            error!("debugger step-out halted: {e:?}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        }
        self.surface_debug_stop();
        self.finish_render_for_current_frame();
    }

    /// Run one whole video frame while paused, refreshing the display so
    /// mid-frame raster effects can be inspected frame by frame. A
    /// scheduler quantum is shorter than a PAL frame, so step until the
    /// frame counter advances (bounded for safety).
    pub(super) fn debugger_step_frame(&mut self) {
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        let target = self.emu.bus().emulated_frames() + 1;
        for _ in 0..8 {
            if let Err(e) = self.emu.step_frame() {
                error!("debugger frame step halted: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
                break;
            }
            if self.surface_debug_stop() {
                break;
            }
            if self.emu.bus().emulated_frames() >= target {
                break;
            }
        }
        self.finish_render_for_current_frame();
    }

    /// Run until the PC reaches the address typed in the entry box,
    /// bounded so a never-hit address cannot wedge the UI.
    pub(super) fn debugger_run_to(&mut self) {
        const RUN_TO_BUDGET: usize = 2_000_000;
        let Some(panel) = self.debugger_panel.as_ref() else {
            return;
        };
        let Some(addr) = panel.entry_addr() else {
            self.show_osd("Run to: type a hex address first");
            return;
        };
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.debug_run_to_pc(addr, RUN_TO_BUDGET) {
            Ok(true) => {}
            // A breakpoint/watch hit on the way is reported instead of
            // the budget message.
            Ok(false) => {
                if !self.surface_debug_stop() {
                    self.show_osd(format!("PC ${addr:06X} not reached (budget)"));
                }
            }
            Err(e) => {
                error!("debugger run-to halted: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
            }
        }
        self.finish_render_for_current_frame();
    }

    /// Run to the start of the next scanline (the end of the current one),
    /// stopping at exact beam granularity via a one-shot beam trap. The
    /// raster analogue of Step: walk a Copper effect line by line.
    pub(super) fn debugger_run_to_line_end(&mut self) {
        const RUN_TO_LINE_BUDGET: usize = 2_000_000;
        let (vpos, frame_lines) = {
            let bus = self.emu.bus();
            (bus.agnus.vpos, bus.agnus.current_frame_lines())
        };
        let target = ((vpos + 1) % frame_lines.max(1)).min(u32::from(u16::MAX)) as u16;
        self.run_to_beam_target(target, None, RUN_TO_LINE_BUDGET, "Line end");
    }

    /// Toggle a beam trap from the entry box ("VPOS [HPOS]", decimal).
    pub(super) fn debugger_toggle_beam_trap(&mut self) {
        let spec = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| ui::parse_beam_spec(&panel.entry));
        let Some((vpos, hpos)) = spec else {
            self.show_osd("Beam: type \"VPOS [HPOS]\" (decimal) first");
            return;
        };
        let set = self.emu.bus_mut().ui_toggle_beam_trap(vpos, hpos);
        let mut msg = format!("Beam trap v{vpos}");
        if let Some(hpos) = hpos {
            msg.push_str(&format!(" h{hpos}"));
        }
        msg.push_str(if set { " set" } else { " removed" });
        self.show_osd(msg);
    }

    /// Arm a waveform (VCD) capture from the Waveform tab's entry box:
    /// an order-free "[PATH] [TRIGGER] [DURATION] [SIGNALS]" spec, with
    /// an empty entry meaning all defaults (trigger now, one frame, all
    /// signals, timestamped path).
    pub(super) fn debugger_wave_arm(&mut self) {
        let entry = self
            .debugger_panel
            .as_ref()
            .map(|panel| panel.entry.clone())
            .unwrap_or_default();
        let opts = match crate::waveform::parse_wave_args(entry.split_whitespace()) {
            Ok(opts) => opts,
            Err(e) => {
                self.show_osd(format!("Waveform: {e}"));
                return;
            }
        };
        let summary = format!(
            "Waveform armed ({}) -> {}",
            opts.trigger,
            opts.path.display()
        );
        match self.emu.machine.ui_wave_start(opts) {
            Ok(()) => self.show_osd(summary),
            Err(e) => self.show_osd(format!("Waveform: {e}")),
        }
    }

    /// Stop the waveform capture (Waveform tab), finishing the VCD file.
    pub(super) fn debugger_wave_stop(&mut self) {
        match self.emu.machine.ui_wave_stop() {
            Some(status) => self.show_osd(format!(
                "Waveform stopped: {} samples in {}",
                status.samples,
                status.path.display()
            )),
            None => self.show_osd("No waveform capture"),
        }
    }

    /// Toggle bitplane `plane` in the presented picture (Video tab).
    pub(super) fn debugger_toggle_plane(&mut self, plane: usize) {
        let shown = self.emu.bus_mut().ui_toggle_layer_plane(plane);
        self.show_osd(format!(
            "Bitplane {} {}",
            plane + 1,
            if shown { "shown" } else { "hidden" }
        ));
        self.rerender_after_debug_view_change();
    }

    /// Toggle sprite `sprite` in the presented picture (Video tab).
    pub(super) fn debugger_toggle_sprite(&mut self, sprite: usize) {
        let shown = self.emu.bus_mut().ui_toggle_layer_sprite(sprite);
        self.show_osd(format!(
            "Sprite {sprite} {}",
            if shown { "shown" } else { "hidden" }
        ));
        self.rerender_after_debug_view_change();
    }

    /// Re-render and re-present the current frame after a debug-only view
    /// change (layer isolation). Uses the pure snapshot render, so unlike
    /// the normal frame path nothing feeds back into the machine: toggling
    /// a layer while paused cannot perturb the emulation.
    pub(super) fn rerender_after_debug_view_change(&mut self) {
        let visible_start_vpos = self.emu.bus().frame_visible_start_vpos();
        let h_shift = if self.hcenter {
            // Re-resolving the same frame through the latch is idempotent:
            // the same snapshot yields the same class and the same shift.
            self.presentation_latch
                .presentation_h_shift(&self.emu.bus().frame_render_base(), self.overscan)
        } else {
            0
        };
        bitplane::render_display_only(self.emu.bus(), &mut self.fb);
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
        )
        .rows;
        let base = self.emu.bus().frame_render_base();
        let (rows, width) = self.deinterlacer.present_field_into(
            &self.fb,
            field_rows,
            FB_WIDTH * canvas_scale,
            base.bplcon0 & 0x0004 != 0,
            base.long_field,
            !geometry.programmable,
            &mut self.present_fb,
        );
        self.present_rows = rows;
        self.present_width = width;
        self.request_redraw();
    }

    /// Toggle an exception catchpoint from the entry box ("irq N",
    /// "trap N", or "vec N").
    pub(super) fn debugger_toggle_catch(&mut self) {
        let spec = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| ui::parse_catch_spec(&panel.entry));
        let Some(vector) = spec else {
            self.show_osd("Catch: type \"irq N\", \"trap N\", or \"vec N\" first");
            return;
        };
        let set = self.emu.machine.ui_toggle_catch(vector);
        self.show_osd(format!(
            "Catch {} {}",
            crate::debugger::exception_vector_name(vector),
            if set { "set" } else { "removed" }
        ));
    }

    /// Toggle a Copper breakpoint at the entry address (Copper tab).
    pub(super) fn debugger_toggle_copper_break(&mut self) {
        let Some(addr) = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| panel.entry_addr())
        else {
            self.show_osd("CBreak: type a hex Copper-list address first");
            return;
        };
        let set = self.emu.bus_mut().ui_toggle_copper_break(addr);
        self.show_osd(format!(
            "Copper breakpoint ${:06X} {}",
            addr & 0x00FF_FFFE,
            if set { "set" } else { "removed" }
        ));
    }

    /// Run until the Copper retires one instruction (Copper tab CStep).
    pub(super) fn debugger_step_copper(&mut self) {
        const COPPER_STEP_BUDGET: usize = 2_000_000;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.debug_step_copper(COPPER_STEP_BUDGET) {
            Ok(true) => {
                self.surface_debug_stop();
            }
            Ok(false) => self.show_osd("Copper did not advance (stopped or DMA off)"),
            Err(e) => {
                error!("copper step halted: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
            }
        }
        self.finish_render_for_current_frame();
    }

    /// Run until the beam reaches the analyzer's selected slot, stopping
    /// at exact colour-clock granularity via a one-shot beam trap.
    pub(super) fn frame_analyzer_run_to_slot(&mut self) {
        const RUN_TO_SLOT_BUDGET: usize = 2_000_000;
        let Some((vpos, hpos)) = self
            .frame_analyzer_panel
            .as_ref()
            .map(|panel| (panel.selected_vpos, panel.selected_hpos))
        else {
            return;
        };
        self.emu.bus_mut().set_frame_analyzer_full(true);
        self.run_to_beam_target(vpos, Some(hpos), RUN_TO_SLOT_BUDGET, "Beam slot");
    }

    /// Shared run-to-beam-position transport: pause bookkeeping, the
    /// bounded run, stop reporting, and the display refresh.
    pub(super) fn run_to_beam_target(
        &mut self,
        vpos: u16,
        hpos: Option<u16>,
        budget: usize,
        what: &str,
    ) {
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.debug_run_to_beam(vpos, hpos, budget) {
            Ok(true) => {
                self.surface_debug_stop();
            }
            Ok(false) => self.show_osd(format!("{what} not reached (budget)")),
            Err(e) => {
                error!("debugger run-to-beam halted: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
            }
        }
        self.finish_render_for_current_frame();
    }

    /// Step one instruction backward, reconstructed from the snapshot ring.
    pub(super) fn debugger_reverse_step(&mut self) {
        use crate::timetravel::ReverseOutcome;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.tt_reverse_step(1) {
            Ok(ReverseOutcome::Found(_)) => {}
            Ok(ReverseOutcome::BeyondHistory) => self.show_osd("Reverse: beyond recorded history"),
            Ok(ReverseOutcome::NotFound) => self.show_osd("Reverse: nothing earlier to step to"),
            Err(e) => error!("reverse step halted: {e:?}"),
        }
        self.finish_render_for_current_frame();
    }

    /// Step one emulated video frame backward, reconstructed from the
    /// snapshot ring.
    pub(super) fn debugger_reverse_frame(&mut self) {
        use crate::timetravel::ReverseOutcome;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.tt_reverse_frame() {
            Ok(ReverseOutcome::Found(_)) => {}
            Ok(ReverseOutcome::BeyondHistory) => {
                self.show_osd("Reverse frame: beyond recorded history")
            }
            Ok(ReverseOutcome::NotFound) => {
                self.show_osd("Reverse frame: no earlier frame to step to")
            }
            Err(e) => error!("reverse frame step halted: {e:?}"),
        }
        self.finish_render_for_current_frame();
    }

    /// Run backward to the previous breakpoint hit (reconstructed from the
    /// snapshot ring).
    pub(super) fn debugger_reverse_continue(&mut self) {
        use crate::timetravel::ReverseOutcome;
        self.paused = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        match self.emu.tt_reverse_continue() {
            Ok(ReverseOutcome::Found((_, reason))) => {
                let message = format!("Reverse: {reason}");
                info!("debugger stop: {message}");
                self.last_debug_stop = Some(message.clone());
                self.show_osd(message);
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.tab = ui::DebugTab::Break;
                }
            }
            Ok(ReverseOutcome::NotFound) => self.show_osd("Reverse run: no earlier stop hit"),
            Ok(ReverseOutcome::BeyondHistory) => {
                self.show_osd("Reverse run: beyond recorded history")
            }
            Err(e) => error!("reverse continue halted: {e:?}"),
        }
        self.finish_render_for_current_frame();
    }

    /// Surface a pending breakpoint/watchpoint hit: pause the machine,
    /// bring up the debugger window, and report the reason. Returns true
    /// when a stop was pending. Also reports (once) a CPU double-fault
    /// halt -- the guest is dead at that point, so it must not pass
    /// silently.
    pub(super) fn surface_debug_stop(&mut self) -> bool {
        if self.emu.machine.cpu_double_faulted() {
            if !self.reported_double_fault {
                self.reported_double_fault = true;
                let message = format!(
                    "CPU halted: double fault at pc ${:06X} (bus/address error during exception)",
                    self.emu.machine.pc() & self.emu.machine.ui_addr_mask()
                );
                warn!("{message}");
                self.last_debug_stop = Some(message.clone());
                if let Some(panel) = self.console_panel.as_mut() {
                    panel.push_output(format!("!{message}"));
                }
                self.paused = true;
                self.sync_live_audio_suspension();
                // Notify first (it self-suppresses while a resume is
                // pending), then answer every pending resume.
                #[cfg(feature = "control")]
                self.control_notify_stopped("double_fault", &message);
                self.complete_remote_resumes("double_fault", &message);
                self.show_osd(message);
                self.request_redraw();
                return true;
            }
        } else {
            self.reported_double_fault = false;
        }
        let Some(stop) = self.emu.machine.take_ui_debug_stop() else {
            return false;
        };
        let message = stop.describe();
        info!("debugger stop: {message}");
        // A stop while a remote resume is pending answers every client
        // that has one -- a control-protocol continue and a GDB continue
        // can both be outstanding when both share the window -- and pauses
        // without commandeering the local debugger window; a control
        // client with nothing pending still hears of it as event.stopped.
        // Only a stop no client consumed opens the panel.
        #[allow(unused_mut)]
        let mut consumed = false;
        #[cfg(feature = "control")]
        {
            consumed |= self.control_completes_stop(&stop);
        }
        #[cfg(feature = "gdb")]
        {
            consumed |= self.gdb_completes_stop(&stop);
        }
        self.paused = true;
        self.sync_live_audio_suspension();
        if !consumed {
            self.paused_before_debugger = true;
            self.open_debugger();
        }
        self.last_debug_stop = Some(message.clone());
        self.show_osd(message);
        self.request_redraw();
        true
    }

    /// Toggle a PC breakpoint from the entry box. The entry may carry an
    /// optional condition and ignore count: "ADDR [LHS OP RHS] [IGN N]".
    pub(super) fn debugger_toggle_breakpoint(&mut self) {
        let spec = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| ui::parse_break_spec(&panel.entry));
        let Some((addr, cond, ignore)) = spec else {
            self.show_osd("Break: ADDR [LHS OP RHS] [IGN N] e.g. C033C2 D0 EQ 5");
            return;
        };
        let set = self.emu.machine.ui_set_breakpoint(addr, cond, ignore);
        let mut msg = format!(
            "Breakpoint ${:06X} {}",
            addr & self.emu.machine.ui_addr_mask(),
            if set { "set" } else { "removed" }
        );
        if set {
            if let Some(cond) = &cond {
                msg.push_str(&format!(" when {}", cond.describe()));
            }
            if ignore > 0 {
                msg.push_str(&format!(" ign {ignore}"));
            }
        }
        self.show_osd(msg);
    }

    /// Toggle a memory word watchpoint at the entry-box address.
    pub(super) fn debugger_toggle_watchpoint(&mut self) {
        let Some(addr) = self.debugger_entry_addr("Watch") else {
            return;
        };
        let addr = addr & self.emu.machine.ui_addr_mask() & !1;
        let set = self.emu.machine.ui_toggle_watch(addr);
        self.show_osd(format!(
            "Watchpoint ${addr:06X} {}",
            if set { "set" } else { "removed" }
        ));
    }

    /// Toggle a chipset-register write watch. The entry accepts either a
    /// bare register offset (96) or a full address (DFF096).
    pub(super) fn debugger_toggle_reg_watch(&mut self) {
        let Some(addr) = self.debugger_entry_addr("Reg") else {
            return;
        };
        let off = (addr & 0x1FE) as u16;
        let set = self.emu.machine.ui_toggle_reg_watch(off);
        self.show_osd(format!(
            "{} (${off:03X}) write watch {}",
            crate::debugger::custom_reg_name(off),
            if set { "set" } else { "removed" }
        ));
    }

    /// The debugger entry-box address, or an OSD prompt when empty.
    pub(super) fn debugger_entry_addr(&mut self, what: &str) -> Option<u32> {
        let panel = self.debugger_panel.as_ref()?;
        let addr = panel.entry_addr();
        if addr.is_none() {
            self.show_osd(format!("{what}: type a hex address first"));
        }
        addr
    }

    /// Page the Memory tab's hex dump up or down.
    pub(super) fn debugger_mem_page(&mut self, direction: i32) {
        if let Some(panel) = self.debugger_panel.as_mut() {
            if panel.tab == ui::DebugTab::Memory {
                // Bits mode pages by one bitmap screenful; hex by one page.
                let delta = if panel.mem_view_bits {
                    (panel.mem_bitmap_stride * ui::mem_bitmap_rows() as u32).max(1)
                } else {
                    ui::MEM_PAGE_BYTES
                };
                panel.mem_addr = if direction < 0 {
                    panel.mem_addr.wrapping_sub(delta)
                } else {
                    panel.mem_addr.wrapping_add(delta)
                } & self.emu.machine.ui_addr_mask();
                if !panel.mem_view_bits {
                    panel.mem_addr &= !0xF;
                }
            }
        }
    }

    /// Scroll the Memory tab by `rows` display rows (16 bytes each in the
    /// hex view, one stride in the bitmap view).
    pub(super) fn debugger_mem_scroll(&mut self, rows: i32) {
        if let Some(panel) = self.debugger_panel.as_mut() {
            if panel.tab == ui::DebugTab::Memory && rows != 0 {
                let step = if panel.mem_view_bits {
                    panel.mem_bitmap_stride.max(1)
                } else {
                    16
                };
                let delta = step.wrapping_mul(rows.unsigned_abs());
                panel.mem_addr = if rows < 0 {
                    panel.mem_addr.wrapping_sub(delta)
                } else {
                    panel.mem_addr.wrapping_add(delta)
                } & self.emu.machine.ui_addr_mask();
                self.request_redraw();
            }
        }
    }

    /// Find the entry's hex byte pattern in CPU-visible memory, starting
    /// past the previous hit (or the current page) and wrapping around
    /// the decoded memory map once.
    pub(super) fn debugger_mem_find(&mut self) {
        let Some(panel) = self.debugger_panel.as_ref() else {
            return;
        };
        let Some(pattern) = panel.find_pattern() else {
            self.show_osd("Find: type hex byte pairs first (e.g. 4E75)");
            return;
        };
        let start = panel
            .mem_last_find
            .map(|addr| addr.wrapping_add(1))
            .unwrap_or(panel.mem_addr)
            & self.emu.machine.ui_addr_mask();
        let regions = self.emu.bus().searchable_regions();
        let found = console::search_cpu_memory(&self.emu.machine, &regions, &pattern, start);
        match found {
            Some(addr) => {
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.mem_last_find = Some(addr);
                    panel.mem_addr = addr & !0xF;
                }
                self.show_osd(format!("Found at ${addr:06X}"));
            }
            None => self.show_osd("Pattern not found"),
        }
        self.request_redraw();
    }

    /// Save the entry's "ADDR LEN" region of CPU-visible memory to a file
    /// picked in a save dialog (the GUI counterpart of the headless
    /// COPPERLINE_DBG_RAMDUMP knob).
    pub(super) fn debugger_mem_save_region(&mut self) {
        let Some((addr, len)) = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| panel.region_spec())
        else {
            self.show_osd("Save: type \"ADDR LEN\" (hex) first");
            return;
        };
        // Through the machine's address bus, like every other debugger
        // surface: the file name and the OSD then name the address the
        // bytes actually came from on a 24-bit model, and a 32-bit dump
        // above the 24-bit space passes through untouched on 020+.
        let addr = addr & self.emu.machine.ui_addr_mask();
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Save memory region")
            .set_file_name(format!("mem-{addr:06X}-{len:X}.bin"))
            .save_file();
        if let Some(path) = picked {
            let bytes = self.emu.machine.debug_read_memory(addr, len as usize);
            match std::fs::write(&path, &bytes) {
                Ok(()) => self.show_osd(format!(
                    "Saved ${addr:06X}+${len:X} to {}",
                    display_file_name(&path)
                )),
                Err(e) => {
                    warn!("memory region save failed ({}): {e:#}", path.display());
                    self.show_osd("Memory save failed (see log)");
                }
            }
        }
        self.finish_host_io_pause();
    }

    /// Report the last instruction that wrote the word at the entry
    /// address, replayed from the reverse-debug snapshot ring (the GUI
    /// counterpart of GDB's "monitor last-writer").
    pub(super) fn debugger_mem_writer(&mut self) {
        use crate::timetravel::ReverseOutcome;
        let Some(addr) = self
            .debugger_panel
            .as_ref()
            .and_then(|panel| panel.entry_addr())
        else {
            self.show_osd("Writer: type a hex address first");
            return;
        };
        let addr = addr & self.emu.machine.ui_addr_mask() & !1;
        let before = self.emu.retired_instructions();
        match self.emu.tt_last_writer(addr, before) {
            Ok(ReverseOutcome::Found(rec)) => {
                let message = format!(
                    "${:06X}: {:04X}->{:04X} by pc ${:06X} (frame {})",
                    rec.addr,
                    rec.old,
                    rec.new,
                    rec.pc & self.emu.machine.ui_addr_mask(),
                    rec.frame
                );
                info!("last-writer {message}");
                self.last_debug_stop = Some(format!("Last writer {message}"));
                self.show_osd(message);
            }
            Ok(ReverseOutcome::NotFound) => {
                self.show_osd(format!("No write to ${addr:06X} in retained history"))
            }
            Ok(ReverseOutcome::BeyondHistory) => {
                self.show_osd(format!("Write to ${addr:06X} predates history"))
            }
            Err(e) => {
                error!("last-writer failed: {e:?}");
                self.show_osd("Last-writer failed (see log)");
            }
        }
        self.finish_render_for_current_frame();
        self.request_redraw();
    }

    /// Toggle the Memory tab between hex and the 1-bpp bitplane view. An
    /// entry holding a small decimal number sets the bitmap row stride.
    pub(super) fn debugger_mem_toggle_bits(&mut self) {
        if let Some(panel) = self.debugger_panel.as_mut() {
            if let Some(stride) = panel
                .entry
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|stride| (1..=512).contains(stride))
            {
                panel.mem_bitmap_stride = stride;
            }
            panel.mem_view_bits = !panel.mem_view_bits;
            let mode = if panel.mem_view_bits {
                format!("bitmap (stride {} bytes)", panel.mem_bitmap_stride)
            } else {
                "hex".to_string()
            };
            self.show_osd(format!("Memory view: {mode}"));
            self.request_redraw();
        }
    }

    /// Write the entry box's value live while paused: a memory word from
    /// "ADDR VALUE" on the Memory tab, or a register from "REG VALUE" on the
    /// CPU tab. The panel borrow is resolved into a plain action first so the
    /// emulator can then be borrowed mutably to perform the write.
    pub(super) fn debugger_poke(&mut self) {
        enum Poke {
            Mem(u32, u16),
            Reg(usize, u32),
            MemHelp,
            RegHelp,
            None,
        }
        let action = match self.debugger_panel.as_ref() {
            Some(panel) => match panel.tab {
                ui::DebugTab::Memory => match panel.poke_target() {
                    Some((addr, value)) => Poke::Mem(addr, value),
                    None => Poke::MemHelp,
                },
                ui::DebugTab::Cpu => match panel.reg_poke() {
                    Some((reg, value)) => Poke::Reg(reg, value),
                    None => Poke::RegHelp,
                },
                _ => Poke::None,
            },
            None => Poke::None,
        };
        match action {
            Poke::Mem(addr, value) => {
                let written = self
                    .emu
                    .machine
                    .debug_write_memory(addr, &value.to_be_bytes());
                if written == 2 {
                    self.show_osd(format!("Poked ${value:04X} -> ${addr:06X}"));
                } else {
                    self.show_osd(format!("${addr:06X} is not writable RAM"));
                }
            }
            Poke::Reg(reg, value) => {
                self.emu.machine.debug_set_register(reg, value);
                self.show_osd(format!("{} <- ${value:X}", gdb_reg_label(reg)));
            }
            Poke::MemHelp => self.show_osd("Poke: type \"ADDR VALUE\" (hex) first"),
            Poke::RegHelp => self.show_osd("Set Reg: type \"REG VALUE\" e.g. D0 1234"),
            Poke::None => {}
        }
    }

    /// Build the per-redraw view data for the open panel, if any.
    pub(super) fn build_panel_view_data(&self) -> Option<ui::PanelViewData> {
        match self.ui.panel.as_ref()? {
            Panel::About => Some(ui::PanelViewData::About(crate::video::about::AboutView {
                machine_lines: self.about_machine_lines.clone(),
                elapsed_ms: self.about_opened_at.elapsed().as_millis() as u64,
                machine_fitted: self.about_machine_lines.first().map(String::as_str)
                    != Some(crate::config::ABOUT_PLACEHOLDER_LINE),
            })),
            Panel::Shortcuts => Some(ui::PanelViewData::Shortcuts),
            // Self-contained: the panel's own state is everything it draws.
            Panel::InputMap(_) => None,
            Panel::Calibration(session) => Some(ui::PanelViewData::Calibration(
                build_calibration_view(session, self.cal_pad_drives),
            )),
            Panel::Debugger(panel) => Some(ui::PanelViewData::Debugger(Box::new(
                self.build_debugger_view(panel),
            ))),
            Panel::FrameAnalyzer(panel) => Some(ui::PanelViewData::FrameAnalyzer(Box::new(
                self.build_frame_analyzer_view(panel),
            ))),
            // The console, configuration, and drop-chooser panels render
            // from their own state.
            Panel::Console(_) => None,
            Panel::Launcher(_) => None,
            Panel::DropChooser(_) => None,
        }
    }

    pub(super) fn build_tool_panel_view_data(
        &self,
        kind: ToolPanelKind,
    ) -> Option<ui::PanelViewData> {
        match kind {
            ToolPanelKind::Debugger => self.debugger_panel.as_ref().map(|panel| {
                ui::PanelViewData::Debugger(Box::new(self.build_debugger_view(panel)))
            }),
            ToolPanelKind::FrameAnalyzer => self.frame_analyzer_panel.as_ref().map(|panel| {
                ui::PanelViewData::FrameAnalyzer(Box::new(self.build_frame_analyzer_view(panel)))
            }),
            // The console panel carries everything it renders.
            ToolPanelKind::Console => None,
        }
    }

    pub(super) fn build_frame_analyzer_view(
        &self,
        panel: &ui::FrameAnalyzerPanel,
    ) -> ui::FrameAnalyzerView {
        let bus = self.emu.bus();
        let status = format!(
            "{} frame {} {:.2}s",
            if self.paused { "paused" } else { "running" },
            bus.emulated_frames(),
            bus.emulated_seconds()
        );
        // The heat map records bus activity, not beam slots, so it has
        // something to show even on a frame the analyzer captured no trace
        // for: built before the no-trace return and carried by both arms.
        let heat = self.build_analyzer_heat_view(panel);
        let resources = (panel.tab == ui::AnalyzerTab::Resources)
            .then(|| self.build_analyzer_resources_view(panel));
        let Some(trace) = bus.frame_bus_trace() else {
            return ui::FrameAnalyzerView {
                running: !self.paused,
                status,
                trace: None,
                underlay: None,
                scrub: false,
                heat,
                resources,
            };
        };
        let underlay = (panel.underlay_active() && self.analyzer_underlay_rows > 0).then(|| {
            ui::AnalyzerUnderlayView {
                fb: std::rc::Rc::clone(&self.analyzer_underlay_fb),
                rows: self.analyzer_underlay_rows,
                width: self.analyzer_underlay_width,
            }
        });
        let selected_vpos = usize::from(panel.selected_vpos).min(trace.rows.saturating_sub(1));
        let selected_hpos = usize::from(panel.selected_hpos).min(trace.cols.saturating_sub(1));
        let selected_owner_code = trace.owner_code_at(selected_vpos, selected_hpos);
        let selected_owner = owner_name_from_code(selected_owner_code);
        let selected_cpu_wait_code = trace.cpu_wait_code_at(selected_vpos, selected_hpos);
        let mut owners = Vec::with_capacity(trace.rows * trace.cols);
        let mut cpu_waits = Vec::with_capacity(trace.rows * trace.cols);
        for vpos in 0..trace.rows {
            if let Some(row) = trace.owner_row(vpos) {
                owners.extend_from_slice(row);
            }
            if let Some(row) = trace.cpu_wait_row(vpos) {
                cpu_waits.extend_from_slice(row);
            }
        }
        // Marker capacity bounds the per-redraw copy, not what is worth
        // seeing: a Copper writing a palette split on every line stays
        // well inside it.
        const MARKER_CAP: usize = 4000;
        let markers = bus
            .frame_render_events()
            .iter()
            .take(MARKER_CAP)
            .map(|event| ui::AnalyzerMarker {
                vpos: event.vpos.min(u32::from(u16::MAX)) as u16,
                hpos: event.hpos.min(u32::from(u16::MAX)) as u16,
                offset: event.offset & 0x01FE,
                value: event.value,
                source: match event.source {
                    BeamWriteSource::Cpu => "cpu",
                    BeamWriteSource::CpuCopperIrq => "irq",
                    BeamWriteSource::Copper => "copper",
                },
            })
            .collect();
        // Frame-start DIW/DDF overlays, decoded with the display model's
        // own rules (DiwHigh carries the OCS implicit bits or the ECS
        // DIWHIGH extension; DIW h units are lores pixels, two per cck).
        let base = bus.frame_render_base();
        let diw_programmed = !(base.diwstrt == 0 && base.diwstop == 0);
        let (diw_v, diw_h_cck) = if diw_programmed {
            let v0 = base.diwhigh.v_start(base.diwstrt);
            let mut v1 = base.diwhigh.v_stop(base.diwstop);
            if v1 <= v0 {
                // Hardware vstop wrap: a stop at or above the start means
                // the window runs past the 8-bit rollover.
                v1 += 0x100;
            }
            let h0 = base.diwhigh.h_start(base.diwstrt) / 2;
            let h1 = base.diwhigh.h_stop(base.diwstop) / 2;
            (Some((v0, v1)), Some((h0, h1)))
        } else {
            (None, None)
        };
        let ddf_cck = diw_programmed.then_some((base.ddfstrt & 0x00FE, base.ddfstop & 0x00FE));
        // Annotate the selected slot with the blit whose beam span
        // contains it, so clicking a blitter run names the blit.
        let selected_beam = (selected_vpos as u16, selected_hpos as u16);
        let selected_blit = trace.blits.iter().enumerate().find_map(|(i, blit)| {
            let end = blit.end?;
            (blit.start <= selected_beam && selected_beam <= end).then(|| {
                format!(
                    "in blit #{i} ({}x{} D ${:06X})",
                    blit.width_words,
                    blit.height,
                    blit.dpt & 0x00FF_FFFF
                )
            })
        });
        let rom_symbols = crate::amigaos::symbols::snapshot_on_bus(bus);
        let top_stalled_pcs = trace
            .top_stalled_pcs(ui::ANALYZER_TOP_STALLED_PCS)
            .into_iter()
            .map(|(pc, cck)| {
                let symbol = rom_symbols.resolve(pc).map(|symbol| symbol.display_name());
                (pc, cck, symbol)
            })
            .collect();
        ui::FrameAnalyzerView {
            running: !self.paused,
            status,
            underlay,
            scrub: panel.show_scrub,
            heat,
            resources,
            trace: Some(ui::AnalyzerTraceView {
                frame: trace.frame,
                seconds: trace.seconds,
                rows: trace.rows,
                cols: trace.cols,
                line_cck: trace.line_cck,
                visible_start_vpos: trace.visible_start_vpos,
                visible_lines: trace.visible_lines,
                display_hpos_start: trace.display_hpos_start,
                display_hpos_end: trace.display_hpos_end,
                owner_cck: trace.owner_cck,
                blitter_busy_cck: trace.blitter_busy_cck,
                blitter_starve_cck: trace.blitter_starve_cck,
                partial: trace.partial,
                selected_vpos,
                selected_hpos,
                selected_owner,
                selected_owner_code,
                owners,
                records: trace.records_arc(),
                markers,
                selected_blit,
                diw_v,
                diw_h_cck,
                ddf_cck,
                cpu_waits,
                cpu_wait_cck: trace.cpu_wait_cck,
                cpu_wait_by_class: trace.cpu_wait_by_class,
                cpu_wait_by_kind: trace.cpu_wait_by_kind,
                top_stalled_pcs,
                selected_cpu_wait_code,
            }),
        }
    }

    /// The Memory tab's picture of the live heat map, or None while no map
    /// is armed. Everything is read out here rather than in the drawing
    /// code: the map lives on the bus, and the panel only ever sees the
    /// rendered grid, the census, and the pinned cell's record.
    pub(super) fn build_analyzer_heat_view(
        &self,
        panel: &ui::FrameAnalyzerPanel,
    ) -> Option<ui::AnalyzerHeatView> {
        let bus = self.emu.bus();
        let map = bus.heat_map()?;
        let frame = bus.emulated_frames();
        let mut image = vec![0xFF00_0000u32; heatmap::CELLS];
        map.render(frame, &mut image);
        let bytes_per_cell = map.bytes_per_cell();
        // The map's census reports only the touchers holding cells; the
        // column wants every toucher, in a fixed order, so the rows read as
        // a legend and do not move as activity comes and goes.
        let counts = map.census(frame);
        let census = HEAT_TOUCHERS
            .iter()
            .map(|toucher| {
                let cells = counts
                    .iter()
                    .find(|(recorded, _)| recorded == toucher)
                    .map_or(0, |(_, cells)| *cells);
                ui::AnalyzerHeatCensusRow {
                    name: toucher.name(),
                    colour: toucher.colour(),
                    cells,
                    bytes: cells as u64 * u64::from(bytes_per_cell),
                }
            })
            .collect();
        let selected = panel.heat_selected.map(|cell| {
            let record = map.cell(cell);
            ui::AnalyzerHeatCell {
                cell,
                toucher: record.map(|(toucher, _)| toucher.name()),
                colour: record.map_or(0, |(toucher, _)| toucher.colour()),
                // The stamp is the frame counter's low 32 bits, the same
                // arithmetic the map's own fade uses.
                age_frames: record.map(|(_, stamp)| (frame as u32).saturating_sub(stamp)),
            }
        });
        // Registered resources, sorted, so the pane can name what the
        // guest says lives under a cell (crate::uaelib).
        let mut resources: Vec<ui::AnalyzerHeatResource> = self
            .emu
            .uaelib_resources()
            .iter()
            .map(|resource| ui::AnalyzerHeatResource {
                start: resource.address,
                end: resource.address.saturating_add(resource.size.max(1)),
                name: resource.name.clone(),
                kind: resource.kind_name(),
            })
            .collect();
        resources.sort_by_key(|resource| resource.start);
        Some(ui::AnalyzerHeatView {
            image,
            base: map.base(),
            span: map.span(),
            bytes_per_cell,
            frame,
            census,
            selected,
            resources,
        })
    }

    /// The Audio tab's line-mixed source rows for this machine, in draw
    /// order: CD-DA always, then one row per fitted source. The view
    /// builder and the mute-click dispatcher both use this list, so a
    /// click on row `4 + i` always lands on the source drawn there.
    pub(super) fn audio_extra_kinds(bus: &crate::bus::Bus) -> Vec<ui::AudioExtraKind> {
        let mut kinds = vec![ui::AudioExtraKind::Cd];
        #[cfg(feature = "midi")]
        if bus.midi_serial().is_some_and(Self::midi_synth_fitted) {
            kinds.push(ui::AudioExtraKind::Synth);
        }
        if bus.toccata_board().is_some() {
            kinds.push(ui::AudioExtraKind::Toccata);
        }
        #[cfg(feature = "mhi")]
        if bus.mhi_board().is_some() {
            kinds.push(ui::AudioExtraKind::Mhi);
        }
        kinds
    }

    /// Whether the serial port's MIDI sink has an in-process synth fitted
    /// (an MT-32 or the Coppersynth) -- the condition for the Audio tab's
    /// synth row. A host MIDI endpoint sounds on the host, so there is
    /// nothing for the mixer (or the row's scope) to show.
    #[cfg(feature = "midi")]
    pub(super) fn midi_synth_fitted(midi: &crate::midi::MidiSerialSink) -> bool {
        #[cfg(feature = "mt32")]
        if midi.mt32().is_some() {
            return true;
        }
        #[cfg(feature = "coppersynth")]
        if midi.csynth().is_some() {
            return true;
        }
        let _ = midi;
        false
    }

    /// The fitted in-process synth's display name for the Audio tab row.
    #[cfg(feature = "midi")]
    pub(super) fn midi_synth_label(midi: &crate::midi::MidiSerialSink) -> String {
        #[cfg(feature = "mt32")]
        if midi.mt32().is_some() {
            return match midi.mt32_version() {
                Some(version) => format!("MT-32 ({version})"),
                None => "MT-32".to_string(),
            };
        }
        #[cfg(feature = "coppersynth")]
        if midi.csynth().is_some() {
            return crate::midi::MIDI_OUT_CSYNTH_LABEL.to_string();
        }
        let _ = midi;
        "synth".to_string()
    }

    /// Snapshot the machine into the debugger panel's formatted lines.
    /// Everything reads through side-effect-free peeks, so inspecting
    /// state never perturbs the emulation.
    pub(super) fn build_debugger_view(&self, panel: &ui::DebuggerPanel) -> ui::DebuggerView {
        let machine = &self.emu.machine;
        let bus = self.emu.bus();
        let mut status = format!(
            "{} frame {} {:.2}s",
            if self.paused { "paused" } else { "running" },
            bus.emulated_frames(),
            bus.emulated_seconds()
        );
        // Reverse-debug position and history depth, when the ring is armed.
        if let Some(ring) = self.emu.time_travel_ring() {
            if !ring.is_empty() {
                status.push_str(&format!(
                    "  | pos {} rev {} snaps, {} MB",
                    self.emu.retired_instructions(),
                    ring.len(),
                    ring.used_bytes() / (1024 * 1024),
                ));
            }
        }
        let read = |addr: u32| bus.peek_word_any(addr);
        let mut lines: Vec<ui::DbgLine> = Vec::new();
        let mut bitmap: Option<ui::MemBitmapView> = None;
        let mut video: Option<ui::VideoView> = None;
        let mut audio: Option<ui::AudioScopeView> = None;
        match panel.tab {
            ui::DebugTab::Cpu => {
                let pc = machine.pc();
                let sr = machine.sr();
                lines.push(ui::DbgLine::plain(format!(
                    "PC {pc:08X}   SR {sr:04X} [{}]{}",
                    ui::sr_flags(sr),
                    if machine.stopped() { "   STOPPED" } else { "" }
                )));
                lines.push(ui::DbgLine::plain(""));
                for (name, regs) in [("D", 0usize), ("A", 1)] {
                    for half in 0..2 {
                        let row: Vec<String> = (0..4)
                            .map(|i| {
                                let reg = half * 4 + i;
                                let value = if regs == 0 {
                                    machine.d(reg)
                                } else {
                                    machine.a(reg)
                                };
                                format!("{name}{reg} {value:08X}")
                            })
                            .collect();
                        lines.push(ui::DbgLine::plain(row.join("   ")));
                    }
                }
                lines.push(ui::DbgLine::plain(""));
                // "How did I get here": the most recent retired PCs
                // (oldest first; the console's HISTORY command shows the
                // full ring with disassembly).
                let history = machine.ui_pc_history();
                if !history.is_empty() {
                    let recent: Vec<String> = history
                        .iter()
                        .rev()
                        .take(8)
                        .rev()
                        .map(|pc| format!("{pc:06X}"))
                        .collect();
                    lines.push(ui::DbgLine::plain(format!("recent {}", recent.join(" "))));
                    lines.push(ui::DbgLine::plain(""));
                }
                if let Some(origin) = panel.disasm_addr {
                    lines.push(ui::DbgLine::plain(format!(
                        "Disassembly whdload_entry at ${origin:06X} (empty box + Enter follows PC)"
                    )));
                }
                let breaks = machine.ui_breaks();
                let mut addr = panel.disasm_addr.unwrap_or(pc) & !1;
                for _ in 0..24 {
                    let (text, len) = crate::disasm::disassemble(read, addr, machine.cpu_type());
                    // A leading bullet marks a line that carries a breakpoint.
                    let marker = if breaks.is_breakpoint(addr) { "*" } else { " " };
                    let line = format!("{marker}{addr:08X}  {text}");
                    lines.push(if addr == pc {
                        ui::DbgLine::hilit(line)
                    } else {
                        ui::DbgLine::plain(line)
                    });
                    addr = addr.wrapping_add(len);
                }
            }
            ui::DebugTab::Chipset => {
                let agnus = &bus.agnus;
                let base = bus.current_render_base();
                let intreq = bus.cpu_visible_intreq();
                let intena = bus.paula.intena;
                lines.push(ui::DbgLine::hilit(format!(
                    "Beam vpos {:>3} hpos {:>3}   frame {}",
                    agnus.vpos,
                    agnus.hpos,
                    bus.emulated_frames()
                )));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(format!(
                    "DMACON {:04X}  {}",
                    agnus.dmacon,
                    ui::dmacon_flags(agnus.dmacon)
                )));
                lines.push(ui::DbgLine::plain(format!(
                    "INTENA {:04X}  {}",
                    intena,
                    ui::int_flags(intena)
                )));
                lines.push(ui::DbgLine::plain(format!(
                    "INTREQ {:04X}  {}",
                    intreq,
                    ui::int_flags(intreq)
                )));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(format!(
                    "COP1LC {:06X}   COP2LC {:06X}   COPPC {:06X} ({})",
                    agnus.cop1lc,
                    agnus.cop2lc,
                    bus.copper.pc(),
                    if bus.copper.is_running() {
                        "running"
                    } else {
                        "stopped"
                    }
                )));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(format!(
                    "BPLCON0 {:04X}  BPLCON1 {:04X}  BPLCON2 {:04X}  FMODE {:04X}",
                    base.bplcon0, base.bplcon1, base.bplcon2, base.fmode
                )));
                lines.push(ui::DbgLine::plain(format!(
                    "DIWSTRT {:04X}  DIWSTOP {:04X}  DDFSTRT {:04X}  DDFSTOP {:04X}",
                    base.diwstrt, base.diwstop, base.ddfstrt, base.ddfstop
                )));
                lines.push(ui::DbgLine::plain(format!(
                    "BPL1MOD {}  BPL2MOD {}",
                    base.bpl1mod, base.bpl2mod
                )));
                lines.push(ui::DbgLine::plain(""));
                for (label, ptrs) in [("BPLPT", &base.bplpt), ("SPRPT", &base.sprpt)] {
                    let row: Vec<String> = ptrs.iter().map(|p| format!("{p:06X}")).collect();
                    lines.push(ui::DbgLine::plain(format!("{label} {}", row.join(" "))));
                }
                lines.push(ui::DbgLine::plain(""));
                let colors = base.palette.hi_words();
                for half in 0..2 {
                    let row: Vec<String> = (0..16)
                        .map(|i| format!("{:03X}", colors[half * 16 + i] & 0x0FFF))
                        .collect();
                    lines.push(ui::DbgLine::plain(format!(
                        "COLOR{:02} {}",
                        half * 16,
                        row.join(" ")
                    )));
                }
            }
            ui::DebugTab::Video => {
                let base = bus.frame_render_base();
                let aga = base.agnus_revision == crate::chipset::agnus::AgnusRevision::AgaAlice;
                let bplcon0 = base.bplcon0;
                let nplanes =
                    (((bplcon0 >> 12) & 7) as usize + (((bplcon0 >> 4) & 1) as usize * 8)).min(8);
                let res = if bplcon0 & 0x0040 != 0 {
                    "shres"
                } else if bplcon0 & 0x8000 != 0 {
                    "hires"
                } else {
                    "lores"
                };
                let mut modes = String::new();
                if bplcon0 & 0x0800 != 0 {
                    modes.push_str("  HAM");
                }
                if bplcon0 & 0x0400 != 0 {
                    modes.push_str("  DPF");
                }
                let header = format!(
                    "BPLCON0 {bplcon0:04X}: {nplanes} planes {res}{modes}   DMACON: BPLEN {} SPREN {}",
                    if base.dmacon & 0x0100 != 0 {
                        "on"
                    } else {
                        "off"
                    },
                    if base.dmacon & 0x0020 != 0 {
                        "on"
                    } else {
                        "off"
                    },
                );
                let captured = bus.frame_captured_sprite_lines();
                let sprites = (0..8)
                    .map(|sprite| {
                        let pos = base.sprpos[sprite];
                        let ctl = base.sprctl[sprite];
                        let vstart = (pos >> 8) | ((ctl & 0x04) << 6);
                        let vstop = (ctl >> 8) | ((ctl & 0x02) << 7);
                        let hstart = ((pos & 0xFF) << 1) | (ctl & 0x01);
                        let attached = ctl & 0x80 != 0;
                        let mut dma_lines: Vec<&crate::bus::CapturedSpriteLine> = captured
                            .iter()
                            .filter(|line| line.sprite == sprite)
                            .collect();
                        dma_lines.sort_by_key(|line| line.beam_y);
                        let text = format!(
                            "SPR{sprite} v{vstart}-{vstop} h{hstart}{}{}  dma lines {}",
                            if attached { " att" } else { "" },
                            if base.spr_armed[sprite] { " armed" } else { "" },
                            dma_lines.len(),
                        );
                        // Thumbnail: sample the DMA lines to the thumb
                        // height; classic 2-bpp decode against the pair's
                        // frame-start palette bank (an attached pair or an
                        // AGA BPLCON4 bank shifts real colours, but shape
                        // is what the thumbnail is for).
                        let total = dma_lines.len();
                        let rows = total.min(ui::VIDEO_THUMB_MAX_ROWS);
                        let mut thumb = vec![0u32; rows * 16];
                        for row in 0..rows {
                            let line = dma_lines[row * total / rows.max(1)];
                            for x in 0..16usize {
                                let bit = 15 - x;
                                let idx =
                                    ((line.data >> bit) & 1) | ((((line.datb) >> bit) & 1) << 1);
                                if idx == 0 {
                                    continue;
                                }
                                let entry = 16 + (sprite / 2) * 4 + idx as usize;
                                let rgb = base.palette.rgb24(entry);
                                thumb[row * 16 + x] =
                                    rgba((rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF);
                            }
                        }
                        ui::SpriteRowView {
                            text,
                            thumb,
                            thumb_rows: rows,
                        }
                    })
                    .collect();
                let palette_entries = if aga { 256 } else { 32 };
                let palette = (0..palette_entries)
                    .map(|entry| {
                        let rgb = base.palette.rgb24(entry);
                        rgba((rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF)
                    })
                    .collect();
                let masks = bus.ui_layer_masks();
                video = Some(ui::VideoView {
                    header,
                    plane_mask: masks.planes,
                    nplanes,
                    sprite_mask: masks.sprites,
                    sprites,
                    palette,
                });
            }
            ui::DebugTab::Copper => {
                // Leave room for the CBreak/CStep buttons drawn at the top
                // of the content area.
                for _ in 0..ui::COPPER_TAB_HEADER_LINES {
                    lines.push(ui::DbgLine::plain(""));
                }
                let agnus = &bus.agnus;
                // Anchor the listing on the current instruction's start:
                // mid-instruction the PC already points at the second word.
                let anchor = bus
                    .copper
                    .pc()
                    .wrapping_sub(if bus.copper.mid_instruction() { 2 } else { 0 });
                let state = if bus.copper.is_running() {
                    "running".to_string()
                } else if let Some(wait) = bus.copper.waiting() {
                    let pos = wait.position_bits();
                    format!("waiting v{} h{}", (pos >> 8) & 0xFF, pos & 0xFE)
                } else {
                    "stopped".to_string()
                };
                lines.push(ui::DbgLine::plain(format!(
                    "COP1LC {:06X}   COP2LC {:06X}   COPPC {:06X} ({state})",
                    agnus.cop1lc,
                    agnus.cop2lc,
                    bus.copper.pc(),
                )));
                lines.push(ui::DbgLine::plain(""));
                // Follow the live Copper around its PC (a stopped Copper
                // shows the head of the COP1 list instead). Breakpointed
                // addresses are marked with `*`.
                let stopped = !bus.copper.is_running() && bus.copper.waiting().is_none();
                let start = if stopped {
                    agnus.cop1lc
                } else {
                    anchor.saturating_sub(5 * 4)
                };
                let cbreaks = bus.ui_copper_breaks();
                for (addr, text) in crate::disasm::dump_copper_list(read, start, 30) {
                    let marker = if cbreaks.contains(&addr) { "*" } else { " " };
                    let line = format!("{marker}{addr:06X}  {text}");
                    lines.push(if !stopped && addr == anchor {
                        ui::DbgLine::hilit(line)
                    } else {
                        ui::DbgLine::plain(line)
                    });
                }
            }
            ui::DebugTab::Audio => {
                let dmacon = bus.agnus.dmacon;
                let master = dmacon & 0x0200 != 0; // DMACON DMAEN
                let adkcon = bus.paula.adkcon;
                // Audio interrupt-pending latches live in INTREQ bits 7..10
                // (AUD0..AUD3); use the CPU-visible copy like the Chipset tab.
                let intreq = bus.cpu_visible_intreq();
                // Per-channel AUDxEN bit (AUD0..AUD3 = bits 0..3).
                let auden: Vec<&str> = (0..4)
                    .map(|ch| if dmacon & (1 << ch) != 0 { "1" } else { "." })
                    .collect();
                let header = format!(
                    "DMACON {:04X}  DMAEN {}  AUDEN {}   ADKCON {:04X}  {}",
                    dmacon,
                    if master { "on" } else { "off" },
                    auden.join(" "),
                    adkcon,
                    ui::adkcon_audio_flags(adkcon),
                );
                let mut channels: Vec<ui::AudioRowView> = Vec::with_capacity(4);
                for ch in 0..4 {
                    let Some(a) = bus.paula.audio_channel_debug(ch) else {
                        continue;
                    };
                    let dma_on = master && (dmacon & (1 << ch)) != 0;
                    let mut text: Vec<ui::DbgLine> = Vec::new();
                    let head = format!(
                        "AUD{} [{}]  DMA {}  IRQ {}",
                        ch,
                        a.state,
                        if dma_on { "on" } else { "off" },
                        if intreq & (1 << (7 + ch)) != 0 {
                            "pend"
                        } else {
                            "-"
                        },
                    );
                    // Highlight a channel that is actively streaming samples.
                    text.push(if a.playing {
                        ui::DbgLine::hilit(head)
                    } else {
                        ui::DbgLine::plain(head)
                    });
                    text.push(ui::DbgLine::plain(format!(
                        "  LC {:06X}  LEN {:04X}  PER {:04X}  VOL {:02X}",
                        a.lc, a.len, a.per, a.vol
                    )));
                    text.push(ui::DbgLine::plain(format!(
                        "  PTR {:06X}  cnt {:04X}  percnt {:05X}  vol {:02X}  out {}",
                        a.ptr, a.audlen, a.percnt, a.audvol, a.current
                    )));
                    let mut pending: Vec<&str> = Vec::new();
                    if a.intreq2 {
                        pending.push("intreq2");
                    }
                    if a.sm_request {
                        pending.push("dma-req");
                    }
                    if a.agnus_request {
                        pending.push("dma-req-latched");
                    }
                    if !pending.is_empty() {
                        text.push(ui::DbgLine::plain(format!(
                            "  pending: {}",
                            pending.join(" ")
                        )));
                    }
                    channels.push(ui::AudioRowView {
                        text,
                        muted: bus.paula.channel_muted(ch),
                        scope: bus.paula.audio_scope_samples(ch),
                    });
                }
                // One row per line-mixed source, in the same order the
                // mute-click dispatcher maps clicks back through.
                let mut extras: Vec<ui::AudioExtraRow> = Vec::new();
                for kind in Self::audio_extra_kinds(bus) {
                    let row = match kind {
                        ui::AudioExtraKind::Cd => {
                            let cd_scope = bus.paula.cd_scope_samples();
                            let cd_active = cd_scope.iter().any(|&s| s != 0);
                            let cd_peak = cd_scope
                                .iter()
                                .map(|&s| (s as i16).abs())
                                .max()
                                .unwrap_or(0);
                            // A SCSI CD-ROM drive reports its play operation
                            // (state, track, position); the CDTV/CD32
                            // controllers stream without one, so their row
                            // keeps the scope-derived state.
                            let cd_status = bus.scsi_cd_playback_line().unwrap_or_else(|| {
                                if cd_active { "playing" } else { "idle" }.to_string()
                            });
                            ui::AudioRowView {
                                text: vec![
                                    ui::DbgLine::hilit(format!("CD-DA  {cd_status}")),
                                    ui::DbgLine::plain(format!("  peak {cd_peak:>3}")),
                                ],
                                muted: bus.paula.cd_muted(),
                                scope: cd_scope,
                            }
                        }
                        ui::AudioExtraKind::Synth => {
                            #[cfg(feature = "midi")]
                            {
                                let scope = bus.paula.synth_scope_samples();
                                let sounding = scope.iter().any(|&s| s != 0);
                                let peak =
                                    scope.iter().map(|&s| (s as i16).abs()).max().unwrap_or(0);
                                let label = bus
                                    .midi_serial()
                                    .map(Self::midi_synth_label)
                                    .unwrap_or_else(|| "synth".to_string());
                                let head = format!(
                                    "MIDI  {label}  {}",
                                    if sounding { "sounding" } else { "idle" }
                                );
                                ui::AudioRowView {
                                    text: vec![
                                        if sounding {
                                            ui::DbgLine::hilit(head)
                                        } else {
                                            ui::DbgLine::plain(head)
                                        },
                                        ui::DbgLine::plain(format!("  peak {peak:>3}")),
                                    ],
                                    muted: bus.paula.synth_muted(),
                                    scope,
                                }
                            }
                            #[cfg(not(feature = "midi"))]
                            unreachable!("synth row is only offered with the midi feature")
                        }
                        ui::AudioExtraKind::Toccata => {
                            let Some(board) = bus.toccata_board() else {
                                continue;
                            };
                            let d = board.debug_status();
                            let head =
                                format!("Toccata  {}", if d.playing { "playing" } else { "idle" });
                            ui::AudioRowView {
                                text: vec![
                                    if d.playing {
                                        ui::DbgLine::hilit(head)
                                    } else {
                                        ui::DbgLine::plain(head)
                                    },
                                    ui::DbgLine::plain(format!(
                                        "  {} Hz {} {}  FIFO {:>4}/{}",
                                        d.rate_hz,
                                        if d.sixteen_bit { "16-bit" } else { "8-bit" },
                                        if d.channels == 2 { "stereo" } else { "mono" },
                                        d.fifo_len,
                                        d.fifo_capacity,
                                    )),
                                ],
                                muted: bus.paula.toccata_muted(),
                                scope: bus.paula.toccata_scope_samples(),
                            }
                        }
                        ui::AudioExtraKind::Mhi => {
                            #[cfg(feature = "mhi")]
                            {
                                let Some(board) = bus.mhi_board() else {
                                    continue;
                                };
                                let d = board.debug_status();
                                let head = format!("MHI  {}  {} Hz", d.state, d.native_rate);
                                ui::AudioRowView {
                                    text: vec![
                                        if d.state == "playing" {
                                            ui::DbgLine::hilit(head)
                                        } else {
                                            ui::DbgLine::plain(head)
                                        },
                                        ui::DbgLine::plain(format!(
                                            "  queue {}  vol {}  pan {}  B/M/T {}/{}/{}",
                                            d.queued, d.volume, d.panning, d.bass, d.mid, d.treble,
                                        )),
                                    ],
                                    muted: bus.paula.mhi_muted(),
                                    scope: bus.paula.mhi_scope_samples(),
                                }
                            }
                            #[cfg(not(feature = "mhi"))]
                            unreachable!("MHI row is only offered with the mhi feature")
                        }
                    };
                    extras.push(ui::AudioExtraRow { kind, row });
                }
                // Mirror the text into `lines` for the headless/text fallback
                // and the non-empty-view invariant; the tab itself is drawn
                // graphically from the structured view.
                lines.push(ui::DbgLine::hilit(header.clone()));
                for row in channels.iter().chain(extras.iter().map(|extra| &extra.row)) {
                    lines.push(ui::DbgLine::plain(""));
                    lines.extend(row.text.iter().cloned());
                }
                audio = Some(ui::AudioScopeView {
                    header,
                    channels,
                    extras,
                });
            }
            ui::DebugTab::Memory => {
                // Leave room for the Find/Save/Writer/Bits buttons drawn at
                // the top of the content area.
                for _ in 0..ui::MEM_TAB_HEADER_LINES {
                    lines.push(ui::DbgLine::plain(""));
                }
                if panel.mem_view_bits {
                    let stride = panel.mem_bitmap_stride.max(1) as usize;
                    let rows = ui::mem_bitmap_rows();
                    let base = panel.mem_addr & machine.ui_addr_mask();
                    lines.push(ui::DbgLine::plain(format!(
                        "bitplane at ${base:06X}, stride {stride} bytes ({} px), {rows} rows",
                        stride * 8
                    )));
                    bitmap = Some(ui::MemBitmapView {
                        base,
                        stride,
                        rows,
                        data: machine.debug_read_memory(base, stride * rows),
                    });
                } else {
                    lines.push(ui::DbgLine::plain(
                        "$ box: jump / \"ADDR VALUE\" poke / \"ADDR LEN\" save / hex bytes find",
                    ));
                    lines.push(ui::DbgLine::plain(""));
                    let base = panel.mem_addr & machine.ui_addr_mask() & !0xF;
                    for row in 0..16u32 {
                        let addr = base.wrapping_add(row * 16) & machine.ui_addr_mask();
                        let mut bytes = [0u8; 16];
                        for word in 0..8u32 {
                            let value = bus.peek_word_any(addr.wrapping_add(word * 2));
                            bytes[word as usize * 2] = (value >> 8) as u8;
                            bytes[word as usize * 2 + 1] = value as u8;
                        }
                        lines.push(ui::DbgLine::plain(ui::hex_dump_row(addr, &bytes)));
                    }
                }
            }
            ui::DebugTab::IoMap => {
                const ROWS: usize = 26;
                const COLS: usize = 3;
                const PER_PAGE: usize = ROWS * COLS;
                let sel = usize::from(panel.iomap_sel & 0x1FE) / 2;
                let page = sel / PER_PAGE;
                lines.push(ui::DbgLine::plain(format!(
                    "custom registers $DFF000-$DFF1FE  (page {}/{}; arrows/wheel move, $ box jumps)",
                    page + 1,
                    256usize.div_ceil(PER_PAGE)
                )));
                lines.push(ui::DbgLine::plain(""));
                for row in 0..ROWS {
                    let mut text = String::new();
                    let mut row_has_sel = false;
                    for col in 0..COLS {
                        let idx = page * PER_PAGE + col * ROWS + row;
                        if idx >= 256 {
                            continue;
                        }
                        let off = (idx * 2) as u16;
                        let value = bus
                            .debug_custom_word(off)
                            .map(|v| format!("{v:04X}"))
                            .unwrap_or_else(|| "----".to_string());
                        let cursor = if idx == sel {
                            row_has_sel = true;
                            '>'
                        } else {
                            ' '
                        };
                        text.push_str(&format!(
                            "{cursor}{off:03X} {:<8} {value}   ",
                            crate::debugger::custom_reg_name(off)
                        ));
                    }
                    let text = text.trim_end().to_string();
                    lines.push(if row_has_sel {
                        ui::DbgLine::hilit(text)
                    } else {
                        ui::DbgLine::plain(text)
                    });
                }
                lines.push(ui::DbgLine::plain(""));
                let off = panel.iomap_sel & 0x1FE;
                let value = bus.debug_custom_word(off);
                lines.push(ui::DbgLine::hilit(format!(
                    "${off:03X} {} = {}",
                    crate::debugger::custom_reg_name(off),
                    value
                        .map(|v| format!("${v:04X}"))
                        .unwrap_or_else(|| "(no latch)".to_string())
                )));
                if let Some(value) = value {
                    for line in crate::debugger::custom_reg_bit_decode(off, value) {
                        lines.push(ui::DbgLine::plain(format!("  {line}")));
                    }
                }
            }
            ui::DebugTab::Break => {
                // Leave room for the toggle buttons drawn at the top of
                // the content area.
                for _ in 0..ui::BREAK_TAB_HEADER_LINES {
                    lines.push(ui::DbgLine::plain(""));
                }
                if let Some(stop) = &self.last_debug_stop {
                    lines.push(ui::DbgLine::hilit(format!("Stopped: {stop}")));
                    lines.push(ui::DbgLine::plain(""));
                }
                lines.push(ui::DbgLine::plain(
                    "Type a hex address in the $ box, then a toggle button.",
                ));
                lines.push(ui::DbgLine::plain(
                    "Reg takes a custom-register offset (96) or address (DFF096).",
                ));
                lines.push(ui::DbgLine::plain(
                    "Break cond: ADDR [LHS OP RHS] [IGN N]  e.g. C033C2 D0 EQ 5",
                ));
                lines.push(ui::DbgLine::plain(
                    "  ops EQ NE LT GT LE GE AND; operand Dn An PC SR Mhex hex",
                ));
                lines.push(ui::DbgLine::plain(
                    "Beam takes decimal \"VPOS [HPOS]\" (stop when the beam gets there).",
                ));
                lines.push(ui::DbgLine::plain(
                    "Catch takes \"irq N\", \"trap N\", or \"vec N\" (stop entering the vector).",
                ));
                lines.push(ui::DbgLine::plain(""));
                let breaks = self.emu.machine.ui_breaks();
                lines.push(ui::DbgLine::plain("Breakpoints:"));
                if breaks.breakpoints.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for bp in &breaks.breakpoints {
                    let mut text = format!("  ${:06X}", bp.addr);
                    if let Some(cond) = &bp.cond {
                        text.push_str(&format!("  {}", cond.describe()));
                    }
                    if bp.ignore > 0 {
                        text.push_str(&format!("  ign {}/{}", bp.hits, bp.ignore));
                    }
                    lines.push(ui::DbgLine::plain(text));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain("Watchpoints (word, stop on change):"));
                if breaks.watches.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for watch in &breaks.watches {
                    lines.push(ui::DbgLine::plain(format!(
                        "  ${:06X}  now {:04X}{}",
                        watch.addr,
                        bus.peek_word_any(watch.addr),
                        watch
                            .filter
                            .map(|f| format!("  [{} only]", f.label()))
                            .unwrap_or_default()
                    )));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain("Register watches (stop on write):"));
                if breaks.reg_watches.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for off in &breaks.reg_watches {
                    lines.push(ui::DbgLine::plain(format!(
                        "  {} (${off:03X})",
                        crate::debugger::custom_reg_name(*off)
                    )));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Exception catchpoints (stop entering the vector):",
                ));
                if breaks.catches.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for vector in &breaks.catches {
                    lines.push(ui::DbgLine::plain(format!(
                        "  {} (vector {vector})",
                        crate::debugger::exception_vector_name(*vector)
                    )));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Copper breakpoints (set on Copper tab):",
                ));
                let cbreaks = bus.ui_copper_breaks();
                if cbreaks.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for addr in cbreaks {
                    lines.push(ui::DbgLine::plain(format!("  ${addr:06X}")));
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain("Beam traps (stop at beam position):"));
                let beam_traps = bus.ui_beam_traps();
                if beam_traps.is_empty() {
                    lines.push(ui::DbgLine::plain("  (none)"));
                }
                for trap in beam_traps {
                    let mut text = format!("  v{}", trap.vpos);
                    if let Some(hpos) = trap.hpos {
                        text.push_str(&format!(" h{hpos}"));
                    } else {
                        text.push_str(" (line start)");
                    }
                    if trap.once {
                        text.push_str("  once");
                    }
                    lines.push(ui::DbgLine::plain(text));
                }
            }
            ui::DebugTab::Waveform => {
                // Leave room for the Arm/Stop buttons drawn at the top of
                // the content area.
                for _ in 0..ui::WAVEFORM_TAB_HEADER_LINES {
                    lines.push(ui::DbgLine::plain(""));
                }
                match self.emu.machine.ui_wave_status() {
                    Some(status) => {
                        for (index, text) in
                            console::wave_status_lines(&status).into_iter().enumerate()
                        {
                            lines.push(if index == 0 {
                                ui::DbgLine::hilit(text)
                            } else {
                                ui::DbgLine::plain(text)
                            });
                        }
                    }
                    None => lines.push(ui::DbgLine::plain("No waveform capture.")),
                }
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Arm records chipset signals to a VCD file for GTKWave.",
                ));
                lines.push(ui::DbgLine::plain(
                    "Type an order-free spec in the box, then Arm. Empty = defaults",
                ));
                lines.push(ui::DbgLine::plain(
                    "(trigger now, 1 frame, all signals, copperline-wave-*.vcd).",
                ));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Trigger:  NOW  PC=ADDR  BEAM=VPOS[:HPOS]  REG=OFF  TIME=SECS",
                ));
                lines.push(ui::DbgLine::plain(
                    "Duration: Ncck (bare N)  Nf (frames)  Nms  Ns",
                ));
                lines.push(ui::DbgLine::plain(
                    "Signals:  comma list of beam,bus,cpu,copper,blitter,regs,irq,audio",
                ));
                lines.push(ui::DbgLine::plain(
                    "Path:     any other token (e.g. OUT.VCD)",
                ));
                lines.push(ui::DbgLine::plain(""));
                lines.push(ui::DbgLine::plain(
                    "Example:  OUT.VCD PC=C033C2 20000CCK CPU,BUS,COPPER",
                ));
                lines.push(ui::DbgLine::plain(
                    "The console WAVE command does the same (Cmd/Alt+K).",
                ));
            }
        }
        // Keep lines inside the panel; the blitter clips at the texture
        // edge, not the panel edge.
        for line in &mut lines {
            if line.text.len() > 82 {
                line.text.truncate(82);
            }
        }
        ui::DebuggerView {
            running: !self.paused,
            reverse_available: self.emu.time_travel_enabled(),
            status,
            lines,
            bitmap,
            video,
            audio,
        }
    }
}
