// SPDX-License-Identifier: GPL-3.0-or-later

//! The MT-32, emulated by our own engine,
//! <https://github.com/CopperlineHQ/mt32-rs>.
//!
//! Paula's serial bytes go straight into the engine and stereo frames come
//! back at the mixer's rate, so nothing passes through the host's MIDI stack:
//! no IAC bus on macOS, no loopMIDI on Windows, no ALSA sequencer client.
//! The engine renders through the accurate analogue model at its 48 kHz;
//! the resampler here carries that to the mixer's rate, the one
//! conversion between the module and the mix.
//!
//! The engine needs two ROM images, a control ROM and a PCM ROM, which are
//! not Copperline's to ship and so are the user's to supply. Without them the
//! machine still runs; it simply has nothing on the far end of the cable.

use anyhow::{anyhow, Result};
use std::path::Path;

mod resample;
pub mod rom;
mod sink;

pub use sink::{Mt32Device, Mt32Roms};

/// The songs the second-generation control ROMs carry, and the player that
/// paces them; the engine crate reads both out of the image.
pub use mt32_rs::demo;

/// What the engine puts in a part's place on the main screen while that
/// part is sounding. On the hardware it is a character the display
/// controller is given a shape of its own, a filled cell, so it is drawn as
/// one rather than looked for in a font.
pub const ACTIVE_PART: char = '\u{1}';

/// How wide the emulated LCD is, in characters. The panel is drawn to this.
pub const LCD_WIDTH: usize = mt32_rs::LCD_WIDTH;

/// How many native frames the engine is asked for at a time on the way
/// into the resampler: a millisecond, so queued guest traffic lands
/// within that of the frame the mixer is pulling.
const NATIVE_BLOCK: usize = 32;

/// Whether to log everything the MT-32 does (`COPPERLINE_MT32_DEBUG=1`).
///
/// Off, the module says nothing. The stream off a guest's serial line
/// carries whatever the guest emits, so bytes the module drops are
/// ordinary and say nothing about Copperline.
pub(crate) fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::envcfg::flag("COPPERLINE_MT32_DEBUG"))
}

/// A running MT-32. Dropping it switches the module off.
pub struct Mt32Synth {
    engine: mt32_rs::engine::Engine,
    parser: mt32_rs::midi::Parser,
    /// The rate frames leave at, which is what the mixer asked for.
    sample_rate: u32,
    resampler: resample::Resampler,
    /// Native frames rendered and not yet resampled, and how far the
    /// resampler has drunk from them.
    native: Vec<(i16, i16)>,
    taken: usize,
}

impl Mt32Synth {
    /// Fit an MT-32 with the given ROM images, producing at `sample_rate`.
    ///
    /// The engine identifies a ROM by its content rather than its name, so
    /// a mislabelled or truncated file is rejected here rather than
    /// producing a synth that sounds subtly wrong.
    pub fn open(control_rom: &Path, pcm_rom: &Path, sample_rate: u32) -> Result<Self> {
        let control = read_rom(control_rom, "control")?;
        let pcm = read_rom(pcm_rom, "PCM")?;
        // The accurate analogue model: the output stage the LA32 fed on
        // real hardware, imaging and all, leaving at its own 48 kHz.
        let engine = mt32_rs::engine::Engine::open_with_analog(
            &control,
            &pcm,
            mt32_rs::analog::AnalogMode::Accurate,
        )
        .map_err(|e| anyhow!("the MT-32 engine refused the ROM pair: {e}"))?;
        Ok(Self {
            resampler: resample::Resampler::new(engine.output_sample_rate(), sample_rate),
            engine,
            parser: mt32_rs::midi::Parser::new(),
            sample_rate,
            native: Vec::new(),
            taken: 0,
        })
    }

