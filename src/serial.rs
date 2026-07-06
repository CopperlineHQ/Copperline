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

/// Bidirectional TCP bridge, like the UAE "TCP:" serial device (port 1234
/// there too): listens on a host port, and the connected client talks to
/// Paula's serial port -- with an
/// `AUX:` shell on the Amiga side, a full remote AmigaDOS console. One
/// client at a time; a new connection replaces a finished one. Output with
/// no client connected is dropped, like an unplugged serial cable.
///
/// A background thread owns the accept loop and the read half (pushing
/// bytes into a channel), so Paula's idle fast path polls a channel probe,
/// never a socket syscall.
pub struct TcpSerialSink {
    rx: std::sync::mpsc::Receiver<u8>,
    /// Write half of the current client, shared with the acceptor thread
    /// (which installs/clears it as clients come and go).
    writer: std::sync::Arc<std::sync::Mutex<Option<std::net::TcpStream>>>,
    /// Bytes queued in `rx`: the reader thread increments, `read_byte`
    /// decrements. Lets `has_pending_input` (`&self`) probe the channel
    /// without consuming from it. Signed so a `read_byte` that consumes a
    /// just-sent byte before the reader thread's matching `fetch_add` lands
    /// dips to a transient -1 "debt" instead of wrapping to a stuck-huge
    /// unsigned value.
    buffered: std::sync::Arc<std::sync::atomic::AtomicIsize>,
    /// The bound listen address (resolves port 0 to the real port).
    local_addr: std::net::SocketAddr,
}

impl TcpSerialSink {
    pub fn listen(addr: &str) -> anyhow::Result<Self> {
        let listener = std::net::TcpListener::bind(addr)
            .map_err(|e| anyhow::anyhow!("[serial] tcp: binding {addr}: {e}"))?;
        let local_addr = listener.local_addr()?;
        // `nc` takes host and port as separate args; formatting the
        // SocketAddr's ip()/port() keeps the hint correct for IPv6 (whose
        // literal contains colons that a `:`->` ` replace would mangle).
        log::info!(
            "serial: listening on tcp://{local_addr} (connect with e.g. \"nc {} {}\" \
             or \"socat -,raw,echo=0 tcp:{local_addr}\")",
            local_addr.ip(),
            local_addr.port(),
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = std::sync::Arc::new(std::sync::Mutex::new(None::<std::net::TcpStream>));
        let acceptor_writer = std::sync::Arc::clone(&writer);
        let buffered = std::sync::Arc::new(std::sync::atomic::AtomicIsize::new(0));
        let reader_buffered = std::sync::Arc::clone(&buffered);
        std::thread::Builder::new()
            .name("serial-tcp".into())
            .spawn(move || loop {
                let Ok((stream, peer)) = listener.accept() else {
                    return;
                };
                log::info!("serial: client connected from {peer}");
                let _ = stream.set_nodelay(true);
                match stream.try_clone() {
                    Ok(w) => *acceptor_writer.lock().unwrap() = Some(w),
                    Err(e) => {
                        log::warn!("serial: cloning client stream: {e}");
                        continue;
                    }
                }
                let mut buf = [0u8; 512];
                let mut stream = stream;
                loop {
                    use std::io::Read;
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            for &b in &buf[..n] {
                                if tx.send(b).is_err() {
                                    return;
                                }
                                reader_buffered.fetch_add(1, std::sync::atomic::Ordering::Release);
                            }
                        }
                    }
                }
                log::info!("serial: client {peer} disconnected");
                *acceptor_writer.lock().unwrap() = None;
            })?;
        Ok(Self {
            rx,
            writer,
            buffered,
            local_addr,
        })
    }

    /// The bound listen address (a port of 0 in the config resolves here).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.local_addr
    }
}

impl SerialSink for TcpSerialSink {
    fn write_byte(&mut self, b: u8, _at_cck: u64) {
        let mut guard = self.writer.lock().unwrap();
        if let Some(w) = guard.as_mut() {
            if w.write_all(&[b]).is_err() {
                *guard = None;
            }
        }
    }

    fn read_byte(&mut self) -> Option<u8> {
        let b = self.rx.try_recv().ok();
        if b.is_some() {
            self.buffered
                .fetch_sub(1, std::sync::atomic::Ordering::Release);
        }
        b
    }

    fn has_pending_input(&self) -> bool {
        self.buffered.load(std::sync::atomic::Ordering::Acquire) > 0
    }

    fn flush(&mut self) {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn tcp_sink_round_trips_input_and_output() {
        let mut sink = TcpSerialSink::listen("127.0.0.1:0").unwrap();
        let mut client = std::net::TcpStream::connect(sink.local_addr()).unwrap();
        client.write_all(b"ab").unwrap();
        // Input: wait for the reader thread to stage the bytes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !sink.has_pending_input() {
            assert!(std::time::Instant::now() < deadline, "input never arrived");
            std::thread::yield_now();
        }
        assert_eq!(sink.read_byte(), Some(b'a'));
        while !sink.has_pending_input() {
            assert!(std::time::Instant::now() < deadline, "second byte lost");
            std::thread::yield_now();
        }
        assert_eq!(sink.read_byte(), Some(b'b'));
        assert!(!sink.has_pending_input());

        // Output: bytes written to the sink arrive at the client.
        sink.write_byte(b'x', 0);
        let mut got = [0u8; 1];
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        client.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"x");
    }
}
