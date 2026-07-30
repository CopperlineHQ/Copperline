# Physical floppy drives

Copperline can give any of its four floppy bays a *physical* 3.5" drive instead
of a disk image. The drive is attached to the host over one of three interfaces, 
and Rob Smith's [FloppyBridge](https://amiga.robsmithdev.co.uk/winuae) library 
does the talking:

| Interface | What it is |
|---|---|
| DrawBridge | An Arduino-based reader/writer, by RobSmithDev |
| Greaseweazle | Keir Fraser's flux reader/writer |
| Supercard Pro | Jim Drew's flux board |

The emulated machine is not changed by any of this. The bridge supplies the
MFM the head would be passing over, so Paula, the disk DMA, and
`trackdisk.device` behave exactly as they do with an ADF.

## What you need

You need a [DrawBridge](https://amiga.robsmithdev.co.uk), [Greaseweazle](https://github.com/keirf/Greaseweazle), or [Supercard Pro](https://www.cbmstuff.com/index.php?route=product/product&product_id=52) interface, a 3.5" floppy drive, and some disks. 

FloppyBridge is compiled into Copperline from `vendor/floppybridge`, so a build that offers a physical drive can actually
drive one -- there is no library to fetch, install, or keep beside the binary.

```sh
cargo build --release
```

The `floppybridge` Cargo feature, on by default, is what includes it. Built
without it (`--no-default-features`), none of this exists: no **Physical
drive** tick box in the launcher, no `--floppy-bridge` flags, and a config
file's `bridge` keys are read and ignored.

`vendor/floppybridge/README.md` records the commit vendored and how to move to
a newer one.


## Using a physical drive

From the launcher, the Floppy tab carries a **Physical drive** tick box for
each bay. Tick it and the bay's media row stops offering a disk image and
names the interface instead, with a **Configure** button leading to its
settings. With nothing plugged in the row reads `Not connected`; plug the
interface in and re-tick the box to pick it up.

The Configure page's Interface row also offers **None** -- selected
automatically when a bay is bridged with nothing attached. A bay whose
interface is None keeps its tick box and its settings, but runs (and saves)
as an ordinary unbridged bay until an interface is chosen.

You can also define this in your .TOML file;

```toml
[floppy.df0]
bridge = "greaseweazle"      # or "drawbridge", "supercardpro", "off"
write_protected = true       # emulator-level protection; default true
# bridge_port = "/dev/ttyACM0"   # omit to auto-detect the interface
# bridge_cable = "a"             # a/b (IBM PC) or 0..3 (Shugart)
# bridge_density = "auto"        # auto/dd/hd
# bridge_mode = "normal"         # normal/compatible/stalling
# bridge_speed = 100             # serve captured tracks at 100/125/150%
# bridge_auto_cache = false
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
| `--floppy-bridge-density DFN D` | `bridge_density` |
| `--floppy-bridge-speed DFN PCT` | `bridge_speed = PCT` |
| `--floppy-bridge-auto-cache DFN` | `bridge_auto_cache = true` |
| `--floppy-bridge-writable DFN` | `write_protected = false` |

These layer on top of a config file as every other flag does, so
`--floppy-bridge df0 greaseweazle` turns DF0 over to a physical drive even if
the file gives it an image -- the flag says the bay *is* a physical drive, so the
image it displaces is not a conflict. There is deliberately no flag for
protecting a drive, because that is already the default. The remaining
options -- density, read mode, bridge speed, auto-cache, and profiles -- are
config-file only; they describe a rig rather than a run.

If a bay asks for a physical drive and it cannot be opened, Copperline
stops with the reason rather than booting a machine with an empty drive
where you asked for your disk.

### Serial port

Every current interface connects over a serial port, and every one of them
can be found automatically, which is the default. Name `bridge_port`
explicitly to pin a particular device when more than one is attached.

The launcher's list starts with **Automatic**, then the ports the library's
own scan names, then every other serial device the host has. The long tail
matters: an interface on a serial chip the scan does not recognise -- a
DrawBridge built on an Arduino clone, say -- can still be pointed at by
hand.

### Drive select

`bridge_cable` picks which drive on the ribbon the interface selects: `a` or
`b` for the IBM PC cable convention, `0` to `3` for Shugart. It only applies
to interfaces that have a drive-select line -- a Greaseweazle does, a
DrawBridge does not -- and the launcher greys the row for those that do not,
as reported by the driver itself.

### Read mode

The driver's own enum calls `normal` "Fast"; that spelling is accepted in the
config file too.

`normal`, the default, captures wherever the head happens to be, saving the
wait for the index -- most of a revolution on every track it has not read
before.

A capture that begins away from the index has its two ends joined by the
driver where the recording repeats, and that join is not always perfect.
Copperline therefore verifies each capture before trusting it: one that
decodes as a complete AmigaDOS track with every checksum passing -- join
included -- is kept and replayed like an image's track, and anything less is
served to the guest exactly once. A damaged or unrecognised recording is never
shown twice: the retry the guest makes reaches the recording that followed,
which is what recovering on a real drive means. The cost falls only where the
verdict is less than clean -- a format the scan does not recognise is re-read
from the disk when the guest returns to it, rather than answered from memory.

The drive cannot always finish a capture in the revolution the guest takes to
read the last one. When it has nothing newer, the recording just read is used
again, and the guest meets a splice at the join: the sector straddling it fails
and is retried, costing a revolution. Ten good sectors out of eleven beat none,
which is what refusing the stale recording gives -- the guest is handed filler
and starves. If a disk reads badly this way, `compatible` is the mode with no
seam to begin with.

`compatible` captures each track from the index pulse, so a revolution
begins where the real one does and its two ends meet in the sector gap,
exactly as a captured image's do. Slower, for the reason above; reach for it
if a disk reads badly without the index to anchor it.

`stalling` also captures from the index, but the driver holds the caller up
until a track is ready instead of answering "not yet". The wait lands on the
emulated machine, which stops -- pointer and all -- for as long as it takes.

### Bridge speed and auto-cache

`bridge_speed` serves each captured revolution at `100` (real, the default),
`125`, or `150` percent of the platter's real speed. The capture itself is
untouched -- reading a track off the disk still takes as long as a revolution
takes -- but the recovered cells reach the guest proportionally faster, so
rotational waits on tracks already in hand shrink. The trade is the same as
the image drives' `[floppy] speed`: the operating system and most loaders
tolerate it, but software that times its own loading can notice.

`bridge_auto_cache` caches disk data in the background while the drive is
idle. It is off by default: during a boot the drive is never idle, so there 
is little for it to do, and it moves the real head about on its own.

Every track the guest reads is kept in memory regardless -- re-reads never
touch the platter twice. What auto-cache adds is the tracks the guest has
*not* asked for: once the guest goes quiet, the drive carries on for up to a
minute or so, reading the rest of the disk once, then spins down. Later
reads of anything it reached are served instantly.

While the guest is actively loading, the cacher contends with it for the
head -- each of its excursions costs a seek away and back -- so loading is
audibly busier and somewhat slower than with auto-cache off. It pays for
itself on what comes after: revisits, browsing, and a re-boot from the same
disk.

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
being ignored. Every interface advertises the cable conventions it can drive
-- a Supercard Pro offers only the IBM PC `a`/`b` pair, for instance -- and a
rejected selection would otherwise leave it quietly on Drive A, reading and
writing a different physical drive than the one asked for.

## What behaves differently

**The disk eject button is disabled for a bridged drive.** The status bar keeps a
bridged drive's numbered icon, so you can see the drive is there, but its
eject and swap buttons do nothing: Eject/insert disks as you would with an Amiga!

**No synthesized drive sounds.** The real drive makes its own noise. A bay in the 
same machine running an ADF still sounds as it should when enabled.

**The `[floppy] speed` option does not apply.** It shapes how fast a track is
served from an image; a physical drive's data rate is the disk's own, and
turbo cannot spin a real platter forward in zero time. Image bays in the same
machine still honour it, and the launcher greys the Drive speed row when every
fitted bay is physical.

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

Reading a track means waiting for the drive to capture a whole revolution,
which takes as long as a revolution takes -- about 200 ms. A faithful
recording -- index-aligned, or verified clean -- is kept and served from
Copperline's own copy with no drive involvement at all, so software that
re-reads such a track pays nothing, and `bridge_speed` can serve the
recovered cells faster than the platter turns.

The head follows the emulated stepper, so the driver starts capturing while
the guest is still settling, and nothing waits on the drive with the machine
held still: the pointer keeps moving while a disk loads, as it does on a
real Amiga.

## Troubleshooting

**Nothing is detected, and the drive is definitely there.** On Linux, check
the serial device's group first (above) -- that is the usual answer. Then
confirm the interface is one of the three supported, and that no other program
is holding it open. Starting with `--floppy-bridge df0 greaseweazle` reports
what it found and refuses to run if it found nothing, which is the quickest
check.

**"the built-in FloppyBridge has no *X* driver"** -- the vendored bridge is
older than the interface you asked for; a maintainer needs to update it.

**The launcher shows `None` with the interface plugged in** -- the check runs
when the launcher opens and when a bay is switched over, so untick and re-tick
**Physical drive** after plugging in.

**What it says at startup.** A bridged bay reports what it took hold of, on
the same footing as an image being inserted:

```text
floppy.df0 physical drive attached: Greaseweazle on /dev/ttyACM0, 3.5" DD drive, FloppyDriveBridge v1.6
floppy.df0 disk in the real drive
floppy.df0 write-protected by the configuration; set write_protected = false to write to the disk
```

Putting a disk in or taking one out is reported as it happens, as is the
protection changing and the drive being let go on power off. Nothing here
needs a debug build or a log filter.

**Reads fail or the guest reports errors** -- check the disk's tab, then that
`bridge_cable` matches the drive's jumper. Then set
`COPPERLINE_DIAG_FLOPPYBRIDGE=1`, which turns on the drive's own running
commentary: every head move, every track handed over with how long it took and
how many attempts it cost, and the drive's state whenever a track is not ready.

```text
floppybridge.df0 head to cylinder 40 side 0 (drive at 39)
floppybridge.df0 waiting for track 80 (cyl 40 side 0) [ready=false disk=true motor=true at_cyl=40]
floppybridge.df0 track 80 (cyl 40 side 0) read: 99933 bits, 6246 words, 622ms over 137 attempts, 113 cck/word
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
