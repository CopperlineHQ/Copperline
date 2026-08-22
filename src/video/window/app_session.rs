// SPDX-License-Identifier: GPL-3.0-or-later

//! Session transport: warp, save states, recordings, screenshots, OSD, power/pause/reset, live audio.

use super::*;

impl App {
    /// Toggle borderless fullscreen on the main window. Borderless (not
    /// exclusive) keeps the compositor path and the existing Resized-driven
    /// surface rebuild; the presentation already letterboxes any window
    /// shape, so no display-mode change is wanted.
    pub(super) fn toggle_fullscreen(&mut self) {
        let Some(window) = self.render.as_ref().map(|r| r.window.clone()) else {
            return;
        };
        if window.fullscreen().is_some() {
            window.set_fullscreen(None);
            info!("fullscreen off");
            self.show_osd("Fullscreen off");
        } else {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
            info!("fullscreen on");
            self.show_osd(format!(
                "Fullscreen on ({HOST_SHORTCUT_MODIFIER_LABEL}+F restores)"
            ));
            // Fullscreen leaves no desktop to reach for, so auto mode takes
            // the grab here as well as on focus: entering fullscreen does
            // not itself change the focus, so no Focused event follows.
            self.apply_auto_mouse_capture();
        }
    }

    /// Start the `--run` warp-launch phase: run unpaced with live audio
    /// muted until the guest OS loads the target program
    /// (src/runprog.rs). A machine that refuses to unpace (a bridged
    /// physical drive) still watches for the program, at normal speed
    /// with audio live. One-shot per session.
    pub(super) fn engage_warp_launch(&mut self) {
        let engage = match &self.warp_launch {
            Some(launch) => self.powered_on && !launch.started(),
            None => false,
        };
        if !engage {
            return;
        }
        self.emu.set_paced(false);
        let unpaced = !self.emu.paced();
        let now = self.emu.bus().emulated_seconds();
        let tracker = &mut self.warp_launch_tracker;
        crate::amigaos::with_bus_memory(self.emu.bus(), |os| tracker.arm(os));
        let launch = self.warp_launch.as_mut().expect("checked above");
        launch.engage(now, unpaced);
        if unpaced {
            info!(
                "warp launch: warping until the guest loads {}",
                launch.target()
            );
        } else {
            info!(
                "warp launch: physical floppy drive attached; booting {} at normal speed",
                launch.target()
            );
        }
        self.sync_live_audio_suspension();
    }

    /// One warp-launch poll, once per retired emulated frame (inside the
    /// warp burst: at full warp a thousand frames retire per presented
    /// frame, and the gate must not be a thousand frames late). Returns
    /// true when the launch phase just ended, so the burst breaks and
    /// pacing takes effect at this frame.
    pub(super) fn poll_warp_launch(&mut self) -> bool {
        let Some(launch) = &mut self.warp_launch else {
            return false;
        };
        if !launch.started() {
            return false;
        }
        let tracker = &mut self.warp_launch_tracker;
        let loaded = crate::amigaos::with_bus_memory(self.emu.bus(), |os| {
            tracker.observe(os).map(|module| module.name.clone())
        });
        let now = self.emu.bus().emulated_seconds();
        let target = launch.target().to_string();
        match launch.note(now, loaded.as_deref()) {
            crate::runprog::WarpLaunchOutcome::Waiting => false,
            crate::runprog::WarpLaunchOutcome::Loaded => {
                info!("warp launch: {target} loaded; real-time pacing resumes");
                self.finish_warp_launch();
                self.show_osd(format!("Warp launch: {target} running"));
                true
            }
            crate::runprog::WarpLaunchOutcome::Finished => {
                info!("warp launch: {target} already ran to completion; real-time pacing resumes");
                self.finish_warp_launch();
                self.show_osd(format!("Warp launch: {target} finished"));
                true
            }
            crate::runprog::WarpLaunchOutcome::TimedOut => {
                warn!(
                    "warp launch: {target} was not loaded within {:.0} emulated seconds; \
                     resuming real-time pacing anyway",
                    crate::runprog::WARP_LAUNCH_TIMEOUT_SECS
                );
                self.finish_warp_launch();
                self.show_osd(format!("Warp launch: {target} not seen, warp off"));
                true
            }
        }
    }

    /// End the launch phase: back to real-time pacing (set_paced
    /// re-anchors the pacing clock) and live audio.
    pub(super) fn finish_warp_launch(&mut self) {
        self.warp_launch = None;
        self.emu.set_paced(true);
        self.sync_live_audio_suspension();
    }

