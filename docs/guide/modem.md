# Hayes modem emulation

Copperline provides an emulated Hayes-compatible AT command modem connected
to the Amiga's serial port. The modem bridges Amiga serial communication to
TCP connections, enabling terminal programs (such as Term, NComm, or JR-Comm)
and BBS software to dial Internet telnet BBSes or accept incoming connections.

The modem command interpreter supports standard Hayes AT commands as well
as the `AT*` extension set popularized by the WiModem232.

## Configuration

In the launcher:
1. Navigate to **I/O Ports -> Serial Port**.
2. Set **Device / Mode** to `Modem`.
3. Optionally configure a **Listen** address (for incoming calls) and the **Telnet** NVT translation toggle.

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

- `ATD<hostname>:<port>` or `ATDT<hostname>:<port>`: Connects to a TCP endpoint
  (e.g., `ATDTbbs.example.com:23`). If no port is specified, port 23 (telnet) is used.
- `ATD<digits>`: Looks up numeric numbers in the `[serial.phonebook]` configuration table:

```toml
[serial.phonebook]
"5551234" = "bbs.example.com:23"
"5555678" = "bbs2.example.com"
```

Result codes returned by the modem:
- `CONNECT [baud]`: Successful TCP connection established.
- `BUSY`: Target refused connection.
- `NO CARRIER`: Connection failed or disconnected.

Outbound dialing performs DNS resolution and TCP connection synchronously on the
emulation thread (with a five-second timeout once resolved), which causes a brief
pause in emulation while the connection is established.

## Handling incoming calls

To allow inbound connections, specify a listening address with `listen = "0.0.0.0:2323"`
(or via the launcher).

- When a remote host connects, the modem emits `RING` on the serial port every four seconds.
- Answer manually by sending `ATA`, or configure auto-answer using register `S0` (e.g.,
  `ATS0=1` answers on the first ring).
- If an incoming call is not answered within 10 rings, it is disconnected.
- If a second connection arrives while a call is active, it is immediately rejected with a busy state.

## Telnet NVT translation (`AT*T1` / `AT*T0`)

Raw TCP connections pass 8-bit byte streams directly. For compatibility with telnet
servers requiring RFC 854 option negotiation:

- `AT*T1` (or `telnet = true` in configuration): Enables Telnet NVT negotiation.
  The modem responds to ECHO, SUPPRESS-GO-AHEAD, BINARY, TERMINAL-TYPE (`"ANSI"`),
  and NAWS (window size). It also handles `IAC` escaping (`0xFF`).
- `AT*T0` (Default): Raw 8-bit binary transport. Use this mode for BBS systems
  transferring binary files with ZModem, XModem, or raw protocols.

## WiModem232 command extensions

| Command | Description |
|---|---|
| `AT*B<baud>` | Set reported baud rate string returned in `CONNECT` responses |
| `AT*T0` / `AT*T1` | Disable / enable Telnet NVT translation mode |
| `AT*L<port>` | Rebind inbound listener to a new port on the current interface |
| `AT*P<port>` | Set default destination port for hostname-only dials |
| `AT*N` | List simulated wireless networks |
| `AT*NS<n>,<pass>` | Simulated network join command |
| `AT*REBOOT` | Reset modem state machine (equivalent to `ATZ`) |

Settings modified via AT commands can be saved to persistent NVRAM using `AT&W`
and recalled with `ATZ`. Factory defaults can be restored with `AT&F`.

## S-registers

| Register | Function | Default |
|---|---|---|
| `S0` | Rings before auto-answer (`0` = disabled) | `0` |
| `S1` | Ring counter | `0` |
| `S2` | Escape character code (`+`) | `43` |
| `S3` | Command-line carriage return character | `13` |
| `S4` | Linefeed character | `10` |
| `S9` | Connect delay in tenths of a second (WiModem compatibility) | `0` |
| `S12` | Escape sequence guard time in 1/50th seconds | `50` (1.0s) |

Unimplemented S-registers read as `0` and ignore writes.

## RS-232 control lines

- **DTR (`AT&D`):** Under `AT&D2` (default) and `AT&D3`, dropping DTR (e.g., closing
  the terminal software or resetting the serial port) hangs up the active TCP connection.
- **DCD (`AT&C`):** Carrier Detect asserts when a call is connected and drops in command
  mode. `AT&C0` forces DCD permanently high.
- **CTS / DSR:** Permanently asserted.
- **RI:** Ring Indicator is signaled via serial `RING` text result codes.

(scripted-sessions)=
## Scripted session replay

For deterministic automated testing, CI, or demo playback, network calls can be
replaced with scripted session recordings:

```toml
[serial]
mode = "modem"
session = "session.txt"
```

Directives in a session file:

```text
# Session script example
accept
delay 0.5
send \r\nWelcome to the BBS\r\n
expect BYE\r
send \r\nNO CARRIER\r\n
close
```

| Directive | Description |
|---|---|
| `accept` | The next `ATD` succeeds (`CONNECT`) |
| `refuse [busy\|unreachable]` | The next `ATD` fails with `BUSY` or `NO CARRIER` |
| `delay SECS` | Wait specified emulated seconds before next action |
| `send TEXT` | Transmit text to guest (`\r`, `\n`, `\t` supported) |
| `expect TEXT` | Wait for expected guest text (mismatch drops connection) |
| `close` | Remote hangup |
