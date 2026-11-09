// SPDX-License-Identifier: GPL-3.0-or-later

//! The controlling terminal: raw mode, the protocol modes the session turns
//! on, the start-up capability probe, and the writer thread frames go out on.
//!
//! The session talks to `/dev/tty` directly, so it works whatever stdout and
//! stderr point at. While it owns the terminal, a stdout or stderr that *is*
//! the terminal is pointed at a log file instead -- the log would otherwise be
//! printed over the picture -- and put back when the session ends, by `Drop`
//! or by the panic hook, whichever comes first.

use super::graphics::{self, FrameEncoder, Geometry, Placement, FRAME_IMAGE_IDS};
use super::input::{Event, Parser};
use anyhow::{anyhow, bail, Context as _, Result};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Kitty keyboard enhancements the session asks for: disambiguate escape
/// codes (1), report event types -- releases above all (2), report alternate
/// keys -- the base-layout key (4), and report every key as an escape code,
/// modifiers included (8).
pub const KEYBOARD_FLAGS: u32 = 1 | 2 | 4 | 8;

/// Image id of the start-up graphics probe; frames use [`FRAME_IMAGE_IDS`].
const PROBE_IMAGE_ID: u32 = 31;

const ENTER: &str = concat!(
    "\x1b[?1049h", // alternate screen
    "\x1b[?25l",   // hide the cursor
    "\x1b[2J",
    "\x1b[?1003h", // report all motion
    "\x1b[?1006h", // SGR mouse encoding...
    "\x1b[?1016h", // ...in pixels
    "\x1b[?1004h", // focus in/out
    "\x1b[?2004h", // bracketed paste
);

const LEAVE: &str = concat!(
    // A string terminator first: a frame write given up on part-way (see
    // `Output::finish`) can leave the terminal inside an image command,
    // which would otherwise swallow the rest. Outside one it does nothing.
    "\x1b\\",
    "\x1b_Ga=d,d=A,q=2\x1b\\", // delete every image
    "\x1b[<u",                 // pop the keyboard flags
    "\x1b[?2004l\x1b[?1004l\x1b[?1016l\x1b[?1006l\x1b[?1003l",
    "\x1b[0m\x1b[2J\x1b[H",
    "\x1b[?25h",
    "\x1b[?1049l",
);

/// What the terminal answered to the start-up probe.
#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    pub graphics: bool,
    /// The keyboard flags in force, if the terminal speaks the Kitty
    /// keyboard protocol at all.
    pub keyboard_flags: Option<u32>,
    /// One cell's pixel size, if the terminal reported it.
    pub cell_pixels: Option<(u16, u16)>,
}

/// How long giving the terminal back may wait for it to take the bytes
/// that turn the session's modes off. A link that is not draining at all
/// still gets its terminal mode restored.
const RESTORE_WRITE_BUDGET: Duration = Duration::from_secs(1);

/// What the panic hook needs to give the terminal back.
struct Restore {
    /// The blocking handle the terminal mode was read from.
    fd: RawFd,
    /// The non-blocking output handle, for writes that must not hang.
    out_fd: RawFd,
    termios: libc::termios,
    log_path: Option<PathBuf>,
}

static RESTORE: Mutex<Option<Restore>> = Mutex::new(None);

fn restore_terminal(restore: &Restore) {
    let deadline = Instant::now() + RESTORE_WRITE_BUDGET;
    let _ = write_until(restore.out_fd, LEAVE.as_bytes(), || {
        Instant::now() >= deadline
    });
    // SAFETY: fd is the terminal this session opened and still owns, and
    // termios is the state tcgetattr read from it.
    unsafe {
        libc::tcsetattr(restore.fd, libc::TCSANOW, &restore.termios);
    }
}

