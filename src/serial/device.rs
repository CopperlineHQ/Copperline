// SPDX-License-Identifier: GPL-3.0-or-later

//! A real host serial port on Paula's UART (`[serial] mode = "device"`).
//!
//! The sink stands in for the cable: guest bytes leaving SERDAT go out of
//! the host port, host bytes come back in as received serial data (Paula
//! still clocks them in at the emulated SERPER rate, so the guest sees the
//! same timing it would on a real machine), the guest's `/DTR` and `/RTS`
//! outputs on CIA-B port A drive the host port's DTR and RTS pins, and the
//! host port's CTS, DSR and DCD inputs come back on CIA-B PA3-5 through
//! [`SerialSink::control_lines`]. That is the whole 7-wire null-modem
//! picture, which is what a terminal program or a file-transfer tool (Amiga
//! Explorer, a term program's ZMODEM) needs when the far end is a real
//! Amiga or a PC. The Amiga has no CIA input for RI, so ring indicator is
//! read but only reported on the host side ([`DeviceSerialSink::ring`]).
//!
//! The host port follows the guest's line settings: SERPER's bit period is
//! mapped to the nearest standard rate ([`host_baud_for`]) and the stop-bit
//! count is inferred from the SERDAT word the guest writes (Paula has no
//! stop-bit register: the guest supplies the stop bits as high bits above
//! the data, so `0x1xx` is one stop bit and `0x3xx` two). Host UARTs carry
//! at most eight data bits, so a 9-bit (`SERPER` long) word goes out with
//! its ninth bit dropped, and a received byte comes back with bit 8 clear.
//! OS-level flow control stays off: the guest owns the handshake, the same
//! as on a real machine, and the lines are mirrored rather than managed.
//!
//! Host I/O lives on two background threads so Paula's idle fast path
//! never touches a syscall: a reader that blocks on the port (sampling the
//! handshake inputs after every read, so line changes are seen within one
//! read timeout) and a writer draining a bounded queue. A port that
//! disappears mid-run -- a USB adapter pulled -- detaches rather than
//! panics: every handshake input floats high (an unplugged cable, so a
//! guest sees carrier loss), output is dropped, and the reader keeps
//! trying to reopen the same path, restoring the line settings and the
//! guest's DTR/RTS when it comes back.
//!
//! The port is a live host boundary like a network socket: nothing about
//! it is in a save state. On a state load or rewind the sink stays
//! attached, discards what was queued on the abandoned timeline, and the
//! bus republishes the restored machine's DTR/RTS and SERPER rate onto it.
//!
//! The sink is written against [`HostSerialPort`], a small trait the real
//! backend (the `serialport` crate, behind the `host-serial` feature)
//! implements, so the whole thing runs against an in-memory fake in tests.

use super::{SerialControlLines, SerialSink};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the reader blocks in one read before sampling the handshake
/// inputs again: the latency a CTS/DSR/DCD change is seen with.
pub const READ_TIMEOUT: Duration = Duration::from_millis(20);
/// How often a detached sink retries opening its port.
pub const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
/// Guest -> host bytes queued ahead of the wire. Small on purpose: in
/// real time the guest transmits at the wire's own rate and the queue
/// never fills, and an unthrottled run (warp, headless) that outruns it
/// waits on the wire rather than sending bytes the far end's CTS has
/// already refused.
pub const OUTPUT_QUEUE_CAPACITY: usize = 256;

/// The handshake inputs a host port reports, as RS-232 assertions
/// (`true` = the line is ON).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostLineInputs {
    pub cts: bool,
    pub dsr: bool,
    pub cd: bool,
    pub ri: bool,
}

/// The line settings the sink keeps the host port at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostLineSettings {
    /// Host line rate in bits per second.
    pub baud: u32,
    /// Two stop bits rather than one.
    pub two_stop_bits: bool,
    /// DTR asserted.
    pub dtr: bool,
    /// RTS asserted.
    pub rts: bool,
}

impl HostLineSettings {
    /// What the port is opened at before the guest programs SERPER: the
    /// Kickstart default, 8N1, both outputs deasserted (a machine that has
    /// not opened its port yet).
    pub const INITIAL: Self = Self {
        baud: 9600,
        two_stop_bits: false,
        dtr: false,
        rts: false,
    };
}

/// What the sink needs from a host serial port. Every method may block
/// only for the port's own bounded timeout; all of them run on the sink's
/// background threads or under a short lock from the emulation thread.
pub trait HostSerialPort: Send {
    /// Wait up to the port's read timeout for input. `Ok(0)` means nothing
    /// arrived within the timeout (not end of stream); `Err` means the
    /// port is gone.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Write everything, waiting on the wire as needed. An error means the
    /// port is gone; a timeout is reported as `io::ErrorKind::TimedOut`
    /// and may be retried.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn set_baud_rate(&mut self, bps: u32) -> io::Result<()>;
    fn set_two_stop_bits(&mut self, two: bool) -> io::Result<()>;
    fn set_dtr(&mut self, asserted: bool) -> io::Result<()>;
    fn set_rts(&mut self, asserted: bool) -> io::Result<()>;
    fn read_inputs(&mut self) -> io::Result<HostLineInputs>;
    /// Drop whatever the OS has buffered in either direction.
    fn discard_buffers(&mut self) -> io::Result<()>;
    /// A second handle on the same port, so the reader can block in
    /// `read` while the writer writes.
    fn try_clone(&self) -> io::Result<Box<dyn HostSerialPort>>;
}

