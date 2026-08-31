//! Config unit tests.
use super::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn parse_config(text: &str) -> Result<Config> {
    let raw: RawConfig = toml::from_str(text)?;
    raw.try_into()
}

/// The player startup layers its settings file over the manifest's
/// defaults: what the overlay carries wins, what it omits keeps the
/// base, so a partial or hand-edited file still behaves.
#[test]
fn player_settings_overlay_field_by_field() -> Result<()> {
    let mut base = RawConfig::default();
    base.set_machine_profile("CD32");
    base.set_display_defaults(Some("crt"), Some("1084"), Some(true));

    let overlay = RawConfig::parse(
        "[display]\nshader = \"scanlines\"\nfull_screen = false\n\
             [input]\nport2 = \"cd32\"\n",
    )?;
    base.merge_player_settings(&overlay);

    let cfg: Config = base.try_into()?;
    assert_eq!(cfg.shader, ShaderMode::Scanlines, "overlay wins");
    assert!(!cfg.full_screen, "overlay wins");
    assert_eq!(cfg.bezel, BezelStyle::Model1084, "omitted keeps the base");
    assert_eq!(cfg.port_devices[1], crate::bus::PortDevice::Cd32Pad);
    assert_eq!(cfg.machine, Some(MachineModel::Cd32));
    Ok(())
}

/// One medium cannot be two drives. Two slots pointed at the same disk
/// would have the guest mount and write it through both buses at once,
/// each unaware of the other -- so the configuration is refused rather
/// than opened twice.
#[test]
fn one_disk_cannot_be_attached_twice() {
    let ide = IdeConfig::default();
    let scsi = ScsiConfig::default();
    let twice = [
        RawHostDisk {
            device: "sdb".to_string(),
            fingerprint: None,
            identity_confirmed: false,
            attach: Some("ide-master".to_string()),
            read_only: None,
        },
        RawHostDisk {
            device: "sdb".to_string(),
            fingerprint: None,
            identity_confirmed: false,
            attach: Some("ide-slave".to_string()),
            read_only: None,
        },
    ];
    let error = parse_host_disks(&twice, &ide, &scsi, &LideConfig::default(), true, false)
        .expect_err("the same disk on two slots is refused")
        .to_string();
    assert!(error.contains("already attached"), "{error}");

    let same_identity = [
        RawHostDisk {
            device: "old-ordinal".to_string(),
            fingerprint: Some("v1-same-hardware".to_string()),
            identity_confirmed: false,
            attach: Some("ide-master".to_string()),
            read_only: Some(true),
        },
        RawHostDisk {
            device: "new-ordinal".to_string(),
            fingerprint: Some("v1-same-hardware".to_string()),
            identity_confirmed: false,
            attach: Some("ide-slave".to_string()),
            read_only: Some(true),
        },
    ];
    let error = parse_host_disks(
        &same_identity,
        &ide,
        &scsi,
        &LideConfig::default(),
        true,
        false,
    )
    .expect_err("one fingerprint cannot become two guest drives")
    .to_string();
    assert!(error.contains("one disk cannot be two drives"), "{error}");

    // Two different disks on two slots is the ordinary case.
    let separately = [
        RawHostDisk {
            device: "sdb".to_string(),
            fingerprint: None,
            identity_confirmed: false,
            attach: Some("ide-master".to_string()),
            read_only: None,
        },
        RawHostDisk {
            device: "sdc".to_string(),
            fingerprint: None,
            identity_confirmed: false,
            attach: Some("ide-slave".to_string()),
            read_only: None,
        },
    ];
    let parsed = parse_host_disks(
        &separately,
        &ide,
        &scsi,
        &LideConfig::default(),
        true,
        false,
    )
    .expect("two disks, two slots");
    assert_eq!(parsed.len(), 2);
    assert!(
        parsed.iter().all(|disk| !disk.writable),
        "an omitted physical-disk access mode is safely read-only"
    );
}

fn host_disk_entry(device: &str, attach: &str) -> RawHostDisk {
    RawHostDisk {
        device: device.to_string(),
        fingerprint: None,
        identity_confirmed: false,
        attach: Some(attach.to_string()),
        read_only: None,
    }
}

/// A `[lide]` attachment point requires an actual `[lide]` board, and
/// then only a channel that board's personality has.
#[test]
fn lide_host_disk_attach_requires_a_fitted_channel() {
    let ide = IdeConfig::default();
    let scsi = ScsiConfig::default();

    // No [lide] board at all: neither channel is fitted.
    let disks = [host_disk_entry("sdb", "lide0-master")];
    let error = parse_host_disks(&disks, &ide, &scsi, &LideConfig::default(), true, false)
        .expect_err("no [lide] board means no lide attachment point")
        .to_string();
    assert!(
        error.contains("Attach to Lide requires a [lide] board"),
        "{error}"
    );

    // RIDE: one channel. Channel 0 is fitted, channel 1 is not.
    let ride = LideConfig {
        board: crate::ide_zorro::LidePersonality::Ride,
        board_named: true,
        rom: Some(PathBuf::from("lide.rom")),
        rom_bank2: None,
        drives: [None, None, None, None],
    };
    let ok = parse_host_disks(
        &[host_disk_entry("sdb", "lide0-master")],
        &ide,
        &scsi,
        &ride,
        true,
        false,
    )
    .expect("channel 0 is fitted on RIDE");
    assert_eq!(ok.len(), 1);
    let error = parse_host_disks(
        &[host_disk_entry("sdb", "lide1-master")],
        &ide,
        &scsi,
        &ride,
        true,
        false,
    )
    .expect_err("RIDE has only one channel")
    .to_string();
    assert!(
        error.contains("Attach to Lide requires a [lide] board"),
        "{error}"
    );

    // RIPPLE: two channels, both fitted.
    let ripple = LideConfig {
        board: crate::ide_zorro::LidePersonality::Ripple,
        board_named: true,
        rom: Some(PathBuf::from("lide.rom")),
        rom_bank2: None,
        drives: [None, None, None, None],
    };
    let parsed = parse_host_disks(
        &[
            host_disk_entry("sdb", "lide0-master"),
            host_disk_entry("sdc", "lide1-slave"),
        ],
        &ide,
        &scsi,
        &ripple,
        true,
        false,
    )
    .expect("both channels are fitted on RIPPLE");
    assert_eq!(parsed.len(), 2);
}

/// A `[lide] board = "ripple"` section with no rom and no drive images
/// (legal hardware-only mode) still counts as a fitted board for a host
/// disk. `LideConfig::enabled()` tracks `board_named` for exactly this --
/// without it, a bare board would wrongly read as `enabled() == false`,
/// silently skipping the `rom_bank2` validation below as well as
/// rejecting the host disk. This is the exact repro from the audit.
#[test]
fn lide_host_disk_attach_accepts_a_bare_board_with_no_images() {
    let ide = IdeConfig::default();
    let scsi = ScsiConfig::default();
    let bare = LideConfig {
        board: crate::ide_zorro::LidePersonality::Ripple,
        board_named: true,
        rom: None,
        rom_bank2: None,
        drives: [None, None, None, None],
    };
    assert!(
        bare.enabled(),
        "a named board with no rom/drives is still `enabled()`"
    );
    let parsed = parse_host_disks(
        &[host_disk_entry("sdb", "lide0-master")],
        &ide,
        &scsi,
        &bare,
        true,
        false,
    )
    .expect("a named board with no images still fits a host disk");
    assert_eq!(parsed.len(), 1);
}

/// End-to-end repro through `Config::from_raw`: `[lide] board = "ripple"`
/// with no rom/drives, plus a `[[host_disk]] attach = "lide0-master"`
/// entry, must parse rather than reject with "requires a [lide] board".
#[test]
fn lide_bare_board_config_accepts_a_host_disk() {
    let toml = r#"
            [lide]
            board = "ripple"

            [[host_disk]]
            device = "sdb"
            attach = "lide0-master"
        "#;
    let cfg = parse_config(toml)
        .expect("a bare [lide] board with a host disk on its master channel should be accepted");
    assert_eq!(cfg.host_disks.len(), 1);
}

/// A lide slot already holding an image cannot also take a real disk --
/// one slot holds one drive.
#[test]
fn lide_host_disk_attach_rejects_a_slot_already_holding_an_image() {
    let ide = IdeConfig::default();
    let scsi = ScsiConfig::default();
    let ripple = LideConfig {
        board: crate::ide_zorro::LidePersonality::Ripple,
        board_named: true,
        rom: Some(PathBuf::from("lide.rom")),
        rom_bank2: None,
        drives: [
            Some(DriveImage {
                path: PathBuf::from("ch0-master.hdf"),
                volume_name: None,
                boot_pri: 0,
                filesystem: crate::diskimage::FileSystem::FFS,
            }),
            None,
            None,
            None,
        ],
    };
    let error = parse_host_disks(
        &[host_disk_entry("sdb", "lide0-master")],
        &ide,
        &scsi,
        &ripple,
        true,
        false,
    )
    .expect_err("channel 0 master already has an image")
    .to_string();
    assert!(error.contains("already has an image"), "{error}");
}

/// Tokens for the new lide attachment points round-trip, and label
/// exactly as written.
#[test]
fn lide_host_disk_attach_tokens_round_trip() {
    assert_eq!(
        HostDiskAttach::from_token("lide1-slave"),
        Some(HostDiskAttach::LideSlave(1))
    );
    assert_eq!(
        HostDiskAttach::from_token("lide0-master"),
        Some(HostDiskAttach::LideMaster(0))
    );
    assert_eq!(HostDiskAttach::LideMaster(0).label(), "Lide 0 Master");
    assert_eq!(HostDiskAttach::LideSlave(1).label(), "Lide 1 Slave");
}

/// Where several disks went, said once. A single point reads as its own
/// label; several units on the one controller collapse rather than
/// repeating the controller for each.
#[test]
fn attachment_points_are_named_as_one_phrase() {
    use HostDiskAttach as A;
    assert_eq!(A::describe_all(&[A::Scsi(3)]), "SCSI Unit 3");
    assert_eq!(A::describe_all(&[A::IdeMaster]), "IDE Master");
    assert_eq!(
        A::describe_all(&[A::Scsi(0), A::Scsi(1), A::Scsi(4)]),
        "SCSI Unit 0,1,4"
    );
    assert_eq!(
        A::describe_all(&[A::IdeMaster, A::IdeSlave]),
        "IDE Master, IDE Slave"
    );
    // Mixed: the SCSI group sits where its first disk came.
    assert_eq!(
        A::describe_all(&[A::Scsi(2), A::IdeMaster, A::Scsi(5)]),
        "SCSI Unit 2,5, IDE Master"
    );
    assert_eq!(A::describe_all(&[]), "");
}

/// Build a config from CLI overrides only (no file), exercising the same
/// raw-load + validation path `main` uses.
fn load_overrides(overrides: &ConfigOverrides) -> Result<Config> {
    Config::load_raw(None, overrides)?.try_into()
}

#[test]
fn rtc_time_accepts_both_notations_and_implies_a_fitted_clock() -> Result<()> {
    // Integer form: Unix seconds (an RFC 6238 test-vector instant).
    let cfg = parse_config(
        r#"
            [machine]
            rtc_time = 1111111109
            "#,
    )?;
    assert_eq!(cfg.rtc_seed_unix, Some(1_111_111_109));
    assert!(!cfg.rtc_frozen);
    // The default A500 has no clock, but seeding one fits it.
    assert!(cfg.rtc_present);

    // Calendar form: the same instant as the guest reads it.
    let cfg = parse_config(
        r#"
            [machine]
            rtc_time = "2005-03-18 01:58:29"
            rtc_frozen = true
            "#,
    )?;
    assert_eq!(cfg.rtc_seed_unix, Some(1_111_111_109));
    assert!(cfg.rtc_frozen);
    assert!(cfg.rtc_present);
    Ok(())
}

#[test]
fn rtc_time_misconfigurations_are_rejected() {
    // An explicitly unfitted clock contradicts a configured time.
    let err = parse_config(
        r#"
            [machine]
            rtc = false
            rtc_time = 1111111109
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("rtc = false"), "{err:#}");

    // Freezing needs a time to freeze at.
    let err = parse_config(
        r#"
            [machine]
            rtc_frozen = true
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("rtc_frozen"), "{err:#}");

    // Negative Unix seconds and malformed strings fail validation.
    for bad in ["rtc_time = -1", "rtc_time = \"tomorrow\""] {
        let err = parse_config(&format!("[machine]\n{bad}\n")).unwrap_err();
        assert!(err.to_string().contains("rtc_time"), "{bad}: {err:#}");
    }
}

#[test]
fn rtc_chip_defaults_per_profile_and_implies_a_fitted_clock() -> Result<()> {
    use crate::rtc::RtcChip;

    // The big boxes carry the Ricoh part by default.
    for profile in ["A3000", "A4000"] {
        let cfg = parse_config(&format!("[machine]\nprofile = \"{profile}\"\n"))?;
        assert!(cfg.rtc_present, "{profile}");
        assert_eq!(cfg.rtc_chip, RtcChip::Rp5c01, "{profile}");
    }
    // The clock-equipped small boxes keep the OKI one.
    let cfg = parse_config("[machine]\nprofile = \"A500+\"\n")?;
    assert!(cfg.rtc_present);
    assert_eq!(cfg.rtc_chip, RtcChip::Msm6242);

    // Naming a chip fits the clock on a machine that has none...
    let cfg = parse_config("[machine]\nrtc_chip = \"RP5C01\"\n")?;
    assert!(cfg.rtc_present);
    assert_eq!(cfg.rtc_chip, RtcChip::Rp5c01);
    // ...and the aliases and the big-box override both parse.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A3000"
            rtc_chip = "MSM6242B"
            "#,
    )?;
    assert_eq!(cfg.rtc_chip, RtcChip::Msm6242);
    let cfg = parse_config("[machine]\nrtc_chip = \"rf5c01a\"\n")?;
    assert_eq!(cfg.rtc_chip, RtcChip::Rp5c01);
    Ok(())
}

#[test]
fn rtc_chip_misconfigurations_are_rejected() {
    // A chip named for a socket declared empty is a contradiction.
    let err = parse_config(
        r#"
            [machine]
            rtc = false
            rtc_chip = "RP5C01"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("rtc = false"), "{err:#}");

    let err = parse_config("[machine]\nrtc_chip = \"DS1307\"\n").unwrap_err();
    assert!(err.to_string().contains("rtc_chip"), "{err:#}");
}

/// `Config::try_from` resolves the implicit battery-RAM files through
/// the paths in force, so a startup that adopted `[paths]` *after* the
/// conversion would site this run's NVRAM by the previous answer. The
/// conversion is exercised here with a section already in force, which
/// is the order `load_config` uses.
#[test]
fn the_battery_ram_follows_the_paths_in_force() -> Result<()> {
    let _guard = crate::paths::adopted_store_lock();
    crate::paths::adopt(crate::pathconf::Paths {
        nvram: Some(PathBuf::from("elsewhere")),
        ..Default::default()
    });
    let cfg = parse_config("[machine]\nprofile = \"A4000\"\n")?;
    let battmem = cfg.battmem_path.expect("an A4000 fits an RP5C01");
    // No host-data directory at all leaves the bare name, which is the
    // documented degradation and has no directory to check.
    if crate::paths::config_dir().is_some() {
        assert!(
            battmem.parent().is_some_and(|p| p.ends_with("elsewhere")),
            "the section in force did not site the battery RAM: {battmem:?}"
        );
    }
    Ok(())
}

#[test]
fn battmem_defaults_to_a_backing_file_only_where_an_rp5c01_sits() -> Result<()> {
    // The big boxes get the default backing file with their Ricoh part.
    for profile in ["A3000", "A4000"] {
        let cfg = parse_config(&format!("[machine]\nprofile = \"{profile}\"\n"))?;
        // The file's name, not its directory: where it sits is
        // `paths`' business and moves with the host-data directory.
        assert_eq!(
            cfg.battmem_path
                .as_deref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some("battmem.nvram"),
            "{profile}"
        );
    }
    // The OKI part has no battery RAM: no file, even with a clock.
    let cfg = parse_config("[machine]\nprofile = \"A500+\"\n")?;
    assert_eq!(cfg.battmem_path, None);
    // Fitting an RP5C01 anywhere brings the default with it.
    let cfg = parse_config("[machine]\nrtc_chip = \"RP5C01\"\n")?;
    assert!(cfg.battmem_path.is_some());

    // An explicit path wins; an empty one keeps RAM session-only.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A4000"
            battmem = "shared/A4000.nvram"
            "#,
    )?;
    assert_eq!(
        cfg.battmem_path.as_deref(),
        Some(std::path::Path::new("shared/A4000.nvram"))
    );
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A4000"
            battmem = ""
            "#,
    )?;
    assert_eq!(cfg.battmem_path, None);
    Ok(())
}

#[test]
fn battmem_without_an_rp5c01_is_rejected() {
    // The default A500 has no RP5C01 for the file to back.
    let err = parse_config("[machine]\nbattmem = \"battmem.nvram\"\n").unwrap_err();
    assert!(err.to_string().contains("RP5C01"), "{err:#}");

    // An MSM6242 clock is not enough: it carries no battery RAM.
    let err = parse_config(
        r#"
            [machine]
            rtc = true
            battmem = "battmem.nvram"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("RP5C01"), "{err:#}");
}

#[test]
fn rtc_time_cli_override_matches_the_config_field() -> Result<()> {
    let cfg = load_overrides(&ConfigOverrides {
        rtc_time: Some("1111111109".to_string()),
        rtc_frozen: Some(true),
        ..ConfigOverrides::default()
    })?;
    assert_eq!(cfg.rtc_seed_unix, Some(1_111_111_109));
    assert!(cfg.rtc_frozen);
    assert!(cfg.rtc_present);
    Ok(())
}

#[test]
fn format_size_inverts_parse_size() {
    for (bytes, text) in [
        (0usize, "0"),
        (512 * 1024, "512K"),
        (1024 * 1024, "1M"),
        (2 * 1024 * 1024, "2M"),
        (16 * 1024 * 1024, "16M"),
        (1024 * 1024 * 1024, "1G"),
    ] {
        assert_eq!(format_size(bytes), text, "format_size({bytes})");
        assert_eq!(
            parse_size(&format_size(bytes), "test").unwrap(),
            bytes,
            "round-trip {bytes}"
        );
    }
}

#[test]
fn ram_init_parses_zero_random_fixed_patterns_and_explicit_seeds() -> Result<()> {
    assert_eq!(parse_config("")?.ram_init, RamInit::Zero);
    assert_eq!(
        parse_config("[memory]\ninit = \"random\"\n")?.ram_init,
        RamInit::Random {
            seed: DEFAULT_RANDOM_RAM_SEED,
        }
    );
    assert_eq!(
        parse_config("[memory]\ninit = \"random:0x12_34_AB_CD\"\n")?.ram_init,
        RamInit::Random { seed: 0x1234_ABCD }
    );
    assert_eq!(
        parse_config("[memory]\ninit = \"random:12345\"\n")?.ram_init,
        RamInit::Random { seed: 12345 }
    );
    assert_eq!(
        parse_config("[memory]\ninit = \"pattern:0x5555\"\n")?.ram_init,
        RamInit::Pattern { word: 0x5555 }
    );
    assert_eq!(
        parse_config("[memory]\ninit = \"0xDEAD\"\n")?.ram_init,
        RamInit::Pattern { word: 0xDEAD }
    );
    assert_eq!(
        load_overrides(&ConfigOverrides {
            ram_init: Some("pattern:0xBEEF".to_string()),
            ..ConfigOverrides::default()
        })?
        .ram_init,
        RamInit::Pattern { word: 0xBEEF }
    );
    Ok(())
}

#[test]
fn ram_init_rejects_unknown_modes_and_bad_seeds() {
    for value in [
        "garbage",
        "random:nope",
        "random:",
        "pattern:nope",
        "pattern:0x10000",
    ] {
        let err = parse_config(&format!("[memory]\ninit = {value:?}\n")).unwrap_err();
        assert!(err.to_string().contains("[memory] init"), "{err:#}");
    }
}

#[test]
fn filesys_volume_name_is_validated() {
    use crate::filesys::MountSpec;
    let with_volume = |volume: &str| Config {
        filesys: vec![MountSpec {
            path: std::path::PathBuf::from("."),
            volume: volume.to_string(),
            boot_pri: -128,
            readonly: false,
        }],
        ..Config::default()
    };
    // A sane label mounts (the services board is added).
    assert!(with_volume("Work").build_zorro_chain().is_ok());
    // The three failure modes each report their own error.
    let err = |v: &str| format!("{:#}", with_volume(v).build_zorro_chain().unwrap_err());
    assert!(err("").contains("must not be empty"));
    assert!(err("this-volume-name-is-far-too-long-to-fit").contains("too long"));
    for bad in ["a:b", "a/b", "a\0b"] {
        assert!(err(bad).contains("invalid character"), "volume {bad:?}");
    }
}

