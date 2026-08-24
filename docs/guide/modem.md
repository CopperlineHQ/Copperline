# The Modem

Copperline can put a Hayes-compatible AT-command modem on the other end of
the Amiga's serial port: `ATD` dials out, `RING`/`ATA` answer incoming
calls, and `+++` drops to command mode mid-call, same as the real thing.
The far end is always a TCP connection -- there is no telephone network
under emulation, so "dialling" a `host:port` (or a phonebook number that
resolves to one) is what stands in for the phone line. This is what makes
old terminal software, BBS doors, and dialing directories work unmodified:
Term, NComm, and friends see an ordinary modem and never need to know the
call is actually a socket.

The command set also implements CBMSTUFF's WiModem232 `AT*` extensions,
the de facto standard for retro serial-to-telnet modems and what most
Amiga BBS/terminal setup guides on the web are written against. The goal
is that **those guides run verbatim** against this modem too.

## Turning it on

In the launcher, the **I/O Ports** tab (its Serial Port page) sets
**Device / Mode** to `Modem`, which reveals a **Listen** address (incoming
calls, see below) and a **Telnet** toggle. On the command line,
`--serial modem` selects it directly. In a configuration file:

```toml
[serial]
mode = "modem"
# listen = "0.0.0.0:2323"   # answer incoming calls here (optional)
# telnet = true             # AT*T1 telnet NVT translation at power-on
```

With the port in modem mode, boot a terminal program on the guest side
(Term, NComm, JR-Comm, or `AUX:` from the shell for the least fuss) and
talk AT commands to it. `ATZ` (or just watching for the `OK` after
`AT&F` prints) confirms the emulated modem answers before dialing anything.

## Dialing out

