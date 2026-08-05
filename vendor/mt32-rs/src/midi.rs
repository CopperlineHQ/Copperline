// SPDX-License-Identifier: LGPL-2.1-or-later

//! The MIDI byte stream, parsed: running status, System Realtime bytes
//! arriving in the middle of anything, SysEx frames split across calls,
//! and the module's tolerance for garbage -- malformed data is dropped
//! with a note and the stream carries on from the next byte that makes
//! sense. This is the front door a guest's serial port feeds.
//!
//! Events come out through [Sink] in arrival order, exactly as the
//! reference parser delivers them: a Realtime byte lodged inside another
//! message is handed over the moment it is seen, before the message it
//! interrupted completes.

/// The most a SysEx reassembled from fragments can grow to before the
/// module gives up on it. A frame delivered whole in one [Parser::parse]
/// call bypasses the store and has no such limit.
const MAX_BUFFER: usize = 32768;

/// Where parsed events land. The two silent defaults match the module:
/// Realtime bytes and malformed data are noted and otherwise ignored.
pub trait Sink {
    /// A complete short message, status in the low byte, data bytes
    /// above it.
    fn short_message(&mut self, message: u32);

    /// A complete SysEx frame, F0 through F7 inclusive.
    fn sysex(&mut self, frame: &[u8]);

    /// A System Realtime byte, possibly from inside another message.
    fn realtime(&mut self, byte: u8) {
        let _ = byte;
    }

    /// Something malformed was dropped; parsing continues.
    fn dropped(&mut self, what: &'static str) {
        let _ = what;
    }
}

/// The stream's state between calls: the message being assembled, and
/// the running status the next data byte would borrow.
pub struct Parser {
    buffer: Vec<u8>,
    running_status: u8,
}

impl Default for Parser {
    fn default() -> Parser {
        Parser::new()
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            buffer: Vec::new(),
            running_status: 0,
        }
    }

    /// Feed the parser a run of bytes; complete events land in `sink`
    /// as they finish. A message may span any number of calls.
    pub fn parse(&mut self, mut stream: &[u8], sink: &mut impl Sink) {
        while !stream.is_empty() {
            let parsed = if stream[0] >= 0xF8 {
                // Realtime passes straight through, touching nothing.
                sink.realtime(stream[0]);
                1
            } else if !self.buffer.is_empty() {
                if self.buffer[0] == 0xF0 {
                    self.parse_sysex_fragment(stream, sink)
                } else {
                    self.parse_short_data(stream, sink)
                }
            } else if stream[0] == 0xF0 {
                self.running_status = 0;
                self.parse_sysex(stream, sink)
            } else {
                self.parse_short_status(stream, sink)
            };
            stream = &stream[parsed..];
        }
    }

    /// The status byte's effect on the running status, and the status a
    /// data byte in its place would borrow. Returns the effective status
    /// -- still below 0x80 when there is nothing to borrow -- and
    /// whether a substitution happened.
    fn effective_status(&mut self, status: u8) -> (u8, bool) {
        if status < 0x80 {
            if self.running_status < 0x80 {
                return (status, false);
            }
            return (self.running_status, true);
        }
        if status < 0xF0 {
            self.running_status = status;
        } else if status < 0xF8 {
            // System Common clears the running status.
            self.running_status = 0;
        }
        (status, false)
    }

    /// A message opens: its status into the store, borrowed from the
    /// running status if the byte at hand is data. A borrowed status
    /// consumes nothing -- the byte at hand is the first data byte and
    /// parses again next round.
    fn parse_short_status(&mut self, stream: &[u8], sink: &mut impl Sink) -> usize {
        let (status, substituted) = self.effective_status(stream[0]);
        if status >= 0x80 {
            self.buffer.push(status);
        } else {
            sink.dropped("a data byte with no running status to borrow");
        }
        usize::from(!substituted)
    }

    /// Data bytes for the short message being assembled. A stray status
    /// byte drops the message and is left unconsumed, to open the next
    /// one; Realtime is handed through and does not count.
    fn parse_short_data(&mut self, stream: &[u8], sink: &mut impl Sink) -> usize {
        let want = short_message_length(self.buffer[0]);
        let mut parsed = 0;
        while self.buffer.len() < want && parsed < stream.len() {
            let byte = stream[parsed];
            if byte < 0x80 {
                self.buffer.push(byte);
            } else if byte < 0xF8 {
                sink.dropped("a status byte inside a short message");
                self.buffer.clear();
                return parsed;
            } else {
                sink.realtime(byte);
            }
            parsed += 1;
        }
        if self.buffer.len() < want {
            return parsed;
        }
        let mut message = 0u32;
        for (i, &byte) in self.buffer.iter().enumerate() {
            message |= u32::from(byte) << (i * 8);
        }
        sink.short_message(message);
        self.buffer.clear();
        parsed
    }

    /// A SysEx opens. Terminated within this call, the frame is handed
    /// over straight from the input; interrupted by Realtime or by the
    /// call's end, what has arrived goes to the store for the fragments
    /// to come. Any other status byte inside voids the frame and parses
    /// again as the next message.
    fn parse_sysex(&mut self, stream: &[u8], sink: &mut impl Sink) -> usize {
        let mut len = 1;
        while len < stream.len() {
            let byte = stream[len];
            len += 1;
            if byte < 0x80 {
                continue;
            }
            if byte == 0xF7 {
                sink.sysex(&stream[..len]);
                return len;
            }
            if byte >= 0xF8 {
                // Handled next round, before the fragment resumes.
                len -= 1;
                break;
            }
            sink.dropped("a SysEx without its terminator");
            return len - 1;
        }
        if len < MAX_BUFFER {
            self.buffer.extend_from_slice(&stream[..len]);
        } else {
            // Too big to store: keep the frame marker and a full store,
            // so the fragment path drains and drops the rest.
            self.buffer.resize(MAX_BUFFER, 0);
            self.buffer[0] = 0xF0;
        }
        len
    }

    /// More of a stored SysEx. Data grows the store while it fits; the
    /// terminator delivers the frame, unless the store overran, which
    /// drops it whole.
    fn parse_sysex_fragment(&mut self, stream: &[u8], sink: &mut impl Sink) -> usize {
        let mut parsed = 0;
        while parsed < stream.len() {
            let byte = stream[parsed];
            parsed += 1;
            if byte < 0x80 {
                if self.buffer.len() < MAX_BUFFER {
                    self.buffer.push(byte);
                }
                continue;
            }
            if byte >= 0xF8 {
                sink.realtime(byte);
                continue;
            }
            if byte != 0xF7 {
                sink.dropped("a SysEx without its terminator");
                self.buffer.clear();
                parsed -= 1;
                break;
            }
            if self.buffer.len() < MAX_BUFFER {
                self.buffer.push(byte);
                sink.sysex(&self.buffer);
            } else {
                sink.dropped("a fragmented SysEx past what the module accepts");
            }
            self.buffer.clear();
            break;
        }
        parsed
    }
}

