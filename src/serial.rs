// SPDX-License-Identifier: GPL-3.0-or-later

//! Serial output sink. Paula's SERDAT writes are funneled through here.

use crate::timebase::Instant;
use std::collections::VecDeque;
use std::io::{self, Write};

/// Maximum number of completed Paula transmissions retained for a host-side
/// observer. The tap evicts the oldest word once full; it must never let a
/// debugger client apply unbounded back-pressure to the emulated UART.
pub const SERIAL_OBSERVATION_CAPACITY: usize = 4096;

/// One word that finished shifting out of Paula's serial transmitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialTxObservation {
    pub word: u16,
    pub long: bool,
    pub at_cck: u64,
}

/// Bounded, host-side tap used by streaming debugger transports. This is not
/// emulated state and is deliberately kept independent of the active serial
/// sink, so observing output cannot change what the configured device sees.
#[derive(Default)]
pub struct SerialObserver {
    pending: VecDeque<SerialTxObservation>,
    dropped: u64,
}

impl SerialObserver {
    pub fn push(&mut self, observation: SerialTxObservation) {
        if self.pending.len() == SERIAL_OBSERVATION_CAPACITY {
            self.pending.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.pending.push_back(observation);
    }

    pub fn drain(&mut self) -> (Vec<SerialTxObservation>, u64) {
        let observations = self.pending.drain(..).collect();
        let dropped = std::mem::take(&mut self.dropped);
        (observations, dropped)
    }
}

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

/// CIA-B port A pin of the RS-232 /DSR input (Data Set Ready, DB25 pin 6).
pub const CIAB_PA_DSR: u8 = 1 << 3;
/// CIA-B port A pin of the RS-232 /CTS input (Clear To Send, DB25 pin 5).
pub const CIAB_PA_CTS: u8 = 1 << 4;
/// CIA-B port A pin of the RS-232 /CD input (Carrier Detect, DB25 pin 8).
pub const CIAB_PA_CD: u8 = 1 << 5;
/// The three RS-232 control inputs on CIA-B port A. The outputs /RTS (PA6)
/// and /DTR (PA7) are the CIA's own pins; the remaining PA0-2 are the
/// Centronics status inputs.
pub const CIAB_PA_SERIAL_INPUTS: u8 = CIAB_PA_DSR | CIAB_PA_CTS | CIAB_PA_CD;

/// The RS-232 control lines the device on the far end of the serial cable
/// drives back at the Amiga: DSR, CTS, and carrier detect. Each is `true`
/// when the line is asserted (the RS-232 ON state). The motherboard's 1489
/// receivers invert them onto CIA-B port A -- an asserted line reads as a
/// LOW pin, and a line nothing drives is pulled up -- so `serial.device`'s
/// 7-wire handshake and `SDCMD_QUERY` status see exactly what the attached
/// device is saying.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SerialControlLines {
    /// Data Set Ready: the far-end device is powered and present.
    pub dsr: bool,
    /// Clear To Send: the far-end device accepts data right now.
    pub cts: bool,
    /// Carrier Detect: a connection exists beyond the far-end device (a
    /// modem has a carrier; a null-modem peer has its port open).
    pub cd: bool,
}

impl SerialControlLines {
    /// Nothing plugged in: every input floats high on its pull-up.
    pub const UNPLUGGED: Self = Self {
        dsr: false,
        cts: false,
        cd: false,
    };
    /// A device is present and accepting data, but has no carrier: a modem
    /// with no call up, or a bridge with no client connected.
    pub const READY: Self = Self {
        dsr: true,
        cts: true,
        cd: false,
    };
    /// A device is present, accepting data, and connected through.
    pub const CONNECTED: Self = Self {
        dsr: true,
        cts: true,
        cd: true,
    };

    /// Pack into a bitmask for lock-free mirrors (bit 0 = DSR, 1 = CTS,
    /// 2 = CD). Round-trips through [`from_bits`](Self::from_bits).
    fn to_bits(self) -> u8 {
        u8::from(self.dsr) | u8::from(self.cts) << 1 | u8::from(self.cd) << 2
    }

