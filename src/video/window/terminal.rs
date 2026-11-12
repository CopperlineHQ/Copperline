// SPDX-License-Identifier: GPL-3.0-or-later

//! `--terminal`: run the session inside a Kitty-protocol terminal instead of
//! a window (see [`crate::terminal`]).
//!
//! This is the windowless capture loop's interactive sibling: the same App,
//! with every scheduled-input and capture flag, warp gate, `--run` return and
//! guest exit request serviced as in the window, but the picture goes to the
//! terminal and the keys and mouse come from it. Terminal keys are turned
//! into the window's own host key codes, so they reach the machine through
//! the same Amiga keymap and keyboard-joystick mappings a window would use.
//!
//! F12, which no Amiga keyboard has, is the host key: F12 then a letter
//! quits, pauses, warps, resets or saves a screenshot.

use super::*;
use crate::terminal::graphics::Geometry;
use crate::terminal::input::{
    Event as TermEvent, KeyAction, KeyReport, KeypadKey, ModifierKey, Mods,
    MouseButton as TermMouseButton, MouseReport, Parser, Side, TermKey,
};
use crate::terminal::tty::{EndedBySignal, Output, Terminal, KEYBOARD_FLAGS};
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

/// Emulated frames a key from a terminal that reports no releases stays
/// down: long enough for any keyboard handler to see it, short enough not to
/// start the guest's key repeat.
const TAP_FRAMES: u64 = 3;

/// How often the status line's rates are recomputed.
const STATUS_INTERVAL: Duration = Duration::from_millis(500);

/// Cell size assumed when the terminal reports none.
const FALLBACK_CELL: (u16, u16) = (8, 16);

/// How long a lone ESC waits for the rest of a sequence before it is the
/// Escape key, and how long `ESC [` / `ESC O` wait on a legacy keyboard
/// before they are Alt+[ / Alt+O. The two-byte forms wait longer: taking a
/// real sequence's start for a key would type the rest of it into the
/// machine.
const ESCAPE_WAIT: Duration = Duration::from_millis(15);
const LEGACY_PREFIX_WAIT: Duration = Duration::from_millis(50);

/// How long the terminal may stay silent inside a bracketed paste before
/// the paste is taken as finished without its end marker. A paste arrives
/// in one burst; without this a lost marker would leave every later key,
/// F12 included, as paste text.
const PASTE_IDLE: Duration = Duration::from_secs(2);

/// Emulated time between a paste arriving and its first key. With the Kitty
/// keyboard protocol the terminal reports the paste shortcut's own
/// modifiers as held keys; typing at once would turn the first characters
/// into chords with them. The window's Paste as Keystrokes waits the same.
const PASTE_START_DELAY_MS: u32 = 300;

/// Terminal-side state of a `--terminal` session.
struct TermSession {
    // Declared before `terminal`: fields drop in order, and the writer must
    // stop before the terminal is given back.
    output: Option<Output>,
    terminal: Terminal,
    parser: Parser,
    geometry: Geometry,
    probed_cell: Option<(u16, u16)>,
    keys: HostKeys,
    /// When the terminal last sent anything.
    last_input: Instant,
    /// Keys pressed as taps, with the emulated frame they are let go at.
    taps: Vec<(KeyCode, u64)>,
    /// The pointer's last position, in frame pixels, while over the picture.
    pointer: Option<(f64, f64)>,
    notice: Option<(String, Instant)>,
    rates_at: Instant,
    rates_frames: u64,
    rates_bytes: u64,
    fps: f64,
    kib_per_sec: f64,
    status: String,
}

impl TermSession {
    fn output(&self) -> &Output {
        self.output
            .as_ref()
            .expect("terminal writer runs for the session")
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now() + Duration::from_secs(4)));
    }
}

/// The outcome of a session that failed while starting: a signal asking it
/// to end is the same clean exit the loop makes for one, with the status a
/// shell reports for a process the signal killed; anything else is the
/// error.
fn ended_by_signal(error: anyhow::Error) -> Result<i32> {
    match error
        .chain()
        .find_map(|cause| cause.downcast_ref::<EndedBySignal>())
    {
        Some(ended) => {
            info!("terminal: ending on signal {} during start-up", ended.0);
            Ok(128 + ended.0)
        }
        None => Err(error),
    }
}

fn ambiguity_wait(lone_escape: bool) -> Duration {
    if lone_escape {
        ESCAPE_WAIT
    } else {
        LEGACY_PREFIX_WAIT
    }
}

fn read_geometry(terminal: &Terminal, probed_cell: Option<(u16, u16)>) -> Option<Geometry> {
    let (cols, rows, xpixel, ypixel) = terminal.window_size()?;
    let (cell_width, cell_height) = if xpixel >= cols && ypixel >= rows {
        (xpixel / cols, ypixel / rows)
    } else {
        probed_cell.unwrap_or(FALLBACK_CELL)
    };
    Some(Geometry {
        cols,
        rows,
        cell_width: cell_width.max(1),
        cell_height: cell_height.max(1),
    })
}