/// Opens the port -- at startup, and again after an unplug.
pub type HostPortOpener = Box<dyn Fn() -> io::Result<Box<dyn HostSerialPort>> + Send + Sync>;

/// Line rates a host UART is expected to support, ascending. A guest rate
/// within [`BAUD_MATCH_TOLERANCE`] of one of these is mapped onto it; the
/// odd ones (14400, 28800, 31250 for MIDI, 76800) are here because Amiga
/// software asks for them and a USB adapter usually obliges.
pub const STANDARD_BAUD_RATES: [u32; 18] = [
    110, 300, 600, 1200, 2400, 4800, 9600, 14400, 19200, 28800, 31250, 38400, 57600, 76800, 115200,
    230400, 460800, 921600,
];
/// How far (as a fraction) a SERPER-derived rate may sit from a standard
/// rate and still be taken for it. Paula's divisor lands every standard
/// rate within 1% (19200 is really 19172, 115200 is 114416), while the
/// nearest neighbours are 33% apart, so 3% separates the two cases with
/// room to spare.
pub const BAUD_MATCH_TOLERANCE: f64 = 0.03;
/// Rates outside this range are not a line rate a guest means to use --
/// Paula's SERPER reset value of 0 works out to 3.5 Mbit/s -- and are
/// left off the host port.
pub const BAUD_RANGE: std::ops::RangeInclusive<u32> = 50..=1_000_000;

/// The host rate to program for a SERPER-derived guest rate: the standard
/// rate within [`BAUD_MATCH_TOLERANCE`], the exact rate when none is that
/// close (a driver that cannot do it says so at apply time), or `None`
/// for a rate outside [`BAUD_RANGE`].
pub fn host_baud_for(guest_bps: u32) -> Option<u32> {
    if !BAUD_RANGE.contains(&guest_bps) {
        return None;
    }
    let nearest = STANDARD_BAUD_RATES
        .iter()
        .copied()
        .min_by_key(|&rate| rate.abs_diff(guest_bps))?;
    let error = f64::from(nearest.abs_diff(guest_bps)) / f64::from(nearest);
    Some(if error <= BAUD_MATCH_TOLERANCE {
        nearest
    } else {
        guest_bps
    })
}

/// Whether a SERDAT word carries two stop bits. Paula shifts the word out
/// LSB first up to its highest set bit; the guest puts the stop bits above
/// the data (bit 8 up in 8-bit mode, bit 9 up in 9-bit mode), so two
/// consecutive set bits there are two stop bits. A word with no stop bit
/// at all is a framing error on the wire; the host port keeps one.
pub fn two_stop_bits_in(word: u16, long: bool) -> bool {
    let stop = word >> if long { 9 } else { 8 };
    stop & 0b11 == 0b11
}

/// State shared between the sink and its threads.
struct Shared {
    /// The port the writer writes and the control lines are set on;
    /// `None` while detached.
    port: Mutex<Option<Box<dyn HostSerialPort>>>,
    /// The settings the port is kept at, re-applied after a reopen.
    settings: Mutex<HostLineSettings>,
    /// `SerialControlLines` bits the guest sees; unplugged while detached.
    lines: AtomicU8,
    /// Ring indicator, host-side only.
    ring: AtomicBool,
    attached: AtomicBool,
    /// Set when the sink is dropped, so the reader stops reopening.
    shutdown: AtomicBool,
    /// Bumped when the emulated timeline jumps (a state load, a rewind).
    /// Bytes are stamped with the generation they were read or queued in,
    /// so the ones belonging to the abandoned timeline are dropped instead
    /// of reaching either endpoint late.
    generation: AtomicU64,
    /// Host bytes staged for the guest, mirrored for the idle fast path.
    buffered: AtomicIsize,
    opener: HostPortOpener,
    name: String,
}

impl Shared {
    /// Mark the port gone: lines float high, output is dropped, and the
    /// reader starts trying to reopen. Idempotent, so whichever thread
    /// notices first says so once.
    fn detach(&self, why: &io::Error) {
        if !self.attached.swap(false, Ordering::AcqRel) {
            return;
        }
        *self.port.lock().unwrap() = None;
        self.lines
            .store(SerialControlLines::UNPLUGGED.to_bits(), Ordering::Release);
        self.ring.store(false, Ordering::Release);
        log::warn!(
            "serial: host port {} lost ({why}); the guest sees an unplugged cable until it \
             comes back",
            self.name
        );
    }

    /// Bring a freshly opened port up to the guest's settings and install
    /// it as the writer/control handle, returning the reader's clone.
    fn attach(&self, mut port: Box<dyn HostSerialPort>) -> io::Result<Box<dyn HostSerialPort>> {
        let settings = *self.settings.lock().unwrap();
        apply_settings(port.as_mut(), &settings, &self.name);
        let _ = port.discard_buffers();
        let reader = port.try_clone()?;
        self.publish_inputs(&mut *port);
        *self.port.lock().unwrap() = Some(port);
        self.attached.store(true, Ordering::Release);
        Ok(reader)
    }

    /// Sample the handshake inputs and publish them for the guest.
    fn publish_inputs(&self, port: &mut dyn HostSerialPort) {
        if let Ok(inputs) = port.read_inputs() {
            let lines = SerialControlLines {
                dsr: inputs.dsr,
                cts: inputs.cts,
                cd: inputs.cd,
            };
            self.lines.store(lines.to_bits(), Ordering::Release);
            if inputs.ri != self.ring.swap(inputs.ri, Ordering::AcqRel) {
                log::info!(
                    "serial: host port {} ring indicator {}",
                    self.name,
                    if inputs.ri { "asserted" } else { "dropped" }
                );
            }
        }
    }
}

