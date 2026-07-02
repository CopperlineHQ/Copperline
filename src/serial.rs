// SPDX-License-Identifier: GPL-3.0-or-later

//! Serial output sink. Paula's SERDAT writes are funneled through here.

use std::io::{self, Write};

pub trait SerialSink: Send {
    fn write_byte(&mut self, b: u8);

    fn write_word(&mut self, word: u16, _long: bool) {
        self.write_byte((word & 0x00FF) as u8);
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

    fn flush(&mut self);
}

/// Inert sink: discards output and never produces input. Placeholder used
/// where a `Box<dyn SerialSink>` must exist before the host wires the real
/// one (serde-skipped fields during save-state deserialization).
pub struct NullSerialSink;

impl SerialSink for NullSerialSink {
    fn write_byte(&mut self, _b: u8) {}

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
    fn write_byte(&mut self, b: u8) {
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
