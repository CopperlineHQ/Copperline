// SPDX-License-Identifier: GPL-3.0-or-later

//! The MT-32, emulated by Munt's mt32emu (see `vendor/munt`).
//!
//! Paula's serial bytes go straight into the engine and stereo frames come
//! back at the mixer's rate, so nothing passes through the host's MIDI stack:
//! no IAC bus on macOS, no loopMIDI on Windows, no ALSA sequencer client.
//!
//! The engine needs two ROM images, a control ROM and a PCM ROM, which are
//! not Copperline's to ship and so are the user's to supply. Without them the
//! machine still runs; it simply has nothing on the far end of the cable.

use anyhow::{anyhow, bail, Result};
use std::ffi::{c_char, CString};
use std::path::Path;

pub mod demo;
mod ffi;
pub mod reply;
pub mod rom;
mod sink;

pub use sink::{Mt32Device, Mt32Roms};

/// What the engine puts in a part's place on the main screen while that
/// part is sounding. On the hardware it is a character the display
/// controller is given a shape of its own, a filled cell, so it is drawn as
/// one rather than looked for in a font.
pub const ACTIVE_PART: char = '\u{1}';

/// How wide the emulated LCD is, in characters. The panel is drawn to this.
pub const LCD_WIDTH: usize = 20;

/// Whether to log everything the MT-32 does, the engine's own running
/// commentary included (`COPPERLINE_MT32_DEBUG=1`).
///
/// Off, the engine says nothing. It is chatty about MIDI it cannot make
/// sense of, which on a serial line carrying whatever a guest emits is
/// ordinary and says nothing about Copperline.
pub(crate) fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::envcfg::flag("COPPERLINE_MT32_DEBUG"))
}

/// Which interface version this handler implements.
///
/// The engine calls this one without checking it for null, unlike every
/// other callback here, so it has to be filled in.
unsafe extern "C" fn get_version_id(_handler: ffi::ReportHandler) -> u32 {
    ffi::REPORT_HANDLER_VERSION_0
}

/// Takes the engine's diagnostics, already formatted by the C shim, and
/// puts them in the log rather than on the console.
///
/// # Safety
/// Called from `print_debug.cpp` with a null-terminated string.
#[no_mangle]
pub unsafe extern "C" fn copperline_mt32_log(text: *const c_char) {
    if !debug_enabled() || text.is_null() {
        return;
    }
    // SAFETY: null-checked, and the shim passes a terminated buffer.
    let line = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    log::debug!("mt32: {}", line.trim_end());
}

/// The handler the engine reports through. Only `print_debug` is taken:
/// everything else Copperline wants to know it asks for directly, and a
/// callback left null keeps the engine's own default.
static REPORT_HANDLER_V0: ffi::ReportHandlerV0 = ffi::ReportHandlerV0 {
    get_version_id: Some(get_version_id),
    print_debug: Some(ffi::copperline_mt32_print_debug),
    on_error_control_rom: None,
    on_error_pcm_rom: None,
    show_lcd_message: None,
    on_midi_message_played: None,
    on_midi_queue_overflow: None,
    on_midi_system_realtime: None,
    on_device_reset: None,
    on_device_reconfig: None,
    on_new_reverb_mode: None,
    on_new_reverb_time: None,
    on_new_reverb_level: None,
    on_poly_state_changed: None,
    on_program_changed: None,
};

/// A running MT-32. Dropping it closes the synth and frees the engine.
pub struct Mt32Synth {
    context: ffi::Context,
    /// What the engine reported as its output rate once opened. Asked for
    /// [`crate::audio::MIX_SAMPLE_RATE`]; kept so a mismatch is visible
    /// rather than silently detuning everything.
    sample_rate: u32,
    /// The engine's samples land here on the way to the mixer's frames,
    /// kept so rendering does not allocate once a block.
    scratch: Vec<i16>,
}

// The engine keeps all its state behind the context pointer and Copperline
// drives it from one thread (the emulator loop, like the rest of the mixer).
unsafe impl Send for Mt32Synth {}

