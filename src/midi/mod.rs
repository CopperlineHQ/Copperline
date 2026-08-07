// SPDX-License-Identifier: GPL-3.0-or-later

//! Host MIDI bridge for Paula's serial port (`[serial] mode = "midi"`).
//!
//! On output, each serial byte is stamped with the emulated colour clock it left
//! the wire on (see [`crate::serial::SerialTimeAnchor`]) and scheduled at the
//! matching host time, so the guest's byte timing survives to the host instead of
//! collapsing to the moment a frame's worth of bytes happens to be flushed. On
//! input, the backend fills a ring the receiver drains at the emulated baud rate.
//!
//! The input ring is a lock-free `ringbuf` SPSC queue (producer on the CoreMIDI
//! thread, consumer on the emulation thread). Paula's serial idle fast path polls
//! `has_pending_input` on nearly every device tick, so that poll must not lock.
//!
//! The emulator core only sees the [`SerialSink`]. The platform connection lives
//! behind [`MidiBackend`], chosen by `cfg(target_os)`: macOS drives CoreMIDI,
//! Linux the ALSA sequencer, and other targets get a stub until their backend
//! exists.

use std::time::Instant;

use anyhow::Result;
use ringbuf::traits::{Consumer, Observer, Split};
use ringbuf::HeapRb;

use crate::serial::{SerialSink, SerialTimeAnchor};

#[cfg(target_os = "macos")]
mod coremidi;
#[cfg(target_os = "macos")]
use coremidi as backend;

#[cfg(target_os = "linux")]
mod alsa;
#[cfg(target_os = "linux")]
use alsa as backend;

#[cfg(target_os = "windows")]
mod winmm;
#[cfg(target_os = "windows")]
use winmm as backend;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod stub;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
use stub as backend;

/// Capacity of the host-to-guest input ring. A MIDI stream is a few KB/s and
/// Paula drains it every serial tick, so this only absorbs a burst (a SysEx
/// dump, say) between drains.
const INPUT_RING_BYTES: usize = 8192;

/// Lock-free producer/consumer halves of the input ring.
pub type InputProducer = ringbuf::HeapProd<u8>;
type InputConsumer = ringbuf::HeapCons<u8>;

/// A host MIDI endpoint the user can select by name.
#[derive(Clone, Debug)]
pub struct MidiEndpoint {
    pub name: String,
}

/// Available host endpoints, split by direction.
#[derive(Clone, Debug, Default)]
pub struct MidiEndpoints {
    /// Sources: endpoints that send MIDI to us (the Amiga's MIDI input).
    pub inputs: Vec<MidiEndpoint>,
    /// Destinations: endpoints we send MIDI to (the Amiga's MIDI output).
    pub outputs: Vec<MidiEndpoint>,
}

/// A live platform MIDI connection. Output goes through [`send`]; input is
/// delivered by the backend into the ring producer handed to it at open time.
///
/// [`send`]: MidiBackend::send
pub trait MidiBackend: Send {
    /// Schedule `data` for delivery at host instant `at`. Best effort: a past
    /// instant is sent as soon as possible.
    fn send(&mut self, data: &[u8], at: Instant);

    /// Retarget output to the named endpoint, or `None` to stop sending.
    fn set_output(&mut self, endpoint: Option<&str>);
    /// Retarget input to the named endpoint, or `None` to stop receiving.
    fn set_input(&mut self, endpoint: Option<&str>);
    /// Name of the current output endpoint, if any.
    fn current_output(&self) -> Option<String>;
    /// Name of the current input endpoint, if any.
    fn current_input(&self) -> Option<String>;
}

/// Step a device selection through "None" then the named endpoints, returning
/// the new choice. Shared by the launcher picker and the runtime menu.
pub(crate) fn next_endpoint(
    current: Option<&str>,
    names: &[String],
    forward: bool,
) -> Option<String> {
    let here = current
        .and_then(|c| names.iter().position(|n| n == c))
        .map_or(0, |i| i + 1);
    let count = names.len() + 1;
    let next = if forward {
        (here + 1) % count
    } else {
        (here + count - 1) % count
    };
    (next > 0).then(|| names[next - 1].clone())
}

