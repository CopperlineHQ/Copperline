# Configuration reference

Copperline is configured by a TOML file: `./copperline.toml` by default, or
any file passed with `--config`. Every field is optional; missing fields use
the defaults documented here. `copperline.example.toml` in the repository
root is a commented companion to this reference.

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
| `--chip SIZE` | `[memory] chip` | `512K`, `1M`, `2M`, ... |
| `--fast SIZE` | `[memory] fast` | `0`, `1M`, `4M`, `8M`, ... |
| `--slow SIZE` | `[memory] slow` | `0`, up to `512K` |
| `--floppy-drives COUNT` | `[floppy] drives` | `1` to `4` wired drives (`DF0:` plus external drives) |
| `--joystick MODE` | `[input] joystick` | `gamepad` (default), `keyboard` |
| `--port1 DEVICE` | `[input] port1` | `mouse` (default), `joystick`, `cd32`, `analogue`, `none` |
| `--port2 DEVICE` | `[input] port2` | same devices; default `joystick` (`cd32` on the CD32 profile) |

For example, to boot a stock A1200 profile but with 8 MB of fast RAM and a
faster CPU, with no config file at all:

```sh
./target/release/copperline --model A1200 --fast 8M --cpu-clock 28 KICK31.ROM
```

A `--model` profile supplies the chipset, CPU, and memory defaults of a real
machine; the other flags then override individual values on top of it, just as
explicit `[cpu]`/`[chipset]`/`[memory]` sections override a `[machine]`
profile in a config file.

The audio, serial, and parallel surface has matching per-run flags too --
`--audio-device`, `--audio-channel-mode`, `--audio-stereo-separation`,
`--serial`, `--midi-in`, `--midi-out`, `--parallel`, `--sampler-audio-input`,
`--sampler-input-gain` -- described with their `[audio]`, `[serial]`, and
`[parallel]` keys below.

## Top level

```toml
rom = "KICK13.ROM"            # Kickstart image, exactly 512 KiB
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

## `[machine]` -- machine profiles

```toml
[machine]
profile = "A1200" # A1000, A500, A500OCS, A500Plus (A500+), A600, A1200, A3000, A4000, CDTV, CD32
rtc = true        # add a battery RTC (default: only A500+/CDTV/A3000/A4000 ship with one)
# rtc_time = "2005-03-18 01:58:29" # seed the clock; it then ticks in emulated time
# rtc_frozen = true                # stop the seeded clock at rtc_time exactly
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
| `A3000` | ECS | 68030 @ 25 MHz | 2M | 0 | Ramsey-04, RTC |
| `A4000` | AGA (Alice/Lisa) | 68040 @ 25 MHz | 2M | 0 | Ramsey-07, RTC |
| `CDTV` | ECS | 68000 @ 7.09 MHz | 1M | 0 | DMAC CD controller, RTC, 256K extended ROM |
| `CD32` | AGA (Alice/Lisa) | 68EC020 @ 14.18 MHz | 2M | 0 | Akiko, CD32 pad, NVRAM, 512K extended ROM |

`rtc` exists because most Amigas shipped without a battery-backed clock and
only some carried one. The `A500Plus` (an OKI RTC soldered to the Rev 8A
board), `CDTV`, `A3000`, and `A4000` fit one by default; the base
A500/A500OCS, A600, A1200, A1000, and CD32 have none. Set `rtc = true` to add
one -- for an A600HD or a clock-equipped A1200, say -- so the Workbench clock
keeps time.

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

