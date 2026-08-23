# Configuration reference

Copperline is configured by a TOML file: the file passed with `--config`,
else `./copperline.toml` if present, else the configuration saved with the
configuration screen's **Save default** button (see [](ui.md)). Every field
is optional; missing fields use the defaults documented here.
`copperline.example.toml` in the repository root is a commented companion to
this reference.

`--factory` ignores the saved default and starts from Copperline's own
settings; `--config` and `./copperline.toml` always win over it anyway. With
no default saved, the flag changes nothing.

The configuration is validated up front and the emulator refuses to start
with a clear error message rather than guessing (unknown CPU or chipset
names, out-of-range sizes, missing disk images, and so on).

### Paths on Windows

This applies to every path field below (`rom`, disk images, hard-drive
files, the SCSI ROMs, and so on). In a TOML double-quoted string the
backslash is an escape character, so a Windows path written the obvious way
(`rom = "C:\Kickstarts\KICK31.ROM"`) is rejected: `\K` is not a valid escape.
Use any one of:

```toml
rom = 'C:\Kickstarts\KICK31.ROM'    # single quotes: a literal string, no escaping
rom = "C:\\Kickstarts\\KICK31.ROM"  # double quotes: backslashes doubled
rom = "C:/Kickstarts/KICK31.ROM"    # forward slashes also work on Windows
```

Single-quoted literal strings are the least error-prone. macOS and Linux paths
use forward slashes and need none of this.

## Command-line overrides

The most common machine knobs can be set on the command line without writing
a config file. These flags layer on top of the config file (or, when there is
none, the built-in defaults) and are validated by exactly the same parsers and
range checks as the equivalent TOML fields:

