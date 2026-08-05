// SPDX-License-Identifier: LGPL-2.1-or-later

//! The differential harness: the reference C++ engine, opened on real ROMs,
//! against this crate. Munt is the accepted yardstick for the hardware, so
//! matching it bit for bit at the native rate is the port's definition of
//! done -- these tests are the bar itself.
//!
//! ROMs are Roland's copyright and cannot be committed, so everything here
//! is `#[ignore]` and looks for a local pair under `MT32_RS_ROMS`
//! (`MT32_CONTROL.ROM` and `MT32_PCM.ROM`, as Copperline names them).

#![cfg(feature = "oracle")]

use std::ffi::c_char;
use std::os::raw::{c_int, c_uint};

extern "C" {
    fn mt32_oracle_open(
        control: *const u8,
        control_len: usize,
        pcm: *const u8,
        pcm_len: usize,
        analog_mode: c_int,
    ) -> *mut std::ffi::c_void;
    fn mt32_oracle_close(handle: *mut std::ffi::c_void);
    fn mt32_oracle_sample_rate(handle: *const std::ffi::c_void) -> c_uint;
    fn mt32_oracle_play_msg(handle: *mut std::ffi::c_void, msg: c_uint);
    fn mt32_oracle_play_sysex(handle: *mut std::ffi::c_void, sysex: *const u8, len: c_uint);
    fn mt32_oracle_render(handle: *mut std::ffi::c_void, stream: *mut i16, frames: c_uint);
    fn mt32_oracle_display(handle: *const std::ffi::c_void, buffer21: *mut c_char) -> c_int;
    fn mt32_oracle_read_memory(
        handle: *mut std::ffi::c_void,
        addr: c_uint,
        len: c_uint,
        out: *mut u8,
    );
}

/// The reference engine, opened digital-only so it renders at the native
/// 32 kHz this crate is measured against.
struct Oracle(*mut std::ffi::c_void);

// One test thread drives it at a time; the handle is not shared.
unsafe impl Send for Oracle {}

impl Oracle {
    fn open(control: &[u8], pcm: &[u8]) -> Option<Self> {
        // 0 = AnalogOutputMode_DIGITAL_ONLY: the LA32 stream itself.
        let handle = unsafe {
            mt32_oracle_open(control.as_ptr(), control.len(), pcm.as_ptr(), pcm.len(), 0)
        };
        (!handle.is_null()).then_some(Self(handle))
    }

    fn sample_rate(&self) -> u32 {
        unsafe { mt32_oracle_sample_rate(self.0) }
    }

    fn play_msg(&mut self, msg: u32) {
        unsafe { mt32_oracle_play_msg(self.0, msg) }
    }

    #[allow(dead_code)]
    fn play_sysex(&mut self, sysex: &[u8]) {
        unsafe { mt32_oracle_play_sysex(self.0, sysex.as_ptr(), sysex.len() as c_uint) }
    }

    fn render(&mut self, frames: usize) -> Vec<(i16, i16)> {
        let mut interleaved = vec![0i16; frames * 2];
        unsafe { mt32_oracle_render(self.0, interleaved.as_mut_ptr(), frames as c_uint) };
        interleaved.chunks_exact(2).map(|f| (f[0], f[1])).collect()
    }

    fn display(&self) -> (String, bool) {
        let mut buffer = [0 as c_char; 21];
        let lamp = unsafe { mt32_oracle_display(self.0, buffer.as_mut_ptr()) } != 0;
        let bytes: Vec<u8> = buffer
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as u8)
            .collect();
        (String::from_utf8_lossy(&bytes).into_owned(), lamp)
    }

    fn read_memory(&mut self, addr: u32, out: &mut [u8]) {
        unsafe { mt32_oracle_read_memory(self.0, addr, out.len() as c_uint, out.as_mut_ptr()) }
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        unsafe { mt32_oracle_close(self.0) }
    }
}

/// The local ROM pair, if the runner has one.
fn rom_pair() -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = std::path::PathBuf::from(std::env::var_os("MT32_RS_ROMS")?);
    let control = std::fs::read(dir.join("MT32_CONTROL.ROM")).ok()?;
    let pcm = std::fs::read(dir.join("MT32_PCM.ROM")).ok()?;
    Some((control, pcm))
}

/// The oracle opens, names itself on the display, runs at the native rate,
/// and renders deterministically: the same note twice gives byte-identical
/// PCM. Everything the differential tests will lean on, proven before any
/// Rust engine exists to compare.
#[test]
#[ignore = "needs a ROM pair in MT32_RS_ROMS"]
fn the_oracle_stands() {
    let Some((control, pcm)) = rom_pair() else {
        panic!("MT32_RS_ROMS is not set or the pair is unreadable");
    };

    let render_a_note = || {
        let mut oracle = Oracle::open(&control, &pcm).expect("the reference engine opens");
        assert_eq!(oracle.sample_rate(), mt32_rs::SAMPLE_RATE);
        // Skip the power-on state settling, then middle C on part 1:
        // status 0x91 in the low byte, note 0x3C, velocity 0x7F.
        oracle.render(mt32_rs::SAMPLE_RATE as usize / 4);
        oracle.play_msg(0x007F_3C91);
        let pcm_out = oracle.render(mt32_rs::SAMPLE_RATE as usize);
        let (line, _lamp) = oracle.display();
        (pcm_out, line)
    };

    let (first, line) = render_a_note();
    assert!(
        first.iter().any(|&(l, r)| l != 0 || r != 0),
        "a note makes sound"
    );
    assert!(!line.is_empty(), "the display says something: {line:?}");

    let (second, _) = render_a_note();
    assert_eq!(first, second, "the reference engine is deterministic");

    // And its memory answers: the system area starts with the master tune
    // byte, which power-on sets to 442 Hz = 0x4A.
    let mut oracle = Oracle::open(&control, &pcm).expect("opens again");
    let mut system = [0u8; 1];
    oracle.read_memory(0x10_0000, &mut system);
    assert_eq!(system[0], 0x4A, "master tune answers at its address");
}

extern "C" {
    fn mt32_oracle_identify(data: *const u8, len: usize, name32: *mut c_char) -> c_int;
}

