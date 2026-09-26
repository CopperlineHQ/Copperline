# Hayes modem emulation

Copperline can put an emulated Hayes-compatible AT command modem on the
Amiga's serial port. The modem carries serial traffic over TCP, so terminal
programs (such as Term, NComm, or JR-Comm) and BBS software can dial Internet
telnet BBSes or accept incoming connections.

The command interpreter supports the standard Hayes AT commands (`ATD`,
`ATA`, `ATH`, `ATO`, `ATE`, `ATQ`, `ATV`, `ATI`, `ATZ`, `AT&F`, `AT&C`,
`AT&D`, `AT&W`, S-registers, and the `+++` escape) and the `AT*` extension set
of the WiModem232.

## Configuration

In the launcher:
1. Navigate to **I/O Ports -> Serial Port**.
2. Set **Device / Mode** to `Modem`.
3. Optionally set a **Listen** address (for incoming calls) and the **Telnet**
   NVT translation toggle.

In `copperline.toml`:

```toml
[serial]
mode = "modem"
# listen = "0.0.0.0:2323"   # Bind port for incoming calls (optional)
# telnet = true             # Enable Telnet NVT translation at power-on
```

From the command line:

```sh
copperline --model A1200 --serial modem KICK31.ROM
```

## Dialing outbound connections

In your Amiga terminal software, use standard AT dialing commands:

- `ATD<hostname>:<port>` or `ATDT<hostname>:<port>` (`ATDP` also works):
  Connects to a TCP endpoint (for example `ATDTbbs.example.com:23`). Without a
  port, the modem's default port is used: 23 (telnet) unless `AT*P` changed it.
- `ATD<number>`: A target of digits (plus `#` and `*`, with `-`, spaces, and
  parentheses ignored) is looked up in the `[serial.phonebook]` table; a number
  that is not there gets `NO CARRIER`. An entry without a port uses the default
  port:

```toml
[serial.phonebook]
"5551234" = "bbs.example.com:23"
"5555678" = "bbs2.example.com"
```

Result codes returned by the modem:
- `CONNECT [baud]`: The TCP connection is established. The rate shown is the
  one set with `AT*B`, or else the guest's serial port rate.
- `BUSY`: The target refused the connection.
- `NO CARRIER`: The connection failed or was dropped.
- `OK` and `ERROR`, as usual. `ATV0` switches to numeric codes and `ATQ1`
  silences them.

Dialing out resolves the host name and connects on the emulation thread
(with a five-second connect timeout), so emulation pauses briefly while the
connection is made.

## Handling incoming calls

To allow inbound connections, specify a listening address with `listen = "0.0.0.0:2323"`
(or via the launcher). Without one, or a port stored with `AT&W`, the modem only
dials out.

- When a remote host connects, the modem sends `RING` to the guest every four seconds.
- Answer manually by sending `ATA`, or configure auto-answer using register `S0` (for
  example `ATS0=1` answers on the first ring).
- If an incoming call is not answered within 10 rings, it is disconnected.
- A second caller who connects while a call is active or still ringing gets a
  busy line: the connection is accepted and closed at once.

## Telnet NVT translation (`AT*T1` / `AT*T0`)

By default the modem passes the raw 8-bit byte stream. For telnet servers that
expect RFC 854 option negotiation:

- `AT*T1` (or `telnet = true` in configuration): Enables telnet NVT negotiation
  for the next call. The modem accepts ECHO, SUPPRESS-GO-AHEAD, and BINARY,
  answers TERMINAL-TYPE with `ANSI`, NAWS with a window size, and
  SEND-LOCATION with `Copperline`, and refuses every other option. It also
  handles `IAC` (`0xFF`) escaping.
- `AT*T0` (default): Raw 8-bit binary transport. Use this mode for BBS systems
  transferring binary files with ZModem, XModem, or raw protocols.

## WiModem232 command extensions

| Command | Description |
|---|---|
| `AT*B<baud>` | Set the rate reported in `CONNECT` responses (a standard rate from 75 to 230400); the serial line itself always runs at the guest's rate |
| `AT*T0` / `AT*T1` | Disable / enable telnet NVT translation |
| `AT*L<port>` | Move the inbound listener to a new port on the same interface (127.0.0.1 unless `listen` named another) |
| `AT*P<port>` | Set the default port for dials that name no port |
| `AT*N` | List wireless networks (one simulated network) |
| `AT*NS<n>,<pass>` | Join a network (accepted and ignored) |
| `AT*REBOOT` | Reset the modem, like `ATZ` |

`AT&W` saves the current settings, including an `AT*L` port, and `ATZ`
reloads them; `AT&F` restores the factory defaults. The stored profile is
`modem.profile` in the NVRAM folder (`[paths] nvram`). Keys set explicitly in
`[serial]` take precedence over the stored profile at power-on, and
`--factory` neither reads nor writes it.

## S-registers

| Register | Function | Default |
|---|---|---|
| `S0` | Rings before auto-answer (`0` = disabled) | `0` |
| `S1` | Ring counter | `0` |
| `S2` | Escape character code (`+`) | `43` |
| `S3` | Command-line carriage return character | `13` |
| `S4` | Linefeed character | `10` |
| `S9` | Delay after `CONNECT` before remote data reaches the guest, in tenths of a second of host time (WiModem compatibility) | `0` |
| `S12` | Escape sequence guard time in 1/50th seconds | `50` (1.0s) |

Other S-registers read as `0` and ignore writes.

## RS-232 control lines

- **DTR (`AT&D`):** Under `AT&D2` (default) and `AT&D3`, dropping DTR (for example
  by closing the terminal software or resetting the serial port) hangs up the
  active call. `AT&D0` and `AT&D1` are accepted but not modelled.
- **DCD (`AT&C`):** Carrier Detect follows the call: it is asserted while a
  connection is up (including after a `+++` escape to command mode) and drops
  when the call ends. `AT&C0` forces DCD permanently high.
- **CTS / DSR:** Permanently asserted. RTS is accepted but there is no hardware
  flow control.
- **RI:** There is no Ring Indicator line; incoming calls are announced by the
  `RING` result code.

(scripted-sessions)=
## Scripted session replay

For deterministic automated testing, CI, or demo playback, a scripted session
file can stand in for the network, so a headless run that dials out is
reproducible byte for byte:

```toml
[serial]
mode = "modem"
session = "session.txt"
```

`--serial-session FILE` does the same from the command line (and implies
`--serial modem`). A scripted session only dials out, so it cannot be combined
with `listen`. Rewinding or loading an earlier save state restarts the script
from the top.

Directives in a session file, one per line (blank lines and lines starting
with `#` are ignored):

```text
# Session script example
accept
delay 0.5
send \r\nWelcome to the BBS\r\n
expect BYE\r
send \r\nGoodbye\r\n
close
```

| Directive | Description |
|---|---|
| `accept` | The next `ATD` succeeds (`CONNECT`) |
| `refuse [busy\|unreachable]` | The next `ATD` fails with `BUSY` or, by default, `NO CARRIER` |
| `delay SECS` | Hold the next action back for this many emulated seconds |
| `send TEXT` | Send text to the guest (`\r`, `\n`, `\t`, and `\\` escapes) |
| `expect TEXT` | Wait for the guest to send this text; a mismatch drops the call |
| `close` | The far end hangs up |

Each `accept` or `refuse` covers one call. A dial with none queued gets
`NO CARRIER`, and a script that runs out mid-call leaves the line up with
nothing more arriving.
