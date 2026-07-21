// SPDX-License-Identifier: GPL-3.0-or-later

//! Asset-gated check that a real Kickstart sizes the Ramsey motherboard
//! fast RAM: boots the A4000 profile with the local Kickstart 3.1 A4000
//! ROM and walks exec's memory list for the bank ending at $08000000.
//! Motherboard RAM is not autoconfig -- exec's own probe reads the Ramsey
//! control register for the DRAM geometry and pattern-tests the banks --
//! so a listed MemHeader proves the decode, the register seeding, and the
//! Fat Gary timeout on the unfitted space all line up.

use copperline::config::{Config, ConfigOverrides};

/// The integration-test asset directory (see `tests/README.md`):
/// `COPPERLINE_TEST_ASSETS`, else `test-assets/` under the repo root,
/// else the repo root itself.
fn asset(name: &str) -> Option<std::path::PathBuf> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = match std::env::var_os("COPPERLINE_TEST_ASSETS") {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            let d = root.join("test-assets");
            if d.is_dir() {
                d
            } else {
                root
            }
        }
    };
    let path = dir.join(name);
    path.is_file().then_some(path)
}

fn peek32(bus: &copperline::bus::Bus, addr: u32) -> u32 {
    (u32::from(bus.peek_word_any(addr)) << 16) | u32::from(bus.peek_word_any(addr + 2))
}

#[test]
#[ignore = "needs the local Kickstart 3.1 A4000 ROM (see tests/README.md)"]
fn kickstart_sizes_the_motherboard_ram_bank() {
    let Some(rom) = asset("Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom") else {
        eprintln!("skipping: Kickstart 3.1 A4000 ROM not in the asset directory");
        return;
    };
    let overrides = ConfigOverrides {
        model: Some("A4000".into()),
        ..Default::default()
    };
    let raw = Config::load_raw(None, &overrides).expect("raw config");
    let cfg = Config::try_from(raw)
        .expect("A4000 profile validates")
        .with_rom_override(Some(rom));
    assert_eq!(cfg.mb_ram_bytes, 4 * 1024 * 1024);
    let mut emu = copperline::emulator::build_machine(
        &cfg,
        Box::new(copperline::audio::NullSink),
        false,
        false,
    )
    .expect("machine builds");
    // Exec sizes memory in the first moments of boot; run well past it.
    for _ in 0..400 {
        emu.step_frame().expect("frame");
    }
    let bus = emu.bus();
    // Kickstart moves ExecBase into the best memory it found, which is the
    // motherboard bank ($07C00000-$08000000 with the stock 4 MiB).
    let execbase = peek32(bus, 4);
    assert_eq!(
        execbase & 0xFFC0_0000,
        0x07C0_0000,
        "ExecBase ${execbase:08X} did not move into the motherboard bank"
    );
    // Walk ExecBase->MemList for the MemHeader covering the bank.
    let mut node = peek32(bus, execbase + 322); // lh_Head
    let mut bank = None;
    while node != 0 {
        let succ = peek32(bus, node); // ln_Succ; 0 at the lh_Tail sentinel
        if succ == 0 {
            break;
        }
        let lower = peek32(bus, node + 20); // mh_Lower
        let upper = peek32(bus, node + 24); // mh_Upper
        if upper == 0x0800_0000 {
            bank = Some(lower);
        }
        node = succ;
    }
    let lower = bank.expect("exec lists a MemHeader ending at $08000000");
    // The bank starts at $07C00000; exec claims a little for its header.
    assert_eq!(
        lower & 0xFFF0_0000,
        0x07C0_0000,
        "mh_Lower ${lower:08X} is not the top-down 4 MiB bank"
    );
}

