// SPDX-License-Identifier: GPL-3.0-or-later

//! Conversion between editable machine settings and persisted configuration.

use super::*;

impl MachineSetup {
    /// Build the typed model from a raw config, validating it through the
    /// config pipeline first. The validated [`Config`] supplies the resolved
    /// scalar values; the raw view supplies the things `Config` does not
    /// preserve: whether the Agnus/Denise were explicit overrides, the
    /// "no boot ROM = AROS" distinction, and the `[[zorro]]` board paths.
    pub fn from_raw(raw: &RawConfig) -> Result<Self> {
        let cfg: Config = raw.clone().try_into()?;
        // Boot priority is read from the raw form (the validated
        // `DriveImage` resolves it to a number, losing "unset"), so this
        // needs the same named-key-over-legacy-array merge validation used
        // rather than the positional array alone.
        let lide_raw_slots = raw.lide.drive_slots();
        // One tick box governs both kinds of bay, so read it from whichever
        // of the two a bay actually has.
        let df_write_protected = std::array::from_fn(|i| {
            cfg.floppy.drives[i]
                .as_ref()
                .map(|d| d.write_protected)
                .or_else(|| cfg.floppy.bridges[i].as_ref().map(|b| b.write_protected))
                .unwrap_or(true)
        });
        let connected = connected_floppy_bays(&cfg.floppy_connected);
        Ok(Self {
            model: cfg.machine,
            chipset: cfg.chipset,
            agnus: raw.chipset.agnus.is_some().then_some(cfg.agnus_revision),
            denise: raw.chipset.denise.is_some().then_some(cfg.denise_revision),
            video: cfg.video_standard,
            rtc: cfg.rtc_present,
            identify: cfg.identify_board,
            rtg: cfg.rtg,
            rtg_vram_bytes: cfg.rtg_vram_bytes,
            cpu: cfg.cpu,
            fpu: cfg.fpu,
            clock_mhz: cfg.cpu_clock_mhz,
            icache: cfg.cpu_icache,
            dcache: cfg.cpu_dcache,
            jit: cfg.cpu_jit,
            chip_ram: cfg.chip_ram_bytes,
            fast_ram: cfg.fast_ram_bytes,
            slow_ram: cfg.slow_ram_bytes,
            ram_init: cfg.ram_init,
            ram_pattern: match cfg.ram_init {
                RamInit::Pattern { word } => word,
                _ => DEFAULT_RAM_PATTERN,
            },
            ram_random_seed: match cfg.ram_init {
                RamInit::Random { seed } => seed,
                _ => DEFAULT_RANDOM_RAM_SEED,
            },
            mb_ram: cfg.mb_ram_bytes,
            accel_ram: cfg.accel_ram_bytes,
            z3_ram: cfg.z3_ram_bytes,
            rom: raw.rom.as_deref().map(PathBuf::from),
            extended_rom: raw.extended_rom.as_deref().map(PathBuf::from),
            fmv_rom: raw
                .fmv_rom
                .as_deref()
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
            fmv_rom_disabled: raw.fmv_rom.as_deref() == Some(""),
            floppy_drives: raw.floppy.drives.unwrap_or(connected).min(4),
            floppy_speed: cfg.floppy.speed,
            df_playlists: cfg.floppy_playlists.clone(),
            df_write_protected,
            df_bridge: std::array::from_fn(|i| cfg.floppy.bridges[i].clone()),
            // A config that names a bridge names an interface; "None" is a
            // launcher-session state, never read from a file.
            df_bridge_none: [false; 4],
            bridge_edit_drive: 0,
            bridge_status: bridge_status(),
            #[cfg(feature = "fluxbridge")]
            bridge_ports: sample_bridge_ports(),
            // Not sampled at construction: the page samples when it opens, so
            // a launcher that never visits it never touches the host's disks.
            host_disks: Vec::new(),
            host_disk_warning: None,
            host_disk_selected: Vec::new(),
            host_disk_scroll: 0,
            host_disk_scroll_rate: ScrollRate::default(),
            host_disks_attached: raw_host_disks(raw),
            ide_master: cfg.ide.master.as_ref().map(|d| d.path.clone()),
            ide_master_name: cfg.ide.master.as_ref().and_then(|d| d.volume_name.clone()),
            ide_master_fs: cfg
                .ide
                .master
                .as_ref()
                .map(|d| d.filesystem)
                .unwrap_or(crate::diskimage::FileSystem::FFS),
            ide_master_is_dir: cfg.ide.master.as_ref().is_some_and(|d| d.path.is_dir()),
            ide_master_bootpri: boot_priority_of(raw.ide.master.as_ref().and_then(|d| d.bootpri)),
            ide_master_boot_off: boot_is_off(raw.ide.master.as_ref().and_then(|d| d.bootpri)),
            ide_slave: cfg.ide.slave.as_ref().map(|d| d.path.clone()),
            ide_slave_name: cfg.ide.slave.as_ref().and_then(|d| d.volume_name.clone()),
            ide_slave_fs: cfg
                .ide
                .slave
                .as_ref()
                .map(|d| d.filesystem)
                .unwrap_or(crate::diskimage::FileSystem::FFS),
            ide_slave_is_dir: cfg.ide.slave.as_ref().is_some_and(|d| d.path.is_dir()),
            ide_slave_bootpri: boot_priority_of(raw.ide.slave.as_ref().and_then(|d| d.bootpri)),
            ide_slave_boot_off: boot_is_off(raw.ide.slave.as_ref().and_then(|d| d.bootpri)),
            scsi_controller: cfg.scsi.enabled().then_some(cfg.scsi.controller),
            scsi_rom: cfg.scsi.rom.clone(),
            scsi_rom_odd: cfg.scsi.rom_odd.clone(),
            scsi_units: std::array::from_fn(|i| cfg.scsi.units[i].as_ref().map(|d| d.path.clone())),
            scsi_unit_names: std::array::from_fn(|i| {
                cfg.scsi.units[i]
                    .as_ref()
                    .and_then(|d| d.volume_name.clone())
            }),
            scsi_unit_fs: std::array::from_fn(|i| {
                cfg.scsi.units[i]
                    .as_ref()
                    .map(|d| d.filesystem)
                    .unwrap_or(crate::diskimage::FileSystem::FFS)
            }),
            scsi_unit_is_dir: std::array::from_fn(|i| {
                cfg.scsi.units[i].as_ref().is_some_and(|d| d.path.is_dir())
            }),
            scsi_unit_bootpri: std::array::from_fn(|i| {
                boot_priority_of(raw_scsi_unit(&raw.scsi, i).and_then(|d| d.bootpri))
            }),
            scsi_unit_boot_off: std::array::from_fn(|i| {
                boot_is_off(raw_scsi_unit(&raw.scsi, i).and_then(|d| d.bootpri))
            }),
            copperhf_units: std::array::from_fn(|i| {
                cfg.copperhf.units[i].as_ref().map(|d| d.path.clone())
            }),
            copperhf_unit_names: std::array::from_fn(|i| {
                cfg.copperhf.units[i]
                    .as_ref()
                    .and_then(|d| d.volume_name.clone())
            }),
            copperhf_unit_fs: std::array::from_fn(|i| {
                cfg.copperhf.units[i]
                    .as_ref()
                    .map(|d| d.filesystem)
                    .unwrap_or(crate::diskimage::FileSystem::FFS)
            }),
            copperhf_unit_is_dir: std::array::from_fn(|i| {
                cfg.copperhf.units[i]
                    .as_ref()
                    .is_some_and(|d| d.path.is_dir())
            }),
            copperhf_unit_bootpri: std::array::from_fn(|i| {
                boot_priority_of(raw_copperhf_unit(&raw.copperhf, i).and_then(|d| d.bootpri))
            }),
            copperhf_unit_boot_off: std::array::from_fn(|i| {
                boot_is_off(raw_copperhf_unit(&raw.copperhf, i).and_then(|d| d.bootpri))
            }),
            lide_board: cfg.lide.enabled().then_some(cfg.lide.board),
            // Read from the raw text, not the validated `Config`: an
            // absent `rom` resolves there to the bundled sentinel, which
            // must never reach this field (it would display and, on save,
            // round-trip as the literal sentinel string).
            lide_rom: raw
                .lide
                .rom
                .as_deref()
                .filter(|r| !r.is_empty())
                .map(PathBuf::from),
            lide_rom_disabled: raw.lide.rom.as_deref() == Some(""),
            lide_rom_bank2: raw
                .lide
                .rom_bank2
                .as_deref()
                .filter(|r| !r.is_empty())
                .map(PathBuf::from),
            lide_rom_bank2_disabled: raw.lide.rom_bank2.as_deref() == Some(""),
            lide_drives: std::array::from_fn(|i| {
                cfg.lide.drives[i].as_ref().map(|d| d.path.clone())
            }),
            lide_drive_names: std::array::from_fn(|i| {
                cfg.lide.drives[i]
                    .as_ref()
                    .and_then(|d| d.volume_name.clone())
            }),
            lide_drive_fs: std::array::from_fn(|i| {
                cfg.lide.drives[i]
                    .as_ref()
                    .map(|d| d.filesystem)
                    .unwrap_or(crate::diskimage::FileSystem::FFS)
            }),
            lide_drive_is_dir: std::array::from_fn(|i| {
                cfg.lide.drives[i].as_ref().is_some_and(|d| d.path.is_dir())
            }),
            lide_drive_bootpri: std::array::from_fn(|i| {
                boot_priority_of(lide_raw_slots[i].as_ref().and_then(|d| d.bootpri))
            }),
            lide_drive_boot_off: std::array::from_fn(|i| {
                boot_is_off(lide_raw_slots[i].as_ref().and_then(|d| d.bootpri))
            }),
            filesys_dirs: std::array::from_fn(|i| {
                raw.filesys.get(i).map(|m| PathBuf::from(&m.path))
            }),
            filesys_names: std::array::from_fn(|i| {
                raw.filesys.get(i).and_then(|m| m.volume.clone())
            }),
            filesys_bootpri: std::array::from_fn(|i| {
                raw.filesys.get(i).and_then(|m| m.bootpri).unwrap_or(-128)
            }),
            filesys_readonly: std::array::from_fn(|i| {
                raw.filesys.get(i).and_then(|m| m.readonly).unwrap_or(false)
            }),
            filesys_extra: raw
                .filesys
                .iter()
                .skip(FILESYS_GUI_SLOTS)
                .cloned()
                .collect(),
            cd_image: cfg.cd_image_path.clone(),
            cd_insert_delay: cfg.cd_insert_delay_secs,
            // Use the raw NVRAM path: Config defaults it to "cd32-nvram.bin"
            // on CD32, which we do not want to persist as an explicit setting.
            cd32_nvram: raw.cd.nvram.as_deref().map(PathBuf::from),
            whdload_game: raw.whdload.game.as_deref().map(PathBuf::from),
            whdload_kickstarts: raw.whdload.kickstarts.as_deref().map(PathBuf::from),
            whdload_library: raw.whdload.library.as_deref().map(PathBuf::from),
            whdload_args: raw.whdload.args.clone(),
            whdload_whd_package: raw.whdload.whd_package.as_deref().map(PathBuf::from),
            whdload_skick_package: raw.whdload.skick_package.as_deref().map(PathBuf::from),
            whdload_machine: raw.whdload.machine_type.unwrap_or_default(),
            whdload_library_db: raw.whdload.library_db.as_deref().map(PathBuf::from),
            whdload_library_cache: raw.whdload.library_cache.as_deref().map(PathBuf::from),
            // On unless told otherwise: a fresh installation should find
            // the page there rather than have to be told to show it.
            whdload_enabled: raw.whdload.enabled.unwrap_or(true),
            whdload_games: raw.whdload.games.as_deref().map(PathBuf::from),
            serial_mode: cfg.serial.mode,
            midi_out: cfg.serial.midi_out.clone(),
            midi_in: cfg.serial.midi_in.clone(),
            serial_listen: cfg.serial.listen.clone(),
            serial_connect: cfg.serial.connect.clone(),
            serial_telnet: cfg.serial.telnet.unwrap_or(false),
            parallel_device: cfg.parallel.device,
            parallel_output: cfg.parallel.printer_output.clone(),
            sampler_input: cfg.parallel.sampler_input.clone(),
            sampler_gain_db: cfg.parallel.sampler_gain_db,
            a2065_net: cfg.a2065_net.clone(),
            hostsocket_net: cfg.hostsocket_net.clone(),
            hostsocket_host_mode: cfg.hostsocket_transport.as_deref() == Some("host"),
            hostsocket_dns_server: raw.hostsocket.dns_server.clone(),
            hostsocket_hostname: raw.hostsocket.hostname.clone(),
            hostsocket_address: raw.hostsocket.address.clone(),
            hostsocket_gateway: raw.hostsocket.gateway.clone(),
            hostsocket_resolver: raw.hostsocket.resolver.clone(),
            toccata: cfg.toccata,
            cartridge: cfg.cartridge.model,
            cartridge_rom: raw
                .cartridge
                .rom
                .as_deref()
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
            mhi: cfg.mhi,
            bridge_interfaces: Vec::new(),
            // Filled by refresh_sampler_inputs on open, like the audio devices.
            sampler_input_devices: Vec::new(),
            // Left empty here so config construction stays side-effect free; the
            // config screen fills it via refresh_midi_endpoints on open.
            #[cfg(feature = "midi")]
            midi_endpoints: crate::midi::MidiEndpoints::default(),
            audio_output: crate::audio::AudioOutput::from_config(
                cfg.audio.output_enabled,
                cfg.audio.output_device.as_deref(),
            ),
            // Filled by refresh_audio_devices on open, like the MIDI endpoints.
            audio_devices: Vec::new(),
            audio_channel_mode: cfg.audio.channel_mode,
            audio_stereo_separation: cfg.audio.stereo_separation,
            audio_filter: cfg.audio.filter,
            audio_stem_granularity: cfg.audio.stem_granularity.clone(),
            overscan: cfg.overscan,
            pixel_aspect: cfg.pixel_aspect,
            scaling: cfg.scaling,
            autocrop: cfg.autocrop,
            deinterlace: cfg.deinterlace,
            phosphor: cfg.phosphor,
            shader: cfg.shader.clone(),
            shader_custom: match &cfg.shader {
                ShaderMode::Custom(path) => Some(path.clone()),
                _ => None,
            },
            shader_strength: cfg.shader_strength,
            bezel: cfg.bezel,
            bezel_stickers: cfg.bezel_stickers.clone(),
            perf_overlay: cfg.perf_overlay,
            mt32_control_rom: cfg.serial.mt32_control_rom.clone(),
            mt32_pcm_rom: cfg.serial.mt32_pcm_rom.clone(),
            mt32_panel: cfg.serial.mt32_panel,
            mt32_lcd: cfg.serial.mt32_lcd,
            csynth_soundfont: cfg.serial.coppersynth_soundfont.clone(),
            csynth_mt32_mode: cfg.serial.coppersynth_mt32_mode.clone(),
            csynth_panel: cfg.serial.coppersynth_panel,
            menu_scale: cfg.menu_scale,
            tint: cfg.tint,
            start_fullscreen: cfg.full_screen,
            show_status_bar: cfg.status_bar,
            floppy_sounds: cfg.audio.floppy_sounds,
            floppy_volume: cfg.audio.floppy_sounds_volume,
            power_on: cfg.emulation.power_on,
            auto_launch: cfg.emulation.auto_launch,
            pacing_budget: cfg.emulation.pacing_budget,
            realtime_priority: cfg.emulation.realtime_priority,
            warp: cfg.emulation.warp_speed,
            run_ahead_frames: cfg.emulation.run_ahead_frames,
            warp_boot: cfg.emulation.warp_boot,
            warp_boot_idle: cfg.emulation.warp_boot_idle,
            warp_until: cfg.emulation.warp_until,
            uaelib: cfg.emulation.uaelib,
            uaelib_files: cfg.emulation.uaelib_files,
            joystick_input_mode: cfg.joystick_input_mode,
            mouse_sensitivity: cfg.mouse_sensitivity,
            mouse_capture: cfg.mouse_capture,
            port_devices: cfg.port_devices,
            zorro_boards: raw
                .zorro
                .iter()
                .map(|b| {
                    let mut board = ZorroBoardSetup::load(PathBuf::from(&b.metadata));
                    if let Some(overrides) = &b.config {
                        for (key, value) in overrides {
                            board
                                .overrides
                                .insert(key.clone(), crate::zorro::toml_value_to_string(value));
                        }
                    }
                    board
                })
                .collect(),
            paths: raw.paths.clone(),
        })
    }