    fn from_bits(bits: u8) -> Self {
        Self {
            dsr: bits & 1 != 0,
            cts: bits & 2 != 0,
            cd: bits & 4 != 0,
        }
    }

    /// The pin levels CIA-B port A samples on PA3-5, inverted through the
    /// 1489 receivers: bit clear where a line is asserted, set where it is
    /// not. Other bits are zero.
    pub fn cia_b_pa_levels(self) -> u8 {
        let mut high = CIAB_PA_SERIAL_INPUTS;
        if self.dsr {
            high &= !CIAB_PA_DSR;
        }
        if self.cts {
            high &= !CIAB_PA_CTS;
        }
        if self.cd {
            high &= !CIAB_PA_CD;
        }
        high
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

    /// Whether this sink can EVER produce input (a live host byte source is
    /// wired up), regardless of whether any is pending right now. Input
    /// arrives on the host's schedule, invisible to the emulated event
    /// horizon, so the emulator bounds its blind idle fast-forwards while
    /// such a sink is attached (a real machine halted in STOP wakes within
    /// microseconds of RBF; a multi-millisecond blind nap would overrun the
    /// one-word receive buffer instead). Output-only sinks keep the default.
    fn can_produce_input(&self) -> bool {
        false
    }

    /// The RS-232 control inputs (/DSR, /CTS, /CD) the far-end device is
    /// driving at CIA-B port A right now. The bus samples this on every
    /// guest read of CIA-B PRA, so it must be cheap and must not block.
    /// The default is an unplugged cable -- every input pulled high -- which
    /// is what a guest waiting on CTS or carrier sees on a real machine with
    /// nothing attached. Sinks that stand in for a real device override it:
    /// a present device asserts DSR and CTS, and carrier follows whether
    /// the connection beyond it is up.
    fn control_lines(&self) -> SerialControlLines {
        SerialControlLines::UNPLUGGED
    }

    /// Whether this host endpoint can remain attached during run-ahead.
    /// Speculative transmissions are withheld until their committed
    /// re-emulation, so an output-only byte stream is safe when it has no
    /// host-timing or connection state. Endpoints that can feed bytes back
    /// into the guest, schedule MIDI, or own a live connection keep the
    /// conservative default.
    fn runahead_safe(&self) -> bool {
        false
    }

    /// Update the emulated-to-host time mapping (see [`SerialTimeAnchor`]).
    /// Sinks that schedule output store it; others ignore it.
    fn set_time_anchor(&mut self, _anchor: SerialTimeAnchor) {}

    /// Guest-driven RS-232 outputs, as logical assertions (true = asserted).
    /// The bus decodes them from CIA-B PA7 (/DTR) and PA6 (/RTS) wire
    /// levels, which are active-low; the decode happens there, once.
    fn set_control_outputs(&mut self, _dtr: bool, _rts: bool) {}

    /// The guest reprogrammed SERPER; `bps` is the resulting line rate.
    /// Pacing stays Paula's job -- this only informs a sink whose protocol
    /// text quotes the rate (a modem's CONNECT message).
    fn baud_changed(&mut self, _bps: u32) {}

    /// Once-per-span heartbeat: Paula calls this on every `tick_serial`,
    /// stamped with that span's end color clock, whether or not any byte
    /// moved in either direction. Most sinks have nothing to do with it and
    /// keep the default. It exists for a sink whose protocol runs its own
    /// timers off elapsed emulated time rather than guest traffic -- an
    /// escape-sequence trailing guard that must complete even if the guest
    /// falls silent right after it, or a ring cadence with nothing on the
    /// wire to pace it.
    fn poll(&mut self, _at_cck: u64) {}

    /// Discard host-side state tied to an emulated timeline that has just
    /// been abandoned by save-state load or rewind.
    ///
    /// Ordinary byte sinks have nothing to do. Stateful devices that cannot
    /// be serialized must restart rather than silently continuing from the
    /// future that was just left behind.
    fn reset_after_timeline_jump(&mut self) {}

    /// The machine reset under the sink: an in-process synthesizer
    /// treats it as its line dropping and releases what it was
    /// holding. Ordinary byte sinks have nothing to do.
    fn machine_reset(&mut self) {}

    /// The MIDI sink, when this is one, for runtime device switching. `None`
    /// for every other sink.
    #[cfg(feature = "midi")]
    fn as_midi(&mut self) -> Option<&mut crate::midi::MidiSerialSink> {
        None
    }

    /// The same sink shared, for reads that only look.
    #[cfg(feature = "midi")]
    fn as_midi_ref(&self) -> Option<&crate::midi::MidiSerialSink> {
        None
    }

    /// One stereo frame of what the device on the far end is sounding, at
    /// the mixer's rate, or `None` from a device that makes no sound here.
    ///
    /// Only a device emulated in this process has anything to say: a host
    /// MIDI endpoint makes its noise on the host, out of the emulator's
    /// reach. The mixer treats what comes back the way it treats CD audio --
    /// a line-level source beside Paula's own output, never in place of it.
    ///
    /// A `None` is final for the life of the sink, so the mixer asks once and
    /// then stops. Buffering belongs to whoever implements this, which keeps
    /// the mixer free of state for a device that is usually not there.
    fn next_audio_frame(&mut self) -> Option<(f32, f32)> {
        None
    }

    /// What the in-process synth's line is called in stem captures.
    /// Meaningful only while [`next_audio_frame`](Self::next_audio_frame)
    /// answers; the default names the MT-32, the historical occupant.
    fn synth_source_name(&self) -> &'static str {
        "mt32"
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
/// [`TcpSerialSink::connect`] is the outbound counterpart: it dials a remote
/// service (a telnet BBS, a modem bridge) instead of waiting for one.
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
    /// Whether a peer is connected right now: set by the acceptor (or at
    /// dial time), cleared when the connection ends. This is the modem's
    /// carrier as the guest sees it on CIA-B /CD; an atomic so the bus can
    /// sample it on every PRA read without touching the writer lock.
    connected: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The bound listen address (resolves port 0 to the real port) in listen
    /// mode; the connected peer in connect mode.
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
        let connected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acceptor_connected = std::sync::Arc::clone(&connected);
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
                acceptor_connected.store(true, std::sync::atomic::Ordering::Release);
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
                acceptor_connected.store(false, std::sync::atomic::Ordering::Release);
            })?;
        Ok(Self {
            rx,
            writer,
            buffered,
            connected,
            local_addr,
        })
    }

    /// Dial `addr` and bridge Paula's serial port to that connection: the
    /// outbound counterpart of [`TcpSerialSink::listen`], for talking to a
    /// service that expects to be called (a telnet BBS, `tcpser`, a
    /// `socat`-exposed device). The connection is made once at startup; if
    /// the peer disconnects, output is dropped like an unplugged cable and
    /// input simply stops (restart the emulator to redial).
    pub fn connect(addr: &str) -> anyhow::Result<Self> {
        let stream = std::net::TcpStream::connect(addr)
            .map_err(|e| anyhow::anyhow!("[serial] tcp-connect: connecting {addr}: {e}"))?;
        let peer = stream.peer_addr()?;
        let _ = stream.set_nodelay(true);
        log::info!("serial: connected to tcp://{peer}");
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = std::sync::Arc::new(std::sync::Mutex::new(Some(stream.try_clone()?)));
        let reader_writer = std::sync::Arc::clone(&writer);
        let buffered = std::sync::Arc::new(std::sync::atomic::AtomicIsize::new(0));
        let reader_buffered = std::sync::Arc::clone(&buffered);
        let connected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let reader_connected = std::sync::Arc::clone(&connected);
        std::thread::Builder::new()
            .name("serial-tcp-connect".into())
            .spawn(move || {
                use std::io::Read;
                let mut stream = stream;
                let mut buf = [0u8; 512];
                loop {
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
                log::info!("serial: {peer} disconnected");
                *reader_writer.lock().unwrap() = None;
                reader_connected.store(false, std::sync::atomic::Ordering::Release);
            })?;
        Ok(Self {
            rx,
            writer,
            buffered,
            connected,
            local_addr: peer,
        })
    }

    /// The bound listen address (a port of 0 in the config resolves here);
    /// the peer address in connect mode.
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
                self.connected
                    .store(false, std::sync::atomic::Ordering::Release);
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

    fn can_produce_input(&self) -> bool {
        true
    }

    /// The bridge is a modem: powered and accepting data (DSR, CTS) from
    /// the moment it is configured, with carrier only while a peer is
    /// connected -- so a guest BBS sees the caller hang up as carrier loss,
    /// and a terminal program sees the dial-out drop the same way.
    fn control_lines(&self) -> SerialControlLines {
        if self.connected.load(std::sync::atomic::Ordering::Acquire) {
            SerialControlLines::CONNECTED
        } else {
            SerialControlLines::READY
        }
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
        use std::os::fd::AsRawFd;
        // Drop output the receiver is not draining rather than block the
        // emulation thread, mirroring TcpSerialSink dropping output with no
        // client connected -- like an unplugged serial cable. With no terminal
        // attached, only our keepalive slave holds the pts open and nothing
        // reads it, so the pty buffer fills and a blocking master write would
        // wedge the emulator forever. Poll for room first (0 ms) and drop the
        // byte if there is none.
        let mut pfd = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: polling a single valid fd owned by `self.master`, 0 ms.
        let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
        // Write only when poll positively reports room. A rare EINTR (or any
        // poll error) leaves `ready < 0` and simply drops the byte -- correct
        // for a lossy serial line, and deliberately not an EINTR retry loop
        // (the pattern that makes programs stop responding to ^C).
        if ready == 1 && pfd.revents & libc::POLLOUT != 0 {
            let _ = self.master.write_all(&[b]);
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

    fn can_produce_input(&self) -> bool {
        true
    }

    /// A null-modem cable to a host terminal. Its DTR and RTS cross to our
    /// DSR/CD and CTS, and the terminal's side is invisible from here (the
    /// sink keeps the slave open itself), so the lines are reported as a
    /// terminal that has the port open.
    fn control_lines(&self) -> SerialControlLines {
        SerialControlLines::CONNECTED
    }

    fn flush(&mut self) {
        let _ = self.master.flush();
    }
}

/// Guest->host bytes the channel sink retains while the host does not drain
/// them. Paula's transmitter tops out around 31 KB/s at practical baud rates,
/// so this is minutes of headroom; once full the oldest byte is evicted (and
/// counted), because a frontend that stopped draining must never apply
/// unbounded memory pressure or back-pressure to the emulated UART.
pub const CHANNEL_OUTPUT_CAPACITY: usize = 256 * 1024;

/// Queues shared between a [`ChannelSerialSink`] (owned by Paula) and the
/// [`ChannelSerialHandle`] the host frontend keeps.
#[derive(Default)]
struct ChannelSerialShared {
    /// Host -> guest bytes waiting for Paula's receiver.
    input: VecDeque<u8>,
    /// Guest -> host bytes waiting for the frontend to drain.
    output: VecDeque<u8>,
    /// Output bytes evicted because the frontend stopped draining.
    output_dropped: u64,
}

/// Bidirectional in-process serial bridge with no host I/O of its own: the
/// frontend pushes received bytes in and drains transmitted bytes out through
/// a [`ChannelSerialHandle`]. This is the browser build's serial transport --
/// the page owns the socket (a WebSocket to a telnet bridge), and wasm32 has
/// no threads or TCP -- but it works the same on any host, so a frontend can
/// wire the serial port to any byte stream it likes.
pub struct ChannelSerialSink {
    shared: std::sync::Arc<std::sync::Mutex<ChannelSerialShared>>,
    /// Mirror of the input queue length, so `has_pending_input` (`&self`, on
    /// Paula's idle fast path) reads an atomic instead of taking the lock.
    /// Signed for the same transient-debt reason as
    /// [`TcpSerialSink::buffered`].
    buffered: std::sync::Arc<std::sync::atomic::AtomicIsize>,
    /// The handshake lines the frontend's far end is driving, as
    /// [`SerialControlLines`] bits. An atomic for the same reason as
    /// `buffered`: the bus samples `control_lines` on every guest CIA-B
    /// PRA read, which must never wait on the frontend holding the queue
    /// mutex in `push_input`/`take_output`.
    lines: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

impl ChannelSerialSink {
    /// Create the sink plus the handle the host side keeps. The handle is
    /// `Clone`, so a frontend can hold it in several places.
    pub fn pair() -> (Self, ChannelSerialHandle) {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(ChannelSerialShared::default()));
        let buffered = std::sync::Arc::new(std::sync::atomic::AtomicIsize::new(0));
        // A bridge is present and accepting data from the start; the
        // frontend raises carrier once its own connection is up.
        let lines = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
            SerialControlLines::READY.to_bits(),
        ));
        let handle = ChannelSerialHandle {
            shared: std::sync::Arc::clone(&shared),
            buffered: std::sync::Arc::clone(&buffered),
            lines: std::sync::Arc::clone(&lines),
        };
        (
            Self {
                shared,
                buffered,
                lines,
            },
            handle,
        )
    }
}