#[test]
fn raw_config_serialize_round_trips() {
    // A populated raw config (the kind the configuration screen builds)
    // serialized to TOML and parsed back must be identical -- this guards
    // the Serialize field names/ordering against the deny_unknown_fields
    // deserialize schema.
    let raw = RawConfig {
        rom: Some("kick.rom".to_string()),
        extended_rom: Some("ext.rom".to_string()),
        identify: Some(false),
        machine: RawMachine {
            profile: Some("A1200".to_string()),
            rtc: Some(true),
            rtc_chip: Some("RP5C01".to_string()),
            rtc_time: Some(RawRtcTime::Text("2005-03-18 01:58:29".to_string())),
            rtc_frozen: Some(true),
            battmem: Some("battmem.nvram".to_string()),
            mem_controller: Some("ramsey-07".to_string()),
            rom_scsi_device_disable: Some(true),
        },
        cpu: RawCpu {
            model: Some("68EC020".to_string()),
            fpu: Some(true),
            clock_mhz: Some(14.18),
            icache: Some(true),
            dcache: None,
            unimplemented: None,
            jit: None,
        },
        memory: RawMemory {
            chip: Some("2M".to_string()),
            fast: Some("8M".to_string()),
            slow: None,
            init: Some("random:0x1234".to_string()),
            motherboard: None,
            accelerator: None,
            z3: None,
        },
        chipset: RawChipset {
            revision: Some("AGA".to_string()),
            video: Some("PAL".to_string()),
            agnus: None,
            denise: None,
        },
        ide: RawIde {
            master: Some(RawDrive::from_path("hd0.hdf")),
            slave: None,
        },
        floppy: RawFloppy {
            drives: Some(2),
            df0: Some(RawFloppyDrive {
                enabled: Some(true),
                path: Some("game.adf".to_string()),
                paths: None,
                write_protected: Some(true),
                ..RawFloppyDrive::default()
            }),
            ..RawFloppy::default()
        },
        zorro: vec![RawZorroBoard {
            metadata: "board.toml".to_string(),
            config: None,
        }],
        ..RawConfig::default()
    };
    let text = raw.to_toml_string().unwrap();
    let back: RawConfig = toml::from_str(&text).unwrap();
    assert_eq!(raw, back, "round-trip mismatch; TOML was:\n{text}");
}

#[test]
fn default_raw_config_serializes_to_empty() {
    // Nothing set means nothing written: a default machine saves as an
    // empty file (which re-parses to the defaults).
    let text = RawConfig::default().to_toml_string().unwrap();
    assert!(text.trim().is_empty(), "expected empty TOML, got:\n{text}");
}

#[test]
fn rom_fingerprint_distinguishes_same_shape_kickstarts() {
    // Two machines of identical shape but different boot ROMs must compare
    // as a mismatch (the whole point of fingerprinting the ROM rather than
    // only the machine shape).
    let mut a = MachineDescriptor::default();
    a.set_rom_fingerprint(b"kickstart 3.1 r40.068", b"");
    let mut b = MachineDescriptor::default();
    b.set_rom_fingerprint(b"kickstart 3.1.4 r46.143", b"");
    assert_ne!(a.rom, b.rom);
    let diffs = a.differences(&b);
    assert_eq!(diffs.len(), 1);
    assert!(diffs[0].starts_with("ROM "), "{diffs:?}");

    // The same image fingerprints identically, and an added extended ROM is
    // flagged on its own (same boot ROM, gained an extended ROM).
    let mut c = MachineDescriptor::default();
    c.set_rom_fingerprint(b"kickstart 3.1 r40.068", b"");
    assert_eq!(a.rom, c.rom);
    assert!(a.differences(&c).is_empty());
    let mut d = a.clone();
    d.set_rom_fingerprint(b"kickstart 3.1 r40.068", b"cd32 extended rom");
    let ext_diffs = a.differences(&d);
    assert_eq!(ext_diffs.len(), 1);
    assert!(
        ext_diffs[0].starts_with("extended ROM none -> "),
        "{ext_diffs:?}"
    );
}

#[test]
fn windows_path_escape_error_explains_fix() {
    // A backslash in a double-quoted TOML string is an escape character,
    // so an unescaped Windows path fails on "\K". The error must point at
    // the remedy rather than leaving a bare "invalid escape sequence".
    let path = temp_path("badescape.toml");
    fs::write(&path, "rom = \"C:\\Kickstarts\\KICK31.ROM\"\n").unwrap();
    let err = raw_from_path(&path).unwrap_err();
    let _ = fs::remove_file(&path);
    let msg = format!("{err:#}");
    assert!(
        msg.contains("single quotes") && msg.contains("forward slashes"),
        "error should explain the Windows-path fix, got: {msg}"
    );
}

#[test]
fn missing_emulation_uses_defaults() -> Result<()> {
    let cfg = parse_config("")?;
    assert!(cfg.emulation.power_on);
    assert_eq!(cfg.emulation.pacing_budget, PacingBudget::Cycles);
    assert_eq!(cfg.emulation.warp_speed, WarpSpeed::Max);
    Ok(())
}

#[test]
fn warp_speed_parses_levels_and_rejects_garbage() -> Result<()> {
    for (text, expected) in [
        ("2x", WarpSpeed::X2),
        ("4x", WarpSpeed::X4),
        ("8x", WarpSpeed::X8),
        ("16x", WarpSpeed::X16),
        ("max", WarpSpeed::Max),
        ("MAX", WarpSpeed::Max),
    ] {
        let cfg = parse_config(&format!("[emulation]\nwarp_speed = {text:?}\n"))?;
        assert_eq!(cfg.emulation.warp_speed, expected, "for {text:?}");
    }
    assert!(parse_config("[emulation]\nwarp_speed = \"32x\"\n").is_err());
    Ok(())
}

#[test]
fn joystick_input_mode_defaults_to_gamepad() -> Result<()> {
    // No [input] section: the port-2 source starts in Gamepad, regardless
    // of the machine profile (it is a host-input preference, not hardware).
    // Gamepad is the no-surprise default: with no pad the keyboard reaches
    // the Amiga normally instead of being captured as joystick input.
    assert_eq!(
        parse_config("")?.joystick_input_mode,
        JoystickInputMode::Gamepad
    );
    assert_eq!(
        parse_config("[machine]\nprofile = \"A1200\"\n")?.joystick_input_mode,
        JoystickInputMode::Gamepad
    );
    Ok(())
}

#[test]
fn mouse_sensitivity_defaults_to_50_and_validates() -> Result<()> {
    assert_eq!(parse_config("")?.mouse_sensitivity, 50);
    assert_eq!(
        parse_config("[input]\nmouse_sensitivity = 0\n")?.mouse_sensitivity,
        0
    );
    assert_eq!(
        parse_config("[input]\nmouse_sensitivity = 100\n")?.mouse_sensitivity,
        100
    );
    assert!(parse_config("[input]\nmouse_sensitivity = 101\n").is_err());

    // CLI override.
    let overrides = ConfigOverrides {
        mouse_sensitivity: Some(75),
        ..Default::default()
    };
    assert_eq!(load_overrides(&overrides)?.mouse_sensitivity, 75);
    Ok(())
}

#[test]
fn joystick_input_mode_parses_and_rejects_garbage() -> Result<()> {
    for (text, expected) in [
        // "auto" is a backward-compatibility alias mapping to the default.
        ("auto", JoystickInputMode::Gamepad),
        ("keyboard", JoystickInputMode::Keyboard),
        ("gamepad", JoystickInputMode::Gamepad),
        ("GAMEPAD", JoystickInputMode::Gamepad),
    ] {
        let cfg = parse_config(&format!("[input]\njoystick = {text:?}\n"))?;
        assert_eq!(cfg.joystick_input_mode, expected, "for {text:?}");
    }
    assert!(parse_config("[input]\njoystick = \"mouse\"\n").is_err());
    Ok(())
}

#[test]
fn joystick_cli_override_sets_initial_mode() -> Result<()> {
    let overrides = ConfigOverrides {
        joystick: Some("gamepad".to_string()),
        ..ConfigOverrides::default()
    };
    assert_eq!(
        load_overrides(&overrides)?.joystick_input_mode,
        JoystickInputMode::Gamepad
    );
    Ok(())
}

#[test]
fn mouse_capture_defaults_to_click_and_parses_its_modes() -> Result<()> {
    // The default is the historical behaviour: no config, no change.
    assert_eq!(parse_config("")?.mouse_capture, MouseCapture::Click);

    for (text, expected) in [
        ("click", MouseCapture::Click),
        ("on-click", MouseCapture::Click),
        ("auto", MouseCapture::Auto),
        ("focus", MouseCapture::Auto),
        ("manual", MouseCapture::Manual),
        ("off", MouseCapture::Manual),
        ("none", MouseCapture::Manual),
        ("AUTO", MouseCapture::Auto),
    ] {
        let cfg = parse_config(&format!("[input]\nmouse_capture = {text:?}\n"))?;
        assert_eq!(cfg.mouse_capture, expected, "for {text:?}");
    }

    // A typo is an error rather than a silent fallback to the default.
    assert!(parse_config("[input]\nmouse_capture = \"grab\"\n").is_err());
    Ok(())
}

#[test]
fn mouse_capture_cli_override_sets_the_mode() -> Result<()> {
    let overrides = ConfigOverrides {
        mouse_capture: Some("auto".to_string()),
        ..ConfigOverrides::default()
    };
    assert_eq!(
        load_overrides(&overrides)?.mouse_capture,
        MouseCapture::Auto
    );
    Ok(())
}

/// Every mode's label has to parse back to the same mode: the launcher
/// writes the label into the config file it saves.
#[test]
fn mouse_capture_labels_round_trip_through_the_parser() -> Result<()> {
    for mode in [
        MouseCapture::Click,
        MouseCapture::Auto,
        MouseCapture::Manual,
    ] {
        assert_eq!(parse_mouse_capture(mode.label())?, mode);
    }
    Ok(())
}

#[test]
fn port_devices_default_to_mouse_and_joystick() -> Result<()> {
    // No [input] port keys: the stock wiring, on every non-CD32 profile.
    assert_eq!(
        parse_config("")?.port_devices,
        [PortDevice::Mouse, PortDevice::Joystick]
    );
    assert_eq!(
        parse_config("[machine]\nprofile = \"A1200\"\n")?.port_devices,
        [PortDevice::Mouse, PortDevice::Joystick]
    );
    Ok(())
}

#[test]
fn cd32_profile_defaults_port_2_to_the_bundled_pad() -> Result<()> {
    let cfg = parse_config("[machine]\nprofile = \"CD32\"\n")?;
    assert_eq!(cfg.port_devices, [PortDevice::Mouse, PortDevice::Cd32Pad]);
    // An explicit key beats the profile: a real CD32 accepts any
    // controller.
    let cfg = parse_config("[machine]\nprofile = \"CD32\"\n[input]\nport2 = \"joystick\"\n")?;
    assert_eq!(cfg.port_devices[1], PortDevice::Joystick);
    Ok(())
}

#[test]
fn autofire_off_holds_the_button_and_a_rate_pulses_it() {
    // Off: always asserted, so a held button is simply held.
    for t in [0.0, 0.01, 12.7] {
        assert!(autofire_asserted(0, t));
    }
    // 5 Hz: 100 ms asserted, 100 ms released, from t=0.
    assert!(autofire_asserted(5, 0.0));
    assert!(autofire_asserted(5, 0.099));
    assert!(!autofire_asserted(5, 0.101));
    assert!(!autofire_asserted(5, 0.199));
    assert!(autofire_asserted(5, 0.201));

    // One full cycle per 1/hz second, at every offered rate.
    for hz in AUTOFIRE_RATES.into_iter().filter(|&r| r != 0) {
        let period = 1.0 / f64::from(hz);
        let samples = 400;
        let asserted = (0..samples)
            .filter(|i| autofire_asserted(hz, period * f64::from(*i) / f64::from(samples)))
            .count();
        assert!(
            (asserted as i32 - samples / 2).abs() <= 1,
            "{hz} Hz should be asserted for half of each period, was {asserted}/{samples}"
        );
    }
}

#[test]
fn autofire_rates_are_labelled_and_stay_within_the_usable_range() {
    assert_eq!(AUTOFIRE_RATES[0], 0, "the list opens with off");
    assert_eq!(autofire_label(0), "off");
    assert_eq!(autofire_label(8), "8 Hz");
    // Above the cap the assert window is shorter than the frame the guest
    // samples on, so no rate the menu offers may exceed it.
    for hz in AUTOFIRE_RATES {
        assert!(hz <= AUTOFIRE_MAX_HZ, "{hz} Hz is past the usable range");
    }
}

#[test]
fn autofire_defaults_off_and_rejects_implausible_rates() -> Result<()> {
    assert_eq!(parse_config("")?.autofire_hz, 0);
    assert_eq!(parse_config("[input]\nautofire_hz = 8\n")?.autofire_hz, 8);
    assert_eq!(
        parse_config(&format!("[input]\nautofire_hz = {}\n", AUTOFIRE_MAX_HZ))?.autofire_hz,
        AUTOFIRE_MAX_HZ
    );
    // Faster than the guest can sample the port is a typo, not a setting.
    assert!(parse_config(&format!("[input]\nautofire_hz = {}\n", AUTOFIRE_MAX_HZ + 1)).is_err());

    // The CLI flag layers over the config file, as the other input keys do.
    let overrides = ConfigOverrides {
        autofire_hz: Some(12),
        ..Default::default()
    };
    assert_eq!(load_overrides(&overrides)?.autofire_hz, 12);
    Ok(())
}

#[test]
fn port_device_keys_parse_aliases_and_reject_garbage() -> Result<()> {
    for (text, expected) in [
        ("mouse", PortDevice::Mouse),
        ("joystick", PortDevice::Joystick),
        ("JOY", PortDevice::Joystick),
        ("cd32", PortDevice::Cd32Pad),
        ("cd32pad", PortDevice::Cd32Pad),
        ("pad", PortDevice::Cd32Pad),
        ("analogue", PortDevice::Analogue),
        ("analog", PortDevice::Analogue),
        ("paddle", PortDevice::Analogue),
        ("none", PortDevice::None),
        ("off", PortDevice::None),
    ] {
        let cfg = parse_config(&format!("[input]\nport1 = {text:?}\n"))?;
        assert_eq!(cfg.port_devices[0], expected, "for {text:?}");
    }
    let err = parse_config("[input]\nport2 = \"trackball\"\n").unwrap_err();
    assert!(err.to_string().contains("port2"), "{err}");
    Ok(())
}

#[test]
fn port_device_cli_overrides_swap_the_wiring() -> Result<()> {
    let overrides = ConfigOverrides {
        port1: Some("joystick".to_string()),
        port2: Some("mouse".to_string()),
        ..ConfigOverrides::default()
    };
    assert!(!overrides.is_empty());
    assert_eq!(
        load_overrides(&overrides)?.port_devices,
        [PortDevice::Joystick, PortDevice::Mouse]
    );
    Ok(())
}

#[test]
fn warp_speed_cycle_wraps_through_levels() {
    // The menu/keyboard "cycle" control walks 2x -> 4x -> 8x -> 16x ->
    // Max and back to 2x.
    let order = [
        WarpSpeed::X2,
        WarpSpeed::X4,
        WarpSpeed::X8,
        WarpSpeed::X16,
        WarpSpeed::Max,
    ];
    for window in order.windows(2) {
        assert_eq!(window[0].next(), window[1]);
    }
    assert_eq!(WarpSpeed::Max.next(), WarpSpeed::X2);
    // Fixed levels retire exactly their multiplier in frames; Max is
    // bounded by a wall-clock budget rather than a small fixed count.
    assert_eq!(WarpSpeed::X8.frame_cap(), 8);
    assert!(WarpSpeed::X8.time_budget_ms().is_none());
    assert_eq!(WarpSpeed::Max.time_budget_ms(), Some(WARP_MAX_BUDGET_MS));
}

#[test]
fn deprecated_speed_option_is_accepted_and_ignored() -> Result<()> {
    // `[emulation] speed` was removed once "real" became the only timing
    // model. Any value is now tolerated (and warned about) so old configs
    // still parse, but it has no effect.
    for value in ["real", "turbo", "warp"] {
        parse_config(&format!("[emulation]\nspeed = {value:?}\n"))?;
    }
    Ok(())
}

#[test]
fn power_on_defaults_to_true() -> Result<()> {
    let cfg = parse_config("")?;
    assert!(cfg.emulation.power_on);
    Ok(())
}

#[test]
fn power_on_false_parses() -> Result<()> {
    let cfg = parse_config(
        r#"
            [emulation]
            power_on = false
            "#,
    )?;
    assert!(!cfg.emulation.power_on);
    Ok(())
}