    /// Load a configuration file into the typed model, validating it.
    pub fn load_from(path: &Path) -> Result<Self> {
        let setup = Self::from_raw(&crate::config::raw_from_path(path)?)?;
        // The loaded configuration is now the one in hand, so its `[paths]`
        // is where things go from here -- a screenshot taken after loading
        // it should not still be following the one before.
        setup.apply_paths();
        Ok(setup)
    }

    /// The bare-profile config this setup is compared against when emitting
    /// minimal TOML: the machine the selected profile produces with no
    /// overrides, resolved through the same `TryFrom` as a real boot so the
    /// comparison matches exactly (including derived clock/cache defaults).
    pub(super) fn baseline(&self) -> Config {
        let mut raw = RawConfig::default();
        raw.machine.profile = self.model.map(|m| model_name(m).to_string());
        raw.try_into().unwrap_or_else(|_| {
            self.model
                .map_or_else(Config::default, machine_profile_defaults)
        })
    }

    /// Number of bays needed to keep every configured image visible.
    /// Physical bridges are deliberately excluded: reducing the drive count
    /// releases those host interfaces, while an image stays in the launcher
    /// until the user clears it explicitly.
    pub(super) fn image_floppy_bays(&self) -> u8 {
        self.df_playlists
            .iter()
            .rposition(|playlist| !playlist.is_empty())
            .map(|idx| idx as u8 + 1)
            .unwrap_or(0)
    }

