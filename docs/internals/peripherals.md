# Peripherals and expansion

## Zorro autoconfig (`zorro.rs`)

The `ZorroChain` implements the Zorro II/III autoconfig protocol --
nibble-encoded config ROMs in the `$E80000` window, base-address
assignment, shut-up, chain advance, and power-on reset. Boards are
described by data (`BoardSpec`) rather than a trait; the built-in fast and
Z3 RAM options and user `[[zorro]]` metadata boards all build the same
specs. The user-facing guide, including the metadata file format and the
autoconfig walk-through, is [](../zorro).

## Gayle IDE (`gayle.rs`)

A600/A1200 machines get the Gayle gate array: the ID register at
`$DE1000`, the IDE task file at `$DA0000` (byte registers on the odd word
half, 4-byte stride), and the IDE interrupt and status bits. Drives are
raw flat HDF images with an RDB inside, opened read/write; PIO transfers
complete synchronously within the access. One hardware subtlety worth
knowing: Gayle byte-swaps the IDE bus, so IDENTIFY data words are
low-byte-first while sector data passes through untouched -- Kickstart
3.1 expects exactly this. The absent-slave behaviour follows the
WinUAE-verified model so device scans terminate correctly. PCMCIA reports
an empty slot (the status/config registers exist so card.resource
behaves); credit-card device emulation is a non-goal.

## A4000 motherboard IDE (`ide_a4000.rs`)

The A4000 profile decodes the same ATA task file (`ata.rs`) at `$DD2020`
with no gate array in front of it -- the layout Kickstart's own
`scsi.device` probes, with the Gayle-style 4-byte register stride, the
control block one A12 page up at `$DD3038`, and an interrupt status byte
at `$DD3020` whose bit 7 is the drive's INTRQ. Unlike Gayle there is no
interrupt-change latch: INTRQ feeds INT2 directly and the driver drops it
by reading the status register. Drives come from the same `[ide]`
section as Gayle machines.

## SCSI controllers (`a2091.rs`, `a4091.rs`, `sdmac.rs`, `scsi.rs`)

The `[scsi]` option attaches one of three host adapters, selected by its
`controller` key: the Zorro II A2091 (the default), the Zorro III A4091,
or the A3000's motherboard Super DMAC. All three drive the same SCSI-2
target layer in `scsi.rs`.

### A2091 (`a2091.rs`)

The A2091 is a Zorro II device board pairing the Commodore DMAC (rev 02
modeled) with a WD33C93A SBIC, plus the board's autoboot ROM whose
`scsi.device` drives them. The autoconfig
identity comes from the DMAC -- Commodore West Chester (514), product 3,
`ERTF_DIAGVALID` with `er_InitDiagVec` pointing at `$2000` -- while the
ROM supplies the DiagArea and the driver; the ROM image therefore is a
required configuration input (`rom`/`rom_odd`, split even/odd EPROM
dumps interleaved U13-first).

Board window layout: ISTR `$40`, CNTR `$42`, WTC `$80/$82`, ACR
`$84/$86` (low bit forced even), DAWR `$8E`, the WD33C93 SASR/auxiliary
status at `$90/$91` and data port at `$92/$93`, the ST_DMA/SP_DMA/CINT/
FLUSH strobes at `$E0/$E2/$E4/$E8` (read- or write-triggered), and the
boot ROM repeating from `$2000` to the end of the 64K window. Unpopulated
decode below the ROM reads as floating bus (`$FF`): the boot ROM's drive
probe ANDs the A590 XT-interface bytes at `$A1/$A3/$A5/$A7` and only
takes the SCSI-only path when they all read `$FF` -- zeros wedge it
polling a phantom XT drive.

The WD33C93A model covers both ways drivers run the bus, verified against
the real 7.0 boot ROM booting a Workbench install end-to-end:

- the **Select-and-Transfer** combination command (full transaction in
  one command, status byte landing in the Target LUN register, CSR
  `$16` then the `$85` disconnect interrupt), including the short-data
  pause (`$4B`, command phase `$46`) and resume that real targets force
  on MODE SENSE-style reads; and
