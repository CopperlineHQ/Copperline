// SPDX-License-Identifier: GPL-3.0-or-later

//! The controlling terminal: raw mode, the protocol modes the session turns
//! on, the start-up capability probe, and the writer thread frames go out on.
//!
//! The session talks to `/dev/tty` directly, so it works whatever stdout and
//! stderr point at. While it owns the terminal, a stdout or stderr that *is*
//! the terminal is pointed at a log file instead -- the log would otherwise be
//! printed over the picture -- and put back when the session ends, by `Drop`
//! or by the panic hook, whichever comes first.
//!
//! Raw mode turns the terminal's own Ctrl+C and Ctrl+\ off, but the process
//! can still be signalled from outside (`kill`, `timeout`, a closing SSH
//! session). SIGTERM, SIGHUP, SIGQUIT and SIGINT are caught for the life of
//! the session and reported through [`Terminal::end_signal`], so the session loop
//! leaves the way F12 q does and the terminal is given back.

use super::graphics::{self, FrameEncoder, Geometry, Placement, FRAME_IMAGE_IDS};
use super::input::{Event, Parser};
use super::LogTarget;
use anyhow::{anyhow, bail, Context as _, Result};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
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

/// How long a write made while the session starts may wait for the terminal
/// to take it. These are a few dozen bytes; a terminal that has not taken
/// them by now has its output stopped or its link down.
const STARTUP_WRITE_BUDGET: Duration = Duration::from_secs(10);

/// The session was asked to end by this signal before its loop was
/// running. The caller leaves with the status the loop would have.
#[derive(Debug)]
pub struct EndedBySignal(pub i32);

impl std::fmt::Display for EndedBySignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ended by signal {}", self.0)
    }
}

impl std::error::Error for EndedBySignal {}

/// How long the terminal must stay quiet, after it was told to stop
/// reporting, for its input to count as drained, and the longest the
/// drain waits for that on a terminal that keeps talking.
const DRAIN_QUIET: Duration = Duration::from_millis(40);
const DRAIN_BUDGET: Duration = Duration::from_millis(300);

/// What it takes to give the terminal back: held by the session, and by the
/// panic hook through [`RESTORE`].
#[derive(Clone)]
struct Restore {
    /// The blocking handle the terminal mode was read from.
    fd: RawFd,
    /// The non-blocking output handle, for writes that must not hang.
    out_fd: RawFd,
    /// The non-blocking input handle, to discard what is still queued.
    in_fd: RawFd,
    termios: libc::termios,
    log_path: Option<PathBuf>,
    /// The protocol modes were turned on, so they are to be turned off. A
    /// session that fails before that must not send [`LEAVE`]: it would
    /// clear a screen the session never took over.
    entered: bool,
    /// Stops the writer thread, once one runs.
    cancel: Option<Arc<AtomicBool>>,
}

/// The process's own session, for the panic hook. The session's `Drop`
/// takes it back; whichever comes first does the restoring.
static RESTORE: Mutex<Option<Restore>> = Mutex::new(None);

fn restore_terminal(restore: &Restore) {
    // The writer first: nothing may follow the restore sequence onto a
    // screen that has been given back.
    if let Some(cancel) = &restore.cancel {
        cancel.store(true, Ordering::Relaxed);
    }
    if restore.entered {
        let deadline = Instant::now() + RESTORE_WRITE_BUDGET;
        let _ = write_until(restore.out_fd, LEAVE.as_bytes(), || {
            Instant::now() >= deadline
        });
        discard_input(restore.in_fd);
    }
    // TCSANOW, not TCSAFLUSH: the latter waits for output to drain first,
    // which a stalled link never does. The input was discarded above.
    // SAFETY: fd is the terminal this session opened and still owns, and
    // termios is the state tcgetattr read from it.
    unsafe {
        libc::tcsetattr(restore.fd, libc::TCSANOW, &restore.termios);
    }
}