    /// Number of bays occupied by either image media or a physical bridge.
    pub(super) fn occupied_floppy_bays(&self) -> u8 {
        self.df_playlists
            .iter()
            .enumerate()
            .rposition(|(idx, playlist)| !playlist.is_empty() || self.df_bridge[idx].is_some())
            .map(|idx| idx as u8 + 1)
            .unwrap_or(0)
    }

    /// Convert back to a raw config, emitting only the fields that differ from
    /// the selected profile's defaults (so saved files stay minimal).
    pub fn to_raw(&self) -> RawConfig {
        let base = self.baseline();
        let mut raw = RawConfig::default();
        if let Some(model) = self.model {
            raw.machine.profile = Some(model_name(model).to_string());
        }
        self.write_machine_config(&mut raw, &base);
        self.write_media_config(&mut raw, &base);
        self.write_whdload_config(&mut raw);
        self.write_presentation_config(&mut raw, &base);
        self.write_io_config(&mut raw, &base);
        self.write_audio_config(&mut raw, &base);
        self.write_zorro_config(&mut raw);
        raw.paths = self.paths.clone();
        raw
    }

    fn write_machine_config(&self, raw: &mut RawConfig, base: &Config) {
        // System
        if self.chipset != base.chipset {
            raw.chipset.revision = Some(chipset_name(self.chipset).to_string());
        }
        if let Some(a) = self.agnus {
            raw.chipset.agnus = Some(agnus_name(a).to_string());
        }
        if let Some(d) = self.denise {
            raw.chipset.denise = Some(denise_name(d).to_string());
        }
        if self.video != base.video_standard {
            raw.chipset.video = Some(video_name(self.video).to_string());
        }
        if self.rtc != base.rtc_present {
            raw.machine.rtc = Some(self.rtc);
        }
        if self.identify != base.identify_board {
            raw.identify = Some(self.identify);
        }
        if self.rtg != base.rtg {
            raw.rtg.card = Some(rtg_card_value(self.rtg).to_string());
        }
        if self.rtg_vram_bytes != base.rtg_vram_bytes {
            raw.rtg.vram = Some(format_size(self.rtg_vram_bytes));
        }
        // CPU
        if self.cpu != base.cpu {
            raw.cpu.model = Some(cpu_name(self.cpu).to_string());
        }
        if self.fpu != base.fpu {
            raw.cpu.fpu = Some(self.fpu);
        }
        if (self.clock_mhz - base.cpu_clock_mhz).abs() > 1e-9 {
            raw.cpu.clock_mhz = Some(self.clock_mhz);
        }
        if self.icache != base.cpu_icache {
            raw.cpu.icache = Some(self.icache);
        }
        if self.dcache != base.cpu_dcache {
            raw.cpu.dcache = Some(self.dcache);
        }
        if self.jit != base.cpu_jit {
            raw.cpu.jit = Some(self.jit);
        }
        // Memory
        if self.chip_ram != base.chip_ram_bytes {
            raw.memory.chip = Some(format_size(self.chip_ram));
        }
        if self.fast_ram != base.fast_ram_bytes {
            raw.memory.fast = Some(format_size(self.fast_ram));
        }
        if self.slow_ram != base.slow_ram_bytes {
            raw.memory.slow = Some(format_size(self.slow_ram));
        }
        if self.ram_init != base.ram_init {
            raw.memory.init = Some(self.ram_init.config_value());
        }
        if self.mb_ram != base.mb_ram_bytes {
            raw.memory.motherboard = Some(format_size(self.mb_ram));
        }
        if self.accel_ram != base.accel_ram_bytes {
            raw.memory.accelerator = Some(format_size(self.accel_ram));
        }
        if self.z3_ram != base.z3_ram_bytes {
            raw.memory.z3 = Some(format_size(self.z3_ram));
        }
        // ROM
        raw.rom = self.rom.as_deref().map(path_string);
        raw.extended_rom = self.extended_rom.as_deref().map(path_string);
        raw.fmv_rom = match self.fmv_rom.as_deref() {
            Some(path) => Some(path_string(path)),
            None if self.fmv_rom_disabled => Some(String::new()),
            None => None,
        };
    }