/// Enumerate host MIDI endpoints for the device picker and `--list-midi`.
pub fn enumerate() -> MidiEndpoints {
    backend::enumerate()
}

/// The [`SerialSink`] used for `[serial] mode = "midi"`. Bridges Paula's serial
/// port to the selected host MIDI endpoints.
pub struct MidiSerialSink {
    backend: Box<dyn MidiBackend>,
    anchor: Option<SerialTimeAnchor>,
    input: InputConsumer,
    framer: MidiFramer,
    debug: Option<MidiDebug>,
    /// Where the MT-32's ROMs are, so one can be attached and dropped while
    /// the machine runs. Empty when none were configured, which is what
    /// keeps [`MIDI_OUT_MT32`] out of the picker.
    #[cfg(feature = "mt32")]
    mt32_roms: crate::mt32::Mt32Roms,
    /// What the control ROM calls itself, read from the image once when the
    /// pair is configured. The engine keeps its copy of the ROM private, so
    /// this is read from the file, and read once: it cannot change while
    /// the machine runs.
    #[cfg(feature = "mt32")]
    mt32_version: Option<String>,
    /// The fitted MT-32. Absent costs nothing: no ROMs read, no engine, and
    /// no rendering asked for. Absent while it is switched off, too -- an
    /// unpowered synth is not a quieter synth, it is no synth.
    #[cfg(feature = "mt32")]
    mt32: Option<crate::mt32::Mt32Device>,
    /// Whether the MT-32 is the chosen output, which it stays across being
    /// switched off and on again.
    #[cfg(feature = "mt32")]
    mt32_selected: bool,
    /// Why the MT-32 could not be fitted when it was last asked for, so the
    /// window can say so rather than leaving a silent blank panel.
    #[cfg(feature = "mt32")]
    mt32_fault: Option<String>,
    /// Whether the MT-32's own MIDI OUT is wired back to the machine, which
    /// is what a patch editor on the Amiga needs to read the module back.
    /// Only meaningful while the MT-32 is also the output: it answers what
    /// it was sent, so with nothing going to it there is nothing to answer.
    #[cfg(feature = "mt32")]
    mt32_input: bool,
    /// The reply waiting to go back to the guest. Paula's receiver drains it
    /// at the emulated baud rate exactly as it drains the host ring, so a
    /// dump arrives over the wire at MIDI speed rather than all at once.
    #[cfg(feature = "mt32")]
    mt32_reply: std::collections::VecDeque<u8>,
}

/// The output-device name that means the built-in MT-32 rather than a host
/// endpoint. Shown as [`MIDI_OUT_MT32_LABEL`].
#[cfg(feature = "mt32")]
pub use crate::config::MIDI_OUT_MT32;

/// What the MT-32 output is called anywhere a person reads it.
pub const MIDI_OUT_MT32_LABEL: &str = "MT-32";

/// MIDI Active Sensing status byte.
pub(crate) const ACTIVE_SENSE: u8 = 0xFE;

/// Whether to drop Active Sensing (0xFE) at the bridge. A real Amiga passes it
/// straight down the serial line, so the faithful default is to forward it. Some
/// host MIDI interfaces do strip it, so `COPPERLINE_MIDI_STRIP_ACTIVE_SENSE=1`
/// opts into that behaviour.
pub(crate) fn strip_active_sense() -> bool {
    static STRIP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STRIP.get_or_init(|| crate::envcfg::flag("COPPERLINE_MIDI_STRIP_ACTIVE_SENSE"))
}

