// SPDX-License-Identifier: GPL-3.0-or-later

//! The scripted-session [`ModemTransport`]: replays a canned dial-out
//! session from a file instead of touching a real socket, so a modem-using
//! headless run (or unit test) is byte-for-byte reproducible the same way
//! `--script`/`--press-after` make keyboard and joystick input reproducible
//! -- see `docs/guide/headless.md`'s scripted-input table for the sibling
//! mechanism this one is modelled on.
//!
//! A session file is a line-oriented list of directives, one per line,
//! executed in order against a single simulated call:
//!
//! ```text
//! # blank lines and lines starting with # are ignored
//! accept                    # the next dial() succeeds (CONNECT)
//! refuse busy                # ...or fails instead: "busy" (BUSY) or
//!                             # "unreachable" (NO CARRIER, the default)
//! delay 0.5                  # hold the next `send` back until this many
//!                             # emulated seconds have passed
//! send Welcome to the BBS\r\n # bytes the far end emits to the guest
//! expect ATH\r                # bytes the guest must send next; a mismatch
//!                             # logs and drops the line (NO CARRIER)
//! close                      # the far end hangs up here
//! ```
//!
//! `send`/`expect` text runs to the end of the line with `\r`, `\n`, `\t`,
//! and `\\` recognized as escapes; every other byte is taken literally
//! (UTF-8 encoded), so 8-bit/binary payloads need `\r`/`\n`-only framing or
//! aren't expressible here -- this format targets terminal-program
//! dialogue, not binary transfers.
//!
//! `accept`/`refuse` are only meaningful as the directive immediately after
//! script start or a `close`/mismatch: each simulates exactly one call.
//! Calling [`ScriptedTransport::dial`] with none queued (the script never
//! had one, or a previous call already consumed it) is honest NO CARRIER,
//! not a hang -- see its doc comment. Running out of directives mid-call
//! (the far end simply stops talking) is not an error: the line stays up
//! with nothing more happening, same as a quiet BBS.
//!
//! Timing is deferred to two places depending on what the caller has: a
//! `delay` gates on real emulated time (`at_cck`, via
//! [`ModemTransport::advance`], the same clock `Settings::s12`'s guard-time
//! math uses) when [`ModemSerialSink`] has one to offer; `expect` completion
//! releases anything with no *undue* `delay` ahead of it immediately from
//! [`ModemTransport::write_byte`], which runs with no `at_cck` of its own
//! (see [`ScriptedTransport::progress`]'s two call sites).
//!
//! [`ModemSerialSink`]: super::ModemSerialSink

use super::{DialError, ModemTransport};
use crate::chipset::paula::PAULA_CLOCK_HZ;
use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::io;
use std::path::Path;

#[derive(Debug, Clone)]
enum Directive {
    Accept,
    Refuse(DialError),
    /// Emulated color-clock ticks to withhold the next `Send` (or whatever
    /// follows) by, converted from the script's seconds at parse time.
    Delay(u64),
    Send(Vec<u8>),
    Expect(Vec<u8>),
    Close,
}

/// [`ModemTransport`] backed by a parsed session file. See the module doc
/// comment for the file format and the two timing paths.
pub(crate) struct ScriptedTransport {
    directives: VecDeque<Directive>,
    /// The script as parsed, kept so a timeline jump (rewind, or loading
    /// an earlier save state) can put the replay back to the top. The
    /// consumed position is host-side state that no save state carries, so
    /// without this a rewind would carry on from directives consumed in
    /// the abandoned future -- the next dial silently short of its script,
    /// or straight to NO CARRIER. See [`ModemTransport::reset_timeline`].
    initial: VecDeque<Directive>,
    /// Bytes released by completed `Send` directives, waiting for
    /// [`ModemTransport::read_byte`].
    ready: VecDeque<u8>,
    /// An in-progress `Expect`: the full expected bytes and how many have
    /// matched so far. `Some` blocks [`Self::progress`] from advancing
    /// past it until [`ModemTransport::write_byte`] completes or busts it.
    pending_expect: Option<(Vec<u8>, usize)>,
    /// The color-clock tick a pending `Delay` unblocks at, set the first
    /// time [`Self::progress`] sees it with a real `at_cck` to measure
    /// from. `None` between delays, and while the current one is
    /// unresolved for lack of a real `at_cck` (see [`Self::progress`]).
    delay_due_cck: Option<u64>,
    carrier: bool,
    /// The script's exhaustion has already been logged once, so idle
    /// polling after doesn't spam the log every tick.
    ended: bool,
    /// The session file's path (or a fixed label in tests), quoted in every
    /// log line so a multi-modem run's messages are attributable.
    label: String,
}