`ATD<host>[:<port>]` or `ATDT<host>[:<port>]` connects to a TCP service --
a telnet BBS, a `tcpser` bridge, another Copperline instance also in
`modem` mode, or `nc -l` for a quick loopback test. No port means 23
(telnet) unless `AT*P` changed the default (see below). A successful
connect prints `CONNECT` (with the baud rate appended, from `AT*B` if set,
otherwise whatever the guest's own SERPER last showed); a refused
connection is `BUSY`; anything else that fails to connect is `NO CARRIER`.

`ATD<digits>` -- an all-digit target with no `.`/`:` in it -- is a "phone
number" instead, looked up in `[serial.phonebook]`:

```toml
[serial.phonebook]
"5551234" = "bbs.example.com:23"
"5555678" = "bbs2.example.com"   # no port: AT*P's default is appended
```

so a terminal program's own dialing directory, configured with a plain
phone number the way its manual describes, resolves to a real address
without the guest ever typing one. A number with no phonebook entry is
`NO CARRIER` -- there is nothing to fall back to. The phonebook is
config-file only; there is no launcher row for it.

Dialling still blocks the calling thread for up to five seconds while the
TCP connect resolves (see [Notes](#modem-notes) below) -- the same wait a real
modem's own dial tone and handshake would cost, just spent in a `connect()`
call instead.

## Incoming calls

`[serial] listen` (or the launcher's **Listen** row) binds an address for
inbound calls -- unlike `tcp` mode, an incoming connection there does not
bridge straight through. It produces `RING` on the guest's serial port
every four seconds, counted in S1, exactly as a real modem waits for the
terminal program (or a BBS door) to pick up: `ATA` answers manually, or set
`S0` to a nonzero ring count for auto-answer (`ATS0=1` answers on the first
ring, the usual BBS-door setting). A caller nobody answers within ten rings
is dropped, the exchange giving up the way a real line eventually would.
While a call is up, a second incoming connection gets an immediate
fast-busy close rather than a ring -- the phone-line-busy behaviour a
single-line modem has no way around.

`AT*L<port>` rebinds the listen address live to a new port on the same
host interface, and `AT&W` persists it (see below) so the next session
answers there again without the config file needing an update.

## The telnet toggle

The wire is raw TCP by default: fine for `nc`, `tcpser`, or any byte
service, but a real telnet server expects RFC 854 option negotiation
first and treats a bare `\r` as needing a following NUL or `\n`. `AT*T1`
(or `[serial] telnet = true` for the power-on default) turns on a minimal
NVT layer for the current call: it answers ECHO/SUPPRESS-GO-AHEAD/BINARY
and TERMINAL-TYPE (reporting `"ANSI"`) affirmatively, refuses everything
else, escapes/unescapes `IAC` bytes both directions, and folds a bare `CR`
into `CR NUL` outbound (undoing it inbound) once BINARY is negotiated on.
With `AT*T1` on, it also volunteers window size (`NAWS`) and offers
`SEND-LOCATION` if asked, the extra reporting the manual documents `AT*T1`
as implying. `AT*T0` (the default) leaves every byte untouched -- use it
for a raw BBS door, `tcpser`, or anything else that is not a real telnet
server; turning translation on against a raw byte service makes `IAC`
(`0xFF`) bytes in binary transfers get escaped, which is not what a
protocol like ZModem wants to see.

## WiModem232 compatibility

The Hayes base is extended with the WiModem232 `AT*` command set so a
guide written for real WiModem232 hardware runs unchanged:

| Command | Effect |
|---|---|
| `AT*B<baud>` | Stores a baud value to quote in `CONNECT` text. Real hardware would retarget its own UART; there is nothing to retarget here -- Paula's SERPER always governs the wire, so this affects only what `CONNECT` prints. |
| `AT*T0` / `AT*T1` | Telnet NVT translation for the next call (see above). |
| `AT*L<port>` | Rebind the inbound-call listener to a new port on the same host address. |
| `AT*P<port>` | The port `ATD`/`ATDT` (and a portless phonebook entry) append to a bare host from now on. |
| `AT*N` | Lists "networks in range" -- one synthetic entry, in the manual's own format, so a guide's Wi-Fi setup step sees plausible output instead of an empty list. |
| `AT*NS<n>,<passphrase>` | "Join" network `n`. Validates the syntax and succeeds; there is no real Wi-Fi to join under emulation. |
| `AT*REBOOT` | Resets the state machine, exactly like `ATZ` (reload the stored profile, drop any call). |

`S9` is the WiModem connect-delay register, in tenths of a second of *host*
time: `CONNECT` still fires the instant the TCP connection succeeds, but
remote-to-guest output is withheld until `S9` has elapsed, the pause
dialer-era terminal software (Terminate and its contemporaries) expects
before BBS output starts. It is measured in host time deliberately -- see
[Notes](#modem-notes).

`AT&W` persists the modem's active settings (echo/verbose/quiet, the
S-registers below, `&C`/`&D`, telnet, the listen port, and the default
port) to a sidecar file kept beside Copperline's other emulated batteries
-- the same pattern as Coppersynth's NVRAM. `ATZ` reloads it; an explicit
`[serial] telnet` in the config always wins over whatever was stored, the
same way a config value beats a battery-backed default everywhere else in
Copperline. `AT&F` is a hardcoded factory reset that ignores both the
config override and the stored profile.

## S-registers

| Register | Meaning | Default |
|---|---|---|
| `S0` | Rings to wait through before auto-answering; `0` disables auto-answer | `0` |
| `S1` | Rings answered since the last call (read-only in practice; decays to 0 eight seconds after the last ring) | `0` |
| `S2` | The `+++` escape character | `43` (`+`) |
| `S3` | Command-line terminator | `13` (CR) |
| `S4` | Response linefeed character (paired with `S3` as every result code's `<CR><LF>` framing; numeric-mode responses use `S3` alone) | `10` (LF) |
| `S9` | WiModem connect-delay, tenths of a second of host time | `0` |
| `S12` | `+++` escape guard time, fiftieths of a second | `50` (one second) |

Every other register reads `0` and accepts a write with no effect --
stubbed honestly rather than erroring, since real modems carry dozens that
almost no software actually probes.

## DTR and DCD

`AT&D2` (the default) and `AT&D3` hang up the call on a DTR true-to-false
transition -- closing a terminal program's window, or the guest resetting
the serial port, drops the line the way pulling the RS-232 cable would.
`AT&D0`/`AT&D1` are accepted but do not model DTR at all, matching the
manual's own note that a modem ignoring DTR is a valid (if old-fashioned)
mode.

`/CD` (carrier detect) tracks whether a call is up: asserted while
connected, deasserted in command mode with no call, or with `AT&C0`
("DCD always"), asserted unconditionally as a stub for the (rare) software
that gets confused by a modem that ever deasserts it. `/DSR` and `/CTS`
are always asserted -- there is no hardware flow control or "no terminal
attached" state modeled. `RI` (ring indicator) is not wired to anything:
real Amiga serial hardware never wired the connector's RING pin to a
CPU-readable input either, so `RING` is the result-code text (and `S1`)
doing the whole job, exactly as it does on real Amiga serial ports.

## Scripted sessions

Live network traffic -- like MIDI, like a camera -- is outside Copperline's
replay guarantees: two runs against a real telnet BBS are not going to
produce the same bytes back. For a modem-using test case, a demo recording,
or any headless run that has to be byte-for-byte reproducible, `[serial]
session = "path"` (or `--serial-session PATH`, which implies `--serial
modem`) replaces the TCP transport with a scripted one that replays a
canned dial-out from a file instead of touching a socket at all.

A session file is a line-oriented list of directives, executed in order
against one simulated call:

```text
# lines starting with # (and blank lines) are ignored
accept
delay 0.5
send \r\nWelcome to the BBS\r\n
expect BYE\r
send \r\nNO CARRIER\r\n
close
```

| Directive | Meaning |
|---|---|
| `accept` | The guest's next `ATD`/`ATDT` succeeds (`CONNECT`) |
| `refuse [busy\|unreachable]` | ...or fails instead: `busy` for `BUSY`, `unreachable` (the default with no argument) for `NO CARRIER` |
| `delay SECS` | Hold the next `send` back until `SECS` of *emulated* time have passed since the call connected (or since the previous `delay`) |
| `send TEXT` | Bytes the far end emits to the guest |
| `expect TEXT` | Bytes the guest must send next; anything else logs a mismatch and drops the line (`NO CARRIER`), same as a call going wrong for real |
| `close` | The far end hangs up here |

`send`/`expect` text runs to the end of the line, with `\r`, `\n`, `\t`,
and `\\` recognized as escapes -- enough for terminal-program dialogue and
BBS banners; there is no framing for binary payloads. `accept`/`refuse`
are each consumed by one `ATD`; dialing again once the script has none left
queued reports `NO CARRIER` and logs why, rather than hanging the session
-- the same honesty a mismatch gets. Running out of directives mid-call
(the far end simply stops talking) is not an error: the line just stays up
with nothing more happening, the way a quiet BBS would leave it.

`delay` is measured against the same emulated clock `S12`'s guard time
uses, not host time, which is what makes a scripted run's output identical
run to run regardless of host scheduling. The scripted transport has no
inbound side: it plays back a fixed sequence of outbound calls only, so
`[serial] listen` (and a guest's own `AT*L`) cannot be combined with
`session` -- Copperline refuses the configuration outright rather than
silently answering nothing.

(modem-notes)=
## Notes

- **No real network under emulation.** Dialing a `host:port` is a TCP
  connection Copperline itself makes; there is no phone system, no dial
  tone, and no long-distance charges to simulate.
- **Pacing follows the guest's own SERPER.** The modem never paces bytes
  itself -- Paula's UART shifts them at whatever rate the guest programmed,
  same as every other serial mode. `AT*B` only changes what `CONNECT`
  prints.
- **`ATD` still blocks for up to five seconds** against the real TCP
  transport while the connection attempt resolves, holding up the whole
  emulation thread for that span. A background (non-blocking) dial is
  possible future work; the scripted-session transport above sidesteps
  the whole question, since there is no socket underneath it to block on.
- **No real Wi-Fi.** The WiModem232 `AT*N`/`AT*NS` commands are stubs that
  keep a guide's setup script from erroring out; they do not scan or join
  anything.