- the **manual path** the 7.0 ROM uses: Select-with-ATN posting CSR
  `$11` then service-required `$88|phase`, identify message and CDB via
  Transfer Info (with the single-byte-transfer modifier), phase-qualified
  completions (`CSR_XFER_DONE | next phase`), message-in pausing with
  `$20` until Negate ACK releases the target to disconnect.

Data phases run through the DMAC handshake (a word per DMAC cycle into
chip, slow, or Zorro RAM with the 24-bit ACR auto-incrementing) or
through the PIO data register with DBR. Like the Gayle model, transfers
complete within the access; completion interrupts are delivered after a
short emulated delay, and INT2 is the level `CNTR_INTEN && ISTR &
(INTS|E_INT)` fed to Paula's PORTS latch each tick. DMAC bus-master
cycles are not yet arbitrated against the CPU (TODO in `a2091.rs`).

### A4091 (`a4091.rs`)

The A4091 is a Zorro III SCSI-2 controller carrying an NCR 53C710 and a
nibble-wide autoboot ROM. Within its 16M window, `$000000-$7FFFFF` is the
boot ROM presented nibble-wide (expansion.library reassembles the
DiagArea with DAC_NIBBLEWIDE, and the ROM's own relocator copies the
driver the same way), `$800000` is the 53C710 register file (only the low
6 address bits decode, so it mirrors across the window -- the driver
relies on the `+$40` shadow as a cache write-allocate workaround), and
`$8C0003` reads the DIP-switch byte (host ID, termination, negotiation
enables). A DSP write starts the 53C710's SCRIPTS processor, whose phase
engine executes the driver's SCRIPTS programs against the disk targets.
The autoconfig identity is Commodore product 84 with `er_InitDiagVec`
`$0200`.

### A3000 Super DMAC (`sdmac.rs`)

The SDMAC is the SCSI DMA controller on the A3000 motherboard, not a
Zorro board: a register file at `$DD0000` (repeating at `$DD0100` -- the
"ALT" shadow that write-through tools use to defeat CPU write buffering)
that owns the DMA FIFO and interrupt plumbing and maps a WD33C93's
register file into a select latch and data port. It is the same layering
as the A2091 -- two front-ends onto the one `Wd33c93` core -- differing in
the register map, the ISTR bits, and a 32-bit DMA address counter
(physically in Ramsey) instead of the Zorro II DMAC's 24-bit one.
Kickstart's built-in `scsi.device` drives the pair directly, so there is
no boot ROM to configure.

### Shared drive backend

All IDE and SCSI drives share the `harddrive.rs` sector backend: raw
HDF images, bare partition hardfiles wrapped in a synthesized RDB
(bootable `DHn` named after the unit), and host directories built into
in-memory FFS volumes by `dirfs.rs` (whose volume label defaults to the
directory name, or a `name` override configured on the drive). The
SCSI-2 target layer in
`scsi.rs` answers INQUIRY, MODE SENSE pages 3/4, READ CAPACITY,
READ/WRITE(6)/(10), REQUEST SENSE, and the no-op housekeeping commands,
with sense state kept per target.

The drive controllers latch read/write activity, which the bus drains to
light the status-bar HDD LED; the LED holds for a short minimum period so
brief accesses stay visible. Gayle, the A4000 IDE, the A2091, and the
SDMAC report activity today; the A4091 shows the LED but does not latch
activity into it yet.

## Host filesystem service (`filesys.rs`)

`[[filesys]]` mounts export host directories as live AmigaDOS volumes
(`HOSTFS0:` ... up to 8 mounts), with no disk image in between -- distinct
from the `dirfs.rs` path above, which snapshots a directory into an
in-memory FFS volume behind a virtual drive. The guest side is a tiny
handler (see `guest/services/`) mapped into the Copperline services board
with a mount table and a hand-built DiagArea; at expansion init it builds
one DeviceNode per mount and `AddBootNode`s it, so DOS mounts the devices
at boot. The handler forwards every DosPacket to the host through a
reserved A-line trap, and all `ACTION_*` semantics -- reads, writes,
create/rename/delete, directory walks, protection, comments, datestamps
-- are implemented host-side against the real filesystem, with results
written straight into guest memory.