impl ScriptedTransport {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading scripted modem session {}", path.display()))?;
        let directives =
            parse_script(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        Ok(Self::from_directives(
            directives,
            path.display().to_string(),
        ))
    }

    #[cfg(test)]
    fn from_script(text: &str) -> Self {
        let directives = parse_script(text).expect("test script parses");
        Self::from_directives(directives, "<test script>".to_string())
    }

    fn from_directives(directives: VecDeque<Directive>, label: String) -> Self {
        Self {
            initial: directives.clone(),
            directives,
            ready: VecDeque::new(),
            pending_expect: None,
            delay_due_cck: None,
            carrier: false,
            ended: false,
            label,
        }
    }

    /// Run as many due directives as possible. `at_cck` is `Some` when
    /// called from [`ModemTransport::advance`] (a real emulated-time
    /// reference to measure a `Delay` from); `None` from
    /// [`ModemTransport::write_byte`]'s expect-completion path, which has
    /// no `at_cck` of its own -- an unresolved `Delay` is left queued
    /// rather than guessing "now" is the delay's start, since that would
    /// make the delay's length depend on when the guest happened to type
    /// rather than on the call's own clock.
    fn progress(&mut self, at_cck: Option<u64>) {
        if !self.carrier {
            return;
        }
        loop {
            if self.pending_expect.is_some() {
                return;
            }
            let Some(directive) = self.directives.pop_front() else {
                if !self.ended {
                    self.ended = true;
                    log::debug!(
                        "scripted modem session {}: script exhausted, line stays up with \
                         nothing more happening",
                        self.label
                    );
                }
                return;
            };
            match directive {
                Directive::Delay(ticks) => {
                    let Some(at_cck) = at_cck else {
                        self.directives.push_front(Directive::Delay(ticks));
                        return;
                    };
                    let due = *self.delay_due_cck.get_or_insert(at_cck + ticks);
                    if at_cck < due {
                        self.directives.push_front(Directive::Delay(ticks));
                        return;
                    }
                    self.delay_due_cck = None;
                }
                Directive::Send(bytes) => self.ready.extend(bytes),
                Directive::Expect(bytes) => {
                    self.pending_expect = Some((bytes, 0));
                    return;
                }
                Directive::Close => {
                    log::info!("scripted modem session {}: close", self.label);
                    self.carrier = false;
                    return;
                }
                Directive::Accept | Directive::Refuse(_) => {
                    // Only meaningful as the directive a `dial()` consumes
                    // directly; reaching one here means the script named a
                    // second call inline mid-session, which this format has
                    // no way to attach to a second `dial()` -- skip it
                    // rather than silently reinterpreting it as call data.
                    log::warn!(
                        "scripted modem session {}: accept/refuse mid-call ignored (only \
                         valid immediately before a dial)",
                        self.label
                    );
                }
            }
        }
    }
}

