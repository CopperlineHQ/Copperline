// SPDX-License-Identifier: GPL-3.0-or-later

//! Telnet NVT (RFC 854) layer for the modem's `AT*T1` translation mode.
//!
//! The guest side is a plain terminal program talking to serial.device, so
//! it knows nothing of IAC negotiation; this module answers a server's
//! option negotiation, unescapes inbound data, and escapes outbound data. A
//! faithful Rust port of `crates/copperline-web/www/serial-telnet.js` (the
//! browser build's own telnet layer, for the same reason: a WebSocket
//! bridge to a raw TCP host, sitting between a terminal-only guest and a
//! server that expects a telnet client on the other end). It deliberately
//! implements the minimal subset a BBS session needs:
//!
//! - ECHO (1) and SUPPRESS-GO-AHEAD (3): accepted from the server, the
//!   normal character-at-a-time BBS mode.
//! - BINARY (0): accepted both ways, which telnet-aware BBSes negotiate
//!   before ZModem transfers so nothing rewrites the stream.
//! - TERMINAL-TYPE (24): answered with a fixed name ("ANSI"), which BBS
//!   menus use to pick their art/charset.
//! - NAWS (31) and SEND-LOCATION (23): only under the WiModem `AT*T1`
//!   "reporting" extra -- see [`TelnetNvt::new`].
//! - Everything else is refused (WONT/DONT), which every server must
//!   accept.
//!
//! Data transforms: IAC (0xFF) bytes are doubled outbound and undoubled
//! inbound; in non-binary mode a bare outbound CR becomes CR NUL (the RFC
//! form, servers strip the NUL) and an inbound CR NUL becomes CR.

// Nothing calls this module yet (integration is a later milestone); only
// the unit tests below exercise it until then.
#![cfg_attr(not(test), allow(dead_code))]

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

const OPT_BINARY: u8 = 0;
const OPT_ECHO: u8 = 1;
const OPT_SEND_LOCATION: u8 = 23;
const OPT_SGA: u8 = 3;
const OPT_TTYPE: u8 = 24;
const OPT_NAWS: u8 = 31;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

/// Fixed terminal type answered to TTYPE SEND, matching the JS default.
const TERM_TYPE: &str = "ANSI";

/// Fixed location string answered under `AT*T1`'s SEND-LOCATION support
/// (RFC 779). There is no real host geography to report, so this names the
/// emulator instead of lying about a place.
const LOCATION: &str = "Copperline";

/// Parser state machine, mirroring `TelnetSession`'s `state` field in the
/// JS original.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Ordinary data bytes.
    Data,
    /// An IAC byte was just seen; the next byte says what kind of command.
    Iac,
    /// A WILL/WONT/DO/DONT verb was seen; the next byte is the option.
    Opt(u8),
    /// Inside a subnegotiation (`IAC SB ... IAC SE`), buffering its payload.
    Sb,
    /// An IAC was seen while buffering a subnegotiation's payload.
    SbIac,
}

/// A minimal RFC 854 Network Virtual Terminal: byte-oriented, so it fits
/// the modem's one-byte-at-a-time relay (see [`crate::modem`]'s
/// `ModemTransport`).
pub(crate) struct TelnetNvt {
    /// The WiModem `AT*T1` extras: NAWS and SEND-LOCATION. Off means every
    /// option outside the base BBS set (BINARY/ECHO/SGA/TTYPE) is refused,
    /// same as the browser build's telnet layer.
    reporting: bool,
    state: State,
    /// Subnegotiation payload being buffered; the first byte is the option.
    sb: Vec<u8>,
    /// Whether the previous data byte was a bare CR, for the CR-NUL
    /// collapse. Cleared on entering a command sequence so a stale flag
    /// cannot swallow an unrelated NUL that follows an IAC boundary (see
    /// [`TelnetNvt::receive`]).
    last_was_cr: bool,
    /// Options the remote WILLed and we accepted (DO in reply).
    remote_on: [bool; 256],
    /// Options we agreed to perform ourselves (WILL in reply to a DO).
    local_on: [bool; 256],
}