#[test]
fn display_fullscreen_and_status_bar_default_and_parse() -> Result<()> {
    // Defaults: windowed, status bar shown.
    let cfg = parse_config("")?;
    assert!(!cfg.full_screen);
    assert!(cfg.status_bar);

    let cfg = parse_config("[display]\nfull_screen = true\nstatus_bar = false\n")?;
    assert!(cfg.full_screen);
    assert!(!cfg.status_bar);

    // CLI overrides.
    let overrides = ConfigOverrides {
        full_screen: Some(true),
        status_bar: Some(false),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert!(cfg.full_screen);
    assert!(!cfg.status_bar);
    Ok(())
}

#[test]
fn display_overscan_parses_and_defaults_to_tv() -> Result<()> {
    assert_eq!(parse_config("")?.overscan, Overscan::Tv);
    let cfg = parse_config(
        r#"
            [display]
            overscan = "Full"
            "#,
    )?;
    assert_eq!(cfg.overscan, Overscan::Full);
    assert!(parse_config("[display]\noverscan = \"crop\"").is_err());
    Ok(())
}

#[test]
fn display_pixel_aspect_parses_and_defaults_to_tv() -> Result<()> {
    assert_eq!(parse_config("")?.pixel_aspect, PixelAspect::Tv);
    let cfg = parse_config(
        r#"
            [display]
            pixel_aspect = "Square"
            "#,
    )?;
    assert_eq!(cfg.pixel_aspect, PixelAspect::Square);
    assert!(parse_config("[display]\npixel_aspect = \"1:1\"").is_err());
    Ok(())
}

#[test]
fn display_scaling_parses_and_defaults_to_smooth() -> Result<()> {
    assert_eq!(parse_config("")?.scaling, DisplayScaling::Smooth);
    let cfg = parse_config(
        r#"
            [display]
            scaling = "Integer"
            "#,
    )?;
    assert_eq!(cfg.scaling, DisplayScaling::Integer);
    assert!(parse_config("[display]\nscaling = \"2x\"").is_err());
    Ok(())
}

#[test]
fn display_autocrop_parses_and_defaults_off() -> Result<()> {
    assert!(!parse_config("")?.autocrop);
    let cfg = parse_config(
        r#"
            [display]
            autocrop = true
            "#,
    )?;
    assert!(cfg.autocrop);
    Ok(())
}

#[test]
fn display_phosphor_parses_and_rejects_out_of_range() -> Result<()> {
    assert_eq!(parse_config("")?.phosphor, 0.0);
    let cfg = parse_config(
        r#"
            [display]
            phosphor = 0.4
            "#,
    )?;
    assert_eq!(cfg.phosphor, 0.4);
    assert!(parse_config("[display]\nphosphor = 1.5").is_err());
    assert!(parse_config("[display]\nphosphor = -0.1").is_err());
    Ok(())
}

#[test]
fn display_tv_centre_parses_and_rejects_out_of_range() -> Result<()> {
    assert_eq!(parse_config("")?.tv_centre, TvCentre::default());
    let cfg = parse_config(
        r#"
            [display]
            tv_h_centre = 6
            tv_v_centre = -3
            "#,
    )?;
    assert_eq!(cfg.tv_centre, TvCentre { h: 6, v: -3 });
    assert!(parse_config("[display]\ntv_h_centre = 17").is_err());
    assert!(parse_config("[display]\ntv_h_centre = -17").is_err());
    assert!(parse_config("[display]\ntv_v_centre = 9").is_err());
    assert!(parse_config("[display]\ntv_v_centre = -9").is_err());
    Ok(())
}

#[test]
fn display_deinterlace_parses_and_defaults_on() -> Result<()> {
    assert!(parse_config("")?.deinterlace);
    assert!(!parse_config("[display]\ndeinterlace = false")?.deinterlace);
    assert!(parse_config("[display]\ndeinterlace = true")?.deinterlace);
    Ok(())
}

#[test]
fn display_shader_parses_presets_and_defaults_to_none() -> Result<()> {
    assert_eq!(parse_config("")?.shader, ShaderMode::None);
    assert_eq!(parse_shader(" None ")?, ShaderMode::None);
    // "off" is the label spelling, and must parse back to the same mode.
    assert_eq!(parse_shader("off")?, ShaderMode::None);
    assert_eq!(parse_shader(" OFF ")?, ShaderMode::None);
    assert_eq!(parse_shader(ShaderKind::None.label())?, ShaderMode::None);
    assert_eq!(parse_shader("SCANLINES")?, ShaderMode::Scanlines);
    assert_eq!(parse_shader("Mask")?, ShaderMode::Mask);
    assert_eq!(parse_shader("\tcrt\n")?, ShaderMode::Crt);
    let cfg = parse_config(
        r#"
            [display]
            shader = "CRT"
            "#,
    )?;
    assert_eq!(cfg.shader, ShaderMode::Crt);
    Ok(())
}

#[test]
fn display_bezel_names_a_style_and_defaults_to_off() -> Result<()> {
    assert_eq!(parse_config("")?.bezel, BezelStyle::None);
    for (written, want) in [
        ("\"1084\"", BezelStyle::Model1084),
        ("\"classic\"", BezelStyle::Classic),
        ("\"off\"", BezelStyle::None),
        // The boolean from when there was only one frame to turn on.
        ("true", BezelStyle::Model1084),
        ("false", BezelStyle::None),
    ] {
        let cfg = parse_config(&format!("[display]\nbezel = {written}\n"))?;
        assert_eq!(cfg.bezel, want, "bezel = {written}");
    }
    // Every style's config name is what it round-trips as.
    for style in BezelStyle::MENU_ORDER {
        assert_eq!(parse_bezel(style.label())?, style);
    }
    assert!(parse_config("[display]\nbezel = \"1084s\"\n").is_err());
    Ok(())
}

#[test]
fn display_bezel_stickers_names_a_folder_and_defaults_to_none() -> Result<()> {
    assert_eq!(parse_config("")?.bezel_stickers, None);
    let cfg = parse_config("[display]\nbezel_stickers = \"decals/retro32\"\n")?;
    assert_eq!(
        cfg.bezel_stickers.as_deref(),
        Some(Path::new("decals/retro32"))
    );
    // Written but empty means none, not a folder called "".
    assert_eq!(
        parse_config("[display]\nbezel_stickers = \"  \"\n")?.bezel_stickers,
        None
    );
    Ok(())
}

#[test]
fn display_perf_overlay_parses_and_defaults_to_off() -> Result<()> {
    assert!(!parse_config("")?.perf_overlay);
    let cfg = parse_config(
        r#"
            [display]
            perf_overlay = true
            "#,
    )?;
    assert!(cfg.perf_overlay);

    let mut raw = RawConfig::default();
    ConfigOverrides {
        perf_overlay: Some(true),
        ..Default::default()
    }
    .apply_to(&mut raw);
    assert_eq!(raw.display.perf_overlay, Some(true));
    Ok(())
}

#[test]
fn display_shader_takes_a_wgsl_path_verbatim() -> Result<()> {
    // Host paths are case-sensitive, so only the extension match is
    // case-insensitive: the path itself must survive unchanged.
    assert_eq!(
        parse_shader("shaders/my.wgsl")?,
        ShaderMode::Custom(PathBuf::from("shaders/my.wgsl"))
    );
    assert_eq!(
        parse_shader(" /abs/path/My.WGSL ")?,
        ShaderMode::Custom(PathBuf::from("/abs/path/My.WGSL"))
    );
    assert_eq!(parse_shader("shaders/my.wgsl")?.kind(), ShaderKind::Custom);
    assert_eq!(ShaderKind::Custom.label(), "custom");

    // The same through a whole config: a missing file is the loader's
    // problem, so parsing keeps the path as written.
    let cfg = parse_config(
        r#"
            [display]
            shader = "shaders/Aperture.wgsl"
            "#,
    )?;
    assert_eq!(
        cfg.shader,
        ShaderMode::Custom(PathBuf::from("shaders/Aperture.wgsl"))
    );
    Ok(())
}

#[test]
fn display_shader_rejects_an_unknown_name() {
    let e = parse_shader("bloom").unwrap_err().to_string();
    assert!(
        e.contains("scanlines") && e.contains("crt") && e.contains(".wgsl"),
        "{e}"
    );
    // Quoted as written, since a rejected value is usually a mistyped
    // path and lowercasing it would hide the typo.
    let e = parse_shader(" Shaders/Bloom.wsgl ")
        .unwrap_err()
        .to_string();
    assert!(e.contains(r#""Shaders/Bloom.wsgl""#), "{e}");
    assert!(parse_config("[display]\nshader = \"bloom\"").is_err());
}

#[test]
fn display_shader_strength_parses_and_rejects_out_of_range() -> Result<()> {
    assert_eq!(parse_config("")?.shader_strength, 1.0);
    let cfg = parse_config(
        r#"
            [display]
            shader_strength = 0.5
            "#,
    )?;
    assert_eq!(cfg.shader_strength, 0.5);
    assert!(parse_config("[display]\nshader_strength = 1.5").is_err());
    assert!(parse_config("[display]\nshader_strength = -0.1").is_err());
    Ok(())
}

#[test]
fn display_menu_scale_parses_and_defaults_to_1x() -> Result<()> {
    assert_eq!(parse_config("")?.menu_scale, MenuScale::Normal);
    let cfg = parse_config(
        r#"
            [display]
            menu_scale = "2x"
        "#,
    )?;
    assert_eq!(cfg.menu_scale, MenuScale::Large);
    // Every label round-trips through the parser.
    for scale in MenuScale::MENU_ORDER {
        assert_eq!(
            parse_menu_scale(scale.label()).expect("label parses"),
            scale
        );
    }
    Ok(())
}

#[test]
fn display_menu_scale_rejects_an_unknown_size() {
    let err = parse_config(
        r#"
            [display]
            menu_scale = "3x"
        "#,
    )
    .expect_err("unknown size");
    assert!(err.to_string().contains("menu_scale"), "{err}");
}

#[test]
fn display_tint_parses_names_and_defaults_to_none() -> Result<()> {
    assert_eq!(parse_config("")?.tint, Tint::None);
    assert_eq!(parse_tint(" None ")?, Tint::None);
    // "off" is the label spelling, and must parse back to the same tint.
    assert_eq!(parse_tint("off")?, Tint::None);
    assert_eq!(parse_tint(Tint::None.label())?, Tint::None);
    assert_eq!(parse_tint("BW")?, Tint::Bw);
    assert_eq!(parse_tint("Green")?, Tint::Green);
    assert_eq!(parse_tint("\tamber\n")?, Tint::Amber);
    assert_eq!(parse_tint("sepia")?, Tint::Sepia);
    // Every label round-trips through the parser.
    for tint in [Tint::Bw, Tint::Green, Tint::Amber, Tint::Sepia] {
        assert_eq!(parse_tint(tint.label())?, tint);
    }
    let cfg = parse_config(
        r#"
            [display]
            tint = "green"
            "#,
    )?;
    assert_eq!(cfg.tint, Tint::Green);
    Ok(())
}

#[test]
fn display_tint_rejects_an_unknown_name() {
    let e = parse_tint("purple").unwrap_err().to_string();
    assert!(
        e.contains("green") && e.contains("sepia") && e.contains(r#""purple""#),
        "{e}"
    );
    assert!(parse_config("[display]\ntint = \"purple\"").is_err());
}

#[test]
fn display_shader_keys_round_trip_through_saved_toml() {
    let raw = RawConfig {
        display: RawDisplay {
            shader: Some("crt".to_string()),
            shader_strength: Some(0.75),
            tint: Some("amber".to_string()),
            ..RawDisplay::default()
        },
        ..RawConfig::default()
    };
    let text = raw.to_toml_string().unwrap();
    let back: RawConfig = toml::from_str(&text).unwrap();
    assert_eq!(raw, back, "round-trip mismatch; TOML was:\n{text}");
}

#[test]
fn display_custom_shader_path_round_trips_through_saved_toml() {
    // A Windows path is all backslashes, which TOML escapes: the saved
    // file must parse back to the identical path, not a mangled one.
    let path = r"C:\Amiga\shaders\crt.wgsl";
    let raw = RawConfig {
        display: RawDisplay {
            shader: Some(path.to_string()),
            ..RawDisplay::default()
        },
        ..RawConfig::default()
    };
    let text = raw.to_toml_string().unwrap();
    let back: RawConfig = toml::from_str(&text).unwrap();
    assert_eq!(raw, back, "round-trip mismatch; TOML was:\n{text}");

    let cfg: Config = back.try_into().unwrap();
    assert_eq!(cfg.shader, ShaderMode::Custom(PathBuf::from(path)));
}

#[test]
fn chipset_video_standard_parses() -> Result<()> {
    let cfg = parse_config(
        r#"
            [chipset]
            video = "NTSC"
            "#,
    )?;
    assert_eq!(cfg.video_standard, VideoStandard::Ntsc);
    Ok(())
}

#[test]
fn machine_profile_defaults_match_bare_profile_configs() -> Result<()> {
    // machine_profile_defaults is also consumed directly, outside the
    // raw-config pipeline (the browser build, the launcher fallback), so
    // the machine it returns must be the machine a config file naming
    // just the profile produces -- including everything the pipeline
    // derives for absent [chipset]/[cpu] keys. The browser's first
    // A1200 shipped with this broken: an AGA machine carrying the
    // default 1 MiB-reach ECS Agnus, whose chip-window mirroring made
    // the guest size 1 MiB of the 2 MiB fitted chip RAM.
    use MachineModel::*;
    for model in [
        A1000, A500, A500Ocs, A500Plus, A600, A1200, A3000, A4000, Cdtv, Cd32,
    ] {
        let direct = machine_profile_defaults(model);
        let piped = parse_config(&format!("[machine]\nprofile = \"{model:?}\"\n"))?;
        assert_eq!(piped.chipset, direct.chipset, "{model:?} chipset");
        assert_eq!(
            piped.agnus_revision, direct.agnus_revision,
            "{model:?} agnus"
        );
        assert_eq!(
            piped.denise_revision, direct.denise_revision,
            "{model:?} denise"
        );
        assert_eq!(piped.cpu, direct.cpu, "{model:?} cpu");
        assert!(
            (piped.cpu_clock_mhz - direct.cpu_clock_mhz).abs() < 1e-9,
            "{model:?} cpu clock: piped {} vs direct {}",
            piped.cpu_clock_mhz,
            direct.cpu_clock_mhz
        );
        assert_eq!(piped.fpu, direct.fpu, "{model:?} fpu");
        assert_eq!(piped.cpu_icache, direct.cpu_icache, "{model:?} icache");
        assert_eq!(piped.cpu_dcache, direct.cpu_dcache, "{model:?} dcache");
        assert_eq!(
            piped.chip_ram_bytes, direct.chip_ram_bytes,
            "{model:?} chip RAM"
        );
        assert_eq!(
            piped.slow_ram_bytes, direct.slow_ram_bytes,
            "{model:?} slow RAM"
        );
        assert_eq!(piped.mb_ram_bytes, direct.mb_ram_bytes, "{model:?} mb RAM");
        assert_eq!(piped.gate_array, direct.gate_array, "{model:?} gate array");
        assert_eq!(
            piped.mem_controller, direct.mem_controller,
            "{model:?} mem controller"
        );
        assert_eq!(piped.rtc_present, direct.rtc_present, "{model:?} rtc");
        assert_eq!(piped.rtc_chip, direct.rtc_chip, "{model:?} rtc chip");
        assert_eq!(piped.rtg, direct.rtg, "{model:?} rtg");
        assert_eq!(
            piped.rtg_vram_bytes, direct.rtg_vram_bytes,
            "{model:?} RTG VRAM"
        );
        assert_eq!(
            piped.floppy_connected, direct.floppy_connected,
            "{model:?} floppy drives"
        );
        assert_eq!(piped.fmv_rom_path, direct.fmv_rom_path, "{model:?} FMV ROM");
    }
    Ok(())
}

#[test]
fn fmv_rom_is_cd32_only() -> Result<()> {
    let bundled = parse_config(
        r#"
            [machine]
            profile = "CD32"
            "#,
    )?;
    assert_eq!(
        bundled.fmv_rom_path.as_deref(),
        Some(Path::new(BUNDLED_FMV_ROM))
    );

    let cfg = parse_config(
        r#"
            fmv_rom = "cd32fmv.rom"
            [machine]
            profile = "CD32"
            "#,
    )?;
    assert_eq!(cfg.fmv_rom_path.as_deref(), Some(Path::new("cd32fmv.rom")));

    let unfitted = parse_config(
        r#"
            fmv_rom = ""
            [machine]
            profile = "CD32"
            "#,
    )?;
    assert_eq!(unfitted.fmv_rom_path, None);

    let err = parse_config(
        r#"
            fmv_rom = "cd32fmv.rom"
            [machine]
            profile = "A1200"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("only valid for a CD32"), "{err:#}");
    Ok(())
}

#[test]
fn machine_profiles_supply_defaults_and_keep_overrides() -> Result<()> {
    // No [machine] section: the default machine is the A500 Rev 6A
    // (ECS 8372A Agnus + OCS 8362 Denise), no gate array, no RTC (the base
    // A500 had no battery clock), stock 512K chip + 512K trapdoor slow RAM.
    // cfg.machine stays None -- the profile id only changes with an
    // explicit [machine] profile.
    let cfg = parse_config("")?;
    assert_eq!(cfg.machine, None);
    assert_eq!(cfg.chipset, Chipset::Ecs);
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
    assert_eq!(cfg.gate_array, GateArray::None);
    assert_eq!(cfg.chip_ram_bytes, 512 * 1024);
    assert_eq!(cfg.slow_ram_bytes, 512 * 1024);
    assert!(!cfg.rtc_present);

    let cfg = parse_config(
        r#"
            [machine]
            profile = "A500"
            "#,
    )?;
    assert_eq!(cfg.machine, Some(MachineModel::A500));
    // Rev 6A board: ECS Agnus (the 1 MiB 8372A) with the original OCS
    // Denise, stock 512 KiB chip + 512 KiB trapdoor slow RAM.
    assert_eq!(cfg.chipset, Chipset::Ecs);
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
    assert_eq!(cfg.chip_ram_bytes, 512 * 1024);
    assert_eq!(cfg.slow_ram_bytes, 512 * 1024);

    let cfg = parse_config(
        r#"
            [machine]
            profile = "A500"
            [memory]
            slow = "0"
            "#,
    )?;
    assert_eq!(cfg.slow_ram_bytes, 0);

    let cfg = parse_config(
        r#"
            [machine]
            profile = "A600"
            "#,
    )?;
    assert_eq!(cfg.machine, Some(MachineModel::A600));
    assert_eq!(cfg.gate_array, GateArray::GayleA600);
    assert_eq!(cfg.chipset, Chipset::Ecs);
    assert_eq!(cfg.chip_ram_bytes, 1024 * 1024);
    assert_eq!(cfg.slow_ram_bytes, 0);
    // The A600 board carries the 2 MB-capable 8375 even with 1 MB fitted.
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8375);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ecs8373);
    assert_eq!(cfg.cpu, CpuModel::M68000);
    // The base A600 shipped without a battery clock.
    assert!(!cfg.rtc_present);

    // Explicit sections override profile defaults: an A600HD re-fits the
    // RTC the base A600 lacks.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A600"
            rtc = true
            [memory]
            chip = "2M"
            "#,
    )?;
    assert_eq!(cfg.chip_ram_bytes, 2 * 1024 * 1024);
    assert!(cfg.rtc_present);

    let cfg = parse_config(
        r#"
            [machine]
            profile = "A500Plus"
            "#,
    )?;
    assert_eq!(cfg.mem_controller, MemController::None);
    assert_eq!(cfg.chipset, Chipset::Ecs);
    assert_eq!(cfg.chip_ram_bytes, 1024 * 1024);
    assert_eq!(cfg.slow_ram_bytes, 0);
    // The A500+ (Rev 8A) board carries the 2 MB-capable 8375, like the
    // A600, even though it ships with 1 MB chip RAM fitted.
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8375);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ecs8373);
    assert_eq!(cfg.gate_array, GateArray::None);
    // The A500+ has an OKI RTC soldered to the motherboard.
    assert!(cfg.rtc_present);

    let cfg = parse_config(
        r#"
            [machine]
            profile = "A1200"
            "#,
    )?;
    assert_eq!(cfg.cpu, CpuModel::M68EC020);
    assert_eq!(cfg.slow_ram_bytes, 0);
    assert_eq!(cfg.gate_array, GateArray::GayleA1200);
    assert_eq!(cfg.agnus_revision, AgnusRevision::AgaAlice);
    assert_eq!(cfg.denise_revision, DeniseRevision::AgaLisa);

    // The big-box machines: Ramsey instead of Gayle, and a real CPU.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A4000"
            "#,
    )?;
    assert_eq!(cfg.chipset, Chipset::Aga);
    assert_eq!(cfg.cpu, CpuModel::M68040);
    assert_eq!(cfg.chip_ram_bytes, 2 * 1024 * 1024);
    assert_eq!(cfg.slow_ram_bytes, 0);
    // Fat Gary, not Gayle: the big-box machines fill the same seat with the
    // other chip, so no PCMCIA and no Gayle IDE.
    assert_eq!(cfg.gate_array, GateArray::FatGary);
    assert_eq!(cfg.gate_array.gayle_id(), None);
    assert_eq!(cfg.mem_controller, MemController::Ramsey7);
    assert!(cfg.rtc_present);
    // With no [ide] drives the ROM's scsi.device would only stall the
    // boot probing the empty cable, so it is disabled by default...
    assert!(cfg.ide_a4000);
    assert!(cfg.rom_scsi_device_disable);

    // ...but drives on the cable need it: scsi.device is their boot path.
    let img = std::env::temp_dir().join(format!("clfs-ide-{}.img", std::process::id()));
    std::fs::write(&img, vec![0u8; 512 * 16]).unwrap();
    // TOML literal (single-quoted) strings so a Windows temp path's
    // backslashes are not parsed as escape sequences.
    let cfg = parse_config(&format!(
        r#"
            [machine]
            profile = "A4000"
            [ide]
            master = '{}'
            "#,
        img.display()
    ))?;
    assert!(!cfg.rom_scsi_device_disable);

    // Same rule on a Gayle machine: an A1200 with no drives skips the
    // driver, one with drives keeps it.
    let cfg = parse_config("[machine]\nprofile = \"A1200\"")?;
    assert!(cfg.rom_scsi_device_disable);
    let cfg = parse_config(&format!(
        "[machine]\nprofile = \"A1200\"\n[ide]\nmaster = '{}'",
        img.display()
    ))?;
    assert!(!cfg.rom_scsi_device_disable);

    let cfg = parse_config(
        r#"
            [machine]
            profile = "A3000"
            "#,
    )?;
    assert_eq!(cfg.chipset, Chipset::Ecs);
    assert_eq!(cfg.cpu, CpuModel::M68030);
    assert_eq!(cfg.gate_array, GateArray::FatGary);
    assert_eq!(cfg.mem_controller, MemController::Ramsey4);
    assert!(cfg.sdmac);
    // An empty SDMAC SCSI bus is probe time too, and a drive on it brings
    // the driver back, exactly like the IDE machines.
    assert!(cfg.rom_scsi_device_disable);
    let cfg = parse_config(&format!(
        "[machine]\nprofile = \"A3000\"\n[scsi]\nunit0 = '{}'",
        img.display()
    ))?;
    assert!(!cfg.rom_scsi_device_disable);
    std::fs::remove_file(&img).unwrap();

    // The default is an opt-out, not a lock-out: setting the flag wins
    // over the empty-bus heuristic in both directions.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A3000"
            rom_scsi_device_disable = false
            "#,
    )?;
    assert!(!cfg.rom_scsi_device_disable);
    // A machine with no built-in controller has no scsi.device in ROM;
    // there is nothing to disable.
    assert!(!parse_config("")?.rom_scsi_device_disable);

    let err = parse_config(
        r#"
            [machine]
            profile = "A5000"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown machine model"), "{err:#}");
    Ok(())
}

#[test]
fn a500_rev6a_agnus_allows_up_to_1mb_chip() -> Result<()> {
    // The Fatter 8372A reaches 1 MiB, so the 1 MiB chip-RAM mod is a
    // valid A500 configuration and still carries the 8372A (not the
    // 2 MiB 8375) alongside the OCS Denise.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A500"
            [memory]
            chip = "1M"
            slow = "0"
            "#,
    )?;
    assert_eq!(cfg.chip_ram_bytes, 1024 * 1024);
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);

    // 2 MiB chip exceeds the 8372A's 1 MiB address reach: rejected
    // rather than silently promoted to a 2 MiB 8375.
    let err = parse_config(
        r#"
            [machine]
            profile = "A500"
            [memory]
            chip = "2M"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("Agnus address reach"), "{err:#}");

    // An explicit [chipset] revision overrides the profile's board chips:
    // profile = "A500" + revision = "OCS" is a plain 8371/8362 OCS
    // machine, not the Fatter-Agnus Rev 6A.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A500"
            [chipset]
            revision = "OCS"
            "#,
    )?;
    assert_eq!(cfg.chipset, Chipset::Ocs);
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ocs);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
    Ok(())
}

#[test]
fn a500ocs_profile_is_a_plain_512k_ocs_machine() -> Result<()> {
    // The early A500 (Rev 3/5) / A2000: 8370/8371 Fat Agnus + OCS Denise,
    // 512 KiB chip + 512 KiB trapdoor slow RAM, no gate array.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A500OCS"
            "#,
    )?;
    assert_eq!(cfg.machine, Some(MachineModel::A500Ocs));
    assert_eq!(cfg.chipset, Chipset::Ocs);
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ocs);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
    assert_eq!(cfg.chip_ram_bytes, 512 * 1024);
    assert_eq!(cfg.slow_ram_bytes, 512 * 1024);
    assert_eq!(cfg.gate_array, GateArray::None);

    // The OCS Fat Agnus tops out at 512 KiB chip RAM.
    let err = parse_config(
        r#"
            [machine]
            profile = "A500OCS"
            [memory]
            chip = "1M"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("chipset maximum"), "{err:#}");
    Ok(())
}