impl SerialSink for ChannelSerialSink {
    fn write_byte(&mut self, b: u8, _at_cck: u64) {
        let mut shared = self.shared.lock().unwrap();
        if shared.output.len() == CHANNEL_OUTPUT_CAPACITY {
            shared.output.pop_front();
            shared.output_dropped += 1;
        }
        shared.output.push_back(b);
    }

    fn read_byte(&mut self) -> Option<u8> {
        let b = self.shared.lock().unwrap().input.pop_front();
        if b.is_some() {
            self.buffered
                .fetch_sub(1, std::sync::atomic::Ordering::Release);
        }
        b
    }

    fn has_pending_input(&self) -> bool {
        self.buffered.load(std::sync::atomic::Ordering::Acquire) > 0
    }

    fn can_produce_input(&self) -> bool {
        true
    }

    fn control_lines(&self) -> SerialControlLines {
        SerialControlLines::from_bits(self.lines.load(std::sync::atomic::Ordering::Acquire))
    }

    fn flush(&mut self) {}
}

/// The host side of a [`ChannelSerialSink`].
#[derive(Clone)]
pub struct ChannelSerialHandle {
    shared: std::sync::Arc<std::sync::Mutex<ChannelSerialShared>>,
    buffered: std::sync::Arc<std::sync::atomic::AtomicIsize>,
    lines: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

impl ChannelSerialHandle {
    /// Queue bytes for Paula's receiver (host -> guest). The input queue is
    /// unbounded on purpose: the UART consumes it at the emulated baud rate,
    /// so a fast remote (a file download) backlogs here rather than being
    /// dropped, and the caller paces its own reads with [`input_backlog`]
    /// (stop reading the socket while the backlog is large).
    ///
    /// [`input_backlog`]: ChannelSerialHandle::input_backlog
    pub fn push_input(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.shared
            .lock()
            .unwrap()
            .input
            .extend(bytes.iter().copied());
        self.buffered
            .fetch_add(bytes.len() as isize, std::sync::atomic::Ordering::Release);
    }