| Flag | Overrides | Accepts |
|---|---|---|
| `--model NAME` | `[machine] profile` | `A1000`, `A500`, `A500OCS`, `A500Plus`, `A600`, `A1200`, `A3000`, `A4000`, `CDTV`, `CD32` |
| `--chipset NAME` | `[chipset] revision` | `OCS`, `ECS`, `AGA` |
| `--cpu MODEL` | `[cpu] model` | `68000`, `68010`, `68EC020`, `68020`, `68030`, `68040`, `68060` |
| `--cpu-clock MHZ` | `[cpu] clock_mhz` | a number of MHz |
| `--fpu` / `--no-fpu` | `[cpu] fpu` | fit / omit a 68881/68882 |
| `--jit` / `--no-jit` | `[cpu] jit` | experimental fast batch/trace-JIT CPU execution (68020+; not cycle-exact) |
| `--chip SIZE` | `[memory] chip` | `512K`, `1M`, `2M`, ... |
| `--fast SIZE` | `[memory] fast` | `0`, `1M`, `4M`, `8M`, ... |
| `--slow SIZE` | `[memory] slow` | `0`, up to `512K` |
| `--ram-init MODE` | `[memory] init` | `zero` (default), `random[:SEED]`, `pattern:WORD`, or `0xWORD` |
| `--motherboard SIZE` | `[memory] motherboard` | Ramsey RAM (A3000/A4000): `0`, `1M`..`4M`, `8M`, `12M`, `16M`; A4000 up to `64M` |
| `--accelerator SIZE` | `[memory] accelerator` | CPU-slot RAM at `$08000000` (32-bit CPUs): `0` to `128M` |
| `--floppy-drives COUNT` | `[floppy] drives` | `1` to `4` wired drives (`DF0:` plus external drives) |
| `--floppy-speed PERCENT` | `[floppy] speed` | `100` (real), `200`, `400`, `800`, or `0` (turbo) |
| `--floppy-bridge DFN NAME` | `[floppy.dfN] bridge` | drive a physical floppy drive: `greaseweazle`, or `off` |
| `--floppy-bridge-port DFN PORT` | `[floppy.dfN] bridge_port` | that interface's serial port (default: auto-detect) |
| `--floppy-bridge-cable DFN SEL` | `[floppy.dfN] bridge_cable` | drive select: `a`/`b` (IBM PC cable) or `0`-`3` (Shugart) |
| `--floppy-bridge-mode DFN MODE` | `[floppy.dfN] bridge_mode` | how tracks are captured: `normal`, `compatible`, `stalling` |
| `--floppy-bridge-density DFN D` | `[floppy.dfN] bridge_density` | force a density: `auto`, `dd`, `hd` |
| `--floppy-replay-speed DFN SPEED` | `[floppy.dfN] replay_speed` | replay already-captured tracks at `fast` (the default, double speed) or `normal` (the platter's own) |
| `--floppy-bridge-writable DFN` | `[floppy.dfN] write_protected = false` | allow writing to the real disk |
| `--joystick MODE` | `[input] joystick` | `gamepad` (default), `keyboard` |
| `--mouse-sensitivity N` | `[input] mouse_sensitivity` | `0`-`100` host mouse speed (`50` default = 1:1) |
| `--mouse-capture MODE` | `[input] mouse_capture` | When the host mouse is grabbed: `click` (default), `auto`, `manual` |
| `--port1 DEVICE` | `[input] port1` | `mouse` (default), `joystick`, `cd32`, `analogue`, `none` |
| `--port2 DEVICE` | `[input] port2` | same devices; default `joystick` (`cd32` on the CD32 profile) |
| `--autofire HZ` | `[input] autofire_hz` | `0` (off, the default) to `30` |
| `--full-screen` / `--windowed` | `[display] full_screen` | open fullscreen or windowed at start (default windowed) |
| `--show-status-bar` / `--hide-status-bar` | `[display] status_bar` | status bar at start (default shown) |
| `--menu-scale SIZE` | `[display] menu_scale` | size of the pop-up menu: `1x` (default) or `2x` |

For example, to boot a stock A1200 profile but with 8 MB of fast RAM and a
faster CPU, with no config file at all:

```sh
./target/release/copperline --model A1200 --fast 8M --cpu-clock 28 KICK31.ROM
```

A `--model` profile supplies the chipset, CPU, and memory defaults of a real
machine; the other flags then override individual values on top of it, just as
explicit `[cpu]`/`[chipset]`/`[memory]` sections override a `[machine]`
profile in a config file.

The audio, serial, parallel, and network surface has matching per-run flags
too -- `--audio-device`, `--audio-channel-mode`, `--audio-filter`,
`--audio-stereo-separation`, `--serial`, `--midi-in`, `--midi-out`,
`--mt32-control-rom`, `--mt32-pcm-rom`, `--mt32-panel`, `--parallel`,
`--sampler-audio-input`, `--sampler-input-gain`, `--a2065-net`,
`--a2065-interface`, `--hostsocket-net`, `--hostsocket-interface` --
described with their `[audio]`, `[serial]`, `[parallel]`, `[a2065]`, and
`[hostsocket]` keys below.

## Top level

```toml
rom = "KICK13.ROM"            # Kickstart image, 512 KiB (or a 256 KiB 1.x part)
extended_rom = "cd32ext.rom"  # optional: CDTV (256K at $F00000) or
                              # CD32 (512K at $E00000) extended ROM
# identify = false            # drop the Copperline identification board
                              # from the Zorro chain (default: present)
```

`identify` controls a small, inert Zorro autoconfig board Copperline puts on
the expansion chain (manufacturer 5192 / product 2) so guest software such
as [identify.library](https://github.com/shred/identify) can detect that it
is running under the emulator. It is on by default and does not change the
machine's usable memory; set `identify = false` for a chain with no
emulator-identifying board. See [](../zorro) for details.

The ROM path can be overridden by a positional CLI argument. Omit `rom`
entirely (and pass no ROM argument) to boot the bundled AROS open-source
Kickstart replacement, which ships with Copperline as the default boot ROM;
its main and extended halves are located next to the binary (under
`share/copperline/aros` for a Homebrew install) or set
`COPPERLINE_AROS_DIR`. You can also fit a different ROM at runtime from the
menu's **Load Kickstart ROM...** item, which hard-resets the machine. Machine
profiles that need an extended ROM (CDTV, CD32) will tell you if it is
missing.

Both ROM keys accept images in either byte order. Alongside plain CPU-order
dumps, the byte-swapped images prepared for EPROM programmers -- the
single-chip `.bin` ROM files in Hyperion's Kickstart 3.1.4/3.2 releases, such
as `kick.a500a600a2000.46.143.bin`, store every 16-bit word with its bytes
exchanged -- are recognised from their header and restored on load, so either
file boots identically. A 256 KiB Kickstart 1.x part is mirrored across the
512 KiB ROM window, as it decodes on real hardware. The split `hi`/`lo` chip
pairs for the 32-bit machines are not accepted; use the matching single-file
image instead.

Copperline also names the ROM it is given. A ROM file's name is whatever its
dumper called it, so the image is identified by checksum against a table of
the released Amiga boot ROMs -- every Kickstart from 1.0 to 3.2.3, the CD32
Kickstart and extended ROM, the CDTV/A570 extended ROMs and the A1000
bootstrap -- and the version is reported as, for example,
`Kickstart 3.1 (40.68) A1200`. Identification survives the same forms the
loader accepts (byte-swapped dumps, a 256 KiB part stored doubled), and an
Amiga Forever image still in its `AMIROMTYPE1` container is reported as
encrypted rather than unknown. The identification appears in the start-up
`config:` log line, in the About window's `ROM:` line, in the OSD when a ROM
is fitted from the menu, and under each path row of the machine-configuration
screen's ROM tab. The bundled AROS ROM reports itself as `bundled AROS`; any
other image the table does not carry -- DiagROM, a ROM you built yourself --
simply goes unnamed and boots as usual.

## `[paths]` -- where files go

```toml
[paths]
# base = "/somewhere/else"   # move the whole tree; the rest are under it
# states = "states"          # save states, incl. the quick-save slots
# screenshots = "screenshots"
# recordings = "recordings"  # video captures and recorded input scripts
# nvram = "nvram"            # battery-backed RAMs, incl. CD32 game saves
# traces = "traces"          # debugger traces and waveform captures
# configs = "configs"        # configurations saved from the launcher
# roms = "roms"              # the rest set where an empty file dialog opens:
# mt32_roms = "roms/mt32"    # MT-32 control and PCM ROMs
# floppies = "floppies"
# harddrives = "harddrives"
# cds = "cds"
```

Where Copperline writes what it produces, and where its file dialogs open
when the field they were launched from is empty. Every key is optional and
an omitted key uses the default shown, so the section only needs to exist
once something is moved. The launcher edits the same keys on its **A/V &
Emu -> Paths** page.

Each default is a folder of that name under the host-data directory --
`~/.config/copperline` on macOS and Linux, `%APPDATA%\copperline` on
Windows, or the executable's own folder in a [portable
installation](ui.md#where-files-go). A relative path is taken from
`base` (itself taken from the host-data directory when relative or unset),
an absolute path is used as given. Output folders are created on first
write; the dialog folders are never created, and a dialog only starts in
one that exists.

These name folders on the machine that runs the configuration, so a config
copied to another machine may name folders that are not there. At startup
each entry set here is checked and one that cannot be found is ignored with
a warning in the log, falling back to its default -- the check waits no
more than a moment, so a dead network mount delays starting rather than
preventing it. Explicit output paths on the command line
(`--screenshot-after`, `--save-state-after`, and so on) are used exactly as
written and never consult this section.

## `[machine]` -- machine profiles

```toml
[machine]
profile = "A1200" # A1000, A500, A500OCS, A500Plus (A500+), A600, A1200, A3000, A4000, CDTV, CD32
rtc = true        # add a battery RTC (default: only A500+/CDTV/A3000/A4000 ship with one)
# rtc_chip = "RP5C01"              # MSM6242 (default) or RP5C01 (A3000/A4000 default)
# rtc_time = "2005-03-18 01:58:29" # seed the clock; it then ticks in emulated time
# rtc_frozen = true                # stop the seeded clock at rtc_time exactly
# battmem = "battmem.nvram"        # RP5C01 battery-RAM backing file (default when fitted)
mem_controller = "ramsey-07" # none, ramsey-04 (A3000), ramsey-07 (A4000)
rom_scsi_device_disable = true # skip the ROM's scsi.device (default: when its bus has no drives)
```

A machine profile bundles the chipset, CPU, memory, gate array, and
peripheral defaults of a real machine. The key is `profile` (the deprecated
`model` alias still parses) so it never collides with `[cpu] model`. Explicit `[cpu]`, `[chipset]`, and
`[memory]` sections override individual profile defaults. Without a
`[machine]` section you get the A500 Rev 6A default (the same as the `A500`
profile: ECS 8372A Agnus, OCS 8362 Denise, 68000, 512K chip RAM, 512K
trapdoor slow RAM) -- the most common and most-targeted Amiga. An explicit
`[chipset] revision` overrides the per-machine chips, so `revision = "OCS"`
gives a plain 8371/8362 OCS machine.

| Profile | Chipset | CPU | Chip RAM | Slow RAM | Extras |
|---|---|---|---|---|---|
| `A1000` | OCS (8361/8367 Agnus, OCS Denise) | 68000 @ 7.09 MHz | 256K | 0 | WCS, boot ROM + Kickstart disk |
| `A500` | Rev 6A: ECS 8372A Agnus, OCS 8362 Denise | 68000 @ 7.09 MHz | 512K (up to 1M) | 512K | -- |
| `A500OCS` | OCS (8371 Fat Agnus, OCS Denise) | 68000 @ 7.09 MHz | 512K | 512K | early A500 / A2000 |
| `A500Plus` | ECS (8375 Agnus, ECS Denise) | 68000 @ 7.09 MHz | 1M | 0 | RTC |
| `A600` | ECS (8375 Agnus, ECS Denise) | 68000 @ 7.09 MHz | 1M | 0 | Gayle IDE |
| `A1200` | AGA (Alice/Lisa) | 68EC020 @ 14.18 MHz | 2M | 0 | Gayle IDE |
| `A3000` | ECS | 68030 @ 25 MHz | 2M | 0 | Ramsey-04, RP5C01 RTC |
| `A4000` | AGA (Alice/Lisa) | 68040 @ 25 MHz | 2M | 0 | Ramsey-07, RP5C01 RTC |
| `CDTV` | ECS | 68000 @ 7.09 MHz | 1M | 0 | DMAC CD controller, RTC, 256K extended ROM |
| `CD32` | AGA (Alice/Lisa) | 68EC020 @ 14.18 MHz | 2M | 0 | Akiko, CD32 pad, NVRAM, 512K extended ROM |

`rtc` exists because most Amigas shipped without a battery-backed clock and
only some carried one. The `A500Plus` (an OKI RTC soldered to the Rev 8A
board), `CDTV`, `A3000`, and `A4000` fit one by default; the base
A500/A500OCS, A600, A1200, A1000, and CD32 have none. Set `rtc = true` to add
one -- for an A600HD or a clock-equipped A1200, say -- so the Workbench clock
keeps time.

`rtc_chip` names the part in that socket, because Commodore used two with
different register protocols: the OKI **MSM6242** on the small boxes, the
CDTV, and the aftermarket clock expansions, and the Ricoh **RP5C01** on the
A3000/A4000 motherboards (the Ricoh also carries 26 nibbles of battery RAM,
which AmigaOS uses via `battmem.resource` on those machines). The default
follows the profile -- `RP5C01` on `A3000`/`A4000`, `MSM6242` everywhere else
-- and setting the key implies `rtc = true`. AmigaOS probes for either part,
so the choice is mostly invisible to it, but Linux/m68k does not probe: it
drives the chip the machine model dictates, so an A3000/A4000 booting Linux
needs the RP5C01 answering for its clock to work.

`battmem` persists the RP5C01's battery-backed registers -- the 26 RAM
nibbles behind `battmem.resource` plus the alarm and 12/24 settings --
across runs, the way the real board's battery does. This is where
`scsi.device` keeps its per-unit SCSI host settings (an A3000's or A4091's
synchronous-transfer, disconnect, and last-drive options, including
remembering attached CD-ROM drives), so without it those revert every run.
The file uses the same `.nvram` layout as WinUAE and Amiberry, so backing
files interchange between emulators; only the battery payload loads back --
the time-of-save digits in the file never override the (host- or
`rtc_time`-driven) clock. It defaults to `battmem.nvram` in the working
directory whenever an RP5C01 is fitted; point it elsewhere with a path, or
set `battmem = ""` to keep the battery registers session-only. Note that a
persisted file carries guest-visible state from one run into the next by
design, so delete it (or disable it) where byte-for-byte reproducible
headless runs matter.

`rtc_time` seeds the clock instead of letting it mirror the host's: the value
is either an integer (Unix seconds, UTC) or a string
`"YYYY-MM-DD HH:MM[:SS]"` giving exactly the wall-clock time the guest reads
at power-on. A seeded clock ticks with *emulated* time, so the time the guest
sees is deterministic and reproducible byte-for-byte across runs -- the way
to test time-dependent guest software (TOTP/RFC 6238 vectors, timestamped
logs, date rollovers) or just to boot into a fixed date. Setting a time
implies `rtc = true`; combining it with an explicit `rtc = false` is an
error. `rtc_frozen = true` additionally stops the tick so every read returns
`rtc_time` exactly. Both are also available as `--rtc-time` / `--rtc-frozen`
CLI flags, and a control-protocol session can inspect and move the clock live
with `rtc.get` / `rtc.set` (see `docs/debugger/control.md`).

Two guest-side notes: Kickstart 2.0+ loads the system time from the battery
clock automatically at boot, while Kickstart 1.3 only does so when the
startup-sequence runs `SetClock LOAD`; and the chip's two-digit year registers
mean AmigaOS applies its usual century window, so seeds outside 1978-2077
will not read back as the year you set. A host-initiated reset or power
cycle restarts the emulated timeline and therefore restarts a seeded clock
from `rtc_time`; a guest-initiated reboot (the 68000 `RESET` instruction)
leaves it ticking, like real battery-backed hardware.

The `A3000` and `A4000` profiles are the big-box machines. They carry a
Ramsey memory controller (`mem_controller`), which is what the two registers
at `$DE0003` and `$DE0043` answer as, and they carry Gary rather than Gayle
-- so no PCMCIA and no Gayle IDE.

- The **A3000** has its motherboard SCSI: a Super DMAC at `$DD0000` driving a
  WD33C93, which Kickstart's own `scsi.device` initialises at boot. Attach
  drives to it with `[scsi] controller = "a3000"` (the default controller on
  an A3000; see the `[scsi]` section below).
- The **A4000** has its motherboard IDE interface at `$DD2020`; attach drives
  with `[ide]`, exactly as on a Gayle machine.

`rom_scsi_device_disable` skips Kickstart's built-in disk driver. It defaults
on when the machine's own controller -- the IDE port of an A600, A1200, or
A4000, or the A3000's SCSI -- has no drives configured: with nothing to boot,
the driver only costs startup time probing an empty bus. Configuring a drive
turns the driver back on automatically (it is the boot path for those
drives), and setting the flag explicitly wins in either direction. The ROM
file itself is never modified.

Both profiles fit their stock 4M of Ramsey-controlled motherboard fast RAM;
`[memory] motherboard` resizes it up to 16M, and on the A4000 up to 64M
via the motherboard RAM expansion space (see the `[memory]` section).
`[memory] accelerator` adds CPU-slot RAM at `$08000000` on any 32-bit
machine.

`mem_controller` is normally left to the profile. It is broken out because
Ramsey's registers collide with nothing else, so it can be fitted to
a wedge machine to exercise diagnostic tools that expect one.

The `A1000` profile models the original Amiga, which has no Kickstart ROM.
Its `rom` is instead the 64K bootstrap ROM ("Amiga ROM Bootstrap"); on
power-up the bootstrap loads Kickstart from the Kickstart disk in DF0 into
256K of writable control store (WCS) at `$FC0000`, write-protects it, and runs
it -- exactly as the real machine does. So an A1000 config names the bootstrap
ROM as `rom` and puts the Kickstart disk in `[floppy.df0]`; leave it in and the
machine boots to Kickstart (which then asks for a Workbench disk). See the
ready-made `a1000.example.toml`.

The `A500` profile models the common Rev 6A board: the ECS "Fatter" 8372A
Agnus (a 1 MiB chip-RAM reach and the software-selectable PAL/NTSC switch via
`BEAMCON0`) paired with the original OCS 8362 Denise. It is therefore an
Agnus-only ECS upgrade, not a full-ECS machine -- the OCS Denise means no
superhires or `BRDRBLNK`, exactly as on the real board. Chip RAM defaults to
the stock 512K but accepts up to 1M (`[memory] chip = "1M"`); more than 1M is
rejected because the 8372A cannot address it. Booting with no `[machine]`
section uses the same Rev 6A defaults; select `A500OCS` or set
`[chipset] revision = "OCS"` for the older 8371/8362 machine.

## `[emulation]`

```toml
[emulation]
power_on = true            # false = start powered off at the test screen
pacing_budget = "cycles"   # "cycles" (hardware-accurate) or "instructions"
realtime_priority = false  # true = raise the pacer/audio thread priority
warp_speed = "max"         # turbo limit: "2x", "4x", "8x", "16x", or "max"
rewind = false             # true = record rewind history from power-on
rewind_budget_mb = 256     # host memory the rewind history may hold
rewind_interval_frames = 25 # emulated frames per rewind step
```

The deterministic cycle-driven core is the only emulation timing. It is
paced to wall-clock for the interactive window and runs unthrottled for
headless captures; the emulated result is identical. (An older `speed` key
here is accepted but ignored -- "real" was the only timing model, so it
carried no information.)

- `power_on = false` starts the machine powered off showing a test screen
  until you click the status-bar power button -- useful for arming video
  capture first. The power button cold-boots (reinitialising RAM according to
  `[memory] init`).
- `pacing_budget` selects how real-time pacing budgets CPU work per frame:
  `"cycles"` (default) charges each instruction its actual 68000 cycle cost
  plus chip-bus waits, matching real hardware speed; `"instructions"` uses a
  flat `COPPERLINE_REAL_CPU_CPI` (default 4.0) cycles/instruction quota,
  which is cheaper but runs the CPU faster than hardware.
  `COPPERLINE_REAL_PACING_BUDGET` overrides this for one run. See
  [](../internals/timing) for the full rationale.
- `realtime_priority = true` asks the OS to schedule Copperline's two
  latency-critical threads -- the wall-clock pacer and the audio callback --
  above normal, which reduces frame stutter and audio glitches when the host
  is busy. It is best effort and off by default, and never fails the run:
  - **macOS** -- the pacer thread joins the `USER_INTERACTIVE` QoS class. The
    audio callback is left alone because Core Audio already runs it on a
    real-time thread (overriding that would only demote it).
  - **Windows** -- both threads are raised via `SetThreadPriority`; no
    privilege required.
  - **Linux/other Unix** -- raising priority needs privilege (an `rtprio`
    rlimit, `CAP_SYS_NICE`, or root). Without it the request is logged and
    declined, and the thread keeps normal scheduling.

  `COPPERLINE_REALTIME_PRIORITY` overrides this for one run; set it to
  `0`/`false`/`off` to force it off, or to any other value (or leave it empty)
  to force it on.
- `warp_speed` sets the default speed of Warp Speed (turbo) mode. The window
  presents with vsync, so emulating one frame per presented frame would pin
  warp to the host monitor's refresh rate. This option is an output frame
  skip -- `"2x"`, `"4x"`, `"8x"`, `"16x"`, or `"max"` (default) -- so warp
  retires that many emulated frames per presented frame, making warp roughly
  the limit times the refresh rate (host CPU permitting). `"max"` runs flat
  out and still presents at vsync. Adjust it live from the **Warp Limit**
  menu item or `Cmd+Shift+W` / `Alt+Shift+W` (see [The window and its
  controls](ui.md)).
- `rewind = true` records rewind history from power-on, so `Cmd+Z` / `Alt+Z`
  and the **Rewind** menu item can step the whole machine backward through
  it. It rides the same deterministic snapshot ring as the debugger's reverse
  controls (see [](../debugger/reverse)) and is off by default because it is
  not free: `rewind_budget_mb` (default 256) of host memory holds the
  snapshots, and one whole-machine serialize happens every
  `rewind_interval_frames` (default 25, half a second of PAL). One rewind
  step goes back exactly one interval; oldest snapshots are evicted first, so
  how much emulated time the budget buys scales with the machine's RAM size.
  Turning the menu item off releases the retained snapshots. The same
  determinism preconditions as reverse debugging apply -- a real-time clock
  and host disk writes are not rolled back.

## `[cpu]`

```toml
[cpu]
model = "68000"     # 68000, 68010, 68EC020, 68020, 68030, 68040, 68060
clock_mhz = 14.0    # optional; defaults to the model's stock speed
# icache = false    # instruction-cache model (on by default on all 020+ models)
# dcache = false    # data-cache model (on by default: 030/040/060)
# fpu = true        # fit a 68881/68882 (68020/68030; needs the coprocessor
#                   # interface, so not valid on a 68000). The full 68040's
#                   # and 68060's on-die FPUs are enabled by default.
# unimplemented = "trap"  # 68060 only: "trap" (faithful; the OS needs
#                   # 68060.library) or "native" (execute the removed
#                   # instructions directly)
# jit = true        # experimental: fast batch/trace-JIT CPU execution
#                   # (68020+; not cycle-exact)
```

- `model`: the 68010 models the vector base register, the format-stacking
  exception model, and DBcc loop mode; the 68EC020 is a 68020 instruction
  set with a 24-bit external address bus.
- `clock_mhz` defaults to the model's stock speed (68000/68010 ~7.09, 020 ~14,
  030/040 ~25, 060 50) and is modelled as a whole multiple of the colour clock
  (3.546895 MHz). Fast RAM and ROM run at the CPU clock; chip and slow RAM
  stay chip-bus bound, so overclocking speeds up only what a real
  accelerator would speed up.
- `icache`/`dcache` model the on-chip caches and default **on** for the
  silicon that has them (instruction cache on the 020/68EC020/030/040/060,
  data cache on the 030/040/060), matching real hardware where AmigaOS
  enables them via CACR. The cache is sized to the CPU: 256 bytes on the
  020/030, 4 KB on the 040, 8 KB on the 060. Set either to `false` to opt
  out.
- `unimplemented` (68060 only) picks what happens on the instructions the
  68060 dropped from silicon: MOVEP, CHK2/CMP2, CAS2, misaligned CAS,
  64-bit MUL/DIV, and most of the FPU beyond basic arithmetic
  (transcendentals, FMOVECR, packed decimal). `"trap"` (the default) is
  what the chip does - they raise the unimplemented-instruction exceptions
  and the OS-side `68060.library` emulates them, exactly as on a real
  CyberStorm or Blizzard board, so software using them needs that library
  installed. `"native"` executes them directly for systems without it.
  Kickstart 3.1 itself boots fine under `"trap"`. `fpu = false` on the
  68060 models the LC/EC060: FP instructions take the FPU-disabled
  exception (PCR.DFP), which an OS handler can also use to enable and
  restart. The 68060's superscalar dual-issue and branch cache are
  modelled and activate when system software enables them (PCR.ESS and
  CACR.EBC, which `68060.library` does at boot); until then the chip runs
  scalar, as on real silicon. This is not cosmetic: code that loops
  out of chip RAM otherwise contends with bitplane DMA on every instruction
  fetch and can run at roughly half speed, which is why an AGA demo's music or
  animation may pace correctly only with the cache modelled. The data cache
  caches expansion RAM/ROM only, since chip and slow RAM are DMA-visible and
  cache-inhibited as on real Amigas.
- `jit` (experimental, also `--jit`/`--no-jit`) runs the CPU through the
  m68k core's batch/trace-JIT path instead of the cycle-exact
  per-instruction model: hot code compiles to native traces and fast-RAM
  accesses run through a zero-cost direct-memory window. The machine
  behaves like an ideal accelerator running at `clock_mhz`: one
  instruction per CPU clock with zero-wait fast RAM and ROM, so a 50 MHz
  68040 delivers on the order of 50 MIPS (raise `clock_mhz` for more).
  Interrupts are recognized at batch boundaries, so chip-level races and
  cycle-counted effects no longer line up; leave it off for anything
  timing-sensitive (games, demos). Chip and slow RAM still arbitrate onto
  the shared chip bus in order, and the on-chip cache models stay active
  (as on a real accelerator, they are what lets chip-RAM-resident code
  run at CPU speed), so displays, blits, and device I/O work normally. Requires a 68020
  or later: the 68000/68010 share one bus with the chipset and their
  floating-bus and prefetch semantics need the precise core, so
  `jit = true` on those models logs a note and stays precise (the
  launcher greys the toggle). Known issue: the bundled AROS ROM's boot
  screen may stay grey under JIT on some 020+ configurations (the guest
  still runs; Kickstart ROMs are unaffected).

## `[memory]`

```toml
[memory]
chip = "512K"        # OCS max 512K; ECS/AGA max 2M
fast = "0"           # Zorro II fast RAM at $200000: 64K..8M board sizes
slow = "512K"        # A500 trapdoor RAM at $C00000: 0 or up to 512K
init = "zero"         # or "random[:SEED]" / "pattern:0x5555" for read testing
motherboard = "0"    # Ramsey motherboard RAM (A3000/A4000): up to 16M (A4000: 64M)
accelerator = "0"    # CPU-slot RAM at $08000000 (32-bit CPUs): up to 128M
z3   = "0"           # Zorro III RAM (needs a 32-bit CPU): 64K..1G, power of two
```

Sizes accept `K`/`KB`/`M`/`MB` (and `G`/`GB` for Zorro III) suffixes or
plain byte counts, and must be multiples of 4 KiB.

`init` controls cold power-on contents for writable system RAM. The default,
`"zero"`, preserves Copperline's normal compatibility behaviour. `"random"`
fills chip, slow, motherboard, accelerator, A1000 WCS, and RAM-backed Zorro
boards with deterministic pseudo-random bytes, which helps expose guest code
that reads memory before initialising it. The fixed default seed is identical
across hosts and repeated cold resets; `"random:SEED"` selects another decimal
or `0x` hexadecimal 64-bit seed for test matrices. `"pattern:WORD"` instead
repeats one decimal or `0x` hexadecimal 16-bit word in big-endian Amiga byte
order; for convenience a bare value such as `"0x5555"` means the same thing as
`"pattern:0x5555"`. The Machine Configuration screen exposes these as
**Power-on fill** and **Fill pattern** on its Memory page. Warm keyboard resets
still preserve RAM, and save-state restores reproduce the saved bytes exactly.

For a one-off developer run:

```sh
./target/release/copperline --ram-init random --config game.toml --noaudio \
  --screenshot-after 20 /tmp/game-random-ram.png
```

- **Chip RAM** is range-checked against the chipset: 512K on OCS, 2M on
  ECS/AGA (also bounded by the selected Agnus revision's address reach).
- **Fast RAM** is exposed as a Zorro II autoconfig board at `$200000`, so
  it must be a legal Zorro II board size: 64K, 128K, 256K, 512K, 1M, 2M,
  4M, or 8M.
- **Slow RAM** ($C00000 "ranger" RAM) is arbitrated on the chip bus through
  Agnus exactly like chip RAM -- it is slow in the authentic way.
- **Motherboard RAM** is the 32-bit local memory Ramsey drives on the
  A3000/A4000: it ends at `$08000000` and grows downward (16M reaches
  `$07000000`), and Kickstart sizes it with its own probe -- no autoconfig
  involved. It needs a Ramsey (`[machine] mem_controller`, fitted by the
  A3000/A4000 profiles, which also fit their stock 4M of this RAM by
  default) and a 32-bit CPU, and must fill whole Ramsey banks: 1M-4M in
  1M steps, or 8M, 12M, 16M. On the A4000 (Ramsey-07), sizes beyond 16M
  keep growing downward into the `$04000000`-`$06FFFFFF` motherboard RAM
  expansion space, in 4M steps up to 64M (which reaches `$04000000`).
  Set `motherboard = "0"` to remove it.
- **Accelerator RAM** is CPU-slot local memory: it starts at `$08000000`
  and grows upward through the coprocessor-slot expansion space, up to
  128M (ending at `$10000000`, where Zorro III space begins). This is the
  RAM an accelerator/CPU board carries, so it needs a 32-bit CPU but no
  particular machine profile; any whole number of megabytes fits.
  Kickstart sizes it with its own probe, like the motherboard bank.
- **Z3 RAM** requires a 68020/68030/68040/68060 (a 24-bit bus cannot reach it);
  Kickstart assigns its base address, usually `$40000000`.

Additional expansion boards can be described with `[[zorro]]` metadata
files; see [](../zorro).

## `[chipset]`

```toml
[chipset]
revision = "OCS"   # OCS, ECS, or AGA preset
video = "PAL"      # PAL or NTSC
# agnus = "8372A"  # optional fine-grained override
# denise = "OCS"   # optional fine-grained override
```

`revision` is a preset; `agnus` and `denise` allow the mixed configurations
real machines shipped with (a late A500 with an ECS Agnus but OCS Denise,
for example):

- `agnus`: `OCS`/`8370`/`8371` (OCS), `8372`/`8372A` (ECS, 1M chip),
  `8375`/`8372B` (ECS, 2M chip), `8374`/`ALICE` (AGA).
- `denise`: `OCS`/`8362`, `ECS`/`8373`, `LISA`/`4203`.

The ECS preset picks an 8372A for up to 1M chip RAM and an 8375 above; the
A600 profile always uses the 8375 as the real machine did. The AGA preset
resolves to Alice and Lisa: 8 bitplanes, the 256-entry 25-bit palette with
BPLCON3 BANK/LOCT banking, HAM8, FMODE wide bitplane and sprite fetch
(DMA and manual sprites), SSCAN2/BSCAN2 scan doubling, BPLCON4, and
CLXCON2 (remaining gaps, such as true 35 ns SuperHires sprite output, are
recorded in [](../internals/chipset)).

## `[display]`

```toml
[display]
overscan = "tv"       # "tv" (default) or "full"
tv_h_centre = 0       # TV picture centring in lo-res pixels, -16..16 (+ = right)
tv_v_centre = 0       # TV picture centring in scan lines, -8..8 (+ = down)
pixel_aspect = "tv"   # "tv" (default, 4:3 CRT) or "square" (exact 2x2 lo-res)
scaling = "smooth"    # "smooth" (default, aspect fit) or "integer" (whole multiples)
deinterlace = true    # motion-adaptive interlace weaving (default true)
phosphor = 0.0        # CRT persistence fraction, 0.0 (off) to 0.95
shader = "none"       # "none" (default), "scanlines", "mask", "crt", or a .wgsl file
shader_strength = 1.0 # how strongly the shader is mixed in, 0.0-1.0
tint = "none"         # "none" (default), "bw", "green", "amber", or "sepia"
menu_scale = "1x"     # size of the pop-up menu: "1x" (default) or "2x"
full_screen = false   # open fullscreen at start (default false)
status_bar = true     # show the status bar at start (default true)
```

The emulated framebuffer always carries the full overscan field Denise
produces. `"tv"` presents what the monitor's glass shows: the captured
aperture -- the standard window plus the symmetric overscan margin the
framebuffer captures on its right edge -- fills the whole 4:3 glass, the
way a real set's raster overscans its screen, so the picture (border
colour included) reaches every edge with no black bezel columns. The
live window and PNG screenshots / `--dump-frames` present the same
716x540 glass; PAL and NTSC scans share the one shape because both
apertures fill the same glass -- an NTSC scan's shorter crop (the
200-line standard window plus the same overscan margin) is scaled onto
the same output rows. `"full"` shows everything, which
is useful when debugging display alignment. `COPPERLINE_OVERSCAN=full|tv`
overrides this for a single run. In both modes the presentation geometry
holds steady across the blank frames a screen change produces: a frame
showing only border colour keeps the previous frame's aperture and
centring instead of snapping to the full framebuffer, so the picture does
not jump sideways at Kickstart screen changes.