impl Mt32Synth {
    /// Fit an MT-32 with the given ROM images and start it at `sample_rate`.
    ///
    /// The engine identifies a ROM by its size and SHA-1 rather than its
    /// name, so a mislabelled or truncated file is rejected here rather than
    /// producing a synth that sounds subtly wrong.
    pub fn open(control_rom: &Path, pcm_rom: &Path, sample_rate: u32) -> Result<Self> {
        let handler = ffi::ReportHandler {
            v0: &raw const REPORT_HANDLER_V0,
        };
        // SAFETY: the handler is static and outlives the context; its
        // unfilled callbacks are null, which the engine checks for.
        let context = unsafe { ffi::mt32emu_create_context(handler, std::ptr::null_mut()) };
        if context.is_null() {
            bail!("could not create the MT-32 engine context");
        }
        // One owner from here on: every early return below drops `synth`,
        // which closes and frees the context exactly once. (`close` on a
        // synth that never opened is a no-op the engine guards.)
        let mut synth = Self {
            context,
            sample_rate: 0,
            scratch: Vec::new(),
        };

        synth.add_rom(control_rom, "control")?;
        synth.add_rom(pcm_rom, "PCM")?;

        // SAFETY: `context` is live, and both calls only record intent for
        // the `open` below.
        unsafe {
            ffi::mt32emu_set_stereo_output_samplerate(context, f64::from(sample_rate));
            // The analogue output stage the LA32 fed on real hardware. The
            // accurate model is what the engine resamples from.
            ffi::mt32emu_set_analog_output_mode(context, ffi::AOM_ACCURATE);
        }

        // SAFETY: `context` is live and has both ROMs.
        let rc = unsafe { ffi::mt32emu_open_synth(context) };
        if rc != ffi::RC_OK {
            bail!("the MT-32 engine refused to start (code {rc})");
        }
        // SAFETY: the synth is open.
        synth.sample_rate = unsafe { ffi::mt32emu_get_actual_stereo_output_samplerate(context) };
        Ok(synth)
    }

    fn add_rom(&self, path: &Path, what: &str) -> Result<()> {
        let text = path
            .to_str()
            .ok_or_else(|| anyhow!("the {what} ROM path is not valid UTF-8: {}", path.display()))?;
        let c_path = CString::new(text)?;
        // SAFETY: `context` is live and `c_path` outlives the call.
        let rc = unsafe { ffi::mt32emu_add_rom_file(self.context, c_path.as_ptr()) };
        match rc {
            ffi::RC_ADDED_CONTROL_ROM | ffi::RC_ADDED_PCM_ROM => Ok(()),
            _ => Err(anyhow!(
                "{} is not a ROM the MT-32 engine recognises (code {rc}); \
                 it identifies ROMs by content, not by name",
                path.display()
            )),
        }
    }

    /// The rate the engine is actually producing at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Feed raw MIDI bytes, exactly as they arrived on Paula's serial line.
    /// Running status and split messages are the engine's problem, which is
    /// what its stream parser is for.
    pub fn parse(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if debug_enabled() {
            log::debug!("mt32: in {bytes:02X?}");
        }
        // SAFETY: `context` is live and the slice is valid for the call.
        unsafe {
            ffi::mt32emu_parse_stream(self.context, bytes.as_ptr(), bytes.len() as u32);
        }
    }

    /// Render `frames.len()` stereo frames, normalised for the mixer.
    pub fn render(&mut self, frames: &mut [(f32, f32)]) {
        if frames.is_empty() {
            return;
        }
        self.scratch.resize(frames.len() * 2, 0);
        // SAFETY: `context` is live and `scratch` holds two samples per frame,
        // which is what the engine writes.
        unsafe {
            ffi::mt32emu_render_bit16s(
                self.context,
                self.scratch.as_mut_ptr(),
                frames.len() as u32,
            );
        }
        for (frame, pair) in frames.iter_mut().zip(self.scratch.chunks_exact(2)) {
            *frame = (f32::from(pair[0]) / 32768.0, f32::from(pair[1]) / 32768.0);
        }
    }