/// How many bytes a short message runs, status included. The reference
/// notes its own table is not quite right about System Common -- the
/// note travels with the behaviour.
fn short_message_length(status: u8) -> usize {
    if status & 0xF0 == 0xF0 {
        match status {
            0xF1 | 0xF3 => 2,
            0xF2 => 3,
            _ => 1,
        }
    } else if status & 0xE0 == 0xC0 {
        2
    } else {
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log {
        shorts: Vec<u32>,
        frames: Vec<Vec<u8>>,
        realtimes: Vec<u8>,
        drops: usize,
    }

    impl Sink for Log {
        fn short_message(&mut self, message: u32) {
            self.shorts.push(message);
        }
        fn sysex(&mut self, frame: &[u8]) {
            self.frames.push(frame.to_vec());
        }
        fn realtime(&mut self, byte: u8) {
            self.realtimes.push(byte);
        }
        fn dropped(&mut self, _what: &'static str) {
            self.drops += 1;
        }
    }

    /// Running status carries a stream of note pairs, and a System
    /// Common message takes it away again.
    #[test]
    fn running_status_carries_and_clears() {
        let mut parser = Parser::new();
        let mut log = Log::default();
        parser.parse(&[0x91, 0x3C, 0x7F, 0x40, 0x7F, 0x3C, 0x00], &mut log);
        assert_eq!(log.shorts, [0x007F_3C91, 0x007F_4091, 0x0000_3C91]);
        parser.parse(&[0xF6, 0x40, 0x00], &mut log);
        assert_eq!(log.shorts.len(), 4, "tune request came through");
        assert_eq!(log.drops, 2, "both data bytes had nothing to borrow");
    }

    /// A SysEx splits across calls, swallows a Realtime byte on the way,
    /// and still comes out whole; a bare status inside one voids it.
    #[test]
    fn sysex_reassembles_around_interruptions() {
        let mut parser = Parser::new();
        let mut log = Log::default();
        parser.parse(&[0xF0, 0x41, 0x10], &mut log);
        parser.parse(&[0x16, 0xF8, 0x12], &mut log);
        parser.parse(&[0xF7], &mut log);
        assert_eq!(log.frames, [&[0xF0, 0x41, 0x10, 0x16, 0x12, 0xF7]]);
        assert_eq!(log.realtimes, [0xF8]);

        parser.parse(&[0xF0, 0x41, 0x91, 0x3C, 0x7F], &mut log);
        assert_eq!(log.drops, 1, "the frame died at the stray status");
        assert_eq!(log.shorts, [0x007F_3C91], "which opened a note instead");
    }

    /// One byte at a time is the same stream as all at once.
    #[test]
    fn chunking_changes_nothing() {
        let stream = [
            0x91, 0x3C, 0x7F, 0xF0, 0x41, 0x10, 0x16, 0x12, 0xF7, 0x3C, 0x00, 0x92, 0xFE, 0x40,
            0x60,
        ];
        let mut whole = Log::default();
        Parser::new().parse(&stream, &mut whole);
        let mut split = Log::default();
        let mut parser = Parser::new();
        for byte in stream {
            parser.parse(&[byte], &mut split);
        }
        assert_eq!(whole.shorts, split.shorts);
        assert_eq!(whole.frames, split.frames);
        assert_eq!(whole.realtimes, split.realtimes);
    }
}