/// One-line description of a complete MIDI message for the debug trace.
fn describe(msg: &[u8]) -> String {
    let Some(&status) = msg.first() else {
        return "(empty)".to_string();
    };
    let d = |i: usize| {
        msg.get(i)
            .map_or_else(|| "?".to_string(), |v| v.to_string())
    };
    let ch = (status & 0x0F) + 1;
    match status & 0xF0 {
        0x80 => format!("Note Off   ch{ch} note {} vel {}", d(1), d(2)),
        0x90 if msg.get(2) == Some(&0) => format!("Note Off   ch{ch} note {} (vel 0)", d(1)),
        0x90 => format!("Note On    ch{ch} note {} vel {}", d(1), d(2)),
        0xA0 => format!("Poly AT    ch{ch} note {} press {}", d(1), d(2)),
        0xB0 => format!("Control    ch{ch} cc {} val {}", d(1), d(2)),
        0xC0 => format!("Program    ch{ch} {}", d(1)),
        0xD0 => format!("Chan AT    ch{ch} {}", d(1)),
        0xE0 => format!("Pitch Bend ch{ch} lsb {} msb {}", d(1), d(2)),
        0xF0 => match status {
            0xF0 => format!("SysEx ({} bytes)", msg.len()),
            0xF1 => format!("MTC Quarter Frame {}", d(1)),
            0xF2 => "Song Position".to_string(),
            0xF3 => format!("Song Select {}", d(1)),
            0xF6 => "Tune Request".to_string(),
            0xF8 => "Clock".to_string(),
            0xFA => "Start".to_string(),
            0xFB => "Continue".to_string(),
            0xFC => "Stop".to_string(),
            0xFE => "Active Sense".to_string(),
            0xFF => "Reset".to_string(),
            _ => format!("System 0x{status:02x}"),
        },
        _ => format!("data {}", hex_bytes(msg)),
    }
}

/// Space-separated hex of a byte slice, for the debug trace.
fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Total length in bytes of a channel-voice or system message from its status
/// byte. SysEx (`0xF0`) is variable and handled separately; real-time bytes
/// (`>= 0xF8`) are always one byte.
fn message_len(status: u8) -> usize {
    match status & 0xF0 {
        0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => 3, // note off/on, poly AT, CC, pitch bend
        0xC0 | 0xD0 => 2,                      // program change, channel pressure
        0xF0 => match status {
            0xF2 => 3,        // song position pointer
            0xF1 | 0xF3 => 2, // MTC quarter frame, song select
            _ => 1,           // tune request, undefined F-series
        },
        _ => 1,
    }
}

/// Reassembles the guest's serial byte stream into complete MIDI messages, the
/// unit a receiver expects per packet. Sent a byte at a time, a multi-byte
/// message never forms and a receiver rejects each data byte as invalid. Tracks
/// running status and SysEx, and passes interleaved real-time bytes straight
/// through.
#[derive(Default)]
struct MidiFramer {
    /// Bytes of the message being assembled.
    buf: Vec<u8>,
    /// Total bytes the current message needs (0 while idle).
    expected: usize,
    /// Last channel-voice status, for running status (0 = none).
    status: u8,
    in_sysex: bool,
}

impl MidiFramer {
    /// Feed one serial byte. `emit` is called with each complete message. The
    /// buffer is reused across messages, so a settled stream never allocates.
    fn push(&mut self, b: u8, at: Instant, mut emit: impl FnMut(&[u8], Instant)) {
        // System real-time (0xF8..=0xFF) is a single byte that may appear
        // between the bytes of another message without disturbing it.
        if b >= 0xF8 {
            emit(&[b], at);
            return;
        }
        if self.in_sysex {
            if b < 0x80 {
                self.buf.push(b);
                return;
            }
            // Any non-real-time status byte ends the running SysEx.
            if b == 0xF7 {
                self.buf.push(b);
            }
            emit(&self.buf, at);
            self.buf.clear();
            self.in_sysex = false;
            if b == 0xF7 {
                return;
            }
            // Fall through to handle b as a fresh status byte.
        }
        if b >= 0x80 {
            if b == 0xF0 {
                self.in_sysex = true;
                self.buf.clear();
                self.buf.push(b);
                self.status = 0;
                return;
            }
            // Channel-voice status arms running status; system common clears it.
            self.status = if b < 0xF0 { b } else { 0 };
            self.expected = message_len(b);
            self.buf.clear();
            self.buf.push(b);
            self.emit_if_complete(at, &mut emit);
            return;
        }
        // Data byte. An empty buffer with a live running status opens a new
        // message from it; a data byte with no status is dropped.
        if self.buf.is_empty() {
            if self.status == 0 {
                return;
            }
            self.buf.push(self.status);
            self.expected = message_len(self.status);
        }
        self.buf.push(b);
        self.emit_if_complete(at, &mut emit);
    }