/// What the reference engine calls one image, if it is a known full ROM.
fn oracle_identify(data: &[u8]) -> Option<String> {
    let mut name = [0 as c_char; 32];
    let full = unsafe { mt32_oracle_identify(data.as_ptr(), data.len(), name.as_mut_ptr()) };
    (full != 0).then(|| {
        let bytes: Vec<u8> = name
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Identification agrees with the reference engine on every file in the
/// ROM directory: same short name for every full image, and nothing this
/// crate accepts that the engine does not. The directory's half- and
/// interleaved-dump files exercise the refusals.
#[test]
#[ignore = "needs a ROM directory in MT32_RS_ROMS_ALL"]
fn identification_agrees_with_the_reference_engine() {
    let Some(dir) = std::env::var_os("MT32_RS_ROMS_ALL") else {
        panic!("MT32_RS_ROMS_ALL is not set");
    };
    let mut seen = 0;
    let mut full = 0;
    for entry in std::fs::read_dir(dir)
        .expect("the directory opens")
        .flatten()
    {
        let Ok(data) = std::fs::read(entry.path()) else {
            continue;
        };
        let theirs = oracle_identify(&data);
        let ours = mt32_rs::rom::identify(&data).map(|r| r.short_name.to_string());
        assert_eq!(
            ours,
            theirs,
            "{} identified differently",
            entry.file_name().to_string_lossy()
        );
        seen += 1;
        full += usize::from(ours.is_some());
    }
    assert!(seen > 0, "no files to check");
    assert!(full > 0, "no full ROMs among them");
    println!("agreed on {seen} files, {full} of them full ROMs");
}

/// The layouts read real images correctly: every full control ROM in the
/// directory yields printable startup and error messages at the layout's
/// offsets, and its wave directory decodes in bounds against its own
/// family's sample store.
#[test]
#[ignore = "needs a ROM directory in MT32_RS_ROMS_ALL"]
fn the_layouts_read_the_real_images() {
    let Some(dir) = std::env::var_os("MT32_RS_ROMS_ALL") else {
        panic!("MT32_RS_ROMS_ALL is not set");
    };
    let mut checked = 0;
    for entry in std::fs::read_dir(dir)
        .expect("the directory opens")
        .flatten()
    {
        let Ok(data) = std::fs::read(entry.path()) else {
            continue;
        };
        let Some(info) = mt32_rs::rom::identify(&data) else {
            continue;
        };
        let Some(layout) = mt32_rs::layout::layout_for(info) else {
            continue;
        };
        let line = |offset: u16| {
            let at = usize::from(offset);
            let text = &data[at..at + 20];
            assert!(
                text.iter().all(|&b| (0x20..0x7F).contains(&b)),
                "{}: not a display line at 0x{offset:04X}: {text:?}",
                info.short_name
            );
            String::from_utf8_lossy(text).into_owned()
        };
        let startup = line(layout.startup_message);
        let error = line(layout.sysex_error_message);
        // The biggest store this control ROM's family can address; its
        // own directory must fit inside it.
        let samples = if layout.pcm_count == 256 {
            0x80000
        } else {
            0x40000
        };
        let waves = mt32_rs::pcm::waves(&data, layout, samples).expect(info.short_name);
        assert_eq!(waves.len(), usize::from(layout.pcm_count));
        println!(
            "  {:<18} startup {startup:?}  error {error:?}  {} waves",
            info.short_name,
            waves.len()
        );
        checked += 1;
    }
    assert!(checked > 0, "no control ROMs to check");
}

extern "C" {
    fn mt32_oracle_tables(
        curves559: *mut u8,
        exp512: *mut u16,
        logsin512: *mut u16,
        decay8: *mut u8,
    );
}

/// Every constant table matches the engine's, entry for entry. Needs no
/// ROMs: the tables are computed, and this is the test that catches a
/// formula mirrored at the wrong precision -- or a libm that rounds an
/// entry differently, which is worth knowing about the moment it happens.
#[test]
fn the_constant_tables_match_the_reference_engine() {
    let mut curves = [0u8; 559];
    let mut exp9 = [0u16; 512];
    let mut logsin9 = [0u16; 512];
    let mut decay = [0u8; 8];
    unsafe {
        mt32_oracle_tables(
            curves.as_mut_ptr(),
            exp9.as_mut_ptr(),
            logsin9.as_mut_ptr(),
            decay.as_mut_ptr(),
        )
    };
    let ours = mt32_rs::tables::Tables::new();
    assert_eq!(ours.level_to_amp_subtraction[..], curves[..101]);
    assert_eq!(ours.env_logarithmic_time[..], curves[101..357]);
    assert_eq!(ours.master_vol_to_amp_subtraction[..], curves[357..458]);
    assert_eq!(ours.pulse_width_100_to_255[..], curves[458..559]);
    assert_eq!(ours.exp9, exp9);
    assert_eq!(ours.logsin9, logsin9);
    assert_eq!(ours.res_amp_decay_factors, decay);
}

/// The readable regions, drained from an engine via its own RQ1 path.
fn oracle_regions(oracle: &mut Oracle) -> Vec<(&'static str, u32, usize, Vec<u8>)> {
    [
        ("patch temp", 0x03_0000, 9 * 16),
        ("rhythm temp", 0x03_0110, 85 * 4),
        ("timbre temp", 0x04_0000, 8 * 246),
        ("patches", 0x05_0000, 128 * 8),
        ("memory timbres", 0x08_0000, 64 * 256),
        ("system", 0x10_0000, 23),
    ]
    .into_iter()
    .map(|(name, addr, len)| {
        let mut bytes = vec![0u8; len];
        oracle.read_memory(addr, &mut bytes);
        (name, addr, len, bytes)
    })
    .collect()
}

/// Power-on memory matches the reference engine byte for byte, for every
/// control ROM in the directory: the timbre banks through their clamped
/// loads and compressed expansion, the part programs, the rhythm setup,
/// and the system defaults. The strongest whole-layer check before any
/// sound exists to compare.
#[test]
#[ignore = "needs a ROM directory in MT32_RS_ROMS_ALL"]
fn power_on_memory_matches_the_reference_engine() {
    let Some(dir) = std::env::var_os("MT32_RS_ROMS_ALL") else {
        panic!("MT32_RS_ROMS_ALL is not set");
    };
    let dir = std::path::PathBuf::from(dir);
    let mut images = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .expect("the directory opens")
        .flatten()
    {
        if let Ok(data) = std::fs::read(entry.path()) {
            if let Some(info) = mt32_rs::rom::identify(&data) {
                images.push((info, data));
            }
        }
    }
    let pcm_for = |family: mt32_rs::rom::Family| {
        let want = if family == mt32_rs::rom::Family::Cm32l {
            "pcm_cm32l"
        } else {
            "pcm_mt32"
        };
        images
            .iter()
            .find(|(info, _)| info.short_name == want)
            .map(|(_, data)| data.clone())
            .expect(want)
    };
    let mut checked = 0;
    for (info, control) in &images {
        let Some(family) = info.family else { continue };
        let layout = mt32_rs::layout::layout_for(info).expect(info.short_name);
        let mut oracle = Oracle::open(control, &pcm_for(family)).expect(info.short_name);
        let mut ours = mt32_rs::memory::Memory::power_on(control, layout).expect(info.short_name);
        for (name, addr, len, theirs) in oracle_regions(&mut oracle) {
            let mut mine = vec![0u8; len];
            let got = ours.read(mt32_rs::memory::flat(addr), &mut mine);
            assert_eq!(got, len, "{}: {name} short read", info.short_name);
            if mine != theirs {
                let at = mine.iter().zip(&theirs).position(|(a, b)| a != b).unwrap();
                panic!(
                    "{}: {name} differs first at +0x{at:X}: ours 0x{:02X} theirs 0x{:02X}",
                    info.short_name, mine[at], theirs[at]
                );
            }
        }
        checked += 1;
        println!("  {:<18} power-on memory identical", info.short_name);
    }
    assert!(checked > 0, "no control ROMs to check");
}

/// Both engines' readable memory, compared byte for byte; panics with the
/// first differing byte named.
fn assert_memory_matches(oracle: &mut Oracle, ours: &mut mt32_rs::memory::Memory, when: &str) {
    for (name, addr, len, theirs) in oracle_regions(oracle) {
        let mut mine = vec![0u8; len];
        ours.read(mt32_rs::memory::flat(addr), &mut mine);
        if mine != theirs {
            let at = mine.iter().zip(&theirs).position(|(a, b)| a != b).unwrap();
            panic!(
                "{when}: {name} differs first at +0x{at:X}: ours 0x{:02X} theirs 0x{:02X}",
                mine[at], theirs[at]
            );
        }
    }
}

/// The same DT1 traffic into both engines leaves the same memory behind:
/// plain writes, over-maximum values that clamp, a write spanning two
/// regions, a bad checksum that must change nothing, and the 0x7F reset
/// that puts everything back. The write path's whole behaviour, held to
/// the reference.
#[test]
#[ignore = "needs a ROM pair in MT32_RS_ROMS"]
fn sysex_writes_match_the_reference_engine() {
    let Some((control, pcm)) = rom_pair() else {
        panic!("MT32_RS_ROMS is not set or the pair is unreadable");
    };
    let info = mt32_rs::rom::identify(&control).expect("a known control ROM");
    let layout = mt32_rs::layout::layout_for(info).expect(info.short_name);
    let mut oracle = Oracle::open(&control, &pcm).expect("the reference engine opens");
    let mut ours = mt32_rs::memory::Memory::power_on(&control, layout).expect(info.short_name);

    let timbre: Vec<u8> = (0..246u32).map(|i| (i % 101) as u8).collect();
    let steps: Vec<(&str, Vec<u8>)> = vec![
        (
            "reverb settings",
            mt32_rs::sysex::dt1(0x10_0001, &[1, 4, 5]),
        ),
        (
            "master volume clamps",
            mt32_rs::sysex::dt1(0x10_0016, &[0x7F]),
        ),
        (
            "a part's patch temp",
            mt32_rs::sysex::dt1(0x03_0010, &[1, 5, 30, 60, 10, 2, 0]),
        ),
        (
            "part levels and pans",
            mt32_rs::sysex::dt1(0x03_0018, &[90, 3]),
        ),
        (
            "spanning patch temp into rhythm temp",
            mt32_rs::sysex::dt1(0x03_0108, &[7, 7, 7, 7, 7, 7, 7, 7, 40, 50, 7, 1]),
        ),
        (
            "a rhythm key",
            mt32_rs::sysex::dt1(0x03_0112, &[60, 80, 7, 1]),
        ),
        ("a memory timbre", mt32_rs::sysex::dt1(0x08_0A00, &timbre)),
        (
            "a patch",
            mt32_rs::sysex::dt1(0x05_0000, &[2, 5, 20, 40, 8, 1, 0, 0]),
        ),
        (
            "timbre temp",
            mt32_rs::sysex::dt1(0x04_0000, &timbre[..100]),
        ),
        (
            "system channel assignment",
            mt32_rs::sysex::dt1(0x10_000D, &[2, 3, 4, 5, 6, 7, 8, 9, 0]),
        ),
    ];
    for (what, message) in &steps {
        unsafe { mt32_oracle_play_sysex(oracle.0, message.as_ptr(), message.len() as c_uint) };
        let outcome = mt32_rs::sysex::play(&mut ours, message);
        assert!(
            matches!(outcome, mt32_rs::sysex::Outcome::Written(_)),
            "{what}: {outcome:?}"
        );
        assert_memory_matches(&mut oracle, &mut ours, what);
    }

    // One wrong byte: both engines must leave everything exactly alone.
    let mut bad = mt32_rs::sysex::dt1(0x10_0001, &[3]);
    let at = bad.len() - 2;
    bad[at] ^= 1;
    unsafe { mt32_oracle_play_sysex(oracle.0, bad.as_ptr(), bad.len() as c_uint) };
    assert_eq!(
        mt32_rs::sysex::play(&mut ours, &bad),
        mt32_rs::sysex::Outcome::ChecksumError
    );
    assert_memory_matches(&mut oracle, &mut ours, "after a bad checksum");

    // The reset address puts both back to power-on.
    let reset = mt32_rs::sysex::dt1(0x7F_0000, &[]);
    unsafe { mt32_oracle_play_sysex(oracle.0, reset.as_ptr(), reset.len() as c_uint) };
    assert_eq!(
        mt32_rs::sysex::play(&mut ours, &reset),
        mt32_rs::sysex::Outcome::Reset
    );
    ours = mt32_rs::memory::Memory::power_on(&control, layout).expect(info.short_name);
    assert_memory_matches(&mut oracle, &mut ours, "after the reset");
    println!(
        "  {} sysex steps, memory identical throughout",
        steps.len() + 2
    );
}

/// Our display beside the reference engine's: the same DT1 traffic and the
/// same rendered clock drive both, through the wiring the synth layer will
/// use -- the lamp on any accepted write, custom messages by their offset,
/// the volume poke on a system write, the banner on a bad checksum. Run
/// over an elder and a later firmware, whose display behaviours differ.
#[test]
#[ignore = "needs a ROM directory in MT32_RS_ROMS_ALL"]
fn the_display_matches_the_reference_engine() {
    let Some(dir) = std::env::var_os("MT32_RS_ROMS_ALL") else {
        panic!("MT32_RS_ROMS_ALL is not set");
    };
    let dir = std::path::PathBuf::from(dir);
    let read_named = |short: &str| {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            if let Ok(data) = std::fs::read(entry.path()) {
                if mt32_rs::rom::identify(&data).map(|i| i.short_name) == Some(short) {
                    return data;
                }
            }
        }
        panic!("{short} not present");
    };
    let pcm = read_named("pcm_mt32");

    for short in ["ctrl_mt32_1_07", "ctrl_mt32_2_07"] {
        let control = read_named(short);
        let info = mt32_rs::rom::identify(&control).unwrap();
        let layout = mt32_rs::layout::layout_for(info).unwrap();
        let mut oracle = Oracle::open(&control, &pcm).expect(short);
        let mut memory = mt32_rs::memory::Memory::power_on(&control, layout).expect(short);
        let mut display = mt32_rs::display::Display::power_on(&control, layout);
        let mut now: u32 = 0;

        let render = |oracle: &mut Oracle,
                      display: &mut mt32_rs::display::Display,
                      now: &mut u32,
                      frames: u32| {
            oracle.render(frames as usize);
            *now += frames;
            display.refresh(*now);
        };
        // The synth-layer wiring this crate's callers will use.
        let play = |oracle: &mut Oracle,
                    memory: &mut mt32_rs::memory::Memory,
                    display: &mut mt32_rs::display::Display,
                    now: u32,
                    message: &[u8]| {
            unsafe { mt32_oracle_play_sysex(oracle.0, message.as_ptr(), message.len() as c_uint) };
            match mt32_rs::sysex::play(memory, message) {
                mt32_rs::sysex::Outcome::Written(touched) => {
                    display.midi_message_played(now);
                    for touch in touched {
                        match touch {
                            mt32_rs::memory::Touched::Display { offset, data } => {
                                let len = data.len().min(mt32_rs::display::LCD_TEXT_SIZE);
                                display.custom_message(&data[..len], offset);
                            }
                            mt32_rs::memory::Touched::Ram {
                                region: mt32_rs::memory::Region::System,
                                offset,
                                len,
                            } if offset <= 22 && offset + len > 22 => {
                                display.master_volume_changed();
                            }
                            _ => {}
                        }
                    }
                }
                mt32_rs::sysex::Outcome::DisplayControl(bytes) => {
                    display.midi_message_played(now);
                    display.display_control(&bytes[1..]);
                }
                mt32_rs::sysex::Outcome::ChecksumError => display.checksum_error(now),
                other => panic!("{short}: unexpected outcome {other:?}"),
            }
        };
        let check = |oracle: &mut Oracle,
                     memory: &mut mt32_rs::memory::Memory,
                     display: &mut mt32_rs::display::Display,
                     when: &str| {
            let (theirs, their_led) = oracle.display();
            let mut vol = [0u8; 1];
            memory.read(mt32_rs::memory::flat(0x10_0016), &mut vol);
            let (mine, my_led) = display.state(vol[0]);
            let mine_text: String = mine
                .iter()
                .map(|&b| if b == 1 { '\u{25A0}' } else { b as char })
                .collect();
            assert_eq!(mine_text, theirs, "{short}: {when}");
            assert_eq!(my_led, their_led, "{short}: lamp, {when}");
        };

        render(&mut oracle, &mut display, &mut now, 256);
        check(&mut oracle, &mut memory, &mut display, "the startup banner");

        render(&mut oracle, &mut display, &mut now, 44_000);
        check(&mut oracle, &mut memory, &mut display, "the main screen");

        let hello = mt32_rs::sysex::dt1(0x20_0000, b"GREETINGS FROM RUST ");
        play(&mut oracle, &mut memory, &mut display, now, &hello);
        render(&mut oracle, &mut display, &mut now, 256);
        check(
            &mut oracle,
            &mut memory,
            &mut display,
            "a custom message, lamp lit",
        );

        render(&mut oracle, &mut display, &mut now, 8_000);
        check(&mut oracle, &mut memory, &mut display, "the lamp gone out");

        let position = mt32_rs::sysex::dt1(0x20_0005, b"!");
        play(&mut oracle, &mut memory, &mut display, now, &position);
        render(&mut oracle, &mut display, &mut now, 256);
        check(&mut oracle, &mut memory, &mut display, "a positional write");

        let volume = mt32_rs::sysex::dt1(0x10_0016, &[51]);
        play(&mut oracle, &mut memory, &mut display, now, &volume);
        render(&mut oracle, &mut display, &mut now, 256);
        check(&mut oracle, &mut memory, &mut display, "the volume written");

        let mut bad = mt32_rs::sysex::dt1(0x10_0016, &[52]);
        let at = bad.len() - 2;
        bad[at] ^= 1;
        play(&mut oracle, &mut memory, &mut display, now, &bad);
        render(&mut oracle, &mut display, &mut now, 256);
        check(
            &mut oracle,
            &mut memory,
            &mut display,
            "the checksum banner",
        );

        render(&mut oracle, &mut display, &mut now, 50_000);
        check(
            &mut oracle,
            &mut memory,
            &mut display,
            "whether the banner outlives its timer",
        );

        let reset = mt32_rs::sysex::dt1(0x20_0100, &[]);
        play(&mut oracle, &mut memory, &mut display, now, &reset);
        render(&mut oracle, &mut display, &mut now, 256);
        check(
            &mut oracle,
            &mut memory,
            &mut display,
            "after Display Reset",
        );

        println!("  {short:<18} display identical through the scenario");
    }
}

extern "C" {
    fn mt32_oracle_ramp_new() -> *mut std::ffi::c_void;
    fn mt32_oracle_ramp_free(ramp: *mut std::ffi::c_void);
    fn mt32_oracle_ramp_start(ramp: *mut std::ffi::c_void, target: u8, increment: u8);
    fn mt32_oracle_ramp_next(ramp: *mut std::ffi::c_void) -> u64;
}

/// Every ramp the chip can be asked for, against the reference: all 256
/// increments crossed with rising, falling and degenerate targets, every
/// step's value and interrupt compared. Needs no ROMs -- the ramp reads
/// only the exponent table.
#[test]
fn the_ramp_matches_the_reference_engine() {
    let tables = mt32_rs::tables::Tables::new();
    let theirs = unsafe { mt32_oracle_ramp_new() };
    let mut ours = mt32_rs::la32::ramp::Ramp::new();
    for increment in 0..=255u8 {
        for (from, target) in [(0u8, 255u8), (255, 0), (128, 128), (200, 60), (60, 200)] {
            // Put both ramps at `from` exactly: aim there with the fastest
            // slew in the right direction and run until settled.
            let seed = if from >= 128 { 0x7F } else { 0xFF };
            unsafe { mt32_oracle_ramp_start(theirs, from, seed) };
            ours.start(&tables, from, seed);
            for _ in 0..64 {
                unsafe { mt32_oracle_ramp_next(theirs) };
                ours.next_value();
            }
            unsafe { mt32_oracle_ramp_start(theirs, target, increment) };
            ours.start(&tables, target, increment);
            for step in 0..2048 {
                let reference = unsafe { mt32_oracle_ramp_next(theirs) };
                let value = ours.next_value();
                let interrupt = ours.check_interrupt();
                let packed = u64::from(value) | (u64::from(interrupt) << 32);
                assert_eq!(
                    packed, reference,
                    "increment 0x{increment:02X}, {from}->{target}, step {step}"
                );
            }
        }
    }
    unsafe { mt32_oracle_ramp_free(theirs) };
}

extern "C" {
    fn mt32_oracle_pair_new() -> *mut std::ffi::c_void;
    fn mt32_oracle_pair_free(pair: *mut std::ffi::c_void);
    fn mt32_oracle_pair_init(pair: *mut std::ffi::c_void, ring_modulated: c_int, mixed: c_int);
    fn mt32_oracle_pair_init_synth(
        pair: *mut std::ffi::c_void,
        which: c_int,
        sawtooth: c_int,
        pulse_width: u8,
        resonance: u8,
    );
    fn mt32_oracle_pair_init_pcm(
        pair: *mut std::ffi::c_void,
        which: c_int,
        pcm_wave: *const i16,
        len: c_uint,
        looped: c_int,
    );
    fn mt32_oracle_pair_generate(
        pair: *mut std::ffi::c_void,
        which: c_int,
        amp: c_uint,
        pitch: u16,
        cutoff: c_uint,
    );
    fn mt32_oracle_pair_next_out(pair: *mut std::ffi::c_void) -> i16;
}

/// The wave generator pair against the reference across the parameter
/// space: square and sawtooth at swept pulse widths, resonances, pitches
/// and cutoffs, with the amp ramping; then PCM waves looped and unlooped,
/// mixed pairs and ring-modulated pairs with and without the master mixed
/// back in. Every output sample equal. Needs no ROMs: the PCM wave is a
/// synthetic store both engines read.
#[test]
fn the_wave_generator_matches_the_reference_engine() {
    let tables = mt32_rs::tables::Tables::new();
    // A deterministic sample store with the full value range represented.
    let pcm: Vec<i16> = (0..0x1000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 16) as i16)
        .collect();
    let mut compared = 0u64;

    let mut run = |setup_theirs: &dyn Fn(*mut std::ffi::c_void),
                   setup_ours: &mut dyn FnMut(&mut mt32_rs::la32::wave::IntPartialPair),
                   samples: usize,
                   what: &str| {
        let theirs = unsafe { mt32_oracle_pair_new() };
        let mut ours = mt32_rs::la32::wave::IntPartialPair::new();
        setup_theirs(theirs);
        setup_ours(&mut ours);
        for n in 0..samples {
            // Sweep the live parameters so every segment and phase is
            // crossed: amp decaying, pitch climbing, cutoff swinging.
            let amp = (n as u32 * 997) % (1 << 26);
            let pitch = ((0x2000 + n * 37) & 0x7FFF) as u16;
            let cutoff = ((n as u32 * 61) % 250) << 18;
            unsafe {
                mt32_oracle_pair_generate(theirs, 0, amp, pitch, cutoff);
                mt32_oracle_pair_generate(theirs, 1, amp / 2, pitch ^ 0x111, cutoff / 2);
            }
            ours.generate_next_sample(
                &tables,
                &pcm,
                mt32_rs::la32::wave::Pair::Master,
                amp,
                pitch,
                cutoff,
            );
            ours.generate_next_sample(
                &tables,
                &pcm,
                mt32_rs::la32::wave::Pair::Slave,
                amp / 2,
                pitch ^ 0x111,
                cutoff / 2,
            );
            let reference = unsafe { mt32_oracle_pair_next_out(theirs) };
            let sample = ours.next_out_sample(&tables);
            assert_eq!(sample, reference, "{what}, sample {n}");
            compared += 1;
        }
        unsafe { mt32_oracle_pair_free(theirs) };
    };

    for sawtooth in [false, true] {
        for pulse_width in [0u8, 64, 128, 200, 255] {
            for resonance in [1u8, 10, 20, 30] {
                run(
                    &|t| unsafe {
                        mt32_oracle_pair_init(t, 0, 0);
                        mt32_oracle_pair_init_synth(
                            t,
                            0,
                            sawtooth as c_int,
                            pulse_width,
                            resonance,
                        );
                        mt32_oracle_pair_init_synth(
                            t,
                            1,
                            (!sawtooth) as c_int,
                            pulse_width / 2,
                            31 - resonance,
                        );
                    },
                    &mut |o| {
                        o.init(false, false);
                        o.init_synth(
                            &tables,
                            mt32_rs::la32::wave::Pair::Master,
                            sawtooth,
                            pulse_width,
                            resonance,
                        );
                        o.init_synth(
                            &tables,
                            mt32_rs::la32::wave::Pair::Slave,
                            !sawtooth,
                            pulse_width / 2,
                            31 - resonance,
                        );
                    },
                    512,
                    &format!("synth pair saw={sawtooth} pw={pulse_width} res={resonance}"),
                );
            }
        }
    }

    // PCM against synth, all pair structures, looped and once-through.
    for (ring, mixed) in [(false, false), (true, false), (true, true)] {
        for looped in [false, true] {
            run(
                &|t| unsafe {
                    mt32_oracle_pair_init(t, ring as c_int, mixed as c_int);
                    mt32_oracle_pair_init_pcm(
                        t,
                        0,
                        pcm.as_ptr().wrapping_add(0x100),
                        0x800,
                        looped as c_int,
                    );
                    mt32_oracle_pair_init_pcm(
                        t,
                        1,
                        pcm.as_ptr().wrapping_add(0x300),
                        0x400,
                        looped as c_int,
                    );
                },
                &mut |o| {
                    o.init(ring, mixed);
                    o.init_pcm(mt32_rs::la32::wave::Pair::Master, 0x100, 0x800, looped);
                    o.init_pcm(mt32_rs::la32::wave::Pair::Slave, 0x300, 0x400, looped);
                },
                2048,
                &format!("pcm pair ring={ring} mixed={mixed} looped={looped}"),
            );
            run(
                &|t| unsafe {
                    mt32_oracle_pair_init(t, ring as c_int, mixed as c_int);
                    mt32_oracle_pair_init_synth(t, 0, 1, 90, 25);
                    mt32_oracle_pair_init_pcm(
                        t,
                        1,
                        pcm.as_ptr().wrapping_add(0x40),
                        0x800,
                        looped as c_int,
                    );
                },
                &mut |o| {
                    o.init(ring, mixed);
                    o.init_synth(&tables, mt32_rs::la32::wave::Pair::Master, true, 90, 25);
                    o.init_pcm(mt32_rs::la32::wave::Pair::Slave, 0x40, 0x800, looped);
                },
                2048,
                &format!("synth+pcm ring={ring} mixed={mixed} looped={looped}"),
            );
        }
    }
    println!("  {compared} pair samples identical");
}

/// Render the same frame count through both engines in uneven chunks --
/// crossing the render loop's internal boundaries -- and panic at the
/// first differing frame, named by the running position.
fn assert_frames_match(
    oracle: &mut Oracle,
    ours: &mut mt32_rs::engine::Engine,
    frames: usize,
    at: &mut usize,
    when: &str,
) {
    let mut sizes = [1usize, 7, 100, 977, 4096, 6000].into_iter().cycle();
    let mut left = frames;
    while left > 0 {
        let chunk = sizes.next().unwrap().min(left);
        let theirs = oracle.render(chunk);
        let mut mine = vec![(0i16, 0i16); chunk];
        ours.render(&mut mine);
        if mine != theirs {
            let i = mine.iter().zip(&theirs).position(|(a, b)| a != b).unwrap();
            panic!(
                "{when}: frames diverge at {} (ours {:?}, theirs {:?})",
                *at + i,
                mine[i],
                theirs[i]
            );
        }
        *at += chunk;
        left -= chunk;
    }
}

/// The full sound path against the reference: both engines opened on the
/// same pair, the same reverb-off DT1 putting the whole weight on the dry
/// path, the same note held, bent and released, every output frame
/// compared. This is the bar the sound path aims at: the LA32 pair, the
/// ramps, all three envelopes, the jitter stream, voice allocation and
/// the analog combine have to agree at once for a single frame to match.
#[test]
#[ignore = "needs a ROM pair in MT32_RS_ROMS"]
fn a_whole_note_matches_the_reference_engine() {
    let Some((control, pcm)) = rom_pair() else {
        panic!("MT32_RS_ROMS is not set or the pair is unreadable");
    };
    let mut oracle = Oracle::open(&control, &pcm).expect("the reference engine opens");
    let mut ours = mt32_rs::engine::Engine::open(&control, &pcm).expect("the engine opens");

    let rate = mt32_rs::SAMPLE_RATE as usize;
    let mut at = 0usize;

    // A quarter second of power-on quiet: the render machinery agrees
    // before any voice sounds.
    assert_frames_match(&mut oracle, &mut ours, rate / 4, &mut at, "power-on quiet");

    // Part 1's reverb off. No partial feeds the reference's reverb, so
    // its wet stream is silence too and the engines are on equal terms.
    let dry = mt32_rs::sysex::dt1(0x03_0006, &[0]);
    oracle.play_sysex(&dry);
    ours.play_sysex(&dry);

    // Middle C, full velocity, channel 1: part 1's piano, held.
    oracle.play_msg(0x007F_3C91);
    ours.play_msg(0x007F_3C91);
    assert_frames_match(&mut oracle, &mut ours, 2 * rate, &mut at, "the note held");

    // Bend it up a little: the pitch envelope re-aims in flight.
    oracle.play_msg(0x0060_00E1);
    ours.play_msg(0x0060_00E1);
    assert_frames_match(&mut oracle, &mut ours, rate / 2, &mut at, "the note bent");

    // Off, and the release tail out to nothing.
    oracle.play_msg(0x0000_3C81);
    ours.play_msg(0x0000_3C81);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        2 * rate,
        &mut at,
        "the release tail",
    );

    assert_eq!(ours.rendered_sample_count(), at as u32);
    println!("  {at} frames identical through hold, bend and release");
}

