# Changelog

User-visible changes and upgrade notes for Copperline. New entries go under
**Unreleased** in the same change as the behaviour they describe. Entries
before this changelog was introduced are selected compatibility milestones;
see the [GitHub releases](https://github.com/CopperlineHQ/Copperline/releases)
for the earlier release notes.

## Unreleased

### Added

- The A2000, A3000 and A4000 model Buster expansion-bus arbitration for
  SCSI controllers and DMA-capable WebAssembly cards. Memory TAS, CAS and
  CAS2 hold the bus through their operand cycles. A3000 and A4000
  configurations can select Super Buster revision 7, 9 or 11 with
  `[machine] buster_revision`.

- The DAP adapter serves the debugger features the Amiga Assembly VS Code
  extension (`prb28.amiga-assembly`) offers for FS-UAE and WinUAE, so its
  `emulatorType: "copperline"` sessions get them too. The Debug Console
  and Watch take the UAE debug adapter's commands: `m address[,size[,
  wordSize[,rowSize]]][,ab|a|b|d|c]` dumps memory (or disassembles it),
  `M address=bytes` writes memory from the Debug Console, `d` and `c`
  disassemble CPU code and Copper lists, and `h` prints help.
  Expressions accept its memory references (`@(addr[,size])`,
  `@s(...)`, `#{addr}`, `${expr}`), `0b` binary numbers and the bitwise
  operators with uae-dap's 32-bit semantics. Hovering a label, or
  watching a label or address register, expands to the memory there. The
  extension's *Disassemble memory* and *Disassemble copper memory*
  commands and its *View as Decimal / Hex / Binary* menu work. The
  Variables view gains a Segments scope with the program's hunks, address
  registers and PC show the program label they point into
  (`<twice+$8>`), and the Debug Console completes register, symbol and
  variable names (see docs/debugger/dap.md, "Debugger commands").
- Crashes in the IDE debugger: when an exception breakpoint stops a program
  (an address error, an illegal instruction), the DAP adapter unwinds
  through the exception stack frame, so the call stack continues from the
  handler into the instruction that raised the exception, at its source line
  from vasm `LINE` debug hunks or DWARF, with that code's registers, SR and
  stack, then its callers. The exception details name the faulting address,
  its line, and the address a bus or address error accessed (see
  docs/debugger/dap.md, "Crashes"). In the control protocol a `catch` stop
  reports `exception` (`vector`, `name`, `handler`, `fault_pc`), `regs.get`
  reports `usp` (and `msp` on the 68020-68040), and a catch stop's description names the faulting
  instruction (`Caught Address error (vector 3) at $C0123A, handler
  $F80A2E`).
- Animated clips can be saved as APNG (animated PNG) as well as GIF. The
  Recording menu's new Clip Format, or `[recording] clip_format = "apng"`,
  makes Save Clip write `copperline-clip-<timestamp>.png`, and a headless
  `--gif-after` clip whose path ends in `.png` or `.apng` is written as
  an APNG. An APNG is lossless: a clip whose frames use 256 colours or
  fewer between them shares one palette, and HAM or AGA pictures with more
  are kept as 24-bit RGB instead of being reduced to 256 colours. Frames
  store only the area that changed, which usually makes the file much
  smaller than the GIF, and frame timings are exact to the millisecond, so
  50 and 60 fps clips play at their real rate. Browsers show an APNG in an
  `<img>` tag like a GIF. GIF stays the default.
- The configuration screen has a Recording page (A/V & Emu -> Recording)
  for the `[recording]` section: the clip format (GIF or APNG), clip
  length and frame rate, and the video format (AVI or MP4). With MP4
  chosen it shows which ffmpeg the recording will use, or that none was
  found, and **Browse** picks a different one. These settings are kept
  when the machine model is changed.

### Fixed

- A save state preserves the 68040/68060 MMU translation cache. Resuming
  keeps the resident page mappings and the bus clocks a table walk would
  consume on a miss.

- A save state now records the identity (length and SHA-256) of the board
  firmware built into Copperline that its machine maps -- the
  `copperhf.device` boot ROM, and the bundled HostSocket and zz9k boards'
  plugin modules and HostSocket ROM -- in a new optional `BROM` chunk. A
  build whose firmware differs refuses such a state with an error naming
  the board and saying to boot afresh, instead of resuming the guest
  against changed firmware and hanging it.