impl App {
    /// Drive the session in the controlling terminal until it ends; the value
    /// is the process exit status it concluded on (src/verdict.rs).
    pub fn run_terminal(self, options: crate::terminal::Options) -> Result<i32> {
        let terminal = match Terminal::open(&options.log_target()) {
            Ok(terminal) => terminal,
            Err(error) => return ended_by_signal(error),
        };
        self.run_terminal_on(terminal, &options)
    }

    /// The session on an open terminal: probe it, run the loop, and give
    /// the terminal back before returning.
    fn run_terminal_on(
        mut self,
        mut terminal: Terminal,
        options: &crate::terminal::Options,
    ) -> Result<i32> {
        let caps = match terminal.probe() {
            Ok(caps) => caps,
            Err(error) => {
                // Give the terminal back before the status is reported.
                drop(terminal);
                return ended_by_signal(error);
            }
        };
        if !caps.graphics {
            drop(terminal);
            let tmux = if std::env::var_os("TMUX").is_some() {
                " (tmux does not pass Kitty graphics through; run outside it)"
            } else {
                ""
            };
            return Err(anyhow!(
                "--terminal: this terminal did not answer the Kitty graphics protocol query; \
                 use kitty, Ghostty, WezTerm or another terminal with Kitty graphics{tmux}"
            ));
        }
        let key_releases = caps.keyboard_flags.is_some_and(|flags| flags & 2 != 0);
        info!(
            "terminal: Kitty graphics ok; keyboard flags {:?} (asked for {KEYBOARD_FLAGS}); cell {:?}",
            caps.keyboard_flags, caps.cell_pixels
        );
        let geometry = read_geometry(&terminal, caps.cell_pixels)
            .ok_or_else(|| anyhow!("--terminal: the terminal reports no window size"))?;
        let output = terminal.start_output(geometry, options.min_frame_interval())?;
        let mut parser = Parser::new();
        // Without the protocol's disambiguation (flag 1) a key can send the
        // first bytes of a control sequence.
        parser.set_legacy_keyboard(caps.keyboard_flags.is_none_or(|flags| flags & 1 == 0));
        let now = Instant::now();
        let mut session = TermSession {
            output: Some(output),
            terminal,
            parser,
            geometry,
            probed_cell: caps.cell_pixels,
            keys: HostKeys::new(key_releases),
            last_input: now,
            taps: Vec::new(),
            pointer: None,
            notice: None,
            rates_at: now,
            rates_frames: 0,
            rates_bytes: 0,
            fps: 0.0,
            kib_per_sec: 0.0,
            status: String::new(),
        };
        if !key_releases {
            session.notify("terminal reports no key releases: keys are taps, no joystick");
            warn!("terminal: no Kitty keyboard event types; every key is a tap");
        }
        self.arm_scheduled_events();
        let result = self.terminal_loop(&mut session);
        if let Some(output) = session.output.take() {
            output.finish();
        }
        let log = session.terminal.log_path().map(Path::to_path_buf);
        // Give the terminal back before anything is printed to it.
        drop(session);
        if let Some(log) = log {
            let _ = writeln!(
                std::io::stderr(),
                "copperline: log written to {}",
                log.display()
            );
        }
        result
    }