    fn emit_if_complete(&mut self, at: Instant, emit: &mut impl FnMut(&[u8], Instant)) {
        if self.buf.len() >= self.expected {
            emit(&self.buf, at);
            self.buf.clear();
        }
    }
}

/// `COPPERLINE_MIDI_DEBUG=1` byte-flow tracing. Counts bytes taken from the
/// serial port (tx) and handed back to the guest (rx), reported about once a
/// second. No tx while a song plays means the guest is not driving the serial
/// port, so the fault is upstream of this sink.
struct MidiDebug {
    tx_bytes: u64,
    rx_bytes: u64,
    last_report: Instant,
    /// The most recent transmitted bytes (rolling), to check they read as MIDI
    /// (a note-on is `9n nn vv`, e.g. `90 3c 64`).
    sample: Vec<u8>,
    /// `COPPERLINE_MIDI_DEBUG=2`: decode and log every message in each
    /// direction, to inspect the actual stream (e.g. Active Sense handling).
    verbose: bool,
    /// Reassembles received bytes into messages for the verbose log (the guest
    /// still gets the raw byte stream).
    rx_framer: MidiFramer,
}

impl MidiDebug {
    /// Keep a rolling window of the last transmitted bytes.
    fn record_sample(&mut self, msg: &[u8]) {
        const KEEP: usize = 24;
        for &b in msg {
            if self.sample.len() >= KEEP {
                self.sample.remove(0);
            }
            self.sample.push(b);
        }
    }
}

impl MidiSerialSink {
    /// Open the selected endpoints (case-insensitive substring match on their
    /// display names). Both are optional: a machine can send only, receive only,
    /// or both.
    pub fn open(midi_out: Option<&str>, midi_in: Option<&str>) -> Result<Self> {
        let (producer, input) = HeapRb::<u8>::new(INPUT_RING_BYTES).split();
        let backend = backend::open(midi_out, midi_in, producer)?;
        let debug = crate::envcfg::flag("COPPERLINE_MIDI_DEBUG").then(|| {
            // Announce the flag: with no traffic the per-byte report never
            // fires, so this separates "no bytes" from "tracing off".
            log::info!("midi: byte-flow tracing enabled (COPPERLINE_MIDI_DEBUG)");
            let verbose = crate::envcfg::var("COPPERLINE_MIDI_DEBUG").as_deref() == Some("2");
            MidiDebug {
                tx_bytes: 0,
                rx_bytes: 0,
                last_report: Instant::now(),
                sample: Vec::new(),
                verbose,
                rx_framer: MidiFramer::default(),
            }
        });
        Ok(Self {
            backend,
            anchor: None,
            input,
            framer: MidiFramer::default(),
            debug,
            #[cfg(feature = "mt32")]
            mt32_roms: crate::mt32::Mt32Roms::default(),
            #[cfg(feature = "mt32")]
            mt32_version: None,
            #[cfg(feature = "mt32")]
            mt32: None,
            #[cfg(feature = "mt32")]
            mt32_selected: false,
            #[cfg(feature = "mt32")]
            mt32_fault: None,
            #[cfg(feature = "mt32")]
            mt32_input: false,
            #[cfg(feature = "mt32")]
            mt32_reply: std::collections::VecDeque::new(),
        })
    }

    fn debug_report(&mut self) {
        if let Some(dbg) = &mut self.debug {
            if dbg.last_report.elapsed().as_secs_f64() >= 1.0 {
                eprintln!(
                    "midi: tx {} bytes, rx {} bytes, first tx: [{}]",
                    dbg.tx_bytes,
                    dbg.rx_bytes,
                    hex_bytes(&dbg.sample)
                );
                dbg.last_report = Instant::now();
            }
        }
    }

    /// Switch the output endpoint to the next host device (used by the runtime
    /// menu). The device list is re-read so freshly connected devices appear.
    pub fn cycle_output(&mut self, forward: bool) {
        let names: Vec<String> = enumerate().outputs.into_iter().map(|e| e.name).collect();
        let next = next_endpoint(self.backend.current_output().as_deref(), &names, forward);
        self.backend.set_output(next.as_deref());
        log::info!("midi: output -> {}", self.output_label());
    }

