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

/// Bidirectional pseudo-terminal bridge. Allocates a pty pair, prints the
/// slave device path, and wires Paula's serial port to it -- so a host
/// terminal (`minicom -D <path>`, `screen <path>`, `cu -l <path>`) talks to
/// the Amiga serial port directly, with no network in the loop. With an
/// `AUX:` shell on the Amiga side, that terminal is a local AmigaDOS console.
///
/// The sink holds its own slave fd open for its whole lifetime. That keeps
/// the pty alive across terminal programs attaching and detaching, and stops
/// the master read from returning `EIO` (the "no slave attached" error a naive
/// blocking reader would hot-spin on) while nothing is attached. A background
/// thread owns the read half, pushing bytes into a channel, so Paula's idle
/// fast path polls a counter, never a syscall -- exactly like [`TcpSerialSink`].
#[cfg(unix)]
pub struct PtySerialSink {
    /// Master fd; the Amiga's output is written here and reaches the terminal.
    master: std::fs::File,
    rx: std::sync::mpsc::Receiver<u8>,
    /// Bytes staged in `rx`; signed for the same reason as
    /// [`TcpSerialSink::buffered`] (a read racing ahead of the reader thread's
    /// `fetch_add` dips to a transient -1 rather than wrapping huge).
    buffered: std::sync::Arc<std::sync::atomic::AtomicIsize>,
    /// The `/dev/pts/N` slave path terminal programs connect to.
    slave_path: String,
    /// Held open for the sink's lifetime; see the type docs.
    _slave_keepalive: std::fs::File,
}

#[cfg(unix)]
impl PtySerialSink {
    pub fn open() -> anyhow::Result<Self> {
        use std::os::fd::AsRawFd;
        let last_err = || std::io::Error::last_os_error();
        // SAFETY: posix_openpt with a valid flag set. The fd it returns is
        // wrapped in a File immediately below so it is closed on any early
        // return from this function.
        let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if master_fd < 0 {
            return Err(anyhow::anyhow!(
                "[serial] pty: posix_openpt: {}",
                last_err()
            ));
        }
        // SAFETY: master_fd is a fresh owned fd from posix_openpt.
        let master = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(master_fd) };
        // SAFETY: master_fd is a valid pty master for both of these calls.
        if unsafe { libc::grantpt(master_fd) } != 0 {
            return Err(anyhow::anyhow!("[serial] pty: grantpt: {}", last_err()));
        }
        if unsafe { libc::unlockpt(master_fd) } != 0 {
            return Err(anyhow::anyhow!("[serial] pty: unlockpt: {}", last_err()));
        }
        // ptsname's static-buffer non-reentrancy is fine here: open() runs
        // single-threaded before the reader thread is spawned, and the path is
        // copied out immediately.
        // SAFETY: master_fd is a valid pty master; the returned pointer is
        // owned by libc and only read (before any further libc call).
        let name_ptr = unsafe { libc::ptsname(master_fd) };
        if name_ptr.is_null() {
            return Err(anyhow::anyhow!("[serial] pty: ptsname: {}", last_err()));
        }
        // SAFETY: name_ptr is a valid NUL-terminated C string from ptsname.
        let slave_path = unsafe { std::ffi::CStr::from_ptr(name_ptr) }
            .to_string_lossy()
            .into_owned();

        // Hold the slave open for our lifetime (keepalive) and put the shared
        // line discipline in raw mode so it neither echoes nor rewrites CR/LF:
        // the Amiga wants the bytes verbatim.
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&slave_path)?;
        // SAFETY: slave.as_raw_fd() is a valid tty fd; termios is fully
        // initialised by tcgetattr before use and only read by cfmakeraw.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(slave.as_raw_fd(), &mut termios) != 0 {
                log::warn!(
                    "serial: pty: tcgetattr failed ({}); the terminal may echo \
                     or rewrite CR/LF",
                    io::Error::last_os_error()
                );
            } else {
                libc::cfmakeraw(&mut termios);
                if libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &termios) != 0 {
                    log::warn!(
                        "serial: pty: tcsetattr(raw) failed ({}); the terminal \
                         may echo or rewrite CR/LF",
                        io::Error::last_os_error()
                    );
                }
            }
        }

        log::info!(
            "serial: pty at {slave_path} (connect with e.g. \"minicom -D {slave_path}\", \
             \"screen {slave_path}\" or \"cu -l {slave_path}\")"
        );

        let (tx, rx) = std::sync::mpsc::channel();
        let buffered = std::sync::Arc::new(std::sync::atomic::AtomicIsize::new(0));
        let reader_buffered = std::sync::Arc::clone(&buffered);
        let mut reader = master.try_clone()?;
        std::thread::Builder::new()
            .name("serial-pty".into())
            .spawn(move || {
                use std::io::Read;
                let mut buf = [0u8; 512];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => return,
                        Ok(n) => {
                            for &b in &buf[..n] {
                                if tx.send(b).is_err() {
                                    return;
                                }
                                reader_buffered.fetch_add(1, std::sync::atomic::Ordering::Release);
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        // Anything else is teardown (the sink dropped, closing
                        // the last slave -> EIO): let the thread exit instead
                        // of spinning.
                        Err(_) => return,
                    }
                }
            })?;

        Ok(Self {
            master,
            rx,
            buffered,
            slave_path,
            _slave_keepalive: slave,
        })
    }

    /// The `/dev/pts/N` path terminal programs attach to.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn slave_path(&self) -> &str {
        &self.slave_path
    }
}

#[cfg(unix)]
impl SerialSink for PtySerialSink {
    fn write_byte(&mut self, b: u8, _at_cck: u64) {
        let _ = self.master.write_all(&[b]);
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

    fn flush(&mut self) {
        let _ = self.master.flush();
    }
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

    #[cfg(unix)]
    #[test]
    fn pty_sink_round_trips_input_and_output() {
        let mut sink = PtySerialSink::open().unwrap();
        let mut term = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(sink.slave_path())
            .unwrap();

        // Input: bytes the terminal writes reach the sink.
        term.write_all(b"ab").unwrap();
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

        // Output: bytes written to the sink arrive at the terminal. Poll with a
        // deadline first, so a delivery (or raw-mode) regression fails the test
        // instead of wedging CI on a blocking tty read.
        use std::os::fd::AsRawFd;
        sink.write_byte(b'x', 0);
        sink.flush();
        let mut pfd = libc::pollfd {
            fd: term.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: polling a single valid fd, owned by `term`, for 5000 ms.
        let n = unsafe { libc::poll(&mut pfd, 1, 5000) };
        assert!(
            n == 1 && pfd.revents & libc::POLLIN != 0,
            "output byte never arrived (poll returned {n}, revents {})",
            pfd.revents
        );
        let mut got = [0u8; 1];
        term.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"x");
    }
}
