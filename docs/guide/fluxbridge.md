# Physical floppy drives

Copperline can give any of its four floppy bays a *physical* 3.5" drive
instead of a disk image. The drive is attached to the host over a
[Greaseweazle](https://github.com/keirf/greaseweazle), and
[FluxBridge](https://github.com/CopperlineHQ/FluxBridge) -- CopperlineHQ's
own pure-Rust library, grown from a port of Rob Smith's FloppyDriveBridge --
does the talking. FluxBridge also carries DrawBridge and SuperCard Pro
protocols for the future; Copperline compiles in the drivers it supports and
the launcher offers exactly those, so today the interface list reads
Greaseweazle.

The emulated machine is not changed by any of this. The bridge supplies the
MFM the head would be passing over, so Paula, the disk DMA, and
`trackdisk.device` behave exactly as they do with an ADF.

## What you need

A [Greaseweazle](https://github.com/keirf/greaseweazle) (any revision with
main firmware 0.27 or newer), a 3.5" floppy drive, and some disks.

FluxBridge is pure Rust, pinned by revision in `Cargo.toml` and compiled into
Copperline, so a build that offers a physical drive can actually drive one --
there is no library to fetch, install, or keep beside the binary, and no C or
C++ toolchain involved in building it.

```sh
cargo build --release
```

The `fluxbridge` Cargo feature, on by default, is what includes it. Built
without it (`--no-default-features`), none of this exists: no **Physical
drive** tick box in the launcher, no `--floppy-bridge` flags, and a config
file's `bridge` keys are read and ignored.

## Using a physical drive

From the launcher, the Floppy tab carries a **Physical drive** tick box for
each bay. Tick it and the bay's media row stops offering a disk image and
names the interface instead, with a **Configure** button leading to its
settings. With nothing plugged in the row reads `Not connected`; plug the
interface in and re-tick the box to pick it up.

You can also define this in your .TOML file;

```toml
[floppy.df0]
bridge = "greaseweazle"      # or "off"
write_protected = true       # emulator-level protection; default true
# bridge_port = "/dev/ttyACM0"   # omit to auto-detect the interface
# bridge_cable = "a"             # a/b (IBM PC) or 0..3 (Shugart)
# bridge_density = "auto"        # auto/dd/hd
# bridge_mode = "normal"         # normal/compatible/stalling
# replay_speed = "fast"          # or "normal"; fast is the default
```

A bay cannot have both a bridge and an image -- the disk in the drive is its
media -- and saying so is an error rather than a silent preference. Setting
`bridge = "off"` returns the bay to images while leaving the rest of its
bridge settings in place.

And from the command line, with no config file at all:

```sh
copperline --model A500 --floppy-bridge df0 greaseweazle kickstart.rom
```

| Flag | Config key |
|---|---|
| `--floppy-bridge DFN NAME` | `bridge` |
| `--floppy-bridge-port DFN PORT` | `bridge_port` |
| `--floppy-bridge-cable DFN SEL` | `bridge_cable` |
| `--floppy-bridge-mode DFN MODE` | `bridge_mode` |
| `--floppy-replay-speed DFN SPEED` | `replay_speed` |
| `--floppy-bridge-writable DFN` | `write_protected = false` |

These layer on top of a config file as every other flag does, so
`--floppy-bridge df0 greaseweazle` turns DF0 over to a physical drive even if
the file gives it an image -- the flag says the bay *is* a physical drive, so
the image it displaces is not a conflict. There is deliberately no flag for
protecting a drive, because that is already the default.

If a bay asks for a physical drive and it cannot be opened, Copperline
stops with the reason rather than booting a machine with an empty drive
where you asked for your disk.

### Serial port

The interface connects over a serial port and can be found automatically,
which is the default. Name `bridge_port` explicitly to pin a particular
device when more than one is attached.

The launcher lists **Automatic**, then every serial device the host has.

### Drive select

`bridge_cable` picks which drive on the ribbon the interface selects: `a` or
`b` for the IBM PC cable convention, `0` to `3` for Shugart. A Greaseweazle
drives both conventions; pick the one matching your cable and the drive's
jumper. Disk-change sensing works on the IBM PC cable; the Shugart bus
cannot report it, so a swap there is noticed on the next read instead.

### Density

`bridge_density` is `auto` by default: the density is read from the flux
itself, which cannot confuse DD with HD, and a freshly inserted disk is
sensed anew. Force `dd` or `hd` only when a disk misreads -- an HD disk
formatted as DD in an Amiga drive is the classic case for forcing `dd`.

### Read mode

`bridge_mode` decides how flux gets off the disk. Two of the three modes are
the ones to know:

**`normal` -- the default, and the fastest.** The capture starts the instant
the head settles, wherever the disk happens to be in its spin -- no waiting
for the index hole -- and the guest is served the early sectors while the
later ones are still passing the real head, exactly as a real controller
reads. A capture that begins away from the index has its two ends joined
where the recording repeats; FluxBridge proves that join by pattern
matching, and verifies the capture decodes as a complete AmigaDOS track with
every checksum passing besides. A proven capture replays like an image's
track; one that cannot be proven is served once and fetched afresh on the
next visit, so a retry always reads new data.
This is the mode for essentially everything: it reads the same disks as
`compatible` and reaches a Workbench desktop appreciably sooner.

**`compatible` -- the archivist's mode, and the one for copy-protected
disks.** Each track is captured from one index pulse to the next, so the
revolution begins where the real one does and its two ends meet by
construction -- there is no join to prove and nothing to reconstruct, on any
format, including protected and non-AmigaDOS disks the verifier cannot read.
The price is waiting for the index, on average an extra half-revolution per
fresh track. Reach for it for disk preservation work, for titles whose
protection reads the track layout itself, or for any disk that misbehaves in
`normal`.

**`stalling`** also captures from the index, but holds the emulated machine
-- pointer and all -- whenever a track is not ready, for as long as it
takes. A real Amiga never does that; it is the last resort for pathological
loaders that cannot tolerate being answered "not yet".

The driver's own enum calls `normal` "Fast"; that spelling is accepted in
the config file too.

### Replay speed

Every track the guest reads is kept in memory, so re-reads never touch the
platter twice. `replay_speed` is how fast those replays are served:

- `fast` (the default): replays run at double speed. A track's *first* read
  always arrives at the platter's own pace -- the capture is served as it
  arrives -- so this only compresses the wait when the guest asks for a
  track already in hand, which is pure gain on loaders that revisit tracks.
- `normal`: replays run at the platter's real speed, indistinguishable from
  the disk itself. The opt-in for software that times its own drive.

As with `[floppy] speed`, software that measures its loading can notice
`fast`; nothing can notice `normal`.

## Write protection

A real disk is protected twice over, and both have to be open before anything
is written to physical media:

- the disk's own write-protect tab, sensed from the drive; and
- `write_protected` in the config, which defaults to `true` exactly as it
  does for an image.

So writing to a real floppy takes a deliberate `write_protected = false`
*and* an open tab. Both are enforced where the write would reach the platter,
not merely reported to the guest through the drive's /WPRO line, so a program
that writes without asking is stopped as well -- and they are the same two
facts the /WPRO line is built from, rather than a second reading taken at the
write, which could disagree with what the guest was told.

The driver keeps the tab's last reading and hands it back whatever the motor
is doing, so the state is good with the platter stopped -- which matters,
because a drive the guest is not actively reading is stopped nearly all the
time. With no disk in the drive there is no tab to have an opinion, so only
the configured protection applies.

In the launcher the same **Write protect** box covers a bay whether it holds
an image or a physical drive, and it starts ticked.

### What cannot be written

Two writes are refused rather than attempted, and both say so in the log.

A **partial write that does not start at the index** cannot be placed. The
interface takes a whole track and one bit of positional information: start at
the index pulse, or start wherever the head happens to be. There is no way to
say "start 3,000 bits in", so a partial write asking for that would land on
whichever sector was passing at the time. AmigaDOS writes all eleven sectors
at once, which is a whole revolution and lands correctly wherever it begins,
so this refusal should never be seen in ordinary use.

A **drive select the interface does not support** fails the open instead of
being ignored. Every driver advertises the cable conventions it can drive,
and a rejected selection would otherwise leave it quietly on Drive A,
reading and writing a different physical drive than the one asked for.

## What behaves differently

**The disk eject button is disabled for a bridged drive.** The status bar keeps a
bridged drive's numbered icon, so you can see the drive is there, but its
eject and swap buttons do nothing: Eject/insert disks as you would with an Amiga!
Putting a disk in or taking one out raises the same on-screen message an
image insert or eject shows, from the drive's own report.

**No synthesized drive sounds.** The real drive makes its own noise. A bay in the
same machine running an ADF still sounds as it should when enabled.

**The `[floppy] speed` option does not apply.** A physical drive is served
at the disk's own rate; `replay_speed` is its speed option.

**Powering off releases the drive.** A real drive takes its power from the
machine, and a bridged one behaves the same way: the power button hands the
interface back to the host, so it stops turning and another program -- or the
next machine this window builds -- can have it. Powering back on takes it
again. If it cannot be reopened, the machine still comes up, with that bay
empty, and the log says why.

**A bridged machine runs at real time.** The platter turns in wall-clock time
and cannot be hurried, so a machine with a real drive is paced like an Amiga
even in a headless run that would otherwise be unthrottled. Left to run free,
the emulated machine outruns the drive badly enough to spin the motor up and
down faster than it can reach speed, and the guest sees a drive that answers
almost nothing.

**A bridged run is not reproducible.** Save states cannot capture the medium,
and a replayed input recording will not line up. The emulated core is as
deterministic as ever; it is the disk under it that is not.

## Speed

A track's first read is served while the platter is still turning it, so the
guest starts on the early sectors as the later ones arrive -- the same
pipelining a real Amiga gets from a real drive. The head steps at the
Amiga's own 3 ms rate, and a faithful recording -- index-aligned, or
verified clean -- is kept and served from Copperline's own copy with no
drive involvement at all, so software that re-reads a track pays nothing and
`replay_speed = "fast"` serves the recovered cells at double speed.

Put together, a Workbench 1.3 boot from a physical drive lands within a few
seconds of the same disk in a real A500.

## Troubleshooting

**Nothing is detected, and the drive is definitely there.** On Linux, check
the serial device's group first (above) -- that is the usual answer. Then
confirm the interface is a Greaseweazle on main firmware (0.27 or newer), and
that no other program is holding the port open. Starting with
`--floppy-bridge df0 greaseweazle` reports what it found and refuses to run
if it found nothing, which is the quickest check.

**The launcher shows `None` with the interface plugged in** -- the check runs
when the launcher opens and when a bay is switched over, so untick and re-tick
**Physical drive** after plugging in.

**What it says at startup.** A bridged bay reports what it took hold of, on
the same footing as an image being inserted:

```text
floppy.df0 physical drive attached: Greaseweazle on /dev/ttyACM0, 3.5" HD drive, FluxBridge v0.2.0
floppy.df0 disk in the physical drive
floppy.df0 write-protected by the configuration; set write_protected = false to write to the disk
```

Putting a disk in or taking one out is reported as it happens -- in the log
and on screen -- as is the protection changing and the drive being let go on
power off. Nothing here needs a debug build or a log filter.

**Reads fail or the guest reports errors** -- check the disk's tab, then that
`bridge_cable` matches the drive's jumper. Then set
`COPPERLINE_DIAG_FLUXBRIDGE=1`, which turns on the drive's own running
commentary: every head move, every track handed over with how long it took and
how many attempts it cost, and the drive's state whenever a track is not ready.

```text
fluxbridge.df0 head to cylinder 40 side 0 (drive at 39)
fluxbridge.df0 waiting for track 80 (cyl 40 side 0) [ready=false disk=true motor=true at_cyl=40]
fluxbridge.df0 track 80 (cyl 40 side 0) read: 99933 bits, 6246 words, 622ms over 137 attempts, 113 cck/word
```

A healthy track is one revolution, so around 200ms plus whatever the seek
cost. Far longer, or attempts climbing without a track ever arriving, points
at the drive or the disk rather than at Copperline. The bit count dropping as
the head moves outward (101358 at cylinder 0 against 99933 at cylinder 40 in
the trace above) is the disk's own data rate and is not a fault.

**The interface works once, then needs unplugging.** Check whether its own
tools can still reach it -- `gw info` for a Greaseweazle. If they cannot
either, the fault is in the host's USB stack rather than in the emulator.

**The drive stops responding mid-session** -- an interface pulled out stops
answering, and Copperline says so once in the log. Reconnect it and restart
the machine.