    /// Switch the input endpoint to the next host device.
    pub fn cycle_input(&mut self, forward: bool) {
        let names: Vec<String> = enumerate().inputs.into_iter().map(|e| e.name).collect();
        let next = next_endpoint(self.backend.current_input().as_deref(), &names, forward);
        self.backend.set_input(next.as_deref());
        log::info!("midi: input -> {}", self.input_label());
    }

    /// Point the output at a named host endpoint, at the built-in MT-32, or
    /// at nothing.
    ///
    /// The MT-32 is fitted here and dropped again when the output moves on,
    /// so a session that never selects one never reads a ROM or runs an
    /// engine.
    pub fn set_output_endpoint(&mut self, endpoint: Option<&str>) {
        #[cfg(feature = "mt32")]
        if crate::config::midi_out_is_mt32(endpoint) {
            self.attach_mt32();
            return;
        }
        #[cfg(feature = "mt32")]
        {
            self.mt32 = None;
            self.mt32_selected = false;
            // Nothing reaches the module any more, so it has nothing left
            // to answer: its MIDI OUT goes with its MIDI IN.
            self.mt32_input = false;
        }
        #[cfg(feature = "mt32")]
        self.stop_answering();
        self.backend.set_output(endpoint);
        log::info!("midi: output -> {}", self.output_label());
    }

    /// Fit the MT-32, leaving the host output silent: the device on the far
    /// end of the cable is the one here.
    #[cfg(feature = "mt32")]
    fn attach_mt32(&mut self) {
        if self.mt32.is_some() {
            return;
        }
        self.mt32_selected = true;
        self.mt32_fault = None;
        let Some((control, pcm)) = self.mt32_roms.pair() else {
            log::warn!(
                "midi: {MIDI_OUT_MT32_LABEL} needs both ROM images; \
                 set [serial] mt32_control_rom and mt32_pcm_rom"
            );
            self.mt32_fault = Some("missing ROM(s)".to_string());
            return;
        };
        match crate::mt32::Mt32Device::open(control, pcm) {
            Ok(device) => {
                self.backend.set_output(None);
                self.mt32 = Some(device);
            }
            Err(e) => {
                log::warn!("midi: {MIDI_OUT_MT32_LABEL} could not be fitted: {e:#}");
                self.mt32_fault = Some("invalid ROM(s)".to_string());
            }
        }
    }

    /// Switch the fitted MT-32 off or on again, leaving it the chosen
    /// output either way.
    ///
    /// Off drops the engine entirely, which is what a real one being switched
    /// off amounts to; on builds a fresh one, so it comes up with its
    /// power-on greeting and its defaults, exactly as the hardware does.
    #[cfg(feature = "mt32")]
    pub fn set_mt32_power(&mut self, on: bool) {
        if !self.mt32_selected || on == self.mt32.is_some() {
            return;
        }
        if on {
            self.attach_mt32();
        } else {
            self.mt32 = None;
            // A module with no power answers nothing.
            self.stop_answering();
            log::info!("midi: {MIDI_OUT_MT32_LABEL} switched off");
        }
    }

    /// Stop answering the machine: nothing half-gathered, nothing half-sent.
    ///
    /// Called wherever the module stops being what the guest is talking to,
    /// so a request begun against the old one cannot finish against the new,
    /// and a dump cut off partway cannot arrive as a broken message.
    #[cfg(feature = "mt32")]
    fn stop_answering(&mut self) {
        self.mt32_reply.clear();
        // Whatever the module had queued on its OUT jack goes with it.
        if let Some(mt32) = self.mt32.as_mut() {
            let _ = mt32.take_midi_out();
        }
    }

    /// Why the MT-32 could not be fitted, if it could not. Taken rather than
    /// borrowed: it is said once, not on every frame.
    #[cfg(feature = "mt32")]
    pub fn take_mt32_fault(&mut self) -> Option<String> {
        self.mt32_fault.take()
    }

    /// Whether the MT-32 is the chosen output, powered or not.
    #[cfg(feature = "mt32")]
    pub fn mt32_selected(&self) -> bool {
        self.mt32_selected
    }

