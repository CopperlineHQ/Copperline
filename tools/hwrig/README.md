# Hardware reference rig

Host tooling for driving a real Amiga as a measurement instrument, so that
timing questions Copperline currently settles by arbitrating between vAmiga and
FS-UAE can be settled against actual silicon instead. Design rationale and
phasing: `HARDWARE-RIG-PLAN.md` in the repository root.

Contents:

| Path | What it is |
|---|---|
| `hwrig.py` | Host harness. Talks to the probe server over TCP (Copperline) or a UART (real machine), and to the control MCU. |
| `hwrig-mcu/hwrig-mcu.ino` | Arduino Uno firmware: Amiga keyboard injection, reset, cold-boot relay. |
| `../../timing-test/probesrv.asm` | The probe server that runs on the Amiga. Built into a bootable ADF. |

## How the pieces fit

```
  host
   |
   +-- UART  ---> Amiga serial (DB25)      probe upload + results
   +-- USB   ---> Arduino Uno ---> keyboard connector   typing + reset
   |                           \-> relay               cold boot
   +-- capture <- RGBtoHDMI <- Amiga RGB   raster probes
```

The Amiga boots `probesrv.adf` from a Gotek once and then stays up. Probes are
uploaded over the serial link and run on demand, so iterating on a probe is
`vasm && upload && read numbers` -- seconds, no media handling, no human. The
MCU exists for the two things that link cannot do: type at software that is not
the probe server, and recover a machine a probe has wedged.

## Part 1: the probe server ADF

Build it like any other probe:

```sh
cd timing-test
VASM=/path/to/vasmm68k_mot ./build.sh probesrv
```

Write `probesrv.adf` to a Gotek image (or a real floppy) and boot it. The screen
goes red while the server starts and green once it is serving; there is no
display output beyond that, because the server turns all DMA off and leaves the
machine in the state a probe expects.

### Memory contract

```
  $00000-$2FFFF  free for probes
  $30000-$6FFFF  free for probes -- the conventional load address is $30000
  $70000-$7FFFF  RESERVED for the server (code, variables, stacks)
```

The committed probes (`test.bin`, `ddfprobe-*.bin`, `clxprobe.bin`, ...) upload
and run byte-for-byte unmodified: they still load at $30000 and still use
SCREEN $40000 / RESULTS $48000 / scratch $60000. A probe that writes into
$70000-$7FFFF destroys the server, and the recovery is an MCU reset. That is
expected, not exceptional.

The server hands a probe **unprivileged mode at IPL 0** -- exactly what a boot
block is entered with. This matters: an early version took supervisor mode with
IPL 7 for its own convenience and `test.bin` rows 19/20 silently read `0000`
instead of `003D`/`0147`, because no interrupt can reach a CPU masked at level
7. The server keeps its own supervisor stack (relocated at startup via a TRAP
so exceptions cannot land in its own code) but never runs the probe in it.

### Wire protocol

Line oriented ASCII, all numbers hex without a `0x` prefix. **Terminate commands
with a bare LF, not CRLF**: the server's line reader stops on the first
terminator, and a trailing LF left in the receiver becomes the first raw byte of
a following `LOAD` payload -- which shows up as a CRC failure that looks exactly
like line noise. (The server also peeks and swallows a stray line ending, so an
interactive terminal works, but do not rely on it.)

```
  -> ID                        <- BANNER cl-probe 1 agnus=... 
  -> PING                      <- READY
  -> LOAD <addr> <len> <crc16> <- LOADRDY, then send <len> raw bytes,
                                  then LOADOK, or LOADERR crc=<computed>
  -> RUN <addr>                <- BEGIN, then whatever the probe emits
```

`crc16` is CRC-16/XMODEM (poly 0x1021, init 0, no reflection, no final xor;
check value of `"123456789"` is 0x31C3). On a mismatch the server reports the
CRC it computed: a wrong-but-stable value means the implementations disagree, a
value that changes between retries means bytes are being lost on the link.