    /// Drain everything the guest has transmitted since the last call.
    pub fn take_output(&self) -> Vec<u8> {
        self.shared.lock().unwrap().output.drain(..).collect()
    }

    /// Bytes pushed with [`push_input`] that the guest's UART has not yet
    /// consumed. Flow control for the pushing side.
    ///
    /// [`push_input`]: ChannelSerialHandle::push_input
    pub fn input_backlog(&self) -> usize {
        self.shared.lock().unwrap().input.len()
    }

    /// Total guest output bytes evicted because [`take_output`] was not
    /// called often enough (see [`CHANNEL_OUTPUT_CAPACITY`]). Diagnostics.
    ///
    /// [`take_output`]: ChannelSerialHandle::take_output
    pub fn output_overflow(&self) -> u64 {
        self.shared.lock().unwrap().output_dropped
    }

    /// Set the RS-232 control inputs the guest sees on CIA-B (/DSR, /CTS,
    /// /CD). The sink starts as a present, ready device with no carrier
    /// ([`SerialControlLines::READY`]); a frontend bridging to a socket
    /// raises carrier when that connection opens and drops it when it
    /// closes, which is how a guest BBS or terminal notices the hang-up.
    pub fn set_control_lines(&self, lines: SerialControlLines) {
        self.lines
            .store(lines.to_bits(), std::sync::atomic::Ordering::Release);
    }