    fn write_media_config(&self, raw: &mut RawConfig, base: &Config) {
        // Floppy: cover any drive carrying media so the count never orphans it.
        let drives = self.floppy_drives.max(self.occupied_floppy_bays());
        let base_drives = connected_floppy_bays(&base.floppy_connected);
        if drives != base_drives {
            raw.floppy.drives = Some(drives);
        }
        if self.floppy_speed != 100 {
            raw.floppy.speed = Some(self.floppy_speed);
        }
        raw.floppy.df0 = self.floppy_drive_raw(0);
        raw.floppy.df1 = self.floppy_drive_raw(1);
        raw.floppy.df2 = self.floppy_drive_raw(2);
        raw.floppy.df3 = self.floppy_drive_raw(3);
        // Hard disk
        raw.ide.master = drive_raw(
            self.ide_master.as_deref(),
            self.ide_master_name.as_deref(),
            self.effective_bootpri(F::IdeMasterBoot),
            self.ide_master_fs,
        );
        raw.ide.slave = drive_raw(
            self.ide_slave.as_deref(),
            self.ide_slave_name.as_deref(),
            self.effective_bootpri(F::IdeSlaveBoot),
            self.ide_slave_fs,
        );
        // Only emit `[scsi]` when a controller is fitted, so an unset board
        // leaves the section absent rather than writing dangling ROM/units.
        if let Some(controller) = self.scsi_controller {
            // Name every controller: which one a bare [scsi] means depends on
            // the machine (an A3000 defaults to its motherboard SCSI).
            raw.scsi.controller = Some(
                match controller {
                    ScsiController::A2091 => "a2091",
                    ScsiController::A4091 => "a4091",
                    ScsiController::A3000 => "a3000",
                }
                .to_string(),
            );
            // The motherboard SCSI has no boot ROM of its own.
            raw.scsi.rom = controller
                .is_zorro_board()
                .then(|| self.scsi_rom.as_deref().map(path_string))
                .flatten();
            // rom_odd is an A2091 split-EPROM option; the A4091 has one ROM.
            // It is the odd half OF rom, so without rom there is nothing for it
            // to complete and the config would not validate.
            raw.scsi.rom_odd = (controller == ScsiController::A2091 && raw.scsi.rom.is_some())
                .then(|| self.scsi_rom_odd.as_deref().map(path_string))
                .flatten();
            raw.scsi.unit0 = drive_raw(
                self.scsi_units[0].as_deref(),
                self.scsi_unit_names[0].as_deref(),
                self.effective_bootpri(F::ScsiUnit0Boot),
                self.scsi_unit_fs[0],
            );
            raw.scsi.unit1 = drive_raw(
                self.scsi_units[1].as_deref(),
                self.scsi_unit_names[1].as_deref(),
                self.effective_bootpri(F::ScsiUnit1Boot),
                self.scsi_unit_fs[1],
            );
            raw.scsi.unit2 = drive_raw(
                self.scsi_units[2].as_deref(),
                self.scsi_unit_names[2].as_deref(),
                self.effective_bootpri(F::ScsiUnit2Boot),
                self.scsi_unit_fs[2],
            );
            raw.scsi.unit3 = drive_raw(
                self.scsi_units[3].as_deref(),
                self.scsi_unit_names[3].as_deref(),
                self.effective_bootpri(F::ScsiUnit3Boot),
                self.scsi_unit_fs[3],
            );
            raw.scsi.unit4 = drive_raw(
                self.scsi_units[4].as_deref(),
                self.scsi_unit_names[4].as_deref(),
                self.effective_bootpri(F::ScsiUnit4Boot),
                self.scsi_unit_fs[4],
            );
            raw.scsi.unit5 = drive_raw(
                self.scsi_units[5].as_deref(),
                self.scsi_unit_names[5].as_deref(),
                self.effective_bootpri(F::ScsiUnit5Boot),
                self.scsi_unit_fs[5],
            );
            raw.scsi.unit6 = drive_raw(
                self.scsi_units[6].as_deref(),
                self.scsi_unit_names[6].as_deref(),
                self.effective_bootpri(F::ScsiUnit6Boot),
                self.scsi_unit_fs[6],
            );
        }
        // `[copperhf]` has no controller/ROM to gate on -- the board is
        // always there -- so its units are always emitted, unlike `[scsi]`
        // above and `[lide]` below.
        raw.copperhf.unit0 = drive_raw(
            self.copperhf_units[0].as_deref(),
            self.copperhf_unit_names[0].as_deref(),
            self.effective_bootpri(F::CopperhfUnit0Boot),
            self.copperhf_unit_fs[0],
        );
        raw.copperhf.unit1 = drive_raw(
            self.copperhf_units[1].as_deref(),
            self.copperhf_unit_names[1].as_deref(),
            self.effective_bootpri(F::CopperhfUnit1Boot),
            self.copperhf_unit_fs[1],
        );
        raw.copperhf.unit2 = drive_raw(
            self.copperhf_units[2].as_deref(),
            self.copperhf_unit_names[2].as_deref(),
            self.effective_bootpri(F::CopperhfUnit2Boot),
            self.copperhf_unit_fs[2],
        );
        raw.copperhf.unit3 = drive_raw(
            self.copperhf_units[3].as_deref(),
            self.copperhf_unit_names[3].as_deref(),
            self.effective_bootpri(F::CopperhfUnit3Boot),
            self.copperhf_unit_fs[3],
        );
        raw.copperhf.unit4 = drive_raw(
            self.copperhf_units[4].as_deref(),
            self.copperhf_unit_names[4].as_deref(),
            self.effective_bootpri(F::CopperhfUnit4Boot),
            self.copperhf_unit_fs[4],
        );
        raw.copperhf.unit5 = drive_raw(
            self.copperhf_units[5].as_deref(),
            self.copperhf_unit_names[5].as_deref(),
            self.effective_bootpri(F::CopperhfUnit5Boot),
            self.copperhf_unit_fs[5],
        );
        raw.copperhf.unit6 = drive_raw(
            self.copperhf_units[6].as_deref(),
            self.copperhf_unit_names[6].as_deref(),
            self.effective_bootpri(F::CopperhfUnit6Boot),
            self.copperhf_unit_fs[6],
        );
        // Only emit `[lide]` when a board is fitted, matching `[scsi]` above.
        if let Some(board) = self.lide_board {
            raw.lide.board = Some(board.name().to_string());
            raw.lide.rom = match &self.lide_rom {
                Some(p) => Some(path_string(p)),
                // A path takes priority over the disabled flag (typing one
                // is how the field un-disables in the UI, but this also
                // covers stale state defensively); otherwise preserve an
                // explicit opt-out the session never touched.
                None if self.lide_rom_disabled => Some(String::new()),
                None => None,
            };
            // AT-Bus 2008 has no flash banking; a second bank there does not
            // validate, so the row is hidden and nothing is ever emitted for it.
            raw.lide.rom_bank2 = (board != LidePersonality::AtBus2008)
                .then(|| match &self.lide_rom_bank2 {
                    Some(p) => Some(path_string(p)),
                    None if self.lide_rom_bank2_disabled => Some(String::new()),
                    None => None,
                })
                .flatten();
            // One named `driveN` key per slot, so an empty slot before a
            // filled one round-trips: each is emitted independently rather
            // than as a positional array that could not express the hole.
            // The deprecated `drives` array is never written back -- reading
            // one and saving migrates the config to the named form.
            const LIDE_DRIVE_BOOT_FIELDS: [LauncherField; 4] = [
                F::LideDrive0Boot,
                F::LideDrive1Boot,
                F::LideDrive2Boot,
                F::LideDrive3Boot,
            ];
            let slot_raw = |i: usize| {
                (i < board.max_drives())
                    .then(|| {
                        drive_raw(
                            self.lide_drives[i].as_deref(),
                            self.lide_drive_names[i].as_deref(),
                            self.effective_bootpri(LIDE_DRIVE_BOOT_FIELDS[i]),
                            self.lide_drive_fs[i],
                        )
                    })
                    .flatten()
            };
            raw.lide.drives = Vec::new();
            raw.lide.drive0 = slot_raw(0);
            raw.lide.drive1 = slot_raw(1);
            raw.lide.drive2 = slot_raw(2);
            raw.lide.drive3 = slot_raw(3);
        }
        // Host FS mounts: the edited slots (empty ones drop out), then any
        // hand-written extras beyond what the GUI shows.
        raw.filesys = (0..FILESYS_GUI_SLOTS)
            .filter_map(|i| {
                self.filesys_dirs[i].as_ref().map(|p| RawFilesysMount {
                    path: path_string(p),
                    volume: self.filesys_names[i]
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    bootpri: (self.filesys_bootpri[i] != -128).then_some(self.filesys_bootpri[i]),
                    // Emitted only when set, like bootpri: writable is the
                    // default, so an untouched config stays as written.
                    readonly: self.filesys_readonly[i].then_some(true),
                })
            })
            .chain(self.filesys_extra.iter().cloned())
            .collect();
        // Real host disks. Emitted in attachment order so the file reads the
        // way the page does. Access is always explicit; older entries with no
        // read_only field remain protected by the parser's safe default.
        raw.host_disk = crate::config::HostDiskAttach::all()
            .iter()
            .filter_map(|attach| self.host_disk_at(*attach))
            .map(|disk| RawHostDisk {
                device: disk.device.clone(),
                fingerprint: disk.fingerprint.clone(),
                identity_confirmed: false,
                attach: Some(disk.attach.token().to_string()),
                // Always state the access mode. Older hand-written entries with
                // no field stay safely read-only.
                read_only: Some(!disk.writable),
            })
            .collect();
        // CD
        raw.cd.image = self.cd_image.as_deref().map(path_string);
        if self.cd_insert_delay != 0.0 {
            raw.cd.insert_delay = Some(self.cd_insert_delay);
        }
        raw.cd.nvram = self.cd32_nvram.as_deref().map(path_string);
    }