A probe that returns via RTS lands back in the command loop and the server
prints `READY`. The committed probes do **not** return -- they end in an
infinite display loop -- so for those the host collects output until it times
out and then resets. Both are normal.

Everything in the banner is raw, and is interpreted host-side by `hwrig.py`, so
the mapping from ID fields to part numbers lives in one place:

```
BANNER cl-probe 1 agnus=0020 denise=FFFF cpu=0000 chipkb=0200 reachkb=0400 lines=0139 serper=00B7
```

Note `chipkb` and `reachkb` are different facts and are easy to conflate.
`reachkb` is where the chip window starts mirroring, i.e. how far Agnus decodes;
`chipkb` is how much RAM is actually fitted. A 512K A500 with an 8372A reports
`chipkb=0200 reachkb=0400`, and a mirror test alone would wrongly call it a 1M
machine.

### Serial rate

`SERPER_V` in `probesrv.asm` sets the rate; the value in use is reported in the
banner and both ends must agree. The default 183 gives 19200 baud (+0.4% PAL,
+1.3% NTSC). The comment block there lists 38400 and 115200 divisors.

Note that `test.asm` sets `SERPER` to 9600 itself before streaming its results,
as several probes do. Over Copperline's TCP bridge that is invisible, but **on
real hardware the host must follow the probe's rate** for the duration of the
run, or switch the server to match. Check what a probe does to SERPER before
running it on iron.

## Part 2: the control MCU

### Why an Uno and not an RP2040

KCLK, KDAT and /RESET are open-collector lines held at +5V by pull-ups on the
Amiga side, so they idle high at 5V. The RP2040 is not 5V tolerant: even with a
pin in hi-Z it would clamp 5V into its 3.3V supply through the input protection
diodes. Use a natively 5V part -- ATmega328P at 5V (Uno, Nano, Pro Mini
5V/16MHz), or ATmega32U4 at 5V (Leonardo, Pro Micro **5V/16MHz**, not the
3.3V/8MHz variant) if native USB is wanted. The protocol is 20us-scale with
143ms timeouts, so a 16MHz AVR has enormous margin; there is no timing argument
for a faster part.

If a 3.3V MCU is ever preferred anyway, all three lines need bidirectional level
shifting; a BSS138-style FET shifter is the right topology for open-drain lines
with pull-ups. That is three more parts on the critical recovery path, which is
an argument for just using the 5V one.

### Wiring

Four signals, plus an optional relay:

| Uno pin | Signal | Notes |
|---|---|---|
| D2 | KCLK | keyboard clock, driven by the keyboard |
| D3 | KDAT | keyboard data, released for the Amiga's acknowledge |
| D4 | /RESET | open drain, active low, never driven high |
| D5 | relay control | optional, for cold boot |
| GND | GND | **required** -- common ground with the Amiga |

**Ground and power.** Tie the Uno's GND to the Amiga's keyboard-connector
ground. If the Uno is USB powered (the normal case, since the host needs the
serial link anyway) leave the Amiga's +5V **unconnected** -- do not tie two
supplies together. Only take +5V from the keyboard connector if the Uno is not
USB powered, and then it cannot be the host's serial port either.

**Connector pinout.** Take the pin numbers from your own adapter or the
machine's schematic rather than from this file: the A500 uses an internal header
whose numbering differs between board revisions, while the big-box machines
(A2000/A3000/A4000) use an external 5-pin DIN conventionally wired
1=KCLK, 2=KDAT, 3=/RESET, 4=GND, 5=+5V. The A600/A1200 have an integrated
membrane and are the awkward case. Verify with a meter before powering
anything.

**Drive discipline.** KCLK and KDAT are actively driven while the keyboard is
transmitting, and only KDAT is released for the acknowledge pulse -- this is
what a real keyboard MCU does and what the firmware implements. /RESET is
different: it is a wired-OR line, so the firmware only ever pulls it low
(`pinMode(OUTPUT)` + `LOW`) or releases it (`pinMode(INPUT)`), never drives it
high.