/// Boot a machine and collect exec's memory list as (mh_Lower, mh_Upper)
/// pairs once Kickstart has finished sizing memory.
fn boot_and_walk_memlist(cfg: &copperline::config::Config) -> Vec<(u32, u32)> {
    let mut emu = copperline::emulator::build_machine(
        cfg,
        Box::new(copperline::audio::NullSink),
        false,
        false,
    )
    .expect("machine builds");
    // Exec sizes memory in the first moments of boot; run well past it.
    for _ in 0..400 {
        emu.step_frame().expect("frame");
    }
    let bus = emu.bus();
    let execbase = peek32(bus, 4);
    let mut headers = Vec::new();
    let mut node = peek32(bus, execbase + 322); // lh_Head
    while node != 0 {
        let succ = peek32(bus, node); // ln_Succ; 0 at the lh_Tail sentinel
        if succ == 0 {
            break;
        }
        headers.push((peek32(bus, node + 20), peek32(bus, node + 24)));
        node = succ;
    }
    headers
}

/// The A4000 motherboard RAM expansion space: with 64 MiB fitted the bank
/// grows down from $08000000 all the way to $04000000, and Kickstart's
/// top-down probe must size the whole window, not stop at Ramsey's own
/// four-bank 16 MiB.
#[test]
#[ignore = "needs the local Kickstart 3.1 A4000 ROM (see tests/README.md)"]
fn kickstart_sizes_the_expanded_motherboard_ram() {
    let Some(rom) = asset("Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom") else {
        eprintln!("skipping: Kickstart 3.1 A4000 ROM not in the asset directory");
        return;
    };
    let overrides = ConfigOverrides {
        model: Some("A4000".into()),
        motherboard: Some("64M".into()),
        ..Default::default()
    };
    let raw = Config::load_raw(None, &overrides).expect("raw config");
    let cfg = Config::try_from(raw)
        .expect("A4000 with 64M motherboard RAM validates")
        .with_rom_override(Some(rom));
    assert_eq!(cfg.mb_ram_bytes, 64 * 1024 * 1024);
    let headers = boot_and_walk_memlist(&cfg);
    let (lower, upper) = headers
        .iter()
        .copied()
        .find(|&(_, upper)| upper == 0x0800_0000)
        .unwrap_or_else(|| panic!("no MemHeader ending at $08000000 in {headers:X?}"));
    assert_eq!(upper, 0x0800_0000);
    // Exec claims a little of the bottom of the bank for its header.
    assert_eq!(
        lower & 0xFFF0_0000,
        0x0400_0000,
        "mh_Lower ${lower:08X}: the probe did not reach the bottom of the \
         expansion window at $04000000"
    );
}

/// CPU-slot (accelerator) RAM at $08000000: Kickstart's bottom-up probe of
/// the coprocessor-slot space must find the fitted bank and add it to the
/// memory list.
#[test]
#[ignore = "needs the local Kickstart 3.1 A4000 ROM (see tests/README.md)"]
fn kickstart_sizes_the_cpu_slot_ram() {
    let Some(rom) = asset("Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom") else {
        eprintln!("skipping: Kickstart 3.1 A4000 ROM not in the asset directory");
        return;
    };
    let overrides = ConfigOverrides {
        model: Some("A4000".into()),
        accelerator: Some("64M".into()),
        ..Default::default()
    };
    let raw = Config::load_raw(None, &overrides).expect("raw config");
    let cfg = Config::try_from(raw)
        .expect("A4000 with 64M accelerator RAM validates")
        .with_rom_override(Some(rom));
    assert_eq!(cfg.accel_ram_bytes, 64 * 1024 * 1024);
    let headers = boot_and_walk_memlist(&cfg);
    let (lower, upper) = headers
        .iter()
        .copied()
        .find(|&(lower, _)| (0x0800_0000..0x0810_0000).contains(&lower))
        .unwrap_or_else(|| panic!("no MemHeader starting at $08000000 in {headers:X?}"));
    // Exec claims a little of the bottom of the bank for its header.
    assert_eq!(
        upper, 0x0C00_0000,
        "mh_Upper ${upper:08X}: the probe did not size the whole 64 MiB \
         CPU-slot bank from ${lower:08X}"
    );
}
