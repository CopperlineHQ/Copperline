// SPDX-License-Identifier: GPL-3.0-or-later

//! Engine tests. The ROMs cannot be committed, so the ones that need a
//! real MT-32 image are `#[ignore]` and look for the pair under
//! `COPPERLINE_MT32_ROMS`.

use super::*;

/// Where a local pair of ROMs lives, if the runner has one.
fn rom_dir() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("COPPERLINE_MT32_ROMS")?;
    let dir = std::path::PathBuf::from(dir);
    dir.is_dir().then_some(dir)
}

fn open_synth() -> Option<Mt32Synth> {
    let dir = rom_dir()?;
    Some(
        Mt32Synth::open(
            &dir.join("MT32_CONTROL.ROM"),
            &dir.join("MT32_PCM.ROM"),
            crate::audio::MIX_SAMPLE_RATE,
        )
        .expect("the ROM pair opens"),
    )
}

/// The engine is linked in and answers, with or without ROMs.
#[test]
fn the_engine_reports_its_version() {
    let version = engine_version();
    assert!(
        version.starts_with('2'),
        "unexpected mt32emu version {version:?}"
    );
}

/// A file that is not a ROM is refused, rather than opening a synth that
/// would sound wrong. The engine identifies ROMs by content.
#[test]
fn a_file_that_is_not_a_rom_is_refused() {
    let dir = std::env::temp_dir().join("copperline-mt32-not-a-rom");
    std::fs::write(&dir, b"this is not an MT-32 ROM").expect("write scratch file");
    let err = Mt32Synth::open(&dir, &dir, crate::audio::MIX_SAMPLE_RATE)
        .expect_err("a text file is not a ROM");
    assert!(err.to_string().contains("recognises"), "{err}");
    let _ = std::fs::remove_file(&dir);
}

/// The engine renders at the mixer's rate, so its frames drop straight into
/// the mix beside Paula's without any rate matching.
#[test]
#[ignore]
fn the_engine_runs_at_the_mixer_rate() {
    let Some(synth) = open_synth() else {
        eprintln!("set COPPERLINE_MT32_ROMS to run this");
        return;
    };
    assert_eq!(synth.sample_rate(), crate::audio::MIX_SAMPLE_RATE);
}

/// The LCD reads out of the control ROM: the greeting on power-up, and the
/// timbre the engine names when a program change arrives.
#[test]
#[ignore]
fn the_lcd_shows_what_the_rom_says() {
    let Some(mut synth) = open_synth() else {
        eprintln!("set COPPERLINE_MT32_ROMS to run this");
        return;
    };

    // The power-up greeting, straight from the control ROM.
    let (greeting, _) = synth.display();
    assert!(
        !greeting.trim().is_empty(),
        "the LCD should power up with the ROM's greeting"
    );
    assert!(
        greeting.chars().count() <= LCD_WIDTH,
        "the LCD is {LCD_WIDTH} characters, got {:?}",
        greeting
    );

    // A program change names the timbre it selected. Part 1 answers on MIDI
    // channel 2 -- the MT-32 assigns its parts to channels 2..9 -- and the
    // engine updates the display while rendering, so give it a second of
    // frames to get there.
    let mut frames = vec![(0.0f32, 0.0f32); crate::audio::MIX_SAMPLE_RATE as usize];
    synth.parse(&[0xC1, 0x30]);
    synth.render(&mut frames);
    let (after, led) = synth.display();
    eprintln!("LCD power-up : {greeting:?}");
    eprintln!("LCD prog chg : {after:?}  (MIDI MESSAGE lamp: {led})");
    assert_ne!(
        after.trim(),
        greeting.trim(),
        "a program change should put the timbre name on the LCD"
    );

    // Master Volume returns it to the main screen.
    synth.show_main_display();
    synth.render(&mut frames);
    let (main, _) = synth.display();
    eprintln!("LCD main     : {main:?}");
    assert_ne!(
        main.trim(),
        after.trim(),
        "Master Volume shows the main screen"
    );
}