impl ModemTransport for ScriptedTransport {
    fn dial(&mut self, host_port: &str) -> Result<(), DialError> {
        match self.directives.pop_front() {
            Some(Directive::Accept) => {
                self.carrier = true;
                self.delay_due_cck = None;
                self.ended = false;
                log::info!(
                    "scripted modem session {}: accept (dialed {host_port:?})",
                    self.label
                );
                Ok(())
            }
            Some(Directive::Refuse(reason)) => {
                log::info!(
                    "scripted modem session {}: refuse (dialed {host_port:?})",
                    self.label
                );
                Err(reason)
            }
            other => {
                // Exhausted, or the next directive was not a call decision
                // at all: a scripted session names a fixed number of calls,
                // and dialing past the end is a script/test bug. Honest
                // NO CARRIER either way, not a hang.
                if let Some(d) = other {
                    self.directives.push_front(d);
                }
                log::warn!(
                    "scripted modem session {}: dial({host_port:?}) with no accept/refuse \
                     directive next (script exhausted or out of order); reporting NO CARRIER",
                    self.label
                );
                Err(DialError::Unreachable)
            }
        }
    }

    fn read_byte(&mut self) -> Option<u8> {
        self.ready.pop_front()
    }

    fn has_pending(&self) -> bool {
        !self.ready.is_empty()
    }

    fn write_byte(&mut self, b: u8) {
        let Some((expected, idx)) = self.pending_expect.as_mut() else {
            // Nothing currently expected: the byte is not checked against
            // the script (see the module doc comment).
            return;
        };
        if expected[*idx] == b {
            *idx += 1;
            if *idx == expected.len() {
                self.pending_expect = None;
                // Whatever follows (a `send` with no gating `delay`, most
                // often) is released now rather than waiting for the next
                // `advance` tick, so a synchronous test that never calls
                // `poll` still sees it.
                self.progress(None);
            }
        } else {
            log::error!(
                "scripted modem session {}: expected {:?}, guest sent {:?} at offset {idx}; \
                 dropping the line",
                self.label,
                String::from_utf8_lossy(expected),
                b as char,
            );
            self.pending_expect = None;
            self.carrier = false;
        }
    }

    fn carrier(&self) -> bool {
        self.carrier
    }

    fn hangup(&mut self) {
        self.carrier = false;
        self.pending_expect = None;
        self.delay_due_cck = None;
        // Bytes a `Send` directive already released but the guest never
        // read belong to the call that just ended. Left queued they would
        // surface partway through the *next* scripted call, which is both
        // wrong and not reproducible from reading the script.
        self.ready.clear();
    }

    fn reset_timeline(&mut self) {
        // Replay from the top: the emulated machine has gone backwards, so
        // the directives consumed on the abandoned timeline have not
        // happened as far as the guest is concerned. Restarting is what
        // keeps "the same script gives the same session" true across a
        // rewind; carrying on from the old position would not.
        self.directives = self.initial.clone();
        self.ready.clear();
        self.pending_expect = None;
        self.delay_due_cck = None;
        self.carrier = false;
        self.ended = false;
    }

    fn listen(&mut self, _addr: &str) -> io::Result<()> {
        // The scripted transport only ever plays back a fixed sequence of
        // outbound calls; there is no inbound side to bind. Honest error
        // rather than a silent no-op, so a config combining `session` with
        // `listen` (or a guest's `AT*L`) finds out immediately.
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the scripted modem session transport has no inbound-call support \
             (dial-out replay only)",
        ))
    }

    fn ringing(&self) -> bool {
        false
    }

    fn answer(&mut self) -> bool {
        false
    }

    fn reject(&mut self) {}

    fn advance(&mut self, at_cck: u64) {
        self.progress(Some(at_cck));
    }
}