    /// Whether an MT-32 could be selected at all.
    #[cfg(feature = "mt32")]
    pub fn mt32_available(&self) -> bool {
        self.mt32_roms.configured()
    }

    /// The attached MT-32, for the front panel.
    #[cfg(feature = "mt32")]
    pub fn mt32(&self) -> Option<&crate::mt32::Mt32Device> {
        self.mt32.as_ref()
    }

    #[cfg(feature = "mt32")]
    pub fn mt32_mut(&mut self) -> Option<&mut crate::mt32::Mt32Device> {
        self.mt32.as_mut()
    }

    /// Where the ROMs are. Set once from the configuration; the picker and
    /// the runtime switch both read it.
    #[cfg(feature = "mt32")]
    pub fn set_mt32_roms(&mut self, roms: crate::mt32::Mt32Roms) {
        self.mt32_version = roms
            .control
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|image| crate::mt32::rom::version_line(&image));
        self.mt32_roms = roms;
    }

    /// What the control ROM calls itself, for the panel's version screen.
    #[cfg(feature = "mt32")]
    pub fn mt32_version(&self) -> Option<&str> {
        self.mt32_version.as_deref()
    }

    /// Stop whatever song is playing.
    #[cfg(feature = "mt32")]
    pub fn stop_mt32_demo(&mut self) {
        if let Some(mt32) = self.mt32.as_mut() {
            mt32.play_demo(None);
        }
    }

    /// Whether a demo song is still running.
    #[cfg(feature = "mt32")]
    pub fn mt32_demo_playing(&self) -> bool {
        self.mt32
            .as_ref()
            .is_some_and(crate::mt32::Mt32Device::demo_playing)
    }

    /// Start one of the control ROM's own songs, and say what it is called.
    /// The image is read here rather than kept: a demo is a rare thing to
    /// ask for, and the songs are a good deal larger than the answer.
    #[cfg(feature = "mt32")]
    pub fn play_mt32_demo(&mut self, track: usize) -> Option<String> {
        let image = std::fs::read(self.mt32_roms.control.as_deref()?).ok()?;
        let mut songs = crate::mt32::demo::songs(&image);
        if songs.is_empty() {
            return None;
        }
        let song = songs.swap_remove(track % songs.len());
        let title = song.title.clone();
        self.mt32.as_mut()?.play_demo(Some(song));
        Some(title)
    }

    /// Point the input at a named host endpoint, at the MT-32's own MIDI
    /// OUT, or at nothing.
    pub fn set_input_endpoint(&mut self, endpoint: Option<&str>) {
        #[cfg(feature = "mt32")]
        if crate::config::midi_out_is_mt32(endpoint) {
            // Its MIDI OUT and its MIDI IN are the same cable pair to the
            // same module, so taking the input silences the host source.
            self.backend.set_input(None);
            self.mt32_input = true;
            log::info!("midi: input -> {}", self.input_label());
            return;
        }
        #[cfg(feature = "mt32")]
        {
            self.mt32_input = false;
            self.stop_answering();
        }
        self.backend.set_input(endpoint);
        log::info!("midi: input -> {}", self.input_label());
    }

    /// Whether the module's MIDI OUT is the machine's MIDI IN.
    #[cfg(feature = "mt32")]
    pub fn mt32_input(&self) -> bool {
        self.mt32_input
    }

    /// Say what the port ended up wired to, once the output is settled.
    ///
    /// Reported here rather than by each backend, because until the MT-32
    /// has had its chance to attach, "no host endpoint" does not yet mean
    /// the port is inert.
    pub fn report_wiring(&self) {
        let (out, input) = (self.output_label(), self.input_label());
        if out == "None" && input == "None" {
            log::warn!("[serial] mode = midi but no endpoint is selected; MIDI is inert");
        } else {
            log::info!("midi: out {out}, in {input}");
        }
    }

    /// Current output device name, or "None".
    pub fn output_label(&self) -> String {
        #[cfg(feature = "mt32")]
        if self.mt32_selected {
            return MIDI_OUT_MT32_LABEL.to_string();
        }
        self.backend
            .current_output()
            .unwrap_or_else(|| "None".to_string())
    }

    /// Current input device name, or "None".
    pub fn input_label(&self) -> String {
        #[cfg(feature = "mt32")]
        if self.mt32_input {
            return MIDI_OUT_MT32_LABEL.to_string();
        }
        self.backend
            .current_input()
            .unwrap_or_else(|| "None".to_string())
    }
}