/// Write all of `bytes` to the non-blocking `fd`, waiting for room as it
/// fills, until `give_up` says to stop. Returns whether everything was
/// written.
fn write_until(fd: RawFd, mut bytes: &[u8], give_up: impl Fn() -> bool) -> std::io::Result<bool> {
    while !bytes.is_empty() {
        if give_up() {
            return Ok(false);
        }
        // SAFETY: bytes is a live slice; fd is an open descriptor.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n > 0 {
            bytes = &bytes[n as usize..];
            continue;
        }
        let err = std::io::Error::last_os_error();
        match err.kind() {
            std::io::ErrorKind::Interrupted => {}
            std::io::ErrorKind::WouldBlock => wait_fd(fd, true, Duration::from_millis(20))?,
            _ if n == 0 => wait_fd(fd, true, Duration::from_millis(20))?,
            _ => return Err(err),
        }
    }
    Ok(true)
}

/// Wait up to `timeout` for `fd` to become readable (or writable). This is
/// select(2), not poll(2): macOS poll does not support `/dev/tty` and
/// reports it ready at once.
fn wait_fd(fd: RawFd, writable: bool, timeout: Duration) -> std::io::Result<()> {
    // FD_SETSIZE's integer type differs between libc targets.
    #[allow(clippy::unnecessary_cast)]
    let limit = libc::FD_SETSIZE as usize;
    if fd < 0 || fd as usize >= limit {
        // Out of select's reach (a process with a thousand files open):
        // poll by sleeping a little instead.
        std::thread::sleep(timeout.min(Duration::from_millis(5)));
        return Ok(());
    }
    // SAFETY: fd_set is plain data that FD_ZERO/FD_SET initialise, for a
    // descriptor checked to be below FD_SETSIZE above.
    let ready = unsafe {
        let mut set: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut set);
        libc::FD_SET(fd, &mut set);
        let mut tv = libc::timeval {
            tv_sec: timeout.as_secs().min(i32::MAX as u64) as libc::time_t,
            tv_usec: timeout.subsec_micros() as libc::suseconds_t,
        };
        let set: *mut libc::fd_set = &mut set;
        let (read, write) = if writable {
            (std::ptr::null_mut(), set)
        } else {
            (set, std::ptr::null_mut())
        };
        libc::select(fd + 1, read, write, std::ptr::null_mut(), &mut tv)
    };
    if ready < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(())
}