    /// The rate the module is producing at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Feed raw MIDI bytes, exactly as they arrived on Paula's serial line.
    /// Running status and split messages are the stream parser's problem,
    /// which is what it is for.
    pub fn parse(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if debug_enabled() {
            log::debug!(
                "mt32: in@{} {bytes:02X?}",
                self.engine.rendered_sample_count()
            );
        }
        let mut sink = EngineSink {
            engine: &mut self.engine,
        };
        self.parser.parse(bytes, &mut sink);
    }

    /// Render `frames.len()` stereo frames at the mixer's rate, normalised
    /// for the mix. The engine runs at its native rate underneath; blocks
    /// of it are pulled through the resampler as the output needs them.
    pub fn render(&mut self, frames: &mut [(f32, f32)]) {
        let Self {
            engine,
            resampler,
            native,
            taken,
            ..
        } = self;
        for frame in frames.iter_mut() {
            *frame = resampler.next(|| {
                if *taken == native.len() {
                    native.clear();
                    native.resize(NATIVE_BLOCK, (0, 0));
                    engine.render(native);
                    *taken = 0;
                }
                let (l, r) = native[*taken];
                *taken += 1;
                (f32::from(l) / 32768.0, f32::from(r) / 32768.0)
            });
        }
    }

    /// What the LCD reads and whether the MIDI MESSAGE lamp is lit.
    ///
    /// Both come from the engine, which draws the text out of the control
    /// ROM: the greeting it powers up with, the timbre names it shows on a
    /// program change, whatever a program puts there over SysEx, and its
    /// own checksum errors.
    pub fn display(&mut self) -> (String, bool) {
        let (text, lamp) = self.display_raw();
        let text = text
            .iter()
            .map(|&b| if b == 1 { ACTIVE_PART } else { b as char })
            .collect::<String>()
            .trim_end()
            .to_string();
        (text, lamp)
    }

    /// The glass and the lamp as the engine holds them, character cells
    /// and all: what [`Self::display`] reads, without the string. The
    /// redraw check asks every frame, so it asks for this.
    pub fn display_raw(&mut self) -> ([u8; LCD_WIDTH], bool) {
        let mut volume = [0u8; 1];
        self.engine
            .memory()
            .read(mt32_rs::memory::flat(addr::MASTER_VOLUME), &mut volume);
        self.engine.display().state(volume[0])
    }

    /// Write into the synth's memory the way the front panel does, as a
    /// DT1 message.
    ///
    /// The panel on the hardware edits the same patch and system memory that
    /// arrives over SysEx, so driving it this way is what the buttons do --
    /// and the engine answers by updating its display, exactly as it would
    /// for a program change from the guest.
    ///
    /// `addr` is written as the manual prints it, a byte per pair of hex
    /// digits, which is what [`addr`] holds and what goes on the wire.
    pub fn write_memory(&mut self, addr: u32, data: &[u8]) {
        if debug_enabled() {
            log::debug!("mt32: write {addr:06X} <- {data:02X?}");
        }
        // The immediate entry: a panel edit lands now, ahead of whatever
        // guest traffic is queued for the next rendered block.
        self.engine.play_sysex_now(&dt1(addr, data));
    }

    /// Read the synth's memory back. False means no memory lives there and
    /// there is nothing to answer with.
    ///
    /// `addr` is written as the manual prints it, the same way [`addr`] and
    /// [`write_memory`] take one.
    ///
    /// [`write_memory`]: Self::write_memory
    pub fn read_memory(&mut self, addr: u32, out: &mut [u8]) -> bool {
        let got = self.engine.memory().read(mt32_rs::memory::flat(addr), out);
        if got != out.len() {
            if debug_enabled() {
                log::debug!("mt32: read {addr:06X} -- no memory there");
            }
            return false;
        }
        if debug_enabled() {
            log::debug!("mt32: read {addr:06X} -> {} bytes", out.len());
        }
        true
    }

    /// Return the LCD to its main screen, as the Master Volume button does.
    pub fn show_main_display(&mut self) {
        if debug_enabled() {
            log::debug!("mt32: display -> main");
        }
        self.engine.display().set_main_display_mode();
    }