    /// What the LCD reads and whether the MIDI MESSAGE lamp is lit.
    ///
    /// Both come from the engine, which draws the text out of the control
    /// ROM: the greeting it powers up with, the timbre names it shows on a
    /// program change, whatever a program puts there over SysEx, and its
    /// own checksum errors.
    pub fn display(&self) -> (String, bool) {
        // The engine documents this as needing room for 20 characters and a
        // terminator.
        let mut buffer = [0i8; LCD_WIDTH + 1];
        // SAFETY: `context` is live and the buffer is the documented size.
        let led = unsafe {
            ffi::mt32emu_get_display_state(self.context, buffer.as_mut_ptr().cast::<c_char>(), 0)
        };
        // The ROM writes ASCII (plus the filled-cell mark), so the bytes
        // are characters as they stand.
        let text = buffer
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8 as char)
            .collect();
        (text, led != 0)
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
        let msg = dt1(addr, data);
        if debug_enabled() {
            log::debug!("mt32: write {addr:06X} <- {data:02X?}");
        }
        // SAFETY: `context` is live and the message outlives the call.
        unsafe {
            ffi::mt32emu_play_sysex_now(self.context, msg.as_ptr(), msg.len() as u32);
        }
    }

    /// Read the synth's memory back, for the dump a librarian asks for.
    ///
    /// `addr` is written as the manual prints it, the same way [`addr`] and
    /// [`write_memory`] take one. False means no memory lives there and
    /// there is nothing to answer with: the engine leaves the buffer alone
    /// for an address it does not recognise, which the fill makes visible --
    /// a byte it did write is seven-bit, so a whole block still reading `FF`
    /// is one it never touched.
    ///
    /// [`write_memory`]: Self::write_memory
    pub fn read_memory(&self, addr: u32, out: &mut [u8]) -> bool {
        out.fill(0xFF);
        // The engine addresses its memory as a flat seven-bit count, which
        // is what it converts an incoming message's address bytes to.
        let flat = reply::linear(addr);
        // SAFETY: `context` is live and the buffer is valid for `out.len()`.
        unsafe {
            ffi::mt32emu_read_memory(self.context, flat, out.len() as u32, out.as_mut_ptr());
        }
        if out.iter().all(|&b| b == 0xFF) {
            if debug_enabled() {
                log::debug!("mt32: read {addr:06X} -- no memory there");
            }
            return false;
        }
        // Seven bits per byte, so the reply cannot carry a value that would
        // read as the end of the message.
        for b in out.iter_mut() {
            *b &= 0x7F;
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
        // SAFETY: `context` is live.
        unsafe { ffi::mt32emu_set_main_display_mode(self.context) };
    }
}

impl Drop for Mt32Synth {
    fn drop(&mut self) {
        // SAFETY: `context` was created by the engine and is closed once.
        unsafe {
            ffi::mt32emu_close_synth(self.context);
            ffi::mt32emu_free_context(self.context);
        }
    }
}

impl std::fmt::Debug for Mt32Synth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mt32Synth")
            .field("sample_rate", &self.sample_rate)
            .finish_non_exhaustive()
    }
}

/// One DT1 message -- `F0 41 10 16 12 <addr> <data> <checksum> F7` -- with
/// `addr` written as the manual prints it, a byte per pair of hex digits.
/// Everything Copperline writes into the module is built here.
pub(crate) fn dt1(addr: u32, data: &[u8]) -> Vec<u8> {
    const HEADER: usize = 5;
    let mut msg = Vec::with_capacity(HEADER + 3 + data.len() + 2);
    msg.extend_from_slice(&[0xF0, 0x41, 0x10, 0x16, 0x12]);
    msg.extend_from_slice(&addr.to_be_bytes()[1..]);
    msg.extend_from_slice(data);
    // Everything from the address on adds up to a multiple of 128.
    let sum: u32 = msg[HEADER..].iter().map(|&b| u32::from(b) & 0x7F).sum();
    msg.push(((128 - sum % 128) % 128) as u8);
    msg.push(0xF7);
    msg
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

/// The engine's own version string, for the About panel and the log.
pub(crate) fn engine_version() -> String {
    // SAFETY: the engine returns a static, null-terminated string.
    let ptr = unsafe { ffi::mt32emu_get_library_version_string() };
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: null-checked above, and the string is static.
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests;