fn write_fd(fd: RawFd, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        // SAFETY: bytes is a live slice; fd is an open descriptor.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

/// The terminal, owned for the life of the session.
pub struct Terminal {
    /// Blocking handle: the terminal mode, the window size, and the start-up
    /// writes.
    tty: File,
    /// Non-blocking opens of the terminal for reading, and for the writer
    /// thread and the restore, which must never hang on a stalled link. Each
    /// is its own open file description, so O_NONBLOCK stays off `tty`.
    input: File,
    output: File,
    /// stdout/stderr descriptors pointed at the log, with their originals.
    redirected: Vec<(RawFd, OwnedFd)>,
    log_path: Option<PathBuf>,
    restored: bool,
}

impl Terminal {
    /// Open the controlling terminal, put it in raw mode, point a stdout or
    /// stderr that is the terminal at `log_path`, and turn on the protocol
    /// modes. Fails without changing anything when there is no terminal.
    pub fn open(log_path: &Path) -> Result<Self> {
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
            .open("/dev/tty")
            .context("--terminal needs a terminal (opening /dev/tty)")?;
        let input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open("/dev/tty")
            .context("opening /dev/tty for input")?;
        let output = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open("/dev/tty")
            .context("opening /dev/tty for output")?;
        let fd = tty.as_raw_fd();
        let out_fd = output.as_raw_fd();
        // SAFETY: termios is plain data; tcgetattr fills it from an open fd.
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut termios) } != 0 {
            return Err(std::io::Error::last_os_error()).context("reading the terminal mode");
        }
        let mut raw = termios;
        // SAFETY: raw is a valid termios copy; fd is open.
        unsafe {
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("switching the terminal to raw mode");
            }
        }
        let mut terminal = Self {
            tty,
            input,
            output,
            redirected: Vec::new(),
            log_path: None,
            restored: false,
        };
        *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Restore {
            fd,
            out_fd,
            termios,
            log_path: None,
        });
        install_panic_hook();
        terminal.redirect_output(log_path)?;
        terminal.write(ENTER.as_bytes())?;
        terminal.write(format!("\x1b[>{KEYBOARD_FLAGS}u").as_bytes())?;
        Ok(terminal)
    }

    fn redirect_output(&mut self, log_path: &Path) -> Result<()> {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        let ttys: Vec<RawFd> = [libc::STDOUT_FILENO, libc::STDERR_FILENO]
            .into_iter()
            // SAFETY: isatty only inspects the descriptor.
            .filter(|&fd| unsafe { libc::isatty(fd) } == 1)
            .collect();
        if ttys.is_empty() {
            return Ok(());
        }
        let log = File::create(log_path)
            .with_context(|| format!("creating the log file {}", log_path.display()))?;
        for fd in ttys {
            // SAFETY: dup/dup2 on open descriptors; the duplicate is owned.
            let saved = unsafe { libc::dup(fd) };
            if saved < 0 {
                return Err(std::io::Error::last_os_error()).context("saving stdout/stderr");
            }
            let saved = unsafe { OwnedFd::from_raw_fd(saved) };
            if unsafe { libc::dup2(log.as_raw_fd(), fd) } < 0 {
                return Err(std::io::Error::last_os_error()).context("redirecting the log");
            }
            self.redirected.push((fd, saved));
        }
        self.log_path = Some(log_path.to_path_buf());
        if let Some(restore) = RESTORE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            restore.log_path = self.log_path.clone();
        }
        Ok(())
    }

    /// Where the log went, if it was redirected.
    pub fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        write_fd(self.tty.as_raw_fd(), bytes).context("writing to the terminal")
    }

    /// A handle on the terminal's non-blocking output, for the writer
    /// thread.
    pub fn output_handle(&self) -> Result<File> {
        self.output
            .try_clone()
            .context("duplicating the terminal handle")
    }

    /// Read what input is there, waiting up to `timeout` for some when there
    /// is none yet. Returns the number of bytes read (0 when none came in
    /// time).
    pub fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        if let Some(n) = self.read_now(buf)? {
            return Ok(n);
        }
        if timeout.is_zero() {
            return Ok(0);
        }
        self.wait_readable(timeout)?;
        Ok(self.read_now(buf)?.unwrap_or(0))
    }

    /// One non-blocking read: `None` when nothing is waiting.
    fn read_now(&self, buf: &mut [u8]) -> Result<Option<usize>> {
        // SAFETY: buf is a live, writable slice; the descriptor is open.
        let n = unsafe { libc::read(self.input.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if matches!(
                err.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) {
                return Ok(None);
            }
            return Err(err).context("reading terminal input");
        }
        if n == 0 {
            bail!("the terminal went away");
        }
        Ok(Some(n as usize))
    }

    /// Wait up to `timeout` for input.
    fn wait_readable(&self, timeout: Duration) -> Result<()> {
        wait_fd(self.input.as_raw_fd(), false, timeout).context("waiting for terminal input")
    }

    /// The window size: columns, rows, and the text area's pixel size (zero
    /// where the terminal does not report it).
    pub fn window_size(&self) -> Option<(u16, u16, u16, u16)> {
        // SAFETY: winsize is plain data; TIOCGWINSZ fills it from an open fd.
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.tty.as_raw_fd(), libc::TIOCGWINSZ, &mut size) } != 0 {
            return None;
        }
        (size.ws_col > 0 && size.ws_row > 0).then_some((
            size.ws_col,
            size.ws_row,
            size.ws_xpixel,
            size.ws_ypixel,
        ))
    }

    /// Ask what the terminal supports and wait (up to two seconds) for the
    /// answers. The device-attributes query goes last: every terminal
    /// answers it, and answers come back in order, so its reply means every
    /// reply that is coming has come. Anything else typed meanwhile is
    /// dropped.
    pub fn probe(&self) -> Result<Capabilities> {
        let query = format!(
            "\x1b[?u\x1b_Gi={PROBE_IMAGE_ID},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c"
        );
        self.write(query.as_bytes())?;
        let mut caps = Capabilities::default();
        let mut parser = Parser::new();
        let mut events = Vec::new();
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let n = self.read(&mut buf, left)?;
            parser.feed(&buf[..n], &mut events);
            let mut done = false;
            for event in events.drain(..) {
                match event {
                    Event::KeyboardFlags(flags) => caps.keyboard_flags = Some(flags),
                    Event::Graphics { id, ok } if id == PROBE_IMAGE_ID => caps.graphics = ok,
                    Event::CellPixels { width, height } if width > 0 && height > 0 => {
                        caps.cell_pixels = Some((width as u16, height as u16));
                    }
                    Event::DeviceAttributes => done = true,
                    _ => {}
                }
            }
            if done {
                break;
            }
        }
        Ok(caps)
    }

    fn restore(&mut self) {
        if std::mem::replace(&mut self.restored, true) {
            return;
        }
        if let Some(restore) = RESTORE.lock().unwrap_or_else(|e| e.into_inner()).take() {
            restore_terminal(&restore);
        }
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        for (fd, saved) in self.redirected.drain(..) {
            // SAFETY: both descriptors are open; dup2 puts the original back.
            unsafe {
                libc::dup2(saved.as_raw_fd(), fd);
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.restore();
    }
}

fn install_panic_hook() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Give the terminal back first, so the user is not left with a raw,
        // mouse-reporting alternate screen; the panic itself goes to the log
        // with the rest of the output.
        if let Some(restore) = RESTORE.lock().unwrap_or_else(|e| e.into_inner()).take() {
            restore_terminal(&restore);
            let note = match &restore.log_path {
                Some(path) => format!("copperline: panicked; details in {}\r\n", path.display()),
                None => "copperline: panicked\r\n".to_string(),
            };
            let deadline = Instant::now() + RESTORE_WRITE_BUDGET;
            let _ = write_until(restore.out_fd, note.as_bytes(), || {
                Instant::now() >= deadline
            });
        }
        previous(info);
    }));
}