    fn terminal_loop(&mut self, s: &mut TermSession) -> Result<i32> {
        let mut buf = vec![0u8; 16 * 1024];
        let mut events = Vec::new();
        loop {
            // Input first, so it lands before this pass's frame. A running
            // machine paces the loop itself (each step sleeps to real time);
            // a stopped one waits on the terminal instead of spinning.
            let mut wait = if self.machine_advances() {
                Duration::ZERO
            } else {
                Duration::from_millis(20)
            };
            loop {
                let n = s.terminal.read(&mut buf, wait)?;
                if n == 0 {
                    break;
                }
                s.parser.feed(&buf[..n], &mut events);
                s.last_input = Instant::now();
                wait = Duration::ZERO;
            }
            if s.parser.ambiguous_prefix_pending() {
                // A lone ESC is the Escape key unless more of its sequence
                // is just behind it; the same for the two bytes a legacy
                // Alt+[ or Alt+O sends. (Any longer unfinished sequence
                // simply waits for its remaining bytes.)
                // One pass waits no longer than the Escape key does, so a
                // legacy prefix's longer wait is spread over passes and
                // never holds the machine up for more than that.
                let due = s.last_input + ambiguity_wait(s.parser.lone_escape_pending());
                let left = due.saturating_duration_since(Instant::now());
                let n = s.terminal.read(&mut buf, left.min(ESCAPE_WAIT))?;
                if n > 0 {
                    s.parser.feed(&buf[..n], &mut events);
                    s.last_input = Instant::now();
                } else if Instant::now() >= due {
                    s.parser.flush_ambiguous_prefix(&mut events);
                }
            }
            if s.parser.paste_open() && s.last_input.elapsed() >= PASTE_IDLE {
                s.parser.end_stale_paste(&mut events);
            }
            for event in events.drain(..) {
                self.terminal_event(s, event);
            }
            if self.quit_requested {
                return Ok(self.exit_status());
            }
            if let Some(signal) = s.terminal.end_signal() {
                // Asked to stop from outside: leave as for a quit, with the
                // status a shell reports for a process the signal killed.
                info!("terminal: ending on signal {signal}");
                return Ok(128 + signal);
            }
            if s.output().stopped() {
                let failed = s
                    .output()
                    .stats()
                    .failed
                    .load(std::sync::atomic::Ordering::Relaxed);
                return Err(anyhow!(if failed {
                    "--terminal: the terminal stopped taking output"
                } else {
                    "--terminal: the terminal writer stopped"
                }));
            }
            self.terminal_track_resize(s);
            if self.netplay.is_none() {
                self.service_uaelib();
            }
            if self.guest_exit_requested || self.note_guest_exit_request() || self.poll_run_return()
            {
                return Ok(self.exit_status());
            }
            if self.machine_advances() {
                self.recover_audio_if_device_lost();
                self.pump_joystick_input();
                self.deliver_host_mouse_motion();
                self.update_warp_input_yield();
                // While a capture is scheduled, one frame per pass, as the
                // window does: scheduled input and captures are serviced
                // between passes, so a warp burst would move them in
                // emulated time and skip the frames they capture.
                let (frames, budget) = self.warp_burst_plan(self.headless_capture_active());
                let started = Instant::now();
                for _ in 0..frames {
                    if let Err(e) = self.emu.step_frame() {
                        error!("emulator step halted: {e:?}");
                        self.cpu_halted = true;
                        self.sync_live_audio_suspension();
                        s.notify(format!("CPU halted: {e}"));
                        break;
                    }
                    if self.poll_warp_launch() || self.poll_warp_boot() || self.service_uaelib() {
                        break;
                    }
                    if budget.is_some_and(|b| started.elapsed() >= b) {
                        break;
                    }
                }
                self.terminal_release_due_taps(s);
            }
            if self.powered_on && self.render_emulated_frame_if_needed() {
                let image = self.capture_present_image();
                s.output()
                    .post_frame(&image.pixels, image.width, image.height);
            }
            if self.dump_frame_if_due() || self.fire_gif_captures() {
                return Ok(self.headless_exit_status());
            }
            self.fire_scheduled_events();
            self.fire_auto_save_state();
            if self.fire_auto_shot() {
                return Ok(self.headless_exit_status());
            }
            self.terminal_update_status(s);
        }
    }

    fn terminal_track_resize(&mut self, s: &mut TermSession) {
        let Some(geometry) = read_geometry(&s.terminal, s.probed_cell) else {
            return;
        };
        if geometry != s.geometry {
            s.geometry = geometry;
            s.pointer = None;
            s.output().post_geometry(geometry);
        }
    }

    fn terminal_event(&mut self, s: &mut TermSession, event: TermEvent) {
        match event {
            TermEvent::Key(report) => self.terminal_key(s, report),
            TermEvent::Mouse(report) => self.terminal_mouse(s, report),
            TermEvent::Focus(false) => self.terminal_release_all(s),
            TermEvent::Paste { text, truncated } => {
                let (queued, skipped) = self.terminal_paste(&text);
                if truncated {
                    s.notify(format!("pasted {queued} keys; the paste was cut short"));
                } else if !skipped.is_empty() {
                    s.notify(format!(
                        "pasted {queued} keys; {} characters have no US Amiga key",
                        skipped.len()
                    ));
                }
            }
            _ => {}
        }
    }

    /// Type a pasted text into the machine, starting a moment from now (see
    /// [`PASTE_START_DELAY_MS`]).
    fn terminal_paste(&mut self, text: &str) -> (usize, Vec<char>) {
        self.type_text(text, PASTE_START_DELAY_MS)
    }