impl TelnetNvt {
    /// `reporting` = the WiModem AT*T1 extras: NAWS and location.
    pub(crate) fn new(reporting: bool) -> Self {
        Self {
            reporting,
            state: State::Data,
            sb: Vec::new(),
            last_was_cr: false,
            remote_on: [false; 256],
            local_on: [false; 256],
        }
    }

    /// Options we agree to perform ourselves when the remote sends `IAC DO
    /// x`.
    fn accepts_local(&self, opt: u8) -> bool {
        opt == OPT_BINARY
            || opt == OPT_SGA
            || opt == OPT_TTYPE
            || (self.reporting && (opt == OPT_NAWS || opt == OPT_SEND_LOCATION))
    }

    /// Options we want the remote to perform when it offers `IAC WILL x`.
    fn accepts_remote(&self, opt: u8) -> bool {
        opt == OPT_BINARY || opt == OPT_ECHO || opt == OPT_SGA
    }

    /// One wire byte in; data bytes for the guest onto `data`, negotiation
    /// replies (already escaped) onto `reply`.
    pub(crate) fn receive(&mut self, b: u8, data: &mut Vec<u8>, reply: &mut Vec<u8>) {
        match self.state {
            State::Data => {
                if b == IAC {
                    // CR/NUL collapsing applies only to adjacent data
                    // bytes: a command sequence starting here must not
                    // leave a stale CR flag that would swallow a later,
                    // unrelated NUL.
                    self.last_was_cr = false;
                    self.state = State::Iac;
                } else if self.last_was_cr && b == 0 && !self.remote_on[OPT_BINARY as usize] {
                    // CR NUL is the NVT encoding of a bare CR; the CR
                    // already went through, so the NUL is swallowed.
                    self.last_was_cr = false;
                } else {
                    self.last_was_cr = b == 13;
                    data.push(b);
                }
            }
            State::Iac => {
                if b == IAC {
                    // Doubled IAC is a literal 0xFF data byte.
                    data.push(IAC);
                    self.state = State::Data;
                } else if b == WILL || b == WONT || b == DO || b == DONT {
                    self.state = State::Opt(b);
                } else if b == SB {
                    self.sb.clear();
                    self.state = State::Sb;
                } else {
                    // NOP, GA, AYT, ... - nothing this layer needs to act on.
                    self.state = State::Data;
                }
            }
            State::Opt(command) => {
                self.negotiate(command, b, reply);
                self.state = State::Data;
            }
            State::Sb => {
                if b == IAC {
                    self.state = State::SbIac;
                } else {
                    self.sb.push(b);
                }
            }
            State::SbIac => {
                if b == SE {
                    self.subnegotiate(reply);
                    self.state = State::Data;
                } else {
                    // IAC IAC inside a subnegotiation is a literal 0xFF.
                    self.sb.push(b);
                    self.state = State::Sb;
                }
            }
        }
    }

    fn negotiate(&mut self, command: u8, opt: u8, reply: &mut Vec<u8>) {
        match command {
            DO => {
                // The remote asks us to perform `opt`.
                if self.accepts_local(opt) {
                    if !self.local_on[opt as usize] {
                        self.local_on[opt as usize] = true;
                        reply.extend_from_slice(&[IAC, WILL, opt]);
                        if self.reporting && opt == OPT_NAWS {
                            // Volunteer the window size unasked: a fixed
                            // 80x24 terminal, which is the only size this
                            // emulated modem has.
                            reply.extend_from_slice(&[IAC, SB, OPT_NAWS, 0, 80, 0, 24, IAC, SE]);
                        }
                        if self.reporting && opt == OPT_SEND_LOCATION {
                            // RFC 779: the WILL is followed by the location
                            // subnegotiation itself, unprompted.
                            send_location(reply);
                        }
                    }
                } else {
                    reply.extend_from_slice(&[IAC, WONT, opt]);
                }
            }
            DONT => {
                if self.local_on[opt as usize] {
                    self.local_on[opt as usize] = false;
                    reply.extend_from_slice(&[IAC, WONT, opt]);
                }
            }
            WILL => {
                // The remote offers to perform `opt`.
                if self.accepts_remote(opt) {
                    if !self.remote_on[opt as usize] {
                        self.remote_on[opt as usize] = true;
                        reply.extend_from_slice(&[IAC, DO, opt]);
                    }
                } else {
                    reply.extend_from_slice(&[IAC, DONT, opt]);
                }
            }
            WONT => {
                if self.remote_on[opt as usize] {
                    self.remote_on[opt as usize] = false;
                    reply.extend_from_slice(&[IAC, DONT, opt]);
                }
            }
            _ => unreachable!("State::Opt only ever holds a WILL/WONT/DO/DONT verb"),
        }
    }