/// Program every line setting onto a port, one at a time so a driver that
/// refuses one (an odd rate, two stop bits) still gets the others. A
/// refusal is logged, not fatal: the bytes still flow at whatever the
/// port stayed at, which is better than no port.
fn apply_settings(port: &mut dyn HostSerialPort, settings: &HostLineSettings, name: &str) {
    if let Err(e) = port.set_baud_rate(settings.baud) {
        log::warn!(
            "serial: host port {name} refused {} baud ({e}); keeping its current rate",
            settings.baud
        );
    }
    if let Err(e) = port.set_two_stop_bits(settings.two_stop_bits) {
        log::warn!(
            "serial: host port {name} refused {} stop bits ({e})",
            if settings.two_stop_bits { 2 } else { 1 }
        );
    }
    if let Err(e) = port.set_dtr(settings.dtr) {
        log::warn!("serial: host port {name}: setting DTR: {e}");
    }
    if let Err(e) = port.set_rts(settings.rts) {
        log::warn!("serial: host port {name}: setting RTS: {e}");
    }
}

/// Paula's serial port wired to a real host serial port. See the module
/// docs for the model.
pub struct DeviceSerialSink {
    shared: Arc<Shared>,
    /// Host -> guest bytes from the reader thread, with the generation
    /// they were read in.
    rx: mpsc::Receiver<(u64, u8)>,
    /// Guest -> host bytes for the writer thread, likewise stamped.
    tx: mpsc::SyncSender<(u64, u8)>,
    /// Whether the 9-bit word warning has been given.
    warned_nine_bit: bool,
    /// The guest rate SERPER last set, so a repeat write is not re-applied.
    last_guest_bps: Option<u32>,
}

impl DeviceSerialSink {
    /// Open the host port at `path` (`/dev/tty.usbserial-XXXX`,
    /// `/dev/ttyUSB0`, `COM3`). Failure is a startup error that names the
    /// path, the reason, and the ports the host does have.
    #[cfg(feature = "host-serial")]
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let path = path.to_string();
        let opener: HostPortOpener = {
            let path = path.clone();
            Box::new(move || native::open(&path, HostLineSettings::INITIAL.baud))
        };
        let port = opener().map_err(|e| anyhow::anyhow!("{}", describe_open_error(&path, &e)))?;
        let sink = Self::attach(&path, port, opener)?;
        log::info!(
            "serial: host port {path} open (8N1 at {} baud until the guest programs SERPER; \
             DTR/RTS follow CIA-B, CTS/DSR/DCD feed it)",
            HostLineSettings::INITIAL.baud
        );
        Ok(sink)
    }

    /// Wire an already-open port, keeping `opener` for reopening it after
    /// an unplug. The port is brought to [`HostLineSettings::INITIAL`].
    pub fn attach(
        name: &str,
        port: Box<dyn HostSerialPort>,
        opener: HostPortOpener,
    ) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            port: Mutex::new(None),
            settings: Mutex::new(HostLineSettings::INITIAL),
            lines: AtomicU8::new(SerialControlLines::UNPLUGGED.to_bits()),
            ring: AtomicBool::new(false),
            attached: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            buffered: AtomicIsize::new(0),
            opener,
            name: name.to_string(),
        });
        let reader_port = shared
            .attach(port)
            .map_err(|e| anyhow::anyhow!("[serial] device: {name}: {e}"))?;

        let (in_tx, rx) = mpsc::channel();
        let (tx, out_rx) = mpsc::sync_channel(OUTPUT_QUEUE_CAPACITY);
        let reader_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("serial-device-rx".into())
            .spawn(move || reader_loop(reader_shared, reader_port, in_tx))?;
        let writer_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("serial-device-tx".into())
            .spawn(move || writer_loop(writer_shared, out_rx))?;
        Ok(Self {
            shared,
            rx,
            tx,
            warned_nine_bit: false,
            last_guest_bps: None,
        })
    }

    /// The path the port was opened by.
    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// Whether the port is open right now (false after an unplug, until
    /// the reopen succeeds).
    pub fn attached(&self) -> bool {
        self.shared.attached.load(Ordering::Acquire)
    }

    /// The host port's ring indicator. The Amiga has no CIA pin for it, so
    /// it never reaches the guest; a host-side status only.
    pub fn ring(&self) -> bool {
        self.shared.ring.load(Ordering::Acquire)
    }

    /// The settings the host port is being kept at.
    pub fn settings(&self) -> HostLineSettings {
        *self.shared.settings.lock().unwrap()
    }

    /// Change one line setting and push it to the port if it is open.
    fn update_settings(
        &self,
        change: impl FnOnce(&mut HostLineSettings) -> bool,
        apply: impl FnOnce(&mut dyn HostSerialPort, &HostLineSettings) -> io::Result<()>,
        what: &str,
    ) {
        let settings = {
            let mut settings = self.shared.settings.lock().unwrap();
            if !change(&mut settings) {
                return;
            }
            *settings
        };
        let mut guard = self.shared.port.lock().unwrap();
        if let Some(port) = guard.as_mut() {
            if let Err(e) = apply(port.as_mut(), &settings) {
                log::warn!("serial: host port {}: {what}: {e}", self.shared.name);
            }
        }
    }
}