    fn terminal_key(&mut self, s: &mut TermSession, report: KeyReport) {
        let (key, pressed, tap) = match s.keys.route(report) {
            KeyRoute::Ignore => return,
            KeyRoute::Command(key) => {
                self.terminal_command(s, key);
                return;
            }
            KeyRoute::Machine { key, pressed, tap } => (key, pressed, tap),
        };
        let Some(code) = terminal_keycode(key) else {
            return;
        };
        if !tap {
            self.terminal_key_transition(code, pressed);
            return;
        }
        // A tap: the terminal will not say when the key went up. The
        // modifiers its report names go down first and up with it; one
        // still down from an earlier tap that this report does not name
        // goes up first, or "A" then "b" typed quickly would be "AB".
        for stale in stale_tap_modifiers(&s.taps, report.mods) {
            s.taps.retain(|(c, _)| *c != stale);
            self.terminal_key_transition(stale, false);
        }
        let due = self.emu.bus().emulated_frames() + TAP_FRAMES;
        for held in modifier_keycodes(report.mods) {
            if !s.taps.iter().any(|(c, _)| *c == held) {
                self.terminal_key_transition(held, true);
                s.taps.push((held, due));
            }
        }
        if let Some(i) = s.taps.iter().position(|(c, _)| *c == code) {
            // The same key again before its tap ended: end that one first,
            // or the machine would see one long press.
            s.taps.swap_remove(i);
            self.terminal_key_transition(code, false);
        }
        self.terminal_key_transition(code, true);
        s.taps.push((code, due));
    }

    /// A key going down or up, routed as the window routes a host key:
    /// keyboard-joystick bindings first, then the Amiga keyboard.
    fn terminal_key_transition(&mut self, code: KeyCode, pressed: bool) {
        if self.handle_keyboard_joystick_key(code, pressed) {
            return;
        }
        if let Some(rawkey) = host_to_amiga_rawkey(code) {
            self.handle_live_amiga_key_event(rawkey, pressed);
        }
    }

    fn terminal_release_due_taps(&mut self, s: &mut TermSession) {
        let frame = self.emu.bus().emulated_frames();
        let mut due = Vec::new();
        s.taps.retain(|&(code, at)| {
            let release = frame >= at;
            if release {
                due.push(code);
            }
            !release
        });
        // The key itself before the modifiers it was typed with.
        for code in due.into_iter().rev() {
            self.terminal_key_transition(code, false);
        }
    }

    /// The terminal lost the focus: whatever is held is let go now, since
    /// its release will go to another window.
    fn terminal_release_all(&mut self, s: &mut TermSession) {
        for (code, _) in std::mem::take(&mut s.taps) {
            self.terminal_key_transition(code, false);
        }
        for rawkey in 0..128 {
            self.release_amiga_rawkey_if_held(rawkey);
        }
        if self
            .keyboard_joy_held
            .iter()
            .any(crate::keymap::HeldKeys::any_held)
        {
            self.keyboard_joy_held = Default::default();
            self.pump_joystick_input();
        }
        self.release_mouse_buttons();
        s.pointer = None;
        s.keys.lose_focus();
    }

    fn terminal_mouse(&mut self, s: &mut TermSession, report: MouseReport) {
        let shown = s.output().shown();
        // SGR pixel reports are 1-based.
        let point = shown.placement.and_then(|placement| {
            placement.image_point(
                s.geometry,
                shown.width,
                shown.height,
                f64::from(report.x.saturating_sub(1)),
                f64::from(report.y.saturating_sub(1)),
            )
        });
        // The host pointer is not captured, so it moves the Amiga's by the
        // distance it travels over the picture, as an uncaptured window
        // does.
        if let (Some(now), Some(before)) = (point, s.pointer) {
            self.add_host_mouse_delta(now.0 - before.0, now.1 - before.1);
        }
        s.pointer = point;
        if report.motion {
            return;
        }
        let index = match report.button {
            TermMouseButton::Left => 0,
            TermMouseButton::Right => 1,
            TermMouseButton::Middle => 2,
            _ => return,
        };
        // A press counts over the picture only; a release always does, or a
        // button let go over the status line would stay held.
        if report.pressed && point.is_none() {
            return;
        }
        if let Some(port) = self.mouse_port() {
            self.emu
                .bus_mut()
                .input
                .set_mouse_button(port, index, report.pressed);
            self.note_live_mouse_button(index, report.pressed);
        }
    }

    fn terminal_command(&mut self, s: &mut TermSession, key: TermKey) {
        match key {
            TermKey::Char('q') => self.quit_requested = true,
            TermKey::Char('p') => {
                self.toggle_pause();
                s.notify(if self.paused { "paused" } else { "resumed" });
            }
            TermKey::Char('w') => {
                self.toggle_warp();
                s.notify(if self.emu.paced() {
                    "warp off"
                } else {
                    "warp on"
                });
            }
            TermKey::Char('r') => {
                self.reset_emulator(true);
                s.taps.clear();
                s.notify("reset");
            }
            TermKey::Char('s') => {
                let path = crate::screenshot::auto_filename();
                self.save_screenshot(&path);
                s.notify(format!("screenshot: {}", path.display()));
            }
            TermKey::Escape => {}
            _ => s.notify("F12 then q, p, w, r or s"),
        }
    }