Amiga attributes a host filesystem cannot hold live in UAE-style `.uaem`
sidecar files (read when present, written back on change, hidden from
guest listings); the delete-protection bit is honoured on
`ACTION_DELETE_OBJECT`. Filenames map between host UTF-8 and guest
Latin-1, hiding names with no Latin-1 spelling; host symlinks are
followed (the guest cannot create one, so a symlink is the host user
deliberately grafting a tree into the mount), while path escapes that a
guest could construct on its own (`..`, embedded separators) are
blocked. A `readonly`
mount refuses writes with the standard write-protection error.

## A2065 Ethernet (`a2065.rs`, `net.rs`)

The `[a2065]` option fits a Commodore A2065: a Zorro II board carrying an
Am7990 LANCE and 32 KiB of on-board RAM, driven by the AmigaOS SANA-II
`a2065.device`. Unlike the DMAC boards the LANCE never masters the Amiga
bus: its init block, descriptor rings, and packet buffers all live in the
board's own RAM, which the CPU reaches through the board window, so the
board is self-contained and owns a host `NetBackend` (`net/`) for real
frames. The LANCE engine models the Am7990 programming surface a real
driver exercises: TX and RX buffer chaining (STP..ENP spans across
descriptors), the stored FCS trailer (MCNT counts it; drivers read the
payload as `MCNT - 4`), the init-block MODE gates (DTX/DRX and the LOOP
internal-loopback self-test SANA-II drivers run at power-up), and MISS on
an RX ring overrun.