Motherboard fast RAM is not emulated on either; use `[memory] z3` instead,
which the OS is equally happy with.

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
```

The deterministic cycle-driven core is the only emulation timing. It is
paced to wall-clock for the interactive window and runs unthrottled for
headless captures; the emulated result is identical. (An older `speed` key
here is accepted but ignored -- "real" was the only timing model, so it
carried no information.)

- `power_on = false` starts the machine powered off showing a test screen
  until you click the status-bar power button -- useful for arming video
  capture first. The power button cold-boots (clears RAM).
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

## `[memory]`

```toml
[memory]
chip = "512K"   # OCS max 512K; ECS/AGA max 2M
fast = "0"      # Zorro II fast RAM at $200000: 64K..8M board sizes
slow = "512K"   # A500 trapdoor RAM at $C00000: 0 or up to 512K
z3   = "0"      # Zorro III RAM (needs a 32-bit CPU): 64K..1G, power of two
```

Sizes accept `K`/`KB`/`M`/`MB` (and `G`/`GB` for Zorro III) suffixes or
plain byte counts, and must be multiples of 4 KiB.

- **Chip RAM** is range-checked against the chipset: 512K on OCS, 2M on
  ECS/AGA (also bounded by the selected Agnus revision's address reach).
- **Fast RAM** is exposed as a Zorro II autoconfig board at `$200000`, so
  it must be a legal Zorro II board size: 64K, 128K, 256K, 512K, 1M, 2M,
  4M, or 8M.
- **Slow RAM** ($C00000 "ranger" RAM) is arbitrated on the chip bus through
  Agnus exactly like chip RAM -- it is slow in the authentic way.
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
overscan = "tv"      # "tv" (default) or "full"
pixel_aspect = "tv"  # "tv" (default, 4:3 CRT) or "square" (exact 2x2 lo-res)
phosphor = 0.0       # CRT persistence fraction, 0.0 (off) to 0.95
```

The emulated framebuffer always carries the full overscan field Denise
produces. `"tv"` masks the deep horizontal overscan margins in black like a
CRT bezel, presenting the standard PAL window plus 24 lo-res pixels per side
of TV-style overscan while preserving vertical border colour changes. PNG
screenshots and `--dump-frames` crop standard PAL TV output to a 692x540
aperture for reference-emulator comparison. `"full"` shows everything, which
is useful when debugging display alignment. `COPPERLINE_OVERSCAN=full|tv`
overrides this for a single run.

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

`phosphor` blends each presented frame with a fraction of the previous
one, approximating the exponential decay of CRT phosphor. Software that
relies on the tube to fuse field-rate flicker -- alternate-field dither
transparency or flicker-dithered animation -- reads as intended with values
around `0.3`-`0.5`,
at the cost of a slight motion trail. Off by default so screenshots and
frame dumps stay frame-exact. `COPPERLINE_PHOSPHOR=0.4` overrides the
config for a single run.

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
```

The drive sounds are generated from scratch: motor hum with spin-up/down,
head-step clicks for seeks and the empty-drive poll, and faint read/write
hiss during disk DMA. Only step pulses that actually fire the stepper are
audible: like a real 3.5" mechanism, an outward pulse with the head at
track 0 is gated by the /TRK0 sensor, so NoClick-style patches silence
the empty-drive poll just as they do on real hardware.

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

On Linux with PipeWire/PulseAudio, individual sinks are not ALSA devices, so
only the `default`/`pipewire` route is offered; pick the output in the desktop
sound settings (or route Copperline in `pavucontrol`) and it follows. macOS and
Windows select each device directly.

## `[input]`

```toml
[input]
port1 = "mouse"        # mouse | joystick | cd32 | analogue | none
port2 = "joystick"     # same values; default "cd32" on the CD32 profile
joystick = "gamepad"   # "gamepad" (default) or "keyboard"
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

## `[serial]` -- serial port and MIDI

```toml
[serial]
mode = "stdout"          # off, stdout, midi, tcp, or pty
# midi_out = "FluidSynth"  # midi mode: host destination, substring match
# midi_in = "Keystation"   # midi mode: host source, substring match
# listen = "127.0.0.1:1234"  # tcp mode: bind address
```

The Amiga serial port doubles as the MIDI port. `mode` selects where
Paula's serial in/out is connected:

- `stdout` (the default) -- serial output prints to the host terminal,
  matching the historical behaviour (DiagROM and similar tools log here).
- `off` -- serial output is discarded and there is no serial input.
- `midi` -- serial in/out is bridged to host MIDI endpoints. Needs a build
  with the `midi` feature (the default); `midi_out`/`midi_in` name the
  endpoints by case-insensitive substring (a USB interface or a virtual
  port). `--list-midi` prints the host endpoints.
- `tcp` -- serial in/out is bridged to a host TCP port, like UAE's `TCP:`
  device. `listen` sets the bind address (default `127.0.0.1:1234`);
  connect with e.g. `nc`, `socat`, or a raw-mode telnet client.
- `pty` -- serial in/out is bridged to a host pseudo-terminal (Unix only).
  The slave path (`/dev/pts/N`) is logged at startup; attach a terminal
  with e.g. `minicom -D`, `screen`, or `cu -l`.

With an `AUX:` shell on the Amiga side, `tcp`/`pty` give a remote AmigaDOS
console. `--serial MODE` overrides the mode per run, and
`--midi-out NAME`/`--midi-in NAME` imply `mode = "midi"`. The launcher's
**Serial** tab and the in-window **MIDI In / MIDI Out** menu items select
the MIDI endpoints interactively.

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

Supported image formats: standard 901120-byte DD ADF, gzip-compressed
images (ADZ), single file ZIP archives, DMS archives, UAE extended ADF, and
read-only SCP flux images. DMS, gzip, and SCP images are decoded at load time
and always treated as write-protected; set `write_protected = false` on a plain
ADF to allow write-through updates to the image file.

The native loader deliberately rejects IPF/CAPS images. Useful native support
would require either a direct IPF parser or an optional SPS/CAPS library, with
its licensing, platform packaging, and dynamic-loading strategy settled before
it becomes a desktop dependency. (The browser frontend has its own
write-protected media decoder and does accept IPF.)

A `paths` playlist lets multi-disk software that only drives DF0: run
without a second drive: the first entry is the boot disk and the disk-swap
shortcut (`Cmd+D` on macOS, `Alt+D` on Linux/Windows) or the status-bar
swap button cycles to the next image, wrapping around.

## `[ide]` -- IDE hard disks

```toml
[machine]
profile = "A600"             # IDE needs a machine with an IDE port
                             # (A600 or A1200 Gayle, or the A4000)

[ide]
master = "AmigaSYS.hdf"      # raw flat HDF, read/write
# slave = "scratch.hdf"
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

A path may also name a **host directory**: its tree is built into an
in-memory FFS volume at startup (volume name = directory name, files and
subdirectories included; entries whose names cannot exist on an Amiga
volume are skipped with a warning). The guest sees an ordinary bootable
FFS disk and may write to it, but the volume lives only in memory --
nothing is written back to the host directory, and changes are lost at
exit. Note that the stock A1200/A600 Kickstart `scsi.device` only probes
the IDE master; a slave drive needs a guest OS or driver that supports
two units (e.g. Kickstart 3.1.4).

To override the volume name (instead of inheriting the directory name),
give the drive as a table with `path` and `name`:

```toml
[ide]
master = { path = "/host/Games", name = "Games" }
# slave = "scratch.hdf"        # the bare-string form still works
```

The name sets the FFS volume label of a directory mount; AmigaDOS volume
names hold up to 30 characters and cannot contain `:` or `/`. It has no
effect on a raw HDF, which carries its own label inside the image.

The drive responds to ATA IDENTIFY with the Gayle byte order real hardware
uses, so both Kickstart 3.1 variants boot from it. An HDD activity LED
appears in the status bar on IDE machines. On the `A4000` profile the same
`[ide]` section attaches drives to the motherboard IDE interface at
`$DD2020` (no Gayle involved; Kickstart's `scsi.device` drives it the same
way).

CD images (`.cue`/`.iso`) are rejected here: the emulated IDE port speaks
plain ATA, not ATAPI. Attach CD-ROM drives as `[scsi]` units instead (see
below).

## `[scsi]` -- SCSI controllers

```toml
[scsi]
# controller = "a2091"       # a2091 (default), a4091, or a3000
rom = "a2091-v6.6.rom"       # boot ROM image (a2091/a4091; the a3000 needs none)
# rom_odd = "a2091-odd.rom"  # a2091 only: split even/odd EPROM dumps
unit0 = "workbench.hdf"      # SCSI IDs 0-6
unit1 = "data.hdf"
unit2 = "game.cue"           # a .cue or .iso attaches a CD-ROM drive
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
  autoconfig board, for machines with a 32-bit CPU. It needs a raw A4091
  EPROM image (e.g. the open-source `a4091.rom`) as `rom`; it has a single
  ROM, so `rom_odd` does not apply.
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
partition, named after the SCSI ID), and host directories built into
in-memory FFS volumes -- including the `{ path = "...", name = "..." }`
table form that overrides a directory mount's volume name. The HDD
activity LED covers SCSI traffic too.