    fn terminal_update_status(&mut self, s: &mut TermSession) {
        let now = Instant::now();
        let elapsed = now.duration_since(s.rates_at);
        if elapsed >= STATUS_INTERVAL {
            let stats = s.output().stats();
            let frames = stats.frames.load(std::sync::atomic::Ordering::Relaxed);
            let bytes = stats.bytes.load(std::sync::atomic::Ordering::Relaxed);
            let secs = elapsed.as_secs_f64();
            s.fps = (frames - s.rates_frames) as f64 / secs;
            s.kib_per_sec = (bytes - s.rates_bytes) as f64 / secs / 1024.0;
            s.rates_frames = frames;
            s.rates_bytes = bytes;
            s.rates_at = now;
        }
        if s.notice.as_ref().is_some_and(|(_, until)| now >= *until) {
            s.notice = None;
        }
        let text = if s.keys.menu {
            " F12:  q quit   p pause   w warp   r reset   s screenshot   Esc back".to_string()
        } else {
            let secs = self.emu.bus().emulated_seconds();
            let state = if self.cpu_halted {
                "halted"
            } else if !self.powered_on {
                "off"
            } else if self.paused {
                "paused"
            } else if !self.emu.paced() {
                "warp"
            } else {
                "running"
            };
            let message = s
                .notice
                .as_ref()
                .map(|(text, _)| text.as_str())
                .or_else(|| {
                    self.osd
                        .as_ref()
                        .filter(|osd| now < osd.expires_at)
                        .map(|osd| osd.text.as_str())
                })
                .unwrap_or("");
            format!(
                " Copperline  {:02}:{:04.1}  {state}  |  {:.0} fps  {:.0} KiB/s  |  F12 menu  {message}",
                (secs / 60.0) as u64,
                secs % 60.0,
                s.fps,
                s.kib_per_sec,
            )
        };
        if text != s.status {
            s.output().post_status(text.clone());
            s.status = text;
        }
    }
}

/// Where a key report goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyRoute {
    /// Nowhere: a repeat, F12 itself, or the release of a command key.
    Ignore,
    /// F12 then this key: a host command.
    Command(TermKey),
    /// A transition for the machine; `tap` when no release will follow.
    Machine {
        key: TermKey,
        pressed: bool,
        tap: bool,
    },
}

/// The host side of the keyboard: the F12 menu, and which releases belong
/// to command keys rather than to the machine.
#[derive(Debug, Default)]
struct HostKeys {
    /// The terminal reports key releases (Kitty keyboard protocol with
    /// event types). Without them every key is a tap.
    key_releases: bool,
    /// F12 was pressed: the next key is a host command.
    menu: bool,
    /// Command keys whose release is still to come; it is not the
    /// machine's.
    swallowed: Vec<TermKey>,
}

impl HostKeys {
    fn new(key_releases: bool) -> Self {
        Self {
            key_releases,
            ..Self::default()
        }
    }

    fn route(&mut self, report: KeyReport) -> KeyRoute {
        // The Amiga keyboard repeats by itself; a host repeat is no new key.
        if report.action == KeyAction::Repeat {
            return KeyRoute::Ignore;
        }
        let pressed = report.action == KeyAction::Press;
        if report.key == TermKey::F(12) {
            if pressed {
                self.menu = !self.menu;
            }
            return KeyRoute::Ignore;
        }
        let tap = report.legacy || !self.key_releases;
        if let Some(i) = self.swallowed.iter().position(|k| *k == report.key) {
            self.swallowed.swap_remove(i);
            if !pressed {
                return KeyRoute::Ignore;
            }
            // A new press of a command key whose release never came: that
            // release was lost, and this press is an ordinary one.
        }
        if !pressed {
            // A release reaches the machine even with the menu open: its
            // press may well have gone there before F12 was pressed.
            return KeyRoute::Machine {
                key: report.key,
                pressed,
                tap,
            };
        }
        // Modifiers pass through the menu, so a held Shift stays the
        // machine's.
        if self.menu && !matches!(report.key, TermKey::Modifier(..)) {
            self.menu = false;
            if !tap {
                self.swallowed.push(report.key);
            }
            return KeyRoute::Command(report.key);
        }
        KeyRoute::Machine {
            key: report.key,
            pressed,
            tap,
        }
    }

    /// The terminal lost the focus: releases will go elsewhere, and the
    /// menu closes.
    fn lose_focus(&mut self) {
        self.menu = false;
        self.swallowed.clear();
    }
}