/// The same note through every control ROM in the directory, each on its
/// own family's PCM ROM: both MT-32 generations and the CM-32L line, with
/// each firmware's own reverb ringing. The elder paths -- the slower
/// envelope timer, the generation's key handling, the older voice books,
/// the two reverb flavours and their different wet gains -- all sit under
/// this one.
#[test]
#[ignore = "needs a ROM directory in MT32_RS_ROMS_ALL"]
fn a_note_matches_on_every_rom() {
    let Some(dir) = std::env::var_os("MT32_RS_ROMS_ALL") else {
        panic!("MT32_RS_ROMS_ALL is not set");
    };
    let dir = std::path::PathBuf::from(dir);
    let mut images = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .expect("the directory opens")
        .flatten()
    {
        if let Ok(data) = std::fs::read(entry.path()) {
            if let Some(info) = mt32_rs::rom::identify(&data) {
                images.push((info, data));
            }
        }
    }
    let pcm_for = |family: mt32_rs::rom::Family| {
        let want = if family == mt32_rs::rom::Family::Cm32l {
            "pcm_cm32l"
        } else {
            "pcm_mt32"
        };
        images
            .iter()
            .find(|(info, _)| info.short_name == want)
            .map(|(_, data)| data.clone())
            .expect(want)
    };
    let mut checked = 0;
    for (info, control) in &images {
        let Some(family) = info.family else { continue };
        let pcm = pcm_for(family);
        let mut oracle = Oracle::open(control, &pcm).expect(info.short_name);
        let mut ours = mt32_rs::engine::Engine::open(control, &pcm).expect(info.short_name);
        let rate = mt32_rs::SAMPLE_RATE as usize;
        let mut at = 0usize;
        let quiet = format!("{}: power-on quiet", info.short_name);
        assert_frames_match(&mut oracle, &mut ours, rate / 4, &mut at, &quiet);
        oracle.play_msg(0x007F_3C91);
        ours.play_msg(0x007F_3C91);
        let held = format!("{}: the note held", info.short_name);
        assert_frames_match(&mut oracle, &mut ours, rate, &mut at, &held);
        oracle.play_msg(0x0000_3C81);
        ours.play_msg(0x0000_3C81);
        let tail = format!("{}: the release tail", info.short_name);
        assert_frames_match(&mut oracle, &mut ours, rate, &mut at, &tail);
        checked += 1;
        println!("  {:<18} {at} frames identical", info.short_name);
    }
    assert!(checked > 0, "no control ROMs to check");
}