    fn subnegotiate(&mut self, reply: &mut Vec<u8>) {
        // TTYPE SEND -> TTYPE IS "ANSI".
        if self.sb.len() >= 2 && self.sb[0] == OPT_TTYPE && self.sb[1] == TTYPE_SEND {
            reply.extend_from_slice(&[IAC, SB, OPT_TTYPE, TTYPE_IS]);
            reply.extend(TERM_TYPE.bytes().map(|c| c & 0x7f));
            reply.extend_from_slice(&[IAC, SE]);
            return;
        }
        // A location request under the simple SEND-LOCATION form (an empty
        // or SEND-style subnegotiation) gets the fixed string back.
        if self.reporting && !self.sb.is_empty() && self.sb[0] == OPT_SEND_LOCATION {
            send_location(reply);
        }
    }

    /// One guest byte out; wire bytes onto `out`.
    pub(crate) fn send(&mut self, b: u8, out: &mut Vec<u8>) {
        if b == IAC {
            out.extend_from_slice(&[IAC, IAC]);
        } else if b == 13 && !self.local_on[OPT_BINARY as usize] {
            out.extend_from_slice(&[13, 0]);
        } else {
            out.push(b);
        }
    }
}

/// `IAC SB LOCATION "Copperline" IAC SE`, the fixed answer under `AT*T1`.
fn send_location(reply: &mut Vec<u8>) {
    reply.extend_from_slice(&[IAC, SB, OPT_SEND_LOCATION]);
    reply.extend(LOCATION.bytes());
    reply.extend_from_slice(&[IAC, SE]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(nvt: &mut TelnetNvt, bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut data = Vec::new();
        let mut reply = Vec::new();
        for &b in bytes {
            nvt.receive(b, &mut data, &mut reply);
        }
        (data, reply)
    }

    fn send_all(nvt: &mut TelnetNvt, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in bytes {
            nvt.send(b, &mut out);
        }
        out
    }

    // ---- Option accept/refuse matrix -----------------------------------

    #[test]
    fn will_binary_echo_sga_are_accepted_with_do() {
        for opt in [OPT_BINARY, OPT_ECHO, OPT_SGA] {
            let mut nvt = TelnetNvt::new(false);
            let (data, reply) = feed(&mut nvt, &[IAC, WILL, opt]);
            assert!(data.is_empty());
            assert_eq!(reply, vec![IAC, DO, opt], "option {opt}");
        }
    }

    #[test]
    fn will_other_options_are_refused_with_dont() {
        let mut nvt = TelnetNvt::new(false);
        let (data, reply) = feed(&mut nvt, &[IAC, WILL, 99]);
        assert!(data.is_empty());
        assert_eq!(reply, vec![IAC, DONT, 99]);
    }

    #[test]
    fn do_binary_sga_ttype_are_accepted_with_will() {
        for opt in [OPT_BINARY, OPT_SGA, OPT_TTYPE] {
            let mut nvt = TelnetNvt::new(false);
            let (data, reply) = feed(&mut nvt, &[IAC, DO, opt]);
            assert!(data.is_empty());
            assert_eq!(reply, vec![IAC, WILL, opt], "option {opt}");
        }
    }

    #[test]
    fn do_other_options_are_refused_with_wont() {
        let mut nvt = TelnetNvt::new(false);
        let (_, reply) = feed(&mut nvt, &[IAC, DO, 99]);
        assert_eq!(reply, vec![IAC, WONT, 99]);
    }

    #[test]
    fn do_echo_is_refused_locally_even_though_will_echo_is_accepted() {
        // ECHO is only ever accepted as something the remote does (WILL),
        // never as something we perform ourselves (DO): a guest terminal
        // has no local echo logic to turn on.
        let mut nvt = TelnetNvt::new(false);
        let (_, reply) = feed(&mut nvt, &[IAC, DO, OPT_ECHO]);
        assert_eq!(reply, vec![IAC, WONT, OPT_ECHO]);
    }

    // ---- No re-ack loop ---------------------------------------------------

    #[test]
    fn an_option_already_on_is_not_re_acked() {
        let mut nvt = TelnetNvt::new(false);
        let (_, reply1) = feed(&mut nvt, &[IAC, WILL, OPT_BINARY]);
        assert_eq!(reply1, vec![IAC, DO, OPT_BINARY]);
        let (_, reply2) = feed(&mut nvt, &[IAC, WILL, OPT_BINARY]);
        assert!(reply2.is_empty(), "re-acked an option already on");
    }

    #[test]
    fn wont_only_answered_when_clearing_an_on_option() {
        let mut nvt = TelnetNvt::new(false);
        // Never turned on: WONT for an option that was never WILLed gets
        // no reply (nothing to clear).
        let (_, reply) = feed(&mut nvt, &[IAC, WONT, OPT_BINARY]);
        assert!(reply.is_empty());

        // Turn it on, then off: the off transition gets DONT.
        feed(&mut nvt, &[IAC, WILL, OPT_BINARY]);
        let (_, reply) = feed(&mut nvt, &[IAC, WONT, OPT_BINARY]);
        assert_eq!(reply, vec![IAC, DONT, OPT_BINARY]);

        // Already off: no second DONT.
        let (_, reply) = feed(&mut nvt, &[IAC, WONT, OPT_BINARY]);
        assert!(reply.is_empty());
    }

    #[test]
    fn dont_only_answered_when_clearing_an_on_option() {
        let mut nvt = TelnetNvt::new(false);
        feed(&mut nvt, &[IAC, DO, OPT_SGA]);
        let (_, reply) = feed(&mut nvt, &[IAC, DONT, OPT_SGA]);
        assert_eq!(reply, vec![IAC, WONT, OPT_SGA]);
        let (_, reply) = feed(&mut nvt, &[IAC, DONT, OPT_SGA]);
        assert!(reply.is_empty());
    }

    // ---- Doubled IAC both directions ---------------------------------------

    #[test]
    fn doubled_iac_inbound_is_a_literal_0xff() {
        let mut nvt = TelnetNvt::new(false);
        let (data, reply) = feed(&mut nvt, &[b'a', IAC, IAC, b'b']);
        assert_eq!(data, vec![b'a', 0xFF, b'b']);
        assert!(reply.is_empty());
    }

    #[test]
    fn iac_doubled_outbound() {
        let mut nvt = TelnetNvt::new(false);
        let out = send_all(&mut nvt, &[b'a', 0xFF, b'b']);
        assert_eq!(out, vec![b'a', IAC, IAC, b'b']);
    }

    #[test]
    fn binary_heavy_blob_round_trips_through_send_and_receive() {
        let blob: Vec<u8> = (0..=255u8).chain(0..=255u8).collect();
        let mut sender = TelnetNvt::new(false);
        // Binary mode both ways, so CR gets no NVT rewriting either.
        feed(&mut sender, &[IAC, WILL, OPT_BINARY]);
        feed(&mut sender, &[IAC, DO, OPT_BINARY]);
        let wire = send_all(&mut sender, &blob);

        let mut receiver = TelnetNvt::new(false);
        feed(&mut receiver, &[IAC, WILL, OPT_BINARY]);
        let (data, _) = feed(&mut receiver, &wire);
        assert_eq!(data, blob);
    }

    // ---- CR NUL collapse ---------------------------------------------------

    #[test]
    fn inbound_cr_nul_collapses_to_cr_in_non_binary_mode() {
        let mut nvt = TelnetNvt::new(false);
        let (data, _) = feed(&mut nvt, &[b'a', 13, 0, b'b']);
        assert_eq!(data, vec![b'a', 13, b'b']);
    }

    #[test]
    fn inbound_cr_nul_does_not_collapse_in_binary_mode() {
        let mut nvt = TelnetNvt::new(false);
        feed(&mut nvt, &[IAC, WILL, OPT_BINARY]);
        let (data, _) = feed(&mut nvt, &[b'a', 13, 0, b'b']);
        assert_eq!(data, vec![b'a', 13, 0, b'b']);
    }

    #[test]
    fn outbound_bare_cr_becomes_cr_nul_in_non_binary_mode() {
        let mut nvt = TelnetNvt::new(false);
        let out = send_all(&mut nvt, &[b'a', 13, b'b']);
        assert_eq!(out, vec![b'a', 13, 0, b'b']);
    }

    #[test]
    fn cr_then_iac_boundary_then_nul_does_not_swallow_the_nul() {
        // A CR immediately followed by an IAC command sequence must not
        // leave a stale "last was CR" flag: the NUL that comes after the
        // command is unrelated data, not part of a CR NUL pair.
        let mut nvt = TelnetNvt::new(false);
        let (data, reply) = feed(&mut nvt, &[13, IAC, WILL, OPT_BINARY, 0, b'x']);
        assert_eq!(data, vec![13, 0, b'x']);
        assert_eq!(reply, vec![IAC, DO, OPT_BINARY]);
    }

    // ---- TTYPE SEND exchange -----------------------------------------------

    #[test]
    fn ttype_send_is_answered_with_ansi() {
        let mut nvt = TelnetNvt::new(false);
        // DO TTYPE first, as a real server would, so local_on is set (not
        // load-bearing for the SB reply itself, but the realistic order).
        feed(&mut nvt, &[IAC, DO, OPT_TTYPE]);
        let (_, reply) = feed(&mut nvt, &[IAC, SB, OPT_TTYPE, TTYPE_SEND, IAC, SE]);
        let mut expected = vec![IAC, SB, OPT_TTYPE, TTYPE_IS];
        expected.extend(TERM_TYPE.bytes());
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(reply, expected);
    }

    // ---- SB IAC IAC literal -------------------------------------------------

    #[test]
    fn subnegotiation_payload_unescapes_doubled_iac() {
        // A payload byte of 0xFF inside a subnegotiation must survive as
        // IAC IAC, and still terminate correctly on the real IAC SE.
        let mut nvt = TelnetNvt::new(false);
        let (_, reply) = feed(
            &mut nvt,
            &[IAC, SB, OPT_TTYPE, TTYPE_SEND, IAC, IAC, IAC, SE],
        );
        // The literal 0xFF in the middle of the payload does not change
        // what TTYPE SEND means (payload[0..2] still matches), so the
        // TTYPE IS reply still fires.
        let mut expected = vec![IAC, SB, OPT_TTYPE, TTYPE_IS];
        expected.extend(TERM_TYPE.bytes());
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(reply, expected);
    }

    // ---- NAWS / location under reporting true vs false ---------------------

    #[test]
    fn naws_and_location_refused_when_reporting_is_false() {
        let mut nvt = TelnetNvt::new(false);
        let (_, reply) = feed(&mut nvt, &[IAC, DO, OPT_NAWS]);
        assert_eq!(reply, vec![IAC, WONT, OPT_NAWS]);
        let (_, reply) = feed(&mut nvt, &[IAC, DO, OPT_SEND_LOCATION]);
        assert_eq!(reply, vec![IAC, WONT, OPT_SEND_LOCATION]);
    }

    #[test]
    fn naws_accepted_and_volunteered_when_reporting_is_true() {
        let mut nvt = TelnetNvt::new(true);
        let (_, reply) = feed(&mut nvt, &[IAC, DO, OPT_NAWS]);
        assert_eq!(
            reply,
            vec![IAC, WILL, OPT_NAWS, IAC, SB, OPT_NAWS, 0, 80, 0, 24, IAC, SE]
        );
    }

    #[test]
    fn send_location_accepted_and_answered_when_reporting_is_true() {
        let mut nvt = TelnetNvt::new(true);
        let (_, reply) = feed(&mut nvt, &[IAC, DO, OPT_SEND_LOCATION]);
        let mut expected = vec![IAC, WILL, OPT_SEND_LOCATION, IAC, SB, OPT_SEND_LOCATION];
        expected.extend(LOCATION.bytes());
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(reply, expected);
    }
}
