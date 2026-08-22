// SPDX-License-Identifier: GPL-3.0-or-later

//! The pop-up menu tree and main-window UI control/key dispatch.

use super::*;

impl App {
    /// The menu as it stands for this machine, built when it is opened.
    ///
    /// Everything the tree needs is read here, once: the tree is then a plain
    /// value, so nothing it offers can shift under the pointer while it is
    /// up, and the drawing code never reaches into the machine.
    pub(super) fn build_menu(&mut self, fullscreen: bool) -> Vec<crate::video::menu::MenuRow> {
        use crate::video::menu::{AudioOutputChoice, MenuState};

        let midi_active = self.serial_is_midi;
        let (midi_in, midi_out) = if midi_active {
            self.midi_menu_labels()
        } else {
            (String::new(), String::new())
        };
        #[cfg(feature = "midi")]
        let (midi_inputs, midi_outputs) = if midi_active {
            let ends = crate::midi::enumerate();
            (
                ends.inputs.into_iter().map(|e| e.name).collect::<Vec<_>>(),
                ends.outputs.into_iter().map(|e| e.name).collect::<Vec<_>>(),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        #[cfg(not(feature = "midi"))]
        let (midi_inputs, midi_outputs): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());

        let sampler_active = self.sampler_stream.is_some();
        let sampler_inputs = if sampler_active {
            crate::sampler::picker_input_devices()
        } else {
            Vec::new()
        };

        let audio_output = match &self.audio_output {
            crate::audio::AudioOutput::Disabled => AudioOutputChoice::Disabled,
            crate::audio::AudioOutput::Device(name) => AudioOutputChoice::Named(name.clone()),
            crate::audio::AudioOutput::Default => AudioOutputChoice::Default,
        };

        // Whether the MT-32 is the chosen output, whether the unit is
        // actually running, whether its own MIDI OUT is wired back to the
        // machine, and which ROM images it holds.
        #[cfg(feature = "mt32")]
        let (mt32_selected, mt32_attached, mt32_input, mt32_control_rom, mt32_pcm_rom) = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .map_or((false, false, false, None, None), |sink| {
                let rom_name = |p: &Option<std::path::PathBuf>| {
                    p.as_deref()
                        .and_then(std::path::Path::file_name)
                        .map(|n| n.to_string_lossy().into_owned())
                };
                let roms = sink.mt32_roms();
                let (control, pcm) = (rom_name(&roms.control), rom_name(&roms.pcm));
                (
                    sink.mt32_selected(),
                    sink.mt32().is_some(),
                    sink.mt32_input(),
                    control,
                    pcm,
                )
            });
        #[cfg(not(feature = "mt32"))]
        let (mt32_selected, mt32_attached, mt32_input, mt32_control_rom, mt32_pcm_rom) =
            (false, false, false, None::<String>, None::<String>);
        // Selected, not powered: the Front Panel row stays reachable while
        // the unit is switched off at its own fascia.
        #[cfg(feature = "coppersynth")]
        let csynth_attached = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.csynth_selected());
        #[cfg(not(feature = "coppersynth"))]
        let csynth_attached = false;
        #[cfg(feature = "coppersynth")]
        let csynth_mt32_mode = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .map(|sink| sink.csynth_mt32_mode().to_string())
            .unwrap_or_default();
        #[cfg(not(feature = "coppersynth"))]
        let csynth_mt32_mode = String::new();
        #[cfg(feature = "coppersynth")]
        let csynth_custom_font = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.csynth_custom_soundfont());
        #[cfg(not(feature = "coppersynth"))]
        let csynth_custom_font = false;