/// A crowded scene against the reference: chords across three parts and
/// the drums, the pedal held through a shower of notes that overruns the
/// thirty-two partials -- voice stealing, and the render loop dropping to
/// one frame at a time while a poly aborts -- a program change mid-note,
/// bend, modulation and expression, the pedal released, one part cut off
/// by All Notes Off. Every controller and allocator seam the engine has,
/// in one differential.
#[test]
#[ignore = "needs a ROM pair in MT32_RS_ROMS"]
fn a_crowded_scene_matches_the_reference_engine() {
    let Some((control, pcm)) = rom_pair() else {
        panic!("MT32_RS_ROMS is not set or the pair is unreadable");
    };
    let mut oracle = Oracle::open(&control, &pcm).expect("the reference engine opens");
    let mut ours = mt32_rs::engine::Engine::open(&control, &pcm).expect("the engine opens");
    let rate = mt32_rs::SAMPLE_RATE as usize;
    let mut at = 0usize;

    // Reverb off everywhere a voice will sound: the eight melodic parts,
    // and the three rhythm keys the pattern uses (each key's fourth byte,
    // all comfortably below the SysEx address bytes' 0x80 carry).
    for part in 0..8u32 {
        let msg = mt32_rs::sysex::dt1(0x03_0000 + part * 0x10 + 6, &[0]);
        oracle.play_sysex(&msg);
        ours.play_sysex(&msg);
    }
    for addr in [0x03_0143u32, 0x03_014B, 0x03_015B] {
        let msg = mt32_rs::sysex::dt1(addr, &[0]);
        oracle.play_sysex(&msg);
        ours.play_sysex(&msg);
    }

    let play = |oracle: &mut Oracle, ours: &mut mt32_rs::engine::Engine, msg: u32| {
        oracle.play_msg(msg);
        ours.play_msg(msg);
    };

    // Chords on three parts at three velocities.
    for note in [0x30u32, 0x34, 0x37, 0x3C] {
        play(&mut oracle, &mut ours, 0x0064_0091 | (note << 8));
    }
    for note in [0x28u32, 0x2F] {
        play(&mut oracle, &mut ours, 0x0050_0092 | (note << 8));
    }
    for note in [0x41u32, 0x45, 0x48] {
        play(&mut oracle, &mut ours, 0x007F_0093 | (note << 8));
    }
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 2,
        &mut at,
        "three parts chording",
    );

    // Kick, snare and hat on the rhythm channel.
    play(&mut oracle, &mut ours, 0x007F_2499);
    play(&mut oracle, &mut ours, 0x0064_2699);
    play(&mut oracle, &mut ours, 0x005A_2A99);
    assert_frames_match(&mut oracle, &mut ours, rate / 4, &mut at, "the drums in");

    // Pedal down, five notes struck and released into it.
    play(&mut oracle, &mut ours, 0x007F_40B1);
    for note in [0x3Eu32, 0x40, 0x43, 0x47, 0x4A] {
        play(&mut oracle, &mut ours, 0x006E_0091 | (note << 8));
    }
    for note in [0x3Eu32, 0x40, 0x43, 0x47, 0x4A] {
        play(&mut oracle, &mut ours, 0x0000_0081 | (note << 8));
    }
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 4,
        &mut at,
        "held by the pedal",
    );

    // The shower: a dozen more held notes overrun the chip's thirty-two
    // partials, and the books have to steal the same voices.
    for note in [
        0x24u32, 0x26, 0x29, 0x2B, 0x2D, 0x32, 0x35, 0x39, 0x3A, 0x3F, 0x46, 0x4D,
    ] {
        play(&mut oracle, &mut ours, 0x0078_0091 | (note << 8));
    }
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 2,
        &mut at,
        "the shower past thirty-two",
    );

    // A program change lands on part 2 mid-note: its voices abort.
    play(&mut oracle, &mut ours, 0x0000_0AC2);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 4,
        &mut at,
        "a program change mid-note",
    );

    // Bend part 1 to the stop, modulation on part 3, expression down.
    play(&mut oracle, &mut ours, 0x007F_7FE1);
    play(&mut oracle, &mut ours, 0x0050_01B3);
    play(&mut oracle, &mut ours, 0x0040_0BB1);
    assert_frames_match(&mut oracle, &mut ours, rate / 4, &mut at, "bent and shaped");

    // The pedal comes up: everything it held releases at once.
    play(&mut oracle, &mut ours, 0x0000_40B1);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 2,
        &mut at,
        "the pedal released",
    );

    // All Notes Off cuts part 3; the rest ring out.
    play(&mut oracle, &mut ours, 0x0000_7BB3);
    assert_frames_match(&mut oracle, &mut ours, rate, &mut at, "the long tail");

    assert_eq!(ours.rendered_sample_count(), at as u32);
    println!("  {at} frames identical through the crowd");
}