The `nat` backend (`net/nat/`, `net-nat` build feature) is a slirp-style
userspace NAT: a dedicated `a2065-nat` thread owns a smoltcp interface
that terminates ARP and the guest's TCP on the virtual gateway
(10.0.2.2, DNS forwarder 10.0.2.3, guest 10.0.2.15/24), splices each TCP
flow onto a non-blocking host socket, NATs UDP per flow, resolves DNS
through the host's own resolver, and answers BOOTP/DHCP and ICMP echo at
frame level. Frames cross to the emulated NIC over bounded channels that
drop on overflow, so the emulator thread never blocks on the host
network. Networking is inherently non-deterministic, so a fitted NIC
breaks byte-identical replay while traffic flows; save states record only
the chosen backend and bring up a fresh one on load (flows die; the
guest's TCP retransmits). The board and backend story, including the WASM
plugin `net` capability, is covered in [](../zorro).

## CDTV (`cdtv.rs`, `cdrom.rs`)

The CDTV model pairs the DMAC (which autoconfigs ahead of the Zorro chain,
as on the real machine -- the CDTV firmware requires the DMAC to be the
first configured board) with a Matshita drive speaking its fixed-length
command/response protocol: seek, read, play (LSN/MSF/track), status, SubQ,
and TOC queries, with responses delivered byte-by-byte with STEN pulses.
Data sectors DMA onto the system bus at the 24-bit ACR address -- chip,
slow, or Zorro board RAM, like the A2091's DMAC; Kickstart allocates the
CD buffers in fast RAM when a board is fitted -- paced at single speed and
raising the DMAC interrupt on completion. The 256 KiB extended ROM sits at
`$F00000`.

## CD32 Akiko (`akiko.rs`)

Akiko sits at `$B80000` with its `$C0CACAFE` ID: the chunky-to-planar
converter, the I2C lines to the 24C08 NVRAM EEPROM (persisted to the
`[cd] nvram` file), and the CD command/response rings talking to a Chinon
drive model (stop, pause, seek/play/read, LED, SubQ, status). Data sectors
stream as 2352-byte raw frames at 75 (or 150 at 2x) sectors/second; CD
audio mixes into the host output, and both light the blue CD LED. The
512 KiB extended ROM sits at `$E00000`, and the CD32 pad protocol drives
port 2.

`cdrom.rs` parses BIN/CUE cue sheets (single- or multi-file;
MODE1/2048, MODE1/2352, and AUDIO tracks) for both machines.

## RTC (`rtc.rs`)

An MSM6242-compatible register view at `$DC0000`, present on machines
configured with `rtc = true`. Reads reflect host time; guest writes only
affect the emulated latch/control state, never the host clock.

## Input (`gamepad.rs`, window input paths)

Host keyboard events translate to Amiga raw codes and feed a 6500/1
keyboard-MCU model (`chipset/keyboard.rs`) that clocks each event into
CIA-A bit by bit over the emulated KCLK/KDAT lines: 60 us bit cells,
the KDAT handshake after every byte (the MCU samples the line within
microseconds and accepts any deliberate pulse, so software that reads
the keyboard with a brief handshake -- e.g. Pinball Dreams at ~13.5 us
-- works, not just the boot ROM's longer pulse), lost-sync recovery
(lone sync bits, $F9, retransmission), the $FD/$FE power-up stream,
the $78/KCLK-low reset protocol behind Ctrl+Amiga+Amiga, Caps Lock's
keyboard-owned LED toggle, a 10-event type-ahead buffer with $FA
overflow, and ghost suppression on the real A500 key matrix (the seven
qualifiers are on dedicated lines and never ghost). The protocol was
cross-checked against real-hardware-validated replacement keyboard
firmware. Mouse deltas
feed the JOY0DAT quadrature counters. Gamepads are read through raw
`gilrs` events against the per-UUID calibration described in
[](../guide/ui); on CD32 machines the pad output is serialized through
the CD32 pad protocol instead of the plain digital joystick lines.

The window layer has one host-source policy for the emulated port-2
joystick/CD32 pad: gamepad (the default) or keyboard. Keyboard mode
skips gamepad polling for port-2 input; gamepad mode disables keyboard
joystick capture so the mapped keys take the normal Amiga keyboard path.
(The old auto-detect mode has been removed; `"auto"` in a config parses
as a backward-compatibility alias for gamepad.) Both sources ultimately
call the same `InputState::set_joystick_port2`
and `set_cd32_buttons_port2` helpers, so JOY1DAT, /FIR1, POT1Y/POTGOR, and
the CD32 serial bits remain hardware-derived.

Keyboard joystick emulation is deliberately a host input source, not a
guest-keyboard behaviour. When active, the winit key handler consumes the
mapped host keys before rawkey translation: cursor keys drive directions,
Right Ctrl/Right Alt drive fire, and the CD32 extras are C/X/D/S/Return/Z/A.
Each alias is tracked independently before resolving to a single joystick
state, so releasing one fire alias does not clear fire while another alias
is still held. Releases for keys already captured as joystick controls are
also swallowed if the source mode changes before key-up, preventing stray
Amiga rawkey releases.

## Audio output (`audio.rs`)

`AudioSink` abstracts the host boundary: a cpal live sink, a WAV-file sink
(`--audio-wav`), and a null sink (`--noaudio`). Paula renders in emulated
time; the live sink resamples and buffers against wall-clock. The
`CPAL_*` lead/prebuffer/stale-drop targets in `audio.rs` are fixed rather
than adaptive (currently a 131072-frame ring, a ~150 ms prebuffer equal to
the ~150 ms steady lead, and a ~300 ms stale-drop threshold at 44.1 kHz).
Playback starts only after the first audible frames have filled that
prebuffer, so silent boot/load periods do not queue seconds of zeros. If the
cpal callback later drains the queue completely, it stops playback, outputs
silence, and waits for the same prebuffer depth before restarting. While an
already-started queue is merely below target, the sink reports the missing
buffer depth as extra live-audio lead so the real-time pacer runs ahead and
restores the cushion without forcing a host-side silence gap first.

The live queue is host presentation state, not Paula state. A save-state or
reverse-debug timeline jump keeps the restored Paula/CD/floppy mixer state but
discards queued cpal frames from the abandoned timeline, then rebuilds the live
prebuffer from the restored emulated audio stream. Offline WAV capture is not
affected by any of this buffering policy.

Two profiling knobs cover the audio/pacing boundary, both emitting one
`info` line per second:

- `COPPERLINE_AUDIO_PROFILE=1` -- live-audio queue depth and the cpal
  callback counters (callbacks, callback frames, estimated device CCK,
  plus cumulative underrun/overrun/stale-frame totals). The cpal callback
  itself never logs; it only updates atomic counters under this flag.
- `COPPERLINE_REAL_PACING_PROFILE=1` -- the real-speed pacing line:
  retired instructions, raw `m68k` cycles, chip-bus wait CCK, device CCK,
  CPU chip-bus slots, host sleep count/time, and wall-time late
  count/time. Kept separate so CPU/device pacing can be measured without
  enabling the lower-level cpal counters.

Default live-audio warnings are emitted from the producer side at the same
one-second cadence, and only when an underrun, overrun, or stale-frame
counter is nonzero.

## Serial (`serial.rs`)

Paula's SERDAT transmit path lands on a `SerialSink`. The default
`StdoutSink` prints to the host terminal -- this
is how DiagROM's diagnostic stream and the `timing-test/` results are
captured in terminals and CI logs. `TcpSerialSink` bridges the port to a
listening TCP socket (`[serial] mode = "tcp"`, one client at a time) and
`PtySerialSink` to a host pseudo-terminal pair (`mode = "pty"`, Unix
only); both are bidirectional, so an `AUX:` shell on the Amiga side gives
a remote AmigaDOS console.

A `SerialSink` that can *produce* input must override
`has_pending_input` alongside `read_byte`/`read_word`:
Paula's per-tick UART step takes an idle fast path that skips the receiver
entirely while it reports false -- the TCP and pty sinks poll a counter
there, never a syscall.

CCP serial observability is a host-side tap beside `SerialSink`, not another
serial device. When a control connection subscribes, each successfully
completed transmit word is copied into a 4,096-entry `VecDeque`; the normal
sink receives the same word immediately. Overflow evicts and counts the
oldest observation, so a debugger cannot back-pressure Paula. The tap is
skipped by serde and carried across state loads with the live serial/audio
sinks; disconnecting or unsubscribing removes it.

## MIDI serial bridge (`midi/`)

`[serial] mode = "midi"` (or `--midi-out`/`--midi-in`) bridges Paula's
serial port to host MIDI, behind the optional `midi` cargo feature -- a
plain build compiles none of it and the mode falls back with a clear
message. The whole thing hangs off one `SerialSink`, `MidiSerialSink`, so
the emulator core is unchanged from any other serial target.

The load-bearing detail is that byte timing survives to the wire. Paula
stamps each transmitted byte with the emulated colour clock it left on
(`SerialTimeAnchor`); `MidiSerialSink` maps that to a host `Instant` and
asks the backend to *schedule* the message for that instant rather than
send it now, so a frame's worth of bytes flushed together still leaves at
the original spacing. Two host-agnostic pieces sit above the backend: a
`MidiFramer` reassembles the single-byte serial stream into whole MIDI
messages (a receiver rejects lone data bytes), tracking running status and
SysEx and passing interleaved real-time bytes straight through; and Active
Sensing (`0xFE`) is forwarded by default -- a real Amiga passes it down the
wire -- and only dropped under `COPPERLINE_MIDI_STRIP_ACTIVE_SENSE=1`.
Input arrives on a lock-free SPSC ring the receiver drains on its idle
fast path, so the poll never locks.

The host connection lives behind the `MidiBackend` trait, chosen by
`cfg(target_os)`: macOS drives CoreMIDI (`coremidi.rs`), Linux the ALSA
sequencer (`alsa.rs`), and Windows WinMM (`winmm.rs`); any other target gets
`stub.rs`, which enumerates nothing and refuses to open. Each backend links its
platform library directly with no wrapper crate, and each maps the
scheduled send onto that platform's timed-delivery primitive: a CoreMIDI
packet timestamp, an ALSA real-time queue event, or -- since WinMM carries no
timestamp -- a scheduler thread that fires each message when it comes due. A
new backend implements `send`/`set_output`/`set_input`/`current_output`/`current_input`
plus free `enumerate`/`open`; nothing else changes. The raw FFI is
layout-sensitive -- CoreMIDI packs its packet list to 4 bytes, the ALSA
`snd_seq_event_t` scheduling helpers are header-only inlines whose field writes
are replicated by hand, and WinMM's `MIDIHDR` is packed -- so the mirrors are
pinned with compile-time layout assertions and want checking against live MIDI,
not just review.

Two debug knobs help tell a dead path from a routing one:
`COPPERLINE_MIDI_DEBUG=1` reports per-second tx/rx byte counts and the
first bytes sent (no tx while a song plays means the guest is not driving
serial, i.e. the fault is upstream of the bridge); `=2` decodes every
message in each direction. `COPPERLINE_MIDI_IMMEDIATE=1` bypasses
scheduling and sends each message for immediate delivery, to separate a
timing problem from a connection one.