impl SerialSink for MidiSerialSink {
    fn next_audio_frame(&mut self) -> Option<(f32, f32)> {
        #[cfg(feature = "mt32")]
        if let Some(mt32) = &mut self.mt32 {
            let frame = mt32.next_frame();
            // A request the module has answered goes back down the wire --
            // when its MIDI OUT is wired to the machine. Unwired, the jack
            // answers into a cable that is not there.
            let replies = mt32.take_midi_out();
            if self.mt32_input && !replies.is_empty() {
                self.mt32_reply.extend(replies);
            }
            return Some(frame);
        }
        None
    }

    fn write_byte(&mut self, b: u8, at_cck: u64) {
        // Faithful by default; only drops Active Sensing when opted in (see
        // strip_active_sense).
        if b == ACTIVE_SENSE && strip_active_sense() {
            return;
        }
        // With an MT-32 attached, it is the device on the far end of the
        // cable: the bytes go to it rather than out to the host, and no
        // scheduling is needed because it answers in emulated time.
        #[cfg(feature = "mt32")]
        if let Some(mt32) = &mut self.mt32 {
            if let Some(dbg) = &mut self.debug {
                dbg.tx_bytes += 1;
            }
            mt32.write_byte(b);
            return;
        }
        // Map the emit clock onto host time so the byte is scheduled rather
        // than sent now. Until the first anchor arrives, or in an unpaced run,
        // deliver immediately.
        let at = self
            .anchor
            .map(|anchor| anchor.host_time(at_cck))
            .unwrap_or_else(Instant::now);
        if let Some(dbg) = &mut self.debug {
            dbg.tx_bytes += 1;
        }
        // Assemble whole MIDI messages before sending; a stream of single-byte
        // packets is rejected as invalid by receivers.
        let backend = &mut self.backend;
        let debug = &mut self.debug;
        self.framer.push(b, at, |msg, at| {
            backend.send(msg, at);
            if let Some(dbg) = debug.as_mut() {
                if dbg.verbose {
                    eprintln!("midi out: {}", describe(msg));
                }
                dbg.record_sample(msg);
            }
        });
        self.debug_report();
    }

    fn read_byte(&mut self) -> Option<u8> {
        // The module's own reply comes first: it is answering something the
        // guest asked for, and a host source is a separate cable anyway.
        #[cfg(feature = "mt32")]
        if let Some(b) = self.mt32_reply.pop_front() {
            if let Some(dbg) = &mut self.debug {
                dbg.rx_bytes += 1;
            }
            return Some(b);
        }
        let b = self.input.try_pop();
        if let Some(byte) = b {
            if let Some(dbg) = &mut self.debug {
                dbg.rx_bytes += 1;
                if dbg.verbose {
                    dbg.rx_framer.push(byte, Instant::now(), |msg, _| {
                        eprintln!("midi in:  {}", describe(msg));
                    });
                }
            }
        }
        b
    }

    fn has_pending_input(&self) -> bool {
        #[cfg(feature = "mt32")]
        if !self.mt32_reply.is_empty() {
            return true;
        }
        !self.input.is_empty()
    }

    fn set_time_anchor(&mut self, anchor: SerialTimeAnchor) {
        self.anchor = Some(anchor);
    }

    fn reset_after_timeline_jump(&mut self) {
        // Host MIDI is outside the serialized machine. Do not carry a
        // half-framed output message or an old host-time anchor into the
        // restored timeline. Incoming bytes are live host input, not emulated
        // future state, so leave that queue connected.
        self.anchor = None;
        self.framer = MidiFramer::default();

        #[cfg(feature = "mt32")]
        {
            // mt32-rs does not expose a snapshot of its voices, memory,
            // display, MIDI parser, analogue filters or resampler. Carrying
            // the live engine across a load is worse than restarting it: it
            // keeps notes and edits made in the abandoned future. Preserve
            // the host wiring and power choice, but power-cycle a running
            // module and discard every pending reply.
            let powered = self.mt32.take().is_some();
            self.stop_answering();
            if powered {
                log::info!(
                    "midi: timeline changed; power-cycling {MIDI_OUT_MT32_LABEL} \
                     because synthesizer state is not serializable"
                );
                self.attach_mt32();
            }
        }
    }