`tv_h_centre` / `tv_v_centre` nudge where the TV presentation centres the
picture on the glass -- the H-CENTER/V-CENTER controls a real monitor
carried on its front. `tv_h_centre` is in lo-res pixels, positive moving
the picture right; `tv_v_centre` in scan lines, positive moving it down.
The default aperture centres the standard window, which leaves most of
the captured left overscan off-glass; software that leans its artwork
into that overscan (the CD32 boot logo's leading "A" serif, for example)
comes fully into view a few pixels of `tv_h_centre` later, exactly as it
did on a set whose picture sat a little right of centre. Glass the nudge
exposes beyond the captured raster is unscanned and shows black. The
knobs move the TV aperture, so `overscan = "full"` ignores them; captures
(screenshots, frame dumps) follow them like the live window. The menu's
*Screen Centring* rows (under Video Settings) step the same knobs live
without touching the config.

`pixel_aspect` selects how emulated scanlines map to host rows. The default
`"tv"` presents the field with the non-square pixel aspect of a 4:3 CRT:
the full overscan scan fills a 4:3 picture, so PAL lo-res pixels come out
slightly wider than tall, exactly as a real TV shows them (a 320x256 screen
spans about 640x482 window pixels). `"square"` uses one host row per woven
scanline instead, so every low-resolution pixel is an integer 2x2 square
and a 320x256 PAL screen occupies precisely 640x512 window pixels --
slightly taller than a real CRT picture, but exact for side-by-side pixel
comparison with square-pixel emulators. The menu's *Pixel Aspect* item
flips the mode live without touching the config, and
`COPPERLINE_PIXEL_ASPECT=tv|square` overrides it for a single run.

`scaling` selects how that presentation canvas reaches the window, which is
a separate question from what the canvas is. The default `"smooth"` fits the
canvas to the window preserving its aspect ratio and interpolates, so the
picture always uses the full window height (or width) whatever fraction the
scale works out to. `"integer"` instead draws the canvas at the largest
whole-number multiple of itself that fits the window, measured in physical
device pixels, centred in black borders and point-sampled: every canvas
pixel becomes the same square block of host pixels, with no row or column
sampled twice, which is the look WinUAE and Amiberry call integer scaling.
The fit is taken in whole canvas pixels against the physical surface --
the canvas is re-rendered at whatever factor fits, rather than drawn at
whole multiples of a fixed high-DPI texture -- so every step exists on
every display: a 2x-DPI laptop whose screen holds three physical pixels
per canvas pixel but not four gets the 3x picture, and fractional desktop
scales such as 150% take their whole physical multiples the same way. The
status bar and menus are rendered at the fitted factor too, so they stay
sharp at any step (the factor is capped at 4x; larger fits continue as
whole multiples of the 4x canvas). Only when the window is too small for
even a 1:1 copy -- smaller than the canvas itself in physical pixels --
does the picture fall back to the smooth fit rather than cropping to what
fits. RTG board modes follow the setting too: their frame is scaled
from its own native resolution, so a 640x480 board screen is drawn at 1x,
2x, 3x of *those* pixels inside the display area.

`pixel_aspect = "square"` with `scaling = "integer"` is the fully
pixel-exact combination: the square-pixel canvas is one host row per woven
scanline, so a whole-number window scale carries the emulated bitmap to the
screen untouched. Integer scaling of the default TV aspect is still crisp,
but crisp pixels of an already-resampled image -- that canvas fits the scan
onto 537 rows for the 4:3 shape before presentation. The monitor-bezel mode
(`bezel`) composes with either, but its picture opening is a fraction of
the window by design and is not itself integer-exact. The menu's *Video
Settings > Scaling* item switches modes live without touching the config;
there is no environment-variable override.

`deinterlace` controls how interlaced (LACE) displays are presented. On
(the default), a motion-adaptive deinterlacer weaves the two fields into a
full-height picture where the content is static and interpolates where it
moves, recovering the full vertical resolution without combing. Off, every
field is simply line-doubled as it arrives, which shows interlace bob and
flicker much as a TV without persistence would. `COPPERLINE_DEINTERLACE=0`
overrides the config for a single run.

`phosphor` blends each presented frame with a fraction of the previous
one, approximating the exponential decay of CRT phosphor. Software that
relies on the tube to fuse field-rate flicker -- alternate-field dither
transparency or flicker-dithered animation -- reads as intended with values
around `0.3`-`0.5`,
at the cost of a slight motion trail. Off by default so screenshots and
frame dumps stay frame-exact. `COPPERLINE_PHOSPHOR=0.4` overrides the
config for a single run.

`shader` runs a GPU shader pass over the window's picture, for the tube
look a phosphor trail on its own cannot give. Three presets are built in:

- `"scanlines"` -- the line structure a 15 kHz set leaves between beam
  passes: a raised-cosine gap at the pitch of the emulated lines, with the
  brightness the gaps cost compensated back so the picture dims only
  slightly rather than by half.
- `"mask"` -- a shadow mask. The picture is modulated through staggered RGB
  phosphor triads keyed to physical window pixels, so the mask keeps its
  size whatever the Amiga resolution behind it, again
  brightness-compensated.
- `"crt"` -- the lot, in the spirit of the 1084 the Amiga shipped with: a
  bowed tube face, scanlines, an aperture grille, and a corner vignette,
  all faded in together. The face geometry is taken from the datasheet of
  the 1084's picture tube (the Philips M34EAQ10X): the bow reproduces its
  published screen-edge arcs -- the top and bottom edges bow about twice
  as far as the sides, as they do on the real screen -- and the corners
  are rounded at the scale of its 11.6 mm corner arcs. The bow shapes
  only the face outline, not the picture: a real monitor's deflection is
  corrected so the raster stays rectilinear on the curved glass, and
  straight content stays straight here too. The picture overscans the
  face like the real raster overscans the glass, filling it to the
  edges, with the bowed outline deepening the crop toward the rounded
  corners, and on-face black is lifted by a faint glass glow, the room
  light a real tube reflects, so the face keeps its silhouette even when
  the picture is dark.

`"none"` (the default; `"off"` is accepted for the same thing) presents the
picture untouched, and any value ending in `.wgsl` is the path of a shader
of your own -- see [Custom WGSL shaders](#custom-wgsl-shaders) below.

The scanline gaps are drawn at the pitch of the emulated field lines the
window is actually showing: 270 in the default TV-overscan presentation
(214 on an NTSC scan) and 285 in `"full"`, so the line structure follows
the picture rather than the window size. TV overscan with `pixel_aspect = "square"` is 285 as well --
that canvas is taller than the TV aperture and pads it with bezel rows, so
the same 270 lines are rescaled to keep their pitch across the whole
window. Interlaced content is deliberately drawn at field-line pitch
over the woven frame, which is what a 15 kHz set fed an interlaced signal
looks like, rather than one gap per woven row.

`shader_strength` (0.0 to 1.0, default 1.0) is how strongly the effect is
mixed in, so a preset can be dialled back without editing shaders. At `0.0`
the shader arithmetic is an exact no-op, but the pass still resamples the
picture through a plain bilinear sampler, which is a shade softer than the
texel-snapped pass-through the window otherwise uses at magnification.
`"none"` skips the pass altogether and is the only truly zero-cost setting.

The menu's *CRT Shader* item cycles the presets live for the rest of the
session without touching the config file; the launcher's *A/V & Emu* tab
(*Video* category) has *CRT shader* and *Shader strength* rows that do
write it.
`COPPERLINE_SHADER=crt|scanlines|mask|none|PATH.wgsl` and
`COPPERLINE_SHADER_STRENGTH=0.0..1.0` override the config for a single run.
There is no command-line flag.

The pass is presentation and nothing else. Screenshots
(`--screenshot-after`), frame dumps (`--dump-frames`), video recordings, the
[control protocol](../debugger/control.md)'s capture methods and the web
frontend all read the CPU presentation buffer, which the shader never
touches, so captures stay comparable whatever is selected here. Individual
frames also skip the pass in three cases: while a menu or overlay panel is
open (a phosphor mask and a curved face make overlay text unreadable), for
frames coming from an RTG board's scanout (see `[rtg]` below), and for
programmable multisync scan modes -- a 31 kHz scanout has no 15 kHz line
structure to reproduce.

`bezel` frames the window's picture with a monitor front, drawn at the
window's resolution so it stays sharp at any size, with the picture keeping
its aspect inside the opening. Three settings:

- `"off"` (the default) -- no frame; the picture fills the display.
- `"1084"` -- the monitor the Amiga shipped with: a pale cabinet with a
  darker moulding sunk around the tube, the model badge, the Copperline
  name where the 1084 wore its maker's, and a red power lamp.
- `"classic"` -- the plainer rounded frame Copperline drew before the 1084
  arrived.

`true` and `false` are still read, from when there was only the one frame
to turn on; `true` means `"1084"`.

A drawn bezel also changes what its tube shows: the glass presents the
whole captured raster -- every rendered line, with all the overscan border
the framebuffer captures -- rather than the tighter TV aperture the plain
window crops to. The raster fills the glass edge to edge the way a real
set's overscanned picture does, border colour and all, and the opening's
rounded corners crop into that border instead of into the picture, which
the extra border keeps clear of the arcs.

The bezel is independent of `shader` and composes with any preset: with
`"crt"` the bowed tube face sits inside the opening for the full monitor
look. Cmd+M (macOS) / Alt+M turns it off and back on to whichever front is
chosen, for the rest of the session and without touching the config;
*Video Settings > Monitor Bezel* picks the front, the launcher's *Monitor
bezel* row (*A/V & Emu*, *Video*) writes it, and `COPPERLINE_BEZEL` (a
style name, or the old `1`/`0`) overrides the config for a single run.

Like the shader pass, a bezel is presentation and nothing else: captures
never include it, and it is skipped while a menu or overlay panel is open
and for RTG scanout frames. Unlike the shader it does stay on for
programmable multisync scans -- a frame has no line structure to get wrong.

`bezel_stickers` names a folder of PNG images to draw onto the bezel as
die-cut stickers, the way owners dress a real monitor with community and
maker logos. Unset (the default) draws none. Every `*.png` in the folder
becomes one decal (up to 16, alpha respected, large sources scaled down),
laid along the cabinet's top band in file-name order with a slight
alternating tilt; each picks up a soft drop shadow and the plastic's
lighting so it reads as stuck on rather than pasted over. The folder is
re-read when a machine starts, so editing it and restarting (or switching
machine in the launcher) picks up changes; a folder that fails to load
reports on the on-screen display and falls back to bare plastic.

An optional `stickers.toml` in the folder chooses and places the images
instead, one `[[sticker]]` table per decal, drawn in written order:

```toml
[[sticker]]
image = "retro32.png"   # file name in the folder
x = 0.32                # sticker centre, fraction of the front's width
y = 0.935               # and of its height (down from the top)
width = 0.11            # width as a fraction of the front's width
rotate = -2.5           # degrees clockwise
opacity = 1.0

[[sticker]]
image = "badge.png"     # no x/y: an auto slot on the top band
```

`x` and `y` come as a pair; a table without them takes the next automatic
top-band slot (where `width` and `rotate` still override that slot's own).
Height always follows the image's aspect. The same keys, as JSON, drive
the web player's `#bezel-stickers` page hook
([the browser chapter](browser.md)), so one sheet lays out identically in
the app and on a hosting page.

Stickers ride the bezel: they draw only while a front does, follow it
through Cmd+M / Alt+M, and are presentation only -- captures never include
them. `COPPERLINE_BEZEL_STICKERS` (a folder path, or empty for none)
overrides the config for a single run.

`perf_overlay` (default `false`) shows the performance overlay at start: a
live readout of emulated fps, speed factor, per-frame emulation cost, host
utilisation, audio health, and pacer slips in the top-right corner of the
display, one line per data point (see
[the window chapter](ui.md#performance-overlay) for what each line means).
Cmd+P (macOS) / Alt+P toggles it live for the rest of the session without
touching the config, `--perf-overlay` shows it for one run, and
`COPPERLINE_PERF_OVERLAY=1|0` overrides the config for a single run; the
launcher's *Perf overlay* row (*A/V & Emu*, *Video*) writes it. Like the
transient message overlay it is presentation only: screenshots, frame
dumps, and recordings never include it.

`tint` recolours the picture like the phosphor of a monochrome monitor:
`"bw"` (black and white), `"green"` and `"amber"` (the two classic
monochrome phosphors), or `"sepia"`; `"none"` (the default; `"off"` is
accepted) presents full colour. The same five looks the web frontend's
*Screen* selector offers, produced by the same colour chain, so a tint
chosen in the browser matches the desktop. It composes with `shader` --
green phosphor under the `crt` preset makes a convincing monochrome tube.
Like the shader, the tint is presentation only: screenshots, frame dumps,
recordings and headless runs stay untinted, the status bar and overlay
menus keep their colours, and RTG board scanout (the monitor on the
board's own output, not the Amiga's video output) is never tinted. The
menu's *Screen Tint* item cycles the tints live for the rest of the
session without touching the config file; the launcher's *A/V & Emu* tab
(*Video* category) has a *Screen tint* row that does write it.
`COPPERLINE_TINT=bw|green|...` overrides the config for a single run.

### Custom WGSL shaders

Pointing `shader` at a `.wgsl` file loads a fragment shader of your own into
the same pass. The quickest start is to copy one of the presets --
`src/video/window/shaders/scanlines.wgsl`, `mask.wgsl` or `crt.wgsl` in the
source tree -- and edit its `fs_main`: everything above the
`--- end shared contract ---` marker in those files is the contract, and is
byte-identical in all three.

A shader must declare exactly these bindings and both entry points:

```wgsl
struct CrtUniforms {
    // Display sub-rect of src_tex in UV space: xy origin, zw size.
    src_rect: vec4<f32>,
    // xy: viewport size in physical pixels. zw: source display texels.
    size: vec4<f32>,
    // x: strength, 0 (no-op) to 1 (full). y: scanline count across the
    // display height. zw: preset-internal, do not rely on them.
    params: vec4<f32>,
    // Preset-internal and reserved; zero for a custom shader.
    params2: vec4<f32>,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;
@group(0) @binding(2) var<uniform> u: CrtUniforms;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VOut {
    // Fullscreen triangle; the viewport restricts it to the display rect.
    let tc = vec2<f32>(f32((idx << 1u) & 2u), f32(idx & 2u));
    var out: VOut;
    out.uv = tc;
    out.pos = vec4<f32>(tc * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

// Sample the display region only, clamped half a texel inside src_rect so
// a linear tap on the bottom edge never blends in the status bar's first
// row underneath it.
fn sample_display(uv: vec2<f32>) -> vec4<f32> {
    let half_texel = 0.5 * u.src_rect.zw / max(u.size.zw, vec2<f32>(1.0));
    let lo = u.src_rect.xy + half_texel;
    let hi = u.src_rect.xy + u.src_rect.zw - half_texel;
    let tc = clamp(u.src_rect.xy + uv * u.src_rect.zw, lo, hi);
    return textureSample(src_tex, src_samp, tc);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let uv = clamp(in.uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let base = sample_display(uv);
    let strength = clamp(u.params.x, 0.0, 1.0);

    // Your own look goes here. This one is a green monochrome monitor with
    // a gap between beam passes at the emulated line pitch.
    let lines = max(u.params.y, 1.0);
    let profile = 0.5 - 0.5 * cos(6.283185307 * uv.y * lines);
    let luma = dot(base.rgb, vec3<f32>(0.299, 0.587, 0.114));
    let tube = vec3<f32>(0.2, 1.0, 0.35) * luma * (0.6 + 0.4 * profile);

    // Mixing back toward the untouched sample keeps strength 0 a no-op.
    return vec4<f32>(mix(base.rgb, tube, strength), 1.0);
}
```

Points worth knowing:

- All three bindings are fragment-visibility only. `vs_main` cannot read
  them, so all the work happens in `fs_main`.
- Sampling goes through `src_rect`. The pass draws over the display
  rectangle of a texture that also carries the status bar below it, and the
  half-texel inset is what keeps the status bar's separator hairline out of
  the bottom of a magnified picture.
- Of the uniforms, a custom shader can count on `src_rect`, `size`,
  `params.x` (strength) and `params.y` (scanline count). `params.z`,
  `params.w` and `params2` carry the built-in presets' own look parameters,
  are zero for a custom shader, and are reserved for future use.
- Making strength `0.0` a visual no-op is a convention, not something the
  loader enforces, but the *Shader strength* control and
  `COPPERLINE_SHADER_STRENGTH` are only useful if you honour it.

The file is read and checked when the window is created, when the launcher
starts a machine, and every time *Video Settings > CRT Shader > Custom* is
chosen -- which re-reads it from disk. That is the live-reload story: leave
the emulator running, edit the shader, then pick **Custom** again to see the
new version.

Checking is a parse, a full validation, and a look for the two entry points,
all before any GPU pipeline is built, so a mistake is reported as WGSL with
its line and column rather than as a driver error. Files over 1 MiB are
refused unread, on the grounds that a shader that big is a mistyped path.
Whatever goes wrong -- a missing file, a syntax error, a missing `fs_main` --
the full diagnostic goes to the log, a one-line summary appears in the
window's on-screen message, and the shader falls back to off. A bad custom
shader never fails the config, and never stops the machine from running.

`menu_scale` draws the pop-up menu at `"1x"` (the default) or `"2x"` -- the
whole menu, rows and text together. It is a start-up preference:
*Video Settings > Menu Size* changes it live without altering the saved
value, `--menu-scale` sets it on the command line, and the launcher's A/V &
Emu page (Video category) has a *Menu size* picker for the same.

`full_screen` opens the window fullscreen at start (borderless), and
`status_bar` chooses whether the status bar starts visible. Both are start-up
preferences; the runtime toggles -- `Cmd+F` / `Alt+F` for fullscreen and
`Cmd+Shift+F` / `Alt+Shift+F` for the status bar, plus their menu items --
still flip either live without changing the saved value. On the command line
`--full-screen` / `--windowed` set the fullscreen state and `--show-status-bar` /
`--hide-status-bar` set the status bar; the launcher's A/V & Emu page (Video
category) has *Start fullscreen* and *Status bar* toggles for the same. Left
unset they keep the defaults: windowed, status bar shown.

Rendering completed frames uses a worker thread by default so emulation can
advance while the previous frame is painted. The worker is an implementation
detail of presentation: screenshots, frame dumps, and recordings wait for
the exact frame they save. `COPPERLINE_THREADED_RENDER=0` forces the old
synchronous render path for comparison.

## `[audio]`

```toml
[audio]
floppy_sounds = true        # synthesized drive sounds (not sampled)
floppy_sounds_volume = 100  # 0-100, relative to Paula's output
# output_device = "..."     # host output device (substring); omit = system default
# output_enabled = true     # false = no sound (GUI "Disabled"); --audio/--noaudio still win
channel_mode = "stereo"     # "stereo" (default) or "mono"
stereo_separation = 100     # 0-100; 100 = hardware panning, 0 = mono
audio_filter = "auto"       # Paula filter: "auto" (guest-driven), "on", or "off"
# stem_granularity = "master,source"  # default --audio-stems-mode; see below
```

The drive sounds are generated from scratch: motor hum with spin-up/down
over a rumble that repeats with each platter revolution, and head-step
clacks (an isolated step -- the empty-drive poll, or the track-to-track
advance while loading -- lands with its rebound clatter, and fast
multi-track seeks blur into the characteristic buzz). Reading adds no
noise of its own; the loading sound is the step rhythm over the spinning
motor, as on the real mechanism. The synthesis targets were measured
from recordings of real Amiga drive mechanisms, but no sample data is
used. Only step pulses that actually fire the stepper are audible: like
a real 3.5" mechanism, an outward pulse with the head at track 0 is
gated by the /TRK0 sensor, so NoClick-style patches silence the
empty-drive poll just as they do on real hardware.

`output_device` picks the host output by a case-insensitive substring of the
names `--list-audio-devices` prints (`--audio-device` overrides it); an omitted
or unmatched name uses the system default. `channel_mode = "mono"` averages the
left and right output into both channels, and `stereo_separation` narrows the
Amiga's hardware left/right panning between full (100) and mono (0) -- so it is
ignored when `channel_mode` is mono. `output_enabled = false` runs with no sound
at all (the launcher and runtime-menu "Disabled" option); the `--audio` and
`--noaudio` CLI flags still override it. These are host-output settings that do
not change the emulated audio and are not stored in save states. The equivalent
CLI flags are `--audio-device`, `--audio-channel-mode`, `--audio-stereo-separation`
and `--list-audio-devices`.

`audio_filter` controls Paula's analogue low-pass filter, the one a
post-A1000 Amiga switches with the same CIA-A line that drives the power
LED. `"auto"` (the default) lets the guest engage or bypass it as the
software asks, matching real hardware; `"on"` and `"off"` force it either
way as a listener override. Unlike the host-output settings above it is
part of the emulated audio path, so it also affects WAV capture. Also on
`--audio-filter`, *Audio Settings > Audio Filter* in the menu, and
Cmd/Alt+A.
The status-bar PWR LED is lit whenever the machine is powered and follows
the guest's /LED line itself -- full brightness while engaged, dimmed like
an A500 rev 6+ board while released -- so this override changes what you
hear, never the LED.

On Linux with PipeWire/PulseAudio, individual sinks are not ALSA devices, so
only the `default`/`pipewire` route is offered; pick the output in the desktop
sound settings (or route Copperline in `pavucontrol`) and it follows. macOS and
Windows select each device directly.

`stem_granularity` sets the default granularity list for `--audio-stems`
(headless-only; see [](headless.md)) so it doesn't need `--audio-stems-mode`
repeated on every invocation -- the CLI flag still wins when both are given.
It has no effect without `--audio-stems DIR` on the command line.

## `[input]`

```toml
[input]
port1 = "mouse"           # mouse | joystick | cd32 | analogue | none
port2 = "joystick"        # same values; default "cd32" on the CD32 profile
joystick = "gamepad"      # "gamepad" (default) or "keyboard"
mouse_sensitivity = 50    # host mouse speed 0-100 (50 default = 1:1)
mouse_capture = "click"   # when to grab the mouse: click | auto | manual
autofire_hz = 0           # pulse a held fire button at this rate; 0 = off
```

### Port devices

`port1` and `port2` name the controller device plugged into each game port.
Either port accepts any device, exactly as on real hardware:

- `mouse` -- a quadrature mouse. Its three buttons are the left (`/FIRx`),
  right (`POTxY`), and middle (`POTxX`) lines.
- `joystick` -- a digital switch joystick with a fire button and a second
  button on the `POTxY` line.
- `cd32` -- a CD32 joypad: a digital joystick plus the serial button
  protocol lowlevel.library reads (Red/Blue ride the fire/button-2 lines;
  Green, Yellow, Play, Rewind and Forward exist only serially).
- `analogue` -- analogue paddles or a proportional stick presenting
  resistances on the `POTxX`/`POTxY` pins, with the two paddle buttons on
  the left/right direction lines. No live host device maps to it yet:
  drive it with `--pot-after` scripting or the control protocol's
  `input.analogue` method (positions default to centre).
- `none` -- an empty port.

The defaults are today's stock wiring: a mouse in port 1 and a joystick in
port 2 -- a CD32 pad on the CD32 profile, whose bundled controller the
machine expects (an explicit key beats the profile; a real CD32 accepts any
controller too). `--port1` / `--port2` override for one run, the runtime
menu's **Port 1/2 Device** items hot-plug a device live, and the control
protocol's `input.set_port` does the same from a script.

Putting joysticks in *both* ports is a real two-player setup: the host
gamepad and the keyboard mapping then drive one port each (see below).

### Joystick input source

`joystick` selects the initial host source for the joystick/CD32-pad port.
There are two explicit modes, so the active source is always visible rather
than depending on whether a pad happens to be connected:

- `gamepad` (the default) -- only a physical pad drives the joystick port.
  The keyboard is left to the Amiga, so it passes straight through to a
  Shell, an editor, or Workbench, and no keys are unexpectedly captured as
  joystick input. With no pad connected there is simply no joystick input.
- `keyboard` -- use the keyboard-joystick mapping (cursor keys plus the fire
  keys), so the port stays usable without a controller.

With one joystick/CD32-pad port the mode picks its source. With two, both
sources are in play -- the gamepad and the cursor-key mapping drive one
port each -- and the mode picks which source gets the lower-numbered port;
whenever no physical pad is present, a second keyboard mapping on the
numeric keypad (`8`/`2`/`4`/`6` directions, `0` fire, `.` second button)
stands in for the gamepad, so two players can share one keyboard.

The keyboard mapping drives whatever device its port carries. In
particular, with mice in *both* ports the host mouse takes the
lower-numbered one and, in `keyboard` mode, the cursor-key mapping drives
the second as an emulated mouse: cursor keys move the pointer, the fire
keys are the left button, `X` the right, `D` the middle.

This only sets the starting mode. The status-bar toggle (the gamepad /
keyboard icon next to the volume control), `Cmd+J` / `Alt+J`, the menu's
**Joystick Input** item, and the launcher's *Input* tab all flip it live
without changing the config. `--joystick MODE` overrides this for a single
run. (`auto` is still accepted here as a backward-compatibility alias for
`gamepad`; the old auto-detect mode has been removed.)

`mouse_sensitivity` scales how fast the emulated pointer tracks the host
mouse, 0-100. `50` (the default, shown as *Default* in the GUI) is 1:1 --
exactly the previous behaviour -- `0` is a quarter speed and `100` quadruple,
on an exponential scale so each step is an even ratio. It is a host-input
scale applied to live mouse motion only: it never touches the emulated machine
or scripted `--mouse-after` input, so headless and recorded runs stay
deterministic. Set it from the launcher's *Input* tab or
`--mouse-sensitivity N`, and adjust it live with `Cmd+Shift+>` / `Cmd+Shift+<`
(`Alt+Shift+>` / `Alt+Shift+<` on Linux and Windows), which ramp while held.

### Mouse capture

Capturing the mouse confines the host pointer to the window and hides the
host cursor, so the Amiga pointer is the only one on screen. `mouse_capture`
decides when that grab is taken:

- `click` (the default) -- clicking the display grabs it. That click is a
  window action and is not passed to the Amiga, so the first click the guest
  sees is the first one aimed at it.
- `auto` -- grab as soon as the window has the focus, and again whenever it
  regains it, so no host cursor is ever loose over the display. Entering
  fullscreen grabs too. This suits a mouse-driven game or a fullscreen
  session where the host desktop is not wanted.
- `manual` -- only the shortcut grabs. Clicks on the display go straight to
  the Amiga and the host cursor is left alone.

`Cmd+G` / `Alt+G` releases and re-takes the grab by hand in every mode, and
an explicit release is never undone automatically. Opening a panel or tool
window borrows the cursor and hands the capture back when the last one
closes.

Uncaptured, host cursor motion over the display still drives the emulated
mouse in every mode; this setting only decides when the grab is taken, not
whether motion reaches the machine. Set it from the launcher's *Input* tab
or `--mouse-capture MODE`.

### Autofire

`autofire_hz` turns a *held* fire button into a pulse train at that many
presses per second; `0` (the default) leaves the button alone. It applies to
live input only -- the gamepad and the keyboard mapping -- and never to
scripted input (`--joy-after`, `--script`, the control protocol's
`input.joy`), which must replay exactly the events it was given.

The phase comes from emulated time, so the rate is the same under warp and
on PAL or NTSC. Nothing about the emulated machine changes: the port sees an
ordinary button being pressed and released on `/FIRx`. Only the fire button
is pulsed; directions and the second button pass through untouched.

`--autofire HZ` sets it for one run, and the menu's **Autofire** item cycles
off / 3 / 5 / 8 / 12 / 16 Hz live. The maximum is 30 Hz -- above that the
assert window is shorter than the frame the guest samples the port on.

### Remapping the keyboard controller

The keyboard-to-controller bindings are a host preference, not part of the
emulated machine, so they live beside the gamepad calibration rather than in
a machine config: the menu's **Input Mapping...** item edits them and Save
writes `keymap.toml` next to `gamepads.toml` (see
[Gamepad calibration](ui.md#gamepad-calibration) for the per-platform
location). Any control may take several keys -- fire ships with four aliases
so compact keyboards without the right-hand modifiers still work -- and
binding a key removes it from wherever it was before, including from the
other mapping, so the two controllers can never fight over one key. Deleting
the file restores the built-in layouts, as does the panel's **Defaults**
button.

## `[serial]` -- serial port and MIDI

```toml
[serial]
mode = "stdout"          # off, stdout, midi, tcp, tcp-connect, pty, or modem
# midi_out = "FluidSynth"  # midi mode: host destination, "mt32", or "coppersynth"
# midi_in = "Keystation"   # midi mode: host source, or "mt32"
# listen = "127.0.0.1:1234"  # tcp mode: bind address
# connect = "bbs.example.com:1337"  # tcp-connect mode: remote to dial
```

The Amiga serial port doubles as the MIDI port. `mode` selects where
Paula's serial in/out is connected:

- `stdout` (the default) -- serial output prints to the host terminal,
  matching the historical behaviour (DiagROM and similar tools log here).
- `off` -- serial output is discarded and there is no serial input.
- `midi` -- serial in/out is bridged to host MIDI endpoints. Needs a build
  with the `midi` feature (the default); `midi_out`/`midi_in` name the
  endpoints by case-insensitive substring (a USB interface or a virtual
  port). `--list-midi` prints the host endpoints. Either may instead be
  `"mt32"`, which is Copperline's own emulated MT-32 rather than anything
  on the host, and brings its own `mt32_*` keys with it; see
  [the MT-32 chapter](mt32.md). `midi_out` may also be `"coppersynth"` --
  Coppersynth, the built-in General MIDI synthesizer, which needs no
  ROMs at all and brings its own `coppersynth_*` keys with it; see
  [the Coppersynth chapter](coppersynth.md).
- `tcp` -- serial in/out is bridged to a host TCP port, like UAE's `TCP:`
  device. `listen` sets the bind address (default `127.0.0.1:1234`);
  connect with e.g. `nc`, `socat`, or a raw-mode telnet client.
- `tcp-connect` -- the outbound counterpart of `tcp`: at startup the
  serial port dials the remote named by `connect` (required, `host:port`)
  and the session talks to that service. Point a guest terminal program
  at a telnet BBS, a `tcpser` modem bridge, or any TCP byte service. The
  connection is made once; if the remote hangs up, output drops like an
  unplugged cable until the next run. Note that the wire carries raw
  bytes: for telnet servers that insist on option negotiation, put a
  telnet-aware relay in between, or pick a BBS/port that accepts raw
  connections (most do).
- `pty` -- serial in/out is bridged to a host pseudo-terminal (Unix only).
  The slave path (`/dev/pts/N`) is logged at startup; attach a terminal
  with e.g. `minicom -D`, `screen`, or `cu -l`.
- `modem` -- a Hayes/AT-command modem personality: the guest dials out with
  `ATD host:port` rather than the port being wired to a peer at startup, so
  (unlike `tcp-connect`) there is no `connect` address in the config; carrier
  presence drives /CD.

With an `AUX:` shell on the Amiga side, `tcp`/`pty` give a remote AmigaDOS
console. `--serial MODE` overrides the mode per run,
`--serial-connect HOST:PORT` sets the dial-out target (and implies
`mode = "tcp-connect"`), and `--midi-out NAME`/`--midi-in NAME` imply
`mode = "midi"`. The launcher's **I/O Ports** tab (Serial Port page) sets all
of this interactively: **Device / Mode** picks the mode, and the mode brings
its own address box with it -- **Connect** under `tcp-connect` for the
remote to dial, **Listen** under `tcp` for the local bind address (it shows
the `127.0.0.1:1234` default until something else is typed). Either box
takes a `host:port`, with an IPv6 literal in brackets
(`[::1]:1337`); clearing it unsets the key. The in-window
**MIDI In / MIDI Out** menu items select the MIDI endpoints.

The browser build has its own serial transport (the page bridges the port
to a WebSocket); see [the browser chapter](browser.md).

## `[parallel]` -- Centronics parallel port

```toml
[parallel]
device = "printer"           # none | printer | sampler
output = "printer.raw"       # printer capture path
# device = "sampler"
# sampler_input = "MacBook Air Microphone"  # host input; omit for the default
# sampler_gain = 6.0                          # preamp gain in dB (0 = unity)
```

`device` chooses the peripheral on the parallel port (one at a time). Without
this section the connector is electrically disconnected: CIA-A still produces
its hardware `PC` strobe on port-B accesses (`$BFE101`), but no peripheral
acknowledges it and port-B reads see the CIA's own pins. The equivalent per-run
flags are `--parallel DEVICE`, `--sampler-audio-input NAME`, and
`--sampler-input-gain X`; `--sampler-list-audio-inputs` prints the input-device
names and exits.

`"printer"` attaches a raw Centronics sink at `output` (a bare `output` with no
`device` still selects the printer, for compatibility). The file is created at
startup, replacing any existing file; each strobed byte is written verbatim and
returns the printer `/ACK` falling edge through CIA-A `FLAG`, including the
normal CIA interrupt delay. The printer also drives the Centronics status
lines on CIA-B port A -- SEL high, BUSY and POUT low -- so the guest's
`parallel.device` sees a ready online printer and starts sending. (Without an
attached device those lines float high, and printing waits forever for a
printer to appear, as on a real machine.) It is intentionally not decoded, since the guest may
emit any printer language; pass it to a converter or spooler afterwards.

`"sampler"` attaches an 8-bit audio sampler (digitizer) on the data lines -- the
emulated equivalent of a classic parallel-port sampler cartridge, driving
software such as AudioMaster, ProTracker, OctaMED, and TurboSound. It captures
from a host input device (cpal, like live audio output, so it needs a build with
the `frontend` feature) and presents each read of the data lines as an 8-bit
offset-binary sample in emulated time, mono (host left/right are summed).
`sampler_input` names the host device (case-insensitive substring, as
`--sampler-list-audio-inputs` prints; omitted uses the system default);
`sampler_gain` is the preamp gain in decibels (0 dB = unity) applied before the
ADC, clamped to the sampler's range (roughly -24 to +24 dB). The input device
and gain can also be changed live
from the runtime menu, and the gain with `Cmd/Alt+Shift +/-`. On macOS the CLI
binary needs microphone permission to capture a real input; routing audio in
through a loopback device such as BlackHole needs none.

## `[floppy]` and `[floppy.df0]` .. `[floppy.df3]`

```toml
[floppy]
drives = 2                 # DF0 and DF1 connected; default is DF0 only
speed = 100                # 100/200/400/800 percent, or 0 for turbo

[floppy.df0]
path = "demo.adf"            # single image, or:
# paths = ["disk1.adf", "disk2.adf"]   # swap playlist (shortcut cycles)
write_protected = true       # default true
# enabled = true             # implied by path/paths
```

`drives` controls how many mechanisms are wired, from one to four. DF0 is
the internal drive; DF1-DF3 are external drives that answer the standard
Amiga external-drive ID protocol when connected. A configured disk image
also connects that drive automatically, so existing configs that name
`[floppy.df1]` .. `[floppy.df3]` keep working.

`speed` accelerates the emulated drives beyond the authentic data rate.
`100` (the default) is real speed. `200`, `400`, and `800` clock the whole
data path -- platter rotation, the MFM read shifter, sync detection,
DSKBYTR, and DMA pacing -- at that multiple, so everything software can
observe stays bit-identical to real speed, only compressed in time. `0`
selects turbo: a started disk DMA transfer completes almost instantly
(deferred by two scanlines, matching other emulators' turbo modes, so
loaders that clear stale interrupt flags right after starting a transfer
still see the completion). Drive mechanics are never accelerated: motor
spin-up, head stepping, and post-seek settle always run at real time.
Faster-than-real speeds are a compatibility trade-off, exactly as in other
emulators: the operating system and most loaders tolerate them, but
software that times its own loading against the beam, CIA timers, or music
playback can break. The setting can be changed live from the runtime menu
("Floppy Speed") without restarting the machine. It applies to image-backed
bays only; a physical drive has its own `replay_speed`.

Supported image formats: standard 901120-byte DD ADF, gzip-compressed
images (ADZ), single file ZIP archives, DMS archives, UAE extended ADF, and
read-only IPF and SCP images. DMS, gzip, IPF, and SCP images are decoded at
load time and always treated as write-protected; set `write_protected = false`
on a plain ADF to allow write-through updates to the image file. A DMS archive
is unpacked cylinder by cylinder into the DD image the disk was read from, so
archives that repeat a cylinder (an advertising boot block ahead of the real
one) or omit the cylinders that read back blank load normally; high-density
DMS archives are rejected, matching the DD-only floppy support.

IPF (the SPS/CAPS preservation format) is decoded by Copperline itself rather
than through the closed-source `capsimg` library, so every build reads IPF on
every platform with nothing to install. Because an IPF preserves the encoded
track -- sync marks, gaps, and sector headers, not just sector contents -- it
carries the custom trackloaders and copy protections that an ADF cannot
express. Each track is decoded to the revolution of MFM the head would pass
over and read back through the same path as a flux capture. Two limits are
worth knowing: tracks recorded with a variable cell *rate* (the Copylock,
Speedlock, and Brierley density models) are decoded with uniform 2 us cells,
which is logged at load time and leaves a protection that measures cell timing
seeing the wrong answer; and weak ("flakey") bits are replayed as the single
deterministic revolution the file stores rather than varying per revolution.
The browser frontend shares this decoder, so it reads IPF too.

A `paths` playlist lets multi-disk software that only drives DF0: run
without a second drive: the first entry is the boot disk and the disk-swap
shortcut (`Cmd+D` on macOS, `Alt+D` on Linux/Windows) or the status-bar
swap button cycles to the next image, wrapping around.

### A real drive on a bay

A bay can be given a physical 3.5" drive instead of an image, over a
Greaseweazle:

```toml
[floppy.df0]
bridge = "greaseweazle"      # or "off"
write_protected = true       # emulator-level protection, on top of the tab
# bridge_port = "/dev/ttyACM0"   # omit to auto-detect the interface
# bridge_cable = "a"             # a/b (IBM PC) or 0..3 (Shugart)
# bridge_density = "auto"        # auto/dd/hd
# bridge_mode = "normal"         # normal/compatible/stalling
# replay_speed = "fast"          # or "normal"; fast is the default
```

A bay takes either a bridge or an image, never both: the disk in the drive
is its media, and naming a `path` alongside is an error. `bridge = "off"`
returns the bay to images and keeps the other bridge settings for later.

Nothing needs installing -- the pure-Rust FluxBridge library is built into
Copperline -- but a physical drive changes how the machine runs in several
ways: writes need both the disk's tab and `write_protected = false`, the
status bar's eject and swap do nothing for that bay, and a machine with a
physical drive is paced to wall-clock time and is not reproducible.
[](fluxbridge) covers the whole feature: what each option does, which mode
suits which disks, and what to expect of it.

## `[ide]` -- IDE hard disks

```toml
[machine]
profile = "A600"             # IDE needs a machine with an IDE port
                             # (A600 or A1200 Gayle, or the A4000)

[ide]
master = "AmigaSYS.hdf"      # raw flat HDF, read/write
# slave = "scratch.hdz"      # gzip-compressed hardfile, writes not kept
```

Images are opened read/write. Both kinds of HDF work directly:

- a full disk image with its own Rigid Disk Block (RDSK/PART chain), and
- a bare partition hardfile (boot block starts with `DOS\x..`), which is
  wrapped in a synthesized RDB on the fly: one extra cylinder of
  16-surface x 32-sector geometry holding an RDSK and a bootable `DH0`
  PART block, with the image's own dostype. The image must be a multiple
  of 256 KiB so the partition is an exact cylinder count. Writes to the
  partition go back to the image file; writes to the synthesized RDB area
  (re-partitioning) live only for the session.

A **gzip-compressed hardfile** attaches too -- the `.hdz` convention, or a
plainly gzipped `.hdf`. Like the floppy formats it is recognised by content
and not by name, so the extension is free. Deflate cannot be read a sector
at a time, so the image is unpacked into memory when the drive opens: the
guest sees an ordinary writable disk of either kind above, but the writes
live only in that unpacked copy -- nothing goes back to the compressed
file, and changes are lost at exit. An image that unpacks to more than
1 GiB is refused rather than held in host RAM; decompress that one to a
plain hardfile and attach it instead.

A path may also name a **host directory**: its tree is built into an
in-memory volume at startup (volume name = directory name, files and
subdirectories included; entries whose names cannot exist on an Amiga
volume are skipped with a warning). The guest sees an ordinary bootable
disk and may write to it, but the volume lives only in memory -- nothing
is written back to the host directory, and changes are lost at exit. Note
that the stock A1200/A600 Kickstart `scsi.device` only probes the IDE
master; a slave drive needs a guest OS or driver that supports two units
(e.g. Kickstart 3.1.4).

To override the volume name (instead of inheriting the directory name),
the boot priority, or the filesystem, give the drive as a table with
`path` plus `name`, `bootpri`, and/or `filesystem`:

```toml
[ide]
master = { path = "/host/Games", name = "Games" }
slave = { path = "wb.hdf", bootpri = 6 }
# slave = "scratch.hdf"        # the bare-string form still works
```

The name sets the volume label of a directory mount; AmigaDOS volume
names hold up to 30 characters and cannot contain `:` or `/`. It has no
effect on a raw HDF, which carries its own label inside the image.

`filesystem` (`"ffs"`, the default, or `"ofs"`) picks the filesystem a
directory mount's in-memory volume is built with; it only applies to a
directory (an HDF/gzip image already carries its own filesystem inside
it, and Copperline refuses the key on one at config-parse time). FFS is
the default because it is the more modern, more capacious choice, but
Kickstart 1.3's ROM has no FFS handler built in -- real 1.3 hardware needs
`L:FastFileSystem` loaded from disk, or an RDB `FileSystemHeader` chain,
neither of which Copperline can bundle (it is Commodore-copyrighted). OFS
(`DOS\x00`) is built into every Kickstart ROM from 1.2 onward, so a
directory mount meant to work under 1.3 with no guest-side setup should
set `filesystem = "ofs"`:

```toml
[ide]
master = { path = "/host/Games", filesystem = "ofs" }
```

`bootpri` (-128..127, default 0) is the `de_BootPri` written into the
**synthesized** RDB's partition, which is what the ROM's strap ranks boot
candidates by. Kickstart enters DF0: at priority 5, so the default 0 loses
the tie to a bootable floppy; raise it to 6 to boot the hard disk ahead of
one, or lower it to sort two hardfiles against each other. The sentinel
-128 also clears the partition's `PBFB_BOOTABLE` flag, so the volume
mounts but is never offered for boot. It has **no effect on an image that
carries its own RDB** -- those priorities live inside the image, where
HDToolBox put them -- and Copperline logs a warning if you set it on one.

The configuration screen edits `bootpri` on the *Storage* tab's **Boot
Priority** sub-page, one row per drive (see [](ui.md)): a Priority number and a
Bootable box, the cleared box being this -128 sentinel. A drive left at 0 with
no cascade default writes no `bootpri` key.

The drive responds to ATA IDENTIFY with the Gayle byte order real hardware
uses, so both Kickstart 3.1 variants boot from it. An HDD activity LED
appears in the status bar on IDE machines. On the `A4000` profile the same
`[ide]` section attaches drives to the motherboard IDE interface at
`$DD2020` (no Gayle involved; Kickstart's `scsi.device` drives it the same
way).

A path ending in `.cue`, `.iso`, or `.chd` attaches an **ATAPI CD-ROM
drive** at that slot instead of a hard disk, through the PACKET (0xA0)
command -- the same read-only SCSI-2 command engine `[scsi]` CD-ROM units
use (see below), reached over the ATA task file instead of a WD33C93 SCSI
bus. It mounts and swaps discs, plays CD audio, and answers `scsi.device`
filesystems the same way a `[scsi]` CD-ROM unit does.

## `[scsi]` -- SCSI controllers

```toml
[scsi]
# controller = "a2091"       # a2091 (default), a4091, or a3000
rom = "a2091-v6.6.rom"       # boot ROM (a2091 needs one; a4091 defaults to bundled; a3000 none)
# rom_odd = "a2091-odd.rom"  # a2091 only: split even/odd EPROM dumps
unit0 = "workbench.hdf"      # SCSI IDs 0-6
unit1 = "data.hdf"
unit2 = "game.cue"           # a .cue, .iso, or .chd attaches a CD-ROM drive
# unit3..unit6 = ...
```

The `[scsi]` section attaches a SCSI host adapter with up to **seven
drives**. `controller` picks which one:

- `"a2091"` (the default on machines without onboard SCSI): a Commodore
  A2091 (Commodore DMAC + WD33C93A) as a Zorro II autoconfig board. It
  works on **any machine model** (the board needs no Gayle) and has no
  dependence on the Kickstart IDE driver -- the board's own boot ROM
  carries `scsi.device` and autoboots on Kickstart 1.3 and newer, which
  also sidesteps the stock A600/A1200 `scsi.device` only probing the IDE
  master. `[ide]` remains available, and both can be used at once.
- `"a4091"`: a Commodore A4091 (NCR 53C710 SCSI-2) as a Zorro III
  autoconfig board, for machines with a 32-bit CPU. `rom` is a raw A4091
  EPROM image (single ROM, so `rom_odd` does not apply); omit it to use the
  bundled open-source ROM from the [A4091 software
  project](https://github.com/A4091/a4091-software). The ROM only autoboots
  under AmigaOS -- Linux and NetBSD drive the board without it.
- `"a3000"` (the default on the `A3000` profile): the A3000's motherboard
  SCSI -- the Super DMAC at `$DD0000` driving a WD33C93. It is silicon, not
  a card, so it needs no boot ROM: Kickstart's own `scsi.device` drives it
  and autoboots from an RDB drive. It is only valid on a machine with the
  Super DMAC (the A3000).

For the A2091, `rom` must point at an A590/A2091 boot ROM image (version
6.6 or later; 16K/32K, available from the same vendors and dump sets as
Kickstart ROMs). Dumps split into even/odd EPROM halves can be given as
`rom` (even, U13) plus `rom_odd` (odd, U12). The ROM is required on the
Zorro boards because the autoboot DiagArea and the scsi.device driver
itself live in it; the autoconfig identity comes from the board (the
A2091 is Commodore product 3, with its DiagArea vector at `$2000`).

Each `unitN` accepts everything `[ide]` paths do: RDB images, bare
partition hardfiles (a synthesized RDB advertises a bootable `DHn`
partition, named after the SCSI ID), gzip-compressed hardfiles (`.hdz`),
and host directories built into in-memory FFS/OFS volumes -- including the
`{ path = "...", name = "...", bootpri = N, filesystem = "..." }` table
form that overrides a directory mount's volume name, filesystem, and the
synthesized partition's boot priority. The HDD activity LED covers SCSI
traffic too.

A `unitN` path ending in `.cue`, `.iso`, or `.chd` attaches a **SCSI
CD-ROM drive** at that ID instead of a hard disk: a read-only removable
SCSI-2 target (INQUIRY device type 5) serving 2048-byte blocks, with the
full READ TOC / READ CD / mode-page surface CD filesystems expect.
Cue/bin and CHD images may mix data and audio tracks; a bare `.iso` is a
single data track. The drive answers on the host adapter's `scsi.device` like any
other unit, so mount it the way you would on real hardware: a
`DOSDrivers` mount entry (or MountList) pointing `CDFileSystem` --
CacheCDFS, AsimCDFS, and AmiCDROM work the same way -- at the controller's
`scsi.device` and the drive's unit number.

CD audio plays: the PLAY AUDIO command group streams the disc's audio
tracks into the machine's audio output at 75 sectors per second of
emulated time (as if the drive's analogue output were cabled to the
machine), the sub-channel reports the live playback position, and the
debugger's Audio tab shows the stream on its CD-DA row with the play
state, track, and position. Discs swap at runtime like CDTV/CD32 media:
the status bar's CD load/eject buttons, dropping a `.cue`/`.iso`/`.chd`
on the window, the scheduled `--insert-cd-after SECS PATH` flag, or the
control protocol's `media.cd.insert` all eject the current disc, run the
tray for a second of emulated time, and mount the new one with a
medium-change unit attention for the guest's filesystem to notice.

## `[lide]` -- a lide.device-compatible Zorro II IDE board

```toml
[lide]
# board = "ripple"        # ripple (default), ride, or atbus2008
rom = "lide.rom"           # user-supplied; omit for hardware-only mode
# rom_bank2 = "cdfs.rom"   # optional second flash bank (ripple/ride only)
drives = ["workbench.hdf", "data.hdf"]
```

A built-in Zorro II IDE board compatible with LIV2's actively-maintained
open-source [lide.device](https://github.com/LIV2/lide.device), giving
autobooting IDE storage under any Kickstart including 1.3 -- unlike `[ide]`,
which needs a Gayle or A4000 IDE port, `[lide]` works on **any machine
model**, the same way `[scsi]`'s Zorro boards do. A `.cue`/`.iso`/`.chd`
drive entry attaches an ATAPI CD-ROM drive, exactly as it does on `[ide]`.

`board` picks the AutoConfig identity: `"ripple"` (the default), LIV2's
open-hardware Zorro II card with two ATA channels (four drives); `"ride"`,
LIV2's expansion-port board, which shares RIPPLE's ROM image and register
layout but has one channel (two drives); or `"atbus2008"`, the AT-Bus 2008
and its clone family (Dicke Olga, the TK accelerator boards' IDE,
CDTV-RAM-IDE, Zorro-LAN-IDE), one register model covering the whole family,
one channel, no ROM banking. None of the three wire an interrupt line --
`lide.device` is a purely polling driver.

`drives` takes the same bare-path/table form as `[ide]`/`[scsi]` (RDB images,
bare partition hardfiles, `.hdz`, host directories, and the `{ path = "...",
name = "...", bootpri = N, filesystem = "..." }` table), in (channel,
master/slave) order:
entries 0 and 1 are channel 0's master and slave, entries 2 and 3 are channel
1's (`"ripple"` only -- `"ride"` and `"atbus2008"` have one channel).

`rom` is **always user-supplied**, never bundled: fetch a release from
[lide.device's GitHub releases page](https://github.com/LIV2/lide.device/releases)
-- `lide.rom` (32768 bytes) covers `"ripple"` and `"ride"`, `lide-atbus.rom`
covers `"atbus2008"`. Omitting `rom` is a legal **hardware-only mode**: no
DiagArea, no autoboot, but drives still work once a disk-loaded
`lide.device` finds them -- the same setup lide's own CI uses to test
in-development driver builds without flashing anything. `rom_bank2`
optionally supplies a second flash bank (e.g. `cdfs.rom`, LIV2's CD
filesystem, from the same releases page); it requires `rom` and does not
apply to `"atbus2008"`, which has no ROM banking.

## `[[host_disk]]` -- a real disk of the host's

Give the machine a real disk of this computer's instead of an image -- a card
in a reader, a USB drive, a drive on a SATA port. The medium is used exactly
as it is, with its own RDB, partitions, and filesystem.

```toml
[[host_disk]]
device = "sdb"                 # last name shown by `--list-disks`
fingerprint = "v1-..."         # opaque identity written by the launcher
attach = "ide-master"          # ide-master (default), ide-slave, lide0-master,
                                # lide0-slave, lide1-master, lide1-slave, or scsi0..scsi6
read_only = true               # the default; false explicitly allows writes
```

`device` is the host's current enumeration name -- `sdb` on Linux, `disk4` on
macOS, `PhysicalDrive1` on Windows -- and `copperline --list-disks` prints it.
Those names can change between boots. A launcher-saved `fingerprint` is
authoritative and lets Copperline follow the same hardware to a changed name
only when exactly one attached disk matches; a missing or ambiguous match is
refused. Keep this opaque value as written. Older and hand-written entries
without it retain exact `device` lookup. To record one, select and mount the
disk afresh in the launcher, then save or launch; a plain open-and-save keeps
the field absent. An unambiguous fingerprint is sufficient for read-only use.
Persisted writable use additionally requires a fixed, non-removable disk with
a credible serial/WWN: removable media is considered weak even when a USB
bridge reports its own serial, and must be freshly selected for each writable
session, as must a fixed disk without a credible serial/WWN.

Absent `read_only` means read-only, including for older configurations that
previously relied on the writable default. Set it to `false` explicitly to
allow guest writes. The command-line equivalents make the access choice for
the current run: `--host-disk DEVICE [ATTACH]` is read-write, while
`--host-disk-read-only` is protected. An unresolved disk leaves that drive
slot empty and the machine starts anyway.

The disk this computer is running from is never offered and never opened,
whatever is written here, and no RDB is synthesised over a disk that has
none. Opening a real disk asks for permission the first time, and the host's
volumes on it are unmounted while the machine has it. Attach read-only the
first time: [](host-disks) covers the whole of it.

(filesys-mounts)=

## `[[filesys]]` -- host directories as live volumes

```toml
[[filesys]]
path = "/data/amiga/Workbench"
volume = "Workbench"   # optional, defaults to the directory name
bootpri = 6            # optional boot priority; default -128 = never boot

[[filesys]]
path = "/data/amiga/downloads"
readonly = true        # optional, export the directory write-protected
```

Each `[[filesys]]` entry exports a host directory to the guest as an
AmigaDOS volume on its own `HOSTFS<n>:` device, served live by the
emulator: no disk image is built, and guest reads always see the current
host contents. This differs from giving `[ide]`/`[scsi]` a directory
path, which snapshots the tree into an in-memory FFS or OFS volume at
startup (see `filesystem` above). Up to 8 mounts.

The volumes are read-write by default: the guest creates, writes, renames,
and deletes the host's files directly, and changes land in the directory
as you would expect. Set `readonly = true` to export a directory
write-protected instead -- the guest sees a read-only disk and every write
fails with the same "disk is write-protected" error a physical
write-protected disk gives, which is worth setting on anything you would
rather the Amiga could not damage. The launcher's Host Folder sub-page (under
the Storage tab) exposes the same choice as its **Access** field.

Amiga file attributes a host filesystem cannot hold -- protection bits
such as script/pure/archive, file comments, and exact datestamps -- are
kept in UAE-style `.uaem` sidecar files, read when present and written
back when the guest changes them; the sidecars stay hidden from guest
listings, and the delete-protection bit is honoured. Host filenames are
mapped between UTF-8 and the guest's Latin-1 (names with no Latin-1
spelling are hidden, since the guest could neither display nor reopen
them). Host symlinks inside the mount are followed, wherever they point:
the guest has no way to create one, so a symlink is treated as the host
user deliberately grafting a directory into the mount, the same trust
model as the UAE family.

`volume` sets the AmigaDOS volume name (up to 30 characters, no `:` or
`/`). `bootpri` enters the volume in the boot-device vote (-128..127;
the default -128 means mounted but never booted from): hard-disk boot
partitions typically sit at priority 0 and DF0: at 5, so a bootable
Workbench directory with `bootpri = 6` boots ahead of both.

Kickstart 1.3 and newer get the full feature set, booting included: a
`bootpri` above the competition boots the machine from the host
directory as `SYS:` under 1.3 exactly as under 3.1 (the service speaks
both the V36 boot-node protocol and V34's own autoboot and handler
startup conventions). Kickstart 1.2 and older lack the expansion-ROM
hook entirely and never see the mounts.

## `[whdload]` -- direct WHDLoad boot

```toml
[whdload]
game = "Turrican.lha"       # .lha, .zip, or a folder with a .slave
# library = "..."           # unpacked games and saves; default:
#                           # <config dir>/whdload/save
# kickstarts = "..."        # directory scanned for Kickstart images
# args = "ButtonWait"       # extra WHDLoad command-line options
# machine_type = "auto"     # or "copperline" to boot on this machine
# whd_package = "..."       # your own WHDLoad_usr.lha
# skick_package = "..."     # your own skick*.lha

# Launcher only.
# enabled = true            # false removes the WHDLoad page entirely
# games = "..."             # the folder the Library page lists
# library_db = "..."        # default: <config>/whdload/support/launcher.db
# library_cache = "..."     # default: <config>/whdload/support/cache
```

Boots straight into a WHDLoad-installed game: the package is unpacked
into the game library (once -- saves the game writes persist there), a
minimal boot volume is synthesized around the real WHDLoad program, raw
Kickstart images from `kickstarts` are identified by content and staged
into `Devs:Kickstarts/`, and the machine is derived from the slave
header (an A1200 with 8 MiB fast RAM) unless `[machine]`, `rom`, or
`[memory]` say otherwise. `--whdload GAME` is the command-line
equivalent and overrides `game`. The whole story, including what the
support archives are and how Kickstart identification works, is in
[](whdload.md).

The developer-oriented cousin is `--run PROG`: it stages the same kind
of boot volume around an ordinary Amiga executable and warp-boots into
it. It is CLI-only by design (no config section) and is covered in
[](run.md).

## `[cd]` -- CDTV and CD32

```toml
[machine]
profile = "CD32"

[cd]
image = "disc.cue"        # BIN/CUE cue sheet (MODE1/2048, MODE1/2352, AUDIO)
insert_delay = 0.0        # emulated seconds after power-on to insert
# nvram = "cd32-nvram.bin" # CD32 save-game EEPROM backing file (default)
```

`image` takes a BIN/CUE cue sheet, a bare `.iso` (single data track), or
a `.chd` -- MAME's compressed CHD CD format (v5, as chdman's `createcd`
writes: LZMA/Deflate/FLAC-compressed hunks with data and audio tracks).
The disc mounts on the machine's CD controller: Akiko on CD32, the DMAC on
CDTV. `insert_delay` inserts the disc some emulated seconds after power-on
with the proper media-change notification; some CDTV discs only boot when
inserted after the boot screen appears. CD32 NVRAM
persists to `cd32-nvram.bin` in the `[paths]` nvram folder unless
overridden with `[cd] nvram`; a `cd32-nvram.bin` already in the working
directory from an earlier Copperline keeps being used from there.

## `[[zorro]]` -- expansion boards

```toml
[[zorro]]
metadata = "boards/megaram.toml"

[[zorro]]
metadata = "boards/myboard.toml"
# config = { mode = "fast" }  # WASM plugin boards: setting overrides
```

Each entry adds a Zorro board described by a TOML metadata file, configured
in file order after the built-in `[memory]` fast/z3 boards. For a WASM
plugin board, the optional `config` table overrides individual settings
that the plugin's manifest declares (layered over the manifest's `[config]`
defaults; the launcher's Zorro tab edits the same values). See
[](../zorro) for the metadata format, the plugin ABI, and how autoconfig
assigns addresses.

## `[a2065]` -- Ethernet

```toml
[a2065]
net = "nat"   # or "bridge", "loopback"; "none" for an isolated NIC
# interface = "en0"  # required for "bridge"
```

Fits a Commodore A2065 Ethernet board (Am7990 LANCE) on the Zorro chain;
`--a2065-net BACKEND` is the matching per-run flag, and the launcher's
**I/O Ports** tab (Networking page) has the same picker. `net` selects the
host network backend:

- `"nat"` -- userspace NAT: the guest gets outbound IPv4 internet through a
  virtual gateway with no host privileges or setup, identically on Linux,
  macOS, and Windows. Configure the guest's TCP/IP stack with IP
  `10.0.2.15`, netmask `255.255.255.0`, gateway `10.0.2.2`, DNS `10.0.2.3`
  (or let it BOOTP/DHCP). Outbound only, IPv4 only.
- `"bridge"` -- attaches complete Ethernet frames directly to `interface`, so
  the Amiga is a separate station on the physical LAN and can accept inbound
  connections from LAN peers. Use `copperline --list-net-interfaces` for exact
  identifiers, or `--a2065-interface NAME` (which implies the bridge backend).
  Configure the guest by DHCP from the real LAN or with an address appropriate
  to that LAN. The guest can reach peers and the router; communication with the
  host's own IP is adapter/OS-dependent and is not guaranteed. Frames keep the
  Amiga's source MAC, so Wi-Fi is best-effort: many access points reject a
  second source MAC behind one wireless station. Copperline reports adapter,
  driver, and permission failures at startup instead of falling back to NAT.
- `"loopback"` -- echoes transmitted frames back (self-contained, useful
  for driver bring-up).
- `"none"` -- the NIC is fitted but isolated.

Omit the section entirely for no board. Note that host networking is
inherently non-deterministic: inbound frames arrive on the host's
schedule, not the emulated clock, so a NIC board breaks byte-identical
replay and save-state determinism while traffic flows. A save state stores the
bridge adapter identifier and must be restored on a host where that adapter can
be opened. See [](../zorro) for board details, platform bridge setup, and the
NAT's limitations.

## `[toccata]` -- AD1848 sound board

```toml
[toccata]
enabled = true
```

Fits a MacroSystem Toccata sound board (an AD1848 codec) on the Zorro
chain. Its stock, open-source AHI driver (`toccata.audio`) works unmodified,
so any AHI-aware guest application gets 16-bit sound with no
Copperline-specific setup. No other options exist yet. Omit the section
(or `enabled = false`) for no board. The launcher's **I/O Ports** tab
(Audio page) has the matching fit/don't-fit toggle; host-side audio
capture and backend settings (`--audio-wav`, `--audio-stems`, device
selection) stay command-line/config-file only and have no launcher row.
The board's output joins the mixer as the `toccata` source for
`--audio-stems` (see [](headless)); see [](../zorro) and
[](../internals/toccata) for the register model.

## `[mhi]` -- virtual MPEG audio decoder board

```toml
[mhi]
enabled = true
```

Fits a virtual MPEG-1/2/2.5 Layer III audio decoder board on the Zorro
chain, serving the Amiga MHI API through the ported
`mhi_copperline.library`, so MHI-aware guest software (e.g. AmigaAMP) gets
hardware-accelerated MP3 decoding. No other options exist yet. Omit the
section (or `enabled = false`) for no board. Needs a build with the `mhi`
feature (on by default; off only for the wasm32 build). The launcher's
**I/O Ports** tab (Audio page) has the matching fit/don't-fit toggle;
host-side audio capture and backend settings (`--audio-wav`,
`--audio-stems`, device selection) stay command-line/config-file only and
have no launcher row.

The guest library is not auto-installed: copy the committed artifact,
`guest/mhi/mhi_copperline.library`, into the guest's `LIBS:mhi/` drawer.
AmigaAMP (and other MHI clients) find MHI drivers by scanning `LIBS:mhi/`
for `mhi#?.library`, so the file must sit in that drawer under a matching
name; check the player's own `MHISupport`/`MHI-Driver` settings if it does
not pick up the library automatically. The board's decoded output joins
the mixer as the `mhi` source for `--audio-stems` (see [](headless)); since
the board consumes each descriptor's bitstream at the decoded audio's own
emulated-time rate, playback through it stays deterministic and
reproducible byte-for-byte the same way `--audio-wav` captures of the rest
of Copperline's audio path are. See [](../zorro) and [](../internals/mhi)
for the register model.

## `[hostsocket]` -- bsdsocket.library without a guest TCP/IP stack

```toml
[hostsocket]
net = "nat"   # or "bridge", "loopback", "host"; "none" for a dead wire
# interface = "en0"       # required for "bridge"
# dns_server = "10.0.2.3" # only used when resolver = "dns" (see below)
# hostname = "amiga"      # gethostname() return value (cosmetic)
# address = "192.168.1.50/24" # interface address; only for "bridge" (see below)
# gateway = "192.168.1.1"     # default gateway; only for "bridge" (see below)
# resolver = "dns"            # gethostbyname() via dns_server directly instead
#                             # of the host's own resolver (the default under
#                             # "nat"/"bridge"/"host"); see below
```

Fits the bundled HostSocket board: `bsdsocket.library` for the guest, backed
by a TCP/IP stack that runs on the host instead of inside the emulated CPU.
Any bsdsocket-consuming application (an Aminet tool, an MQTT client, a game
with an online mode) opens `bsdsocket.library` and calls
`socket()`/`connect()`/`send()`/`recv()` exactly as it would against AmiTCP or
Roadshow -- but there is no guest-side stack to install, configure, or boot:
the library autoboots from the board's ROM on Kickstart 1.3 through 3.x and
on the bundled AROS ROM. `--hostsocket-net BACKEND` is the matching per-run
flag, and the launcher's **I/O Ports** tab (Networking page) has the same
picker.

`net` selects the same host network backends as the A2065 (see above for
`"nat"`/`"bridge"` details and caveats), with one important difference in
what they mean here. The guest never configures an IP address -- the
host-side stack owns addressing -- so under `"nat"` sockets simply reach the
outside world, and under `"loopback"` the guest can talk to itself
(`127.0.0.1`) with fully deterministic behavior, which also makes
`"loopback"` the right backend for reproducible headless test runs of
socket-using software. `gethostbyname()` just works under
`"nat"`/`"bridge"`/`"host"` with no further configuration (see `resolver`
below for why); under `"loopback"` lookups return failure (nothing is
listening, and nothing should be -- see `resolver`'s own note on why
loopback never routes through a real resolver).

`net = "host"` is a fourth, different kind of backend, not a `crate::net`
one at all: instead of terminating TCP/UDP on this board's own embedded
TCP/IP stack and pushing frames through a NAT/bridge/loopback backend, each
socket operation (`socket`/`connect`/`send`/`recv`/`bind`/`listen`/
`accept`/`sendto`/`recvfrom`/`getsockname`/`getpeername`) delegates
directly to a real host OS socket, the same approach Amiberry/WinUAE's own
bsdsocket emulation uses. The guest ends up sharing the host's network
identity rather than getting an address of its own: outbound connections
work with zero configuration (no interface address, no gateway, no
pcap/TAP privileges, nothing to pick), and a `bind()`/`listen()`/`accept()`
server is a plain host port bind. This is not true bridging -- the Amiga
gets no LAN IP -- but it covers the common reason people reach for bridge
mode, with none of bridge's setup. `interface`/`address`/`gateway` (below)
are all rejected outright under `"host"`, since none of them mean anything
when there is no interface of the board's own to configure. Raw ICMP
(ping) is unaffected by any of this -- `"host"` only touches TCP and UDP --
and still goes through the underlying (hardcoded to `"loopback"` under
`"host"`) smoltcp interface exactly as it would under a literal
`net = "loopback"`.

`address` and `gateway` matter only under `"bridge"`. The default interface
address/gateway (`10.0.2.15/24` and `10.0.2.2`) are Copperline NAT's own
virtual addresses, hardcoded on the NAT side too -- correct and required
under `"nat"`/`"loopback"`, but meaningless on a real physical LAN, where
nothing answers ARP for a `10.0.2.2` gateway that doesn't exist there. Set
both to match the LAN `interface` is bridged to (e.g. `address =
"192.168.1.50/24"`, `gateway = "192.168.1.1"`) -- there is no DHCP client,
so the address is always static, picked the same way you would pick one for
any other statically-configured device on that network. Leave both unset
for `"nat"` and `"loopback"`; they are rejected outright (not merely
ignored) under `"host"`.

`resolver` picks how `gethostbyname()` itself works, independent of the
addressing above. Left unset, it defaults to `"host"` under `"nat"`/
`"bridge"`/`"host"`: Copperline's own process resolves the name via the
host OS's resolver on a background thread -- the same mechanism
`[a2065]`'s NAT backend already uses internally for its own DNS
forwarding, now available directly to this board -- which is why
`gethostbyname()` just works out of the box under all three backends with
no `dns_server` to hand-configure (`net = "host"`'s own underlying smoltcp
interface is hardcoded to `"loopback"`, which couldn't reach a real
`dns_server` even if one were set). Set `resolver = "dns"` explicitly to
opt back into the board's own DNS query: its own smoltcp stack sends a
real DNS request to `dns_server` over whichever backend is fitted (or
`"loopback"` under `net = "host"`, where it will simply never get an
answer -- there is nothing to opt back into there in practice), which is
the one way to target a *specific* resolver rather than whatever the host
happens to be configured to use (a corporate/internal-only DNS server on a
bridged LAN, for instance). Explicit `resolver = "host"` is rejected under
`"loopback"`/`"none"` (there is no sane default there either, so those
backends simply get no resolver at all): routing through a real host
resolver would silently defeat loopback's whole reason for existing,
byte-identical deterministic replay. Reverse lookups (`gethostbyaddr()`)
are unaffected by this setting -- they always use the `"dns"` path's PTR
query against `dns_server`.

Do not fit this board and also boot a real guest TCP/IP stack (AmiTCP,
Roadshow, ...) in the same session -- both would add a `bsdsocket.library`,
and which one an application opens is undefined. Use `[a2065]` for testing
real stacks and SANA-II drivers; use `[hostsocket]` for running or testing
the applications above them. The determinism note on `[a2065]` applies here
too: `"nat"`, `"bridge"`, and `"host"` traffic all arrive on the host's
schedule and break byte-identical replay, while `"loopback"` and `"none"`
stay deterministic. The board state (open sockets included) rides in save
states, but live TCP/UDP peers do not survive a restore on the host side --
under `"host"` in particular, every socket a save state remembers comes
back closed (real host OS socket handles are never themselves part of the
snapshot), so the guest sees each one as if the connection had simply
dropped, the same as a resumed `"nat"`/`"bridge"` session already does for
its own live traffic.

Implemented as a bundled WASM plugin board (see [](../zorro)); its
verification record against the external bsdsocktest conformance suite
lives in `crates/hostsocket-plugin/docs/bsdsocktest-status.md`.

## `[zz9k]` -- ZZ9000 SDK crypto accelerator board

```toml
[zz9k]
enabled = true
# zorro = 3        # 2 or 3; default: 3 on a 32-bit CPU, else 2
# size = "4M"      # Zorro III window, power of two 1M..256M; Z2 is fixed at 4M
# int2 = false     # completion interrupt on INT2 instead of the INT6 default
# seed = "..."     # reserved deterministic DRBG seed (hex); no current consumer
```

Fits a register-compatible subset of the MNT ZZ9000's "SDK v2" service
platform -- the CORE, MEMORY, and CRYPTO services -- with the crypto
computed host-side: SHA-1/256/384/512, BLAKE2s, HMAC, Poly1305, ChaCha20,
ChaCha20-Poly1305 and AES-GCM AEAD, X25519 and P-256 key exchange, P-256
keygen, and ECDSA/RSA signature verification, all at host speed. The
zz9000-sdk's unmodified Amiga-side software -- `zz9k.library`, the
`zz9k-info`/`zz9k-hash`/`zz9k-aead`/... tools, and its accelerated AmiSSL
build -- detects the board exactly as it detects real hardware
(manufacturer 0x6D6E, product 4 or 3), so TLS on an emulated 68k stops
being bottlenecked by guest-side crypto. See [](../internals/zz9k) for the
protocol contract and the pinned SDK revision.

The board is pure compute: fitting it keeps the machine fully
deterministic and replay-safe, and it works headless, in save states
(including mid-operation), and on any machine profile. On Zorro II the
window is pinned at 4M -- the only Zorro II size the SDK transport accepts
shared-buffer allocations for -- and the SDK's Zorro II path polls instead
of using the doorbell, which the board handles transparently. `zorro = 3`
needs a 32-bit CPU (68020+), like any Zorro III board.

The guest picks the completion-interrupt line itself via the SDK's
`ENV:ZZ9K_INT2` / `ZZ9000.CFG` convention; `int2` here sets what the
board's config-key register reports (leave it false for the INT6 default
unless guest-side software asks otherwise).

Only the SDK services above exist: the real ZZ9000's RTG, USB, and
Ethernet faces are absent (registers read zero, services report
unsupported), so do not install the P96 `zz9000.card` display driver
against this board -- it is harmless but shows nothing.

## `[rtg]` -- RTG graphics card

```toml
[rtg]
card = "picasso2"
vram = "2M"
```

`card` is `"picasso2"`, `"picasso2plus"`, `"graffityz2"`, `"graffityz3"`,
`"z3660"`, or `"none"`; a machine takes at most one. All of these boards give
the guest high-resolution, high-colour screens through Picasso96.

`"picasso2"` fits a Village Tronic Picasso II with a CL-GD5426 graphics
controller. `"picasso2plus"` fits the later CL-GD5428 revision, reports its
distinct autoconfig serial number, and wires vertical blank to INT2. Both are
Zorro II boards, so they work with 68000/68010 and 24-bit 68EC020 machines as
well as 32-bit CPUs. `vram` selects either real board's `"1M"` or `"2M"`
memory configuration and defaults to `"2M"`; it is ignored for other cards.
Install the Picasso96 `PicassoII.card` driver and its monitor file in the
guest. The board starts on native Amiga pass-through and switches the
Copperline display to RTG only while the guest enables a valid Picasso screen.

`"graffityz2"` and `"graffityz3"` fit Atéo Concepts' Graffity, which reuses
the same CL-GD5428 core as Picasso II+ under its own autoconfig identity;
`vram` selects `"1M"` or `"2M"` for either, same as the Picasso II family.
`"graffityz2"` is a Zorro II board, so it works on any CPU. `"graffityz3"` is
a Zorro III board and needs a 32-bit address bus (68020/030/040/060), same
restriction as `"z3660"`. Install the Picasso96 `Graffity.card` driver and
its monitor file in the guest -- it ships in the classic Aminet
`Picasso96Install` package, so no separate download is needed.

`"z3660"` is a Zorro III board. It comes fitted by default on machines whose
CPU has a 32-bit address bus (the A3000 and A4000) and is unavailable on the
rest; asking for it there is an error, as it is for Zorro III RAM and
`"graffityz3"`. It needs the open-source Z3660.card driver installed in the
guest (with its monitor in `DEVS:Monitors`). With that in place, Z3660 screen
modes appear in ScreenMode, and the window shows the board's output when a
screen is opened.

The Z3660 board's stock monitor ships with the `DISPLAYCHAIN=NO` tooltype, which
models the real hardware's separate RTG monitor and never hands the display
back to the native screen. On a single-window emulator you usually want
`DISPLAYCHAIN=YES`, so the one window follows whichever screen is active.

## `[debug]` -- diagnostics

```toml
[debug]
log_unmapped = "DD0000-DEFFFF"
validate_chipset = true
detect_smc = true
```

`log_unmapped` logs every CPU read and write inside the given range that no
device decodes. Reads report the floating bus value they returned, writes
report the value that went nowhere. The value is a hex `START-END` range whose
end is included (a leading `0x` is allowed), or `all` for the whole address
space.

This is how you find the registers a guest expects and Copperline does not
implement yet. A missing register is usually invisible: a read floats, a write
is dropped, and the guest either sulks or hangs with no diagnostic. Pointing
this at the window a driver probes shows the access pattern directly -- an IDE
presence probe, say, appears as a write of `$A0` to the device/head register
followed by a long run of status reads that never come back ready.

A booting Kickstart probes enough empty address space that `all` produces on
the order of a million lines per boot, so prefer a range once you know roughly
where to look.

`validate_chipset` arms the custom-register access validator: a running
report of software using the chipset in ways the hardware quietly ignores.
It flags writes to registers the fitted Agnus/Denise does not have, bits a
register does not define, writes to read-only registers and reads of
write-only ones, byte or odd-address access to word registers, access
through an address mirror, and DMA pointers aimed past the chip RAM Agnus
can address. It also covers the engines behind those registers, where
misuse hangs rather than glitches: a blit started while the previous one
is still running (there is no register-file interlock, so the running
blit is drained and the replacement starts from whatever pointer state it
left) or with its DMA switched off (BBUSY is set and the blit stays
pending until BLTEN and DMAEN are enabled), disk DMA armed against a
drive that could not serve it at that moment -- no media, or the motor
still off, the class behind the classic loader dead-spins -- and a
keyboard handshake pulse too narrow to count as one while the MCU was
waiting for it, which costs a key and stalls input until the keyboard
resynchronises after 143 ms. Each finding names the PC (or Copper address) that made the
access and the beam position, is deduplicated by (kind, register, writer)
with a repeat count, and is logged the first time it is seen. It also arms
a per-register last-writer table, which answers "what set BPLCON3, and
from where?" without a bisect. Read both over the control protocol with
`chipset.report` and `custom.writer` (see
[the control protocol](../debugger/control.md)), which can also arm and
disarm the validator live. Off by default; an unarmed machine pays nothing
for it.

`detect_smc` reports writes that land on memory the CPU has already
executed. Self-modification is legitimate on a 68000 -- decrunchers,
trackers and Copper-list patchers all do it -- but it is also where a
prefetch-related bug hides, since the CPU has already fetched the word
ahead of the one it is executing, and neither a patch applied too late
nor one applied to the wrong address leaves a trace at the moment it
happens. Each report names the written address, the instruction that
wrote it, and the distance between them, calling out a patch close enough
to sit inside the prefetch. An address counts as code once an instruction
there has retired, so an instruction patching its own extension words on
its only execution is not reported; every repeating pattern is caught on
the pass after the first. Read it with `smc.report` over the control
protocol, which can also arm and disarm the detector live. Off by
default; it costs a 1 MiB execution map while armed.