impl Drop for DeviceSerialSink {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        // The writer exits when its queue's sender (ours) goes; the reader
        // sees the flag within one read timeout. Closing the control
        // handle here means the OS gets the port back promptly.
        *self.shared.port.lock().unwrap() = None;
    }
}

fn reader_loop(
    shared: Arc<Shared>,
    mut port: Box<dyn HostSerialPort>,
    tx: mpsc::Sender<(u64, u8)>,
) {
    let mut buf = [0u8; 512];
    while !shared.shutdown.load(Ordering::Acquire) {
        match port.read(&mut buf) {
            Ok(n) => {
                let generation = shared.generation.load(Ordering::Acquire);
                for &b in &buf[..n] {
                    if tx.send((generation, b)).is_err() {
                        return;
                    }
                    shared.buffered.fetch_add(1, Ordering::Release);
                }
                shared.publish_inputs(&mut *port);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                shared.detach(&e);
                drop(port);
                // Keep trying the same path until it is back or we are
                // done. The reopen sleeps first: the device node of a
                // pulled adapter lingers for a moment after the error.
                port = loop {
                    if shared.shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(RECONNECT_INTERVAL);
                    match (shared.opener)().and_then(|p| shared.attach(p)) {
                        Ok(reader) => {
                            log::info!("serial: host port {} is back", shared.name);
                            break reader;
                        }
                        Err(_) => continue,
                    }
                };
            }
        }
    }
}

fn writer_loop(shared: Arc<Shared>, rx: mpsc::Receiver<(u64, u8)>) {
    let mut batch = Vec::with_capacity(OUTPUT_QUEUE_CAPACITY);
    while let Ok((generation, first)) = rx.recv() {
        batch.clear();
        // A byte the guest queued before the timeline jumped belongs to a
        // run that no longer happened; it must not reach the wire.
        if generation == shared.generation.load(Ordering::Acquire) {
            batch.push(first);
        }
        while let Ok((generation, b)) = rx.try_recv() {
            if generation != shared.generation.load(Ordering::Acquire) {
                continue;
            }
            batch.push(b);
            if batch.len() == OUTPUT_QUEUE_CAPACITY {
                break;
            }
        }
        if batch.is_empty() {
            continue;
        }
        // Retry a full kernel buffer (the far end is slow, or has dropped
        // CTS and the driver honours it); anything else is the port gone.
        loop {
            if shared.shutdown.load(Ordering::Acquire) {
                return;
            }
            let mut guard = shared.port.lock().unwrap();
            // Under the port lock, so a jump cannot land between the test
            // and the write: the timeline moving on while this batch waited
            // for a slow far end makes it a batch from a run that no longer
            // happened.
            if generation != shared.generation.load(Ordering::Acquire) {
                break;
            }
            let Some(port) = guard.as_mut() else {
                // Detached: an unplugged cable drops what is sent down it.
                break;
            };
            match port.write_all(&batch) {
                Ok(()) => break,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    drop(guard);
                    std::thread::yield_now();
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    drop(guard);
                    shared.detach(&e);
                    break;
                }
            }
        }
    }
}

impl SerialSink for DeviceSerialSink {
    fn write_byte(&mut self, b: u8, _at_cck: u64) {
        if !self.attached() {
            return;
        }
        // Blocks only when the guest is more than a queue ahead of the
        // wire (an unthrottled run): the wire paces the machine, as it
        // would a real one. A closed queue means the writer thread is
        // gone, which only happens at teardown.
        let generation = self.shared.generation.load(Ordering::Acquire);
        let _ = self.tx.send((generation, b));
    }

    fn write_word(&mut self, word: u16, long: bool, at_cck: u64) {
        let two = two_stop_bits_in(word, long);
        self.update_settings(
            |s| {
                let changed = s.two_stop_bits != two;
                s.two_stop_bits = two;
                changed
            },
            |port, s| port.set_two_stop_bits(s.two_stop_bits),
            "setting stop bits",
        );
        if long && word & 0x100 != 0 && !self.warned_nine_bit {
            self.warned_nine_bit = true;
            log::warn!(
                "serial: the guest is sending 9-bit words; host port {} carries 8, so the \
                 ninth bit is dropped",
                self.shared.name
            );
        }
        self.write_byte((word & 0x00FF) as u8, at_cck);
    }

    fn read_byte(&mut self) -> Option<u8> {
        // Bytes the reader had already taken off the wire when the
        // timeline jumped carry the old generation and are dropped here,
        // which closes the window between the drain and the reader thread.
        loop {
            let (generation, b) = self.rx.try_recv().ok()?;
            self.shared.buffered.fetch_sub(1, Ordering::Release);
            if generation == self.shared.generation.load(Ordering::Acquire) {
                return Some(b);
            }
        }
    }

    fn has_pending_input(&self) -> bool {
        self.shared.buffered.load(Ordering::Acquire) > 0
    }

    fn can_produce_input(&self) -> bool {
        true
    }

    /// What the far end is driving on the real wire, sampled by the reader
    /// thread; every line high (unplugged) while the port is detached.
    fn control_lines(&self) -> SerialControlLines {
        SerialControlLines::from_bits(self.shared.lines.load(Ordering::Acquire))
    }

    fn set_control_outputs(&mut self, dtr: bool, rts: bool) {
        self.update_settings(
            |s| {
                let changed = s.dtr != dtr || s.rts != rts;
                s.dtr = dtr;
                s.rts = rts;
                changed
            },
            |port, s| {
                port.set_dtr(s.dtr)?;
                port.set_rts(s.rts)
            },
            "setting DTR/RTS",
        );
    }