    fn as_midi(&mut self) -> Option<&mut MidiSerialSink> {
        Some(self)
    }

    fn as_midi_ref(&self) -> Option<&MidiSerialSink> {
        Some(self)
    }

    fn flush(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a byte stream through the framer, collecting the complete messages.
    fn frame(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut framer = MidiFramer::default();
        let now = Instant::now();
        let mut out = Vec::new();
        for &b in bytes {
            framer.push(b, now, |msg, _| out.push(msg.to_vec()));
        }
        out
    }

    #[test]
    fn frames_note_on_from_separate_bytes() {
        // The core bug: three serial bytes must become one 3-byte message.
        assert_eq!(frame(&[0x90, 0x3C, 0x64]), vec![vec![0x90, 0x3C, 0x64]]);
    }

    #[test]
    fn running_status_reuses_last_status() {
        assert_eq!(
            frame(&[0x90, 0x3C, 0x64, 0x40, 0x50]),
            vec![vec![0x90, 0x3C, 0x64], vec![0x90, 0x40, 0x50]]
        );
    }

    #[test]
    fn realtime_byte_interleaves_without_breaking_message() {
        // Active Sense (0xFE) between the data bytes of a note-on.
        assert_eq!(
            frame(&[0x90, 0x3C, 0xFE, 0x64]),
            vec![vec![0xFE], vec![0x90, 0x3C, 0x64]]
        );
    }

    #[test]
    fn program_change_is_two_bytes() {
        assert_eq!(frame(&[0xC0, 0x05]), vec![vec![0xC0, 0x05]]);
    }

    #[test]
    fn sysex_accumulates_until_eox() {
        assert_eq!(
            frame(&[0xF0, 0x7E, 0x00, 0x06, 0x01, 0xF7]),
            vec![vec![0xF0, 0x7E, 0x00, 0x06, 0x01, 0xF7]]
        );
    }

    #[test]
    fn stray_data_byte_without_status_is_dropped() {
        assert!(frame(&[0x3C, 0x64]).is_empty());
    }

    #[test]
    fn timeline_jump_discards_host_midi_partial_state_and_mt32_replies() {
        use crate::serial::SerialSink;

        let mut sink = MidiSerialSink::open(None, None).expect("MIDI sink");
        sink.anchor = Some(SerialTimeAnchor {
            host_epoch: Instant::now(),
            cck_per_second: 1.0,
        });
        let mut emitted = Vec::new();
        sink.framer.push(0x90, Instant::now(), |msg, _| {
            emitted.push(msg.to_vec());
        });
        assert!(emitted.is_empty(), "status alone is an incomplete message");

        #[cfg(feature = "mt32")]
        {
            sink.mt32_selected = true;
            sink.mt32_input = true;
            sink.mt32_reply.extend([0xF0, 0xF7]);
        }

        sink.reset_after_timeline_jump();
        assert!(sink.anchor.is_none(), "the old host-time anchor is gone");

        emitted.clear();
        sink.framer.push(0x3C, Instant::now(), |msg, _| {
            emitted.push(msg.to_vec());
        });
        sink.framer.push(0x64, Instant::now(), |msg, _| {
            emitted.push(msg.to_vec());
        });
        assert!(
            emitted.is_empty(),
            "data bytes cannot complete the abandoned timeline's message"
        );

        #[cfg(feature = "mt32")]
        {
            assert!(sink.mt32_selected, "host output selection survives");
            assert!(sink.mt32_input, "host input wiring survives");
            assert!(sink.mt32.is_none(), "a powered-off module stays off");
            assert!(sink.mt32_reply.is_empty(), "old replies are discarded");
        }
    }
}