extern "C" {
    fn mt32_oracle_reverb_new(mode: c_int, mt32_compatible: c_int) -> *mut std::ffi::c_void;
    fn mt32_oracle_reverb_free(model: *mut std::ffi::c_void);
    fn mt32_oracle_reverb_set(model: *mut std::ffi::c_void, time: u8, level: u8);
    fn mt32_oracle_reverb_process(
        model: *mut std::ffi::c_void,
        in_l: *const i16,
        in_r: *const i16,
        out_l: *mut i16,
        out_r: *mut i16,
        frames: c_uint,
    );
    fn mt32_oracle_reverb_active(model: *const std::ffi::c_void) -> c_int;
}

/// The Boss reverb against the reference: both chip flavours, all four
/// programs, the whole time-and-level grid under full-range deterministic
/// noise; then a deep ring per program that wraps the longest store,
/// re-aims the running parameters mid-flight and drains to quiet with the
/// activity flag agreeing throughout. Needs no ROMs: the tables are
/// traced constants both engines carry.
#[test]
fn the_reverb_chip_matches_the_reference_engine() {
    let noise = |seed: u32, n: usize| -> Vec<i16> {
        (0..n)
            .map(|i| ((i as u32).wrapping_add(seed).wrapping_mul(2654435761) >> 16) as i16)
            .collect()
    };
    let mut compared = 0u64;
    let mut run = |theirs: *mut std::ffi::c_void,
                   ours: &mut mt32_rs::reverb::Reverb,
                   in_l: &[i16],
                   in_r: &[i16],
                   what: &str| {
        let n = in_l.len();
        let mut their_l = vec![0i16; n];
        let mut their_r = vec![0i16; n];
        unsafe {
            mt32_oracle_reverb_process(
                theirs,
                in_l.as_ptr(),
                in_r.as_ptr(),
                their_l.as_mut_ptr(),
                their_r.as_mut_ptr(),
                n as c_uint,
            )
        };
        let mut our_l = vec![0i16; n];
        let mut our_r = vec![0i16; n];
        ours.process(in_l, in_r, &mut our_l, &mut our_r);
        for i in 0..n {
            assert_eq!(
                (our_l[i], our_r[i]),
                (their_l[i], their_r[i]),
                "{what}, sample {i}"
            );
        }
        compared += 2 * n as u64;
    };

    for mt32 in [false, true] {
        let flavour = if mt32 { "mt32" } else { "cm32l" };
        for mode in 0..4u8 {
            for time in 0..8u8 {
                for level in 0..8u8 {
                    let theirs = unsafe { mt32_oracle_reverb_new(i32::from(mode), mt32 as c_int) };
                    let mut ours = mt32_rs::reverb::Reverb::new(mode, mt32);
                    unsafe { mt32_oracle_reverb_set(theirs, time, level) };
                    ours.set_parameters(time, level);
                    run(
                        theirs,
                        &mut ours,
                        &noise(11, 1024),
                        &noise(47, 1024),
                        &format!("{flavour} mode {mode} time {time} level {level}"),
                    );
                    unsafe { mt32_oracle_reverb_free(theirs) };
                }
            }

            // The deep ring: wrap even the tap delay's store, change the
            // running time and level without a reset, drain to quiet.
            let theirs = unsafe { mt32_oracle_reverb_new(i32::from(mode), mt32 as c_int) };
            let mut ours = mt32_rs::reverb::Reverb::new(mode, mt32);
            unsafe { mt32_oracle_reverb_set(theirs, 5, 3) };
            ours.set_parameters(5, 3);
            run(
                theirs,
                &mut ours,
                &noise(3, 40_000),
                &noise(7, 40_000),
                &format!("{flavour} mode {mode} deep ring"),
            );
            unsafe { mt32_oracle_reverb_set(theirs, 2, 6) };
            ours.set_parameters(2, 6);
            run(
                theirs,
                &mut ours,
                &noise(23, 8_000),
                &noise(31, 8_000),
                &format!("{flavour} mode {mode} re-aimed mid-ring"),
            );
            let quiet = vec![0i16; 8_000];
            let mut drained = false;
            for block in 0..40 {
                run(
                    theirs,
                    &mut ours,
                    &quiet,
                    &quiet,
                    &format!("{flavour} mode {mode} drain block {block}"),
                );
                let their_active = unsafe { mt32_oracle_reverb_active(theirs) } != 0;
                assert_eq!(
                    ours.is_active(),
                    their_active,
                    "{flavour} mode {mode} activity after drain block {block}"
                );
                if !their_active {
                    drained = true;
                    break;
                }
            }
            assert!(drained, "{flavour} mode {mode} never drained to quiet");
            unsafe { mt32_oracle_reverb_free(theirs) };
        }
    }
    println!("  {compared} wet samples identical");
}