    fn baud_changed(&mut self, bps: u32) {
        if self.last_guest_bps == Some(bps) {
            return;
        }
        self.last_guest_bps = Some(bps);
        let Some(host) = host_baud_for(bps) else {
            log::debug!(
                "serial: ignoring SERPER rate {bps} bps for host port {} (outside {:?})",
                self.shared.name,
                BAUD_RANGE
            );
            return;
        };
        if host != bps {
            log::debug!(
                "serial: SERPER rate {bps} bps -> host port {} at {host} baud",
                self.shared.name
            );
        }
        self.update_settings(
            |s| {
                let changed = s.baud != host;
                s.baud = host;
                changed
            },
            |port, s| port.set_baud_rate(s.baud),
            "setting baud rate",
        );
    }

    /// The timeline the queued host bytes belonged to is gone: drop them
    /// and whatever the OS holds, and stay attached. The bus republishes
    /// the restored machine's DTR/RTS and SERPER rate right after this.
    fn reset_after_timeline_jump(&mut self) {
        {
            // Hold the port while the generation moves, so the writer
            // cannot be between its check and its write: everything queued
            // in either direction, and anything the reader thread is
            // mid-read on, is stamped with the generation left behind.
            let mut port = self.shared.port.lock().unwrap();
            self.shared.generation.fetch_add(1, Ordering::AcqRel);
            if let Some(port) = port.as_mut() {
                let _ = port.discard_buffers();
            }
        }
        while self.read_byte().is_some() {}
    }

    fn flush(&mut self) {}
}

/// A startup failure to open `path`, spelled for the person reading the
/// log: what went wrong, the usual fix, and the ports that were found.
pub fn describe_open_error(path: &str, e: &io::Error) -> String {
    let reason = match e.kind() {
        io::ErrorKind::NotFound => "no such device".to_string(),
        io::ErrorKind::PermissionDenied => {
            "permission denied (another program may have the port open; on Linux, add your \
             user to the group that owns the device, usually dialout or uucp, and log in \
             again)"
                .to_string()
        }
        _ if e.raw_os_error() == Some(libc::EBUSY) => {
            "the port is in use by another program".to_string()
        }
        _ => e.to_string(),
    };
    let found = available_host_ports();
    let listing = if found.is_empty() {
        "no serial ports were found on this host".to_string()
    } else {
        format!("serial ports found: {}", found.join(", "))
    };
    format!("[serial] device: opening {path:?}: {reason}; {listing}")
}

/// The serial ports the host has, by the path `[serial] device` takes,
/// sorted. Empty when the build has no host-serial backend or the host
/// cannot enumerate them.
pub fn available_host_ports() -> Vec<String> {
    #[cfg(feature = "host-serial")]
    {
        native::available_ports()
    }
    #[cfg(not(feature = "host-serial"))]
    {
        Vec::new()
    }
}

/// The `serialport` backend.
#[cfg(feature = "host-serial")]
mod native {
    use super::{HostLineInputs, HostSerialPort, READ_TIMEOUT};
    use std::io;

    pub struct NativePort(Box<dyn serialport::SerialPort>);

    pub fn open(path: &str, baud: u32) -> io::Result<Box<dyn HostSerialPort>> {
        let port = serialport::new(path, baud)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            // The guest owns the handshake (see the module docs); the OS
            // only mirrors the lines.
            .flow_control(serialport::FlowControl::None)
            .timeout(READ_TIMEOUT)
            .open()
            .map_err(io::Error::from)?;
        Ok(Box::new(NativePort(port)))
    }

    pub fn available_ports() -> Vec<String> {
        let mut names: Vec<String> = serialport::available_ports()
            .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
            .unwrap_or_default();
        names.sort();
        names.dedup();
        names
    }

    impl HostSerialPort for NativePort {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match io::Read::read(&mut self.0, buf) {
                // A tty that reads zero bytes without a timeout has
                // hung up (the adapter is gone); serialport reports a
                // quiet line as TimedOut instead.
                Ok(0) => Err(io::Error::new(io::ErrorKind::BrokenPipe, "port closed")),
                Ok(n) => Ok(n),
                Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
                Err(e) => Err(e),
            }
        }

        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            io::Write::write_all(&mut self.0, bytes)
        }

        fn set_baud_rate(&mut self, bps: u32) -> io::Result<()> {
            self.0.set_baud_rate(bps).map_err(io::Error::from)
        }

        fn set_two_stop_bits(&mut self, two: bool) -> io::Result<()> {
            let bits = if two {
                serialport::StopBits::Two
            } else {
                serialport::StopBits::One
            };
            self.0.set_stop_bits(bits).map_err(io::Error::from)
        }

        fn set_dtr(&mut self, asserted: bool) -> io::Result<()> {
            self.0
                .write_data_terminal_ready(asserted)
                .map_err(io::Error::from)
        }

        fn set_rts(&mut self, asserted: bool) -> io::Result<()> {
            self.0
                .write_request_to_send(asserted)
                .map_err(io::Error::from)
        }

        fn read_inputs(&mut self) -> io::Result<HostLineInputs> {
            Ok(HostLineInputs {
                cts: self.0.read_clear_to_send()?,
                dsr: self.0.read_data_set_ready()?,
                cd: self.0.read_carrier_detect()?,
                // Not every driver reports RI; a refusal is not a lost
                // port, just no ring indicator.
                ri: self.0.read_ring_indicator().unwrap_or(false),
            })
        }

        fn discard_buffers(&mut self) -> io::Result<()> {
            self.0
                .clear(serialport::ClearBuffer::All)
                .map_err(io::Error::from)
        }

        fn try_clone(&self) -> io::Result<Box<dyn HostSerialPort>> {
            let clone = self.0.try_clone().map_err(io::Error::from)?;
            Ok(Box::new(NativePort(clone)))
        }
    }
}