    /// Toggle warp speed: emulation runs unpaced (as fast as the host
    /// allows) until switched back, when pacing re-anchors to "now".
    pub(super) fn toggle_warp(&mut self) {
        // A manual warp toggle during a warp launch takes the session
        // back: cancel the launch so one press means normal-speed,
        // audible emulation, not a fight with the gate.
        if self.warp_launch.take().is_some() {
            self.sync_live_audio_suspension();
            info!("warp launch: cancelled by manual warp toggle");
        }
        let warp = self.emu.paced();
        self.emu.set_paced(!warp);
        if warp {
            let limit = self.warp_speed.label();
            info!("warp speed on (emulation unpaced, limit {limit})");
            self.show_osd(format!("Warp speed on ({limit})"));
        } else {
            info!("warp speed off (real-time pacing)");
            self.show_osd("Warp speed off");
        }
    }

    /// The run-ahead level in effect for this burst, or zero while the
    /// machine is transiently stopped or has a host-side incompatibility.
    pub(super) fn runahead_effective_frames(&self) -> u8 {
        if self.run_ahead_frames == 0
            || !self.powered_on
            || self.cpu_halted
            || self.paused
            || self.runahead_block_reason().is_some()
        {
            return 0;
        }
        self.run_ahead_frames
    }

    /// Why speculative execution is unsafe for the current session. A
    /// configured level stays selected while blocked, and the menu/log can
    /// surface this reason instead of silently pretending it is active.
    pub(super) fn runahead_block_reason(&self) -> Option<&'static str> {
        if !self.emu.paced() {
            return Some("warp active");
        }
        if self.emu.time_travel_enabled() {
            return Some("rewind/reverse history armed");
        }
        if self.rtg_present_dims.is_some() || self.emu.bus().rtg_active() {
            return Some("RTG display active");
        }
        if self.recorder.is_some() {
            return Some("video recording active");
        }
        if self.serial_is_midi {
            return Some("MIDI device on the serial port");
        }
        if !self.emu.bus().paula.serial.runahead_safe() {
            return Some("live serial host endpoint");
        }
        if self.control_client_attached() {
            return Some("control client attached");
        }
        if let Some(reason) = self.emu.machine.runahead_debug_block_reason() {
            return Some(reason);
        }
        self.runahead_machine_block
            .or_else(|| self.emu.bus().runahead_host_block_reason())
    }

    /// Restore the committed boundary after any speculative execution. An
    /// incomplete burst is not presentable and disables run-ahead for the
    /// session, but it is still an abandoned timeline whose host output was
    /// suppressed, so it must never be promoted by skipping the restore.
    pub(super) fn restore_runahead_anchor(
        &mut self,
        anchor_snapshot: Option<&[u8]>,
        burst_complete: bool,
        speculated: bool,
    ) {
        if !speculated {
            return;
        }
        match anchor_snapshot {
            Some(blob) => {
                if let Err(e) = self.emu.runahead_restore(blob) {
                    error!("run-ahead disabled: anchor restore failed: {e:?}");
                    self.run_ahead_frames = 0;
                }
            }
            None => {
                error!("run-ahead disabled: speculative burst has no anchor snapshot");
                self.run_ahead_frames = 0;
            }
        }
        if !burst_complete {
            self.run_ahead_frames = 0;
        }
    }

    fn control_client_attached(&self) -> bool {
        #[cfg(feature = "control")]
        {
            self.control.as_ref().is_some_and(|c| c.handle.connected())
        }
        #[cfg(not(feature = "control"))]
        {
            false
        }
    }

    /// How many emulated frames to retire before presenting the next frame, and
    /// an optional wall-clock budget that bounds that burst. Warp's output frame
    /// skip applies only while warp is engaged and not doing headless capture;
    /// real-time pacing and headless capture both run one frame per presented
    /// frame. The `Max` level returns a budget so the burst presents at vsync
    /// rather than spinning to its frame cap.
    pub(super) fn warp_burst_plan(
        &self,
        headless_capture: bool,
    ) -> (usize, Option<std::time::Duration>) {
        if self.emu.paced() || headless_capture {
            return (1, None);
        }
        (
            self.warp_speed.frame_cap(),
            self.warp_speed
                .time_budget_ms()
                .map(std::time::Duration::from_millis),
        )
    }

    /// Combine warp output-frame skipping with run-ahead without allowing
    /// either policy to collapse the other's frame cap. Scheduled capture is
    /// unthrottled and archives committed frames, so it uses neither.
    pub(super) fn burst_frames(
        &self,
        headless_capture: bool,
    ) -> (usize, u8, Option<std::time::Duration>) {
        let (frame_cap, time_budget) = self.warp_burst_plan(headless_capture);
        let runahead = if frame_cap == 1 && time_budget.is_none() && !headless_capture {
            self.runahead_effective_frames()
        } else {
            0
        };
        (frame_cap + usize::from(runahead), runahead, time_budget)
    }

    /// Cycle the warp/turbo output frame-skip level (2x -> 4x -> 8x -> 16x ->
    /// Max). Takes effect immediately when warp is engaged; otherwise it just
    /// arms the level the next warp toggle will use.
    pub(super) fn cycle_warp_speed(&mut self) {
        self.warp_speed = self.warp_speed.next();
        let limit = self.warp_speed.label();
        info!("warp limit: {limit}");
        let active = !self.emu.paced();
        if active {
            self.show_osd(format!("Warp limit: {limit}"));
        } else {
            self.show_osd(format!("Warp limit: {limit} (warp off)"));
        }
        self.request_redraw();
    }

    /// Interactive shortcut / menu state save: write the whole
    /// emulated machine to an auto-named file in the working directory and
    /// flash the filename on screen. Runs between frames by construction
    /// (the event loop only dispatches input/menu events outside step_frame).
    pub(super) fn save_state_interactive(&mut self) {
        self.suspend_live_audio_for_host_io();
        let path = crate::savestate::auto_filename();
        match self.emu.save_state(&path) {
            Ok(()) => {
                info!("save state written: {}", path.display());
                self.show_osd(format!("Saved {}", display_file_name(&path)));
            }
            Err(e) => {
                warn!("save state failed ({}): {e:#}", path.display());
                self.show_osd("State save failed (see log)");
            }
        }
        self.finish_host_io_pause();
    }

    /// Save to numbered slot `slot` (1-based). Overwrites silently: a quick
    /// save is expected to be instant, and the previous contents of the slot
    /// are what the user is replacing.
    pub(super) fn quick_save_state(&mut self, slot: usize) {
        self.quick_save_state_at(slot, crate::savestate::slot_path(slot));
    }

    /// Test/frontend seam for slot roots that must not touch the host's real
    /// per-user state directory.
    pub(super) fn quick_save_state_at(&mut self, slot: usize, path: Option<PathBuf>) {
        let Some(path) = path else {
            self.show_osd("No per-user directory for save slots");
            return;
        };
        self.suspend_live_audio_for_host_io();
        let result = crate::paths::ensure_parent(&path)
            .map_err(anyhow::Error::from)
            .and_then(|()| self.emu.save_state(&path));
        match result {
            Ok(()) => {
                info!("save state written to slot {slot}: {}", path.display());
                self.show_osd(format!("Slot {slot} saved"));
            }
            Err(e) => {
                warn!("slot {slot} save failed ({}): {e:#}", path.display());
                self.show_osd(format!("Slot {slot} save failed (see log)"));
            }
        }
        self.finish_host_io_pause();
    }

    /// Restore numbered slot `slot` (1-based). An empty slot is reported
    /// rather than treated as an error: the menu and the hotkeys cover all
    /// ten, and most of them are usually unused.
    pub(super) fn quick_load_state(&mut self, slot: usize, event_loop: Option<&ActiveEventLoop>) {
        self.quick_load_state_at(slot, crate::savestate::slot_path(slot), event_loop);
    }

    /// Test/frontend seam paired with [`Self::quick_save_state_at`].
    pub(super) fn quick_load_state_at(
        &mut self,
        slot: usize,
        path: Option<PathBuf>,
        event_loop: Option<&ActiveEventLoop>,
    ) {
        let Some(path) = path else {
            self.show_osd("No per-user directory for save slots");
            return;
        };
        if !path.exists() {
            self.show_osd(format!("Slot {slot} is empty"));
            return;
        }
        self.suspend_live_audio_for_host_io();
        if self.load_state_from_path(&path) {
            self.show_osd(format!("Slot {slot} loaded"));
            if let Some(event_loop) = event_loop {
                event_loop.set_control_flow(ControlFlow::Poll);
            }
        }
        self.finish_host_io_pause();
    }

    /// Pick a save-state file and restore it (shortcut / menu). On
    /// success the machine continues from the state's timeline: power is
    /// forced on, any CPU halt is cleared, and the display re-renders from
    /// the restored Bus. On failure the running machine is untouched.
    pub(super) fn load_state_from_dialog(&mut self, event_loop: Option<&ActiveEventLoop>) {
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Load save state")
            .add_filter("Copperline save states", &["clstate"])
            .pick_file();

        // Re-baseline pacing after the modal dialog, as for floppies; a
        // successful load re-anchors again to the restored timeline inside
        // Emulator::load_state.
        if let Some(path) = picked {
            if self.load_state_from_path(&path) {
                if let Some(event_loop) = event_loop {
                    event_loop.set_control_flow(ControlFlow::Poll);
                }
            }
        }
        self.finish_host_io_pause();
    }

    pub(super) fn load_state_from_path(&mut self, path: &std::path::Path) -> bool {
        // The restored machine carries its own keyboard state, so the
        // strip lets go of its holds against the machine that is still
        // here. Done before the attempt rather than after a success: a
        // release sent into the restored machine would be a key it never
        // saw pressed.
        self.release_keyboard_panel_holds();
        match self.emu.load_state(path) {
            Ok(outcome) => {
                info!(
                    "save state loaded: {} ({})",
                    path.display(),
                    outcome.summary
                );
                // The pre-boot configuration screen runs on a placeholder
                // machine with a silent NullSink (see build_placeholder_machine);
                // a state loaded over it would keep that null sink and play no
                // audio. Detect that case before powering on and give the
                // restored machine a live host output below, mirroring the
                // launcher's Run path. A machine that already has a real sink
                // (any normal running session) is left untouched.
                let restoring_over_placeholder = self.restoring_over_placeholder();
                self.powered_on = true;
                self.cpu_halted = false;
                // A state can carry host-coupled hardware that is unrelated
                // to the session's remembered config. Keep the conservative
                // gate until a freshly resolved machine is launched.
                self.runahead_machine_block = Some("loaded save state");
                // Force a fresh presentation: the restored frame counter
                // may equal (or precede) the last rendered one.
                self.reset_render_pipeline();
                if matches!(self.ui.panel, Some(Panel::Launcher(_))) {
                    self.ui.panel = None;
                }
                if restoring_over_placeholder {
                    self.install_live_audio_after_placeholder_load();
                }
                if outcome.reconfigured {
                    // The state was built on a different machine; the host
                    // has been reconfigured to match it (see log for the
                    // specifics). The disk-swap playlists are host-side and
                    // describe the previous machine's drives, so drop them
                    // rather than let stale swap affordances show in the
                    // status bar; the restored drives keep whatever disks
                    // the state embedded.
                    self.disk_playlists = std::array::from_fn(|_| Vec::new());
                    self.show_osd(format!(
                        "Loaded {} (reconfigured to {})",
                        display_file_name(path),
                        outcome.summary
                    ));
                } else {
                    self.show_osd(format!("Loaded {}", display_file_name(path)));
                }
                self.request_redraw();
                true
            }
            Err(e) => {
                warn!("save state load failed ({}): {e:#}", path.display());
                self.show_osd("State load failed (see log)");
                false
            }
        }
    }

    /// Pick a Kickstart ROM (and an optional extended ROM) and fit it,
    /// cold-resetting the machine as if the chip had been swapped and the
    /// power cycled (menu "Load Kickstart ROM..."). The main ROM is 512 KiB,
    /// or 256 KiB for a Kickstart 1.x part (mirrored up to the full window);
    /// an extended ROM is 512 KiB ($E00000) or 256 KiB ($F00000).
    /// On any error the running machine keeps its current ROM.
    pub(super) fn load_rom_from_dialog(&mut self) {
        self.suspend_live_audio_for_host_io();
        let picked = rfd::FileDialog::new()
            .set_title("Load Kickstart ROM (512 or 256 KiB)")
            .add_filter("Amiga ROM images", &["rom", "bin"])
            .pick_file();
        if let Some(main_path) = picked {
            // Offer an optional extended ROM (AROS/CDTV/CD32). Cancelling skips it
            // and removes any extended ROM currently fitted.
            let ext_path = rfd::FileDialog::new()
                .set_title("Load extended ROM (optional; Cancel to skip)")
                .add_filter("Amiga ROM images", &["rom", "bin"])
                .pick_file();

            // The identification comes off the bytes already in hand (the
            // image is handed to the machine straight after), so the OSD and
            // the log name the Kickstart without re-reading the file.
            let result = (|| -> anyhow::Result<Option<&'static str>> {
                let rom = std::fs::read(&main_path)
                    .map_err(|e| anyhow::anyhow!("reading ROM {}: {e}", main_path.display()))?;
                let ext = match &ext_path {
                    Some(p) => Some(std::fs::read(p).map_err(|e| {
                        anyhow::anyhow!("reading extended ROM {}: {e}", p.display())
                    })?),
                    None => None,
                };
                let identified = crate::romdb::describe(&rom).map(|id| id.label());
                self.emu.reload_rom(rom, ext)?;
                Ok(identified)
            })();

            match result {
                Ok(identified) => {
                    let name = display_file_name(&main_path);
                    // The in-memory identification sees through a Cloanto
                    // wrapper; the path-based one names an AROS image's
                    // version and revision. Prefer the former, fall back
                    // to the latter.
                    let line_id = identified
                        .map(str::to_string)
                        .or_else(|| crate::config::about_rom_identification(&main_path));
                    let rom_line = crate::config::about_rom_line(&name, line_id.as_deref());
                    match identified {
                        Some(id) => info!("boot ROM loaded: {} ({id})", main_path.display()),
                        None => info!("boot ROM loaded: {}", main_path.display()),
                    }
                    self.show_osd(rom_line.clone());
                    // The About panel's machine lines are cached from the
                    // configuration; the chip in the machine just changed,
                    // so its ROM line has to follow the swap.
                    match self
                        .about_machine_lines
                        .iter_mut()
                        .find(|l| l.starts_with("ROM: "))
                    {
                        Some(line) => *line = rom_line,
                        None => self.about_machine_lines.push(rom_line),
                    }
                    // The extended ROM line follows the same swap: updated,
                    // added after the boot ROM's line, or dropped to match
                    // what is now fitted.
                    let ext_line = ext_path.as_deref().map(|p| {
                        crate::config::about_ext_rom_line(
                            &display_file_name(p),
                            crate::config::about_rom_identification(p).as_deref(),
                        )
                    });
                    let at = self
                        .about_machine_lines
                        .iter()
                        .position(|l| l.starts_with("Extended ROM: "));
                    match (at, ext_line) {
                        (Some(i), Some(line)) => self.about_machine_lines[i] = line,
                        (Some(i), None) => {
                            self.about_machine_lines.remove(i);
                        }
                        (None, Some(line)) => {
                            let after_rom = self
                                .about_machine_lines
                                .iter()
                                .position(|l| l.starts_with("ROM: "))
                                .map(|i| i + 1)
                                .unwrap_or(self.about_machine_lines.len());
                            self.about_machine_lines.insert(after_rom, line);
                        }
                        (None, None) => {}
                    }
                    self.powered_on = true;
                    self.cpu_halted = false;
                    // The cold reset restarts the frame timeline; force a repaint.
                    self.reset_render_pipeline();
                    self.request_redraw();
                }
                Err(e) => {
                    warn!("ROM load failed ({}): {e:#}", main_path.display());
                    self.show_osd("ROM load failed (see log)");
                }
            }
        }
        self.finish_host_io_pause();
    }

    /// Start or stop the video+audio capture (shortcut / menu item).
    pub(super) fn toggle_recording(&mut self) {
        if self.recorder.is_some() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    pub(super) fn start_recording(&mut self) {
        self.start_recording_to(crate::recorder::auto_filename());
    }

    pub(super) fn start_recording_to(&mut self, path: PathBuf) {
        match crate::recorder::VideoRecorder::create(&path, FB_WIDTH, present_height()) {
            Ok(rec) => {
                // The Paula tap collects the mixed stereo output from this
                // point on; capture_recorder_output drains it every frame.
                self.emu.bus_mut().paula.set_audio_capture_enabled(true);
                info!("recording video+audio to {}", path.display());
                self.show_osd(format!("Recording {}", display_file_name(&path)));
                self.recorder = Some(rec);
            }
            Err(e) => {
                warn!("recording start failed: {e:#}");
                self.show_osd("Recording start failed (see log)");
            }
        }
        self.request_redraw();
    }

    pub(super) fn stop_recording(&mut self) {
        let Some(mut rec) = self.recorder.take() else {
            return;
        };
        let samples = self.emu.bus_mut().paula.take_captured_audio();
        self.emu.bus_mut().paula.set_audio_capture_enabled(false);
        rec.push_audio(&samples);
        let seconds = rec.recorded_seconds();
        let path = rec.path().to_path_buf();
        match rec.finish() {
            Ok(()) => {
                info!(
                    "recording saved: {} ({seconds:.1}s of emulated time)",
                    path.display()
                );
                self.show_osd(format!(
                    "Saved {} ({seconds:.1}s)",
                    display_file_name(&path)
                ));
            }
            Err(e) => {
                warn!("recording save failed ({}): {e:#}", path.display());
                self.show_osd("Recording save failed (see log)");
            }
        }
        self.request_redraw();
    }

    /// Feed the active recording: drain the audio captured during the
    /// quantum just stepped and, when a new emulated frame was rendered,
    /// append it with the presentation-scaled picture.
    pub(super) fn capture_recorder_output(&mut self, rendered: bool) {
        if self.recorder.is_none() {
            return;
        }
        let samples = self.emu.bus_mut().paula.take_captured_audio();
        let mut failure = None;
        if let Some(rec) = self.recorder.as_mut() {
            rec.push_audio(&samples);
            if rendered {
                // The recorder's frame size is fixed at FB_WIDTH; average a
                // 35 ns canvas's pixel pairs down to it first.
                if self.present_width != FB_WIDTH {
                    screenshot::downsample_x_into(
                        &self.present_fb,
                        self.present_width,
                        self.present_rows,
                        FB_WIDTH,
                        &mut self.record_scratch_fb,
                    );
                    screenshot::scale_y_into(
                        &self.record_scratch_fb,
                        FB_WIDTH,
                        self.present_rows,
                        present_height(),
                        &mut self.record_fb,
                    );
                } else {
                    screenshot::scale_y_into(
                        &self.present_fb,
                        FB_WIDTH,
                        self.present_rows,
                        present_height(),
                        &mut self.record_fb,
                    );
                }
                if let Err(e) = rec.push_frame(&self.record_fb) {
                    failure = Some(e);
                }
            }
        }
        if let Some(e) = failure {
            warn!("recording frame write failed, stopping capture: {e:#}");
            self.stop_recording();
        }
    }

    pub(super) fn suspend_live_audio_for_host_io(&mut self) {
        self.emu.set_live_audio_suspended(true);
    }

    /// Whether a state is being loaded over the pre-boot placeholder machine
    /// that hosts the configuration screen: powered off, the launcher panel
    /// open, and the silent NullSink still installed. Only then does a load need
    /// to install a real audio output; every normal running session already has
    /// one. Evaluate this before powering on / dismissing the launcher.
    pub(super) fn restoring_over_placeholder(&self) -> bool {
        !self.powered_on
            && matches!(self.ui.panel, Some(Panel::Launcher(_)))
            && self.emu.bus().paula.audio.is_null_sink()
    }

    /// Replace the placeholder machine's silent NullSink with a live host audio
    /// output after a save state is loaded over the configuration screen. This
    /// mirrors the launcher Run path (`launcher_run`): the configuration screen
    /// itself stays silent, but a machine started from it -- by Run or by a state
    /// load -- gets real sound. On audio-init failure the state stays loaded and
    /// the machine simply runs without sound, exactly as a failed Run does.
    pub(super) fn install_live_audio_after_placeholder_load(&mut self) {
        match crate::audio::open_output_sink(
            crate::priority::requested(self.realtime_priority),
            &self.audio_output,
        ) {
            Ok(sink) => {
                self.emu.bus_mut().paula.audio.set_master(sink);
                // Apply the current suspension state to the freshly installed
                // stream (it should be live now: powered on and not paused).
                self.sync_live_audio_suspension();
            }
            Err(e) => {
                warn!("audio init after state load failed; continuing without sound: {e:#}");
            }
        }
    }

    /// If the live output device was lost mid-run (unplugged, or the system
    /// default switched away), rebuild the sink on the current default output and
    /// reset the session's selected device to Default (so the runtime menu shows
    /// it) so sound continues. The cpal error callback only flags the loss; the
    /// stream is rebuilt here on the main thread, where creating a (macOS
    /// `!Send`) cpal stream is allowed. Falls back to a silent sink if no device
    /// can be opened, so this never spins retrying a dead machine.
    pub(super) fn recover_audio_if_device_lost(&mut self) {
        if !self.emu.bus().paula.audio.device_lost() {
            return;
        }
        warn!("audio: output device lost; falling back to the default output device");
        // The named device is gone, so the session is back on the default; reset
        // the selection so the runtime menu reflects "Default" too. (A disabled
        // sink never reports a lost device, so we can only get here from a device.)
        self.audio_output = crate::audio::AudioOutput::Default;
        // Reopen on the system default, not the previously named device, which
        // is the one that went away.
        match CpalSink::new(crate::priority::requested(self.realtime_priority), None) {
            Ok(sink) => {
                self.emu.bus_mut().paula.audio.set_master(Box::new(sink));
                self.sync_live_audio_suspension();
                self.show_osd("Audio device lost! Switched to Default".to_string());
            }
            Err(e) => {
                warn!("audio: no fallback output device; continuing without sound: {e:#}");
                self.emu
                    .bus_mut()
                    .paula
                    .audio
                    .set_master(Box::new(crate::audio::NullSink));
            }
        }
    }

    pub(super) fn finish_host_io_pause(&mut self) {
        self.emu.reanchor_realtime_clock();
        self.sync_live_audio_suspension();
    }

    pub(super) fn sync_live_audio_suspension(&mut self) {
        // The warp-launch catch-up is silent: unpaced Paula output is
        // fast-forward noise, and the machine snaps back to live audio
        // the moment the launch finishes (or is cancelled).
        let warp_launching = self.warp_launch.as_ref().is_some_and(|l| l.engaged);
        let suspended = !self.powered_on || self.cpu_halted || self.paused || warp_launching;
        self.emu.set_live_audio_suspended(suspended);
    }

    pub(super) fn save_screenshot(&self, path: &std::path::Path) {
        // COPPERLINE_SHOT_RAW saves the raw woven framebuffer (716x570
        // for standard fields, the native scan height for programmable
        // modes): the presentation resampler blends adjacent lines, so
        // per-scanline forensics need the unscaled field.
        let src_rows = self.present_rows;
        let result = if self.rtg_present_dims.is_some() {
            // An RTG board's frame already has one presentation row per
            // board row: save it at that height, matching the control
            // protocol's capture, instead of scaling to the chipset glass.
            screenshot::save(
                path,
                &self.present_fb[..src_rows * self.present_width],
                self.present_width as u32,
                src_rows as u32,
            )
        } else {
            save_present_frame(
                path,
                &self.present_fb,
                src_rows,
                self.present_width,
                self.overscan,
                self.tv_centre,
                self.present_tv_aperture_rows,
            )
        };
        match result {
            Ok(()) => info!("screenshot saved: {}", path.display()),
            Err(e) => warn!("screenshot save failed ({}): {e:#}", path.display()),
        }
    }

    /// Interactive screenshot grab: save to an auto-named PNG and
    /// flash the filename on screen. The overlay is painted into the
    /// presentation texture after the frame is captured, so it never
    /// appears in the saved image.
    pub(super) fn take_screenshot(&mut self) {
        self.finish_render_for_current_frame();
        let path = screenshot::auto_filename();
        self.save_screenshot(&path);
        self.show_osd(format!("Saved {}", display_file_name(&path)));
    }

    /// Show a transient overlay message over the display for
    /// [`OSD_DURATION`]. The message is cleared automatically; while it is
    /// visible the event loop keeps redrawing even when paused/idle so it
    /// fades on time.
    pub(super) fn show_osd(&mut self, text: impl Into<String>) {
        self.osd = Some(Osd {
            text: text.into(),
            expires_at: Instant::now() + OSD_DURATION,
            warning: false,
        });
        self.request_redraw();
    }

    /// Say something that did not go as asked, in amber. Otherwise as
    /// [`Self::show_osd`].
    #[cfg(any(feature = "mt32", feature = "coppersynth"))]
    pub(super) fn warn_osd(&mut self, text: impl Into<String>) {
        self.osd = Some(Osd {
            text: text.into(),
            expires_at: Instant::now() + OSD_DURATION,
            warning: true,
        });
        self.request_redraw();
    }

    /// The overlay text to draw this frame, or None when nothing is
    /// active. Expired overlays are dropped as a side effect.
    pub(super) fn active_osd_text(&mut self) -> Option<(String, bool)> {
        match &self.osd {
            Some(osd) if Instant::now() < osd.expires_at => Some((osd.text.clone(), osd.warning)),
            Some(_) => {
                self.osd = None;
                None
            }
            None => None,
        }
    }

    pub(super) fn dump_frame_if_due(&mut self) -> bool {
        let Some(state) = self.frame_dump.as_ref() else {
            return false;
        };
        if self.emu.bus().emulated_seconds() < state.start_secs as f64 {
            return false;
        }
        let emulated_frame = self.emu.bus().emulated_frames();
        if state.last_saved_emulated_frame == Some(emulated_frame) {
            return false;
        }
        self.finish_render_for_current_frame();
        if self.last_rendered_emulated_frame != Some(emulated_frame) {
            return false;
        }

        let Some(state) = self.frame_dump.as_mut() else {
            return false;
        };
        let path = state.dir.join(format!("frame-{:06}.png", state.dumped));
        if crate::envcfg::flag("COPPERLINE_DUMP_RENDER_META") {
            log_frame_dump_metadata(state.dumped, &self.emu);
        }
        let src_rows = self.present_rows;
        let result = save_present_frame(
            &path,
            &self.present_fb,
            src_rows,
            self.present_width,
            self.overscan,
            self.tv_centre,
            self.present_tv_aperture_rows,
        );
        match result {
            Ok(()) => {
                state.last_saved_emulated_frame = Some(emulated_frame);
                state.dumped += 1;
                if state.dumped == 1 || state.dumped == state.count || state.dumped % 25 == 0 {
                    info!(
                        "frame dump: saved {}/{} ({})",
                        state.dumped,
                        state.count,
                        path.display()
                    );
                }
            }
            Err(e) => {
                warn!("frame dump failed ({}): {e:#}", path.display());
                self.frame_dump = None;
                return true;
            }
        }

        if state.dumped >= state.count {
            info!(
                "frame dump complete: saved {} frames to {}",
                state.count,
                state.dir.display()
            );
            self.emu.report_stats();
            self.emu.bus().poll_stats.dump_top("at frame dump");
            self.frame_dump = None;
            true
        } else {
            false
        }
    }

    /// Toggle host power. Powering off cold-resets the machine (clearing
    /// RAM) and parks a test screen on the display; powering on boots the
    /// freshly cold machine. The redraw keeps the status-bar button and
    /// display current.
    pub(super) fn toggle_power(&mut self) {
        if self.powered_on {
            self.power_off();
        } else {
            self.powered_on = true;
            self.sync_live_audio_suspension();
            #[cfg(feature = "fluxbridge")]
            self.attach_configured_bridges();
            // The lent disks powering off gave up, lent again -- the session
            // still holds them, so no permission is asked twice.
            self.attach_configured_host_disks();
            info!("power button: machine powered on (cold boot)");
            // A --run session that started powered off begins its warp
            // launch at the first power-on.
            self.engage_warp_launch();
        }
        self.request_redraw();
    }

    /// Open the real floppy drives this machine's configuration asks for.
    ///
    /// Powering off let go of them, so powering back on has to take them
    /// again. A drive that will not open is logged rather than refused: the
    /// machine comes up with an empty bay, which is what an Amiga with a dead
    /// drive does, and the alternative is a power button that does nothing.
    #[cfg(feature = "fluxbridge")]
    pub(super) fn attach_configured_bridges(&mut self) {
        let raw = self.machine_config.clone();
        let cfg = match crate::config::Config::try_from(raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!("could not re-read the configuration to open the physical drives: {e:#}");
                return;
            }
        };
        if !cfg.floppy.bridges.iter().any(Option::is_some) {
            return;
        }
        let floppy = &mut self.emu.bus_mut().floppy;
        if let Err(e) = crate::emulator::attach_floppy_bridges(floppy, &cfg) {
            warn!("physical floppy drive not available: {e:#}");
        }
    }

    /// Put the real disks back on the machine's cables after a power cycle.
    ///
    /// Powering off hands them to the host, so powering on has to take them
    /// again or the machine comes back up with the slot empty. Nothing is
    /// asked of the user: the disks were taken from the host once and are
    /// still held, so this only puts them back where the guest looks for them.
    pub(super) fn attach_configured_host_disks(&mut self) {
        let raw = self.machine_config.clone();
        let cfg = match crate::config::Config::try_from(raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!("could not re-read the configuration to open the real disks: {e:#}");
                return;
            }
        };
        if cfg.host_disks.is_empty() {
            return;
        }
        let back = self.emu.bus_mut().attach_host_disks(&cfg);
        if back > 0 {
            info!("power button: {back} host disk(s) back on with the machine");
        }
    }

    /// Toggle host-level pause. Pausing freezes the emulator in place
    /// (it stops stepping but stays powered on), so the current frame is
    /// held and emulation resumes from the same point when unpaused.
    pub(super) fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.sync_live_audio_suspension();
        if self.paused {
            info!("pause button: emulation paused");
            // A user pause completes a remote client's pending resume;
            // the client learns where the machine stopped.
            #[cfg(feature = "control")]
            self.control_complete_pending("user_pause", "paused from the window");
        } else {
            info!("pause button: emulation resumed");
        }
        self.request_redraw();
    }

    /// Power off: drop into a cold-boot state (RAM cleared) and park the
    /// test screen, so a later power-on comes up as a clean power cycle.
    pub(super) fn power_off(&mut self) {
        // A key held on the on-screen keyboard is let go before the power
        // goes, so the cold-boot machine starts with the caps up and
        // nothing latched against the machine that just stopped.
        self.release_keyboard_panel_holds();
        // A pending warp launch dies with the machine; give the pacing
        // back so the next power-on runs at normal speed.
        if let Some(launch) = self.warp_launch.take() {
            if launch.engaged {
                self.emu.set_paced(true);
            }
            info!("warp launch: cancelled by power off");
        }
        self.powered_on = false;
        self.paused = false;
        self.sync_live_audio_suspension();
        // A real drive is powered by the machine: with the Amiga off it stops,
        // and the interface belongs to the host again. Holding it open would
        // leave it clicking as though the machine were still running, and
        // nothing else -- including the next machine this window builds --
        // could open it.
        #[cfg(feature = "fluxbridge")]
        self.emu.bus_mut().floppy.release_bridges();
        // A real hard disk is different: the machine only ever borrowed it
        // from the session's own hold, which the launcher still shows as
        // attached. The machine's copies go -- an off machine holds nothing
        // -- but the disk stays taken, so powering back on lends it again
        // without a second permission prompt, and only the launcher's
        // Unmount (or quitting) actually hands it back to the host.
        let released = self.emu.bus_mut().release_host_disks();
        if released > 0 {
            info!("power button: {released} host disk(s) off with the machine, still held for it");
        }
        info!("power button: machine powered off (cold boot state)");
        #[cfg(feature = "control")]
        self.control_complete_pending("pause", "power state changed");
        if let Err(e) = self.emu.power_on_reset() {
            error!("cold power-on reset failed: {e:#}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        } else {
            self.cpu_halted = false;
            self.sync_live_audio_suspension();
        }
        self.held_rawkeys = [false; 128];
        self.reset_render_pipeline();
        self.last_fdd_track = None;
        paint_test_screen(&mut self.fb);
        self.deinterlacer
            .push_field(&self.fb, FB_HEIGHT, FB_WIDTH, false, true, true);
        self.refresh_present_from_deinterlacer();
    }

    pub(super) fn reset_emulator(&mut self, clear_host_keys: bool) {
        // The strip's latches are a host-side affordance: they must not
        // ride through a reset and be re-reported by the MCU's power-up
        // stream, which is what `begin_power_up` does with anything the
        // matrix still shows held.
        self.release_keyboard_panel_holds();
        if let Err(e) = self.emu.keyboard_reset() {
            error!("keyboard reset failed: {e:#}");
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
        } else {
            self.cpu_halted = false;
            self.sync_live_audio_suspension();
            self.reset_render_pipeline();
            self.last_fdd_track = None;
            if clear_host_keys {
                self.held_rawkeys = [false; 128];
            }
        }
    }
}