/// A reverberant scene against the reference: the chip selected at power
/// on rings a note's tail past the voice's death, time and level re-aim
/// the running chip without a reset, a mode change powers a fresh chip
/// mid-note, zero time and level switch the chip out and back -- and the
/// 0x7F reset, which kills every voice on the spot, leaves the same
/// mode's tail ringing straight through, exactly as the firmware does.
#[test]
#[ignore = "needs a ROM pair in MT32_RS_ROMS"]
fn a_reverberant_scene_matches_the_reference_engine() {
    let Some((control, pcm)) = rom_pair() else {
        panic!("MT32_RS_ROMS is not set or the pair is unreadable");
    };
    let mut oracle = Oracle::open(&control, &pcm).expect("the reference engine opens");
    let mut ours = mt32_rs::engine::Engine::open(&control, &pcm).expect("the engine opens");
    let rate = mt32_rs::SAMPLE_RATE as usize;
    let mut at = 0usize;
    let play = |oracle: &mut Oracle, ours: &mut mt32_rs::engine::Engine, msg: u32| {
        oracle.play_msg(msg);
        ours.play_msg(msg);
    };
    let write =
        |oracle: &mut Oracle, ours: &mut mt32_rs::engine::Engine, addr: u32, data: &[u8]| {
            let msg = mt32_rs::sysex::dt1(addr, data);
            oracle.play_sysex(&msg);
            ours.play_sysex(&msg);
        };

    // Middle C into the power-on room; the tail rings past the voice.
    play(&mut oracle, &mut ours, 0x007F_3C91);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate,
        &mut at,
        "the note in the room",
    );
    play(&mut oracle, &mut ours, 0x0000_3C81);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate,
        &mut at,
        "the tail past the voice",
    );

    // Re-aim the running chip -- a longer time, then the top level. No
    // reset: the ring carries straight on.
    write(&mut oracle, &mut ours, 0x10_0002, &[7]);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 4,
        &mut at,
        "time re-aimed mid-ring",
    );
    write(&mut oracle, &mut ours, 0x10_0003, &[7]);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 4,
        &mut at,
        "level re-aimed mid-ring",
    );

    // A fresh note, and a mode change under it: a fresh chip mid-note.
    play(&mut oracle, &mut ours, 0x0060_4091);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 2,
        &mut at,
        "a note into the re-aimed room",
    );
    write(&mut oracle, &mut ours, 0x10_0001, &[2, 3, 5]);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 2,
        &mut at,
        "a fresh chip mid-note",
    );
    play(&mut oracle, &mut ours, 0x0000_4081);

    // The chip out -- the wet dies on the instant -- and back, fresh.
    write(&mut oracle, &mut ours, 0x10_0002, &[0, 0]);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 4,
        &mut at,
        "the chip switched out",
    );
    write(&mut oracle, &mut ours, 0x10_0002, &[5, 3]);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 4,
        &mut at,
        "the chip switched back",
    );

    // Back to the power-on mode, a chord ringing, and the 0x7F reset.
    write(&mut oracle, &mut ours, 0x10_0001, &[0]);
    for note in [0x3Cu32, 0x40, 0x43] {
        play(&mut oracle, &mut ours, 0x0070_0091 | (note << 8));
    }
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate / 2,
        &mut at,
        "a chord before the reset",
    );
    write(&mut oracle, &mut ours, 0x7F_0000, &[]);
    assert_frames_match(
        &mut oracle,
        &mut ours,
        rate,
        &mut at,
        "the tail through the reset",
    );

    assert_eq!(ours.rendered_sample_count(), at as u32);
    println!("  {at} frames identical through the rooms");
}