    fn write_whdload_config(&self, raw: &mut RawConfig) {
        // WHDLoad direct boot. `args` has no UI row but still round-trips.
        raw.whdload.game = self.whdload_game.as_deref().map(path_string);
        raw.whdload.kickstarts = self.whdload_kickstarts.as_deref().map(path_string);
        raw.whdload.library = self.whdload_library.as_deref().map(path_string);
        raw.whdload.args = self.whdload_args.clone();
        raw.whdload.whd_package = self.whdload_whd_package.as_deref().map(path_string);
        raw.whdload.skick_package = self.whdload_skick_package.as_deref().map(path_string);
        // Only written when it is not the default, so a saved file stays
        // the short list of what differs from the profile.
        raw.whdload.machine_type = (self.whdload_machine
            != crate::config::WhdloadMachine::default())
        .then_some(self.whdload_machine);
        raw.whdload.library_db = self.whdload_library_db.as_deref().map(path_string);
        raw.whdload.library_cache = self.whdload_library_cache.as_deref().map(path_string);
        // Only written when it is off, since on is the default and a
        // configuration file should say what differs from it.
        raw.whdload.enabled = (!self.whdload_enabled).then_some(false);
        raw.whdload.games = self.whdload_games.as_deref().map(path_string);
    }

    fn write_presentation_config(&self, raw: &mut RawConfig, base: &Config) {
        // A/V and emulation
        if self.overscan != base.overscan {
            raw.display.overscan = Some(overscan_name(self.overscan).to_string());
        }
        if self.pixel_aspect != base.pixel_aspect {
            raw.display.pixel_aspect = Some(pixel_aspect_name(self.pixel_aspect).to_string());
        }
        if self.scaling != base.scaling {
            raw.display.scaling = Some(display_scaling_name(self.scaling).to_string());
        }
        if self.autocrop != base.autocrop {
            raw.display.autocrop = Some(self.autocrop);
        }
        if self.deinterlace != base.deinterlace {
            raw.display.deinterlace = Some(self.deinterlace);
        }
        if (self.phosphor - base.phosphor).abs() > 1e-6 {
            raw.display.phosphor = Some(self.phosphor);
        }
        if self.shader != base.shader {
            raw.display.shader = Some(shader_name(&self.shader));
        }
        if (self.shader_strength - base.shader_strength).abs() > 1e-6 {
            raw.display.shader_strength = Some(self.shader_strength);
        }
        if self.bezel != base.bezel {
            raw.display.bezel = Some(crate::config::RawBezel::Named(self.bezel.label().into()));
        }
        if self.bezel_stickers != base.bezel_stickers {
            raw.display.bezel_stickers = self.bezel_stickers.as_deref().map(path_string);
        }
        if self.perf_overlay != base.perf_overlay {
            raw.display.perf_overlay = Some(self.perf_overlay);
        }
        if self.tint != base.tint {
            raw.display.tint = Some(tint_name(self.tint).to_string());
        }
        if self.mt32_control_rom != base.serial.mt32_control_rom {
            raw.serial.mt32_control_rom = self
                .mt32_control_rom
                .as_ref()
                .map(|p| p.display().to_string());
        }
        if self.mt32_pcm_rom != base.serial.mt32_pcm_rom {
            raw.serial.mt32_pcm_rom = self.mt32_pcm_rom.as_ref().map(|p| p.display().to_string());
        }
        if self.mt32_panel != base.serial.mt32_panel {
            raw.serial.mt32_panel = Some(self.mt32_panel);
        }
        if self.mt32_lcd != base.serial.mt32_lcd {
            raw.serial.mt32_lcd = Some(self.mt32_lcd.label().to_string());
        }
        if self.csynth_soundfont != base.serial.coppersynth_soundfont {
            raw.serial.coppersynth_soundfont = self
                .csynth_soundfont
                .as_ref()
                .map(|p| p.display().to_string());
        }
        if self.csynth_mt32_mode != base.serial.coppersynth_mt32_mode {
            raw.serial.coppersynth_mt32_mode = self.csynth_mt32_mode.clone();
        }
        if self.csynth_panel != base.serial.coppersynth_panel {
            raw.serial.coppersynth_panel = Some(self.csynth_panel);
        }
        if self.menu_scale != base.menu_scale {
            raw.display.menu_scale = Some(self.menu_scale.label().to_string());
        }
        if self.start_fullscreen != base.full_screen {
            raw.display.full_screen = Some(self.start_fullscreen);
        }
        if self.show_status_bar != base.status_bar {
            raw.display.status_bar = Some(self.show_status_bar);
        }
        if self.floppy_sounds != base.audio.floppy_sounds {
            raw.audio.floppy_sounds = Some(self.floppy_sounds);
        }
        if self.floppy_volume != base.audio.floppy_sounds_volume {
            raw.audio.floppy_sounds_volume = Some(u16::from(self.floppy_volume));
        }
        if self.power_on != base.emulation.power_on {
            raw.emulation.power_on = Some(self.power_on);
        }
        if self.auto_launch != base.emulation.auto_launch {
            raw.emulation.auto_launch = Some(self.auto_launch);
        }
        if self.pacing_budget != base.emulation.pacing_budget {
            raw.emulation.pacing_budget = Some(pacing_name(self.pacing_budget).to_string());
        }
        if self.realtime_priority != base.emulation.realtime_priority {
            raw.emulation.realtime_priority = Some(self.realtime_priority);
        }
        if self.warp != base.emulation.warp_speed {
            raw.emulation.warp_speed = Some(self.warp.label().to_ascii_lowercase());
        }
        if self.run_ahead_frames != base.emulation.run_ahead_frames {
            raw.emulation.run_ahead_frames = Some(self.run_ahead_frames);
        }
        if self.warp_boot != base.emulation.warp_boot {
            raw.emulation.warp_boot = Some(self.warp_boot);
        }
        if self.warp_boot_idle != base.emulation.warp_boot_idle {
            raw.emulation.warp_boot_idle = Some(self.warp_boot_idle);
        }
        if self.warp_until != base.emulation.warp_until {
            raw.emulation.warp_until = self.warp_until;
        }
        if self.uaelib != base.emulation.uaelib {
            raw.emulation.uaelib = Some(self.uaelib);
        }
        if self.uaelib_files != base.emulation.uaelib_files {
            raw.emulation.uaelib_files = Some(self.uaelib_files);
        }
    }