fn parse_script(text: &str) -> std::result::Result<VecDeque<Directive>, String> {
    let mut out = VecDeque::new();
    for (i, raw_line) in text.lines().enumerate() {
        let lineno = i + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, rest) = match line.split_once(char::is_whitespace) {
            Some((k, r)) => (k, r.trim_start()),
            None => (line, ""),
        };
        let directive = match keyword {
            "accept" => Directive::Accept,
            "refuse" => match rest.trim() {
                "" | "unreachable" => Directive::Refuse(DialError::Unreachable),
                "busy" => Directive::Refuse(DialError::Refused),
                other => {
                    return Err(format!(
                        "line {lineno}: refuse: {other:?} is not \"busy\" or \"unreachable\""
                    ))
                }
            },
            "delay" => {
                let secs: f64 = rest.trim().parse().map_err(|_| {
                    format!("line {lineno}: delay: {rest:?} is not a number of seconds")
                })?;
                if !secs.is_finite() || secs < 0.0 {
                    return Err(format!(
                        "line {lineno}: delay: {secs} is not a non-negative number of seconds"
                    ));
                }
                Directive::Delay((secs * PAULA_CLOCK_HZ as f64) as u64)
            }
            "send" => Directive::Send(unescape(rest)),
            "expect" => {
                if rest.is_empty() {
                    return Err(format!("line {lineno}: expect: nothing to expect"));
                }
                Directive::Expect(unescape(rest))
            }
            "close" => Directive::Close,
            other => return Err(format!("line {lineno}: unknown directive {other:?}")),
        };
        out.push_back(directive);
    }
    Ok(out)
}

