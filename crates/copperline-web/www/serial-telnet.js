// Telnet NVT (RFC 854) layer for the browser serial bridge: the thin
// protocol between a raw byte stream (the emulated Amiga's serial port) and
// a telnet server (a BBS) reached over a WebSocket-to-TCP gateway.
//
// The guest side is a plain terminal program talking to serial.device, so it
// knows nothing of IAC negotiation; this module answers the server's option
// negotiation, unescapes inbound data, and escapes outbound data. It
// deliberately implements the minimal subset a BBS session needs:
//
// - ECHO (1) and SUPPRESS-GO-AHEAD (3): accepted from the server, the normal
//   character-at-a-time BBS mode.
// - BINARY (0): accepted both ways, which telnet-aware BBSes negotiate
//   before ZModem transfers so nothing rewrites the stream.
// - TERMINAL-TYPE (24): answered with a fixed name ("ANSI" by default),
//   which BBS menus use to pick their art/charset.
// - Everything else is refused (WONT/DONT), which every server must accept.
//
// Data transforms: IAC (0xFF) bytes are doubled outbound and undoubled
// inbound; in non-binary mode a bare outbound CR becomes CR NUL (the RFC
// form, servers strip the NUL) and an inbound CR NUL becomes CR.
//
// Usage:
//   const telnet = new TelnetSession();
//   ws.onmessage: const { data, reply } = telnet.receive(bytes);
//                 if (reply.length) ws.send(reply);   // negotiation answers
//                 feed data to the guest;
//   guest output: ws.send(telnet.send(bytes));

const IAC = 255;
const DONT = 254;
const DO = 253;
const WONT = 252;
const WILL = 251;
const SB = 250;
const SE = 240;

const OPT_BINARY = 0;
const OPT_ECHO = 1;
const OPT_SGA = 3;
const OPT_TTYPE = 24;

const TTYPE_IS = 0;
const TTYPE_SEND = 1;

export class TelnetSession {
  constructor({ termType = 'ANSI' } = {}) {
    this.termType = termType;
    // Parser state: 'data', 'iac', 'opt' (after WILL/WONT/DO/DONT),
    // 'sb' (inside a subnegotiation), 'sb-iac' (IAC seen inside one).
    this.state = 'data';
    this.command = 0; // the pending WILL/WONT/DO/DONT verb
    this.sb = []; // subnegotiation payload, first byte is the option
    this.lastWasCr = false;
    // Negotiated option state. Tracking it (rather than blindly acking)
    // is what prevents the ack loops RFC 854 warns about: an option
    // already in the requested state is not re-acknowledged.
    this.remoteOn = new Set(); // options the server WILLed and we accepted
    this.localOn = new Set(); // options we agreed to perform (DO -> WILL)
  }

  // Options we agree to perform ourselves when the server sends IAC DO x.
  acceptsLocal(opt) {
    return opt === OPT_BINARY || opt === OPT_SGA || opt === OPT_TTYPE;
  }

  // Options we want the server to perform when it offers IAC WILL x.
  acceptsRemote(opt) {
    return opt === OPT_BINARY || opt === OPT_ECHO || opt === OPT_SGA;
  }

  /**
   * Feed bytes received from the socket. Returns { data, reply }: `data`
   * is the application byte stream for the guest, `reply` is negotiation
   * output that must be sent back over the socket (already escaped).
   */
  receive(bytes) {
    const data = [];
    const reply = [];
    for (const b of bytes) {
      switch (this.state) {
        case 'data':
          if (b === IAC) {
            // CR/NUL collapsing applies only to adjacent data bytes: a
            // command sequence starting here must not leave a stale CR
            // flag that would swallow a later, unrelated NUL.
            this.lastWasCr = false;
            this.state = 'iac';
          } else if (this.lastWasCr && b === 0 && !this.remoteOn.has(OPT_BINARY)) {
            // CR NUL is the NVT encoding of a bare CR; the CR already went
            // through, so the NUL is swallowed.
            this.lastWasCr = false;
          } else {
            this.lastWasCr = b === 13;
            data.push(b);
          }
          break;
        case 'iac':
          if (b === IAC) {
            // Doubled IAC is a literal 0xFF data byte.
            data.push(IAC);
            this.state = 'data';
          } else if (b === WILL || b === WONT || b === DO || b === DONT) {
            this.command = b;
            this.state = 'opt';
          } else if (b === SB) {
            this.sb = [];
            this.state = 'sb';
          } else {
            // NOP, GA, AYT, ... - nothing a BBS bridge needs to act on.
            this.state = 'data';
          }
          break;
        case 'opt':
          this.negotiate(this.command, b, reply);
          this.state = 'data';
          break;
        case 'sb':
          if (b === IAC) this.state = 'sb-iac';
          else this.sb.push(b);
          break;
        case 'sb-iac':
          if (b === SE) {
            this.subnegotiate(this.sb, reply);
            this.state = 'data';
          } else {
            // IAC IAC inside a subnegotiation is a literal 0xFF.
            this.sb.push(b);
            this.state = 'sb';
          }
          break;
      }
    }
    return { data: Uint8Array.from(data), reply: Uint8Array.from(reply) };
  }

  negotiate(command, opt, reply) {
    switch (command) {
      case DO: // the server asks us to perform `opt`
        if (this.acceptsLocal(opt)) {
          if (!this.localOn.has(opt)) {
            this.localOn.add(opt);
            reply.push(IAC, WILL, opt);
          }
        } else {
          reply.push(IAC, WONT, opt);
        }
        break;
      case DONT:
        if (this.localOn.delete(opt)) reply.push(IAC, WONT, opt);
        break;
      case WILL: // the server offers to perform `opt`
        if (this.acceptsRemote(opt)) {
          if (!this.remoteOn.has(opt)) {
            this.remoteOn.add(opt);
            reply.push(IAC, DO, opt);
          }
        } else {
          reply.push(IAC, DONT, opt);
        }
        break;
      case WONT:
        if (this.remoteOn.delete(opt)) reply.push(IAC, DONT, opt);
        break;
    }
  }

  subnegotiate(sb, reply) {
    // The only subnegotiation we volunteered for: TTYPE SEND -> TTYPE IS.
    if (sb.length >= 2 && sb[0] === OPT_TTYPE && sb[1] === TTYPE_SEND) {
      reply.push(IAC, SB, OPT_TTYPE, TTYPE_IS);
      for (const c of this.termType) reply.push(c.charCodeAt(0) & 0x7f);
      reply.push(IAC, SE);
    }
  }

  /**
   * Escape guest output for the wire: double IAC bytes, and in non-binary
   * mode send a bare CR as CR NUL (its NVT encoding).
   */
  send(bytes) {
    const out = [];
    for (const b of bytes) {
      if (b === IAC) {
        out.push(IAC, IAC);
      } else if (b === 13 && !this.localOn.has(OPT_BINARY)) {
        out.push(13, 0);
      } else {
        out.push(b);
      }
    }
    return Uint8Array.from(out);
  }
}