#[test]
fn a1000_profile_is_an_ocs_machine_with_wcs_defaults() -> Result<()> {
    // The original Amiga: OCS 8361/8367 Agnus + OCS 8362 Denise, 256 KiB
    // stock chip RAM, no slow RAM, no RTC, no gate array. The `rom` is the
    // 64 KiB bootstrap ROM (loaded by Memory::load_a1000, not here).
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A1000"
            "#,
    )?;
    assert_eq!(cfg.machine, Some(MachineModel::A1000));
    assert_eq!(cfg.chipset, Chipset::Ocs);
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ocs);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
    assert_eq!(cfg.chip_ram_bytes, 256 * 1024);
    assert_eq!(cfg.slow_ram_bytes, 0);
    assert!(!cfg.rtc_present);
    assert_eq!(cfg.gate_array, GateArray::None);
    Ok(())
}

#[test]
fn machine_profile_accepts_deprecated_model_alias() -> Result<()> {
    // `[machine] model` was the original key name; it now collides
    // visually with `[cpu] model`, so the canonical key is `profile`.
    // The old name stays accepted so existing configs keep working.
    let by_alias = parse_config(
        r#"
            [machine]
            model = "A1200"
            "#,
    )?;
    let by_profile = parse_config(
        r#"
            [machine]
            profile = "A1200"
            "#,
    )?;
    assert_eq!(by_alias.machine, Some(MachineModel::A1200));
    assert_eq!(by_alias.machine, by_profile.machine);
    Ok(())
}

#[test]
fn ide_images_require_a_machine_with_an_ide_port() {
    // The default A500 has nowhere to put them.
    let err = parse_config(
        r#"
            [ide]
            master = "disk.hdf"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("IDE port"), "{err:#}");

    let cfg = parse_config(
        r#"
            [machine]
            profile = "A600"
            [ide]
            master = "disk.hdf"
            "#,
    )
    .unwrap();
    assert_eq!(
        cfg.ide.master.as_ref().map(|d| d.path.as_path()),
        Some(Path::new("disk.hdf"))
    );
    assert_eq!(cfg.ide.slave, None);

    // The A4000's port is not Gayle's, but it takes the same drives.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A4000"
            [ide]
            master = "disk.hdf"
            "#,
    )
    .unwrap();
    assert!(cfg.ide_a4000);
    assert!(cfg.gate_array.gayle_id().is_none());
    assert_eq!(
        cfg.ide.master.as_ref().map(|d| d.path.as_path()),
        Some(Path::new("disk.hdf"))
    );
}

#[test]
fn ecs_preset_picks_agnus_variant_from_chip_ram() -> Result<()> {
    let cfg = parse_config(
        r#"
            [chipset]
            revision = "ECS"
            [memory]
            chip = "512K"
            "#,
    )?;
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ecs8373);

    let cfg = parse_config(
        r#"
            [chipset]
            revision = "ECS"
            [memory]
            chip = "2M"
            "#,
    )?;
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8375);
    Ok(())
}

#[test]
fn chipset_agnus_denise_overrides_parse() -> Result<()> {
    // Late-A500 mix: ECS Agnus with the original OCS Denise.
    let cfg = parse_config(
        r#"
            [chipset]
            revision = "ECS"
            denise = "OCS"
            "#,
    )?;
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
    assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);

    let cfg = parse_config(
        r#"
            [chipset]
            revision = "ECS"
            agnus = "8375"
            "#,
    )?;
    assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8375);

    let err = parse_config(
        r#"
            [chipset]
            agnus = "8378"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown chipset agnus"), "{err:#}");
    Ok(())
}

#[test]
fn chip_ram_beyond_agnus_reach_is_rejected() {
    let err = parse_config(
        r#"
            [chipset]
            revision = "ECS"
            agnus = "8372A"
            [memory]
            chip = "2M"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("Agnus address reach"), "{err:#}");
}

#[test]
fn invalid_video_standard_fails_cleanly() {
    let err = parse_config(
        r#"
            [chipset]
            video = "SECAM"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown chipset video"), "{err:#}");
}

#[test]
fn cpu_68ec020_parses_as_24_bit_020() -> Result<()> {
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68EC020"
            "#,
    )?;
    assert_eq!(cfg.cpu, CpuModel::M68EC020);
    Ok(())
}

#[test]
fn fpu_defaults_from_cpu_model() -> Result<()> {
    // 68881/68882 boards are opt-in on the 020/030...
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68020"
            "#,
    )?;
    assert!(!cfg.fpu);

    // ...but the full 68040 has its FPU on-die.
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68040"
            "#,
    )?;
    assert!(cfg.fpu);
    Ok(())
}

#[test]
fn fpu_needs_the_coprocessor_interface() -> Result<()> {
    // A 68000 cannot drive a 68881/68882.
    let err = parse_config(
        r#"
            [cpu]
            model = "68000"
            fpu = true
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("coprocessor interface"));

    // Any 020+ can.
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68EC020"
            fpu = true
            "#,
    )?;
    assert!(cfg.fpu);
    Ok(())
}

#[test]
fn cpu_68060_without_fpu_is_an_lc060() -> Result<()> {
    // fpu = false on the 060 models the LC/EC parts: accepted, and the
    // core presents it as PCR.DFP (FP instructions take the disabled
    // trap) rather than a config error.
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68060"
            fpu = false
            "#,
    )?;
    assert_eq!(cfg.cpu, CpuModel::M68060);
    assert!(!cfg.fpu);
    Ok(())
}

#[test]
fn fast_ram_must_use_zorro_ii_autoconfig_size() {
    let err = parse_config(
        r#"
            [memory]
            fast = "768K"
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not an autoconfigurable"),
        "{err:#}"
    );
}

/// The big-box profiles fit their stock 4 MB of Ramsey motherboard RAM;
/// `[memory] motherboard` resizes it within Ramsey's bank layout, and it
/// is refused where no Ramsey (or no 32-bit CPU) could drive it.
#[test]
fn motherboard_ram_defaults_and_constraints() -> Result<()> {
    let cfg = parse_config("[machine]\nprofile = \"A3000\"")?;
    assert_eq!(cfg.mb_ram_bytes, 4 * 1024 * 1024);
    let cfg = parse_config("[machine]\nprofile = \"A4000\"")?;
    assert_eq!(cfg.mb_ram_bytes, 4 * 1024 * 1024);

    // Resizable up to the full 16 MB, and removable.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "16M"
            "#,
    )?;
    assert_eq!(cfg.mb_ram_bytes, 16 * 1024 * 1024);
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "0"
            "#,
    )?;
    assert_eq!(cfg.mb_ram_bytes, 0);

    // A total that fills no whole bank layout is refused.
    let err = parse_config(
        r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "5M"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("Ramsey banks"), "{err:#}");

    // No Ramsey to drive it on the default A500-class machine.
    let err = parse_config("[memory]\nmotherboard = \"4M\"").unwrap_err();
    assert!(
        err.to_string().contains("Ramsey memory controller"),
        "{err:#}"
    );

    // A 24-bit CPU cannot reach $08000000 at all.
    let err = parse_config(
        r#"
            [machine]
            profile = "A3000"
            [cpu]
            model = "68000"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("24-bit"), "{err:#}");
    Ok(())
}

/// Beyond Ramsey's four banks the A4000 fills the $04000000-$06FFFFFF
/// motherboard RAM expansion space in whole 4M banks up to 64M; the
/// A3000's Ramsey-04 does not, and partial banks are refused.
#[test]
fn motherboard_ram_expansion_space_is_an_a4000_option() -> Result<()> {
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "64M"
            "#,
    )?;
    assert_eq!(cfg.mb_ram_bytes, 64 * 1024 * 1024);
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "20M"
            "#,
    )?;
    assert_eq!(cfg.mb_ram_bytes, 20 * 1024 * 1024);

    // The A3000 stops at Ramsey's own 16M.
    let err = parse_config(
        r#"
            [machine]
            profile = "A3000"
            [memory]
            motherboard = "32M"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("A4000 option"), "{err:#}");

    // Partial expansion banks and totals past the window are refused.
    for size in ["18M", "65M", "128M"] {
        let err = parse_config(&format!(
            "[machine]\nprofile = \"A4000\"\n[memory]\nmotherboard = \"{size}\""
        ))
        .unwrap_err();
        assert!(err.to_string().contains("expansion space"), "{err:#}");
    }
    Ok(())
}

/// Accelerator (CPU-slot) RAM at $08000000 needs only a 32-bit address
/// bus: any megabyte total up to the 128M slot space, on any machine.
#[test]
fn accelerator_ram_gates_on_the_cpu_bus() -> Result<()> {
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68030"
            [memory]
            accelerator = "128M"
            "#,
    )?;
    assert_eq!(cfg.accel_ram_bytes, 128 * 1024 * 1024);
    // Not tied to the big-box profiles: an accelerated A1200 counts.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A1200"
            [cpu]
            model = "68030"
            [memory]
            accelerator = "64M"
            "#,
    )?;
    assert_eq!(cfg.accel_ram_bytes, 64 * 1024 * 1024);

    // The stock A1200 EC020 has a 24-bit bus.
    let err = parse_config(
        r#"
            [machine]
            profile = "A1200"
            [memory]
            accelerator = "64M"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("24-bit"), "{err:#}");

    // Sub-megabyte and beyond-the-slot totals are refused.
    for size in ["512K", "129M"] {
        let err = parse_config(&format!(
            "[cpu]\nmodel = \"68030\"\n[memory]\naccelerator = \"{size}\""
        ))
        .unwrap_err();
        assert!(err.to_string().contains("CPU-slot space"), "{err:#}");
    }
    Ok(())
}

#[test]
fn cpu_cache_flags_gate_on_model() -> Result<()> {
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68030"
            icache = true
            dcache = true
            "#,
    )?;
    assert!(cfg.cpu_icache);
    assert!(cfg.cpu_dcache);

    // Caches default on for the silicon that has them: a 68020/68EC020
    // gets the instruction cache (no data cache); a 68030 gets both.
    let cfg = parse_config("[cpu]\nmodel = \"68020\"")?;
    assert!(cfg.cpu_icache && !cfg.cpu_dcache);
    let cfg = parse_config("[cpu]\nmodel = \"68030\"")?;
    assert!(cfg.cpu_icache && cfg.cpu_dcache);
    // A 68040 gets both its (4 KB) caches by default.
    let cfg = parse_config("[cpu]\nmodel = \"68040\"")?;
    assert!(cfg.cpu_icache && cfg.cpu_dcache);

    // A plain 68000 has neither.
    let cfg = parse_config("[cpu]\nmodel = \"68000\"")?;
    assert!(!cfg.cpu_icache && !cfg.cpu_dcache);

    // The default is overridable: a 020 can opt out of its instruction cache.
    let cfg = parse_config("[cpu]\nmodel = \"68020\"\nicache = false")?;
    assert!(!cfg.cpu_icache);

    let err = parse_config("[cpu]\nicache = true").unwrap_err();
    assert!(err.to_string().contains("icache"), "{err:#}");

    let err = parse_config(
        r#"
            [cpu]
            model = "68020"
            dcache = true
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("68030"), "{err:#}");
    Ok(())
}

#[test]
fn z3_ram_needs_a_32_bit_cpu() {
    let err = parse_config(
        r#"
            [memory]
            z3 = "16M"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("32-bit address bus"), "{err:#}");

    let err = parse_config(
        r#"
            [cpu]
            model = "68EC020"
            [memory]
            z3 = "16M"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("32-bit address bus"), "{err:#}");
}

#[test]
fn z3_ram_parses_with_32_bit_cpu_and_validates_size() -> Result<()> {
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68030"
            [memory]
            z3 = "16M"
            "#,
    )?;
    assert_eq!(cfg.z3_ram_bytes, 16 * 1024 * 1024);

    let err = parse_config(
        r#"
            [cpu]
            model = "68030"
            [memory]
            z3 = "24M"
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not an autoconfigurable"),
        "{err:#}"
    );
    Ok(())
}

#[test]
fn scsi_section_parses_units_and_defaults_to_the_bundled_boot_rom() -> Result<()> {
    let cfg = parse_config(
        r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = "workbench.hdf"
            unit3 = "data.hdf"
            "#,
    )?;
    assert!(cfg.scsi.enabled());
    assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new("a2091.rom")));
    assert_eq!(
        cfg.scsi.units[0].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("workbench.hdf"))
    );
    assert!(cfg.scsi.units[1].is_none());
    assert_eq!(
        cfg.scsi.units[3].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("data.hdf"))
    );

    // A fitted A2091 needs a ROM because it carries scsi.device. When the
    // user does not name one, validation selects Copperline's bundled open
    // replacement.
    let cfg = parse_config(
        r#"
            [scsi]
            unit0 = "workbench.hdf"
            "#,
    )?;
    assert_eq!(cfg.scsi.controller, ScsiController::A2091);
    assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new(BUNDLED_A2091_ROM)));

    // SCSI works on any machine model (no Gayle requirement). This also
    // exercises the deprecated `model` alias for `[machine] profile`.
    let cfg = parse_config(
        r#"
            [machine]
            model = "A500"
            [scsi]
            rom = "a2091.rom"
            unit0 = "dh0.hdf"
            "#,
    )?;
    assert!(cfg.scsi.enabled());
    Ok(())
}

#[test]
fn lide_named_drive_keys_allow_holes_and_beat_the_legacy_array() -> Result<()> {
    // A slot filled with the one before it empty -- the whole point of the
    // named keys, and impossible to express with the positional `drives`
    // array that predates them.
    let cfg = parse_config(
        r#"
            [lide]
            board = "ripple"
            drive1 = "ch0-slave.hdf"
            drive3 = "ch1-slave.hdf"
            "#,
    )?;
    assert!(cfg.lide.drives[0].is_none());
    assert_eq!(
        cfg.lide.drives[1].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("ch0-slave.hdf"))
    );
    assert!(cfg.lide.drives[2].is_none());
    assert_eq!(
        cfg.lide.drives[3].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("ch1-slave.hdf"))
    );

    // The deprecated array still loads, so configs written before the named
    // keys existed keep working unchanged.
    let cfg = parse_config(
        r#"
            [lide]
            board = "ripple"
            drives = ["dh0.hdf", "dh1.hdf"]
            "#,
    )?;
    assert_eq!(
        cfg.lide.drives[0].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("dh0.hdf"))
    );
    assert_eq!(
        cfg.lide.drives[1].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("dh1.hdf"))
    );

    // A named key wins over the same slot in the legacy array, so a config
    // carrying both is unambiguous rather than order-dependent.
    let cfg = parse_config(
        r#"
            [lide]
            board = "ripple"
            drives = ["from-array.hdf"]
            drive0 = "from-named.hdf"
            "#,
    )?;
    assert_eq!(
        cfg.lide.drives[0].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("from-named.hdf"))
    );

    // A named key naming a channel the board does not have is an error, and
    // says which slot rather than just counting entries.
    let err = parse_config(
        r#"
            [lide]
            board = "ride"
            drive2 = "ch1-master.hdf"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("drive2"), "{err:#}");

    // A named key must not buy an over-long legacy array a free pass:
    // `drive_slots` only reads the first four entries, so a fifth would
    // otherwise be dropped without a word.
    let err = parse_config(
        r#"
            [lide]
            board = "ripple"
            drives = ["a.hdf", "b.hdf", "c.hdf", "d.hdf", "e.hdf"]
            drive0 = "named.hdf"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("5 entries"), "{err:#}");

    // ...and a legacy-array overflow with no named key still reports once,
    // by length, not as a `driveN` key the config never wrote.
    let err = parse_config(
        r#"
            [lide]
            board = "ride"
            drives = ["a.hdf", "b.hdf", "c.hdf"]
            "#,
    )
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("3 entries"), "{err:#}");
    assert!(
        !text.contains("drive2"),
        "should not name an unwritten key: {err:#}"
    );
    Ok(())
}

#[test]
fn lide_section_parses_board_rom_and_drives() -> Result<()> {
    let cfg = parse_config(
        r#"
            [lide]
            board = "ripple"
            rom = "lide.rom"
            drives = ["dh0.hdf", "dh1.hdf"]
            "#,
    )?;
    assert!(cfg.lide.enabled());
    assert_eq!(cfg.lide.board, crate::ide_zorro::LidePersonality::Ripple);
    assert_eq!(cfg.lide.rom.as_deref(), Some(Path::new("lide.rom")));
    assert_eq!(
        cfg.lide.drives[0].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("dh0.hdf"))
    );
    assert_eq!(
        cfg.lide.drives[1].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("dh1.hdf"))
    );
    assert!(cfg.lide.drives[2].is_none());

    // Hardware-only mode (rom = "", opting out of the bundled default) is
    // legal: no ROM, no autoboot, but the section still "enabled" via
    // drives alone.
    let cfg = parse_config(
        r#"
            [lide]
            board = "ride"
            rom = ""
            drives = ["dh0.hdf"]
            "#,
    )?;
    assert!(cfg.lide.enabled());
    assert!(cfg.lide.rom.is_none());

    // CD images attach as ATAPI drives, exactly as they do on [ide].
    let cfg = parse_config(
        r#"
            [lide]
            board = "ripple"
            rom = "lide.rom"
            drives = ["game.cue"]
            "#,
    )?;
    assert_eq!(
        cfg.lide.drives[0].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("game.cue"))
    );

    // RIDE only has one channel (two drives); a third entry overflows it.
    let err = parse_config(
        r#"
            [lide]
            board = "ride"
            rom = "lide.rom"
            drives = ["dh0.hdf", "dh1.hdf", "dh2.hdf"]
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("drive(s)"), "{err:#}");

    // rom_bank2 needs rom -- opting out of the bundled default with an
    // explicit rom = "" is the only way left to hit this with a fitted
    // board, since an unset rom on a fitted board now defaults instead.
    let err = parse_config(
        r#"
            [lide]
            board = "ripple"
            rom = ""
            rom_bank2 = "cdfs.rom"
            drives = ["dh0.hdf"]
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("rom_bank2 needs rom"), "{err:#}");

    // rom_bank2 alone, with no board/rom/drives, is not silently
    // accepted just because `LideConfig::enabled()` would say the
    // section is "off" -- rom_bank2 vs. rom is validated unconditionally.
    let err = parse_config(
        r#"
            [lide]
            rom_bank2 = "cdfs.rom"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("rom_bank2 needs rom"), "{err:#}");

    // rom_bank2 does not apply to AT-Bus 2008.
    let err = parse_config(
        r#"
            [lide]
            board = "atbus2008"
            rom = "lide-atbus.rom"
            rom_bank2 = "cdfs.rom"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("no flash banking"), "{err:#}");

    Ok(())
}

/// CD images (CUE/BIN, bare ISO, NRG, and CHD) are recognised by extension:
/// they attach as SCSI CD-ROM drives on [scsi], and as ATAPI drives on
/// [ide]/[lide].
#[test]
fn cd_images_fit_scsi_units_and_the_ide_port() -> Result<()> {
    assert!(is_cd_image_path(Path::new("games/Disc.CUE")));
    assert!(is_cd_image_path(Path::new("cd32.iso")));
    assert!(is_cd_image_path(Path::new("compilation.NRG")));
    assert!(!is_cd_image_path(Path::new("workbench.hdf")));
    assert!(!is_cd_image_path(Path::new("directory/")));

    let cfg = parse_config(
        r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = "workbench.hdf"
            unit2 = "game.cue"
            "#,
    )?;
    assert_eq!(
        cfg.scsi.units[2].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("game.cue"))
    );

    let cfg = parse_config(
        r#"
            [machine]
            profile = "A1200"
            [ide]
            master = "game.iso"
            "#,
    )?;
    assert_eq!(
        cfg.ide.master.as_ref().map(|d| d.path.as_path()),
        Some(Path::new("game.iso"))
    );
    Ok(())
}

/// The A3000's SCSI is motherboard silicon: its drives need no boot ROM,
/// they are the default on that machine, and they fit nowhere else.
#[test]
fn the_a3000_scsi_bus_takes_drives_without_a_boot_rom() -> Result<()> {
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A3000"
            [scsi]
            unit0 = "workbench.hdf"
            "#,
    )?;
    assert!(cfg.sdmac);
    assert_eq!(cfg.scsi.controller, ScsiController::A3000);
    assert_eq!(
        cfg.scsi.units[0].as_ref().map(|d| d.path.as_path()),
        Some(Path::new("workbench.hdf"))
    );

    // A Zorro board still fits an A3000, and there it does need its ROM.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A3000"
            [scsi]
            controller = "a2091"
            rom = "a2091.rom"
            unit0 = "workbench.hdf"
            "#,
    )?;
    assert_eq!(cfg.scsi.controller, ScsiController::A2091);

    // No Super DMAC, no motherboard SCSI.
    let err = parse_config(
        r#"
            [machine]
            profile = "A1200"
            [scsi]
            controller = "a3000"
            unit0 = "workbench.hdf"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("motherboard SCSI"), "{err:#}");

    // And there is no ROM to give it.
    let err = parse_config(
        r#"
            [machine]
            profile = "A3000"
            [scsi]
            rom = "a2091.rom"
            unit0 = "workbench.hdf"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("no boot ROM"), "{err:#}");
    Ok(())
}