/// Throw away the input the terminal has queued and is still sending. Mouse
/// and key reports made before the terminal acted on [`LEAVE`] are on their
/// way or already waiting; left alone, the shell would read them as typed
/// text once the session ends. Reads until the terminal has been quiet for
/// [`DRAIN_QUIET`], for at most [`DRAIN_BUDGET`].
fn discard_input(fd: RawFd) {
    let started = Instant::now();
    let mut quiet_since = started;
    let mut buf = [0u8; 1024];
    while started.elapsed() < DRAIN_BUDGET {
        // SAFETY: buf is a live, writable slice; fd is an open descriptor.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            quiet_since = Instant::now();
            continue;
        }
        if n == 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if !matches!(
            err.kind(),
            std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
        ) {
            break;
        }
        let quiet = quiet_since.elapsed();
        if quiet >= DRAIN_QUIET || wait_fd(fd, false, DRAIN_QUIET - quiet).is_err() {
            break;
        }
    }
    // Whatever arrived between the last read and now, in the line
    // discipline's own queue.
    // SAFETY: tcflush on an open descriptor; a failure (not a terminal)
    // leaves nothing to flush.
    unsafe {
        libc::tcflush(fd, libc::TCIFLUSH);
    }
}

/// The signals that end a session: what `kill`, `timeout`, a closing
/// terminal or SSH connection, and `kill -INT`/`-QUIT` send. (The keyboard's
/// own Ctrl+C and Ctrl+\ are keys for the machine in raw mode.)
const END_SIGNALS: [libc::c_int; 4] = [libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT, libc::SIGINT];

/// The first end signal received, or 0.
static END_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_end_signal(signal: libc::c_int) {
    // Only an atomic store: async-signal-safe.
    let _ = END_SIGNAL.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
}

fn end_signal() -> Option<i32> {
    match END_SIGNAL.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

/// The session's handlers for [`END_SIGNALS`], put back as they were when
/// dropped.
struct SignalHandlers {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

impl SignalHandlers {
    fn install() -> Self {
        END_SIGNAL.store(0, Ordering::SeqCst);
        let mut previous = Vec::new();
        for signal in END_SIGNALS {
            // SAFETY: sigaction is plain data; the handler only stores to an
            // atomic. SA_RESTART keeps other threads' system calls from
            // failing with EINTR; the session loop polls, so it needs no
            // interruption to notice.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction =
                    on_end_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, &action, &mut old) != 0 {
                    continue;
                }
                if old.sa_sigaction == libc::SIG_IGN {
                    // Ignored on purpose (nohup): stay ignored.
                    libc::sigaction(signal, &old, std::ptr::null_mut());
                    continue;
                }
                previous.push((signal, old));
            }
        }
        Self { previous }
    }
}

impl Drop for SignalHandlers {
    fn drop(&mut self) {
        for (signal, old) in self.previous.drain(..) {
            // SAFETY: old is the action sigaction reported for this signal.
            unsafe {
                libc::sigaction(signal, &old, std::ptr::null_mut());
            }
        }
    }
}

/// Create the session's log file. A named file is the user's own choice and
/// is replaced like any output file. The default one goes in a directory
/// other users may write to under a name they can predict, so it is only
/// ever created -- never an existing file, never through a symlink -- and
/// is readable by the owner alone.
fn create_log(target: &LogTarget) -> Result<(File, PathBuf)> {
    match target {
        LogTarget::Named(path) => {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("creating the log file {}", path.display()))?;
            Ok((file, path.clone()))
        }
        LogTarget::Private(dir) => {
            let pid = std::process::id();
            for attempt in 0..100u32 {
                let name = if attempt == 0 {
                    format!("copperline-terminal-{pid}.log")
                } else {
                    format!("copperline-terminal-{pid}-{attempt}.log")
                };
                let path = dir.join(name);
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                {
                    Ok(file) => return Ok((file, path)),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("creating the log file {}", path.display()))
                    }
                }
            }
            bail!(
                "no free log file name in {} (pass --terminal-log PATH)",
                dir.display()
            )
        }
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

