// SPDX-License-Identifier: GPL-3.0-or-later

//! Serial output sink. Paula's SERDAT writes are funneled through here.

use std::io::{self, Write};
use std::time::Instant;

/// Maps the emulated serial timeline onto the host clock so a timing-sensitive
/// sink can schedule its output. `host_epoch` is the host instant of emulated
/// color clock 0 and `cck_per_second` is the color-clock rate, so a byte stamped
/// `at_cck` is due at `host_epoch + at_cck / cck_per_second`. The emulator
/// republishes it whenever it re-anchors the real-time clock, so it tracks
/// pauses and hitches.
///
/// Only the MIDI sink reads this. Without that feature it is still published,
/// harmlessly, but nothing consumes it.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(feature = "midi"), allow(dead_code))]
pub struct SerialTimeAnchor {
    pub host_epoch: Instant,
    pub cck_per_second: f64,
}

#[cfg_attr(not(feature = "midi"), allow(dead_code))]
impl SerialTimeAnchor {
    /// Host instant a byte stamped `at_cck` is due to leave the wire.
    pub fn host_time(&self, at_cck: u64) -> Instant {
        self.host_epoch + std::time::Duration::from_secs_f64(at_cck as f64 / self.cck_per_second)
    }
}

pub trait SerialSink: Send {
    /// Transmit one byte. `at_cck` is the emulated color clock the byte finished
    /// shifting out on, a monotonic power-on count. Sinks that only want the data
    /// ignore it; a timing-sensitive sink (MIDI) maps it to a host clock to keep
    /// the byte timing.
    fn write_byte(&mut self, b: u8, at_cck: u64);

    fn write_word(&mut self, word: u16, _long: bool, at_cck: u64) {
        self.write_byte((word & 0x00FF) as u8, at_cck);
    }

    fn read_byte(&mut self) -> Option<u8> {
        None
    }

    fn read_word(&mut self, _long: bool) -> Option<u16> {
        self.read_byte().map(u16::from)
    }

    /// Whether a read_word call could currently return data. Paula's idle
    /// fast path skips the receiver entirely while this is false; sinks
    /// that can produce input must override it alongside read_byte/read_word.
    fn has_pending_input(&self) -> bool {
        false
    }

    /// Update the emulated-to-host time mapping (see [`SerialTimeAnchor`]).
    /// Sinks that schedule output store it; others ignore it.
    fn set_time_anchor(&mut self, _anchor: SerialTimeAnchor) {}

    /// The MIDI sink, when this is one, for runtime device switching. `None`
    /// for every other sink.
    #[cfg(feature = "midi")]
    fn as_midi(&mut self) -> Option<&mut crate::midi::MidiSerialSink> {
        None
    }

    fn flush(&mut self);
}

/// Inert sink: discards output and never produces input. Placeholder used
/// where a `Box<dyn SerialSink>` must exist before the host wires the real
/// one (serde-skipped fields during save-state deserialization).
pub struct NullSerialSink;

impl SerialSink for NullSerialSink {
    fn write_byte(&mut self, _b: u8, _at_cck: u64) {}

    fn flush(&mut self) {}
}

pub struct StdoutSink {
    buf: Vec<u8>,
}

impl Default for StdoutSink {
    fn default() -> Self {
        Self::new()
    }
}

impl StdoutSink {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(128),
        }
    }
}

impl SerialSink for StdoutSink {
    fn write_byte(&mut self, b: u8, _at_cck: u64) {
        if b == 0 {
            return;
        }
        self.buf.push(b);
        if b == b'\n' || self.buf.len() >= 256 {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(&self.buf);
            let _ = stdout.flush();
            self.buf.clear();
        }
    }
}