        let save_slots = self.save_slot_stamps();
        let state = MenuState {
            player: crate::video::player_profile(),
            player_save_states: crate::video::player_save_states(),
            paused: self.paused,
            fullscreen,
            status_bar_hidden: crate::video::status_bar_hidden(),
            bezel: self.bezel,
            perf_overlay: self.perf_overlay,
            warp: !self.emu.paced(),
            warp_speed: self.warp_speed,
            rewind: self.rewind_armed,
            recording: self.recorder.is_some(),
            input_recording: self.input_recorder.is_some(),
            autofire_hz: self.autofire_hz,
            run_ahead_frames: self.run_ahead_frames,
            joystick_input_mode: self.joystick_input_mode,
            port_devices: [
                self.emu.bus().input.device(0),
                self.emu.bus().input.device(1),
            ],
            pixel_aspect: crate::video::pixel_aspect(),
            scaling: crate::video::display_scaling(),
            tv_centre: self.tv_centre,
            tv_centre_applies: self.overscan == Overscan::Tv,
            shader: self.crt_shader_kind,
            shader_strength: self.shader_strength,
            custom_shader_available: self.custom_shader_path.is_some(),
            tint: self.tint,
            menu_scale: crate::video::menu_scale(),
            floppy_speed: self.emu.bus().floppy.speed_percent(),
            floppy_speed_applies: self.emu.bus().floppy.has_image_drive(),
            audio_filter: self.emu.bus().paula.led_filter_mode(),
            audio_output,
            audio_devices: &crate::audio::picker_output_devices(),
            midi_in: &midi_in,
            midi_out: &midi_out,
            midi_inputs: &midi_inputs,
            midi_outputs: &midi_outputs,
            // Offered while the serial port is in MIDI mode at all --
            // the same gate as the host endpoint lists above.
            mt32_available: cfg!(feature = "mt32") && midi_active,
            mt32_selected,
            mt32_attached,
            mt32_input,
            mt32_control_rom,
            mt32_pcm_rom,
            mt32_panel: crate::video::mt32_panel_shown(),
            csynth_available: cfg!(feature = "coppersynth") && midi_active,
            csynth_attached,
            csynth_panel: crate::video::csynth_panel_shown(),
            csynth_mt32_mode: &csynth_mt32_mode,
            csynth_custom_font,
            keyboard_panel: crate::video::keyboard_panel_shown(),
            mt32_lcd: crate::video::mt32_lcd(),
            sampler_input: self.sampler.input_device.as_deref().unwrap_or(""),
            sampler_inputs: &sampler_inputs,
            sampler_gain: self.sampler.gain_db,
            save_slots: &save_slots,
        };
        crate::video::menu::build(&state)
    }

    /// When each numbered save slot was written, for the Quick Save/Load
    /// rows. A slot that cannot be read is treated as free: the menu is
    /// describing what is there, not diagnosing the disk.
    pub(super) fn save_slot_stamps(&self) -> [Option<String>; crate::video::menu::SAVE_SLOTS] {
        std::array::from_fn(|i| {
            let path = crate::savestate::slot_path(i + 1)?;
            let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
            Some(crate::timestamp::readable(
                modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            ))
        })
    }

    /// Act on the row the cursor is on: open a category, or run its action.
    ///
    /// Rows meant to be used more than once -- a toggle, a step -- leave the
    /// menu up and rebuild it, so what it shows keeps pace with what it just
    /// changed.
    pub(super) fn activate_menu_row(&mut self, event_loop: Option<&ActiveEventLoop>) {
        let rows = std::mem::take(&mut self.ui.menu_rows);
        let Some(cursor) = self.ui.menu_nav.cursor() else {
            self.ui.menu_rows = rows;
            return;
        };
        let Some(row) = self.ui.menu_nav.current(&rows).get(cursor).cloned() else {
            self.ui.menu_rows = rows;
            return;
        };
        self.ui.menu_rows = rows;
        if !row.enabled {
            return;
        }
        if row.is_submenu() {
            let ui = &mut self.ui;
            ui.menu_nav.descend(&ui.menu_rows);
            self.request_redraw();
            return;
        }
        let Some(action) = row.menu_action().cloned() else {
            return;
        };
        let closes = row.closes_menu();
        if closes {
            self.close_menu();
        }
        self.run_menu_action(action, event_loop);
        if !closes {
            // The menu stays up, so rebuild it: a toggle that has just been
            // flipped should read as flipped.
            let fullscreen = self
                .render
                .as_ref()
                .is_some_and(|r| r.window.fullscreen().is_some());
            self.ui.menu_rows = self.build_menu(fullscreen);
        }
        self.request_redraw();
    }

    /// Carry out a menu action.
    ///
    /// Every row of the tree ends here, so the menu's shape and its effects
    /// stay separable: the tree says what is offered, this says what happens.
    pub(super) fn run_menu_action(
        &mut self,
        action: crate::video::menu::MenuAction,
        event_loop: Option<&ActiveEventLoop>,
    ) {
        use crate::video::menu::{AudioOutputChoice, MenuAction as A};
        match action {
            A::OpenMachineConfig => self.open_launcher(),
            A::OpenFrameAnalyzer => self.open_frame_analyzer(),
            A::OpenDebugger => self.open_debugger(),
            A::OpenConsole => self.open_console(),
            A::OpenInputMapping => self.open_input_mapping(),
            A::OpenCalibration => {
                self.ui.panel = Some(Panel::Calibration(crate::gamepad::CalibrationSession::new()));
            }
            A::OpenShortcuts => self.ui.panel = Some(Panel::Shortcuts),
            A::OpenAbout => {
                self.about_opened_at = std::time::Instant::now();
                self.ui.panel = Some(Panel::About);
            }
            A::LoadRom => self.load_rom_from_dialog(),

            A::SetAudioOutput(choice) => {
                let want = match choice {
                    AudioOutputChoice::Default => crate::audio::AudioOutput::Default,
                    AudioOutputChoice::Named(name) => crate::audio::AudioOutput::Device(name),
                    AudioOutputChoice::Disabled => crate::audio::AudioOutput::Disabled,
                };
                self.audio_output = want;
                let realtime = crate::priority::requested(self.realtime_priority);
                match crate::audio::open_output_sink(realtime, &self.audio_output) {
                    Ok(sink) => {
                        self.emu.bus_mut().paula.audio.set_master(sink);
                        self.sync_live_audio_suspension();
                    }
                    Err(e) => {
                        warn!("audio: could not open the selected device; keeping silence: {e:#}");
                        self.emu
                            .bus_mut()
                            .paula
                            .audio
                            .set_master(Box::new(crate::audio::NullSink));
                    }
                }
                self.show_osd(format!("Audio output: {}", self.audio_output.label()));
            }
            A::SetAudioFilter(mode) => {
                use crate::config::AudioFilterMode;
                self.emu.bus_mut().paula.set_led_filter_mode(mode);
                let label = match mode {
                    AudioFilterMode::Auto => "Auto",
                    AudioFilterMode::On => "Enabled",
                    AudioFilterMode::Off => "Disabled",
                };
                self.show_osd(format!("Audio filter: {label}"));
                self.request_redraw();
            }

            A::SetPixelAspect(aspect) => self.apply_pixel_aspect(aspect),
            A::SetDisplayScaling(scaling) => self.apply_display_scaling(scaling),
            A::StepTvCentre(dh, dv) => self.step_tv_centre(dh, dv),
            A::ResetTvCentre => {
                self.tv_centre = crate::config::TvCentre::default();
                self.show_osd("Centring: centred");
                self.main_presentation_dirty = true;
                self.request_redraw();
            }
            A::SetShader(kind) => {
                use crate::config::ShaderKind;
                // A user shader is re-read from disk each time it is chosen,
                // so editing the file and picking it again shows the new
                // version; one that will not compile falls back to off and
                // says why.
                let mut failure = None;
                let mut applied = kind;
                if kind == ShaderKind::Custom {
                    if let Err(msg) = self.reload_custom_shader() {
                        failure = Some(msg);
                        applied = ShaderKind::None;
                    }
                }
                self.crt_shader_kind = applied;
                info!("crt shader: {}", applied.label());
                self.show_osd(match failure {
                    Some(msg) => {
                        format!("CRT shader: {} (custom failed: {msg})", applied.label())
                    }
                    None => format!("CRT shader: {}", applied.label()),
                });
                self.request_redraw();
            }
            A::SetMenuScale(scale) => {
                crate::video::set_menu_scale(scale);
                self.show_osd(format!("Menu size: {}", scale.menu_label()));
                self.request_redraw();
            }
            A::SetTint(tint) => {
                self.set_tint(tint);
                self.show_osd(format!("Screen tint: {}", tint.label()));
                self.request_redraw();
            }
            A::ToggleFullscreen => self.toggle_fullscreen(),
            A::ToggleStatusBar => self.toggle_status_bar(),
            A::SetBezel(style) => self.set_bezel(style),
            A::TogglePerfOverlay => self.toggle_perf_overlay(),

            A::SetPortDevice(port, device) => {
                self.hot_plug_port_device(port, device);
                self.show_osd(format!("Port {}: {}", port + 1, device.menu_label()));
            }
            A::SetJoystickInput(mode) => self.set_joystick_input_mode(mode),
            A::SetAutofire(hz) => {
                self.autofire_hz = hz;
                let label = crate::config::autofire_label(hz);
                info!("autofire: {label}");
                self.show_osd(format!("Autofire: {label}"));
                self.request_redraw();
            }
            A::SetRunAhead(frames) => {
                self.run_ahead_frames = frames;
                let label = if frames == 0 {
                    "off".to_string()
                } else {
                    format!("{frames} frame{}", if frames == 1 { "" } else { "s" })
                };
                match (frames > 0).then(|| self.runahead_block_reason()).flatten() {
                    Some(reason) => {
                        info!("run-ahead: {label} (inactive: {reason})");
                        self.show_osd(format!("Run Ahead: {label} (inactive: {reason})"));
                    }
                    None => {
                        info!("run-ahead: {label}");
                        self.show_osd(format!("Run Ahead: {label}"));
                    }
                }
                self.request_redraw();
            }
            A::ToggleKeyboardPanel => self.toggle_keyboard_panel(),

            #[cfg(feature = "midi")]
            A::SetMidiInput(name) => {
                let mut shown = "None".to_string();
                if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                    sink.set_input_endpoint(name.as_deref());
                    shown = sink.input_label();
                }
                self.show_osd(format!("MIDI input: {shown}"));
            }
            #[cfg(feature = "midi")]
            A::SetMidiOutput(name) => {
                let mut shown = "None".to_string();
                if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                    sink.set_output_endpoint(name.as_deref());
                    shown = sink.output_label();
                }
                // The device on the port changed, so the mixer has to ask it
                // for audio again -- an MT-32 just attached, or just left.
                self.emu.bus_mut().paula.rearm_synth_audio();
                #[cfg(feature = "mt32")]
                {
                    self.sync_mt32_panel();
                    self.report_mt32_fault();
                }
                #[cfg(feature = "coppersynth")]
                self.sync_csynth_panel();
                self.show_osd(format!("MIDI output: {shown}"));
            }
            #[cfg(not(feature = "midi"))]
            A::SetMidiInput(_) | A::SetMidiOutput(_) => {}
            #[cfg(feature = "mt32")]
            A::ToggleMt32Panel => {
                let shown = !crate::video::mt32_panel_shown();
                self.set_mt32_panel_shown(shown);
                self.show_osd(if shown {
                    "MT-32: front panel shown"
                } else {
                    "MT-32: front panel hidden"
                });
            }
            #[cfg(not(feature = "mt32"))]
            A::ToggleMt32Panel => {}
            #[cfg(feature = "coppersynth")]
            A::ToggleCsynthPanel => {
                let shown = !crate::video::csynth_panel_shown();
                self.set_csynth_panel_shown(shown);
                self.show_osd(if shown {
                    "Coppersynth: front panel shown"
                } else {
                    "Coppersynth: front panel hidden"
                });
            }
            #[cfg(not(feature = "coppersynth"))]
            A::ToggleCsynthPanel => {}
            #[cfg(feature = "coppersynth")]
            A::LoadCsynthSoundfont => self.load_csynth_soundfont(),
            #[cfg(not(feature = "coppersynth"))]
            A::LoadCsynthSoundfont => {}
            #[cfg(feature = "mt32")]
            A::LoadMt32ControlRom => self.load_mt32_rom(true),
            #[cfg(feature = "mt32")]
            A::LoadMt32PcmRom => self.load_mt32_rom(false),
            #[cfg(not(feature = "mt32"))]
            A::LoadMt32ControlRom | A::LoadMt32PcmRom => {}
            #[cfg(feature = "coppersynth")]
            A::ResetCsynthSoundfont => {
                if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                    sink.reset_csynth_soundfont();
                }
                self.emu.bus_mut().paula.rearm_synth_audio();
                let fitted = self
                    .emu
                    .bus_mut()
                    .midi_serial_mut()
                    .is_some_and(|sink| sink.csynth().is_some());
                if fitted {
                    self.show_osd("Coppersynth: GeneralUser-GS");
                } else {
                    self.report_csynth();
                }
            }
            #[cfg(not(feature = "coppersynth"))]
            A::ResetCsynthSoundfont => {}
            #[cfg(feature = "coppersynth")]
            A::SetCsynthMt32Mode(mode) => {
                if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                    sink.set_csynth_mt32_mode(mode);
                }
                let parsed = match mode {
                    "on" => crate::csynth::Mt32Mode::On,
                    "off" => crate::csynth::Mt32Mode::Off,
                    _ => crate::csynth::Mt32Mode::Auto,
                };
                if let Some(synth) = self
                    .emu
                    .bus_mut()
                    .midi_serial_mut()
                    .and_then(crate::midi::MidiSerialSink::csynth_mut)
                {
                    synth.set_mt32_mode(parsed);
                }
                self.show_osd(format!("Coppersynth: MT-32 mode {mode}"));
                self.request_redraw();
            }
            #[cfg(not(feature = "coppersynth"))]
            A::SetCsynthMt32Mode(_) => {}
            #[cfg(feature = "mt32")]
            A::SetMt32Lcd(style) => {
                crate::video::set_mt32_lcd(style);
                self.show_osd(format!("MT-32: {} display", style.menu_label()));
                self.request_redraw();
            }
            #[cfg(not(feature = "mt32"))]
            A::SetMt32Lcd(_) => {}
            A::SetSamplerInput(name) => {
                if self.sampler.enabled {
                    self.sampler.input_device = Some(name.clone());
                    self.attach_session_sampler();
                    self.show_osd(format!("Sampler input: {name}"));
                }
            }
            A::StepSamplerGain(dir) => self.step_sampler_gain(dir > 0),

            A::SetFloppySpeed(percent) => {
                self.emu.bus_mut().floppy.set_speed_percent(percent);
                let label = crate::floppy::speed_label(percent);
                info!("floppy speed: {label}");
                self.show_osd(format!("Floppy speed: {label}"));
                self.request_redraw();
            }
            A::ToggleRewind => self.toggle_rewind(),

            A::ToggleWarp => self.toggle_warp(),
            A::SetWarpLimit(limit) => {
                self.warp_speed = limit;
                let label = limit.label();
                info!("warp limit: {label}");
                if self.emu.paced() {
                    self.show_osd(format!("Warp limit: {label} (warp off)"));
                } else {
                    self.show_osd(format!("Warp limit: {label}"));
                }
                self.request_redraw();
            }

            A::ToggleRecord => self.toggle_recording(),
            A::ToggleRecordInput => self.toggle_input_recording(),

            A::SaveState => self.save_state_interactive(),
            A::LoadState => self.load_state_from_dialog(event_loop),
            A::QuickSave(slot) => self.quick_save_state(slot + 1),
            A::QuickLoad(slot) => self.quick_load_state(slot + 1, event_loop),
            A::StepShaderStrength(dir) => {
                let step = crate::video::menu::SHADER_STRENGTH_STEP;
                self.shader_strength = (self.shader_strength + step * dir as f32).clamp(0.0, 1.0);
                self.show_osd(format!(
                    "Shader strength: {:.0}%",
                    self.shader_strength * 100.0
                ));
                self.request_redraw();
            }
            A::TogglePause => self.toggle_pause(),
            A::ResetMachine => self.reset_emulator(true),
            A::Quit => self.quit_requested = true,
        }
        // A player session persists what its user just set; the full build
        // keeps the menu session-only (a no-op inside).
        self.persist_player_prefs();
    }

    /// Put the menu cursor under the pointer. Returns true when it moved.
    ///
    /// Hovering a row of a level closes anything opened deeper from it: the
    /// lit row and the open trail are then always the same thing, whether the
    /// pointer or the keys put it there.
    pub(super) fn follow_menu_hover(&mut self) -> bool {
        if !self.ui.menu_open || self.ui.menu_rows.is_empty() {
            return false;
        }
        let Some(pos) = self.cursor_pos else {
            return false;
        };
        let pos = (pos.0.max(0) as usize, pos.1.max(0) as usize);
        let Some((depth, row)) = ui::menu_hit(&self.ui.menu_rows, &self.ui.menu_nav, pos) else {
            // Off every row: a submenu arming for its dwell stands down.
            self.menu_hover_arm = None;
            return false;
        };
        // The pointer is resting on the row this level is already open to.
        // Leave it alone: the pointer sits on that row for as long as it takes
        // to set off towards the level it opened, and rebuilding the path here
        // would take that level away before it could be reached.
        if self.ui.menu_nav.open_at(depth) == Some(row) {
            return false;
        }
        if self.ui.menu_nav.depth() == depth && self.ui.menu_nav.cursor() == Some(row) {
            return false;
        }
        let mut path = self.menu_path_to(depth);
        // A category opens as the pointer rests on it, so submenus are
        // walked into rather than clicked into -- after a brief dwell, so
        // a pointer passing through on its way somewhere else does not
        // pull levels open behind it. The cursor stays off the level that
        // opens until the pointer is actually over one of its rows; the
        // category itself stays lit as the way back.
        let opens = self
            .menu_row_at(depth, row)
            .is_some_and(|r| r.enabled && r.is_submenu());
        let cursor = if opens {
            let now = Instant::now();
            match self.menu_hover_arm {
                Some((d, r, since)) if (d, r) == (depth, row) => {
                    if now.duration_since(since) < MENU_SUBMENU_DWELL {
                        // Still waiting: light the row, open nothing yet.
                        self.ui.menu_nav.open_path(path, Some(row));
                        return true;
                    }
                    self.menu_hover_arm = None;
                }
                _ => {
                    self.menu_hover_arm = Some((depth, row, now));
                    self.ui.menu_nav.open_path(path, Some(row));
                    self.request_redraw();
                    return true;
                }
            }
            path.push(row);
            None
        } else {
            self.menu_hover_arm = None;
            Some(row)
        };
        self.ui.menu_nav.open_path(path, cursor);
        true
    }

    /// Open an armed submenu whose dwell has elapsed while the pointer
    /// rests still -- resting produces no cursor events, so the frame
    /// poll asks.
    pub(super) fn poll_menu_hover_arm(&mut self) {
        let Some((depth, row, since)) = self.menu_hover_arm else {
            return;
        };
        if !self.ui.menu_open {
            self.menu_hover_arm = None;
            return;
        }
        if since.elapsed() < MENU_SUBMENU_DWELL {
            self.request_redraw();
            return;
        }
        // Only if the pointer is still on the armed row.
        let still_there = self
            .cursor_pos
            .map(|p| (p.0.max(0) as usize, p.1.max(0) as usize))
            .and_then(|pos| ui::menu_hit(&self.ui.menu_rows, &self.ui.menu_nav, pos))
            == Some((depth, row));
        self.menu_hover_arm = None;
        if !still_there {
            return;
        }
        let mut path = self.menu_path_to(depth);
        path.push(row);
        self.ui.menu_nav.open_path(path, None);
        self.request_redraw();
    }

    /// The open path down to `depth`, for a pointer that has landed on a level
    /// without stepping through its parents.
    pub(super) fn menu_path_to(&self, depth: usize) -> Vec<usize> {
        (0..depth)
            .filter_map(|d| self.ui.menu_nav.open_at(d))
            .collect()
    }

    /// The row at `row` on level `depth` of the open menu.
    pub(super) fn menu_row_at(
        &self,
        depth: usize,
        row: usize,
    ) -> Option<&crate::video::menu::MenuRow> {
        let levels = self.ui.menu_nav.levels(&self.ui.menu_rows);
        let level: &[crate::video::menu::MenuRow] = levels.get(depth)?;
        level.get(row)
    }

    /// Walk the open menu with the keyboard. Returns true when the key was
    /// the menu's, so the caller stops before the machine sees it.
    ///
    /// Up and down step the current level, right opens a category and left
    /// leaves one, Return picks, and Escape backs out a level at a time --
    /// closing the menu from the top, which is where Escape has nothing left
    /// to close.
    pub(super) fn handle_menu_key(&mut self, code: KeyCode, event_loop: &ActiveEventLoop) -> bool {
        use crate::video::nav::Dir;
        match code {
            // The menu is a surface like any other: the arrows go to the
            // focus, which knows the menu keeps its own cursor and where
            // walking off the foot of it leads.
            KeyCode::ArrowUp => return self.nav_move(Dir::Up, Some(event_loop)),
            KeyCode::ArrowDown => return self.nav_move(Dir::Down, Some(event_loop)),
            KeyCode::ArrowRight => return self.nav_move(Dir::Right, Some(event_loop)),
            KeyCode::ArrowLeft => return self.nav_move(Dir::Left, Some(event_loop)),
            KeyCode::Enter | KeyCode::NumpadEnter => {
                self.activate_menu_row(Some(event_loop));
                self.ensure_tool_windows_for_open_panels(event_loop);
                return true;
            }
            KeyCode::Escape => {
                if !self.ui.menu_nav.ascend() {
                    self.close_menu();
                }
            }
            _ => return false,
        }
        self.request_redraw();
        true
    }

    /// Close the menu and forget where it was open to.
    pub(super) fn close_menu(&mut self) {
        self.menu_hover_arm = None;
        self.ui.menu_open = false;
        self.ui.menu_rows = Vec::new();
        self.ui.menu_nav.reset();
    }

    /// Run the action behind a clicked status-bar control (volume is
    /// handled separately because it starts a drag).
    pub(super) fn activate_bar_control(&mut self, control: BarControl) {
        match control {
            BarControl::Power => self.toggle_power(),
            BarControl::Pause => self.toggle_pause(),
            BarControl::Reboot => self.reset_emulator(true),
            BarControl::Screenshot => self.take_screenshot(),
            BarControl::Menu => self.toggle_menu(),
            // A bridged drive's media is a real disk in a real drive: it is
            // loaded, swapped, and ejected by hand. The buttons stay drawn so
            // the drive is visibly there and numbered, but they do nothing.
            BarControl::DriveLoad(idx) if self.drive_is_bridged(idx) => {}
            BarControl::DriveSwap(idx) if self.drive_is_bridged(idx) => {}
            BarControl::DriveEject(idx) if self.drive_is_bridged(idx) => {}
            BarControl::DriveLoad(idx) => self.load_drive_disks_from_dialog(idx),
            BarControl::DriveSwap(idx) => self.swap_drive_disk(idx),
            BarControl::DriveEject(idx) => self.eject_drive_disk(idx),
            BarControl::CdLoad => self.load_cd_from_dialog(),
            BarControl::CdEject => self.eject_cd(),
            BarControl::Joystick => {
                self.cycle_joystick_input_mode();
                self.request_redraw();
            }
            BarControl::Keyboard => self.toggle_keyboard_panel(),
            BarControl::Volume => {}
        }
    }

    /// Run the action behind a clicked menu item or panel control.
    #[cfg(test)]
    pub(super) fn activate_ui_control(&mut self, control: UiControl) {
        self.activate_ui_control_with_event_loop(control, None);
    }

    /// Keys consumed by the open menu/panel (Escape, debugger hex entry).
    /// Returns true when the key was handled and must not reach the Amiga.
    pub(super) fn ui_handle_key(
        &mut self,
        code: KeyCode,
        text: Option<&str>,
        event_loop: Option<&ActiveEventLoop>,
    ) -> bool {
        if self.ui.active() {
            // An armed Input Mapping row eats the next key, including Escape
            // (which cancels the binding rather than closing the panel).
            if self.input_map_handle_key(code) {
                return true;
            }
            // So do the launcher's dialogs: each is the only thing being
            // answered while it is up, Escape included.
            #[cfg(feature = "game-library")]
            if self.login_handle_key(code, text) || self.meta_handle_key(code, text) {
                return true;
            }
            if code == KeyCode::Escape {
                // While typing into a plugin option, Escape cancels the edit
                // rather than closing the panel.
                if self.launcher_cancel_edit_if_active() {
                    return true;
                }
                if self.ui.menu_open {
                    self.ui.menu_open = false;
                    self.request_redraw();
                } else {
                    self.close_panel();
                }
                return true;
            }
            // Drop chooser: a digit picks the Nth listed drive (the button
            // labels carry the same numbers).
            if let Some(Panel::DropChooser(state)) = &self.ui.panel {
                let index = match code {
                    KeyCode::Digit1 => Some(0),
                    KeyCode::Digit2 => Some(1),
                    KeyCode::Digit3 => Some(2),
                    KeyCode::Digit4 => Some(3),
                    _ => None,
                };
                if let Some(index) = index {
                    if let Some(drive) = state.drives.get(index).map(|entry| entry.drive) {
                        self.drop_chooser_route(drive);
                    }
                    return true;
                }
            }
            // Route keys to a focused plugin-option text field, if any.
            if self.launcher_handle_edit_key(code, text) {
                return true;
            }
            #[cfg(feature = "game-library")]
            if self.library_handle_key(code) {
                return true;
            }
        }
        // Nothing above wanted it: the focus takes the arrows and Return,
        // so an open surface can be walked and worked without a pointer.
        // A focus that is showing keeps them even with nothing modal up
        // -- walking off the foot of the menu leaves the marker on the
        // status bar, and the bar is meant to be walked from there.
        // Escape hands the keyboard back to the guest.
        if (self.modal_ui_active() || self.nav.showing()) && self.handle_nav_key(code, event_loop) {
            return true;
        }
        // Tool panels (debugger, frame analyzer, console) take their keys
        // through their own windows' events, never from the main window:
        // an unclaimed key here belongs to the Amiga.
        false
    }

    /// Type into the sign-in dialog, and answer it.
    ///
    /// Return is OK and Escape is Cancel, Tab moves between the two boxes,
    /// and everything else goes into whichever has focus. Characters come
    /// from the text winit gives rather than from key codes, so a keyboard
    /// that is not the one this was written on still types what it says.
    #[cfg(feature = "game-library")]
    pub(super) fn login_handle_key(&mut self, code: KeyCode, text: Option<&str>) -> bool {
        use crate::video::launcher::{CaretMove, LoginField};
        if self
            .launcher_state()
            .is_none_or(|state| state.login.is_none())
        {
            return false;
        }
        // A dialog is walked as well as typed into. Up and down are
        // always the focus's -- they are how the boxes and the buttons
        // under them are got between -- and while the marker is on a
        // button so are left, right and Return. Inside a box those stay
        // the caret's, which is what they mean while typing.
        if self.nav_dialog_key_is_focus(code, &[UiControl::LoginOk, UiControl::LoginCancel]) {
            return false;
        }
        match code {
            KeyCode::Escape => {
                self.login_close();
                self.request_redraw();
                return true;
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                self.login_submit();
                self.request_redraw();
                return true;
            }
            _ => {}
        }
        // Cmd+V on macOS, Ctrl+V anywhere: a password long enough to be
        // worth having is a password worth pasting.
        if code == KeyCode::KeyV
            && (host_shortcut_modifier_pressed(self.modifiers) || self.modifiers.control_key())
        {
            self.login_paste();
            return true;
        }
        let held = host_shortcut_modifier_pressed(self.modifiers) || self.modifiers.control_key();
        let Some(login) = self.launcher_login_mut() else {
            return false;
        };
        match code {
            // Anything else typed with a command modifier held is a
            // shortcut that is not ours, not text.
            _ if held && code != KeyCode::Tab && code != KeyCode::Backspace => {}
            KeyCode::Tab => login.focus_on(match login.focus {
                LoginField::User => LoginField::Pass,
                LoginField::Pass => LoginField::User,
            }),
            KeyCode::Backspace => login.backspace(),
            KeyCode::Delete => login.delete(),
            KeyCode::ArrowLeft => login.caret_move(CaretMove::Left),
            KeyCode::ArrowRight => login.caret_move(CaretMove::Right),
            KeyCode::Home => login.caret_move(CaretMove::Home),
            KeyCode::End => login.caret_move(CaretMove::End),
            _ => {
                // Printable characters only: a control code typed into a
                // password is a character nobody can see and nobody meant.
                let typed = text.unwrap_or_default();
                for c in typed.chars().filter(|c| !c.is_control()) {
                    login.insert(c);
                }
            }
        }
        self.request_redraw();
        true
    }

    /// Type into the metadata editor, and answer it.
    ///
    /// Return saves, Escape cancels, Tab walks the fields. The same
    /// shape as the sign-in dialog, because two dialogs that behave
    /// differently is two things to learn.
    #[cfg(feature = "game-library")]
    pub(super) fn meta_handle_key(&mut self, code: KeyCode, text: Option<&str>) -> bool {
        use crate::video::launcher::{CaretMove, MetaField};
        if self
            .launcher_state()
            .is_none_or(|state| state.meta.is_none())
        {
            return false;
        }
        // A dialog is walked as well as typed into. Up and down are
        // always the focus's -- they are how the boxes and the buttons
        // under them are got between -- and while the marker is on a
        // button so are left, right and Return. Inside a box those stay
        // the caret's, which is what they mean while typing.
        if self.nav_dialog_key_is_focus(
            code,
            &[
                UiControl::MetaSave,
                UiControl::MetaClear,
                UiControl::MetaCancel,
                UiControl::MetaArt,
            ],
        ) {
            return false;
        }
        match code {
            KeyCode::Escape => {
                if let Some(state) = self.launcher_state_mut() {
                    state.meta = None;
                }
                self.request_redraw();
                return true;
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                self.meta_save();
                return true;
            }
            _ => {}
        }
        if code == KeyCode::KeyV
            && (host_shortcut_modifier_pressed(self.modifiers) || self.modifiers.control_key())
        {
            let line = clipboard_line();
            if let Some(meta) = self.launcher_meta_mut() {
                let most = meta_field_max(meta.focus);
                for c in line.chars().filter(|c| !c.is_control()) {
                    meta.insert(c, most);
                }
            }
            self.request_redraw();
            return true;
        }
        let held = host_shortcut_modifier_pressed(self.modifiers) || self.modifiers.control_key();
        let Some(meta) = self.launcher_meta_mut() else {
            return false;
        };
        let focus = meta.focus;
        match code {
            _ if held && code != KeyCode::Tab && code != KeyCode::Backspace => {}
            KeyCode::Tab => {
                let at = MetaField::ALL.iter().position(|&f| f == focus).unwrap_or(0);
                meta.focus_on(MetaField::ALL[(at + 1) % MetaField::ALL.len()]);
            }
            KeyCode::Backspace => {
                let mut caret = meta.caret;
                caret.backspace(meta.value_mut(focus));
                meta.caret = caret;
            }
            KeyCode::Delete => {
                let mut caret = meta.caret;
                caret.delete(meta.value_mut(focus));
                meta.caret = caret;
            }
            KeyCode::ArrowLeft => meta.caret_move(CaretMove::Left),
            KeyCode::ArrowRight => meta.caret_move(CaretMove::Right),
            KeyCode::Home => meta.caret_move(CaretMove::Home),
            KeyCode::End => meta.caret_move(CaretMove::End),
            _ => {
                let most = meta_field_max(focus);
                for c in text.unwrap_or_default().chars().filter(|c| !c.is_control()) {
                    meta.insert(c, most);
                }
            }
        }
        self.request_redraw();
        true
    }

    /// Paste the host clipboard into whichever box has focus.
    ///
    /// One line of it: a password manager hands over a password, not a
    /// document, and a newline in the middle of one is a paste that went
    /// wrong rather than a password with a newline in it.
    #[cfg(feature = "game-library")]
    pub(super) fn login_paste(&mut self) {
        let line = clipboard_line();
        if let Some(login) = self.launcher_login_mut() {
            for c in line.chars().filter(|c| !c.is_control()) {
                login.insert(c);
            }
        }
        self.request_redraw();
    }

    /// Walk the Library's lists with the arrow keys, and launch with
    /// Return.
    ///
    /// Held, the arrows accelerate through the same rate the scroll-arrow
    /// buttons use: a key repeating is a scroll continuing, and the two
    /// should not build up at different speeds.
    #[cfg(feature = "game-library")]
    pub(super) fn library_handle_key(&mut self, code: KeyCode) -> bool {
        use crate::video::launcher::LauncherTab;
        if self
            .launcher_state()
            .is_none_or(|state| state.tab != LauncherTab::WhdloadLibrary)
        {
            return false;
        }
        // The list takes these only while the focus is standing on a row
        // of it: the letters across the top are a strip to be walked,
        // not a list to be scrolled.
        if !self.nav_focus_in_library() {
            return false;
        }
        // Return launches what is chosen, which is what Run would do: on
        // this page the selection is the game, so the two are one action.
        if matches!(code, KeyCode::Enter | KeyCode::NumpadEnter) {
            self.launcher_run();
            return true;
        }
        // Up and down belong to the focus, not to the keyboard: they are
        // how the list is walked by either hand, and are spent on it in
        // `nav_move`. Only the jumps to either end are the keyboard's
        // own, having no direction a pad could give them.
        let step = match code {
            KeyCode::Home => isize::MIN,
            KeyCode::End => isize::MAX,
            _ => return false,
        };
        self.step_library_list(step);
        true
    }

    /// Cancel an in-progress plugin-option text edit, if one is focused.
    pub(super) fn launcher_cancel_edit_if_active(&mut self) -> bool {
        let cancelled = matches!(
            self.launcher_state_mut(),
            Some(state) if state.editing().is_some()
        );
        if cancelled {
            if let Some(state) = self.launcher_state_mut() {
                state.edit_cancel();
            }
            self.request_redraw();
        }
        cancelled
    }

    /// Feed a key to a focused plugin-option text field. Returns false (so the
    /// key falls through) when no field is being edited.
    pub(super) fn launcher_handle_edit_key(&mut self, code: KeyCode, text: Option<&str>) -> bool {
        use crate::video::launcher::CaretMove;
        let handled = {
            let Some(state) = self.launcher_state_mut() else {
                return false;
            };
            if state.editing().is_none() {
                return false;
            }
            match code {
                KeyCode::Backspace => state.edit_backspace(),
                KeyCode::Delete => state.edit_delete(),
                KeyCode::Enter | KeyCode::NumpadEnter => state.edit_commit(),
                KeyCode::ArrowLeft => state.edit_caret_move(CaretMove::Left),
                KeyCode::ArrowRight => state.edit_caret_move(CaretMove::Right),
                KeyCode::Home => state.edit_caret_move(CaretMove::Home),
                KeyCode::End => state.edit_caret_move(CaretMove::End),
                _ => {
                    // Prefer the layout- and shift-aware text the platform
                    // reports, so volume names can contain lowercase letters,
                    // underscores, and other printable characters (the
                    // keycode map is uppercase-only and lacks symbols). Fall
                    // back to it only when no text is delivered.
                    if let Some(t) = text.filter(|t| !t.is_empty()) {
                        for ch in t.chars().filter(|c| !c.is_control()) {
                            state.edit_push(ch);
                        }
                    } else if let Some(ch) = entry_char_for_key(code) {
                        state.edit_push(ch);
                    }
                }
            }
            true
        };
        if handled {
            self.request_redraw();
        }
        handled
    }

    pub(super) fn ui_handle_tool_key(&mut self, kind: ToolPanelKind, code: KeyCode) -> bool {
        if code == KeyCode::Escape {
            self.close_tool_panel(kind);
            return true;
        }
        match kind {
            ToolPanelKind::Debugger => self.ui_handle_debugger_key(code),
            ToolPanelKind::FrameAnalyzer => self.ui_handle_frame_analyzer_key(code),
            ToolPanelKind::Console => self.ui_handle_console_key(code),
        }
    }

    /// Open or close the pop-up menu, from the hamburger button or the
    /// keyboard.
    ///
    /// Opening hands the mouse back: the menu is worked with the host
    /// pointer, and a captured one is inside the machine where it cannot
    /// reach it. Closing asks for the grab again, which auto mode takes and
    /// the other modes decline, exactly as closing a panel does.
    pub(super) fn toggle_menu(&mut self) {
        self.ui.menu_open = !self.ui.menu_open;
        // Each open starts at the top of the list; a position left over from
        // the last time would be a small mystery.
        self.ui.menu_nav.reset();
        if self.ui.menu_open {
            self.set_mouse_captured(false);
            let fullscreen = self
                .render
                .as_ref()
                .is_some_and(|r| r.window.fullscreen().is_some());
            self.ui.menu_rows = self.build_menu(fullscreen);
            // The menu opens with its last row chosen -- the foot of
            // the list is where it hangs from -- so a hand on the
            // keyboard or a pad has somewhere to start, and walking on
            // down leaves the menu for the bar. The pointer takes the
            // cursor from it the moment it moves.
            let ui = &mut self.ui;
            ui.menu_nav.step(&ui.menu_rows, false);
        } else {
            self.ui.menu_rows = Vec::new();
            self.apply_auto_mouse_capture();
        }
        self.request_redraw();
    }

    /// Close the open main-window overlay panel.
    pub(super) fn close_panel(&mut self) {
        self.analyzer_dragging = false;
        self.ui.panel = None;
        // The surface the focus was walking has gone with it, and so has
        // the way back into the page it was on.
        self.nav.clear();
        self.nav_entered_from = None;
        self.request_redraw();
    }
}