/// An in-memory port for tests: what the sink writes lands in
/// `from_guest`, what the test pushes to `to_guest` the sink reads, and
/// the settings the sink programs are recorded. [`FakeHostPort::unplug`]
/// makes every call fail the way a pulled adapter does, and the opener
/// [`FakeHostPort::opener`] hands out only refuses until
/// [`FakeHostPort::replug`].
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeHostPort {
    inner: Arc<(Mutex<FakePortState>, std::sync::Condvar)>,
}

#[cfg(test)]
#[derive(Default)]
struct FakePortState {
    to_guest: std::collections::VecDeque<u8>,
    from_guest: Vec<u8>,
    inputs: HostLineInputs,
    baud: Option<u32>,
    two_stop_bits: Option<bool>,
    dtr: Option<bool>,
    rts: Option<bool>,
    discards: u32,
    dead: bool,
    /// Baud rates the fake refuses, to model a driver that cannot do them.
    refused_bauds: Vec<u32>,
    /// While set, writes report a full kernel buffer, so the writer thread
    /// holds what it has instead of putting it on the wire.
    blocked: bool,
    /// Write attempts, blocked or not: proof the writer has a batch.
    write_attempts: u32,
}

#[cfg(test)]
impl FakeHostPort {
    fn handle(&self) -> Box<dyn HostSerialPort> {
        Box::new(FakeHostPort {
            inner: Arc::clone(&self.inner),
        })
    }

    /// An opener that reopens this same fake (after `replug`).
    fn opener(&self) -> HostPortOpener {
        let inner = Arc::clone(&self.inner);
        Box::new(move || {
            if inner.0.lock().unwrap().dead {
                Err(io::Error::new(io::ErrorKind::NotFound, "no such device"))
            } else {
                Ok(Box::new(FakeHostPort {
                    inner: Arc::clone(&inner),
                }))
            }
        })
    }

    fn push_to_guest(&self, bytes: &[u8]) {
        let mut s = self.inner.0.lock().unwrap();
        s.to_guest.extend(bytes.iter().copied());
        self.inner.1.notify_all();
    }

    /// Hold (or release) the far end, so a test can leave bytes in the
    /// writer's hands across a timeline jump.
    fn block_writes(&self, blocked: bool) {
        self.inner.0.lock().unwrap().blocked = blocked;
    }

    fn take_from_guest(&self) -> Vec<u8> {
        std::mem::take(&mut self.inner.0.lock().unwrap().from_guest)
    }

    fn set_inputs(&self, inputs: HostLineInputs) {
        let mut s = self.inner.0.lock().unwrap();
        s.inputs = inputs;
        self.inner.1.notify_all();
    }

    fn unplug(&self) {
        let mut s = self.inner.0.lock().unwrap();
        s.dead = true;
        self.inner.1.notify_all();
    }

    fn replug(&self) {
        let mut s = self.inner.0.lock().unwrap();
        s.dead = false;
        s.baud = None;
        s.two_stop_bits = None;
        s.dtr = None;
        s.rts = None;
    }

    fn refuse_baud(&self, bps: u32) {
        self.inner.0.lock().unwrap().refused_bauds.push(bps);
    }

    fn state<R>(&self, f: impl FnOnce(&FakePortState) -> R) -> R {
        f(&self.inner.0.lock().unwrap())
    }
}

#[cfg(test)]
fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "device unplugged")
}