    /// Raise or drop carrier detect alone, keeping DSR and CTS asserted:
    /// the one line a byte-stream bridge actually knows the state of.
    pub fn set_carrier(&self, connected: bool) {
        self.set_control_lines(if connected {
            SerialControlLines::CONNECTED
        } else {
            SerialControlLines::READY
        });
    }

    /// The control inputs currently presented to the guest.
    pub fn control_lines(&self) -> SerialControlLines {
        SerialControlLines::from_bits(self.lines.load(std::sync::atomic::Ordering::Acquire))
    }
}

/// Inert sink: discards output and never produces input. Placeholder used
/// where a `Box<dyn SerialSink>` must exist before the host wires the real
/// one (serde-skipped fields during save-state deserialization).
pub struct NullSerialSink;

impl SerialSink for NullSerialSink {
    fn write_byte(&mut self, _b: u8, _at_cck: u64) {}

    fn flush(&mut self) {}

    fn runahead_safe(&self) -> bool {
        true
    }
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

    /// The host console is a serial printer or terminal that is always
    /// ready for data (DSR, CTS) but has no call behind it, so a guest
    /// polling for carrier sees none rather than a phantom caller.
    fn control_lines(&self) -> SerialControlLines {
        SerialControlLines::READY
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(&self.buf);
            let _ = stdout.flush();
            self.buf.clear();
        }
    }