    /// Drain the module's MIDI OUT: the dump replies it has made since the
    /// last call, in order. Everything on that jack is an answer.
    pub fn take_midi_out(&mut self) -> Vec<u8> {
        self.engine.take_midi_out()
    }
}

impl std::fmt::Debug for Mt32Synth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mt32Synth")
            .field("sample_rate", &self.sample_rate)
            .finish_non_exhaustive()
    }
}

/// Reads one ROM image whole, naming the file when it cannot.
fn read_rom(path: &Path, what: &str) -> Result<Vec<u8>> {
    let image = std::fs::read(path)
        .map_err(|e| anyhow!("reading the {what} ROM {}: {e}", path.display()))?;
    if mt32_rs::rom::identify(&image).is_none() {
        return Err(anyhow!(
            "{} is not a ROM the MT-32 engine recognises; \
             it identifies ROMs by content, not by name",
            path.display()
        ));
    }
    Ok(image)
}

/// The parser's landing place: complete messages into the engine, with the
/// module's commentary put in the log on the way past.
struct EngineSink<'a> {
    engine: &'a mut mt32_rs::engine::Engine,
}

impl mt32_rs::midi::Sink for EngineSink<'_> {
    fn short_message(&mut self, message: u32) {
        self.engine.play_msg(message);
    }

    fn sysex(&mut self, frame: &[u8]) {
        // What a program writes to the module's display goes in the log,
        // so everything the module says reads the same way.
        if frame.len() > 10 && frame[4] == 0x12 && frame[5] == 0x20 && frame[6] == 0x00 {
            let text: String = frame[8..frame.len() - 2]
                .iter()
                .map(|&b| b as char)
                .collect();
            log::info!("mt32: display {:?}", text.trim());
        }
        self.engine.play_sysex(frame);
    }

    fn dropped(&mut self, what: &'static str) {
        if debug_enabled() {
            log::debug!("mt32: dropped {what}");
        }
    }
}

/// One DT1 message -- `F0 41 10 16 12 <addr> <data> <checksum> F7` -- with
/// `addr` written as the manual prints it, a byte per pair of hex digits.
/// Everything Copperline writes into the module is built this way.
pub(crate) fn dt1(addr: u32, data: &[u8]) -> Vec<u8> {
    mt32_rs::sysex::dt1(addr, data)
}

/// Where the panel writes. Addresses are seven bits per byte.
pub mod addr {
    /// The system area, in the order the MT-32 lays it out: master tune,
    /// then the three reverb parameters, then the partial reserve and
    /// channel tables, then master volume.
    pub const MASTER_TUNE: u32 = 0x10_0000;
    pub const REVERB_MODE: u32 = 0x10_0001;
    /// The nine parts' MIDI channels, 0-16 each (16 being off).
    pub const CHAN_ASSIGN: u32 = 0x10_000D;
    /// The system area's master volume, 0-100.
    pub const MASTER_VOLUME: u32 = 0x10_0016;
    /// A part's patch: timbre group, then timbre number, then (eight bytes
    /// in) its output level. Nine parts, sixteen bytes apart.
    pub const PATCH_TEMP: u32 = 0x03_0000;
    pub const PATCH_STRIDE: u32 = 0x10;
    pub const PATCH_TIMBRE_GROUP: u32 = 0;
    pub const PATCH_TIMBRE_NUMBER: u32 = 1;
    pub const PATCH_OUTPUT_LEVEL: u32 = 8;

    /// The address of `field` within part `n`'s patch.
    pub fn patch(n: usize, field: u32) -> u32 {
        PATCH_TEMP + n as u32 * PATCH_STRIDE + field
    }
}

/// The engine's name and version, for the About panel and the log.
pub(crate) fn engine_version() -> String {
    format!("mt32-rs {}", mt32_rs::VERSION)
}

#[cfg(test)]
mod tests;