    fn write_io_config(&self, raw: &mut RawConfig, base: &Config) {
        if self.joystick_input_mode != base.joystick_input_mode {
            raw.input.joystick = Some(self.joystick_input_mode.label().to_string());
        }
        if self.mouse_sensitivity != base.mouse_sensitivity {
            raw.input.mouse_sensitivity = Some(u16::from(self.mouse_sensitivity));
        }
        if self.mouse_capture != base.mouse_capture {
            raw.input.mouse_capture = Some(self.mouse_capture.label().to_string());
        }
        // Per port against the profile baseline, so a CD32 keeps its pad
        // implicit and a stock machine emits no port keys at all.
        if self.port_devices[0] != base.port_devices[0] {
            raw.input.port1 = Some(self.port_devices[0].label().to_string());
        }
        if self.port_devices[1] != base.port_devices[1] {
            raw.input.port2 = Some(self.port_devices[1].label().to_string());
        }
        if self.serial_mode != base.serial.mode {
            raw.serial.mode = Some(self.serial_mode.label().to_string());
        }
        raw.serial.midi_out = self.midi_out.clone();
        raw.serial.midi_in = self.midi_in.clone();
        // A half-typed address leaves here whole: the config file and the
        // run get the defaults filled in, while the session keeps only what
        // was typed so emptying a box reverts it.
        raw.serial.listen = self.serial_listen.as_deref().map(complete_listen);
        raw.serial.connect = self.serial_connect.as_deref().map(complete_connect);
        // Compared against the resolved value, not the raw tri-state: the
        // toggle is a plain on/off, so "unset" and "explicitly off" look
        // the same to it and must not produce a spurious `telnet = false`
        // write -- which would then fail validation on any non-modem mode.
        // Flipping it away from a base that did say something still writes
        // an explicit value, which is how an explicit off gets recorded.
        if self.serial_telnet != base.serial.telnet.unwrap_or(false) {
            raw.serial.telnet = Some(self.serial_telnet);
        }
        // Parallel port. Carry each peripheral's settings whenever they are set
        // so a Save round-trips them even while another device is temporarily
        // selected. The sampler options do not imply the sampler, so they are
        // always safe to emit; a bare `output` path implies the printer, so an
        // explicit `device` disambiguates when it is carried under None.
        raw.parallel.output = self.parallel_output.as_deref().map(path_string);
        raw.parallel.sampler_input = self.sampler_input.clone();
        raw.parallel.sampler_gain = (self.sampler_gain_db != 0.0).then_some(self.sampler_gain_db);
        raw.parallel.device = match self.parallel_device {
            // None is the resolved default (omitted to keep the TOML minimal),
            // but emit it explicitly to override a carried-over `output` path
            // that would otherwise be read back as the printer.
            ParallelDevice::None => self
                .parallel_output
                .is_some()
                .then(|| ParallelDevice::None.label().to_string()),
            // A printer needs a capture file. Without one it is an incomplete
            // selection, so persist nothing (a bare `output` would already imply
            // the printer, so no explicit device is needed when it is set).
            ParallelDevice::Printer => self
                .parallel_output
                .is_some()
                .then(|| ParallelDevice::Printer.label().to_string()),
            ParallelDevice::Sampler => Some(ParallelDevice::Sampler.label().to_string()),
        };
        // Ethernet: no profile fits an A2065 by default, so the board is
        // emitted whenever it is on (absent key = not fitted).
        raw.a2065.net = self
            .a2065_net
            .as_ref()
            .map(|n| crate::net::net_config_name(n).to_string());
        raw.a2065.interface = match self.a2065_net.as_ref() {
            Some(NetConfig::Bridge { interface }) => Some(interface.clone()),
            _ => None,
        };
        // HostSocket: same shape as the A2065 (absent key = not fitted),
        // plus the pass-through keys this screen does not edit. `net =
        // "host"` is a separate transport, not a `NetConfig` backend (see
        // `hostsocket_host_mode`'s own comment) -- when set it overrides
        // the backend name entirely and clears `interface`/`address`/
        // `gateway`, none of which apply to it (`Config::from_raw` rejects
        // any of the three alongside `net = "host"`).
        if self.hostsocket_host_mode {
            raw.hostsocket.net = Some("host".to_string());
            raw.hostsocket.interface = None;
            raw.hostsocket.address = None;
            raw.hostsocket.gateway = None;
        } else {
            raw.hostsocket.net = self
                .hostsocket_net
                .as_ref()
                .map(|n| crate::net::net_config_name(n).to_string());
            raw.hostsocket.interface = match self.hostsocket_net.as_ref() {
                Some(NetConfig::Bridge { interface }) => Some(interface.clone()),
                _ => None,
            };
            raw.hostsocket.address = self.hostsocket_address.clone();
            raw.hostsocket.gateway = self.hostsocket_gateway.clone();
        }
        raw.hostsocket.dns_server = self.hostsocket_dns_server.clone();
        raw.hostsocket.hostname = self.hostsocket_hostname.clone();
        raw.hostsocket.resolver = self.hostsocket_resolver.clone();
    }