/// The window key code for a terminal key: the physical key it names.
fn terminal_keycode(key: TermKey) -> Option<KeyCode> {
    use KeyCode::*;
    Some(match key {
        TermKey::Char(ch) => match ch {
            'a'..='z' => {
                const LETTERS: [KeyCode; 26] = [
                    KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM,
                    KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
                ];
                LETTERS[(ch as u8 - b'a') as usize]
            }
            '0'..='9' => {
                const DIGITS: [KeyCode; 10] = [
                    Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
                ];
                DIGITS[(ch as u8 - b'0') as usize]
            }
            '`' => Backquote,
            '-' => Minus,
            '=' => Equal,
            '[' => BracketLeft,
            ']' => BracketRight,
            '\\' => Backslash,
            ';' => Semicolon,
            '\'' => Quote,
            ',' => Comma,
            '.' => Period,
            '/' => Slash,
            ' ' => Space,
            _ => return None,
        },
        TermKey::Escape => Escape,
        TermKey::Enter => Enter,
        TermKey::Tab => Tab,
        TermKey::Backspace => Backspace,
        TermKey::Delete => Delete,
        TermKey::Left => ArrowLeft,
        TermKey::Right => ArrowRight,
        TermKey::Up => ArrowUp,
        TermKey::Down => ArrowDown,
        TermKey::CapsLock => CapsLock,
        TermKey::F(n @ 1..=11) => {
            [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11][usize::from(n - 1)]
        }
        TermKey::Keypad(k) => match k {
            KeypadKey::Digit(d) => [
                Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8,
                Numpad9,
            ]
            .get(usize::from(d))
            .copied()?,
            KeypadKey::Decimal => NumpadDecimal,
            KeypadKey::Divide => NumpadDivide,
            KeypadKey::Multiply => NumpadMultiply,
            KeypadKey::Subtract => NumpadSubtract,
            KeypadKey::Add => NumpadAdd,
            KeypadKey::Enter => NumpadEnter,
            KeypadKey::Equal | KeypadKey::Separator => return None,
        },
        TermKey::Modifier(kind, side) => match (kind, side) {
            (ModifierKey::Shift, Side::Left) => ShiftLeft,
            (ModifierKey::Shift, Side::Right) => ShiftRight,
            (ModifierKey::Control, Side::Left) => ControlLeft,
            (ModifierKey::Control, Side::Right) => ControlRight,
            (ModifierKey::Alt, Side::Left) => AltLeft,
            (ModifierKey::Alt, Side::Right) | (ModifierKey::IsoLevel3Shift, _) => AltRight,
            (ModifierKey::Super, Side::Left) => SuperLeft,
            (ModifierKey::Super, Side::Right) => SuperRight,
            _ => return None,
        },
        _ => return None,
    })
}

/// The modifier keys a tap's report says were down.
fn modifier_keycodes(mods: Mods) -> impl Iterator<Item = KeyCode> {
    [
        (mods.shift(), KeyCode::ShiftLeft),
        (mods.ctrl(), KeyCode::ControlLeft),
        (mods.alt(), KeyCode::AltLeft),
    ]
    .into_iter()
    .filter_map(|(down, code)| down.then_some(code))
}