/// A blocking write of all of `bytes`, for the tests' pseudo-terminal.
#[cfg(test)]
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
    /// Blocking handle: the terminal mode and the window size.
    tty: File,
    /// Non-blocking opens of the terminal for reading, and for the writer
    /// thread and the restore, which must never hang on a stalled link. Each
    /// is its own open file description, so O_NONBLOCK stays off `tty`.
    input: File,
    output: File,
    /// stdout/stderr descriptors pointed at the log, with their originals.
    redirected: Vec<(RawFd, OwnedFd)>,
    log_path: Option<PathBuf>,
    /// How to give the terminal back.
    restore: Restore,
    /// This is the process's session: its restore data is also in
    /// [`RESTORE`] for the panic hook, and it owns the process-wide pieces
    /// (signal handlers, the stdout/stderr redirection).
    process_session: bool,
    signals: Option<SignalHandlers>,
    restored: bool,
}

impl Terminal {
    /// Open the controlling terminal, put it in raw mode, point a stdout or
    /// stderr that is the terminal at the log, and turn on the protocol
    /// modes. Fails without changing anything when there is no terminal.
    pub fn open(log: &LogTarget) -> Result<Self> {
        Self::open_device(Path::new("/dev/tty"), Some(log))
    }

    /// Open the terminal at `device`. With `process_log` this is the
    /// process's own session: the log redirection, the panic hook and the
    /// signal handlers are set up for it. Without, only the device itself
    /// is touched, so tests can run sessions on pseudo-terminals side by
    /// side.
    fn open_device(device: &Path, process_log: Option<&LogTarget>) -> Result<Self> {
        let open = |options: &mut OpenOptions, what: &str| {
            options
                .open(device)
                .with_context(|| format!("opening {} {what}", device.display()))
        };
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
            .open(device)
            .with_context(|| {
                format!("--terminal needs a terminal (opening {})", device.display())
            })?;
        let input = open(
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK),
            "for input",
        )?;
        let output = open(
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK),
            "for output",
        )?;
        let fd = tty.as_raw_fd();
        let out_fd = output.as_raw_fd();
        let in_fd = input.as_raw_fd();
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
            restore: Restore {
                fd,
                out_fd,
                in_fd,
                termios,
                log_path: None,
                entered: false,
                cancel: None,
            },
            process_session: process_log.is_some(),
            signals: None,
            restored: false,
        };
        // From here on a failure drops `terminal`, which puts the terminal
        // mode back; the screen is left alone until the modes go on.
        if let Some(log) = process_log {
            terminal.publish_restore();
            install_panic_hook();
            terminal.signals = Some(SignalHandlers::install());
            terminal.redirect_output(log)?;
        }
        terminal.restore.entered = true;
        terminal.publish_restore();
        terminal.write(ENTER.as_bytes())?;
        terminal.write(format!("\x1b[>{KEYBOARD_FLAGS}u").as_bytes())?;
        Ok(terminal)
    }

    /// Give the panic hook the current restore data, if this is the
    /// process's session.
    fn publish_restore(&self) {
        if self.process_session {
            *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.restore.clone());
        }
    }

    fn redirect_output(&mut self, target: &LogTarget) -> Result<()> {
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
        let (log, log_path) = create_log(target)?;
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
        self.restore.log_path = Some(log_path.clone());
        self.log_path = Some(log_path);
        Ok(())
    }

    /// The signal asking this session to end, once one has arrived. The
    /// session loop polls this every pass and leaves as for a quit request.
    pub fn end_signal(&self) -> Option<i32> {
        self.signals.as_ref().and_then(|_| end_signal())
    }

    /// Where the log went, if it was redirected.
    pub fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    /// Write `bytes` to the terminal while the session starts, before the
    /// writer thread runs. Like every write to the terminal this one must
    /// not hang: on a terminal with its output stopped or a link that has
    /// stopped draining, a blocking write would never return, and a signal
    /// asking the session to end would only set a flag nobody gets to read.
    /// So it goes through the non-blocking handle and gives up when an end
    /// signal arrives or [`STARTUP_WRITE_BUDGET`] runs out.
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let deadline = Instant::now() + STARTUP_WRITE_BUDGET;
        self.write_or_give_up(bytes, || Instant::now() >= deadline)
    }

    fn write_or_give_up(&self, bytes: &[u8], give_up: impl Fn() -> bool) -> Result<()> {
        let written = write_until(self.output.as_raw_fd(), bytes, || {
            self.end_signal().is_some() || give_up()
        })
        .context("writing to the terminal")?;
        match (written, self.end_signal()) {
            (true, _) => Ok(()),
            (false, Some(signal)) => Err(EndedBySignal(signal).into()),
            (false, None) => bail!("the terminal is not taking output"),
        }
    }

    /// Start the writer thread on the terminal's non-blocking output (see
    /// [`Output::start`]). Giving the terminal back, from `Drop` or the
    /// panic hook, stops it first.
    pub fn start_output(
        &mut self,
        geometry: Geometry,
        min_interval: Option<Duration>,
    ) -> Result<Output> {
        let handle = self
            .output
            .try_clone()
            .context("duplicating the terminal handle")?;
        let output = Output::start(handle, geometry, min_interval)?;
        self.restore.cancel = Some(Arc::clone(&output.shared.cancel));
        self.publish_restore();
        Ok(output)
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
            if let Some(signal) = self.end_signal() {
                return Err(EndedBySignal(signal).into());
            }
            // Short waits, so an end signal is noticed without relying on
            // it interrupting the wait.
            let n = self.read(&mut buf, left.min(Duration::from_millis(50)))?;
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
        // The process's session shares the job with the panic hook: when
        // the hook got there first there is nothing left in RESTORE and
        // nothing left to do to the terminal.
        let pending = if self.process_session {
            RESTORE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .is_some()
        } else {
            true
        };
        if pending {
            restore_terminal(&self.restore);
        }
        self.signals = None;
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
            give_back_after_panic(&restore);
        }
        previous(info);
    }));
}