    fn write_audio_config(&self, raw: &mut RawConfig, base: &Config) {
        // Sound: both boards are absent by default, so only "on" is emitted.
        if self.toccata != base.toccata {
            raw.toccata.enabled = Some(self.toccata);
        }
        if self.cartridge != base.cartridge.model {
            raw.cartridge.model = Some(cartridge_model_name(self.cartridge).to_string());
        }
        if self.cartridge.is_some() {
            if let Some(rom) = &self.cartridge_rom {
                raw.cartridge.rom = Some(rom.display().to_string());
            }
        }
        if self.mhi != base.mhi {
            raw.mhi.enabled = Some(self.mhi);
        }
        // The Audio output picker is one of default / a named device / Disabled.
        // A named device sets output_device; Disabled sets output_enabled=false
        // (the resolved default is true, so it is omitted otherwise).
        raw.audio.output_device = self.audio_output.device().map(str::to_string);
        raw.audio.output_enabled = (!self.audio_output.is_enabled()).then_some(false);
        // Emit only the non-default mode; Stereo is the resolved default, so
        // omitting it keeps a default machine's TOML minimal.
        raw.audio.channel_mode = (self.audio_channel_mode != ChannelMode::Stereo)
            .then(|| self.audio_channel_mode.label().to_string());
        raw.audio.stereo_separation = (self.audio_stereo_separation != 100)
            .then_some(u16::from(self.audio_stereo_separation));
        raw.audio.audio_filter = (self.audio_filter != AudioFilterMode::Auto)
            .then(|| self.audio_filter.label().to_string());
        raw.audio.stem_granularity = self.audio_stem_granularity.as_ref().map(|list| {
            list.iter()
                .map(|g| g.as_str())
                .collect::<Vec<_>>()
                .join(",")
        });
    }