/// A frame for the writer thread.
struct Frame {
    pixels: Vec<u32>,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct Mailbox {
    /// The newest frame not yet sent; a newer one replaces it unsent.
    frame: Option<Frame>,
    status: Option<String>,
    geometry: Option<Geometry>,
    closed: bool,
    /// Spent pixel buffers, handed back for reuse.
    spare: Vec<Vec<u32>>,
}

impl Mailbox {
    fn has_work(&self) -> bool {
        self.frame.is_some() || self.status.is_some() || self.geometry.is_some() || self.closed
    }
}

/// Counters the writer thread keeps, for the status line.
#[derive(Default)]
pub struct Stats {
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    pub failed: AtomicBool,
}

/// Where the picture is now, as the writer thread last placed it, for
/// mapping mouse positions back to frame pixels.
#[derive(Clone, Copy, Debug, Default)]
pub struct Shown {
    pub placement: Option<Placement>,
    pub width: u32,
    pub height: u32,
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
    /// Set to stop at once: the writer gives up a write in progress and any
    /// frame-rate wait instead of finishing them.
    cancel: AtomicBool,
    stats: Stats,
    shown: Mutex<Shown>,
}

/// The writer thread: frames and status lines are posted to it and it
/// writes them to the terminal. Only the newest unsent frame is kept, so a
/// link slower than the machine drops frames instead of holding the machine
/// back (or its sound).
pub struct Output {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Output {
    /// Start the writer on `tty`, laid out for `geometry`. `min_interval`
    /// spaces frames out to cap the frame rate. `tty` is switched to
    /// non-blocking writes, so stopping never waits on the link.
    pub fn start(tty: File, geometry: Geometry, min_interval: Option<Duration>) -> Result<Self> {
        let fd = tty.as_raw_fd();
        // SAFETY: fcntl on an open descriptor this function owns a handle on.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("making terminal output non-blocking");
            }
        }
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox {
                geometry: Some(geometry),
                ..Mailbox::default()
            }),
            wake: Condvar::new(),
            cancel: AtomicBool::new(false),
            stats: Stats::default(),
            shown: Mutex::new(Shown::default()),
        });
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("terminal-output".into())
            .spawn(move || writer(tty, &thread_shared, min_interval))
            .map_err(|e| anyhow!("starting the terminal writer: {e}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Post a frame (`width` x `height` RGBA8 pixels), replacing any frame
    /// not yet sent.
    pub fn post_frame(&self, pixels: &[u32], width: u32, height: u32) {
        let mut buf = self
            .shared
            .mailbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .spare
            .pop()
            .unwrap_or_default();
        buf.clear();
        buf.extend_from_slice(pixels);
        let mut mailbox = self
            .shared
            .mailbox
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(old) = mailbox.frame.replace(Frame {
            pixels: buf,
            width,
            height,
        }) {
            mailbox.spare.push(old.pixels);
        }
        drop(mailbox);
        self.shared.wake.notify_one();
    }

    pub fn post_status(&self, text: String) {
        self.shared
            .mailbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status = Some(text);
        self.shared.wake.notify_one();
    }

    /// The pane changed size: clear it and lay the picture out again.
    pub fn post_geometry(&self, geometry: Geometry) {
        self.shared
            .mailbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .geometry = Some(geometry);
        self.shared.wake.notify_one();
    }

    pub fn stats(&self) -> &Stats {
        &self.shared.stats
    }

    pub fn shown(&self) -> Shown {
        *self.shared.shown.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stop the writer now. A frame still going out is cut short and a
    /// frame-rate wait ends early, so quitting never waits on the link; the
    /// terminal's restore sequence closes whatever command was cut.
    pub fn finish(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
        self.shared
            .mailbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closed = true;
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop();
    }
}

fn writer(tty: File, shared: &Shared, min_interval: Option<Duration>) {
    let fd = tty.as_raw_fd();
    let cancelled = || shared.cancel.load(Ordering::Relaxed);
    let mut encoder = FrameEncoder::new();
    let mut geometry: Option<Geometry> = None;
    let mut status = String::new();
    // The frame on screen: its pixels (to skip unchanged frames and redraw
    // after a resize), size, placement and image id.
    let mut last: Option<Frame> = None;
    let mut shown_id: Option<u32> = None;
    let mut shown_placement: Option<Placement> = None;
    let mut out = Vec::with_capacity(1 << 20);
    loop {
        let (frame, new_status, new_geometry, closed) = {
            let mut mailbox = shared.mailbox.lock().unwrap_or_else(|e| e.into_inner());
            while !mailbox.has_work() {
                mailbox = shared.wake.wait(mailbox).unwrap_or_else(|e| e.into_inner());
            }
            (
                mailbox.frame.take(),
                mailbox.status.take(),
                mailbox.geometry.take(),
                mailbox.closed,
            )
        };
        let started = Instant::now();
        out.clear();
        let mut redraw = false;
        if let Some(g) = new_geometry {
            geometry = Some(g);
            out.extend_from_slice(b"\x1b[2J");
            graphics::delete_all_images(&mut out);
            shown_id = None;
            shown_placement = None;
            redraw = true;
        }
        let Some(g) = geometry else {
            if closed {
                return;
            }
            continue;
        };
        let mut sent_frame = false;
        let changed = match (&frame, &last) {
            (Some(f), Some(l)) => {
                f.width != l.width || f.height != l.height || f.pixels != l.pixels
            }
            (Some(_), None) => true,
            (None, _) => false,
        };
        if let Some(f) = frame {
            if let Some(old) = last.replace(f) {
                shared
                    .mailbox
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .spare
                    .push(old.pixels);
            }
        }
        if changed || redraw {
            if let Some(f) = &last {
                if let Some(placement) = Placement::fit(f.width, f.height, g) {
                    if shown_placement.is_some_and(|p| p != placement) {
                        // The frame changed shape: clear what it leaves bare.
                        out.extend_from_slice(b"\x1b[2J");
                        redraw = true;
                    }
                    let id = match shown_id {
                        Some(id) if id == FRAME_IMAGE_IDS[0] => FRAME_IMAGE_IDS[1],
                        _ => FRAME_IMAGE_IDS[0],
                    };
                    encoder.encode_frame(&f.pixels, f.width, f.height, id, placement, &mut out);
                    // The new frame is placed before the old one goes, so the
                    // pane never shows neither.
                    if let Some(old) = shown_id.replace(id) {
                        graphics::delete_image(&mut out, old);
                    }
                    shown_placement = Some(placement);
                    *shared.shown.lock().unwrap_or_else(|e| e.into_inner()) = Shown {
                        placement: Some(placement),
                        width: f.width,
                        height: f.height,
                    };
                    sent_frame = true;
                }
            }
        }
        if let Some(text) = new_status {
            status = text;
            redraw = true;
        }
        if redraw || sent_frame {
            graphics::status_line(&mut out, g, &status);
        }
        if !out.is_empty() {
            match write_until(fd, &out, cancelled) {
                Ok(true) => {}
                Ok(false) => return,
                Err(_) => {
                    shared.stats.failed.store(true, Ordering::Relaxed);
                    return;
                }
            }
            shared
                .stats
                .bytes
                .fetch_add(out.len() as u64, Ordering::Relaxed);
        }
        if sent_frame {
            shared.stats.frames.fetch_add(1, Ordering::Relaxed);
            // A frame-rate cap: the next frame waits out the rest of this
            // one's interval (meanwhile newer frames replace it unsent). The
            // wait is on the mailbox, so stopping ends it at once.
            if let Some(interval) = min_interval {
                let until = started + interval;
                let mut mailbox = shared.mailbox.lock().unwrap_or_else(|e| e.into_inner());
                while !mailbox.closed {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    mailbox = shared
                        .wake
                        .wait_timeout(mailbox, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
        if closed {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// A pipe standing in for the terminal: (read end, write end).
    fn pipe() -> (File, File) {
        let mut fds = [0; 2];
        // SAFETY: pipe fills two fresh descriptors this test then owns.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }

    const PANE: Geometry = Geometry {
        cols: 80,
        rows: 25,
        cell_width: 10,
        cell_height: 20,
    };

    /// A frame that compresses badly, so it overfills a pipe's buffer.
    fn noisy_frame(width: u32, height: u32, seed: u32) -> Vec<u32> {
        let mut state = seed;
        (0..width * height)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                state
            })
            .collect()
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn write_until_gives_up_on_a_terminal_that_stopped_reading() {
        let (_read, write) = pipe();
        let fd = write.as_raw_fd();
        // SAFETY: fcntl on the pipe's own write end.
        unsafe {
            libc::fcntl(
                fd,
                libc::F_SETFL,
                libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
            );
        }
        let bytes = vec![0u8; 1 << 20];
        let started = Instant::now();
        let deadline = started + Duration::from_millis(100);
        let finished = write_until(fd, &bytes, || Instant::now() >= deadline).unwrap();
        assert!(!finished, "a megabyte cannot fit an undrained pipe");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn stopping_the_writer_does_not_wait_on_a_stalled_link() {
        // Nobody reads the pipe: the first frame fills it and the writer is
        // left mid-frame.
        let (_read, write) = pipe();
        let output = Output::start(write, PANE, None).unwrap();
        output.post_frame(&noisy_frame(640, 400, 7), 640, 400);
        // Give the writer time to fill the pipe; the frame never finishes
        // going out.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(output.stats().frames.load(Ordering::Relaxed), 0);
        let started = Instant::now();
        output.finish();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stop took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn stopping_the_writer_ends_a_frame_rate_wait() {
        let (mut read, write) = pipe();
        let drain = std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = read.read_to_end(&mut sink);
        });
        // One frame every ten seconds: the writer waits right after the
        // first one.
        let output = Output::start(write, PANE, Some(Duration::from_secs(10))).unwrap();
        output.post_frame(&noisy_frame(64, 40, 3), 64, 40);
        wait_for("the first frame", || {
            output.stats().frames.load(Ordering::Relaxed) == 1
        });
        let started = Instant::now();
        output.finish();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stop took {:?}",
            started.elapsed()
        );
        drain.join().unwrap();
    }
}