Everything is released in `setup()`, so a power-on or watchdog reset of the MCU
can never assert /RESET or hold a keyboard line down. The reset pulse is bounded
in firmware and auto-releases, so a host that dies mid-command cannot leave the
Amiga held in reset.

### Flashing

Open `hwrig-mcu/hwrig-mcu.ino` in the Arduino IDE, select Arduino Uno, upload.
Or with `arduino-cli`:

```sh
arduino-cli compile --fqbn arduino:avr:uno tools/hwrig/hwrig-mcu
arduino-cli upload  --fqbn arduino:avr:uno -p /dev/tty.usbmodemXXXX tools/hwrig/hwrig-mcu
```

The keyboard protocol (bit order, inversion, handshake) follows the
maintainer's own `A500KBFirmware` rather than a reading of the HRM, so it
matches hardware known to work on these machines.

### MCU commands

19200 baud, LF-terminated, one reply line each.

| Command | Effect |
|---|---|
| `ID` / `PING` | identify / liveness |
| `SYNC` | run the keyboard power-on handshake |
| `KEY <hex>` | press and release one raw Amiga keycode |
| `DOWN <hex>` / `UP <hex>` | press or release separately |
| `RESET` | assert /RESET for 500ms |
| `CAA` | keyboard-initiated reset (KCLK low 500ms, the Ctrl-A-A path) |
| `POWER` | cold boot: relay off 3s, on |

Keycodes are raw Amiga codes; bit 7 is the up/down flag and the firmware sets
it, so pass the base code. The link resyncs automatically after a reset.

## Part 3: the host harness

```sh
# against Copperline, no hardware at all
./target/release/copperline --model A500 --chip 512K --slow 512K --noaudio \
    --serial tcp --insert-disk-after 0 df0 timing-test/probesrv.adf \
    --screenshot-after 100000 /tmp/x.png &
tools/hwrig/hwrig.py --tcp 127.0.0.1:1234 id
tools/hwrig/hwrig.py --tcp 127.0.0.1:1234 run timing-test/test.bin

# against the real machine
tools/hwrig/hwrig.py --port /dev/tty.usbserial-A1 --baud 19200 run timing-test/test.bin
tools/hwrig/hwrig.py --mcu /dev/tty.usbmodem1401 reset
```

Developing against Copperline first is the point of `[serial] mode = "tcp"`:
the server, the upload protocol, the framing and this harness were all written
and debugged with no hardware attached, and deploy to real silicon unchanged.

## Reproducibility: read this before trusting a number

**A wire-driven run is not bit-identical to a native boot, even on the
deterministic emulator.** Measured on Copperline, uploading and running
`test.bin` through the server instead of booting it directly moves 8 of its 32
rows by 1-3 ticks, and repeating the upload moves a similar set again between
otherwise identical runs. The cause is that the beam, E-clock and refresh phase
at the moment `RUN` is issued depend on host scheduling during the upload.

The server parks the beam at the top of a frame before handing over, which
removes the frame-phase component, but polling resolution is a few colour clocks
so a residue remains. This is a property of the method, not a bug to be fixed.

Consequences for using the rig:

- Never quote a single run. Repeat a probe and report the distribution -- min,
  max, mode, and how many distinct values appeared.
- A one-tick disagreement with the emulator from a single run is phase noise
  until proven otherwise. The bar for "this is a real divergence" is a stable
  mode across cold boots.
- The golden values in `timing-test/README.md` were measured from native boots.
  Wire-driven numbers need their own baseline; do not compare the two directly.
- On real hardware there is additional genuine non-determinism a native boot
  also has (DRAM refresh phase at power-on, CIA startup phase, disk index), so
  expect the spread to be no smaller than what the emulator shows here.

Every result should also carry the banner it was measured on. One machine
characterised without its Agnus and Denise part numbers is one particular
8372A being mistaken for "hardware".