/// The modifiers among the taps still down that a new tap's report does
/// not name: they belong to the earlier key, not to this one.
fn stale_tap_modifiers(taps: &[(KeyCode, u64)], mods: Mods) -> Vec<KeyCode> {
    let all = Mods(Mods::SHIFT | Mods::CTRL | Mods::ALT);
    modifier_keycodes(all)
        .filter(|code| taps.iter().any(|(c, _)| c == code))
        .filter(|code| !modifier_keycodes(mods).any(|wanted| wanted == *code))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_keys_map_to_the_window_key_codes() {
        assert_eq!(terminal_keycode(TermKey::Char('q')), Some(KeyCode::KeyQ));
        assert_eq!(terminal_keycode(TermKey::Char('0')), Some(KeyCode::Digit0));
        assert_eq!(terminal_keycode(TermKey::Char('/')), Some(KeyCode::Slash));
        assert_eq!(terminal_keycode(TermKey::F(11)), Some(KeyCode::F11));
        // F12 is the host key, never the machine's.
        assert_eq!(terminal_keycode(TermKey::F(12)), None);
        assert_eq!(
            terminal_keycode(TermKey::Keypad(KeypadKey::Digit(7))),
            Some(KeyCode::Numpad7)
        );
        assert_eq!(
            terminal_keycode(TermKey::Modifier(ModifierKey::Super, Side::Left)),
            Some(KeyCode::SuperLeft)
        );
        assert_eq!(terminal_keycode(TermKey::Char('\u{e9}')), None);
    }

    #[test]
    fn every_text_key_reaches_an_amiga_key() {
        for ch in "abcdefghijklmnopqrstuvwxyz0123456789`-=[]\\;',./ ".chars() {
            let code = terminal_keycode(TermKey::Char(ch)).unwrap();
            assert!(host_to_amiga_rawkey(code).is_some(), "{ch:?}");
        }
    }

    fn report(key: TermKey, action: KeyAction) -> KeyReport {
        KeyReport {
            key,
            mods: Mods::default(),
            action,
            legacy: false,
        }
    }

    fn machine(key: TermKey, pressed: bool) -> KeyRoute {
        KeyRoute::Machine {
            key,
            pressed,
            tap: false,
        }
    }

    #[test]
    fn a_key_held_across_the_menu_is_still_released() {
        let a = TermKey::Char('a');
        let mut keys = HostKeys::new(true);
        assert_eq!(keys.route(report(a, KeyAction::Press)), machine(a, true));
        assert_eq!(
            keys.route(report(TermKey::F(12), KeyAction::Press)),
            KeyRoute::Ignore
        );
        assert!(keys.menu);
        // A was down before F12: its release is the machine's.
        assert_eq!(keys.route(report(a, KeyAction::Release)), machine(a, false));
        assert_eq!(
            keys.route(report(TermKey::Escape, KeyAction::Press)),
            KeyRoute::Command(TermKey::Escape)
        );
        assert!(!keys.menu);
        // The command key's own release is not.
        assert_eq!(
            keys.route(report(TermKey::Escape, KeyAction::Release)),
            KeyRoute::Ignore
        );
    }

    #[test]
    fn menu_routing_of_repeats_modifiers_and_taps() {
        let shift = TermKey::Modifier(ModifierKey::Shift, Side::Left);
        let mut keys = HostKeys::new(true);
        assert_eq!(
            keys.route(report(TermKey::Char('a'), KeyAction::Repeat)),
            KeyRoute::Ignore
        );
        keys.route(report(TermKey::F(12), KeyAction::Press));
        // Modifiers pass through an open menu.
        assert_eq!(
            keys.route(report(shift, KeyAction::Press)),
            machine(shift, true)
        );
        assert!(keys.menu);
        assert_eq!(
            keys.route(report(TermKey::Char('q'), KeyAction::Press)),
            KeyRoute::Command(TermKey::Char('q'))
        );
        // A command key whose release was lost: its next press is the
        // machine's, and so is that press's release.
        let p = TermKey::Char('p');
        keys.route(report(TermKey::F(12), KeyAction::Press));
        keys.route(report(p, KeyAction::Press));
        assert_eq!(keys.route(report(p, KeyAction::Press)), machine(p, true));
        assert_eq!(keys.route(report(p, KeyAction::Release)), machine(p, false));
        // Without release reports every key is a tap.
        let mut legacy = HostKeys::new(false);
        assert_eq!(
            legacy.route(report(TermKey::Char('x'), KeyAction::Press)),
            KeyRoute::Machine {
                key: TermKey::Char('x'),
                pressed: true,
                tap: true
            }
        );
    }

    #[test]
    fn losing_focus_closes_the_menu_and_forgets_command_keys() {
        let q = TermKey::Char('q');
        let mut keys = HostKeys::new(true);
        keys.route(report(TermKey::F(12), KeyAction::Press));
        keys.route(report(q, KeyAction::Press));
        keys.route(report(TermKey::F(12), KeyAction::Press));
        keys.lose_focus();
        assert!(!keys.menu);
        assert_eq!(keys.route(report(q, KeyAction::Release)), machine(q, false));
    }

    #[test]
    fn tap_modifiers_follow_the_report() {
        let mods: Vec<_> = modifier_keycodes(Mods(Mods::SHIFT | Mods::CTRL)).collect();
        assert_eq!(mods, vec![KeyCode::ShiftLeft, KeyCode::ControlLeft]);
        assert_eq!(modifier_keycodes(Mods::default()).count(), 0);
    }

    /// A tap's modifiers stay down for a few frames. A second tap inside
    /// that time that does not name them must not inherit them.
    #[test]
    fn a_tap_lets_go_of_modifiers_left_from_the_one_before() {
        // "A" was just tapped: Shift and the key are down until frame 3.
        let taps = [(KeyCode::ShiftLeft, 3), (KeyCode::KeyA, 3)];
        // "b": Shift goes up before it.
        assert_eq!(
            stale_tap_modifiers(&taps, Mods::default()),
            vec![KeyCode::ShiftLeft]
        );
        // "B": Shift is wanted again and stays.
        assert!(stale_tap_modifiers(&taps, Mods(Mods::SHIFT)).is_empty());
        // Ctrl+b: Shift goes, and Ctrl is not there to go.
        assert_eq!(
            stale_tap_modifiers(&taps, Mods(Mods::CTRL)),
            vec![KeyCode::ShiftLeft]
        );
        // Plain keys down are never let go here.
        assert!(stale_tap_modifiers(&[(KeyCode::KeyA, 3)], Mods::default()).is_empty());
    }
    /// The paste shortcut's modifiers are still reported as held when the
    /// pasted text arrives, so its first key waits for them to come up.
    #[test]
    fn pasted_text_starts_after_the_shortcut_modifiers_are_released() {
        let mut app = crate::video::window::tests::test_app();
        let now = app.emu.bus().emulated_seconds();
        let (queued, skipped) = app.terminal_paste("dir");
        assert_eq!((queued, skipped.len()), (3, 0));
        let first = app
            .auto_keys
            .iter()
            .map(|key| key.press_at_emulated_secs)
            .fold(f64::INFINITY, f64::min);
        let delay = f64::from(PASTE_START_DELAY_MS) / 1000.0;
        assert!(
            first >= now + delay,
            "first key at {first}, pasted at {now}"
        );
        assert!(first < now + delay + 0.05);
    }

    use crate::terminal::tty::testing::{contains, Pty, ENTER_BYTES, LEAVE_BYTES};

    /// One step of a fake terminal's script: once the session has written
    /// `after`, send it `keys`.
    type Step = (&'static [u8], &'static [u8]);

    /// What the fake terminal does.
    #[derive(Clone, Copy)]
    struct Fake {
        /// Speaks the Kitty keyboard protocol (flags 15) or reports keys
        /// the legacy way.
        kitty_keyboard: bool,
        script: &'static [Step],
        /// Close the terminal after the script instead of waiting for the
        /// session to leave.
        hang_up: bool,
    }

    /// The terminal side of a session, on the pseudo-terminal's master:
    /// answer the probe as a Kitty-graphics terminal would, then follow the
    /// script. Returns everything the session wrote.
    fn fake_terminal(pty: Pty, fake: Fake) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut seen = pty.output_until(b"\x1b[c");
            if fake.kitty_keyboard {
                pty.send(b"\x1b[?15u");
            }
            pty.send(b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4c");
            let deadline = Instant::now() + Duration::from_secs(20);
            let wait_for = |seen: &mut Vec<u8>, what: &[u8]| {
                while !contains(seen, what) {
                    assert!(
                        Instant::now() < deadline,
                        "the session never wrote {:?}",
                        String::from_utf8_lossy(what)
                    );
                    seen.extend(pty.take_output(Duration::from_millis(50)));
                }
            };
            for (after, keys) in fake.script {
                wait_for(&mut seen, after);
                pty.send(keys);
            }
            if !fake.hang_up {
                wait_for(&mut seen, LEAVE_BYTES);
            }
            seen
        })
    }

    fn run_session(fake: Fake) -> (Result<i32>, Vec<u8>) {
        let pty = Pty::open();
        let terminal = pty.session().unwrap();
        let emulator = fake_terminal(pty, fake);
        let app = crate::video::window::tests::test_app();
        let result = app.run_terminal_on(terminal, &crate::terminal::Options::default());
        (result, emulator.join().unwrap())
    }

    /// The whole loop on a pseudo-terminal: probe, frames and status out,
    /// F12 q in, and the terminal given back.
    #[test]
    fn a_session_shows_frames_and_quits_on_f12_q() {
        let (result, seen) = run_session(Fake {
            kitty_keyboard: true,
            // Once a frame has been shown: F12 press and release, then q
            // press and release.
            script: &[(
                b"\x1b_Ga=T,f=24",
                b"\x1b[24;1:1~\x1b[24;1:3~\x1b[113;1:1u\x1b[113;1:3u",
            )],
            hang_up: false,
        });
        assert_eq!(result.unwrap(), 0);
        assert!(seen.starts_with(ENTER_BYTES));
        assert!(
            contains(&seen, b"\x1b_Ga=T,f=24"),
            "no frame was sent: {:?}",
            String::from_utf8_lossy(&seen[..seen.len().min(600)])
        );
        assert!(seen.ends_with(LEAVE_BYTES));
    }

    /// On a terminal without the Kitty keyboard protocol, Alt+[ sends the
    /// two bytes that start a control sequence and nothing more. Pressed in
    /// the F12 menu it is an unknown command, which the status line says;
    /// left waiting for the rest of a sequence it would never be seen, and
    /// would take the next key typed as that sequence's end.
    #[test]
    fn a_legacy_alt_bracket_is_a_key_and_not_the_start_of_a_sequence() {
        let (result, seen) = run_session(Fake {
            kitty_keyboard: false,
            script: &[
                (b"F12 menu", b"\x1b[24~\x1b["),
                (b"F12 then q, p, w, r or s", b"\x1b[24~q"),
            ],
            hang_up: false,
        });
        assert_eq!(result.unwrap(), 0);
        assert!(seen.ends_with(LEAVE_BYTES));
    }

    /// A terminal that goes away ends the session with an error instead of
    /// leaving it running with nothing to show it on.
    #[test]
    fn a_terminal_that_goes_away_ends_the_session() {
        let (result, _) = run_session(Fake {
            kitty_keyboard: true,
            script: &[(b"F12 menu", b"")],
            hang_up: true,
        });
        assert!(result.is_err(), "the session outlived its terminal");
    }
}