/// A CD32 without an FMV-ROM override resolves the bundled open cartridge.
#[test]
fn cd32_without_fmv_override_resolves_the_bundled_rom() -> Result<()> {
    let mut cfg = parse_config(
        r#"
            [machine]
            profile = "CD32"
            "#,
    )?;
    assert_eq!(
        cfg.fmv_rom_path.as_deref(),
        Some(Path::new(BUNDLED_FMV_ROM))
    );
    resolve_bundled_rom(&mut cfg)?;
    let rom = cfg.fmv_rom_path.as_deref().expect("resolved FMV ROM");
    assert!(rom.ends_with(crate::romsearch::FMV_ROM_FILE), "{rom:?}");
    assert_ne!(rom, Path::new(BUNDLED_FMV_ROM));
    Ok(())
}

/// An A4091 without a named ROM falls back to the bundled one: validation
/// leaves the sentinel in place, and resolution swaps in the real path.
#[test]
fn zorro_scsi_controllers_without_rom_default_to_bundled_roms() -> Result<()> {
    // No [scsi] section means no fitted controller. The controller enum still
    // carries its A2091 personality default for callers which later attach a
    // disk, but validation must not turn that alone into a board.
    let cfg = parse_config("")?;
    assert_eq!(cfg.scsi.controller, ScsiController::A2091);
    assert!(!cfg.scsi.enabled());

    let cfg = parse_config(
        r#"
            [scsi]
            controller = "a4091"
            unit0 = "workbench.hdf"
            "#,
    )?;
    assert_eq!(cfg.scsi.controller, ScsiController::A4091);
    assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new(BUNDLED_A4091_ROM)));

    // A bare `controller = "a4091"` (no drives, no ROM) still fits the
    // board: the bundled ROM makes it enabled, as naming a ROM always did.
    let mut cfg = parse_config(
        r#"
            [scsi]
            controller = "a4091"
            "#,
    )?;
    assert!(cfg.scsi.enabled());
    assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new(BUNDLED_A4091_ROM)));

    // Resolution finds the ROM bundled in the source tree (assets/a4091).
    resolve_bundled_rom(&mut cfg)?;
    let rom = cfg.scsi.rom.as_deref().expect("resolved A4091 rom");
    assert!(rom.ends_with(crate::romsearch::A4091_ROM_FILE), "{rom:?}");
    assert_ne!(rom, Path::new(BUNDLED_A4091_ROM));

    // An explicit rom still wins.
    let cfg = parse_config(
        r#"
            [scsi]
            controller = "a4091"
            rom = "custom-a4091.rom"
            unit0 = "workbench.hdf"
            "#,
    )?;
    assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new("custom-a4091.rom")));

    // The A2091 has its own bundled open ROM and resolves independently.
    let mut cfg = parse_config(
        r#"
            [scsi]
            controller = "a2091"
            unit0 = "workbench.hdf"
            "#,
    )?;
    assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new(BUNDLED_A2091_ROM)));
    resolve_bundled_rom(&mut cfg)?;
    let rom = cfg.scsi.rom.as_deref().expect("resolved A2091 rom");
    assert!(rom.ends_with(crate::romsearch::A2091_ROM_FILE), "{rom:?}");
    assert_ne!(rom, Path::new(BUNDLED_A2091_ROM));

    // Explicit merged/split images still win over the bundled default.
    let cfg = parse_config(
        r#"
            [scsi]
            controller = "a2091"
            rom = "custom-even.rom"
            rom_odd = "custom-odd.rom"
            "#,
    )?;
    assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new("custom-even.rom")));
    assert_eq!(
        cfg.scsi.rom_odd.as_deref(),
        Some(Path::new("custom-odd.rom"))
    );

    // Naming only the odd half is still an error; the bundled merged ROM
    // must not be silently paired with it as though it were the even half.
    let err = parse_config(
        r#"
            [scsi]
            controller = "a2091"
            rom_odd = "custom-odd.rom"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("rom_odd needs rom"), "{err:#}");
    Ok(())
}

/// A fitted [lide] board with no ROM opinion of its own defaults to the
/// bundled lide.rom/cdfs.rom: validation leaves the sentinels in place, and
/// resolution swaps in the real paths. `rom = ""` opts back out.
#[test]
fn lide_without_rom_defaults_to_the_bundled_roms() -> Result<()> {
    let cfg = parse_config(
        r#"
            [lide]
            board = "ripple"
            drives = ["dh0.hdf"]
            "#,
    )?;
    assert_eq!(cfg.lide.rom.as_deref(), Some(Path::new(BUNDLED_LIDE_ROM)));
    assert_eq!(
        cfg.lide.rom_bank2.as_deref(),
        Some(Path::new(BUNDLED_LIDE_CDFS_ROM))
    );

    // A bare `board = "..."` with no drives still fits the board, exactly
    // as an A4091 does: the bundled ROM makes it enabled.
    let cfg = parse_config(
        r#"
            [lide]
            board = "ride"
            "#,
    )?;
    assert!(cfg.lide.enabled());
    assert_eq!(cfg.lide.rom.as_deref(), Some(Path::new(BUNDLED_LIDE_ROM)));

    // AT-Bus 2008 also defaults to a bundled ROM (still the sentinel at
    // this stage -- board only decides which real file resolution picks),
    // but never the CD-filesystem bank: that board has no flash banking to
    // put it in.
    let cfg = parse_config(
        r#"
            [lide]
            board = "atbus2008"
            drives = ["dh0.hdf"]
            "#,
    )?;
    assert_eq!(cfg.lide.rom.as_deref(), Some(Path::new(BUNDLED_LIDE_ROM)));
    assert!(cfg.lide.rom_bank2.is_none());

    // Resolution finds the ROMs bundled in the source tree (assets/lide),
    // and RIPPLE/RIDE resolve to lide.rom.
    let mut cfg = parse_config(
        r#"
            [lide]
            board = "ripple"
            drives = ["dh0.hdf"]
            "#,
    )?;
    resolve_bundled_rom(&mut cfg)?;
    let rom = cfg.lide.rom.as_deref().expect("resolved lide rom");
    assert!(rom.ends_with(crate::romsearch::LIDE_ROM_FILE), "{rom:?}");
    assert_ne!(rom, Path::new(BUNDLED_LIDE_ROM));
    let cdfs = cfg.lide.rom_bank2.as_deref().expect("resolved lide cdfs");
    assert!(
        cdfs.ends_with(crate::romsearch::LIDE_CDFS_ROM_FILE),
        "{cdfs:?}"
    );

    // AT-Bus 2008 resolves to its own build, lide-atbus.rom, not lide.rom --
    // the two are different linker layouts (a leading "LIV2" header vs the
    // bootloader at offset 0), matching the different diag_vec
    // BoardSpec::lide already uses per personality. Getting this wrong
    // means an AT-Bus 2008 board that fails to autoboot from the wrong
    // image, not a working substitute.
    let mut cfg = parse_config(
        r#"
            [lide]
            board = "atbus2008"
            drives = ["dh0.hdf"]
            "#,
    )?;
    resolve_bundled_rom(&mut cfg)?;
    let rom = cfg.lide.rom.as_deref().expect("resolved lide-atbus rom");
    assert!(
        rom.ends_with(crate::romsearch::LIDE_ATBUS_ROM_FILE),
        "{rom:?}"
    );
    assert!(
        cfg.lide.rom_bank2.is_none(),
        "no flash banking on AT-Bus 2008"
    );

    // An explicit rom still wins, and an explicit empty string opts all the
    // way back out to hardware-only mode.
    let cfg = parse_config(
        r#"
            [lide]
            board = "ripple"
            rom = "custom-lide.rom"
            drives = ["dh0.hdf"]
            "#,
    )?;
    assert_eq!(cfg.lide.rom.as_deref(), Some(Path::new("custom-lide.rom")));

    let cfg = parse_config(
        r#"
            [lide]
            board = "ripple"
            rom = ""
            drives = ["dh0.hdf"]
            "#,
    )?;
    assert!(cfg.lide.rom.is_none());
    assert!(cfg.lide.rom_bank2.is_none());
    Ok(())
}

#[test]
fn drive_entries_accept_a_volume_name_override() -> Result<()> {
    // IDE and SCSI drives take either a bare path or a table carrying an
    // explicit volume name; the bare form leaves the name unset.
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A1200"
            [ide]
            master = { path = "games/", name = "Games" }
            slave = "data.hdf"
            "#,
    )?;
    let master = cfg.ide.master.as_ref().expect("master configured");
    assert_eq!(master.path, Path::new("games/"));
    assert_eq!(master.volume_name.as_deref(), Some("Games"));
    let slave = cfg.ide.slave.as_ref().expect("slave configured");
    assert_eq!(slave.path, Path::new("data.hdf"));
    assert_eq!(slave.volume_name, None);

    let cfg = parse_config(
        r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = { path = "work/", name = "Work Disk" }
            "#,
    )?;
    let unit0 = cfg.scsi.units[0].as_ref().expect("unit0 configured");
    assert_eq!(unit0.volume_name.as_deref(), Some("Work Disk"));
    Ok(())
}