    fn write_zorro_config(&self, raw: &mut RawConfig) {
        // Zorro boards: emit the metadata path plus any per-board overrides
        // (typed per the option schema), only when the user changed something.
        raw.zorro = self
            .zorro_boards
            .iter()
            .map(|b| {
                let mut table = toml::Table::new();
                for o in &b.options {
                    if let Some(v) = b.override_toml(o) {
                        table.insert(o.key.clone(), v);
                    }
                }
                RawZorroBoard {
                    metadata: path_string(&b.metadata),
                    config: (!table.is_empty()).then_some(table),
                }
            })
            .collect();
    }

    pub(super) fn floppy_drive_raw(&self, idx: usize) -> Option<RawFloppyDrive> {
        // A bay using a real drive writes its interface and settings instead
        // of an image; only the settings that differ from the cautious
        // defaults are emitted, so a saved config stays readable.
        if let Some(bridge) = self.df_bridge[idx]
            .as_ref()
            .filter(|_| !self.df_bridge_none[idx])
        {
            let default = FluxBridgeConfig::default();
            return Some(RawFloppyDrive {
                bridge: Some(bridge_driver_name(bridge.driver).to_string()),
                bridge_port: bridge.port.clone(),
                bridge_cable: (bridge.cable != default.cable)
                    .then(|| bridge_cable_name(bridge.cable).to_string()),
                bridge_density: (bridge.density != default.density)
                    .then(|| bridge_density_name(bridge.density).to_string()),
                bridge_mode: (bridge.mode != default.mode)
                    .then(|| bridge_mode_name(bridge.mode).to_string()),
                bridge_speed: (bridge.speed != crate::config::DEFAULT_BRIDGE_SPEED_PERCENT).then(
                    || {
                        crate::config::RawReplaySpeed::Word(
                            if bridge.speed == 200 {
                                "fast"
                            } else {
                                "normal"
                            }
                            .into(),
                        )
                    },
                ),
                // Same rule, and the same tick box, as an image: only an
                // unprotected drive says so.
                write_protected: (!self.df_write_protected[idx]).then_some(false),
                ..RawFloppyDrive::default()
            });
        }
        let playlist = &self.df_playlists[idx];
        if playlist.is_empty() {
            // A write-protect flag on an empty drive is meaningless, so an
            // untouched/empty drive emits no [floppy.dfN] table at all.
            return None;
        }
        let (first, rest) = playlist.split_first().expect("non-empty checked above");
        Some(RawFloppyDrive {
            enabled: None,
            path: Some(path_string(first)),
            paths: (!rest.is_empty()).then(|| rest.iter().map(|p| path_string(p)).collect()),
            // write_protected defaults to true; only an unprotected drive is
            // written explicitly.
            write_protected: (!self.df_write_protected[idx]).then_some(false),
            // Bridges are emitted by the FluxBridge page, not the image rows.
            ..RawFloppyDrive::default()
        })
    }

    /// Serialize the configured machine to TOML for the Save action.
    pub fn to_toml(&self) -> Result<String> {
        self.to_raw().to_toml_string()
    }

    /// Validate the configured machine, producing the [`Config`] the Run action
    /// builds from (its boot ROM may still be the AROS sentinel; the caller
    /// resolves that).
    pub fn build_config(&self) -> Result<Config> {
        self.to_raw().try_into()
    }
}
