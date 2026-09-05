// SPDX-License-Identifier: GPL-3.0-or-later

//! Field values and edits. Hardware-dependent choices remain explicit.

use super::*;

// Simple boolean settings have one mapping shared by display and editing.
macro_rules! boolean_settings {
    ($($(#[$gate:meta])* $field:ident => $member:ident,)+) => {
        impl MachineSetup {
            fn boolean_value(&self, field: LauncherField) -> Option<bool> {
                match field {
                    $($(#[$gate])* F::$field => Some(self.$member),)+
                    _ => None,
                }
            }

            fn flip_boolean(&mut self, field: LauncherField) -> bool {
                match field {
                    $($(#[$gate])* F::$field => self.$member = !self.$member,)+
                    _ => return false,
                }
                true
            }
        }
    };
}

boolean_settings! {
    WhdloadEnabled => whdload_enabled,
    Rtc => rtc,
    Identify => identify,
    Fpu => fpu,
    Icache => icache,
    Dcache => dcache,
    Jit => jit,
    FloppySounds => floppy_sounds,
    StartFullscreen => start_fullscreen,
    ShowStatusBar => show_status_bar,
    Autocrop => autocrop,
    Deinterlace => deinterlace,
    PerfOverlay => perf_overlay,
    Mt32Panel => mt32_panel,
    #[cfg(feature = "midi")]
    SerialTelnet => serial_telnet,
    PowerOn => power_on,
    AutoLaunch => auto_launch,
    RealtimePriority => realtime_priority,
    Toccata => toccata,
    #[cfg(feature = "mhi")]
    Mhi => mhi,
    #[cfg(feature = "coppersynth")]
    CsynthPanel => csynth_panel,
}

impl MachineSetup {
    pub fn toggle_value(&self, field: LauncherField) -> bool {
        if let Some(value) = self.boolean_value(field) {
            return value;
        }
        match field {
            F::Df0WriteProtect => self.df_write_protected[0],
            F::Df1WriteProtect => self.df_write_protected[1],
            F::Df2WriteProtect => self.df_write_protected[2],
            F::Df3WriteProtect => self.df_write_protected[3],
            _ => false,
        }
    }

    pub fn value_label(&self, field: LauncherField) -> String {
        if let Some(value) = self.boolean_value(field) {
            return if value { "Enabled" } else { "Disabled" }.to_string();
        }
        match field {
            F::WhdloadMachine => match self.whdload_machine {
                crate::config::WhdloadMachine::Auto => "Auto".to_string(),
                crate::config::WhdloadMachine::Copperline => "Copperline".to_string(),
            },
            F::Chipset => chipset_name(self.chipset).to_string(),
            F::Rtg => rtg_card_name(self.rtg).to_string(),
            F::Agnus => match self.agnus {
                None => "Auto".to_string(),
                Some(a) => agnus_name(a).to_string(),
            },
            F::Denise => match self.denise {
                None => "Auto".to_string(),
                Some(d) => denise_name(d).to_string(),
            },
            F::Video => video_name(self.video).to_string(),
            F::Cpu => cpu_name(self.cpu).to_string(),
            F::Clock => format_mhz(self.clock_mhz),
            F::ChipRam => size_label(self.chip_ram),
            F::FastRam => size_label(self.fast_ram),
            F::SlowRam => size_label(self.slow_ram),
            F::RamInit => match self.ram_init {
                RamInit::Zero => "Zero".to_string(),
                RamInit::Pattern { .. } => "Fixed".to_string(),
                RamInit::Random { .. } => "Random".to_string(),
            },
            F::RamPattern => format!("0x{:04X}", self.ram_pattern),
            F::MbRam => size_label(self.mb_ram),
            F::AccelRam => size_label(self.accel_ram),
            F::Z3Ram => size_label(self.z3_ram),
            F::FloppyDrives => self.floppy_drives.to_string(),
            F::FloppySpeed => crate::floppy::speed_label(self.floppy_speed),
            F::CdInsertDelay => {
                if self.cd_insert_delay <= 0.0 {
                    "At boot".to_string()
                } else {
                    format!("{:.0} s", self.cd_insert_delay)
                }
            }
            F::Overscan => match self.overscan {
                Overscan::Tv => "TV".to_string(),
                Overscan::Full => "Full".to_string(),
            },
            F::PixelAspect => match self.pixel_aspect {
                PixelAspect::Tv => "TV (4:3)".to_string(),
                PixelAspect::Square => "Square".to_string(),
            },
            F::Scaling => self.scaling.label().to_string(),
            F::Tint => self.tint.menu_label().to_string(),
            F::Bezel => self.bezel.menu_label().to_string(),
            F::MenuScale => self.menu_scale.menu_label().to_string(),
            F::Mt32Lcd => self.mt32_lcd.menu_label().to_string(),
            F::Cartridge => cartridge_label(self.cartridge).to_string(),
            F::Phosphor => {
                if self.phosphor <= 0.0 {
                    "Disabled".to_string()
                } else {
                    format!("{:.2}", self.phosphor)
                }
            }
            F::BridgeDevice => match self.bridge_edit() {
                None => "(none)".to_string(),
                Some(_) if self.df_bridge_none[self.bridge_edit_drive] => "None".to_string(),
                Some(c) => c.driver.label().to_string(),
            },
            F::BridgePort => match self.bridge_edit().and_then(|c| c.port.clone()) {
                None => "Automatic".to_string(),
                Some(p) => p,
            },
            // Named as the drive's own jumpers are: A/B on an IBM PC cable,
            // DS0..DS3 on a Shugart one.
            F::BridgeCable => match self.bridge_edit().map(|c| c.cable) {
                Some(BridgeCable::DriveA) => "Drive A (IBM)".to_string(),
                Some(BridgeCable::DriveB) => "Drive B (IBM)".to_string(),
                Some(BridgeCable::Shugart0) => "DS0 (Shugart)".to_string(),
                Some(BridgeCable::Shugart1) => "DS1 (Shugart)".to_string(),
                Some(BridgeCable::Shugart2) => "DS2 (Shugart)".to_string(),
                Some(BridgeCable::Shugart3) => "DS3 (Shugart)".to_string(),
                None => "(none)".to_string(),
            },
            F::BridgeDensity => match self.bridge_edit().map(|c| c.density) {
                Some(BridgeDensity::Auto) => "Automatic".to_string(),
                Some(BridgeDensity::Dd) => "DD only".to_string(),
                Some(BridgeDensity::Hd) => "HD only".to_string(),
                None => "(none)".to_string(),
            },
            F::BridgeReadMode => match self.bridge_edit().map(|c| c.mode) {
                Some(BridgeReadMode::Compatible) => "Compatible".to_string(),
                Some(BridgeReadMode::Normal) => "Normal".to_string(),
                Some(BridgeReadMode::Stalling) => "Stalling".to_string(),
                None => "(none)".to_string(),
            },
            F::BridgeReplaySpeed => match self.bridge_edit().map_or(100, |c| c.speed) {
                200 => "Fast".to_string(),
                _ => "Normal".to_string(),
            },
            F::Shader => self.shader.kind().menu_label().to_string(),
            F::ShaderStrength => format!("{:.2}", self.shader_strength),
            F::FloppyVolume => format!("{}%", self.floppy_volume),
            F::PacingBudget => match self.pacing_budget {
                PacingBudget::Cycles => "Cycles".to_string(),
                PacingBudget::Instructions => "Instructions".to_string(),
            },
            F::Warp => self.warp.label().to_string(),
            F::WarpBoot => match (self.warp_until, self.warp_boot) {
                // A warp_until from the TOML shows as its own state; the
                // panel's own two states are Disabled -- the word every
                // other toggle on the page uses -- and storage-idle.
                (Some(secs), _) => format!("Until {}", format_secs(secs)),
                (None, true) => "Storage idle".to_string(),
                (None, false) => "Disabled".to_string(),
            },
            F::WarpBootIdle => format_secs(self.warp_boot_idle),
            F::Joystick => self.joystick_input_mode.menu_label().to_string(),
            F::MouseSensitivity => crate::config::mouse_sensitivity_label(self.mouse_sensitivity),
            F::MouseCapture => match self.mouse_capture {
                MouseCapture::Click => "On click".to_string(),
                MouseCapture::Auto => "Automatic".to_string(),
                MouseCapture::Manual => "Shortcut only".to_string(),
            },
            F::Port1Device => PortDevice::menu_label(self.port_devices[0]).to_string(),
            F::Port2Device => PortDevice::menu_label(self.port_devices[1]).to_string(),
            F::ScsiController => match self.scsi_controller {
                None => "None".to_string(),
                Some(ScsiController::A2091) => "A2091 (Z2)".to_string(),
                Some(ScsiController::A4091) => "A4091 (Z3)".to_string(),
                Some(ScsiController::A3000) => "A3000 (onboard)".to_string(),
            },
            F::LideBoard => match self.lide_board {
                None => "None".to_string(),
                Some(LidePersonality::Ripple) => "RIPPLE".to_string(),
                Some(LidePersonality::Ride) => "RIDE".to_string(),
                Some(LidePersonality::AtBus2008) => "AT-Bus 2008".to_string(),
            },
            #[cfg(feature = "midi")]
            F::SerialMode => match self.serial_mode {
                // "None" (matching the Parallel device selector) reads better
                // than "Off" for the no-connection state.
                SerialMode::Off => "None".to_string(),
                SerialMode::Stdout => "Stdout".to_string(),
                SerialMode::Midi => "MIDI".to_string(),
                SerialMode::Tcp => "TCP".to_string(),
                SerialMode::TcpConnect => "TCP connect".to_string(),
                SerialMode::Pty => "PTY".to_string(),
                SerialMode::Modem => "Modem".to_string(),
            },
            // The dial-out address has no default -- there is no host to
            // guess -- so an empty box says what it wants instead.
            #[cfg(feature = "midi")]
            F::SerialConnect => self
                .serial_connect
                .as_deref()
                .map(complete_connect)
                .unwrap_or_else(|| "(host:port)".to_string()),
            // The listen address does have a default, so an empty box shows
            // the address the run would actually bind.
            #[cfg(feature = "midi")]
            F::SerialListen => self
                .serial_listen
                .as_deref()
                .map(complete_listen)
                .unwrap_or_else(|| crate::config::SERIAL_TCP_DEFAULT_LISTEN.to_string()),
            #[cfg(feature = "midi")]
            F::MidiOut => {
                if self.midi_out_is_mt32() {
                    return crate::midi::MIDI_OUT_MT32_LABEL.to_string();
                }
                if self.midi_out_is_csynth() {
                    return crate::midi::MIDI_OUT_CSYNTH_LABEL.to_string();
                }
                self.midi_out.clone().unwrap_or_else(|| "None".to_string())
            }
            #[cfg(feature = "midi")]
            F::MidiIn => {
                #[cfg(feature = "mt32")]
                if crate::config::midi_out_is_mt32(self.midi_in.as_deref()) {
                    return crate::midi::MIDI_OUT_MT32_LABEL.to_string();
                }
                self.midi_in.clone().unwrap_or_else(|| "None".to_string())
            }
            #[cfg(feature = "coppersynth")]
            F::CsynthSoundfont if self.csynth_soundfont.is_none() => {
                // The bundled bank, named rather than blank: an unset row
                // is not an empty setting, it is the default in force.
                "GeneralUser-GS".to_string()
            }
            #[cfg(feature = "coppersynth")]
            F::CsynthMt32Mode => match self.csynth_mt32_mode.as_deref() {
                None => "Auto".to_string(),
                Some(m) if m.eq_ignore_ascii_case("on") => "On".to_string(),
                Some(m) if m.eq_ignore_ascii_case("off") => "Off".to_string(),
                Some(_) => "Auto".to_string(),
            },
            F::ParallelDevice => match self.parallel_device {
                ParallelDevice::None => "None".to_string(),
                ParallelDevice::Printer => "Printer".to_string(),
                ParallelDevice::Sampler => "Sampler".to_string(),
            },
            F::SamplerInput => self
                .sampler_input
                .clone()
                .unwrap_or_else(|| "Default".to_string()),
            F::SamplerGain => sampler_gain_label(self.sampler_gain_db),
            F::Ethernet => match self.a2065_net.as_ref() {
                None => "None".to_string(),
                Some(NetConfig::None) => "Isolated".to_string(),
                Some(NetConfig::Loopback) => "Loopback".to_string(),
                Some(NetConfig::Nat) => "NAT".to_string(),
                Some(NetConfig::Bridge { .. }) => "Bridged".to_string(),
            },
            F::EthernetInterface => match self.a2065_net.as_ref() {
                Some(NetConfig::Bridge { interface }) => self
                    .bridge_interfaces
                    .iter()
                    .find(|(name, _)| name == interface)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| format!("{interface} (unavailable)")),
                _ => "—".to_string(),
            },
            F::HostSocket if self.hostsocket_host_mode => "Host".to_string(),
            F::HostSocket => match self.hostsocket_net.as_ref() {
                None => "None".to_string(),
                Some(NetConfig::None) => "Isolated".to_string(),
                Some(NetConfig::Loopback) => "Loopback".to_string(),
                Some(NetConfig::Nat) => "NAT".to_string(),
                Some(NetConfig::Bridge { .. }) => "Bridged".to_string(),
            },
            F::HostSocketInterface => match self.hostsocket_net.as_ref() {
                Some(NetConfig::Bridge { interface }) => self
                    .bridge_interfaces
                    .iter()
                    .find(|(name, _)| name == interface)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| format!("{interface} (unavailable)")),
                _ => "—".to_string(),
            },
            F::AudioDevice => self.audio_output.label().to_string(),
            F::AudioChannelMode => match self.audio_channel_mode {
                ChannelMode::Stereo => "Stereo".to_string(),
                ChannelMode::Mono => "Mono".to_string(),
            },
            F::AudioStereoSeparation => format!("{}%", self.audio_stereo_separation),
            F::AudioFilter => match self.audio_filter {
                AudioFilterMode::Auto => "Auto".to_string(),
                AudioFilterMode::On => "Enabled".to_string(),
                AudioFilterMode::Off => "Disabled".to_string(),
            },
            F::Filesys0Boot | F::Filesys1Boot | F::Filesys2Boot | F::Filesys3Boot => {
                let (slot, _) = filesys_slot(field).expect("boot field");
                match self.filesys_bootpri[slot] {
                    -128 => "Never".to_string(),
                    pri => pri.to_string(),
                }
            }
            F::IdeMasterBoot
            | F::IdeSlaveBoot
            | F::ScsiUnit0Boot
            | F::ScsiUnit1Boot
            | F::ScsiUnit2Boot
            | F::ScsiUnit3Boot
            | F::ScsiUnit4Boot
            | F::ScsiUnit5Boot
            | F::ScsiUnit6Boot
            | F::LideDrive0Boot
            | F::LideDrive1Boot
            | F::LideDrive2Boot
            | F::LideDrive3Boot
            | F::CopperhfUnit0Boot
            | F::CopperhfUnit1Boot
            | F::CopperhfUnit2Boot
            | F::CopperhfUnit3Boot
            | F::CopperhfUnit4Boot
            | F::CopperhfUnit5Boot
            | F::CopperhfUnit6Boot => drive_bootpri_label(self.effective_bootpri(field)),
            F::Filesys0ReadOnly
            | F::Filesys1ReadOnly
            | F::Filesys2ReadOnly
            | F::Filesys3ReadOnly => {
                let slot = filesys_readonly_slot(field).expect("readonly field");
                if self.filesys_readonly[slot] {
                    "Read-only".to_string()
                } else {
                    "Read-write".to_string()
                }
            }
            // SCSI, IDE, and lide drive slots: flag CD images, which attach
            // a CD-ROM drive (SCSI or ATAPI) rather than a hard disk there.
            F::ScsiUnit0
            | F::ScsiUnit1
            | F::ScsiUnit2
            | F::ScsiUnit3
            | F::ScsiUnit4
            | F::ScsiUnit5
            | F::ScsiUnit6
            | F::IdeMaster
            | F::IdeSlave
            | F::LideDrive0
            | F::LideDrive1
            | F::LideDrive2
            | F::LideDrive3 => {
                let label = self.path_label(field, "(none)");
                match self.path(field) {
                    Some(p) if crate::config::is_cd_image_path(p) => format!("{label} (CD-ROM)"),
                    _ => label,
                }
            }
            // The WHDLoad directories all have a place they go when unset,
            // under the one WHDLoad directory (crate::paths::whdload_dir),
            // so their placeholder says the setting is doing something
            // rather than nothing. The game itself has no default, and the
            // two support archives are either there or not.
            F::WhdloadKickstarts | F::WhdloadLibrary | F::WhdloadGames => {
                self.path_label(field, "(default)")
            }
            F::WhdloadWhdPackage | F::WhdloadSkickPackage => self.path_label(field, "(none)"),
            // Path/drive fields: the file name, or a placeholder.
            F::Rom => self.path_label(field, "(bundled AROS)"),
            F::FmvRom if self.fmv_rom_disabled => "(no FMV module)".to_string(),
            F::FmvRom => self.path_label(field, "(bundled open FMV ROM)"),
            // Both Zorro SCSI boards have bundled open autoboot ROMs.
            F::ScsiRom if self.scsi_bundled_rom_label().is_some() => {
                self.path_label(field, self.scsi_bundled_rom_label().unwrap())
            }
            // A fitted lide board defaults to a bundled ROM: lide.rom for
            // RIPPLE/RIDE, lide-atbus.rom for AT-Bus 2008 -- not the same
            // file. rom = "" (hardware-only mode) has no field of its own
            // to hold the path, so it borrows this row's placeholder text
            // instead.
            F::LideRom if self.lide_rom_disabled => "(hardware-only, no ROM)".to_string(),
            F::LideRom if self.lide_board == Some(LidePersonality::AtBus2008) => {
                self.path_label(field, "(bundled lide-atbus.rom)")
            }
            F::LideRom => self.path_label(field, "(bundled lide.rom)"),
            F::LideRomBank2 if self.lide_rom_bank2_disabled => "(none)".to_string(),
            F::LideRomBank2 => self.path_label(field, "(bundled cdfs.rom)"),
            _ if rows_contains_kind(field, RowKind::Path)
                || rows_contains_kind(field, RowKind::Drive)
                || rows_contains_kind(field, RowKind::FloppyMedia) =>
            {
                self.path_label(field, "(none)")
            }
            // Toggles
            _ => {
                if self.toggle_value(field) {
                    "On".to_string()
                } else {
                    "Off".to_string()
                }
            }
        }
    }

    /// Change the fixed power-on word and make it the active policy. The text
    /// box only applies in Fixed mode, but setting both here keeps this method
    /// correct if another frontend reuses it directly.
    pub fn set_ram_pattern(&mut self, word: u16) {
        self.ram_pattern = word;
        self.ram_init = RamInit::Pattern { word };
    }

    pub(super) fn path_label(&self, field: LauncherField, empty: &str) -> String {
        match self.path(field) {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            None => empty.to_string(),
        }
    }

    /// The shader picker's options: the built-in presets, plus the user
    /// shader the loaded config named. There is no file browser for shaders
    /// here, so Custom is offered only when a path came in with the config.
    pub(super) fn shader_options(&self) -> Vec<ShaderMode> {
        let mut options = vec![
            ShaderMode::None,
            ShaderMode::Scanlines,
            ShaderMode::Mask,
            ShaderMode::Crt,
        ];
        if let Some(path) = &self.shader_custom {
            options.push(ShaderMode::Custom(path.clone()));
        }
        options
    }

    /// Like [`cycle_slice`], but for the non-`Copy` shader options.
    pub(super) fn cycled_shader(&self, forward: bool) -> ShaderMode {
        let options = self.shader_options();
        let n = options.len();
        let idx = options.iter().position(|m| *m == self.shader).unwrap_or(0);
        let next = if forward {
            (idx + 1) % n
        } else {
            (idx + n - 1) % n
        };
        options[next].clone()
    }

    /// Step a cycle/stepper field forward (`forward`) or backward.
    pub fn cycle(&mut self, field: LauncherField, forward: bool) {
        if self.flip_boolean(field) {
            return;
        }
        match field {
            F::WhdloadMachine => {
                use crate::config::WhdloadMachine as M;
                self.whdload_machine = match self.whdload_machine {
                    M::Auto => M::Copperline,
                    M::Copperline => M::Auto,
                };
                let _ = forward;
            }
            F::Chipset => self.chipset = cycle_slice(&CHIPSETS, self.chipset, forward),
            F::Rtg => {
                // The Zorro III cards sit at the list's tail so a 24-bit CPU
                // can cycle everything before them (the Zorro II cards).
                let cards = if cpu_is_32bit(self.cpu) {
                    &RTG_CARDS[..]
                } else {
                    &RTG_CARDS[..4]
                };
                self.rtg = cycle_slice(cards, self.rtg, forward);
            }
            F::Agnus => self.agnus = cycle_slice(&AGNUS_CHOICES, self.agnus, forward),
            F::Denise => self.denise = cycle_slice(&DENISE_CHOICES, self.denise, forward),
            F::Video => self.video = cycle_slice(&VIDEO_CHOICES, self.video, forward),
            F::Cpu => {
                self.cpu = cycle_slice(&CPUS, self.cpu, forward);
                // Re-derive the CPU-dependent toggles for the new part, as if
                // the model had been picked fresh (the panel greys whichever
                // do not apply).
                self.fpu = self.cpu.default_fpu();
                self.icache = self.cpu.has_instruction_cache();
                self.dcache = self.cpu.has_data_cache();
                self.clock_mhz = self.cpu.default_clock_mhz();
                if !cpu_is_32bit(self.cpu) {
                    // Zorro III RAM, motherboard RAM, accelerator RAM, and
                    // the Zorro III RTG cards all sit beyond a 24-bit bus;
                    // dropping them (rather than just greying their rows)
                    // keeps the emitted config launchable. Picasso II/II+ and
                    // Graffity [Zorro II] remain fitted (Zorro II cards).
                    self.z3_ram = 0;
                    self.mb_ram = 0;
                    self.accel_ram = 0;
                    if matches!(self.rtg, RtgCard::Z3660 | RtgCard::GraffityZ3) {
                        self.rtg = RtgCard::None;
                    }
                }
            }
            F::Clock => self.clock_mhz = cycle_floats(&CLOCK_PRESETS, self.clock_mhz, forward),
            F::ChipRam => self.chip_ram = cycle_slice(&CHIP_PRESETS, self.chip_ram, forward),
            F::FastRam => self.fast_ram = cycle_nearest(&FAST_PRESETS, self.fast_ram, forward),
            F::SlowRam => self.slow_ram = cycle_nearest(&SLOW_PRESETS, self.slow_ram, forward),
            F::RamInit => {
                self.ram_init = match (self.ram_init, forward) {
                    (RamInit::Zero, true) | (RamInit::Pattern { .. }, false) => RamInit::Random {
                        seed: self.ram_random_seed,
                    },
                    (RamInit::Random { .. }, true) | (RamInit::Zero, false) => RamInit::Pattern {
                        word: self.ram_pattern,
                    },
                    (RamInit::Pattern { .. }, true) | (RamInit::Random { .. }, false) => {
                        RamInit::Zero
                    }
                };
            }
            F::MbRam => {
                // Only the A4000's Ramsey-07 extends past its four banks
                // into the $04000000-$06FFFFFF expansion space.
                let presets: &[usize] = if self.model == Some(MachineModel::A4000) {
                    &MB_PRESETS_A4000
                } else {
                    &MB_PRESETS
                };
                self.mb_ram = cycle_nearest(presets, self.mb_ram, forward);
            }
            F::AccelRam => self.accel_ram = cycle_nearest(&ACCEL_PRESETS, self.accel_ram, forward),
            F::Z3Ram => self.z3_ram = cycle_nearest(&Z3_PRESETS, self.z3_ram, forward),
            F::FloppyDrives => {
                let requested = step_u8(self.floppy_drives, forward, 0, 4);
                // A bay that is no longer fitted has no business holding a
                // physical drive open: the row is gone from the page, so
                // nothing would say why the interface was busy the next time
                // it was asked for.
                #[cfg(feature = "fluxbridge")]
                for bay in self.df_bridge.iter_mut().skip(requested as usize) {
                    *bay = None;
                }
                // Images are not discarded implicitly. Keep their rows visible
                // until the user clears them, so the displayed count and the
                // count emitted by to_raw() cannot disagree.
                self.floppy_drives = requested.max(self.image_floppy_bays());
            }
            F::FloppySpeed => {
                self.floppy_speed = cycle_slice(&FLOPPY_SPEEDS, self.floppy_speed, forward)
            }
            F::CdInsertDelay => {
                let secs = self.cd_insert_delay + if forward { 1.0 } else { -1.0 };
                self.cd_insert_delay = secs.clamp(0.0, 60.0);
            }
            F::Phosphor => {
                let p = self.phosphor + if forward { 0.05 } else { -0.05 };
                // Snap to the 0.05 grid to avoid float drift accumulating.
                self.phosphor = (p.clamp(0.0, 0.95) * 20.0).round() / 20.0;
            }
            F::Shader => self.shader = self.cycled_shader(forward),
            F::ShaderStrength => {
                let s = self.shader_strength + if forward { 0.1 } else { -0.1 };
                // Snap to the 0.1 grid to avoid float drift accumulating.
                self.shader_strength = (s.clamp(0.0, 1.0) * 10.0).round() / 10.0;
            }
            F::FloppyVolume => self.floppy_volume = step_u8(self.floppy_volume, forward, 0, 100),
            F::Overscan => self.overscan = cycle_slice(&OVERSCANS, self.overscan, forward),
            F::Tint => self.tint = cycle_slice(&TINTS, self.tint, forward),
            F::Bezel => self.bezel = cycle_slice(&BezelStyle::MENU_ORDER, self.bezel, forward),
            F::MenuScale => {
                self.menu_scale = cycle_slice(&MenuScale::MENU_ORDER, self.menu_scale, forward);
            }
            F::Mt32Lcd => {
                self.mt32_lcd = cycle_slice(&Mt32Lcd::MENU_ORDER, self.mt32_lcd, forward);
            }
            // Two states cycle the same either way round.
            F::Cartridge => {
                self.cartridge = match self.cartridge {
                    None => Some(crate::cartridge::CartridgeModel::Hrtmon),
                    Some(_) => None,
                }
            }
            F::PixelAspect => {
                self.pixel_aspect = cycle_slice(&PIXEL_ASPECTS, self.pixel_aspect, forward)
            }
            F::Scaling => {
                self.scaling = cycle_slice(&DisplayScaling::MENU_ORDER, self.scaling, forward)
            }
            F::PacingBudget => {
                self.pacing_budget = cycle_slice(&PACINGS, self.pacing_budget, forward)
            }
            F::Warp => self.warp = cycle_slice(&WARPS, self.warp, forward),
            F::WarpBoot => {
                // Two panel states, Off and storage-idle (the boot warps
                // until the floppy/HDD LEDs have been quiet for the idle
                // threshold below). A timestamp warp (warp_until, set from
                // TOML or --warp-until) shows as a third state; the modes
                // are mutually exclusive, so one press clears it and lands
                // on Off in either direction.
                if self.warp_until.take().is_some() {
                    self.warp_boot = false;
                } else {
                    self.warp_boot = !self.warp_boot;
                }
            }
            F::WarpBootIdle => {
                self.warp_boot_idle =
                    cycle_floats(&WARP_BOOT_IDLE_PRESETS, self.warp_boot_idle, forward)
            }
            F::Joystick => {
                self.joystick_input_mode =
                    cycle_slice(&JOYSTICK_MODES, self.joystick_input_mode, forward)
            }
            F::MouseSensitivity => {
                self.mouse_sensitivity = if forward {
                    self.mouse_sensitivity.saturating_add(1).min(100)
                } else {
                    self.mouse_sensitivity.saturating_sub(1)
                }
            }
            F::MouseCapture => {
                self.mouse_capture = cycle_slice(&MOUSE_CAPTURES, self.mouse_capture, forward)
            }
            F::Port1Device => {
                self.port_devices[0] = cycle_slice(&PORT1_DEVICES, self.port_devices[0], forward)
            }
            F::Port2Device => {
                self.port_devices[1] = cycle_slice(&PORT_DEVICES, self.port_devices[1], forward)
            }
            F::ScsiController => {
                // The motherboard SCSI is only on offer where the silicon is.
                let choices: Vec<Option<ScsiController>> = SCSI_CONTROLLERS
                    .into_iter()
                    .filter(|c| self.has_sdmac() || *c != Some(ScsiController::A3000))
                    .collect();
                self.scsi_controller = cycle_slice(&choices, self.scsi_controller, forward);
                self.drop_unreachable_host_disks();
            }
            F::LideBoard => {
                self.lide_board = cycle_slice(&LIDE_BOARDS, self.lide_board, forward);
                // Drop drives beyond the new board's channel count, so a
                // RIPPLE-only channel 1 drive does not linger unreachable
                // (and unrepresentable -- `[lide] drives` is positional)
                // behind a board that no longer has that channel.
                if let Some(board) = self.lide_board {
                    for slot in board.max_drives()..self.lide_drives.len() {
                        self.lide_drives[slot] = None;
                        self.lide_drive_names[slot] = None;
                        self.lide_drive_bootpri[slot] = None;
                        self.lide_drive_boot_off[slot] = false;
                    }
                }
                // A real host disk on a channel the new personality lacks
                // (e.g. RIPPLE channel 1 -> RIDE/AT-Bus 2008) is just as
                // unreachable as an image drive there, and just as invisible
                // if left attached -- drop it the same way ScsiController's
                // handler above does for its own board switch.
                self.drop_unreachable_host_disks();
            }
            #[cfg(feature = "midi")]
            F::SerialMode => {
                // Every mode is on offer: choosing tcp-connect brings its
                // Connect box with it, so the address the mode needs can be
                // typed here rather than only in a hand-written config.
                self.serial_mode = cycle_slice(&SERIAL_MODES, self.serial_mode, forward)
            }
            #[cfg(feature = "midi")]
            F::MidiOut => {
                // The built-in synths ride at the end of the output
                // list: always there to be chosen, whatever the host
                // offers -- the MT-32 first, then Coppersynth.
                let names: Vec<String> = self
                    .midi_endpoints
                    .outputs
                    .iter()
                    .map(|e| e.name.clone())
                    .chain(mt32_endpoint(true))
                    .chain(csynth_endpoint(true))
                    .collect();
                self.midi_out =
                    crate::midi::next_endpoint(self.midi_out.as_deref(), &names, forward);
                // The MT-32 is only a source while it is the destination,
                // so moving the output elsewhere takes the input with it.
                #[cfg(feature = "mt32")]
                if !self.midi_out_is_mt32()
                    && crate::config::midi_out_is_mt32(self.midi_in.as_deref())
                {
                    self.midi_in = None;
                }
            }
            #[cfg(feature = "midi")]
            F::MidiIn => {
                // The module is a sound module: it has no keyboard, and
                // what it sends is an answer to what it was sent. So it is
                // offered as a source only while it is the destination,
                // which is also the wiring a patch editor needs.
                let names: Vec<String> = self
                    .midi_endpoints
                    .inputs
                    .iter()
                    .map(|e| e.name.clone())
                    .chain(mt32_endpoint(self.midi_out_is_mt32()))
                    .collect();
                self.midi_in = crate::midi::next_endpoint(self.midi_in.as_deref(), &names, forward);
            }
            #[cfg(feature = "coppersynth")]
            F::CsynthMt32Mode => {
                // Auto -> On -> Off, stored as the config spells it, with
                // Auto stored as unset so an untouched row emits nothing.
                let next = match self.csynth_mt32_mode.as_deref() {
                    None => Some("on"),
                    Some(m) if m.eq_ignore_ascii_case("on") => Some("off"),
                    Some(m) if m.eq_ignore_ascii_case("off") => None,
                    Some(_) => Some("on"),
                };
                let next = if forward {
                    next
                } else {
                    // The same ring walked the other way.
                    match self.csynth_mt32_mode.as_deref() {
                        None => Some("off"),
                        Some(m) if m.eq_ignore_ascii_case("off") => Some("on"),
                        Some(m) if m.eq_ignore_ascii_case("on") => None,
                        Some(_) => None,
                    }
                };
                self.csynth_mt32_mode = next.map(str::to_string);
            }
            F::ParallelDevice => {
                // None -> Printer -> Sampler. Selecting Printer reveals its
                // Output file row (with a Browse button); until a file is set
                // the printer is not persisted or attached (see to_raw).
                const DEVICES: [ParallelDevice; 3] = [
                    ParallelDevice::None,
                    ParallelDevice::Printer,
                    ParallelDevice::Sampler,
                ];
                self.parallel_device = cycle_slice(&DEVICES, self.parallel_device, forward);
            }
            F::SamplerInput => {
                // Re-read on each step so a device connected since the screen
                // opened appears; on-demand only, so no background polling.
                self.refresh_sampler_inputs();
                self.sampler_input = crate::sampler::next_input_device(
                    self.sampler_input.as_deref(),
                    &self.sampler_input_devices,
                    forward,
                );
            }
            F::SamplerGain => {
                self.sampler_gain_db =
                    cycle_floats(&SAMPLER_GAIN_STEPS, self.sampler_gain_db as f64, forward) as f32;
            }
            F::Ethernet => {
                cycle_net_board(&mut self.a2065_net, &self.bridge_interfaces, forward);
            }
            F::EthernetInterface => {
                cycle_bridge_interface(&mut self.a2065_net, &self.bridge_interfaces, forward);
            }
            F::HostSocket => {
                cycle_hostsocket_board(
                    &mut self.hostsocket_net,
                    &mut self.hostsocket_host_mode,
                    &self.bridge_interfaces,
                    forward,
                );
            }
            F::HostSocketInterface => {
                cycle_bridge_interface(&mut self.hostsocket_net, &self.bridge_interfaces, forward);
            }
            F::AudioDevice => {
                // Re-read on each step so a device connected since the screen
                // opened appears; on-demand only, so no background polling.
                self.refresh_audio_devices();
                self.audio_output = self.audio_output.cycle(&self.audio_devices, forward);
            }
            F::AudioChannelMode => {
                self.audio_channel_mode = match self.audio_channel_mode {
                    ChannelMode::Stereo => ChannelMode::Mono,
                    ChannelMode::Mono => ChannelMode::Stereo,
                }
            }
            F::AudioFilter => {
                self.audio_filter = cycle_slice(&AUDIO_FILTER_MODES, self.audio_filter, forward)
            }
            F::BridgeDevice => {
                // "None" sits before the first driver in the cycle: from it,
                // forward reaches the first interface, backward the last.
                let drivers = bridge_drivers();
                let bay = self.bridge_edit_drive;
                if self.bridge_edit().is_some() && !drivers.is_empty() {
                    if self.df_bridge_none[bay] {
                        self.df_bridge_none[bay] = false;
                        let end = if forward {
                            drivers[0]
                        } else {
                            drivers[drivers.len() - 1]
                        };
                        if let Some(c) = self.bridge_edit_mut() {
                            c.driver = end;
                        }
                    } else {
                        let (first, last) = (drivers[0], drivers[drivers.len() - 1]);
                        let at_edge = self
                            .bridge_edit()
                            .is_some_and(|c| c.driver == if forward { last } else { first });
                        if at_edge {
                            self.df_bridge_none[bay] = true;
                        } else if let Some(c) = self.bridge_edit_mut() {
                            c.driver = cycle_slice(&drivers, c.driver, forward);
                        }
                    }
                }
            }
            F::BridgePort => {
                let options = self.bridge_port_options();
                if let Some(c) = self.bridge_edit_mut() {
                    let idx = options.iter().position(|p| *p == c.port).unwrap_or(0);
                    let n = options.len();
                    let next = if forward {
                        (idx + 1) % n
                    } else {
                        (idx + n - 1) % n
                    };
                    c.port = options[next].clone();
                }
            }
            F::BridgeCable => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.cable = cycle_slice(&BRIDGE_CABLES, c.cable, forward);
                }
            }
            F::BridgeDensity => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.density = cycle_slice(&BRIDGE_DENSITIES, c.density, forward);
                }
            }
            F::BridgeReadMode => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.mode = cycle_slice(&BRIDGE_READ_MODES, c.mode, forward);
                }
            }
            F::BridgeReplaySpeed => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.speed = cycle_slice(&BRIDGE_REPLAY_SPEEDS, c.speed, forward);
                }
            }
            F::AudioStereoSeparation => {
                self.audio_stereo_separation = cycle_nearest(
                    &STEREO_SEPARATION_STEPS,
                    usize::from(self.audio_stereo_separation),
                    forward,
                ) as u8
            }
            F::IdeMasterBoot
            | F::IdeSlaveBoot
            | F::ScsiUnit0Boot
            | F::ScsiUnit1Boot
            | F::ScsiUnit2Boot
            | F::ScsiUnit3Boot
            | F::ScsiUnit4Boot
            | F::ScsiUnit5Boot
            | F::ScsiUnit6Boot
            | F::LideDrive0Boot
            | F::LideDrive1Boot
            | F::LideDrive2Boot
            | F::LideDrive3Boot => {
                // The arrows only move a live priority; a drive whose Bootable
                // box is cleared shows its number greyed and does not step.
                if !self.drive_boot_off(field) {
                    self.set_drive_bootpri(
                        field,
                        step_drive_bootpri(self.drive_bootpri(field), forward),
                    );
                }
            }
            _ => {
                if let Some((slot, true)) = filesys_slot(field) {
                    self.filesys_bootpri[slot] = cycle_bootpri(self.filesys_bootpri[slot], forward);
                } else if let Some(slot) = filesys_readonly_slot(field) {
                    // Two values: either direction lands on the other one.
                    self.filesys_readonly[slot] = !self.filesys_readonly[slot];
                }
            }
        }
    }

    /// Flip a toggle field (no-op if the field is not a toggle).
    pub fn toggle(&mut self, field: LauncherField) {
        match field {
            F::Df0WriteProtect | F::Df1WriteProtect | F::Df2WriteProtect | F::Df3WriteProtect
                if Self::drive_protect_bay(field).is_some() =>
            {
                let bay = Self::drive_protect_bay(field).expect("checked above");
                self.df_write_protected[bay] = !self.df_write_protected[bay];
                if let Some(bridge) = self.df_bridge[bay].as_mut() {
                    bridge.write_protected = self.df_write_protected[bay];
                }
            }
            F::Df0WriteProtect => self.df_write_protected[0] = !self.df_write_protected[0],
            F::Df1WriteProtect => self.df_write_protected[1] = !self.df_write_protected[1],
            F::Df2WriteProtect => self.df_write_protected[2] = !self.df_write_protected[2],
            F::Df3WriteProtect => self.df_write_protected[3] = !self.df_write_protected[3],
            _ => {}
        }
    }
}