#[test]
fn drive_filesystem_key_selects_ofs_or_ffs_for_a_directory_mount() -> Result<()> {
    let dir =
        std::env::temp_dir().join(format!("copperline-config-fs-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_toml = dir.to_string_lossy().replace('\\', "\\\\");

    // Explicit "ofs".
    let cfg = parse_config(&format!(
        r#"
            [machine]
            profile = "A1200"
            [ide]
            master = {{ path = "{dir_toml}", filesystem = "ofs" }}
            "#,
    ))?;
    let master = cfg.ide.master.as_ref().expect("master configured");
    assert_eq!(master.filesystem, crate::diskimage::FileSystem::OFS);

    // Explicit "ffs" (case-insensitive).
    let cfg = parse_config(&format!(
        r#"
            [machine]
            profile = "A1200"
            [ide]
            master = {{ path = "{dir_toml}", filesystem = "FFS" }}
            "#,
    ))?;
    let master = cfg.ide.master.as_ref().expect("master configured");
    assert_eq!(master.filesystem, crate::diskimage::FileSystem::FFS);

    // Absent key: defaults to FFS.
    let cfg = parse_config(&format!(
        r#"
            [machine]
            profile = "A1200"
            [ide]
            master = "{dir_toml}"
            "#,
    ))?;
    let master = cfg.ide.master.as_ref().expect("master configured");
    assert_eq!(master.filesystem, crate::diskimage::FileSystem::FFS);

    // An unknown token is a clear error.
    let err = parse_config(&format!(
        r#"
            [machine]
            profile = "A1200"
            [ide]
            master = {{ path = "{dir_toml}", filesystem = "qfs" }}
            "#,
    ))
    .unwrap_err();
    assert!(err.to_string().contains("qfs"), "{err:#}");

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

#[test]
fn drive_filesystem_key_is_rejected_on_a_non_directory_path() {
    // filesystem only means something for a host-directory mount; an
    // image file has its own dostype baked into the bytes.
    let err = parse_config(
        r#"
            [machine]
            profile = "A1200"
            [ide]
            master = { path = "data.hdf", filesystem = "ofs" }
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("filesystem"), "{err:#}");
    assert!(err.to_string().contains("directory"), "{err:#}");
}

#[test]
fn the_memory_controller_can_be_selected() -> anyhow::Result<()> {
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A1200"
            mem_controller = "ramsey-07"
            "#,
    )?;
    assert_eq!(cfg.mem_controller, MemController::Ramsey7);
    assert_eq!(
        cfg.mem_controller.ramsey_revision(),
        Some(crate::ramsey::RamseyRevision::Rev7)
    );

    let err = parse_config(
        r#"
            [machine]
            mem_controller = "ramsey-08"
            "#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("ramsey-04"), "{err}");
    Ok(())
}

#[test]
fn log_unmapped_takes_all_or_a_hex_range() -> anyhow::Result<()> {
    let cfg = parse_config(
        r#"
            [debug]
            log_unmapped = "DD0000-DEFFFF"
            "#,
    )?;
    assert_eq!(cfg.log_unmapped, Some(0x00DD_0000..=0x00DE_FFFF));

    let cfg = parse_config(
        r#"
            [debug]
            log_unmapped = "all"
            "#,
    )?;
    // "all" must include the very top of the address space.
    assert_eq!(cfg.log_unmapped, Some(0..=u32::MAX));
    assert!(cfg.log_unmapped.unwrap().contains(&0xFFFF_FFFF));

    assert_eq!(parse_config("")?.log_unmapped, None);

    // An end below the start would silently log nothing.
    let err = parse_config(
        r#"
            [debug]
            log_unmapped = "DE0000-DD0000"
            "#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("start must not be above end"), "{err}");
    Ok(())
}

#[test]
fn drive_name_override_is_validated() {
    // A ':' or '/' is illegal in an AmigaDOS volume name.
    let err = parse_config(
        r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = { path = "work/", name = "Bad:Name" }
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("invalid character"), "{err:#}");

    // Over the 30-character FFS volume-label limit.
    let err = parse_config(&format!(
        r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = {{ path = "work/", name = "{}" }}
            "#,
        "X".repeat(31)
    ))
    .unwrap_err();
    assert!(err.to_string().contains("too long"), "{err:#}");

    // A blank name is treated as no override (not an error).
    let cfg = parse_config(
        r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = { path = "work/", name = "  " }
            "#,
    )
    .unwrap();
    assert_eq!(cfg.scsi.units[0].as_ref().unwrap().volume_name, None);

    // An unknown key in the table form is rejected.
    let err = parse_config(
        r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = { path = "work/", label = "Work" }
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("label"), "{err:#}");
}

#[test]
fn mixed_named_and_bare_drives_round_trip_through_saved_toml() {
    // A named drive serializes as a sub-table; a bare sibling must not be
    // swallowed by it (TOML requires scalar keys before sub-tables). Save
    // the whole config the way the panel does and parse it back.
    let raw = RawConfig {
        scsi: RawScsi {
            rom: Some("a2091.rom".to_string()),
            unit0: Some(RawDrive {
                path: "work/".to_string(),
                name: Some("Work".to_string()),
                bootpri: None,
                filesystem: None,
            }),
            unit1: Some(RawDrive::from_path("data.hdf")),
            ..RawScsi::default()
        },
        ..RawConfig::default()
    };
    let text = raw.to_toml_string().unwrap();
    let back: RawConfig = toml::from_str(&text).unwrap();
    assert_eq!(raw, back, "round-trip mismatch; TOML was:\n{text}");
}

#[test]
fn drive_entry_round_trips_through_toml() {
    // No name: serializes back to the bare string form.
    let bare = RawIde {
        master: Some(RawDrive::from_path("disk.hdf")),
        slave: None,
    };
    let text = toml::to_string(&bare).unwrap();
    assert!(text.contains(r#"master = "disk.hdf""#), "{text}");

    // With a name: serializes to the inline table and parses back.
    let named = RawIde {
        master: Some(RawDrive {
            path: "games/".to_string(),
            name: Some("Games".to_string()),
            bootpri: None,
            filesystem: None,
        }),
        slave: None,
    };
    let text = toml::to_string(&named).unwrap();
    let parsed: RawIde = toml::from_str(&text).unwrap();
    assert_eq!(parsed, named);

    // With only a boot priority: still the inline-table form, and the
    // name key stays absent.
    let prioritised = RawIde {
        master: Some(RawDrive {
            path: "wb.hdf".to_string(),
            name: None,
            bootpri: Some(6),
            filesystem: None,
        }),
        slave: None,
    };
    let text = toml::to_string(&prioritised).unwrap();
    assert!(!text.contains("name"), "{text}");
    let parsed: RawIde = toml::from_str(&text).unwrap();
    assert_eq!(parsed, prioritised);
}

// A build without the feature has no bridges to configure: the keys are
// read and ignored, so there is nothing here to assert.
#[cfg(feature = "fluxbridge")]
#[test]
fn floppy_bridge_parses_and_defaults() -> Result<()> {
    let cfg = parse_config(
        r#"
            [floppy.df0]
            bridge = "greaseweazle"
            [floppy.df1]
            bridge = "GreaseWeazle"
            bridge_port = "/dev/tty.usbmodem1111301"
            bridge_mode = "stalling"
            bridge_density = "hd"
            bridge_cable = "b"
            replay_speed = "normal"
        "#,
    )?;
    let df0 = cfg.floppy.bridges[0].as_ref().expect("df0 bridged");
    assert_eq!(df0.driver, BridgeDriver::Greaseweazle);
    // Unset options take the defaults: auto-detect the interface, read
    // without waiting for the index, and sense the density.
    assert_eq!(df0.port, None);
    assert_eq!(df0.mode, BridgeReadMode::Normal);
    assert_eq!(df0.density, BridgeDensity::Auto);
    assert_eq!(df0.speed, DEFAULT_BRIDGE_SPEED_PERCENT);

    // Spelling and case are the user's business, not the parser's.
    let df1 = cfg.floppy.bridges[1].as_ref().expect("df1 bridged");
    assert_eq!(df1.driver, BridgeDriver::Greaseweazle);
    assert_eq!(df1.port.as_deref(), Some("/dev/tty.usbmodem1111301"));
    assert_eq!(df1.mode, BridgeReadMode::Stalling);
    assert_eq!(df1.density, BridgeDensity::Hd);
    assert_eq!(df1.cable, BridgeCable::DriveB);
    assert_eq!(df1.speed, 100);

    // Bridging a bay wires the drive in, and leaves it with no image.
    assert!(cfg.floppy.drives[0].is_none());
    assert!(cfg.floppy_connected[0] && cfg.floppy_connected[1]);
    Ok(())
}

/// A driver name this build does not carry is refused where it is read,
/// naming what is available -- not accepted here only to fail later when
/// the drive is opened, by which time the machine is half built.
#[cfg(feature = "fluxbridge")]
#[test]
fn a_driver_this_build_lacks_is_refused_with_what_it_has() {
    let available = supported_bridge_drivers();
    assert!(available.contains(&"greaseweazle"), "{available:?}");
    for absent in ["drawbridge", "supercardpro"] {
        if available.contains(&absent) {
            continue;
        }
        let err = parse_config(&format!(
            r#"
                [floppy.df0]
                bridge = "{absent}"
            "#
        ))
        .expect_err("a driver this build lacks cannot be configured")
        .to_string();
        assert!(err.contains("this build has no"), "unexpected: {err}");
        assert!(err.contains("greaseweazle"), "must say what it has: {err}");
    }
}

/// Only the listed serving speeds are accepted, by name in the error so
/// a typo explains itself. A value between two of them is still refused.
#[cfg(feature = "fluxbridge")]
#[test]
fn floppy_bridge_speed_rejects_unsupported_values() {
    for bad in [120, 160, 250] {
        let err = parse_config(&format!(
            r#"
                [floppy.df0]
                bridge = "greaseweazle"
                bridge_speed = {bad}
            "#
        ))
        .expect_err("not a supported serving speed");
        let msg = format!("{err:#}");
        assert!(msg.contains("replay_speed"), "unexpected error: {msg}");
        assert!(msg.contains("normal"), "names the accepted values: {msg}");
    }
}

/// Every listed speed parses back as itself, the fastest included.
#[cfg(feature = "fluxbridge")]
#[test]
fn floppy_bridge_speed_accepts_every_listed_value() -> Result<()> {
    for want in SUPPORTED_BRIDGE_SPEED_PERCENTS {
        let cfg = parse_config(&format!(
            r#"
                [floppy.df0]
                bridge = "greaseweazle"
                bridge_speed = {want}
            "#
        ))?;
        let df0 = cfg.floppy.bridges[0].as_ref().expect("df0 bridged");
        assert_eq!(df0.speed, want);
    }
    Ok(())
}

// A build without the feature has no bridges to configure: the keys are
// read and ignored, so there is nothing here to assert.
#[cfg(feature = "fluxbridge")]
#[test]
fn floppy_bridge_rejects_conflicts_and_typos() {
    // A real drive brings its own disk, so an image alongside it is a
    // contradiction rather than a fallback.
    let err = parse_config(
        r#"
            [floppy.df0]
            bridge = "greaseweazle"
            path = "game.adf"
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("bridge and a disk image"), "{err}");

    let err = parse_config(
        r#"
            [floppy.df0]
            bridge = "greasewheel"
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("not a known interface"), "{err}");

    // `off` is how a bay keeps its bridge settings but goes back to images:
    // the image is then parsed normally, so the only complaint left is the
    // missing file rather than anything about bridges.
    let err = parse_config(
        r#"
            [floppy.df0]
            bridge = "off"
            path = "game.adf"
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        !err.contains("bridge"),
        "bridge = off is not a bridge: {err}"
    );

    // "turbo" is not a read mode at all -- it intercepts AmigaDOS calls --
    // so it is named in the refusal rather than silently swapped for one
    // that works, and a config brought over from another emulator
    // explains itself.
    assert_eq!(
        parse_config(
            r#"
                [floppy.df0]
                bridge = "greaseweazle"
                bridge_mode = "normal"
            "#
        )
        .unwrap()
        .floppy
        .bridges[0]
            .as_ref()
            .unwrap()
            .mode,
        BridgeReadMode::Normal
    );
    let err = parse_config(
        r#"
                [floppy.df0]
                bridge = "greaseweazle"
                bridge_mode = "turbo"
            "#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("instead of"), "{err}");
}

#[test]
fn drive_bootpri_defaults_to_zero_and_parses() -> Result<()> {
    let cfg = parse_config(
        r#"
            [machine]
            profile = "A1200"
            [ide]
            master = "wb.hdf"
            slave = { path = "extra.hdf", bootpri = -128 }
            "#,
    )?;
    assert_eq!(
        cfg.ide.master.as_ref().unwrap().boot_pri,
        HARDFILE_DEFAULT_BOOT_PRI
    );
    assert_eq!(cfg.ide.slave.as_ref().unwrap().boot_pri, BOOT_PRI_NEVER);

    // Out-of-range values are rejected by the i8 field type.
    assert!(parse_config(
        r#"
            [machine]
            profile = "A1200"
            [ide]
            master = { path = "wb.hdf", bootpri = 500 }
            "#,
    )
    .is_err());
    Ok(())
}

#[test]
fn zorro_metadata_boards_parse_and_gate_on_cpu() -> Result<()> {
    let meta = temp_path("board.toml");
    fs::write(
        &meta,
        r#"
            name = "MegaRAM"
            zorro = 3
            type = "ram"
            size = "32M"
            manufacturer = 2011
            product = 32
            "#,
    )?;

    let cfg = parse_config(&format!(
        r#"
            [cpu]
            model = "68030"
            [[zorro]]
            metadata = "{}"
            "#,
        toml_path(&meta)
    ))?;
    assert_eq!(cfg.zorro_boards.len(), 1);
    assert_eq!(cfg.zorro_boards[0].name, "MegaRAM");
    assert_eq!(cfg.zorro_boards[0].size_bytes, 32 * 1024 * 1024);

    let err = parse_config(&format!(
        r#"
            [[zorro]]
            metadata = "{}"
            "#,
        toml_path(&meta)
    ))
    .unwrap_err();
    assert!(err.to_string().contains("needs a 32-bit CPU"), "{err:#}");

    let _ = fs::remove_file(&meta);
    Ok(())
}

#[test]
fn zorro_metadata_boards_reject_the_bundled_hostsocket_sentinels() -> Result<()> {
    // A metadata board naming a bundled-artifact sentinel must fail fast,
    // not silently instantiate the embedded HostSocket module/ROM under
    // its own autoconfig identity.
    let meta = temp_path("sentinel-board.toml");
    fs::write(
        &meta,
        format!(
            r#"
                name = "Impostor"
                zorro = 2
                type = "wasm"
                size = "64K"
                manufacturer = 2011
                product = 33
                wasm = "{}"
                "#,
            crate::hostsocket::BUNDLED_HOSTSOCKET_WASM
        ),
    )?;
    let err =
        parse_config(&format!("[[zorro]]\nmetadata = \"{}\"\n", toml_path(&meta))).unwrap_err();
    assert!(err.to_string().contains("reserved"), "{err:#}");

    fs::write(
        &meta,
        r#"
            name = "Impostor"
            zorro = 2
            type = "wasm"
            size = "64K"
            manufacturer = 2011
            product = 33
            wasm = "board.wasm"

            [[option]]
            key = "rom"
            label = "ROM"
            type = "file"
            "#,
    )?;
    let err = parse_config(&format!(
        "[[zorro]]\nmetadata = \"{}\"\nconfig = {{ rom = \"{}\" }}\n",
        toml_path(&meta),
        crate::hostsocket::BUNDLED_HOSTSOCKET_ROM
    ))
    .unwrap_err();
    assert!(err.to_string().contains("reserved"), "{err:#}");

    let _ = fs::remove_file(&meta);
    Ok(())
}

#[test]
fn identify_board_present_by_default() -> Result<()> {
    // A bare config (no fast/Z3/metadata boards) still puts the
    // Copperline identification board on the chain.
    let cfg = parse_config("")?;
    assert!(cfg.identify_board);
    let chain = cfg.build_zorro_chain()?;
    let base = crate::zorro::AUTOCONFIG_BASE;
    // er_Type: Zorro II, no MEMLIST, 64K (size code 1) = 0xC1, exposed
    // high nibble then low nibble (er_Type is not inverted).
    assert_eq!(chain.config_read(base, 1), 0xC0);
    assert_eq!(chain.config_read(base + 2, 1), 0x10);
    // er_Product = 2, inverted to 0xFD on the physical nibbles.
    assert_eq!(chain.config_read(base + 4, 1), 0xF0);
    assert_eq!(chain.config_read(base + 6, 1), 0xD0);
    Ok(())
}

#[test]
fn identify_false_drops_the_board() -> Result<()> {
    let cfg = parse_config("identify = false")?;
    assert!(!cfg.identify_board);
    // No boards configured at all: the autoconfig window floats.
    let chain = cfg.build_zorro_chain()?;
    assert_eq!(chain.config_read(crate::zorro::AUTOCONFIG_BASE, 1), 0xFF);
    Ok(())
}

#[test]
fn toccata_is_absent_by_default_and_fits_when_enabled() -> Result<()> {
    assert!(!parse_config("")?.toccata);
    let cfg = parse_config("[toccata]\nenabled = true\n")?;
    assert!(cfg.toccata);
    Ok(())
}

#[test]
fn mhi_is_absent_by_default_and_fits_when_enabled() -> Result<()> {
    assert!(!parse_config("")?.mhi);
    let cfg = parse_config("[mhi]\nenabled = true\n")?;
    assert!(cfg.mhi);
    Ok(())
}

#[test]
fn slow_ram_parses_for_a500_trapdoor_memory() -> Result<()> {
    let cfg = parse_config(
        r#"
            [memory]
            slow = "512K"
            "#,
    )?;
    assert_eq!(cfg.slow_ram_bytes, 512 * 1024);
    Ok(())
}

#[test]
fn slow_ram_is_limited_to_trapdoor_size() {
    let err = parse_config(
        r#"
            [memory]
            slow = "1M"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("slow RAM"), "{err:#}");
}

#[test]
fn floppy_path_implies_enabled_and_write_protect_defaults() -> Result<()> {
    let adf = temp_adf()?;
    let cfg = parse_config(&format!(
        r#"
            [floppy.df0]
            path = "{}"
            "#,
        toml_path(&adf)
    ))?;
    let df0 = cfg.floppy.drives[0].as_ref().unwrap();
    assert_eq!(df0.path, adf);
    assert!(df0.write_protected);
    assert_eq!(cfg.floppy_connected, [true, false, false, false]);
    Ok(())
}

#[test]
fn floppy_drive_count_connects_empty_external_mechanisms() -> Result<()> {
    let cfg = parse_config(
        r#"
            [floppy]
            drives = 3
            "#,
    )?;
    assert_eq!(cfg.floppy_connected, [true, true, true, false]);
    assert!(cfg.floppy.drives.iter().all(Option::is_none));
    Ok(())
}

#[test]
fn cd_profiles_default_to_no_floppy_drives() -> Result<()> {
    for profile in ["CDTV", "CD32"] {
        let cfg = parse_config(&format!("[machine]\nprofile = \"{profile}\"\n"))?;
        assert_eq!(cfg.floppy_connected, [false; 4], "{profile}");
    }
    assert_eq!(
        parse_config("[machine]\nprofile = \"A500\"\n")?.floppy_connected,
        [true, false, false, false]
    );
    Ok(())
}

#[test]
fn floppy_speed_defaults_and_parses_supported_values() -> Result<()> {
    assert_eq!(parse_config("")?.floppy.speed, 100);
    for speed in [100u16, 200, 400, 800, 0] {
        let cfg = parse_config(&format!("[floppy]\nspeed = {speed}\n"))?;
        assert_eq!(cfg.floppy.speed, speed);
    }
    Ok(())
}

#[test]
fn floppy_speed_rejects_unsupported_values() {
    for speed in [50, 150, 300, 1600] {
        let err = parse_config(&format!("[floppy]\nspeed = {speed}\n")).unwrap_err();
        assert!(
            err.to_string().contains("[floppy] speed"),
            "unexpected error for speed {speed}: {err}"
        );
    }
}

#[test]
fn floppy_speed_cli_override_reaches_config() -> Result<()> {
    let cfg = load_overrides(&ConfigOverrides {
        floppy_speed: Some(0),
        ..Default::default()
    })?;
    assert_eq!(cfg.floppy.speed, 0);
    Ok(())
}

#[test]
fn cpu_clock_defaults_per_model_and_converts_to_cck_multiple() {
    assert_eq!(CpuModel::M68000.default_clock_mhz(), 7.09);
    assert_eq!(CpuModel::M68020.default_clock_mhz(), 14.0);
    assert_eq!(CpuModel::M68040.default_clock_mhz(), 25.0);
    // Whole multiples of the colour clock ("multiples of the bus").
    assert_eq!(clocks_per_cck_for_mhz(7.09), 2);
    assert_eq!(clocks_per_cck_for_mhz(14.0), 4);
    assert_eq!(clocks_per_cck_for_mhz(25.0), 7);
    // Never zero.
    assert_eq!(clocks_per_cck_for_mhz(0.5), 1);
}

#[test]
fn cpu_68060_parses_with_full_defaults() -> Result<()> {
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68060"
            "#,
    )?;
    assert_eq!(cfg.cpu, CpuModel::M68060);
    assert_eq!(cfg.cpu_clock_mhz, 50.0, "060 defaults to 50 MHz");
    assert!(cfg.fpu, "the full 68060 has its FPU on-die");
    assert!(cfg.cpu_icache && cfg.cpu_dcache, "8 KB caches default on");
    assert_eq!(cfg.cpu_unimplemented, UnimplementedPolicy::Trap);
    Ok(())
}

#[test]
fn cpu_jit_parses_and_defaults_off() -> Result<()> {
    let cfg = parse_config("")?;
    assert!(!cfg.cpu_jit, "JIT must be opt-in");
    let cfg = parse_config(
        r#"
            [cpu]
            jit = true
            "#,
    )?;
    assert!(cfg.cpu_jit);
    Ok(())
}

#[test]
fn cpu_unimplemented_policy_parses_and_is_68060_only() -> Result<()> {
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68060"
            unimplemented = "native"
            "#,
    )?;
    assert_eq!(cfg.cpu_unimplemented, UnimplementedPolicy::Native);

    let err = parse_config(
        r#"
            [cpu]
            model = "68040"
            unimplemented = "native"
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("applies only to the 68060"),
        "{err}"
    );

    let err = parse_config(
        r#"
            [cpu]
            model = "68060"
            unimplemented = "sometimes"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("trap"), "{err}");
    Ok(())
}

#[test]
fn cpu_clock_override_is_honoured_and_validated() -> Result<()> {
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68020"
            clock_mhz = 28.0
            "#,
    )?;
    assert_eq!(cfg.cpu, CpuModel::M68020);
    assert_eq!(cfg.cpu_clock_mhz, 28.0);
    // Default applies when unset.
    let cfg = parse_config(
        r#"[cpu]
            model = "68040""#,
    )?;
    assert_eq!(cfg.cpu_clock_mhz, 25.0);
    // Non-positive is rejected.
    assert!(parse_config("[cpu]\nclock_mhz = 0.0").is_err());
    Ok(())
}

#[test]
fn floppy_paths_playlist_is_parsed_in_order() -> Result<()> {
    let disk1 = temp_adf()?;
    let disk2 = temp_adf()?;
    let cfg = parse_config(&format!(
        r#"
            [floppy.df0]
            paths = ["{}", "{}"]
            write_protected = false
            "#,
        toml_path(&disk1),
        toml_path(&disk2),
    ))?;
    // The boot disk is the first playlist entry.
    let df0 = cfg.floppy.drives[0].as_ref().unwrap();
    assert_eq!(df0.path, disk1);
    assert!(!df0.write_protected);
    // The full playlist is exposed in order for the swap key.
    assert_eq!(cfg.floppy_playlists[0], vec![disk1, disk2]);
    assert!(cfg.floppy_playlists[1].is_empty());
    Ok(())
}

#[test]
fn floppy_single_path_yields_one_entry_playlist() -> Result<()> {
    let adf = temp_adf()?;
    let cfg = parse_config(&format!(
        r#"
            [floppy.df0]
            path = "{}"
            "#,
        toml_path(&adf)
    ))?;
    assert_eq!(cfg.floppy_playlists[0], vec![adf]);
    Ok(())
}

#[test]
fn dms_floppy_path_is_accepted() -> Result<()> {
    let dms = temp_path("test.dms");
    fs::write(&dms, b"DMS!test placeholder")?;
    let cfg = parse_config(&format!(
        r#"
            [floppy.df0]
            path = "{}"
            "#,
        toml_path(&dms)
    ))?;
    let df0 = cfg.floppy.drives[0].as_ref().unwrap();
    assert_eq!(df0.path, dms);
    assert!(df0.write_protected);
    let _ = fs::remove_file(df0.path.clone());
    Ok(())
}

#[test]
fn adz_floppy_path_is_accepted() -> Result<()> {
    let adz = temp_path("test.adz");
    fs::write(&adz, [0x1F, 0x8B, 8, 0, 0, 0, 0, 0])?;
    let cfg = parse_config(&format!(
        r#"
            [floppy.df0]
            path = "{}"
            "#,
        toml_path(&adz)
    ))?;
    let df0 = cfg.floppy.drives[0].as_ref().unwrap();
    assert_eq!(df0.path, adz);
    assert!(df0.write_protected);
    let _ = fs::remove_file(df0.path.clone());
    Ok(())
}

#[test]
fn uae_extended_adf_floppy_path_is_accepted() -> Result<()> {
    let adf = temp_path("test.ext.adf");
    let mut image = Vec::new();
    image.extend_from_slice(b"UAE-1ADF");
    image.extend_from_slice(&0u16.to_be_bytes());
    image.extend_from_slice(&0u16.to_be_bytes());
    fs::write(&adf, image)?;
    let cfg = parse_config(&format!(
        r#"
            [floppy.df0]
            path = "{}"
            "#,
        toml_path(&adf)
    ))?;
    let df0 = cfg.floppy.drives[0].as_ref().unwrap();
    assert_eq!(df0.path, adf);
    let _ = fs::remove_file(df0.path.clone());
    Ok(())
}

#[test]
fn ipf_floppy_path_is_accepted() -> Result<()> {
    let ipf = temp_path("test.ipf");
    // The bare CAPS signature chunk an IPF opens with.
    fs::write(&ipf, b"CAPS\x00\x00\x00\x0c")?;
    let cfg = parse_config(&format!(
        r#"
            [floppy.df0]
            path = "{}"
            "#,
        toml_path(&ipf)
    ))?;
    let df0 = cfg.floppy.drives[0].as_ref().unwrap();
    assert_eq!(df0.path, ipf);
    assert!(df0.write_protected);
    let _ = fs::remove_file(df0.path.clone());
    Ok(())
}

#[test]
fn scp_floppy_path_is_accepted() -> Result<()> {
    let scp = temp_path("test.scp");
    fs::write(&scp, b"SCP\x25\x04\x01\x00\x00")?;
    let cfg = parse_config(&format!(
        r#"
            [floppy.df0]
            path = "{}"
            "#,
        toml_path(&scp)
    ))?;
    let df0 = cfg.floppy.drives[0].as_ref().unwrap();
    assert_eq!(df0.path, scp);
    assert!(df0.write_protected);
    let _ = fs::remove_file(df0.path.clone());
    Ok(())
}

#[test]
fn disabled_floppy_ignores_missing_path() -> Result<()> {
    let cfg = parse_config(
        r#"
            [floppy.df1]
            enabled = false
            "#,
    )?;
    assert!(cfg.floppy.drives[1].is_none());
    Ok(())
}

#[test]
fn floppy_image_connects_external_drive_without_count() -> Result<()> {
    let adf = temp_adf()?;
    let cfg = parse_config(&format!(
        r#"
            [floppy.df1]
            path = "{}"
            "#,
        toml_path(&adf)
    ))?;
    assert_eq!(cfg.floppy_connected, [true, true, false, false]);
    Ok(())
}

#[test]
fn floppy_drive_count_allows_zero_and_rejects_media_beyond_connected_slots() -> Result<()> {
    let adf = temp_adf()?;
    let err = parse_config(&format!(
        r#"
            [floppy]
            drives = 1
            [floppy.df1]
            path = "{}"
            "#,
        toml_path(&adf)
    ))
    .unwrap_err();
    assert!(
        err.to_string().contains("leaves floppy.df1 disconnected"),
        "{err:#}"
    );
    let cfg = parse_config("[floppy]\ndrives = 0")?;
    assert_eq!(cfg.floppy_connected, [false; 4]);

    let err = parse_config(&format!(
        "[floppy]\ndrives = 0\n[floppy.df0]\npath = {:?}\n",
        adf.to_string_lossy()
    ))
    .unwrap_err();
    assert!(
        err.to_string().contains("leaves floppy.df0 disconnected"),
        "{err:#}"
    );
    Ok(())
}

#[test]
fn rtg_card_selects_the_board() -> Result<()> {
    // The board is Zorro III, so these need a 32-bit-bus CPU.
    let with_cpu = |rtg: &str| format!("[cpu]\nmodel = \"68030\"\n{rtg}");
    assert_eq!(
        parse_config(&with_cpu("[rtg]\ncard = \"z3660\"\n"))?.rtg,
        RtgCard::Z3660
    );
    // Spelling and spacing are forgiving, as for [scsi] controller.
    assert_eq!(
        parse_config(&with_cpu("[rtg]\ncard = \" Z3660 \"\n"))?.rtg,
        RtgCard::Z3660
    );
    assert_eq!(
        parse_config(&with_cpu("[rtg]\ncard = \"none\"\n"))?.rtg,
        RtgCard::None
    );
    // A bare config is a 68000 machine, which cannot host a Zorro III
    // board, so nothing is fitted.
    assert_eq!(parse_config("")?.rtg, RtgCard::None);

    // Picasso II is a Zorro II card and remains valid on the default
    // 68000 machine. Its fitted memory is part of the resolved config.
    let picasso = parse_config("[rtg]\ncard = \" Picasso2 \"\nvram = \"1M\"\n")?;
    assert_eq!(picasso.rtg, RtgCard::Picasso2);
    assert_eq!(picasso.rtg_vram_bytes, 1024 * 1024);
    let picasso = parse_config("[rtg]\ncard = \"picasso2\"\n")?;
    assert_eq!(picasso.rtg_vram_bytes, 2 * 1024 * 1024);
    let plus = parse_config("[rtg]\ncard = \" Picasso2Plus \"\nvram = \"1M\"\n")?;
    assert_eq!(plus.rtg, RtgCard::Picasso2Plus);
    assert_eq!(plus.rtg_vram_bytes, 1024 * 1024);
    let plus_alias = parse_config("[rtg]\ncard = \"picasso2+\"\n")?;
    assert_eq!(plus_alias.rtg, RtgCard::Picasso2Plus);
    Ok(())
}

#[test]
fn picasso2_rejects_non_hardware_vram_sizes() {
    let err = parse_config("[rtg]\ncard = \"picasso2\"\nvram = \"3M\"\n").unwrap_err();
    assert!(
        err.to_string().contains("must be \"1M\" or \"2M\""),
        "{err:#}"
    );
    let err = parse_config("[rtg]\ncard = \"picasso2plus\"\nvram = \"3M\"\n").unwrap_err();
    assert!(
        err.to_string().contains("must be \"1M\" or \"2M\""),
        "{err:#}"
    );
}

#[test]
fn rtg_vram_is_ignored_by_non_picasso_cards() {
    let cfg = parse_config("[rtg]\ncard = \"none\"\nvram = \"oops\"\n").unwrap();
    assert_eq!(cfg.rtg, RtgCard::None);
    assert_eq!(cfg.rtg_vram_bytes, 2 * 1024 * 1024);
    let cfg = parse_config("[cpu]\nmodel = \"68030\"\n\n[rtg]\ncard = \"z3660\"\nvram = \"3M\"\n")
        .unwrap();
    assert_eq!(cfg.rtg, RtgCard::Z3660);
    assert_eq!(cfg.rtg_vram_bytes, 2 * 1024 * 1024);
}

/// A machine that can host a Zorro III board gets one fitted by default,
/// so RTG needs no config beyond the guest driver. The gate is the CPU's
/// address bus, the same one Zorro III RAM uses, not a model list.
#[test]
fn rtg_card_defaults_to_the_machine_capability() -> Result<()> {
    assert_eq!(
        parse_config("[machine]\nprofile = \"A4000\"\n")?.rtg,
        RtgCard::Z3660
    );
    assert_eq!(
        parse_config("[machine]\nprofile = \"A3000\"\n")?.rtg,
        RtgCard::Z3660
    );
    // 68EC020: 24-bit bus, so no Zorro III and no card.
    assert_eq!(
        parse_config("[machine]\nprofile = \"A1200\"\n")?.rtg,
        RtgCard::None
    );
    assert_eq!(
        parse_config("[machine]\nprofile = \"A500\"\n")?.rtg,
        RtgCard::None
    );
    // Asking anyway is an error rather than a board the CPU cannot reach.
    let err = parse_config("[machine]\nprofile = \"A500\"\n[rtg]\ncard = \"z3660\"\n").unwrap_err();
    assert!(err.to_string().contains("32-bit address bus"), "{err:#}");
    Ok(())
}

#[test]
fn unknown_rtg_card_fails_cleanly() {
    let err = parse_config("[rtg]\ncard = \"picasso4\"\n").unwrap_err();
    assert!(err.to_string().contains("is not known"), "{err:#}");
}

#[test]
fn enabled_floppy_requires_path() {
    let err = parse_config(
        r#"
            [floppy.df0]
            enabled = true
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("has no path"), "{err:#}");
}

#[test]
fn bad_floppy_size_fails_cleanly() -> Result<()> {
    let path = temp_path("bad.adf");
    fs::write(&path, [0u8; 512])?;
    let err = parse_config(&format!(
        r#"
            [floppy.df0]
            path = "{}"
            "#,
        toml_path(&path)
    ))
    .unwrap_err();
    let _ = fs::remove_file(&path);
    assert!(err.to_string().contains("expected 901120 bytes"), "{err:#}");
    Ok(())
}

#[test]
fn cli_overrides_select_a_machine_with_no_config_file() -> Result<()> {
    let overrides = ConfigOverrides {
        model: Some("A1200".to_string()),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(cfg.machine, Some(MachineModel::A1200));
    assert_eq!(cfg.cpu, CpuModel::M68EC020);
    assert_eq!(cfg.chipset, Chipset::Aga);
    assert_eq!(cfg.chip_ram_bytes, 2 * 1024 * 1024);
    Ok(())
}

#[test]
fn cli_overrides_layer_on_top_of_a_profile() -> Result<()> {
    // A model plus explicit CPU/fast-RAM overrides: the profile supplies
    // the chipset and chip RAM, the overrides win where they are set, and
    // everything still goes through the normal validation/derivation.
    let overrides = ConfigOverrides {
        model: Some("A500".to_string()),
        cpu: Some("68020".to_string()),
        fpu: Some(true),
        cpu_clock_mhz: Some(28.0),
        fast: Some("4M".to_string()),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(cfg.machine, Some(MachineModel::A500));
    assert_eq!(cfg.cpu, CpuModel::M68020);
    assert!(cfg.fpu);
    assert_eq!(cfg.cpu_clock_mhz, 28.0);
    assert_eq!(cfg.fast_ram_bytes, 4 * 1024 * 1024);
    assert_eq!(cfg.slow_ram_bytes, 512 * 1024);
    Ok(())
}

#[test]
fn cli_floppy_drive_override_uses_config_validation() -> Result<()> {
    let overrides = ConfigOverrides {
        floppy_drives: Some(0),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(cfg.floppy_connected, [false; 4]);

    let overrides = ConfigOverrides {
        floppy_drives: Some(4),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(cfg.floppy_connected, [true, true, true, true]);

    let overrides = ConfigOverrides {
        floppy_drives: Some(5),
        ..Default::default()
    };
    let err = load_overrides(&overrides).unwrap_err();
    assert!(err.to_string().contains("between 0 and 4"), "{err:#}");
    Ok(())
}

#[test]
fn serial_defaults_to_stdout() -> Result<()> {
    // An unconfigured machine keeps the historical terminal output.
    let cfg = parse_config("")?;
    assert_eq!(cfg.serial.mode, SerialMode::Stdout);
    assert_eq!(Config::default().serial.mode, SerialMode::Stdout);
    Ok(())
}

#[test]
fn serial_section_selects_mode_and_midi_endpoints() -> Result<()> {
    let cfg = parse_config(
        "[serial]\nmode = \"midi\"\nmidi_out = \"USB MIDI\"\nmidi_in = \"USB MIDI\"\n",
    )?;
    assert_eq!(cfg.serial.mode, SerialMode::Midi);
    assert_eq!(cfg.serial.midi_out.as_deref(), Some("USB MIDI"));
    assert_eq!(cfg.serial.midi_in.as_deref(), Some("USB MIDI"));

    let err = parse_config("[serial]\nmode = \"rs232\"\n").unwrap_err();
    assert!(err.to_string().contains("unknown [serial] mode"), "{err:#}");
    Ok(())
}

#[test]
fn serial_section_carries_the_coppersynth_keys() -> Result<()> {
    // All three keys parse under [serial], like the MT-32's.
    let text = "[serial]\nmode = \"midi\"\nmidi_out = \"coppersynth\"\n\
                    coppersynth_soundfont = \"bank.sf2\"\n\
                    coppersynth_mt32_mode = \"on\"\ncoppersynth_panel = true\n";
    let cfg = parse_config(text)?;
    assert_eq!(
        cfg.serial.coppersynth_soundfont.as_deref(),
        Some(std::path::Path::new("bank.sf2"))
    );
    assert_eq!(cfg.serial.coppersynth_mt32_mode.as_deref(), Some("on"));
    assert!(cfg.serial.coppersynth_panel);

    // They serialize back under [serial] and survive the round trip
    // -- the text a launcher Save writes must load again.
    let raw: RawConfig = toml::from_str(text)?;
    let written = raw.to_toml_string()?;
    for key in [
        "coppersynth_soundfont",
        "coppersynth_mt32_mode",
        "coppersynth_panel",
    ] {
        assert!(written.contains(key), "{key} missing from:\n{written}");
    }
    let reloaded = parse_config(&written)?;
    assert_eq!(reloaded.serial.coppersynth_mt32_mode.as_deref(), Some("on"));

    // The rejection names the key exactly as [serial] spells it, so
    // a launcher-saved config that goes stale says where to look.
    let err = parse_config("[serial]\ncoppersynth_mt32_mode = \"maybe\"\n").unwrap_err();
    assert!(err.to_string().contains("coppersynth_mt32_mode"), "{err:#}");
    Ok(())
}

#[test]
fn serial_section_selects_tcp_connect_and_address() -> Result<()> {
    let cfg =
        parse_config("[serial]\nmode = \"tcp-connect\"\nconnect = \"bbs.example.com:1337\"\n")?;
    assert_eq!(cfg.serial.mode, SerialMode::TcpConnect);
    assert_eq!(cfg.serial.connect.as_deref(), Some("bbs.example.com:1337"));
    Ok(())
}

#[test]
fn serial_section_selects_modem_mode_and_telnet_default() -> Result<()> {
    let cfg = parse_config("[serial]\nmode = \"modem\"\ntelnet = true\n")?;
    assert_eq!(cfg.serial.mode, SerialMode::Modem);
    assert_eq!(cfg.serial.telnet, Some(true));
    // An explicit off stays distinguishable from unset, so it can beat a
    // stored profile's own AT*T rather than being read as "no opinion".
    let cfg = parse_config("[serial]\nmode = \"modem\"\ntelnet = false\n")?;
    assert_eq!(cfg.serial.telnet, Some(false));
    // Unset by default.
    let cfg = parse_config("[serial]\nmode = \"modem\"\n")?;
    assert_eq!(cfg.serial.telnet, None);
    Ok(())
}

#[test]
fn serial_telnet_and_phonebook_are_rejected_outside_modem_mode() -> Result<()> {
    let err = parse_config("[serial]\nmode = \"stdout\"\ntelnet = true\n").unwrap_err();
    assert!(err.to_string().contains("mode = \"modem\""), "{err:#}");

    let err = parse_config(
        "[serial]\nmode = \"tcp\"\n[serial.phonebook]\n\"555-1234\" = \"bbs.example.com:23\"\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("mode = \"modem\""), "{err:#}");
    Ok(())
}

#[test]
fn serial_phonebook_parses_and_sorts_by_number() -> Result<()> {
    let text = "[serial]\nmode = \"modem\"\n[serial.phonebook]\n\
                \"555-2000\" = \"bbs2.example.com:2323\"\n\
                \"555-1000\" = \"bbs1.example.com\"\n";
    let cfg = parse_config(text)?;
    assert_eq!(
        cfg.serial.phonebook,
        vec![
            ("555-1000".to_string(), "bbs1.example.com".to_string()),
            ("555-2000".to_string(), "bbs2.example.com:2323".to_string()),
        ]
    );

    // Round-trips through a Save.
    let raw: RawConfig = toml::from_str(text)?;
    let written = raw.to_toml_string()?;
    assert!(written.contains("555-1000"), "{written}");
    let reloaded = parse_config(&written)?;
    assert_eq!(reloaded.serial.phonebook, cfg.serial.phonebook);
    Ok(())
}

#[test]
fn serial_phonebook_rejects_a_bad_number() -> Result<()> {
    let err = parse_config(
        "[serial]\nmode = \"modem\"\n[serial.phonebook]\n\"call mom\" = \"host:23\"\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("call mom"), "{err:#}");
    assert!(err.to_string().contains("phonebook"), "{err:#}");
    Ok(())
}

#[test]
fn serial_phonebook_rejects_a_bad_host_port() -> Result<()> {
    let err = parse_config(
        "[serial]\nmode = \"modem\"\n[serial.phonebook]\n\"555-1234\" = \"host:not-a-port\"\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("not-a-port"), "{err:#}");

    // A bare host with no colon is fine (the modem's default port applies).
    let cfg = parse_config(
        "[serial]\nmode = \"modem\"\n[serial.phonebook]\n\"555-1234\" = \"bbs.example.com\"\n",
    )?;
    assert_eq!(
        cfg.serial.phonebook,
        vec![("555-1234".to_string(), "bbs.example.com".to_string())]
    );

    // An empty value is rejected too.
    let err = parse_config("[serial]\nmode = \"modem\"\n[serial.phonebook]\n\"555-1234\" = \"\"\n")
        .unwrap_err();
    assert!(err.to_string().contains("empty"), "{err:#}");
    Ok(())
}

#[test]
fn cli_serial_connect_implies_tcp_connect_mode() -> Result<()> {
    // Like --midi-out implying midi mode: naming a dial-out address is
    // enough, unless --serial explicitly chose another mode.
    let overrides = ConfigOverrides {
        serial_connect: Some("bbs.example.com:1337".to_string()),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(cfg.serial.mode, SerialMode::TcpConnect);
    assert_eq!(cfg.serial.connect.as_deref(), Some("bbs.example.com:1337"));

    let overrides = ConfigOverrides {
        serial: Some("off".to_string()),
        serial_connect: Some("bbs.example.com:1337".to_string()),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(cfg.serial.mode, SerialMode::Off);
    assert_eq!(cfg.serial.connect.as_deref(), Some("bbs.example.com:1337"));
    Ok(())
}

#[test]
fn parallel_section_selects_raw_capture_path() -> Result<()> {
    // A bare output path implies the printer (back-compat).
    let cfg = parse_config("[parallel]\noutput = \"printer.raw\"\n")?;
    assert_eq!(cfg.parallel.device, ParallelDevice::Printer);
    assert_eq!(
        cfg.parallel.printer_output.as_deref(),
        Some(std::path::Path::new("printer.raw"))
    );
    // An empty port is the default.
    assert_eq!(parse_config("")?.parallel.device, ParallelDevice::None);
    assert_eq!(parse_config("")?.parallel.printer_output, None);

    let err = parse_config("[parallel]\nmode = \"printer\"\n").unwrap_err();
    assert!(err.to_string().contains("unknown field `mode`"), "{err:#}");
    Ok(())
}

#[test]
fn parallel_device_selects_printer_or_sampler() -> Result<()> {
    // An explicit sampler with its options (gain in dB).
    let cfg = parse_config(
        "[parallel]\ndevice = \"sampler\"\nsampler_input = \"BlackHole\"\nsampler_gain = 6.0\n",
    )?;
    assert_eq!(cfg.parallel.device, ParallelDevice::Sampler);
    assert_eq!(cfg.parallel.sampler_input.as_deref(), Some("BlackHole"));
    assert_eq!(cfg.parallel.sampler_gain_db, 6.0);
    // 0 dB (unity) is valid.
    assert_eq!(
        parse_config("[parallel]\ndevice = \"sampler\"\nsampler_gain = 0\n")?
            .parallel
            .sampler_gain_db,
        0.0
    );

    // An explicit printer needs an output path.
    let err = parse_config("[parallel]\ndevice = \"printer\"\n").unwrap_err();
    assert!(err.to_string().contains("needs an output path"), "{err:#}");

    // Out-of-range gain (dB) is rejected.
    let err = parse_config("[parallel]\ndevice = \"sampler\"\nsampler_gain = 100\n").unwrap_err();
    assert!(err.to_string().contains("sampler_gain"), "{err:#}");

    // An unknown device name is rejected.
    let err = parse_config("[parallel]\ndevice = \"plotter\"\n").unwrap_err();
    assert!(err.to_string().contains("must be"), "{err:#}");

    // `none` explicitly empties the port even with a stale output path.
    let cfg = parse_config("[parallel]\ndevice = \"none\"\n")?;
    assert_eq!(cfg.parallel.device, ParallelDevice::None);
    Ok(())
}

#[test]
fn audio_section_selects_output_device() -> Result<()> {
    let cfg = parse_config("[audio]\noutput_device = \"External Speakers\"\n")?;
    assert_eq!(
        cfg.audio.output_device.as_deref(),
        Some("External Speakers")
    );

    // A blank name means "use the system default".
    let cfg = parse_config("[audio]\noutput_device = \"  \"\n")?;
    assert_eq!(cfg.audio.output_device, None);

    // Omitting it entirely is the default.
    assert_eq!(parse_config("")?.audio.output_device, None);
    // A pre-existing [audio] block that never mentions output_device still
    // parses and leaves it None (system default) -- older configs are safe.
    let cfg = parse_config("[audio]\nfloppy_sounds = true\nfloppy_sounds_volume = 80\n")?;
    assert_eq!(cfg.audio.output_device, None);
    Ok(())
}

#[test]
fn audio_output_enabled_defaults_true_and_parses() -> Result<()> {
    // Default and older configs (no key) stay enabled.
    assert!(parse_config("")?.audio.output_enabled);
    assert!(
        parse_config("[audio]\noutput_device = \"Speakers\"\n")?
            .audio
            .output_enabled
    );
    // The GUI "Disabled" option persists as output_enabled = false.
    assert!(
        !parse_config("[audio]\noutput_enabled = false\n")?
            .audio
            .output_enabled
    );
    assert!(
        parse_config("[audio]\noutput_enabled = true\n")?
            .audio
            .output_enabled
    );
    Ok(())
}

#[test]
fn cli_audio_device_overrides_config() -> Result<()> {
    let overrides = ConfigOverrides {
        audio_device: Some("BlackHole".to_string()),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(cfg.audio.output_device.as_deref(), Some("BlackHole"));
    Ok(())
}

#[test]
fn audio_channel_mode_defaults_to_stereo_and_parses() -> Result<()> {
    assert_eq!(parse_config("")?.audio.channel_mode, ChannelMode::Stereo);
    assert_eq!(
        parse_config("[audio]\nchannel_mode = \"mono\"\n")?
            .audio
            .channel_mode,
        ChannelMode::Mono
    );
    assert_eq!(
        parse_config("[audio]\nchannel_mode = \"STEREO\"\n")?
            .audio
            .channel_mode,
        ChannelMode::Stereo
    );
    assert!(parse_config("[audio]\nchannel_mode = \"quad\"\n").is_err());

    // CLI override.
    let overrides = ConfigOverrides {
        audio_channel_mode: Some("mono".to_string()),
        ..Default::default()
    };
    assert_eq!(
        load_overrides(&overrides)?.audio.channel_mode,
        ChannelMode::Mono
    );
    Ok(())
}

#[test]
fn audio_filter_defaults_to_auto_and_parses() -> Result<()> {
    assert_eq!(parse_config("")?.audio.filter, AudioFilterMode::Auto);
    assert_eq!(
        parse_config("[audio]\naudio_filter = \"on\"\n")?
            .audio
            .filter,
        AudioFilterMode::On
    );
    assert_eq!(
        parse_config("[audio]\naudio_filter = \"OFF\"\n")?
            .audio
            .filter,
        AudioFilterMode::Off
    );
    assert_eq!(
        parse_config("[audio]\naudio_filter = \"disabled\"\n")?
            .audio
            .filter,
        AudioFilterMode::Off
    );
    assert!(parse_config("[audio]\naudio_filter = \"sometimes\"\n").is_err());
    // `filter` is accepted as an alias for `audio_filter`.
    assert_eq!(
        parse_config("[audio]\nfilter = \"off\"\n")?.audio.filter,
        AudioFilterMode::Off
    );

    // CLI override.
    let overrides = ConfigOverrides {
        audio_filter: Some("on".to_string()),
        ..Default::default()
    };
    assert_eq!(
        load_overrides(&overrides)?.audio.filter,
        AudioFilterMode::On
    );
    Ok(())
}

#[test]
fn audio_stem_granularity_defaults_to_none_and_parses() -> Result<()> {
    use crate::audio::mux::StemGranularity;
    assert_eq!(parse_config("")?.audio.stem_granularity, None);
    assert_eq!(
        parse_config("[audio]\nstem_granularity = \"master,source\"\n")?
            .audio
            .stem_granularity,
        Some(vec![StemGranularity::Master, StemGranularity::Source])
    );
    assert!(parse_config("[audio]\nstem_granularity = \"bogus\"\n").is_err());
    assert!(parse_config("[audio]\nstem_granularity = \"\"\n").is_err());
    Ok(())
}

#[test]
fn audio_stereo_separation_defaults_to_100_and_validates() -> Result<()> {
    assert_eq!(parse_config("")?.audio.stereo_separation, 100);
    assert_eq!(
        parse_config("[audio]\nstereo_separation = 0\n")?
            .audio
            .stereo_separation,
        0
    );
    assert_eq!(
        parse_config("[audio]\nstereo_separation = 60\n")?
            .audio
            .stereo_separation,
        60
    );
    assert!(parse_config("[audio]\nstereo_separation = 150\n").is_err());

    let overrides = ConfigOverrides {
        audio_stereo_separation: Some(20),
        ..Default::default()
    };
    assert_eq!(load_overrides(&overrides)?.audio.stereo_separation, 20);
    Ok(())
}

#[test]
fn cli_midi_endpoint_implies_midi_mode() -> Result<()> {
    // Naming an endpoint is enough to switch the serial port to MIDI.
    let overrides = ConfigOverrides {
        midi_out: Some("Deluge".to_string()),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(cfg.serial.mode, SerialMode::Midi);
    assert_eq!(cfg.serial.midi_out.as_deref(), Some("Deluge"));

    // An explicit --serial still wins over the implication.
    let overrides = ConfigOverrides {
        serial: Some("stdout".to_string()),
        midi_in: Some("Deluge".to_string()),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(cfg.serial.mode, SerialMode::Stdout);
    Ok(())
}

#[test]
fn a2065_bridge_requires_and_preserves_interface() -> Result<()> {
    let cfg = parse_config(
        r#"
            [a2065]
            net = "bridge"
            interface = "en-test"
            "#,
    )?;
    assert_eq!(
        cfg.a2065_net,
        Some(crate::net::NetConfig::Bridge {
            interface: "en-test".to_string()
        })
    );

    let missing = parse_config("[a2065]\nnet = \"bridge\"\n").unwrap_err();
    assert!(
        missing.to_string().contains("needs an interface"),
        "{missing:#}"
    );
    let stray = parse_config("[a2065]\ninterface = \"en-test\"\n").unwrap_err();
    assert!(stray.to_string().contains("needs net"), "{stray:#}");
    let conflict = parse_config("[a2065]\nnet = \"nat\"\ninterface = \"en-test\"\n").unwrap_err();
    assert!(
        conflict.to_string().contains("applies only"),
        "{conflict:#}"
    );

    let overrides = ConfigOverrides {
        a2065_interface: Some("eth-test".to_string()),
        ..Default::default()
    };
    assert_eq!(
        load_overrides(&overrides)?.a2065_net,
        Some(crate::net::NetConfig::Bridge {
            interface: "eth-test".to_string()
        })
    );

    // Replacing a file's bridge backend from the CLI also clears the
    // now-inapplicable carried interface.
    let mut raw: RawConfig =
        toml::from_str("[a2065]\nnet = \"bridge\"\ninterface = \"en-test\"\n")?;
    ConfigOverrides {
        a2065_net: Some("nat".to_string()),
        ..Default::default()
    }
    .apply_to(&mut raw);
    assert!(raw.a2065.interface.is_none());
    assert_eq!(
        Config::try_from(raw)?.a2065_net,
        Some(crate::net::NetConfig::Nat)
    );
    Ok(())
}

#[test]
fn hostsocket_expands_to_the_bundled_wasm_board() -> Result<()> {
    // No [hostsocket] section, no board.
    let cfg = parse_config("")?;
    assert!(cfg.wasm_boards.is_empty());

    let cfg = parse_config(
        r#"
            [hostsocket]
            net = "loopback"
            hostname = "workbench"
            "#,
    )?;
    assert_eq!(cfg.wasm_boards.len(), 1);
    let board = &cfg.wasm_boards[0];
    assert_eq!(
        board.wasm_path,
        Path::new(crate::hostsocket::BUNDLED_HOSTSOCKET_WASM)
    );
    assert_eq!(
        board.spec.manufacturer,
        crate::zorro::COPPERLINE_MANUFACTURER_ID
    );
    assert_eq!(board.spec.diag_vec, Some(crate::hostsocket::DIAG_OFFSET));
    assert!(board.manifest.caps.dma && board.manifest.caps.net);
    assert_eq!(board.manifest.net, crate::net::NetConfig::Loopback);
    assert_eq!(
        board.manifest.config.get("rom").map(String::as_str),
        Some(crate::hostsocket::BUNDLED_HOSTSOCKET_ROM)
    );
    assert_eq!(
        board.manifest.config.get("dns_server").map(String::as_str),
        Some(crate::hostsocket::DEFAULT_DNS_SERVER)
    );
    assert_eq!(
        board.manifest.config.get("hostname").map(String::as_str),
        Some("workbench")
    );
    assert_eq!(board.manifest.file_keys, vec!["rom".to_string()]);
    // address/gateway are bridge-only and left unset here, so they
    // must not appear in the manifest at all -- the plugin's own
    // nat/loopback-shaped defaults (INTERFACE_ADDR/NAT_GATEWAY_ADDR)
    // must apply, not an empty-string override.
    assert!(!board.manifest.config.contains_key("address"));
    assert!(!board.manifest.config.contains_key("gateway"));

    // The bundled board composes with [[zorro]] metadata boards; it is
    // appended after them, so their windows are assigned first.
    Ok(())
}

#[test]
fn zz9k_expands_to_the_bundled_wasm_board() -> Result<()> {
    // No [zz9k] section, no board.
    let cfg = parse_config("")?;
    assert!(cfg.wasm_boards.is_empty());

    // On the default 68000 machine the board auto-selects Zorro II,
    // pinned to the 4M window the SDK transport requires there.
    let cfg = parse_config("[zz9k]\nenabled = true\n")?;
    assert_eq!(cfg.wasm_boards.len(), 1);
    let board = &cfg.wasm_boards[0];
    assert_eq!(board.wasm_path, Path::new(crate::zz9k::BUNDLED_ZZ9K_WASM));
    assert_eq!(
        board.spec.manufacturer,
        crate::zorro::ZZ9K_MNT_MANUFACTURER_ID
    );
    assert_eq!(board.spec.product, crate::zorro::ZZ9K_PRODUCT_Z2);
    assert_eq!(board.spec.size_bytes, crate::zz9k::Z2_BOARD_SIZE);
    assert_eq!(board.spec.diag_vec, None);
    // Pure compute: no DMA, no network -- the deterministic profile.
    assert!(!board.manifest.caps.dma && !board.manifest.caps.net);
    assert!(!board.manifest.caps.resolve && !board.manifest.caps.host_sockets);
    assert_eq!(board.manifest.net, crate::net::NetConfig::None);
    assert_eq!(
        board.manifest.config.get("size").map(String::as_str),
        Some("4194304")
    );
    assert_eq!(
        board.manifest.config.get("int2").map(String::as_str),
        Some("0")
    );
    assert!(!board.manifest.config.contains_key("seed"));

    // A 32-bit CPU auto-selects Zorro III (product 4), and the
    // int2/size/seed knobs reach the manifest.
    let cfg = parse_config(
        r#"
            [cpu]
            model = "68030"
            [zz9k]
            enabled = true
            size = "16M"
            int2 = true
            seed = "00ff"
            "#,
    )?;
    let board = &cfg.wasm_boards[0];
    assert_eq!(board.spec.product, crate::zorro::ZZ9K_PRODUCT_Z3);
    assert_eq!(board.spec.size_bytes, 16 * 1024 * 1024);
    assert_eq!(
        board.manifest.config.get("int2").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        board.manifest.config.get("seed").map(String::as_str),
        Some("00ff")
    );

    // Explicit zorro = 2 on a 32-bit machine still pins 4M.
    let cfg = parse_config("[cpu]\nmodel = \"68030\"\n[zz9k]\nenabled = true\nzorro = 2\n")?;
    assert_eq!(
        cfg.wasm_boards[0].spec.product,
        crate::zorro::ZZ9K_PRODUCT_Z2
    );

    // Error cases: Z3 on a 24-bit CPU, a non-4M Zorro II window, junk
    // seed, and settings without enabled = true.
    let err = parse_config("[zz9k]\nenabled = true\nzorro = 3\n").unwrap_err();
    assert!(err.to_string().contains("32-bit"), "{err:#}");
    let err = parse_config("[zz9k]\nenabled = true\nzorro = 2\nsize = \"8M\"\n").unwrap_err();
    assert!(err.to_string().contains("fixed at 4M"), "{err:#}");
    let err = parse_config("[zz9k]\nenabled = true\nseed = \"xyz\"\n").unwrap_err();
    assert!(err.to_string().contains("hex"), "{err:#}");
    let err = parse_config("[zz9k]\nint2 = true\n").unwrap_err();
    assert!(err.to_string().contains("enabled = true"), "{err:#}");
    let err = parse_config("[cpu]\nmodel = \"68030\"\n[zz9k]\nenabled = true\nsize = \"3M\"\n")
        .unwrap_err();
    assert!(err.to_string().contains("power of two"), "{err:#}");
    Ok(())
}

#[test]
fn zorro_metadata_boards_reject_the_bundled_zz9k_sentinel() -> Result<()> {
    let meta = temp_path("zz9k-sentinel-board.toml");
    fs::write(
        &meta,
        format!(
            r#"
                name = "Impostor"
                zorro = 2
                type = "wasm"
                size = "64K"
                manufacturer = 2011
                product = 33
                wasm = "{}"
                "#,
            crate::zz9k::BUNDLED_ZZ9K_WASM
        ),
    )?;
    let err =
        parse_config(&format!("[[zorro]]\nmetadata = \"{}\"\n", toml_path(&meta))).unwrap_err();
    assert!(err.to_string().contains("reserved"), "{err:#}");
    let _ = fs::remove_file(&meta);
    Ok(())
}

#[test]
fn hostsocket_bridge_address_and_gateway_reach_the_manifest() -> Result<()> {
    let cfg = parse_config(
        r#"
            [hostsocket]
            net = "bridge"
            interface = "en0"
            address = "192.168.1.50/24"
            gateway = "192.168.1.1"
            "#,
    )?;
    let board = &cfg.wasm_boards[0];
    assert_eq!(
        board.manifest.config.get("address").map(String::as_str),
        Some("192.168.1.50/24")
    );
    assert_eq!(
        board.manifest.config.get("gateway").map(String::as_str),
        Some("192.168.1.1")
    );
    Ok(())
}

#[test]
fn hostsocket_resolver_host_reaches_the_manifest_under_nat_or_bridge() -> Result<()> {
    let cfg = parse_config(
        r#"
            [hostsocket]
            net = "nat"
            resolver = "host"
            "#,
    )?;
    assert_eq!(
        cfg.wasm_boards[0]
            .manifest
            .config
            .get("resolver")
            .map(String::as_str),
        Some("host")
    );

    let cfg = parse_config(
        r#"
            [hostsocket]
            net = "bridge"
            interface = "en0"
            resolver = "HOST"
            "#,
    )?;
    // Normalized to lowercase on the way into the manifest.
    assert_eq!(
        cfg.wasm_boards[0]
            .manifest
            .config
            .get("resolver")
            .map(String::as_str),
        Some("host")
    );
    Ok(())
}

#[test]
fn hostsocket_resolver_defaults_to_host_under_nat_and_bridge_only() -> Result<()> {
    // No explicit `resolver` key anywhere below: nat/bridge should
    // still get "host" (the thing that works without a hand-matched
    // dns_server), while loopback -- where "host" would be rejected
    // outright -- gets no resolver key at all, not a default value
    // that would have failed validation.
    let cfg = parse_config("[hostsocket]\nnet = \"nat\"\n")?;
    assert_eq!(
        cfg.wasm_boards[0]
            .manifest
            .config
            .get("resolver")
            .map(String::as_str),
        Some("host")
    );

    let cfg = parse_config("[hostsocket]\nnet = \"bridge\"\ninterface = \"en0\"\n")?;
    assert_eq!(
        cfg.wasm_boards[0]
            .manifest
            .config
            .get("resolver")
            .map(String::as_str),
        Some("host")
    );

    let cfg = parse_config("[hostsocket]\nnet = \"loopback\"\n")?;
    assert!(!cfg.wasm_boards[0].manifest.config.contains_key("resolver"));

    // An explicit "dns" still opts back out under a backend that would
    // otherwise default to "host" -- e.g. to use a specific dns_server.
    let cfg = parse_config(
        r#"
            [hostsocket]
            net = "nat"
            resolver = "dns"
            dns_server = "1.2.3.4"
            "#,
    )?;
    assert_eq!(
        cfg.wasm_boards[0]
            .manifest
            .config
            .get("resolver")
            .map(String::as_str),
        Some("dns")
    );
    assert_eq!(
        cfg.wasm_boards[0]
            .manifest
            .config
            .get("dns_server")
            .map(String::as_str),
        Some("1.2.3.4")
    );
    Ok(())
}

#[test]
fn hostsocket_resolver_host_rejected_under_loopback_or_bad_value() {
    let err = parse_config(
        r#"
            [hostsocket]
            net = "loopback"
            resolver = "host"
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("needs net = \"nat\", \"bridge\", or \"host\""),
        "{err:#}"
    );

    let err = parse_config(
        r#"
            [hostsocket]
            net = "nat"
            resolver = "carrier-pigeon"
            "#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("is not one of"), "{err:#}");
}

#[test]
fn hostsocket_bridge_requires_and_preserves_interface() -> Result<()> {
    let cfg = parse_config(
        r#"
            [hostsocket]
            net = "bridge"
            interface = "en-test"
            "#,
    )?;
    assert_eq!(
        cfg.wasm_boards[0].manifest.net,
        crate::net::NetConfig::Bridge {
            interface: "en-test".to_string()
        }
    );

    let missing = parse_config("[hostsocket]\nnet = \"bridge\"\n").unwrap_err();
    assert!(
        missing.to_string().contains("needs an interface"),
        "{missing:#}"
    );
    let stray = parse_config("[hostsocket]\ninterface = \"en-test\"\n").unwrap_err();
    assert!(stray.to_string().contains("needs net"), "{stray:#}");
    let conflict =
        parse_config("[hostsocket]\nnet = \"nat\"\ninterface = \"en-test\"\n").unwrap_err();
    assert!(
        conflict.to_string().contains("applies only"),
        "{conflict:#}"
    );

    let overrides = ConfigOverrides {
        hostsocket_interface: Some("eth-test".to_string()),
        ..Default::default()
    };
    assert_eq!(
        load_overrides(&overrides)?.wasm_boards[0].manifest.net,
        crate::net::NetConfig::Bridge {
            interface: "eth-test".to_string()
        }
    );

    // Replacing a file's bridge backend from the CLI also clears the
    // now-inapplicable carried interface.
    let mut raw: RawConfig =
        toml::from_str("[hostsocket]\nnet = \"bridge\"\ninterface = \"en-test\"\n")?;
    ConfigOverrides {
        hostsocket_net: Some("nat".to_string()),
        ..Default::default()
    }
    .apply_to(&mut raw);
    assert!(raw.hostsocket.interface.is_none());
    assert_eq!(
        Config::try_from(raw)?.wasm_boards[0].manifest.net,
        crate::net::NetConfig::Nat
    );
    Ok(())
}

#[test]
fn hostsocket_net_host_selects_the_host_socket_backend() -> Result<()> {
    let cfg = parse_config("[hostsocket]\nnet = \"host\"\n")?;
    let board = &cfg.wasm_boards[0];
    // The underlying smoltcp interface is hardcoded to loopback under
    // "host" -- ICMP/DNS-over-net are the only things still on it,
    // and this mode's whole premise is zero-config, so it isn't
    // user-selectable here the way "bridge" picks a real backend.
    assert_eq!(board.manifest.net, crate::net::NetConfig::Loopback);
    assert!(board.manifest.caps.host_sockets);
    assert_eq!(
        board.manifest.config.get("transport").map(String::as_str),
        Some("host")
    );
    // resolver defaults to "host" here too (loopback's own smoltcp
    // interface couldn't reach a real dns_server anyway).
    assert_eq!(
        board.manifest.config.get("resolver").map(String::as_str),
        Some("host")
    );
    Ok(())
}

#[test]
fn hostsocket_net_host_rejects_interface_address_and_gateway() {
    let err = parse_config("[hostsocket]\nnet = \"host\"\ninterface = \"en0\"\n").unwrap_err();
    assert!(err.to_string().contains("applies only to net"), "{err:#}");

    let err = parse_config(
        r#"
            [hostsocket]
            net = "host"
            address = "192.168.1.50/24"
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("address/gateway don't apply"),
        "{err:#}"
    );

    let err = parse_config(
        r#"
            [hostsocket]
            net = "host"
            gateway = "192.168.1.1"
            "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("address/gateway don't apply"),
        "{err:#}"
    );
}

#[test]
fn hostsocket_net_host_accepts_an_explicit_resolver_override() -> Result<()> {
    // "host" is the default under net = "host" too, but an explicit
    // choice (either value) is still honoured, same as nat/bridge.
    let cfg = parse_config("[hostsocket]\nnet = \"host\"\nresolver = \"dns\"\n")?;
    assert_eq!(
        cfg.wasm_boards[0]
            .manifest
            .config
            .get("resolver")
            .map(String::as_str),
        Some("dns")
    );

    let cfg = parse_config("[hostsocket]\nnet = \"host\"\nresolver = \"host\"\n")?;
    assert_eq!(
        cfg.wasm_boards[0]
            .manifest
            .config
            .get("resolver")
            .map(String::as_str),
        Some("host")
    );
    Ok(())
}

#[test]
fn hostsocket_net_host_reachable_from_the_cli_override() -> Result<()> {
    let overrides = ConfigOverrides {
        hostsocket_net: Some("host".to_string()),
        ..Default::default()
    };
    let cfg = load_overrides(&overrides)?;
    assert_eq!(
        cfg.wasm_boards[0].manifest.net,
        crate::net::NetConfig::Loopback
    );
    assert_eq!(
        cfg.wasm_boards[0]
            .manifest
            .config
            .get("transport")
            .map(String::as_str),
        Some("host")
    );
    Ok(())
}

#[test]
fn cli_overrides_are_validated_like_config_fields() {
    // A 68000 cannot carry an FPU; the override hits the same check as
    // `[cpu] fpu = true` would.
    let overrides = ConfigOverrides {
        cpu: Some("68000".to_string()),
        fpu: Some(true),
        ..Default::default()
    };
    let err = load_overrides(&overrides).unwrap_err();
    assert!(err.to_string().contains("coprocessor interface"), "{err:#}");

    // An unknown chipset name is rejected by the shared parser.
    let overrides = ConfigOverrides {
        chipset: Some("OCS3".to_string()),
        ..Default::default()
    };
    let err = load_overrides(&overrides).unwrap_err();
    assert!(err.to_string().contains("unknown chipset"), "{err:#}");
}

/// The About window's ROM line names the image as well as the file: the
/// checksum table (src/romdb.rs) says which Kickstart a dump is, and the
/// ROM Copperline ships in place of one says so.
#[test]
fn the_about_rom_line_names_the_image_it_can_identify() {
    // No ROM named: the sentinel is the bundled AROS, whether or not it
    // has been resolved to a real path yet.
    let mut cfg = Config::default();
    assert_eq!(
        rom_identification(Path::new(BUNDLED_AROS_ROM)).as_deref(),
        Some("bundled AROS")
    );
    assert_eq!(
        rom_identification(
            Path::new("/opt/share/copperline/aros")
                .join(crate::romsearch::AROS_MAIN_FILE)
                .as_path()
        )
        .as_deref(),
        Some("bundled AROS")
    );

    // A file that is not a known ROM adds nothing to the line.
    let unknown = temp_path("not-a-rom.rom");
    fs::write(&unknown, vec![0xA5u8; 4096]).unwrap();
    assert_eq!(rom_identification(&unknown), None);
    cfg.rom_path = unknown.clone();
    // An unknown extended ROM gets a line of its own, named the same
    // way; without one fitted no such line appears.
    cfg.extended_rom_path = Some(unknown.clone());
    cfg.fmv_rom_path = Some(unknown.clone());
    let name = unknown.file_name().unwrap().to_string_lossy().into_owned();
    let lines = about_machine_lines(&cfg);
    assert!(
        lines.iter().any(|l| *l == format!("ROM: {name}")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| *l == format!("Extended ROM: {name}")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| *l == format!("CD32 FMV ROM: {name}")),
        "{lines:?}"
    );
    cfg.extended_rom_path = None;
    cfg.fmv_rom_path = None;
    assert!(!about_machine_lines(&cfg)
        .iter()
        .any(|l| l.starts_with("Extended ROM: ")),);
    let _ = fs::remove_file(&unknown);

    // A directory, a missing file and an over-large one are all just
    // unknown rather than an error or a panic.
    assert_eq!(rom_identification(Path::new("/no/such/rom.rom")), None);
    assert_eq!(rom_identification(&std::env::temp_dir()), None);
    let huge = temp_path("huge.rom");
    fs::write(&huge, vec![0u8; 3 * 1024 * 1024]).unwrap();
    assert_eq!(rom_identification(&huge), None);
    let _ = fs::remove_file(&huge);

    // A recognised image: the About line shows the identification
    // alone, an unrecognised one falls back to the file name. The
    // bytes of a real Kickstart are not in the tree, so the
    // composition is exercised through the same helper the panel
    // uses, fed the label of a real table entry.
    let entry = crate::romdb::identify_crc(0x1483A091, 512 * 1024).expect("KS 3.1 A1200");
    assert_eq!(entry.label, "Kickstart 3.1 (40.68) A1200");
    assert_eq!(
        about_rom_line("kick40068.A1200", Some(entry.label)),
        "ROM: Kickstart 3.1 (40.68) A1200"
    );
    assert_eq!(about_rom_line("mystery.rom", None), "ROM: mystery.rom");

    // The bundled AROS names its own numbers: a fake image wearing
    // the AROS file name, a version header at offset 12 and a $VER
    // cookie in the body, resolves to the composed line the launcher's
    // Kickstart row shows; stripped of both it stays "bundled AROS".
    // The identification keys on the exact AROS file name, so it
    // sits inside a unique directory rather than being uniquified.
    let dir = temp_path("aros-dir");
    fs::create_dir_all(&dir).unwrap();
    let aros = dir.join(crate::romsearch::AROS_MAIN_FILE);
    let mut data = vec![0u8; 32];
    data[12..14].copy_from_slice(&46u16.to_be_bytes());
    data[14..16].copy_from_slice(&7u16.to_be_bytes());
    data.extend_from_slice(b"$VER: AROS ROM 46.0.7 (1.1.2024)\n");
    fs::write(&aros, &data).unwrap();
    assert_eq!(
        about_rom_identification(&aros).as_deref(),
        Some("AROS 46.7 (46.0.7)")
    );
    fs::write(&aros, b"").unwrap();
    assert_eq!(
        about_rom_identification(&aros).as_deref(),
        Some("bundled AROS")
    );
    let _ = fs::remove_file(&aros);
    let _ = fs::remove_dir(&dir);
}

fn temp_adf() -> Result<PathBuf> {
    let path = temp_path("test.adf");
    fs::write(&path, vec![0u8; 80 * 2 * 11 * 512])?;
    Ok(path)
}

fn temp_path(name: &str) -> PathBuf {
    // The clock alone is not unique enough: tests run in parallel, and
    // two landing on the same nanosecond share a path -- so one reads a
    // file the other is still writing, and fails on a short read. The
    // counter makes the name unique within the process whatever the
    // clock says.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("copperline-config-test-{nanos}-{seq}-{name}"))
}

fn toml_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[test]
fn emulation_run_ahead_frames_parses_and_rejects_out_of_range() -> Result<()> {
    assert_eq!(parse_config("")?.emulation.run_ahead_frames, 0);
    let cfg = parse_config(
        r#"
            [emulation]
            run_ahead_frames = 2
            "#,
    )?;
    assert_eq!(cfg.emulation.run_ahead_frames, 2);
    assert!(parse_config("[emulation]\nrun_ahead_frames = 5").is_err());
    Ok(())
}

#[test]
fn runahead_machine_gate_rejects_host_coupled_storage() -> Result<()> {
    assert_eq!(parse_config("")?.runahead_machine_block_reason(), None);

    let cfg = parse_config(
        r#"
            [[filesys]]
            path = "shared"
        "#,
    )?;
    assert_eq!(
        cfg.runahead_machine_block_reason(),
        Some("host directory volume")
    );

    let cfg = parse_config(
        r#"
            [machine]
            profile = "A600"

            [ide]
            master = "disk.hdf"
        "#,
    )?;
    assert_eq!(
        cfg.runahead_machine_block_reason(),
        Some("hard-drive or ATAPI image")
    );
    Ok(())
}