- An interlaced screen whose display window stops on the long field's last
  line (PAL 312, NTSC 262) no longer shows its two fields woven in the wrong
  order. The vertical display window now closes on the last line of the
  field being scanned, so an interlaced short field closes it one line
  earlier instead of carrying it open into the next field, which started
  that field's picture a line early. The Video Toaster 4.2 switcher screen
  was the visible case. The A1000's original Agnus closes the window on
  line zero of the next field instead, so there an open window now shows
  each field's last line.
- Native screenshots (`--native-screenshots`, the native screenshot
  shortcut and the control protocol's `capture.screenshot` with
  `native: true`) of interlaced software that alternates its per-field
  images without reading the long-field flag are no longer combed one line
  apart when the images landed on the opposite fields. As the window's
  deinterlacer already did, the capture puts the short field on the even
  rows when that order is clearly smoother on four consecutive pairs of
  fields; blank, moving or ambiguous pictures keep the long-field order.
  Kang Fu (CD32) showed its GREED splash text combed.
- A netplay control link (setup, disk changes and the spectator feed) no
  longer fails with "setup data exceeds receive window" when the receiver
  holds a window of data behind messages its session has not taken yet,
  as a spectator catching up on a long backlog could. The sender counted
  those held packets as delivered and ran a whole window past what the
  receiver could accept; it now waits for the receiver to take them.
- An animated GIF clip no longer plays some frames slowly in web
  browsers. A frame that the next one replaced a centisecond later got a
  one-centisecond delay, which browsers show for ten; it happened at
  `[recording] clip_fps` rates above 50, above about 45 on an NTSC
  machine, and wherever frames were presented unevenly. No frame is now
  shorter than two centiseconds: a frame replaced sooner is dropped, and
  the clip keeps its length.

### Compatibility

- States written by 1.2.0 and earlier carry no firmware identities and load
  as before, unchecked, with a warning naming the built-in firmware their
  boards map. 1.2.0 skips the new chunk (with a log line), so the chunk
  does not stop it reading states this build writes. Save states keep
  container version 82 and every existing chunk keeps its version.

## 1.2.0 - 2026-11-21

### Added

- A video slot, with an emulated NewTek Video Toaster 4000
  (`[video_slot] card = "toaster"`) and the NewTek Video Flyer beside it
  (`flyer = true`). The NewTek switcher (Toaster 4.x) starts and runs with
  its muxes, fader, keys, framestores, wipes and digital effects; the card's
  inputs, recordings and Flyer drives are folders in a host `media` folder,
  ffmpeg decodes video inputs, and the Preview and Program outputs open in
  monitor windows, Program recording to MP4 or AVI. The Flyer plays clips
  and sequences into inputs 3 and 4 with their sound, the `flyerclip` and
  `flyer-convert` tools (source and Homebrew builds) convert video files to
  and from Flyer clips, and an MCP client can drive the Toaster software
  through its ARexx ports and manage Flyer clips (`toaster_*` and `flyer_*`
  tools). The Toaster software and the Kickstart ROM are not included (see
  docs/guide/configuration.md).