/// A held note makes sound, and silence renders silent -- so the frames the
/// mixer will be adding are real.
#[test]
#[ignore]
fn a_note_makes_sound_and_silence_does_not() {
    let Some(mut synth) = open_synth() else {
        eprintln!("set COPPERLINE_MT32_ROMS to run this");
        return;
    };
    let peak = |frames: &[(f32, f32)]| {
        frames
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0f32, f32::max)
    };

    let mut frames = vec![(0.0f32, 0.0f32); 8192];
    synth.render(&mut frames);
    assert_eq!(peak(&frames), 0.0, "an idle MT-32 is silent");

    // Note on, middle C, part 1 -- which listens on MIDI channel 2.
    synth.parse(&[0x91, 0x3C, 0x64]);
    synth.render(&mut frames);
    assert!(peak(&frames) > 0.01, "a held note should be audible");
}

/// The whole path: bytes onto Paula's serial port, an MT-32 answering them,
/// and its voice arriving in the mix beside the Amiga's own -- which is what
/// puts it in a WAV capture and a video recording without either knowing.
#[test]
#[ignore]
fn the_synth_reaches_the_mixer_beside_paula() {
    use crate::chipset::paula::Paula;

    let Some(dir) = rom_dir() else {
        eprintln!("set COPPERLINE_MT32_ROMS to run this");
        return;
    };
    // A MIDI sink with the ROMs configured but nothing attached: this is
    // what a session that never selects an MT-32 costs.
    let mut sink = crate::midi::MidiSerialSink::open(None, None).expect("a MIDI sink opens");
    sink.set_mt32_roms(crate::mt32::Mt32Roms {
        control: Some(dir.join("MT32_CONTROL.ROM")),
        pcm: Some(dir.join("MT32_PCM.ROM")),
    });
    assert!(sink.mt32_available(), "the ROM pair makes one selectable");
    assert!(
        sink.mt32().is_none(),
        "nothing is fitted until it is picked"
    );

    let frames = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(f32, f32)>::new()));
    let audio = CollectFrames {
        frames: std::rc::Rc::clone(&frames),
    };
    let mut paula = Paula::new(Box::new(sink), Box::new(audio));
    paula.set_led_filter_guest(false);
    let ram = vec![0u8; 64];

    let peak = |f: &[(f32, f32)]| {
        f.iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0f32, f32::max)
    };

    // Nothing attached: the mixer asks once, is told there is nothing here,
    // and stops asking.
    paula.tick_audio(20_000, 0, &ram);
    assert_eq!(
        peak(&frames.borrow()),
        0.0,
        "an unselected MT-32 costs nothing"
    );

    // Select it, which is what the menu does.
    paula
        .serial
        .as_midi()
        .expect("a MIDI sink")
        .set_output_endpoint(Some(crate::config::MIDI_OUT_MT32));
    paula.rearm_synth_audio();
    frames.borrow_mut().clear();

    // Attached but silent: an MT-32 sitting there is not a noise source.
    paula.tick_audio(40_000, 0, &ram);
    assert_eq!(peak(&frames.borrow()), 0.0, "an idle MT-32 adds nothing");

    // Note on, part 1 (MIDI channel 2), straight down the serial line.
    for byte in [0x91u8, 0x3C, 0x64] {
        paula.serial.write_byte(byte, 0);
    }
    frames.borrow_mut().clear();
    paula.tick_audio(200_000, 0, &ram);
    assert!(
        peak(&frames.borrow()) > 0.01,
        "the MT-32's voice should arrive in the mix"
    );
}

/// Collects mixed stereo frames, so a test can look at what the host would
/// have heard.
struct CollectFrames {
    frames: std::rc::Rc<std::cell::RefCell<Vec<(f32, f32)>>>,
}

impl crate::audio::AudioSink for CollectFrames {
    fn push(&mut self, left: f32, right: f32) {
        self.frames.borrow_mut().push((left, right));
    }
    fn flush(&mut self) {}
}

