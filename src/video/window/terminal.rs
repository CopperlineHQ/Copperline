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
use crate::terminal::tty::{Output, Terminal, KEYBOARD_FLAGS};
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
    pub fn run_terminal(mut self, options: crate::terminal::Options) -> Result<i32> {
        let log_path = options.resolved_log_path();
        let terminal = Terminal::open(&log_path)?;
        let caps = terminal.probe()?;
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
        let output = Output::start(
            terminal.output_handle()?,
            geometry,
            options.min_frame_interval(),
        )?;
        let now = Instant::now();
        let mut session = TermSession {
            output: Some(output),
            terminal,
            parser: Parser::new(),
            geometry,
            probed_cell: caps.cell_pixels,
            keys: HostKeys::new(key_releases),
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
                wait = Duration::ZERO;
            }
            if s.parser.lone_escape_pending() {
                // A lone ESC is the Escape key unless more of its sequence
                // is just behind it. (Any longer unfinished sequence simply
                // waits for its remaining bytes.)
                let n = s.terminal.read(&mut buf, Duration::from_millis(15))?;
                if n == 0 {
                    s.parser.flush_lone_escape(&mut events);
                } else {
                    s.parser.feed(&buf[..n], &mut events);
                }
            }
            for event in events.drain(..) {
                self.terminal_event(s, event);
            }
            if self.quit_requested {
                return Ok(self.exit_status());
            }
            if s.output()
                .stats()
                .failed
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(anyhow!("--terminal: the terminal stopped taking output"));
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
            TermEvent::Paste(text) => {
                let (queued, skipped) = self.type_text(&text, 0);
                if !skipped.is_empty() {
                    s.notify(format!(
                        "pasted {queued} keys; {} characters have no US Amiga key",
                        skipped.len()
                    ));
                }
            }
            _ => {}
        }
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
        // modifiers its report names go down first and up with it.
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
}