    fn runahead_safe(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn serial_observer_evicts_oldest_records_at_its_limit() {
        let mut observer = SerialObserver::default();
        for word in 0..=SERIAL_OBSERVATION_CAPACITY {
            observer.push(SerialTxObservation {
                word: word as u16,
                long: false,
                at_cck: word as u64,
            });
        }
        let (records, dropped) = observer.drain();
        assert_eq!(records.len(), SERIAL_OBSERVATION_CAPACITY);
        assert_eq!(records[0].word, 1);
        assert_eq!(dropped, 1);
        assert!(observer.drain().0.is_empty());
    }

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

    #[test]
    fn tcp_connect_sink_round_trips_input_and_output() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = std::thread::spawn(move || listener.accept().unwrap().0);
        let mut sink = TcpSerialSink::connect(&addr.to_string()).unwrap();
        let mut peer = accepted.join().unwrap();

        // Input: bytes the remote service sends reach the sink.
        peer.write_all(b"ab").unwrap();
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

        // Output: bytes written to the sink arrive at the remote service.
        sink.write_byte(b'x', 0);
        let mut got = [0u8; 1];
        peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        peer.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"x");
    }

    #[test]
    fn control_lines_invert_onto_cia_b_port_a_pins() {
        // Unplugged: the 1489 outputs idle high on the pull-ups.
        assert_eq!(
            SerialControlLines::UNPLUGGED.cia_b_pa_levels(),
            CIAB_PA_DSR | CIAB_PA_CTS | CIAB_PA_CD
        );
        // A present device with no carrier: /DSR and /CTS low, /CD high.
        assert_eq!(SerialControlLines::READY.cia_b_pa_levels(), CIAB_PA_CD);
        // Connected through: all three low.
        assert_eq!(SerialControlLines::CONNECTED.cia_b_pa_levels(), 0);
        // Each line maps to its own pin.
        let cts_only = SerialControlLines {
            dsr: false,
            cts: true,
            cd: false,
        };
        assert_eq!(cts_only.cia_b_pa_levels(), CIAB_PA_DSR | CIAB_PA_CD);
        // The default sink (an output-only byte sink) is an unplugged cable.
        assert_eq!(
            NullSerialSink.control_lines(),
            SerialControlLines::UNPLUGGED
        );
        assert_eq!(StdoutSink::new().control_lines(), SerialControlLines::READY);
    }

    fn wait_for_lines(sink: &dyn SerialSink, want: SerialControlLines, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while sink.control_lines() != want {
            assert!(
                std::time::Instant::now() < deadline,
                "{what}: lines stuck at {:?}",
                sink.control_lines()
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn tcp_listen_sink_carrier_follows_the_client() {
        let mut sink = TcpSerialSink::listen("127.0.0.1:0").unwrap();
        // A listening modem is powered and ready, but nobody has called.
        assert_eq!(sink.control_lines(), SerialControlLines::READY);

        let client = std::net::TcpStream::connect(sink.local_addr()).unwrap();
        wait_for_lines(&sink, SerialControlLines::CONNECTED, "client connect");

        // The caller hangs up: carrier drops, DSR and CTS stay up.
        drop(client);
        wait_for_lines(&sink, SerialControlLines::READY, "client hang-up");
        // Output into the dropped connection stays a no-op and keeps carrier
        // down.
        sink.write_byte(b'x', 0);
        assert_eq!(sink.control_lines(), SerialControlLines::READY);
    }

    #[test]
    fn tcp_connect_sink_carrier_drops_when_the_remote_hangs_up() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = std::thread::spawn(move || listener.accept().unwrap().0);
        let sink = TcpSerialSink::connect(&addr.to_string()).unwrap();
        let peer = accepted.join().unwrap();
        // The dial-out succeeded, so carrier is up from the start.
        assert_eq!(sink.control_lines(), SerialControlLines::CONNECTED);

        drop(peer);
        wait_for_lines(&sink, SerialControlLines::READY, "remote hang-up");
    }

    #[test]
    fn channel_sink_control_lines_follow_the_handle() {
        let (sink, handle) = ChannelSerialSink::pair();
        // A bridge is present and ready before the page has a connection.
        assert_eq!(sink.control_lines(), SerialControlLines::READY);
        assert_eq!(handle.control_lines(), SerialControlLines::READY);
        handle.set_carrier(true);
        assert_eq!(sink.control_lines(), SerialControlLines::CONNECTED);
        handle.set_carrier(false);
        assert_eq!(sink.control_lines(), SerialControlLines::READY);
        handle.set_control_lines(SerialControlLines::UNPLUGGED);
        assert_eq!(sink.control_lines(), SerialControlLines::UNPLUGGED);
    }

    #[test]
    fn tcp_connect_to_nothing_is_an_error() {
        // Bind-then-drop yields a port with no listener behind it.
        let addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        assert!(TcpSerialSink::connect(&addr.to_string()).is_err());
    }

    #[test]
    fn channel_sink_round_trips_input_and_output() {
        let (mut sink, handle) = ChannelSerialSink::pair();

        assert!(!sink.has_pending_input());
        handle.push_input(b"ab");
        assert_eq!(handle.input_backlog(), 2);
        assert!(sink.has_pending_input());
        assert_eq!(sink.read_byte(), Some(b'a'));
        assert_eq!(sink.read_byte(), Some(b'b'));
        assert_eq!(sink.read_byte(), None);
        assert!(!sink.has_pending_input());
        assert_eq!(handle.input_backlog(), 0);

        sink.write_byte(b'x', 0);
        sink.write_byte(b'y', 1);
        assert_eq!(handle.take_output(), b"xy");
        assert!(handle.take_output().is_empty());
        assert_eq!(handle.output_overflow(), 0);
    }

    #[test]
    fn channel_sink_bounds_undrained_output() {
        let (mut sink, handle) = ChannelSerialSink::pair();
        for i in 0..CHANNEL_OUTPUT_CAPACITY + 3 {
            sink.write_byte(i as u8, i as u64);
        }
        let drained = handle.take_output();
        assert_eq!(drained.len(), CHANNEL_OUTPUT_CAPACITY);
        // The oldest three bytes were evicted, so the queue starts at 3.
        assert_eq!(drained[0], 3u8);
        assert_eq!(handle.output_overflow(), 3);
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