/// `\r`, `\n`, `\t`, `\\` escapes; anything else after a backslash (or no
/// backslash at all) is taken literally, UTF-8 encoded.
fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('\\') => out.push(b'\\'),
            Some(other) => {
                out.push(b'\\');
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modem::{ModemOptions, ModemSerialSink};
    use crate::serial::SerialSink;

    fn type_line(sink: &mut ModemSerialSink, s: &str, at_cck: u64) {
        for b in s.bytes() {
            sink.write_byte(b, at_cck);
        }
        sink.write_byte(0x0D, at_cck);
    }

    fn drain(sink: &mut ModemSerialSink) -> Vec<u8> {
        let mut v = Vec::new();
        while let Some(b) = sink.read_byte() {
            v.push(b);
        }
        v
    }

    fn drain_str(sink: &mut ModemSerialSink) -> String {
        String::from_utf8(drain(sink)).unwrap()
    }

    // ---- Parser ---------------------------------------------------------

    #[test]
    fn parses_every_directive_kind() {
        let script = "\
            # a comment\n\
            \n\
            accept\n\
            delay 0.5\n\
            send Welcome\\r\\n\n\
            expect ATH\\r\n\
            close\n";
        let directives = parse_script(script).unwrap();
        assert_eq!(directives.len(), 5);
        assert!(matches!(directives[0], Directive::Accept));
        assert!(matches!(directives[1], Directive::Delay(t) if t == PAULA_CLOCK_HZ as u64 / 2));
        assert!(matches!(&directives[2], Directive::Send(b) if b == b"Welcome\r\n"));
        assert!(matches!(&directives[3], Directive::Expect(b) if b == b"ATH\r"));
        assert!(matches!(directives[4], Directive::Close));
    }

    #[test]
    fn refuse_reasons_parse() {
        let d = parse_script("refuse busy\n").unwrap();
        assert!(matches!(d[0], Directive::Refuse(DialError::Refused)));
        let d = parse_script("refuse unreachable\n").unwrap();
        assert!(matches!(d[0], Directive::Refuse(DialError::Unreachable)));
        let d = parse_script("refuse\n").unwrap();
        assert!(matches!(d[0], Directive::Refuse(DialError::Unreachable)));
    }

    #[test]
    fn unknown_directive_is_a_parse_error() {
        let err = parse_script("frobnicate\n").unwrap_err();
        assert!(err.contains("line 1"), "{err:?}");
        assert!(err.contains("frobnicate"), "{err:?}");
    }

    #[test]
    fn negative_delay_is_a_parse_error() {
        assert!(parse_script("delay -1\n").is_err());
    }

    // ---- ATD -> CONNECT -> banner -> +++ -> ATH round-trip ---------------

    #[test]
    fn full_round_trip_atd_connect_banner_escape_hangup() {
        let script = "accept\nsend \\r\\nWelcome to the BBS\\r\\n\n";
        let transport = ScriptedTransport::from_script(script);
        let mut sink = ModemSerialSink::with_transport(Box::new(transport));
        // ATE0 so the assertions below only see result codes / relayed
        // bytes, not the guest's own echoed command lines.
        type_line(&mut sink, "ATE0", 0);
        drain(&mut sink);

        type_line(&mut sink, "ATD127.0.0.1:2323", 0);
        // The banner (a `send` with no gating `delay`) is available in the
        // same drain as CONNECT itself: `enter_online` hands the transport
        // the real `at_cck` before returning, so there is no separate tick
        // to wait for.
        assert_eq!(
            drain_str(&mut sink),
            "\r\nCONNECT\r\n\r\nWelcome to the BBS\r\n"
        );

        // S12's default guard time is 50 fiftieths-of-a-second (one
        // second); +++ with the guard elapsed on both sides drops to
        // command mode without any of the three escape characters
        // reaching the transport.
        let guard = u64::from(PAULA_CLOCK_HZ);
        sink.write_byte(b'+', guard);
        sink.write_byte(b'+', 2 * guard);
        sink.write_byte(b'+', 3 * guard);
        sink.poll(4 * guard);
        assert_eq!(drain_str(&mut sink), "\r\nOK\r\n");

        // ATH hangs up; the transport's carrier drops with it. Observed
        // through ATO, since the transport itself is private to this sink:
        // asking to resume online mode after a real hangup is NO CARRIER,
        // never CONNECT.
        type_line(&mut sink, "ATH", 5 * guard);
        assert_eq!(drain_str(&mut sink), "\r\nOK\r\n");
        type_line(&mut sink, "ATO", 6 * guard);
        assert_eq!(drain_str(&mut sink), "\r\nNO CARRIER\r\n");
    }

    // ---- Mismatch handling -------------------------------------------

    #[test]
    fn expect_mismatch_drops_carrier_and_logs() {
        let script = "accept\nexpect HELLO\r\n";
        let transport = ScriptedTransport::from_script(script);
        let mut sink = ModemSerialSink::with_transport(Box::new(transport));
        type_line(&mut sink, "ATE0", 0);
        drain(&mut sink);
        type_line(&mut sink, "ATD127.0.0.1:2323", 0);
        drain(&mut sink);

        // The guest sends the wrong byte partway through "HELLO".
        for b in b"HELP" {
            sink.write_byte(*b, 100);
        }
        // The mismatch already dropped carrier; the sink notices on its
        // next read and reports NO CARRIER, same as any other remote
        // hangup mid-call.
        assert_eq!(drain_str(&mut sink), "\r\nNO CARRIER\r\n");
    }

    #[test]
    fn refuse_reports_busy_to_the_guest() {
        let transport = ScriptedTransport::from_script("refuse busy\n");
        let mut sink = ModemSerialSink::with_transport(Box::new(transport));
        type_line(&mut sink, "ATE0", 0);
        drain(&mut sink);
        type_line(&mut sink, "ATD127.0.0.1:2323", 0);
        assert_eq!(drain_str(&mut sink), "\r\nBUSY\r\n");
    }

    #[test]
    fn exhausted_script_reports_no_carrier_on_redial() {
        let transport = ScriptedTransport::from_script("accept\nclose\n");
        let mut sink = ModemSerialSink::with_transport(Box::new(transport));
        type_line(&mut sink, "ATE0", 0);
        drain(&mut sink);
        type_line(&mut sink, "ATD127.0.0.1:2323", 0);
        drain(&mut sink); // CONNECT, then `close` reads back as NO CARRIER
        drain(&mut sink);
        // A second dial finds nothing left in the script.
        type_line(&mut sink, "ATD127.0.0.1:2323", 200);
        assert_eq!(drain_str(&mut sink), "\r\nNO CARRIER\r\n");
    }

    // ---- Determinism: two independent runs match byte-for-byte -------

    #[test]
    fn two_runs_of_the_same_script_produce_identical_output() {
        let script = "\
            accept\n\
            delay 0.1\n\
            send \\r\\nWelcome\\r\\n\n\
            expect BYE\\r\n\
            send \\r\\nNO CARRIER\\r\\n\n";
        let run = || {
            let transport = ScriptedTransport::from_script(script);
            let mut sink = ModemSerialSink::with_transport(Box::new(transport));
            type_line(&mut sink, "ATE0", 0);
            drain(&mut sink);
            type_line(&mut sink, "ATD bbs:23", 0);
            let mut out = drain_str(&mut sink);
            let ticks = PAULA_CLOCK_HZ as u64 / 10;
            sink.poll(ticks);
            out += &drain_str(&mut sink);
            for b in b"BYE\r" {
                sink.write_byte(*b, 2 * ticks);
            }
            out += &drain_str(&mut sink);
            out
        };
        assert_eq!(run(), run());
    }

    // ---- Config-facing constructor smoke test --------------------------

    #[test]
    fn new_scripted_builds_a_working_sink() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-modem-session-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.txt");
        std::fs::write(&path, "accept\nsend OK-FROM-SCRIPT\\r\\n\n").unwrap();
        let mut sink = ModemSerialSink::new_scripted(&path, ModemOptions::default()).unwrap();
        type_line(&mut sink, "ATE0", 0);
        drain(&mut sink);
        type_line(&mut sink, "ATD127.0.0.1:2323", 0);
        assert_eq!(drain_str(&mut sink), "\r\nCONNECT\r\nOK-FROM-SCRIPT\r\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hangup_discards_bytes_the_guest_never_read() {
        // Two calls in one script. The first releases a banner the guest
        // never drains before hanging up; those bytes belong to that call
        // and must not surface inside the second one.
        let script = "accept\nsend FIRST-CALL\\r\\n\naccept\nsend SECOND-CALL\\r\\n\n";
        let transport = ScriptedTransport::from_script(script);
        let mut sink = ModemSerialSink::with_transport(Box::new(transport));
        type_line(&mut sink, "ATE0", 0);
        drain(&mut sink);

        type_line(&mut sink, "ATD127.0.0.1:2323", 0);
        // Deliberately not drained: the banner is sitting in `ready`.
        type_line(&mut sink, "ATH", 0);
        drain(&mut sink);

        type_line(&mut sink, "ATD127.0.0.1:2323", 0);
        let second = drain_str(&mut sink);
        assert!(
            !second.contains("FIRST-CALL"),
            "the first call's undrained bytes leaked into the second: {second:?}"
        );
    }

    #[test]
    fn a_timeline_jump_replays_the_script_from_the_top() {
        use crate::modem::ModemTransport as _;
        let script = "accept\nsend BANNER\\r\\n\n";
        let mut transport = ScriptedTransport::from_script(script);
        assert!(transport.dial("127.0.0.1:2323").is_ok());
        transport.advance(0);
        assert!(transport.has_pending(), "the banner should be queued");

        // A rewind (or an earlier save state loading) puts the emulated
        // machine back before the call; the replay position is host-side
        // state no save state carries, so it has to be reset explicitly or
        // the next dial continues from the abandoned future.
        transport.reset_timeline();
        assert!(!transport.carrier(), "the jump drops the call");
        assert!(
            !transport.has_pending(),
            "the abandoned timeline's bytes must not survive the jump"
        );
        assert!(
            transport.dial("127.0.0.1:2323").is_ok(),
            "the script's first directive must be available again"
        );
        transport.advance(0);
        let mut got = Vec::new();
        while let Some(b) = transport.read_byte() {
            got.push(b);
        }
        assert_eq!(String::from_utf8(got).unwrap(), "BANNER\r\n");
    }
}