A `unitN` path ending in `.cue` or `.iso` attaches a **SCSI CD-ROM
drive** at that ID instead of a hard disk: a read-only removable SCSI-2
target (INQUIRY device type 5) serving 2048-byte blocks, with the full
READ TOC / READ CD / mode-page surface CD filesystems expect. Cue/bin
images may mix data and audio tracks; a bare `.iso` is a single data
track. The drive answers on the host adapter's `scsi.device` like any
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
the status bar's CD load/eject buttons, dropping a `.cue`/`.iso` on the
window, the scheduled `--insert-cd-after SECS PATH` flag, or the control
protocol's `media.cd.insert` all eject the current disc, run the tray
for a second of emulated time, and mount the new one with a
medium-change unit attention for the guest's filesystem to notice.

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
path, which snapshots the tree into an in-memory FFS volume at startup.
Up to 8 mounts.

The volumes are read-write by default: the guest creates, writes, renames,
and deletes the host's files directly, and changes land in the directory
as you would expect. Set `readonly = true` to export a directory
write-protected instead -- the guest sees a read-only disk and every write
fails with the same "disk is write-protected" error a physical
write-protected disk gives, which is worth setting on anything you would
rather the Amiga could not damage. The launcher's Host Mounts tab exposes
the same choice as its **Access** field.

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

## `[cd]` -- CDTV and CD32

```toml
[machine]
profile = "CD32"

[cd]
image = "disc.cue"        # BIN/CUE cue sheet (MODE1/2048, MODE1/2352, AUDIO)
insert_delay = 0.0        # emulated seconds after power-on to insert
# nvram = "cd32-nvram.bin" # CD32 save-game EEPROM backing file (default)
```

The disc mounts on the machine's CD controller: Akiko on CD32, the DMAC on
CDTV. `insert_delay` inserts the disc some emulated seconds after power-on
with the proper media-change notification; some CDTV discs only boot when
inserted after the boot screen appears. CD32 NVRAM
persists to `cd32-nvram.bin` next to the working directory unless
overridden; without a path the EEPROM is session-only.

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
net = "loopback"   # or "none" for an isolated NIC
```

Fits a Commodore A2065 Ethernet board (Am7990 LANCE) on the Zorro chain.
`net` selects the host network backend: `"loopback"` echoes transmitted
frames back (self-contained, useful for driver bring-up), `"none"` leaves
the NIC isolated. Omit the section entirely for no board. Note that host
networking is inherently non-deterministic: inbound frames arrive on the
host's schedule, not the emulated clock, so a NIC board breaks
byte-identical replay and save-state determinism while traffic flows. See
[](../zorro) for the board details.

## `[debug]` -- diagnostics

```toml
[debug]
log_unmapped = "DD0000-DEFFFF"
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