- An `A2000` machine profile (`--model A2000`, `[machine] profile = "A2000"`,
  and in the configuration screen's machine selector): the B2000 Rev 6.x
  board, with the 8372A Agnus and OCS Denise, 1M of chip RAM, the
  battery-backed MSM6242 clock and its Zorro II slots. The Rev 4.x and late
  Rev 6 boards are overrides on it (see docs/guide/configuration.md). The
  FS-UAE and WinUAE importers map their `A2000` to it.
- `--terminal` (macOS and Linux) shows and drives the machine in a terminal
  that speaks the Kitty graphics and keyboard protocols (kitty, Ghostty,
  WezTerm) instead of a window, including over SSH with no display server.
  F12 is the host key; `--terminal-fps` caps the frame rate and
  `--terminal-log` sets the log file (see docs/guide/terminal.md). The
  terminal is given back however the session ends, including on SIGTERM,
  SIGHUP, SIGINT and SIGQUIT (exit status 128 plus the signal number), and
  the default log file is always a new, owner-only file.
- `tools/publish-web` packs a floppy title, described by the same manifest
  as the Player kit, into one self-contained HTML file holding the browser
  emulator, the AROS boot ROM, the disk images and the licence texts. The
  page requests nothing but itself, so it runs from any static host or
  straight from disk, with keyboard, mouse, gamepad and touch controls, a
  DF0 list for multi-disk titles, disk changes kept in the visitor's
  browser and, when the manifest allows it, a quick save state (see
  docs/guide/publishing.md).
- High-density floppies: 1,802,240-byte HD ADFs, HD DMS archives, extended
  ADFs whose AmigaDOS tracks hold 22 sectors, and HD ADFs in ZIP files load
  in every drive. As on an HD mechanism, density follows the inserted disk:
  an HD disk turns at half speed with 22 sectors per track and the drive
  answers the ID poll as an HD drive only while one is in, so DD disks run
  as before. The guest needs an HD-aware `trackdisk.device` (Kickstart 1.x
  has none); IPF images of HD disks are still refused.
- Video recordings can be MP4 files (H.264 video, AAC sound) made by the
  host's ffmpeg: `[recording] video_format = "mp4"` or **Recording -> Video
  Format**. The recorder still captures its lossless AVI and hands it to
  ffmpeg in the background when recording stops, removing the AVI only once
  the MP4 is written; without ffmpeg, or with one lacking libx264, the AVI
  is kept. `[recording] ffmpeg` or `COPPERLINE_FFMPEG` names the program,
  and AVI stays the default (see docs/guide/ui.md).
- Files named on the command line or opened from a file manager start what
  they are: floppy images boot in DF0 (several form its swap playlist),
  WHDLoad archives, game folders and slaves boot directly, CD images go in
  a CD32 or CDTV drive, and a `.toml` file loads as the configuration. The
  Windows installer registers **Open with** entries for them, the macOS
  application declares them to Finder, and the AppImage tools archive adds
  `copperline-desktop-setup` to register them per user (see
  docs/guide/getting-started.md).
- `[chipset] genlock = true` fits an external genlock supplying sync:
  BPLCON0.ERSY then slaves the beam to it, so `VPOSR`/`VHPOSR` keep
  counting, instead of stopping the counters as an empty genlock socket
  does. Kickstart's genlock probe finds it and hides the 31 kHz modes a
  genlock cannot follow. A netplay guest takes the host's setting.
- CD subcode R-W data (CD+G graphics) reaches the CD32 and the CDTV from
  images that stored it: a CloneCD-style `.sub` file beside the image, a cue
  sheet `CDG` track, or a CHD with raw subcode. The CDTV drive now delivers
  subcode bytes at all, behind SCOR, while a disc plays.
- `[[tape]]` puts a SCSI tape drive on the `[scsi]` controller. The
  cartridge is a host directory with one file per tape file, or a single
  host file; the files hold only the data, so `tar` archives are tape files
  as they stand (see docs/guide/configuration.md).
- The CDTV's battery-backed bookmark RAM is kept in a file, so title saves
  and preferences survive between sessions: `cdtv-bookmark.bin` in the
  `[paths]` nvram folder by default, another file with `[cd] nvram`, and
  session-only as before with `nvram = ""` or under `--factory`.
- `[audio] live_lead_ms` sets how much sound the live output keeps ahead of
  the host's audio device, 50 to 1000 milliseconds (default 150, the lead
  it always had). A machine near the host's limit, such as a Video Toaster
  with its Flyer playing a project, sounds smoother with a longer lead, at
  the cost of the sound trailing the picture by that much.

### Changed

- With the default `mouse_capture = "click"`, a fullscreen window that shows
  only the display (status bar hidden, no keyboard, MT-32 or Coppersynth
  strip) takes the mouse without a click, as `auto` does: at start-up, on
  focus, on entering fullscreen or hiding the status bar, and when a menu or
  panel closes. Every click there would land on the display anyway, so a
  session started fullscreen no longer needs one before the Amiga pointer is
  the only one on screen. Wherever a mode grabs unasked, a machine started
  by clicking the launcher's Run now takes the grab, as one started from a
  pad or the keyboard already did.