/// The panic hook's work: stop the writer, give the terminal back, and say
/// where the details went. The session loop sees the stopped writer and
/// ends (a panic on another thread does not unwind the loop's own).
fn give_back_after_panic(restore: &Restore) {
    restore_terminal(restore);
    let note = match &restore.log_path {
        Some(path) => format!("copperline: panicked; details in {}\r\n", path.display()),
        None => "copperline: panicked\r\n".to_string(),
    };
    let deadline = Instant::now() + RESTORE_WRITE_BUDGET;
    let _ = write_until(restore.out_fd, note.as_bytes(), || {
        Instant::now() >= deadline
    });
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
    /// frame-rate wait instead of finishing them. Shared with the terminal's
    /// restore data, so giving the terminal back stops the writer too.
    cancel: Arc<AtomicBool>,
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
            cancel: Arc::new(AtomicBool::new(false)),
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

    /// Whether the writer is no longer writing: it was told to stop (by the
    /// panic hook giving the terminal back, for one), its thread ended on a
    /// failed write, or it panicked. Frames posted now go nowhere, so the
    /// session should end.
    pub fn stopped(&self) -> bool {
        self.shared.cancel.load(Ordering::Relaxed)
            || self
                .thread
                .as_ref()
                .is_none_or(|thread| thread.is_finished())
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
            if cancelled() {
                return;
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

/// A pseudo-terminal standing in for the user's terminal, so whole sessions
/// can be tested without one.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// The master side is the "terminal emulator": what the session writes
    /// is read from it, and what is written to it is the session's input.
    pub(crate) struct Pty {
        pub master: File,
        /// Kept open so the device outlives any one session on it.
        _slave: File,
        pub path: PathBuf,
    }

    impl Pty {
        /// A pseudo-terminal of 160x50 cells, 10x20 pixels each (wide enough
        /// that the status line is never cut short), left in a raw, no-echo
        /// mode: the mode a session finds and must put back.
        pub(crate) fn open() -> Self {
            let (mut master, mut slave) = (0, 0);
            let mut name = [0 as libc::c_char; 1024];
            let mut size = libc::winsize {
                ws_row: 50,
                ws_col: 160,
                ws_xpixel: 1600,
                ws_ypixel: 1000,
            };
            // The window-size parameter is a const pointer on Linux and a
            // mutable one on macOS; a raw mutable pointer suits both.
            let size: *mut libc::winsize = &mut size;
            // SAFETY: openpty fills two fresh descriptors and a path of at
            // most PATH_MAX bytes into buffers this function owns, and
            // reads the window size from a live value.
            let rc = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    name.as_mut_ptr(),
                    std::ptr::null_mut(),
                    size,
                )
            };
            assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
            // SAFETY: both descriptors are open and owned from here on; the
            // name is NUL-terminated.
            let (master, slave, path) = unsafe {
                (
                    File::from_raw_fd(master),
                    File::from_raw_fd(slave),
                    std::ffi::CStr::from_ptr(name.as_ptr())
                        .to_string_lossy()
                        .into_owned(),
                )
            };
            // SAFETY: termios calls on the open slave.
            unsafe {
                let mut mode: libc::termios = std::mem::zeroed();
                assert_eq!(libc::tcgetattr(slave.as_raw_fd(), &mut mode), 0);
                libc::cfmakeraw(&mut mode);
                assert_eq!(libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &mode), 0);
                let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
                libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            Self {
                master,
                _slave: slave,
                path: path.into(),
            }
        }

        /// A session on this pseudo-terminal that touches nothing else in
        /// the process (no log redirection, panic hook or signal handlers).
        pub(crate) fn session(&self) -> Result<Terminal> {
            Terminal::open_device(&self.path, None)
        }

        /// Send `bytes` to the session, as the terminal's input.
        pub(crate) fn send(&self, bytes: &[u8]) {
            write_fd(self.master.as_raw_fd(), bytes).expect("writing to the pty");
        }

        /// Read what the session has written since the last call, waiting
        /// up to `wait` for something when there is nothing yet.
        pub(crate) fn take_output(&self, wait: Duration) -> Vec<u8> {
            let fd = self.master.as_raw_fd();
            let mut out = Vec::new();
            let mut buf = [0u8; 8192];
            let deadline = Instant::now() + wait;
            loop {
                // SAFETY: buf is a live, writable slice; fd is open.
                let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n > 0 {
                    out.extend_from_slice(&buf[..n as usize]);
                    continue;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if !out.is_empty() || left.is_zero() || n == 0 {
                    return out;
                }
                let _ = wait_fd(fd, false, left.min(Duration::from_millis(20)));
            }
        }

        /// Read the session's output until it contains `needle`, returning
        /// everything read. Panics after ten seconds without it.
        pub(crate) fn output_until(&self, needle: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !contains(&out, needle) {
                assert!(
                    Instant::now() < deadline,
                    "the session never wrote {:?}; it wrote {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&out)
                );
                out.extend(self.take_output(Duration::from_millis(50)));
            }
            out
        }

        /// The terminal mode the device is in now.
        pub(crate) fn mode(&self) -> libc::termios {
            // SAFETY: tcgetattr fills plain data from the open slave.
            unsafe {
                let mut mode: libc::termios = std::mem::zeroed();
                assert_eq!(libc::tcgetattr(self._slave.as_raw_fd(), &mut mode), 0);
                mode
            }
        }

        /// Input the session left unread, as the shell would find it.
        pub(crate) fn unread_input(&self) -> Vec<u8> {
            let slave = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
                .open(&self.path)
                .expect("reopening the pty");
            let mut buf = [0u8; 4096];
            // SAFETY: buf is a live, writable slice; the descriptor is open.
            let n = unsafe { libc::read(slave.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            buf[..n.max(0) as usize].to_vec()
        }
    }

    pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    pub(crate) const ENTER_BYTES: &[u8] = ENTER.as_bytes();
    pub(crate) const LEAVE_BYTES: &[u8] = LEAVE.as_bytes();
}

#[cfg(test)]
mod tests {
    use super::testing::{contains, Pty};
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
    fn nonblocking(file: &File) {
        let fd = file.as_raw_fd();
        // SAFETY: fcntl on a descriptor the caller owns.
        unsafe {
            libc::fcntl(
                fd,
                libc::F_SETFL,
                libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
            );
        }
    }

    /// Restore data over pipes: what the session writes to give the
    /// terminal back comes out of the first, and the second holds input
    /// the terminal sent that nobody read.
    fn piped_restore(entered: bool) -> (Restore, File, File, File, File) {
        let (out_read, out_write) = pipe();
        let (in_read, in_write) = pipe();
        for file in [&out_read, &out_write, &in_read] {
            nonblocking(file);
        }
        let restore = Restore {
            // Not a terminal: setting its mode fails and changes nothing.
            fd: out_write.as_raw_fd(),
            out_fd: out_write.as_raw_fd(),
            in_fd: in_read.as_raw_fd(),
            // SAFETY: termios is plain data.
            termios: unsafe { std::mem::zeroed() },
            log_path: None,
            entered,
            cancel: None,
        };
        (restore, out_read, out_write, in_read, in_write)
    }

    fn read_available(file: &mut File) -> Vec<u8> {
        let mut bytes = Vec::new();
        let _ = file.read_to_end(&mut bytes);
        bytes
    }

    #[test]
    fn a_session_that_never_entered_leaves_the_screen_alone() {
        let (restore, mut out_read, _out_write, _in_read, _in_write) = piped_restore(false);
        restore_terminal(&restore);
        assert_eq!(read_available(&mut out_read), b"");

        let (restore, mut out_read, _out_write, _in_read, _in_write) = piped_restore(true);
        restore_terminal(&restore);
        assert_eq!(read_available(&mut out_read), LEAVE.as_bytes());
    }

    /// Reports the terminal sent before it stopped reporting are thrown
    /// away with the session, not left for the shell to read as typing.
    #[test]
    fn restoring_discards_input_the_session_never_read() {
        let (restore, _out_read, _out_write, mut in_read, mut in_write) = piped_restore(true);
        in_write.write_all(b"\x1b[<35;120;80M\x1b[97;1:3u").unwrap();
        // More arrives while the restore is already under way.
        let late = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(15));
            in_write.write_all(b"\x1b[<35;121;80M").unwrap();
            in_write
        });
        let started = Instant::now();
        restore_terminal(&restore);
        let _in_write = late.join().unwrap();
        assert_eq!(read_available(&mut in_read), b"");
        assert!(started.elapsed() >= DRAIN_QUIET);
        assert!(started.elapsed() < DRAIN_BUDGET + Duration::from_secs(1));
    }

    /// A terminal that never stops sending cannot hold the restore up.
    #[test]
    fn draining_a_terminal_that_keeps_talking_is_bounded() {
        let (restore, _out_read, _out_write, _in_read, mut in_write) = piped_restore(true);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_writer = Arc::clone(&stop);
        let chatter = std::thread::spawn(move || {
            while !stop_writer.load(Ordering::Relaxed) {
                if in_write.write_all(b"\x1b[<35;1;1M").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let started = Instant::now();
        restore_terminal(&restore);
        let took = started.elapsed();
        stop.store(true, Ordering::Relaxed);
        chatter.join().unwrap();
        // No lower bound beyond the quiet period: on a loaded machine the
        // chatter itself can pause for longer than that.
        assert!(took >= DRAIN_QUIET, "gave up after {took:?}");
        assert!(
            took < DRAIN_BUDGET + Duration::from_secs(1),
            "took {took:?}"
        );
    }

    /// After a panic the terminal is given back at once, so the writer must
    /// stop: a frame it finished afterwards would land on the user's shell.
    #[test]
    fn giving_the_terminal_back_after_a_panic_stops_the_writer() {
        let (frames_read, frames_write) = pipe();
        let drain = std::thread::spawn(move || {
            let mut frames_read = frames_read;
            let mut sink = Vec::new();
            let _ = frames_read.read_to_end(&mut sink);
        });
        let output = Output::start(frames_write, PANE, None).unwrap();
        output.post_frame(&noisy_frame(64, 40, 3), 64, 40);
        wait_for("the first frame", || {
            output.stats().frames.load(Ordering::Relaxed) == 1
        });
        assert!(!output.stopped());

        let (mut restore, mut out_read, _out_write, _in_read, _in_write) = piped_restore(true);
        restore.cancel = Some(Arc::clone(&output.shared.cancel));
        restore.log_path = Some(PathBuf::from("/tmp/session.log"));
        give_back_after_panic(&restore);
        assert!(output.stopped());
        let written = read_available(&mut out_read);
        assert!(written.starts_with(LEAVE.as_bytes()));
        assert!(contains(&written, b"panicked; details in /tmp/session.log"));

        // A frame posted now is not written, and the thread ends.
        output.post_frame(&noisy_frame(64, 40, 4), 64, 40);
        wait_for("the writer to end", || {
            output.thread.as_ref().unwrap().is_finished()
        });
        assert_eq!(output.stats().frames.load(Ordering::Relaxed), 1);
        drop(output);
        drain.join().unwrap();
    }

    /// The end-signal handlers and the flag they set are process-wide, so
    /// the tests that install them run one at a time.
    fn signal_tests() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn handler_of(signal: libc::c_int) -> libc::sighandler_t {
        // SAFETY: a null new action only queries the current one.
        unsafe {
            let mut current: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut current);
            current.sa_sigaction
        }
    }

    /// kill, timeout and a closing SSH session reach the process as
    /// signals; the session notes them and puts the old handling back when
    /// it ends.
    ///
    /// A signal the process was started ignoring (SIGHUP under nohup, SIGINT
    /// in a background shell job) stays ignored, so what the test expects of
    /// each signal depends on how the test run itself was started.
    #[test]
    fn end_signals_are_noted_and_the_old_handlers_come_back() {
        let _serial = signal_tests();
        let ours = on_end_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        let before: Vec<_> = END_SIGNALS.iter().map(|&s| handler_of(s)).collect();
        let handlers = SignalHandlers::install();
        assert_eq!(end_signal(), None);
        let mut caught = Vec::new();
        for (&signal, &was) in END_SIGNALS.iter().zip(&before) {
            if was == libc::SIG_IGN {
                assert_eq!(handler_of(signal), libc::SIG_IGN, "signal {signal}");
            } else {
                assert_eq!(handler_of(signal), ours, "signal {signal}");
                caught.push(signal);
            }
        }
        // An ignored signal is not noted: it never reaches the handler.
        for (&signal, &was) in END_SIGNALS.iter().zip(&before) {
            if was == libc::SIG_IGN {
                // SAFETY: raising a signal this process ignores does nothing.
                unsafe {
                    libc::raise(signal);
                }
                assert_eq!(end_signal(), None, "signal {signal}");
            }
        }
        // The first caught signal is the one reported, whatever follows.
        for &signal in &caught {
            // SAFETY: the handler for this signal is ours and only stores a
            // flag.
            unsafe {
                libc::raise(signal);
            }
            assert_eq!(end_signal(), Some(caught[0]));
        }
        drop(handlers);
        let after: Vec<_> = END_SIGNALS.iter().map(|&s| handler_of(s)).collect();
        assert_eq!(after, before);
    }

    /// The first caught end signal of this test run, with its handler
    /// installed on `terminal`. `None` when the run ignores all of them.
    fn catch_end_signals(terminal: &mut Terminal) -> Option<libc::c_int> {
        let ignored: Vec<_> = END_SIGNALS
            .iter()
            .map(|&s| handler_of(s) == libc::SIG_IGN)
            .collect();
        terminal.signals = Some(SignalHandlers::install());
        END_SIGNALS
            .iter()
            .zip(ignored)
            .find_map(|(&signal, ignored)| (!ignored).then_some(signal))
    }

    /// Stop or resume the pseudo-terminal's output, as Ctrl+S and Ctrl+Q
    /// (or a link that stops draining) do to a real one.
    fn suspend_output(terminal: &Terminal, suspend: bool) {
        let action = if suspend { libc::TCOOFF } else { libc::TCOON };
        // SAFETY: tcflow on the session's own open terminal.
        assert_eq!(unsafe { libc::tcflow(terminal.tty.as_raw_fd(), action) }, 0);
    }

    /// While the session starts, its writes go out before the loop that
    /// polls for end signals runs. On a terminal that has stopped taking
    /// output such a write must still end when the session is signalled,
    /// or kill and a closing SSH session would leave it stuck with the
    /// terminal taken until SIGKILL.
    #[test]
    fn a_startup_write_to_a_stopped_terminal_ends_on_a_signal() {
        let _serial = signal_tests();
        let pty = Pty::open();
        let mut terminal = pty.session().unwrap();
        let _ = pty.output_until(format!("\x1b[>{KEYBOARD_FLAGS}u").as_bytes());
        let Some(signal) = catch_end_signals(&mut terminal) else {
            return; // every end signal is ignored in this run
        };
        suspend_output(&terminal, true);
        let killer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            // SAFETY: the handler for this signal only stores a flag.
            unsafe {
                libc::kill(libc::getpid(), signal);
            }
        });
        let started = Instant::now();
        let error = terminal.write(&vec![b' '; 1 << 20]).err().unwrap();
        killer.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let ended = error
            .downcast_ref::<EndedBySignal>()
            .unwrap_or_else(|| panic!("not a signal: {error:#}"));
        assert_eq!(ended.0, signal);
        // The probe ends the same way instead of waiting for answers.
        let error = terminal.probe().err().unwrap();
        assert!(error.downcast_ref::<EndedBySignal>().is_some(), "{error:#}");
        suspend_output(&terminal, false);
    }

    /// With no signal, a startup write to a terminal that takes nothing
    /// fails once its time is up rather than hanging.
    #[test]
    fn a_startup_write_to_a_stopped_terminal_runs_out_of_time() {
        let pty = Pty::open();
        let terminal = pty.session().unwrap();
        let _ = pty.output_until(format!("\x1b[>{KEYBOARD_FLAGS}u").as_bytes());
        suspend_output(&terminal, true);
        let deadline = Instant::now() + Duration::from_millis(100);
        let started = Instant::now();
        let error = terminal
            .write_or_give_up(&vec![b' '; 1 << 20], || Instant::now() >= deadline)
            .err()
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(
            format!("{error:#}").contains("not taking output"),
            "{error:#}"
        );
        suspend_output(&terminal, false);
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "copperline-terminal-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The default log lives in the shared temporary directory under a
    /// name anyone can predict: it is created new and private, and a file
    /// or symlink already at that name is left exactly as it was.
    #[test]
    fn the_default_log_is_created_new_and_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch_dir("log");
        let pid = std::process::id();
        let first = dir.join(format!("copperline-terminal-{pid}.log"));
        let second = dir.join(format!("copperline-terminal-{pid}-1.log"));
        let victim = dir.join("victim");
        std::fs::write(&victim, b"keep me").unwrap();
        std::fs::write(&first, b"someone else's").unwrap();
        std::os::unix::fs::symlink(&victim, &second).unwrap();

        let (mut log, path) = create_log(&LogTarget::Private(dir.clone())).unwrap();
        log.write_all(b"session log").unwrap();
        assert_eq!(path, dir.join(format!("copperline-terminal-{pid}-2.log")));
        assert_eq!(std::fs::read(&path).unwrap(), b"session log");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read(&first).unwrap(), b"someone else's");
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");

        // A log the user named is theirs to replace.
        let named = dir.join("named.log");
        std::fs::write(&named, b"an older, longer log").unwrap();
        let (mut log, path) = create_log(&LogTarget::Named(named.clone())).unwrap();
        log.write_all(b"new").unwrap();
        assert_eq!(path, named);
        assert_eq!(std::fs::read(&named).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A whole session on a pseudo-terminal: the modes go on, raw mode is
    /// set, and ending it turns the modes off, puts the terminal mode back
    /// and leaves no unread reports behind.
    #[test]
    fn a_session_takes_the_terminal_and_gives_it_back() {
        let pty = Pty::open();
        let before = pty.mode();
        let terminal = pty.session().unwrap();
        let entered = pty.output_until(format!("\x1b[>{KEYBOARD_FLAGS}u").as_bytes());
        assert!(entered.starts_with(ENTER.as_bytes()));
        assert_eq!(pty.mode().c_lflag & (libc::ICANON | libc::ISIG), 0);

        // Reports the session has not read by the time it ends.
        pty.send(b"\x1b[<35;120;80M\x1b[97;1:3u");
        drop(terminal);
        let left = pty.output_until(LEAVE.as_bytes());
        assert!(left.ends_with(LEAVE.as_bytes()));
        assert_eq!(pty.unread_input(), b"");
        let after = pty.mode();
        assert_eq!(after.c_lflag, before.c_lflag);
        assert_eq!(after.c_iflag, before.c_iflag);
        assert_eq!(after.c_oflag, before.c_oflag);
    }

    #[test]
    fn opening_a_device_that_is_not_there_changes_nothing() {
        let missing = Path::new("/nonexistent/copperline-no-such-tty");
        let error = Terminal::open_device(missing, None).err().unwrap();
        assert!(format!("{error:#}").contains("/nonexistent/copperline-no-such-tty"));
    }
}