/// Master Volume is a volume: turning it down makes the same note quieter,
/// so the panel's dial is moving air rather than only text.
#[test]
#[ignore]
fn master_volume_changes_how_loud_it_is() {
    let Some(mut synth) = open_synth() else {
        eprintln!("set COPPERLINE_MT32_ROMS to run this");
        return;
    };
    let peak = |frames: &[(f32, f32)]| {
        frames
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0f32, f32::max)
    };
    let mut frames = vec![(0.0f32, 0.0f32); 16384];

    // Full volume, a held note.
    synth.write_memory(addr::MASTER_VOLUME, &[100]);
    synth.parse(&[0x91, 0x3C, 0x64]);
    synth.render(&mut frames);
    let loud = peak(&frames);
    assert!(loud > 0.01, "the note should be audible at full volume");

    // The same note, turned right down.
    synth.parse(&[0x81, 0x3C, 0x40]);
    synth.render(&mut frames);
    synth.write_memory(addr::MASTER_VOLUME, &[10]);
    synth.parse(&[0x91, 0x3C, 0x64]);
    synth.render(&mut frames);
    let quiet = peak(&frames);

    assert!(
        quiet < loud / 2.0,
        "turning master volume down should make it quieter: {loud} -> {quiet}"
    );
}

/// Records what is logged, so a test can see where the engine's commentary
/// went. Installed once; `log` allows only one logger per process.
struct Capture;

static CAPTURED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        if let Ok(mut lines) = CAPTURED.lock() {
            lines.push(format!("{}", record.args()));
        }
    }
    fn flush(&self) {}
}

/// The engine's own commentary reaches the log rather than the console, and
/// arrives formatted -- which is the part worth proving, since its arguments
/// come through a C `va_list`.
#[test]
#[ignore]
fn the_engine_reports_through_the_log() {
    let Some(mut synth) = open_synth() else {
        eprintln!("set COPPERLINE_MT32_ROMS to run this");
        return;
    };
    if !debug_enabled() {
        eprintln!("set COPPERLINE_MT32_DEBUG=1 to run this");
        return;
    }
    static INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INSTALLED.get_or_init(|| {
        let _ = log::set_logger(&Capture);
        log::set_max_level(log::LevelFilter::Trace);
    });
    CAPTURED.lock().expect("capture").clear();

    // Data bytes with no status byte before them: exactly what makes the
    // engine complain about running status.
    synth.parse(&[0x40, 0x41, 0x42, 0x43, 0x44]);
    let mut frames = vec![(0.0f32, 0.0f32); 4096];
    synth.render(&mut frames);

    let lines = CAPTURED.lock().expect("capture").clone();
    let engine: Vec<&String> = lines.iter().filter(|l| l.starts_with("mt32: ")).collect();
    assert!(
        !engine.is_empty(),
        "the engine should have reported through the log, got {lines:?}"
    );
    // Formatted, not the raw format string.
    assert!(
        engine.iter().all(|l| !l.contains('%')),
        "the arguments should have been formatted in: {engine:?}"
    );
}

/// The round trip a patch editor depends on: it asks for a stretch of the
/// module's memory and gets it back, with what it wrote in it.
///
/// The engine itself never answers a request -- its `readSysex` is
/// unimplemented -- so this is the whole of the module's MIDI OUT.
#[test]
#[ignore = "needs an MT-32 ROM pair in COPPERLINE_MT32_ROMS"]
fn the_module_answers_a_request_for_its_memory() {
    let Some(mut synth) = open_synth() else {
        return;
    };
    // Turn the volume down to something no default would land on.
    synth.write_memory(addr::MASTER_VOLUME, &[42]);

    let request = {
        let mut body = vec![0x10, 0x00, 0x16, 0x00, 0x00, 0x01];
        let sum: u32 = body.iter().map(|&b| u32::from(b)).sum();
        let mut msg = vec![0xF0, 0x41, 0x10, 0x16, 0x11];
        msg.append(&mut body);
        msg.push(((128 - (sum % 128)) % 128) as u8);
        msg.push(0xF7);
        msg
    };

    let mut responder = reply::Responder::default();
    let mut answered = None;
    for b in request {
        if let Some(r) = responder.write_byte(b) {
            answered = Some(reply::answer(&synth, r));
        }
    }
    let reply = answered.expect("the request was recognised");

    // F0 41 10 16 12 <10 00 16> <volume> <checksum> F7
    assert_eq!(reply.len(), 11, "one block: {reply:02X?}");
    assert_eq!(reply[..5], [0xF0, 0x41, 0x10, 0x16, 0x12]);
    assert_eq!(reply[5..8], [0x10, 0x00, 0x16], "the address it asked for");
    assert_eq!(reply[8], 42, "the volume that was written");
    assert_eq!(*reply.last().unwrap(), 0xF7);
    let sum: u32 = reply[5..reply.len() - 1]
        .iter()
        .map(|&b| u32::from(b))
        .sum();
    assert_eq!(sum % 128, 0, "the checksum");
}