#[cfg(test)]
impl HostSerialPort for FakeHostPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let (lock, cvar) = &*self.inner;
        let mut s = lock.lock().unwrap();
        if s.to_guest.is_empty() && !s.dead {
            s = cvar.wait_timeout(s, READ_TIMEOUT).unwrap().0;
        }
        if s.dead {
            return Err(gone());
        }
        let mut n = 0;
        while n < buf.len() {
            let Some(b) = s.to_guest.pop_front() else {
                break;
            };
            buf[n] = b;
            n += 1;
        }
        Ok(n)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut s = self.inner.0.lock().unwrap();
        if s.dead {
            return Err(gone());
        }
        s.write_attempts += 1;
        if s.blocked {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        s.from_guest.extend_from_slice(bytes);
        Ok(())
    }

    fn set_baud_rate(&mut self, bps: u32) -> io::Result<()> {
        let mut s = self.inner.0.lock().unwrap();
        if s.dead {
            return Err(gone());
        }
        if s.refused_bauds.contains(&bps) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported baud",
            ));
        }
        s.baud = Some(bps);
        Ok(())
    }

    fn set_two_stop_bits(&mut self, two: bool) -> io::Result<()> {
        let mut s = self.inner.0.lock().unwrap();
        if s.dead {
            return Err(gone());
        }
        s.two_stop_bits = Some(two);
        Ok(())
    }

    fn set_dtr(&mut self, asserted: bool) -> io::Result<()> {
        let mut s = self.inner.0.lock().unwrap();
        if s.dead {
            return Err(gone());
        }
        s.dtr = Some(asserted);
        Ok(())
    }

    fn set_rts(&mut self, asserted: bool) -> io::Result<()> {
        let mut s = self.inner.0.lock().unwrap();
        if s.dead {
            return Err(gone());
        }
        s.rts = Some(asserted);
        Ok(())
    }

    fn read_inputs(&mut self) -> io::Result<HostLineInputs> {
        let s = self.inner.0.lock().unwrap();
        if s.dead {
            return Err(gone());
        }
        Ok(s.inputs)
    }

    fn discard_buffers(&mut self) -> io::Result<()> {
        let mut s = self.inner.0.lock().unwrap();
        if s.dead {
            return Err(gone());
        }
        s.to_guest.clear();
        s.discards += 1;
        Ok(())
    }

    fn try_clone(&self) -> io::Result<Box<dyn HostSerialPort>> {
        if self.inner.0.lock().unwrap().dead {
            return Err(gone());
        }
        Ok(self.handle())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "{what}: never happened"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn open_fake() -> (DeviceSerialSink, FakeHostPort) {
        let fake = FakeHostPort::default();
        let sink = DeviceSerialSink::attach("fake0", fake.handle(), fake.opener()).unwrap();
        (sink, fake)
    }

    #[test]
    fn serper_rates_map_to_the_nearest_standard_host_rate() {
        // Paula's divisor never lands exactly on a standard rate; each of
        // these is what PAULA_CLOCK_HZ / (n + 1) gives for the usual n.
        for (guest, host) in [
            (9586, 9600),
            (19172, 19200),
            (38139, 38400),
            (57208, 57600),
            (114416, 115200),
            (2400, 2400),
            (31388, 31250),
            (1200, 1200),
        ] {
            assert_eq!(host_baud_for(guest), Some(host), "{guest} bps");
        }
        // A rate nowhere near a standard one passes through as-is, for
        // the driver to accept or refuse.
        assert_eq!(host_baud_for(250_000), Some(250_000));
        assert_eq!(host_baud_for(45_000), Some(45_000));
        // SERPER's reset value works out to 3.5 Mbit/s: not a line rate.
        assert_eq!(host_baud_for(3_546_895), None);
        assert_eq!(host_baud_for(10), None);
    }

    #[test]
    fn stop_bits_are_read_off_the_serdat_word() {
        // 8-bit words: the guest sets bit 8 for one stop bit, 8-9 for two.
        assert!(!two_stop_bits_in(0x141, false));
        assert!(two_stop_bits_in(0x341, false));
        // No stop bit at all is a framing error; the host keeps one.
        assert!(!two_stop_bits_in(0x041, false));
        // 9-bit words: the stop bits start at bit 9.
        assert!(!two_stop_bits_in(0x341, true));
        assert!(two_stop_bits_in(0x741, true));
    }

    #[test]
    fn bytes_round_trip_through_the_host_port() {
        let (mut sink, fake) = open_fake();
        assert!(sink.attached());
        assert!(sink.can_produce_input());

        // Host -> guest: staged by the reader thread, one byte per read.
        fake.push_to_guest(b"ab");
        wait_until("input staged", || sink.has_pending_input());
        assert_eq!(sink.read_byte(), Some(b'a'));
        wait_until("second byte staged", || sink.has_pending_input());
        assert_eq!(sink.read_byte(), Some(b'b'));
        assert!(!sink.has_pending_input());
        assert_eq!(sink.read_byte(), None);

        // Guest -> host: SERDAT words land as their data byte.
        sink.write_word(0x141, false, 0);
        sink.write_word(0x142, false, 1);
        wait_until("output written", || fake.state(|s| s.from_guest.len() == 2));
        assert_eq!(fake.take_from_guest(), b"AB");
    }

    #[test]
    fn handshake_inputs_reach_the_guest_and_ring_stays_host_side() {
        let (sink, fake) = open_fake();
        // A port with nothing driving its inputs: the lines float high.
        assert_eq!(sink.control_lines(), SerialControlLines::UNPLUGGED);
        fake.set_inputs(HostLineInputs {
            cts: true,
            dsr: true,
            cd: false,
            ri: false,
        });
        wait_until("DSR/CTS seen", || {
            sink.control_lines() == SerialControlLines::READY
        });
        fake.set_inputs(HostLineInputs {
            cts: true,
            dsr: true,
            cd: true,
            ri: true,
        });
        wait_until("carrier seen", || {
            sink.control_lines() == SerialControlLines::CONNECTED
        });
        wait_until("ring seen", || sink.ring());
        // CTS alone maps to its own line.
        fake.set_inputs(HostLineInputs {
            cts: true,
            dsr: false,
            cd: false,
            ri: false,
        });
        wait_until("CTS alone", || {
            sink.control_lines()
                == SerialControlLines {
                    dsr: false,
                    cts: true,
                    cd: false,
                }
        });
        assert!(!sink.ring());
    }

    #[test]
    fn guest_dtr_and_rts_drive_the_host_port() {
        let (mut sink, fake) = open_fake();
        // Attaching brings the port to the initial settings: both down.
        assert_eq!(fake.state(|s| (s.dtr, s.rts)), (Some(false), Some(false)));
        sink.set_control_outputs(true, false);
        assert_eq!(fake.state(|s| (s.dtr, s.rts)), (Some(true), Some(false)));
        sink.set_control_outputs(true, true);
        assert_eq!(fake.state(|s| (s.dtr, s.rts)), (Some(true), Some(true)));
        assert_eq!(
            sink.settings(),
            HostLineSettings {
                dtr: true,
                rts: true,
                ..HostLineSettings::INITIAL
            }
        );
    }

    #[test]
    fn serper_and_serdat_program_the_host_line_settings() {
        let (mut sink, fake) = open_fake();
        assert_eq!(fake.state(|s| s.baud), Some(9600));
        sink.baud_changed(19172);
        assert_eq!(fake.state(|s| s.baud), Some(19200));
        assert_eq!(sink.settings().baud, 19200);
        // A rate the driver refuses is logged and the port keeps its rate;
        // the sink still remembers what the guest asked for.
        fake.refuse_baud(250_000);
        sink.baud_changed(250_000);
        assert_eq!(fake.state(|s| s.baud), Some(19200));
        assert_eq!(sink.settings().baud, 250_000);
        // Not a line rate: left alone entirely.
        sink.baud_changed(3_546_895);
        assert_eq!(sink.settings().baud, 250_000);

        // Stop bits follow the SERDAT word.
        assert_eq!(fake.state(|s| s.two_stop_bits), Some(false));
        sink.write_word(0x341, false, 0);
        assert_eq!(fake.state(|s| s.two_stop_bits), Some(true));
        sink.write_word(0x141, false, 1);
        assert_eq!(fake.state(|s| s.two_stop_bits), Some(false));
        // A 9-bit word goes out as its low byte.
        sink.write_word(0x2FF, true, 2);
        wait_until("output written", || fake.state(|s| s.from_guest.len() == 3));
        assert_eq!(fake.take_from_guest(), [0x41, 0x41, 0xFF]);
    }

    #[test]
    fn unplug_degrades_to_an_unplugged_cable_and_replug_restores_the_settings() {
        let (mut sink, fake) = open_fake();
        fake.set_inputs(HostLineInputs {
            cts: true,
            dsr: true,
            cd: true,
            ri: false,
        });
        wait_until("carrier seen", || {
            sink.control_lines() == SerialControlLines::CONNECTED
        });
        sink.set_control_outputs(true, true);
        sink.baud_changed(57208);

        fake.unplug();
        wait_until("detach noticed", || !sink.attached());
        // No carrier, no panic: writes are dropped, reads yield nothing,
        // outputs are remembered for the reopen.
        assert_eq!(sink.control_lines(), SerialControlLines::UNPLUGGED);
        sink.write_word(0x141, false, 0);
        sink.set_control_outputs(false, true);
        assert_eq!(sink.read_byte(), None);

        // Back again: the reader reopens it and programs the settings the
        // guest had reached in the meantime.
        fake.replug();
        wait_until("reattached", || sink.attached());
        assert_eq!(fake.state(|s| s.baud), Some(57600));
        assert_eq!(fake.state(|s| (s.dtr, s.rts)), (Some(false), Some(true)));
        wait_until("carrier seen again", || {
            sink.control_lines() == SerialControlLines::CONNECTED
        });
        sink.write_word(0x142, false, 1);
        wait_until("output flows again", || {
            fake.state(|s| !s.from_guest.is_empty())
        });
        assert_eq!(fake.take_from_guest(), b"B");
    }

    #[test]
    fn timeline_jump_drops_queued_host_bytes_but_keeps_the_port() {
        let (mut sink, fake) = open_fake();
        fake.push_to_guest(b"stale");
        wait_until("input staged", || sink.has_pending_input());
        sink.reset_after_timeline_jump();
        assert!(!sink.has_pending_input());
        assert_eq!(sink.read_byte(), None);
        assert!(sink.attached());
        // The attach discards once, the jump a second time.
        assert_eq!(fake.state(|s| s.discards), 2);
        // And the port still works afterwards.
        fake.push_to_guest(b"z");
        wait_until("fresh input", || sink.has_pending_input());
        assert_eq!(sink.read_byte(), Some(b'z'));
    }

    /// The other direction: bytes the guest wrote before the jump belong
    /// to a run that no longer happened, so the wire must never see them,
    /// even when the writer is already holding them against a far end that
    /// has not taken them yet.
    #[test]
    fn timeline_jump_drops_guest_bytes_still_queued_for_the_wire() {
        let (mut sink, fake) = open_fake();
        // A far end that will not take anything: the writer keeps the
        // bytes rather than putting them on the wire.
        fake.block_writes(true);
        sink.write_byte(b'a', 0);
        sink.write_byte(b'b', 0);
        wait_until("the writer holds the batch", || {
            fake.state(|s| s.write_attempts > 0)
        });

        sink.reset_after_timeline_jump();
        fake.block_writes(false);
        // Whatever the writer does from here, those two bytes are gone: a
        // fresh byte written after the jump is the only thing that lands.
        sink.write_byte(b'c', 0);
        wait_until("the fresh byte reaches the wire", || {
            fake.state(|s| s.from_guest.contains(&b'c'))
        });
        assert_eq!(
            fake.state(|s| s.from_guest.clone()),
            vec![b'c'],
            "bytes from the abandoned timeline reached the wire"
        );
    }

    #[test]
    fn open_errors_say_what_went_wrong() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let text = describe_open_error("/dev/ttyUSB0", &denied);
        assert!(text.contains("/dev/ttyUSB0"), "{text}");
        assert!(text.contains("dialout"), "{text}");
        let missing = io::Error::from(io::ErrorKind::NotFound);
        let text = describe_open_error("COM7", &missing);
        assert!(text.contains("no such device"), "{text}");
        assert!(text.contains("serial ports"), "{text}");
        let busy = io::Error::from_raw_os_error(libc::EBUSY);
        let text = describe_open_error("/dev/cu.usbserial-1", &busy);
        assert!(text.contains("in use by another program"), "{text}");
    }
}