extern "C" {
    fn mt32_oracle_parser_new() -> *mut std::ffi::c_void;
    fn mt32_oracle_parser_free(parser: *mut std::ffi::c_void);
    fn mt32_oracle_parser_parse(parser: *mut std::ffi::c_void, stream: *const u8, len: c_uint);
    fn mt32_oracle_parser_take_log(
        parser: *mut std::ffi::c_void,
        out: *mut u8,
        cap: c_uint,
    ) -> c_uint;
}

/// The stream parser against the reference: scripted gauntlets through
/// every path -- running status carried and cleared, Realtime lodged
/// inside everything, SysEx whole, split, aborted, oversized whole and
/// oversized fragmented -- then a long pseudo-random byte soup parsed in
/// awkward chunk sizes, the tagged event logs compared after every call.
/// Needs no ROMs: the parser stands alone on both sides.
#[test]
fn the_stream_parser_matches_the_reference_engine() {
    struct Log(Vec<u8>);
    impl mt32_rs::midi::Sink for Log {
        fn short_message(&mut self, message: u32) {
            self.0.push(1);
            self.0.extend_from_slice(&message.to_le_bytes());
        }
        fn sysex(&mut self, frame: &[u8]) {
            self.0.push(2);
            self.0
                .extend_from_slice(&(frame.len() as u32).to_le_bytes());
            self.0.extend_from_slice(frame);
        }
        fn realtime(&mut self, byte: u8) {
            self.0.push(3);
            self.0.push(byte);
        }
        fn dropped(&mut self, _what: &'static str) {
            self.0.push(4);
        }
    }

    let theirs = unsafe { mt32_oracle_parser_new() };
    let mut ours = mt32_rs::midi::Parser::new();
    let mut log = Log(Vec::new());
    let mut compared = 0u64;
    let mut feed = |chunk: &[u8], what: &str| {
        unsafe { mt32_oracle_parser_parse(theirs, chunk.as_ptr(), chunk.len() as c_uint) };
        ours.parse(chunk, &mut log);
        let mut reference = vec![0u8; log.0.len().max(1)];
        let got = unsafe {
            mt32_oracle_parser_take_log(theirs, reference.as_mut_ptr(), log.0.len() as c_uint)
        };
        assert_eq!(got as usize, log.0.len(), "{what}: event log length");
        reference.truncate(got as usize);
        assert_eq!(log.0, reference, "{what}: event log");
        compared += log.0.len() as u64;
        log.0.clear();
    };

    // The scripted gauntlet, one path at a time.
    feed(&[0x91, 0x3C, 0x7F, 0x40, 0x7F], "running status carries");
    feed(&[0x3C, 0x00], "running status across calls");
    feed(&[0x40], "a data byte with no message ahead of it");
    feed(
        &[0x00, 0xC1, 0x0A, 0x05],
        "program change, then borrowed again",
    );
    feed(&[0xF6], "tune request buffered at a call boundary");
    feed(&[0x3C, 0x40], "flushes on the next call, then drops data");
    feed(
        &[0x92, 0x30, 0xFE, 0x50, 0xFA],
        "realtime inside a short message",
    );
    feed(
        &[0x93, 0x30, 0x94, 0x40, 0x60],
        "a message cut off by a status",
    );
    feed(&[0xF0, 0x41, 0x10, 0x16, 0x12, 0x01, 0xF7], "a SysEx whole");
    feed(&[0xF0, 0x41], "a SysEx opens");
    feed(&[0x10, 0xF8, 0x16], "grows around a realtime");
    feed(
        &[0xF7, 0x91, 0x3C, 0x40],
        "and closes, with a note behind it",
    );
    feed(&[0xF0, 0x41, 0x92, 0x30, 0x60], "a SysEx aborted at open");
    feed(&[0xF0, 0x41, 0x10], "a SysEx opens again");
    feed(&[0x93, 0x30, 0x60], "and dies to a status mid-fragment");
    feed(
        &[0xF1, 0x30, 0x3C],
        "system common eats one byte, drops the next",
    );

    // An oversized frame in one call is delivered; fragmented, dropped.
    let mut giant = vec![0xF0];
    giant.extend(std::iter::repeat_n(0x55u8, 40_000));
    giant.push(0xF7);
    feed(&giant, "an oversized SysEx whole in one call");
    feed(&giant[..30_000], "an oversized SysEx starts fragmented");
    feed(&giant[30_000..], "and overruns the store");

    // The byte soup: every state crossed against every input class, in
    // chunk sizes that never line up with message boundaries.
    let soup: Vec<u8> = (0..20_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    let mut sizes = [1usize, 2, 3, 5, 7, 11, 127].into_iter().cycle();
    let mut fed = 0;
    while fed < soup.len() {
        let chunk = sizes.next().unwrap().min(soup.len() - fed);
        feed(&soup[fed..fed + chunk], "the byte soup");
        fed += chunk;
    }

    unsafe { mt32_oracle_parser_free(theirs) };
    println!("  {compared} event-log bytes identical");
}