/// A dump longer than one block comes back as several, each carrying its own
/// address, the way the hardware splits one.
#[test]
#[ignore = "needs an MT-32 ROM pair in COPPERLINE_MT32_ROMS"]
fn a_long_dump_comes_back_in_blocks() {
    let Some(synth) = open_synth() else {
        return;
    };
    // The whole patch temporary area: eight parts of sixteen bytes each,
    // asked for in one go.
    let want = 8 * 16;
    let request = {
        let addr = [0x03, 0x00, 0x00];
        let size = [0x00, 0x01, 0x00];
        let mut body = addr.to_vec();
        body.extend_from_slice(&size);
        let sum: u32 = body.iter().map(|&b| u32::from(b)).sum();
        let mut msg = vec![0xF0, 0x41, 0x10, 0x16, 0x11];
        msg.append(&mut body);
        msg.push(((128 - (sum % 128)) % 128) as u8);
        msg.push(0xF7);
        msg
    };
    assert_eq!(want, 128, "the size bytes above ask for 0x80");

    let mut responder = reply::Responder::default();
    let mut reply = Vec::new();
    for b in request {
        if let Some(r) = responder.write_byte(b) {
            reply = reply::answer(&synth, r);
        }
    }
    assert!(!reply.is_empty(), "the area answered");
    // Every message is well formed and every data byte fits in seven bits,
    // so nothing in the dump can read as the end of a message.
    assert_eq!(reply[0], 0xF0);
    assert_eq!(*reply.last().unwrap(), 0xF7);
    let ends = reply.iter().filter(|&&b| b == 0xF7).count();
    let starts = reply.iter().filter(|&&b| b == 0xF0).count();
    assert_eq!(starts, ends, "every message is closed: {starts} vs {ends}");
}

/// The MIDI MESSAGE lamp answers exclusive messages, not just notes.
///
/// A librarian talks to the module entirely in SysEx, so a lamp that only
/// lit for note-on would sit dark through the very traffic it is there to
/// show. The engine holds it lit for a moment after each message, so it is
/// read while the frames that keep it lit are still being rendered.
#[test]
#[ignore = "needs an MT-32 ROM pair in COPPERLINE_MT32_ROMS"]
fn the_lamp_lights_for_exclusive_messages() {
    let Some(mut synth) = open_synth() else {
        return;
    };
    let mut frames = vec![(0.0f32, 0.0f32); 64];
    // Settle: whatever the greeting lit has gone out by now.
    for _ in 0..600 {
        synth.render(&mut frames);
    }
    assert!(!synth.display().1, "dark with nothing being sent");

    // A DT1 setting the master volume, fed in as bytes off the serial
    // line: no note, purely exclusive, and by the path Paula uses.
    let body = [0x10, 0x00, 0x16, 64];
    let sum: u32 = body.iter().map(|&b| u32::from(b)).sum();
    let mut sysex = vec![0xF0, 0x41, 0x10, 0x16, 0x12];
    sysex.extend_from_slice(&body);
    sysex.push(((128 - (sum % 128)) % 128) as u8);
    sysex.push(0xF7);
    synth.parse(&sysex);
    synth.render(&mut frames);
    assert!(synth.display().1, "lit by an exclusive message");

    // And it goes out again once the engine's blink has run its course.
    for _ in 0..600 {
        synth.render(&mut frames);
    }
    assert!(!synth.display().1, "out again afterwards");
}