- `[cpu] unimplemented = "trap"`, the default, now covers the 68040 as well
  as the 68060: the transcendental instructions, FMOVECR, packed decimal and
  denormalized or unnormalized operands raise the exceptions 68040.library
  handles, as on the real chip. Without that library such an instruction
  ends in a Line-F exception (Software Failure #8000000B under AmigaOS);
  `unimplemented = "native"` computes them in the core as before. The
  68EC040 and a 68040 with `fpu = false` are unaffected.
- The CPU core is the published m68k 0.17.1, with conformance fixes found
  by a bare-metal CPU test suite: DIVU/DIVS and CAS/CAS2 flags, no spurious
  trace after an exception, each model's RTE and address-error or bus-fault
  frame formats, FPU exceptions enabled in FPCR taken through their vectors
  (they were ignored), and MMU table searches that write the used and
  modified bits back into the descriptors. A 68030 root pointer's limit is
  now enforced.
- `[cpu] jit` with the MMU in use (MMULib on a 68040, for example) costs
  about a quarter less host CPU time, from translated-memory,
  indexed-address and bitfield fast paths in the CPU core and a cheaper JIT
  boundary check.
- `[cpu] jit` runs the `[copperhf]` device driver and the `[[filesys]]`
  handlers at CPU speed: their boot ROMs are mapped as local memory, like
  Kickstart, instead of costing an expansion-bus access for every opcode.
  Filesystem-heavy work on those volumes takes less than half the emulated
  time it did. The precise core is unchanged.
- `--gdb`, `--gdb-gui` and `copperline-ctl --dap-listen` accept only
  loopback addresses unless `--allow-remote-debugging` is given, and every
  address a hostname resolves to must pass. The control server never prints
  its token: it prints its endpoint and the `--control-info` file, creating
  a temporary one when none is named, and its tokens always come from the
  operating system's random source. A WebAssembly board is limited to one
  linear memory of 320 MiB and a table of 65,536 elements.
- `[[filesys]]`, `--run` and WHDLoad host-directory volumes report the disk
  type `DOS\1`, a plain FFS disk, as the UAE filesystem does, instead of
  Copperline's own `CLFS`. Software that lists only `DOS\0`-`DOS\5` volumes,
  such as the Video Toaster's file requesters and `C:Info`, now shows them.
- `[[filesys]]`, `--run` and WHDLoad host-directory volumes carry a fixed
  date, 1 January 1978, instead of the host time their handler started, and
  so do the volumes Copperline builds from a host directory for an `[ide]`
  or `[scsi]` drive, so identical runs no longer vary with the host clock. A
  machine with no battery clock that boots from a host directory starts its
  clock at that date; fit an RTC, or seed one with `rtc_time` /
  `--rtc-time`, for another.
- The bundled AROS ROM is rebuilt from upstream master b6dcaeeaa2
  (2026-11-19). Upstream fixes reaching it include a task switch that could
  leave a signalled task never dispatched again, DOS allocations no longer
  taken from the top of memory (where they collided with CD32Load's
  copies), amigavideo blitter limits and chip-RAM checks, ATAPI media
  changes, and the CD32 `cd.device` interrupt-mask update that could stop
  I/O in an interrupt storm (see assets/aros/README.md).
- The browser, libretro and publisher-kit player frontends are now members
  of the repository's Cargo workspace. They share the root `Cargo.lock` and
  build into the root `target/` directory, so a source-built libretro core
  is at `target/release/` rather than `crates/copperline-libretro/target/release/`.

### Fixed

- `A500OCS` with `[scsi] timing = "period"` now selects the A590 sidecar's
  `seagate-st125n` period drive profile, matching `A500`, `A500+`, `A1000`,
  and `CDTV` rather than falling through to the big-box A2000HD profile.
- The VS Code extension's `attach` launch schema now exposes the `windowed`
  boolean option (default `true`) supported by `copperline-ctl --dap`.
- `copperhf.device` implements `AbortIO`. A held `TD_ADDCHANGEINT` is
  released with `IOERR_ABORTED`, so a task that exits without
  `TD_REMCHANGEINT` no longer waits forever, and a read or board-answered
  command still queued on the board is withdrawn. Writes already on their
  way to the medium complete normally.
- On macOS, switching away with Cmd-Tab no longer leaves the Amiga holding
  Left Amiga. Mouse clicks stopped working after switching back (Intuition
  read each one as a screen drag) until Cmd was pressed again in the
  window. Any key or keyboard-joystick control held on the host keyboard
  while the window loses the focus is now released; `--key-after`,
  `--script` and control-protocol key holds still end at their own release.
- The Windows uninstaller removes its Start Menu and desktop shortcuts even
  when another program has one of them open at that moment. It used to leave
  such a shortcut behind.
- On 68020 and faster machines, an interrupt that a program leaves pending
  in INTREQ and unmasks in INTENA for only a few colour clocks is now
  taken. The CD32 Classic Lotus Trilogy froze at the Lotus 1 race screen,
  with only the HUD drawn, waiting for a handler that never ran.
- An ECS Agnus 8372A machine with 512K of chip RAM mirrors that RAM at
  `$080000`, `$100000` and `$180000`, as an OCS machine does. Loaders that
  size chip RAM by looking for the alias no longer take the default
  512K + 512K ECS A500 for a 1M-chip machine; a cracked Cannon Fodder ended
  in a Guru there.
- AGA playfields and sprites move in Lisa's 35 ns steps: BPLCON1's H1/H0
  bits scroll a lo-res playfield by quarter pixels and H0 a hi-res one by
  half pixels, and SPRxCTL's two fine position bits move a sprite by 70 ns
  and 35 ns in every display mode, not only the 70 ns bit in super-hi-res.
  Sprite-playfield collisions compare at the same pitch. SANITY's Roots 2.0
  lines its sprite screen up with its colour-bank switches this way.
- AGA hi-res and super-hi-res scroll counters wrap at the width of the
  selected resolution, correcting where a super-hi-res scroll wraps with
  32-bit fetches, and a bitplane fetch window that starts at colour clock
  zero keeps its geometry, its fetches before `$18` included, when DDFSTRT
  is rewritten behind the beam.
- A sprite armed by a CPU or Copper write with sprite DMA off is shown on
  every line, in that frame and in every later frame without sprite DMA,
  whatever its VSTART and VSTOP, until an SPRxCTL write disarms it, as
  Denise, which has no vertical comparator, does. It also sets CLXDAT bits
  in each of those frames, not only in the frame it was armed in.
- AGA sprites widened to 32 or 64 pixels by FMODE collide over every word
  they shift out, not only over their first 16 pixels.
- A Copper WAIT evaluated on line 255 compares the eight vertical bits as on
  any other line, so a WAIT for a line below `$80` releases there at once
  instead of waiting for the counter to wrap and count up to it again. The
  `WAIT $FFDF,$FFFE` idiom for reaching the lines after 255 is unaffected.
- After a CPU strobe of COPJMP1 or COPJMP2 the Copper spends a bus-request
  cycle before it fetches the new list, so the list's first MOVE lands two
  colour clocks later. A read-modify-write strobe such as `clr.w COPJMP1`,
  which strobes twice, no longer restarts the Copper from a half-written
  COP1LC when that first MOVE rewrites it.
- A chip-RAM write by a 68020 or faster CPU gets the cycle a BLTPRI-clear
  blit yields to a waiting CPU, as the CPU's other chip accesses do. Under
  a blit using all four channels such writes waited for the whole blit,
  slowing the CPU several hundredfold.
- VPOSR's V8 steps with VHPOSR's line byte, so reading VPOSR, VHPOSR and
  VPOSR again no longer reports line 0 for line 256, or line 256 at the
  frame wrap, and loops that count frames from the beam position no longer
  count extra ones. A 68000 long read of a custom register pair returns its
  first word from that word's own bus cycle, two colour clocks earlier than
  before.
- A CPU halted in `STOP` wakes on time for an audio DMA block-end
  interrupt. The handler could be entered thousands of colour clocks late,
  so programs that sleep between audio interrupts measured wrong intervals
  and refilled short blocks late.
- A `JOYTEST` write loads only bits 7-2 of each quadrature counter, keeping
  the two input bits, and `JOYxDAT` behind a joystick or CD32 pad returns
  the counter's bits 7-2 above the direction bits, as the chip does: a
  `JOYTEST` of `$FFFF` now reads back as `$FCFC`, plus the input bits, on
  either kind of port.
- The battery clocks at `$DC0000` can be set: the MSM6242 and RP5C01 take
  counter writes, stop and restart, and carry by their own rules, and the
  MSM6242's STOP, REST, 24/12, HOLD, BUSY and 30-second adjust work, so
  `SetClock SAVE` sets the time. A time the guest sets lasts for the session
  and its save states; the host clock is never changed, and the next power
  cycle starts from the host time or `rtc_time` again.
- An IDE drive reports the heads and sectors per track that its disk's
  Rigid Disk Block records instead of always 16 and 32, so a disk
  partitioned for another geometry keeps the end of its last partition
  reachable by a Kickstart `scsi.device` that addresses it by cylinder,
  head and sector. A disk without a usable RDB keeps 16 and 32.
- IPF weak bits read differently on each revolution, over a repeating
  cycle of eight, instead of identically every time.
- A ZIP-wrapped floppy image whose file name and extra field together run
  past 64 KiB no longer panics a build with overflow checks enabled; it is
  refused as release builds already refused it.
- An ADF in a ZIP written as a stream, with the entry's sizes and CRC in a
  data descriptor after its data rather than in its header, is read at
  either density. macOS Finder's Compress writes every archive that way,
  so an ADF zipped on a Mac was refused with "invalid ADF file size in ZIP
  archive".
- An IPF image whose gap sample is longer than a track is refused instead
  of aborting the process on an impossible allocation.
- A hard-drive or SD-card write that does not fit inside the drive is
  refused; it grew a raw image file, or panicked with an in-memory image.
  A dynamic VHD whose block table points past the end of the file, as a
  truncated copy's does, is refused at open, and a CHD's metadata is read
  one entry at a time, at most 256 entries and 1 MiB in all.
- Cue sheets: a FILE whose first INDEX is not at 00:00:00 keeps its later
  tracks on the sectors the sheet names (they read the wrong data before),
  a sheet saved with a UTF-8 byte order mark loads, a layout too long to
  address is refused, and so is a WAVE track claiming a sample rate above
  768 kHz.
- UAE extended ADFs, PCMCIA SRAM cards, the CD32 NVRAM and the RP5C01
  battery RAM are written to a temporary file that then replaces the old
  one, so a write cut short (a full disk, a killed process) no longer
  leaves the file short and its contents lost. The file keeps its mode
  and, on Unix, its owner and group.
- External floppy drives answer the /RDY ID probe while their motor is off,
  including before the first motor cycle, so boot-time probes can find DF1.
- Covered or minimised windows no longer leak GPU staging memory.
- A guest can no longer create a file, directory or rename target outside a
  `[[filesys]]` host directory on Windows by putting a drive letter in the
  name, nor open a DOS device such as `NUL` or `COM1` through one. Names
  containing `:` are refused on every host.
- A guest can no longer create, overwrite or rename a file onto
  `copperline.toml` in a `[[filesys]]`, `--run` or WHDLoad host directory,
  where a later run started from that directory would load it as its
  configuration, nor make a directory of that name or write it through
  uaelib's `debug_save`, in any letter case or with trailing dots or
  spaces.
- A file the host creates in a `[[filesys]]`, `--run` or WHDLoad host
  directory after the guest has looked for it and missed is found, as a
  listing already showed it, so a rebuilt `--run` program is picked up. A
  `.uaem` sidecar whose date holds a multi-byte character no longer panics
  the emulator.
- A `[[filesys]]` mount holds at most 96 open files and 32 unfinished
  directory scans, refuses negative `Read()`/`Write()` lengths and deletion
  of its own root, and logs an out-of-memory guest buffer once per request
  instead of once per byte.
- A `copperhf.device` read or write whose buffer is not guest RAM fails
  before any host I/O instead of first allocating and reading the full
  request length.
- A TCP serial or modem peer that stops reading no longer freezes the
  emulator: a write that cannot progress for one second drops the link.
  Input the guest has not read yet is limited to 64 KiB per link, during
  a modem call too, and an unterminated telnet subnegotiation keeps only
  its first 64 bytes.
- The GDB stub survives a client that resets, disconnects mid-packet or
  sends a malformed packet: the session ends as a detach and the server
  waits for the next client instead of exiting. A continue notices a client
  that has gone, a stray byte no longer hides Ctrl-C, a watchpoint covers
  at most 4096 bytes and a connection holds at most 32, and `qXfer` replies
  escape the program names that contain packet framing characters.
- A memory size too large to count in bytes, such as
  `--fast 17592186044424M`, is refused; a release build wrapped it to a
  size that was never asked for.
- The browser build stops downloading a linked disk image as soon as it
  passes the 64 MiB limit, including when the server sends no length.
- Save states carry the CPU's interrupt-sample latches and the chip data
  bus value, so a resumed run takes an interrupt that was recognisable at
  the save point on the same instruction as an uninterrupted run.
- The libretro core no longer publishes a chip-RAM pointer during netplay,
  where rollbacks replace the RAM buffers and a cached pointer would dangle.
- The libretro core announces its memory map with the command `libretro.h`
  defines, so a frontend's cheat search, memory viewer and achievement
  support see every memory region rather than only system RAM.
- The AppImage includes Copperline's licence and the AROS README and
  acknowledgements, as the other packages do.
- Netplay: a host accepts only small control messages from guests and
  spectators instead of buffering up to 512 MiB per link, sends one packet
  at a time to a peer that has not answered yet, and drops a spectator that
  acknowledges nothing for ten seconds during setup. An oversized or failed
  UDP datagram no longer ends a session (it did on Windows), and a host
  cannot make a guest load drive sound samples from a path of its choosing.
- Loading a save state logs a warning for each host file, directory, plugin
  module or network backend the state names that the running session had not
  opened. `SECURITY.md` now states which inputs are trusted: a save state has
  the same access to the host as a configuration file.
- Command-line help is written to stdout. Options accept `--flag=value`,
  misspelled options suggest close matches, and fatal desktop-launch errors
  appear in a native dialog.
- Opening a selection of files Copperline cannot combine from a file manager
  (two CD images, an archive with a disk image) shows the reason in a dialog
  instead of exiting with nothing shown.
- WHDLoad archive extraction limits each member to 256 MiB, each operation
  to 1 GiB of decoded data and 16,384 entries. Oversized or malformed packages
  fail with an error and incomplete staging is removed.
- Windows installers support per-user and all-users installation, update
  existing installations, and preserve settings and saves on uninstall. The
  shipped executables carry their product name and version in
  **Properties -> Details**.
- The manual identifies its package version and source revision, and includes
  troubleshooting, known limits and compatibility notes.
- A control-protocol breakpoint condition that names `d0`-`d7` or `a0`-`a7`
  tests that register. The names were read as the hex constants `$D0`-`$A7`,
  so a conditional breakpoint set from an IDE through the debug adapter
  compared a constant.
- `--pcmcia-cf`, `--pcmcia-sram`, `--serial-device`, `--insert-cd-after`,
  `--waveform`, `--warp-boot`, `--warp-until` and `--profile-live-audio` start
  the machine when given on their own. They opened the launcher, which
  dropped them.
- In the Debug layout, `Cmd+Shift+G` / `Alt+Shift+G` saves a GIF clip instead
  of toggling input ownership, and `Cmd+Shift+V` / `Alt+Shift+V` pastes as
  keystrokes.
- `[machine] mem_controller` ignores case and surrounding spaces, the
  `[chipset] agnus` error lists `8372B`, the rewind-is-off notice names the
  *Emulation Settings* menu, and the MCP description of `copper.list` gives
  its real default start.
- The WebAssembly board host is built with wasmtime 36.0.21, which fixes
  RUSTSEC-2026-0566, RUSTSEC-2026-0569 and RUSTSEC-2026-0571.

### Compatibility

- No configuration key or command-line option is removed or renamed. Save
  states keep container version 82 and every chunk keeps its 1.1.0
  version: what this release adds to a state (the video-slot card, tape
  drives, and new CPU, chipset, drive and board fields) is in new fields
  that older states lack and load with defaults, so this build reads 1.1.0
  states. A WebAssembly board plugin, or a state of one, that needs more
  than 320 MiB of linear memory or a table of more than 65,536 elements is
  refused.
- The built-in `copperhf.device` driver has changed, and it runs from a
  board ROM that save states do not carry, so a 1.1.0 state saved with a
  `[copperhf]` drive attached can hang once loaded in this build (a
  Kickstart 3.1 A1200 state saved while booting from one stops at an empty
  Workbench screen). Boot such a machine afresh and save a new state.
- Compatibility is one-way: 1.1.0 cannot read any state this release
  writes, because its decoder requires fields that are no longer written,
  the floppy drives' `external_id_mode` and the device clock's wall-clock
  fields.
- Netplay still needs the same Copperline build on every peer, so a 1.2.0
  peer cannot play with a 1.1.0 one.
- On a 68040 with an FPU, the default `[cpu] unimplemented = "trap"` now
  leaves the instructions the 68040 does not implement to 68040.library. A
  bare Kickstart, a `--run` program on the bundled AROS ROM, or a WHDLoad
  game that uses them without that library stops with a Line-F exception;
  `unimplemented = "native"` restores the 1.1.0 behaviour. Software that
  enables FPU exceptions in FPCR now takes them.
- A GDB or TCP DAP listener on a non-loopback or wildcard address needs
  `--allow-remote-debugging`. The control server no longer prints `token=`
  on stderr; scripts read the token from the `--control-info` file, or from
  the temporary one whose path it prints.
- A file named on the command line is taken as what its type says: floppy
  images, WHDLoad archives and folders, CD images and `.toml` files are no
  longer the Kickstart ROM path. A `.rom`, `.bin`, extensionless or
  otherwise unrecognized name still is.
- The refreshed AROS ROM leaves a booted machine about 3 KB less free
  memory, and its partition.library has lost its table-writing functions,
  which moves the offsets of every function after them: HDToolBox or
  `C:Partition` from an AROS m68k disk built before that change calls the
  wrong functions under this ROM. The ROM also reaches the boot block at a
  different moment and several chipset timing fixes above move events, so
  `--expect-screenshot` references and recorded input scripts made with
  1.1.0 may need to be made again.
- A cue sheet whose FILE starts at an INDEX other than 00:00:00, with no
  INDEX 00 before it, is laid out as the sheet says: later tracks in that
  file move to the sectors it names, and a lone track of that kind starts
  at LBA 150 behind a 150-sector pregap instead of at LBA 0.
- Three inputs that silently did the wrong thing now do what the manual
  says. A scripted key written as `0` (`--press-after 5 0`) is the zero key;
  it was read as raw key code 0, the backtick key, which is still available
  as `0x00`, the form recorded input scripts use. `copperline-ctl --repl`
  given a method reports an error; the method was never sent. A number after
  `--expect-screenshot SECS PATH` that is not a valid tolerance, such as
  `1.5`, is an error; it was taken as the ROM path.

## 1.1.0 - 2026-10-31

- Added scheduled floppy ejection with `--eject-disk-after`, including the
  equivalent input-script directive.
- The launcher remembers menu settings and offers media search folders through
  `[paths]`; `--factory` bypasses saved defaults and an executable-side config.
- CIA-A and CIA-B save-state chunks moved from version 1 to 2 for corrected
  pulse-output semantics. Version-1 chunks migrate on load; a 1.0.x reader
  cannot read the newer CIA chunks. The container remains version 82.

## Earlier compatibility milestones

| First release | Change | What to do with older files |
|---|---|---|
| 0.21.0 | The CD32 FMV module became opt-in. | Set top-level `fmv = true` (or `fmv_rom = "path"`), or choose an **FMV module ROM** in the launcher, when the cartridge is wanted. |
| 0.20.0 | Save states changed from the flat format to named subsystem chunks (container 81); thumbnails and metadata then introduced container 82. | Recreate states made by 0.19 and earlier. Current readers accept container 81 without thumbnails and container 82. |
| 0.18.0 | LIDE gained named `drive0` through `drive3` keys. | The old `[lide] drives` array still loads; a named key wins for its slot. Saving from the launcher writes the named form. |
| 0.15.0 | FluxBridge replay speed became `replay_speed` / `--floppy-replay-speed`; `bridge_auto_cache` was removed. | The old `bridge_speed` key and `--floppy-bridge-speed` flag remain aliases. Remove `bridge_auto_cache`; there is no background sweep to enable. Replay speed accepts `normal` / `fast` or the older 100 / 200 values. |
| 0.3.0 | `[machine] model` became `profile`; `[emulation] speed` was retired. | `model` remains an alias in `[machine]`. Remove `[emulation] speed`: it is accepted with a warning but ignored. Use `[cpu] clock_mhz` / `speed` for CPU speed and the warp controls for host pacing. |

`[audio] audio_filter` also accepts the spelling `filter`. When updating a
config, keep the canonical key only; specifying both an alias and its canonical
key produces a duplicate-field error. Unknown keys are rejected rather than
silently ignored. The troubleshooting guide in the manual describes recovery
steps and save-state media requirements.
