# Hardware-in-the-loop reference rig

A plan for putting a real Amiga on the end of an agent-drivable interface, so
that hardware questions Copperline currently answers by arbitrating between
vAmiga and FS-UAE can be answered by measuring the actual silicon.

Status: phase 0 built and validated against Copperline; no hardware attached
yet. What exists:

- `timing-test/probesrv.asm` -- the probe server, built with
  `./build.sh probesrv` into a bootable `probesrv.adf`.
- `tools/hwrig/hwrig.py` -- host harness, drives the emulator over TCP and a
  real machine over a UART through one code path.
- `tools/hwrig/hwrig-mcu/hwrig-mcu.ino` -- Arduino Uno firmware for keyboard
  injection, reset and cold boot.
- `tools/hwrig/README.md` -- wiring, protocols, and the reproducibility caveat.

Verified end to end under Copperline: the server boots, identifies the machine
(every banner field cross-checks against the emulator's configured chipset), and
`test.bin` uploads over the wire and returns all 32 timing rows.

## 1. Why

The emulator is deterministic and the probe corpus in `timing-test/` is already
built around measuring the machine against its own CIA E-clock. What is missing
is a third column: a reference that is not another emulator.

The repo already records the gap in its own words:

- `timing-test/README.md` cites a "FS-UAE/**real-A500** reference `0x0022`" for
  row 22 -- a hand-carried number.
- Row 28-30 (68060 dispatch) says outright: "real-hardware captures welcome to
  calibrate the split".
- `ddfprobe-agafold` and `dblpal-hires-lace` are marked FS-UAE-verified only,
  "since vAmiga cannot arbitrate AGA". For the AGA probes there is currently
  **one** reference and no way to break a tie.
- Open items with no ground truth on file: `cpu-write-timing-class-`
  `characterization`, `akiko-sector-slot-order-no-ground-truth`,
  `sprite-dma-spren-edge-provisional-pass-race`,
  `vamigats-interrupt-timing-wall`, the DDF flop-FSM work.

The rig turns "run the probe on real hardware" from a manual afternoon into a
command an agent can issue, which is the difference between doing it once and
doing it every time a timing model changes.

## 2. Scope

**In:** anything a guest-side probe can measure against the E-clock or the beam
(CPU cycle costs, chip-bus contention, DMA arbitration, blitter cadence,
interrupt raise position and latency, copper-vs-CPU phase), plus anything that
resolves as a static raster (DDF/DIW placement, sprite serializer positions,
collision matrices, HAM/EHB decode).

**Out, at least initially:** anything requiring visibility into the chip bus
itself (which DMA slot a cycle actually took) -- that needs a logic analyser
clipped to Agnus/Denise, which is phase 4 at the earliest. Also out: anything
where the answer is a distribution over many machines. One machine is one
machine (see section 9).

## 3. Machine choice

The maintainer has every model, so this is a free choice. Recommendation:

**Phase 1: A500, OCS, 512K chip + 512K slow (trapdoor), PAL, KS 1.3.**

Not because it is the most valuable target, but because it is the only one that
can **validate the rig itself**. The whole `timing-test` corpus targets exactly
this configuration and both vAmiga and FS-UAE already produce numbers for it. If
the new rig disagrees with both of them on row 4 (a plain `move.w d2,d0`), the
rig is broken, not the emulator. No other machine gives that self-check, and a
measurement rig you cannot falsify is worse than no rig.

Secondary benefit: the hardest open questions on file are OCS ones.

**Phase 2: A1200, AGA, 68EC020, 2M chip.**

Highest marginal value per measurement, precisely because no trustworthy
reference exists today. `ddfprobe-agafold` (issue #248 hscroll fold),
`dblpal-hires-lace` (issue #270 sprite comparator alias), the wide-FMODE fetch
grid, SHRES, HAM8 -- all currently rest on FS-UAE alone. Real AGA silicon
creates a reference where there is none. Do it second, with a rig already proven
against the A500.

Note `tt-a1200.toml` and `tt-020-noslow.toml` already exist and the probe disk
already handles the no-slow-RAM case (rows 0/1/9/13 store a sentinel).

Wrinkle worth knowing before committing: the A500 keyboard is on an **internal**
header, so the control MCU means opening the case. Big-box machines (A2000/3000/
4000) have an external 5-pin DIN and are the easiest keyboard targets;
A600/A1200 have an integrated membrane and are the hardest. Since phase 1 does
not need keyboard injection at all (section 5.3), this does not block the A500
choice.

## 4. Architecture

```
  host (agent + harness)
    |
    +-- USB serial ------> RS-232 level shift ---> Amiga DB25 serial
    |                                              (probe control + results)
    |
    +-- USB serial ------> control MCU ----------> keyboard header
    |                      (5V Arduino)            (KCLK, KDAT, /RESET)
    |                            |
    |                            +---------------> PSU relay (cold boot)
    |
    +-- capture ---------> RGBtoHDMI ------------> Amiga RGB out
    |
    +-- (later) audio in <- Amiga RCA out
    |
    +-- Gotek/HxC holds the probe-server boot image (written once)
```

Single host process (`hwrig`) owns all of these and exposes one interface
(section 8).

## 5. Components

### 5.1 Serial is the primary instrument

Not video. Almost every open question in section 1 resolves to a number the
guest measures about itself; those arrive over serial exact and
machine-readable, immune to every capture artifact. Video is for the probes
whose answer only exists as a raster.

`timing-test/test.asm:927` already implements the transmit half: poll `SERDATR`
bit 13 (TBE), write `SERDAT`. The receive half is new but trivial.

Electrical: the Amiga side is true RS-232 (1488/1489 level shifters), so use a
proper RS-232 adapter or add a MAX3232 -- do not hang a 3.3V TTL adapter off it.

Baud: Paula derives its rate from the chipset clock, so nominal rates are
slightly off the PC-standard values (roughly 3546895/(n+1) on PAL). Within
tolerance at 115200 but confirm empirically; the probe server should report the
divisor it used.

Throughput sanity check: a 4KB probe blob at 115200 8N1 is about 2.8s to upload.
Acceptable. If that becomes the bottleneck, raise the rate rather than
compressing.

### 5.2 The probe server (the core piece)

**One boot medium, written once.** Not a custom ROM (an EPROM burn per iteration
is a dead loop) and not a new ADF per probe (Gotek image swapping is a human in
the loop). Instead: a resident server that boots, takes over the machine, and
then accepts arbitrary 68k code over the serial port.

Iteration becomes `vasm probe.asm && upload && read numbers` -- seconds, no media
handling, no human.

The existing probes drop straight into this. They are already OS-free, already
take over the machine completely, and `boot.asm` already loads to a fixed
$30000 for the right reason (code-fetch timing must not depend on where the ROM
boot buffer happened to land).

Server responsibilities:

1. Boot, disable interrupts and DMA, establish a known machine state.
2. Identify itself: Agnus/Denise ID where readable, chip RAM size, CPU type,
   Kickstart version, PAL/NTSC. Emit on every connect (section 9).
3. Accept a blob: length, load address, CRC, then payload. Verify CRC before
   jumping.
4. Run it with a documented entry contract (a6 = $DFF000, known register state,
   return by RTS).
5. Stream back results as ASCII hex lines, framed so partial output is
   detectable.
6. Heartbeat between runs, so the host watchdog can tell "wedged" from "busy".
7. Return to the accept loop. A probe that returns cleanly must not require a
   reset.

**Develop it against Copperline first.** `[serial] mode = "tcp"` gives
bidirectional serial over a host TCP port (`docs/guide/configuration.md:630`),
so the entire server, the upload protocol, the framing and the host harness can
be written and debugged against the emulator, then deployed to real silicon
unchanged. This is the single biggest de-risking move available and it costs
nothing.

Recovery contract: a probe that hangs the machine is expected, not exceptional.
The host detects no-heartbeat, asserts reset, waits for the boot banner, and
continues the sweep. This must work unattended or the rig is only usable with a
human in the room.

### 5.3 Control MCU: keyboard and reset on one device

This replaces the "modify DoohicKEY firmware so the HDD LED input is a UART in"
idea. The reasoning that motivated it was right -- DoohicKEY has the reset line
-- but the indirection is unnecessary once an MCU is on the keyboard header
anyway.

A **5V Arduino** on the keyboard connector reaches everything needed on five
wires: KCLK, KDAT, /RESET, +5V, GND. From that one device:

- **Keyboard injection**, by speaking the Amiga keyboard protocol directly.
  Per the HRM: 8 bits, MSB first, data sent inverted, roughly 20us setup /
  20us clock low / 20us hold, then the Amiga acknowledges by pulling KDAT low
  for at least 75us; timeout 143ms triggers resync. Verify these against the
  HRM before coding -- they are from memory.
- **Warm reset**, either by driving /RESET directly or by the keyboard-initiated
  hard reset (hold KCLK low for >=500ms), which is what Ctrl-A-A does. The
  latter needs no extra wire.

**The MCU must be natively 5V.** KCLK, KDAT and /RESET are open-collector lines
held at +5V by pull-ups on the Amiga side, so they idle high at 5V. A 3.3V part
such as the RP2040 is not 5V tolerant and would be clamping 5V into its supply
rail through the input protection diodes even with the pin in hi-Z -- it cannot
be wired directly no matter how the firmware drives it.

Suitable parts:

- ATmega328P at 5V (Uno, Nano, Pro Mini 5V/16MHz) -- direct connection, USB
  serial via the on-board bridge.
- ATmega32U4 at 5V (Leonardo, Pro Micro **5V/16MHz** -- the 3.3V/8MHz variant
  exists and is the wrong one) if native USB is wanted, which also leaves USB
  HID available for later.

The protocol is 20us-scale with 143ms timeouts, so a 16MHz AVR bit-bangs it with
large margin. There is no timing argument for a faster part; my earlier PIO
justification was not a real requirement.

If a 3.3V MCU is ever preferred, all three lines need bidirectional level
shifting -- a BSS138-style FET shifter is the right topology here, since it is
designed for exactly this open-drain-with-pull-up case. That is three more parts
and three more failure modes on the critical recovery path, which is an argument
for just using the 5V part.

Two firmware points that matter:

- The two classes of line are driven differently, and conflating them is wrong
  in both directions. **KCLK and KDAT are actively driven push-pull** while the
  keyboard is transmitting, and only KDAT is released -- to hi-Z -- for the
  Amiga's acknowledge pulse; that is what a real keyboard MCU does. **/RESET is
  open-drain**: a wired-OR line that is only ever pulled low or released, never
  driven high. (An earlier draft of this plan said to drive everything
  open-drain. That is wrong for the keyboard lines; the maintainer's own
  `A500KBFirmware` is the reference and the rig firmware follows it.)
- The MCU must **bound the reset pulse in firmware** and auto-release, and come
  up with every line released. If the host crashes mid-command, or the MCU
  itself resets, the Amiga must not be left held in reset.

Phase 1 does not actually need the keyboard half at all: with a probe server on
the Gotek and a reset line, nothing types. Keyboard injection matters for
driving unmodified software (demos, games, Workbench) in later phases. Build the
reset half first.

### 5.4 Power control for cold boot

A relay or network-controlled outlet on the PSU, owned by the control MCU or the
host.

Warm and cold start are not equivalent and the difference is observable --
`issue17-boot-fleck-prefetch-scroll` is on file as a case where uninitialised
chip RAM contents show up on screen. Any probe touching power-on state needs a
true cold boot, and unattended sweeps need one anyway as the recovery of last
resort when reset alone does not clear a wedge.

Safety: these are 35-year-old machines and A500 PSUs have a reputation. Use a
known-good or modern replacement supply, fuse it, and do not run unattended
overnight sweeps without thinking about what happens if something cooks.

### 5.5 Video

The maintainer designed RGBtoHDMI, so this section is deliberately short: the
capture chain is a solved problem on their side and does not need specifying
here.

One thing worth preserving as a requirement rather than an implementation note:
**capture as close to the source as possible**. If the framebuffer can be pulled
off the Pi before HDMI, that removes the whole chroma-subsampling and rescaling
error class that a generic UVC capture stick would introduce. Whether that is
worth doing depends on how exact the comparison needs to be, which varies by
probe.

Reassurance in the other direction: the `ddfprobe-*` probes deliberately render
as coarse bars and bands, not fine detail, so band-boundary comparison survives
a lossy path. The video requirement is "which band did the edge land in", not
"is this pixel exactly $0F0". Sampling phase and 1 cck = 1 lores pixel is the
resolution that matters.

For non-RGBtoHDMI-friendly machines, any analogue capture that resolves band
boundaries reliably is sufficient.

### 5.6 Audio (phase 4)

Line-in from the RCA outputs. Near-zero marginal cost once the rig exists, and
there are open Paula questions that would use it (`paula-audio-fsm-pr134`,
`issue74-audxen-disable-deferred-to-word-boundary`,
`cd32-boot-tune-len0-mute`). `audprobe-en` currently renders the AUD0 interrupt
cadence as a raster strip precisely because there was no audio path; with one,
it could be measured directly.

## 6. Probe server protocol (sketch)

Line-oriented ASCII, so it can be driven by hand from a terminal when
debugging. Binary payload framed inside it.

```
  <- BANNER cl-probe 1 agnus=8372A denise=8362 cpu=68000 chip=512K slow=512K pal=1 ks=1.3
  -> ID
  <- (banner again)
  -> LOAD 30000 0A44 <crc32>
  -> (0x0A44 raw bytes)
  <- LOADOK
  -> RUN 30000
  <- BEGIN
  <- 0000047A
  <- 000004B2
  <- ...
  <- END 32
  <- READY
```

Requirements:

- CRC on upload, verified before any jump. A corrupted blob must fail loudly,
  not run.
- `END <count>` so truncated output is detectable rather than silently short.
- `READY` heartbeat while idle, so the watchdog can distinguish idle from
  wedged.
- Everything the emulator side needs to fake for testing must be expressible
  over the TCP serial bridge.

## 7. Host harness

A single `hwrig` process owning serial, MCU and capture, exposing:

- A CLI mirroring the existing tooling shape: `tools/hw-ref.sh` as a sibling of
  `tools/vamiga-ref.sh`, taking a probe name and emitting the same rows.
- A JSON/RPC interface, ideally **speaking the Copperline Control Protocol**
  subset -- `status`, `reset`, `screenshot`, `input`, plus a `probe.run`. Then
  `copperline-ctl --info` points at real silicon by changing an address, and
  there is nothing new for an agent to learn.

Deliverable that makes it all worthwhile: the cross-emulator table in
`timing-test/README.md` grows a **real hardware** column, and
`tests/probe_golden.rs` gains a documented provenance for each blessed render.

## 8. Measurement discipline

The rig is a measurement instrument and must be treated as one.

**Measured, not assumed: a wire-driven run is not bit-reproducible even on the
deterministic emulator.** Running `test.bin` through the probe server instead of
booting it directly moves 8 of its 32 rows by 1-3 ticks, and repeating the
upload moves a similar set again between otherwise identical runs, because the
beam, E-clock and refresh phase at the moment `RUN` is issued depend on host
scheduling during the upload. The server now parks the beam at the top of a
frame before handing over, which removes the frame-phase component; a residue
remains because polling resolution is a few colour clocks. This is a property of
the method, not a bug. It also means the golden values in
`timing-test/README.md`, which were measured from native boots, are not directly
comparable to wire-driven numbers -- the rig needs its own baseline.

**Real hardware is not deterministic the way Copperline is** either. DRAM
refresh phase at power-on, CIA startup phase, disk index position all vary per
boot, so expect the spread to be no smaller than what the emulator already
shows. Therefore:

- Probes must self-synchronise (wait for a specific VPOS/HPOS) before starting,
  as the existing ones already do.
- Every probe runs **N times** (start with N=20) and the harness reports the
  distribution -- min, max, mode, and the count of distinct values. Never a
  single number.
- A one-cck disagreement with the emulator from a single run is phase noise
  until proven otherwise. The threshold for "this is a real divergence" is a
  stable mode across cold boots.

**Tag every measurement with the silicon.** Agnus part number, Denise part
number, Paula, CPU, Kickstart revision, PAL/NTSC, RAM configuration. Without
this the rig characterises one particular 8372A and calls it "hardware". The
banner in section 6 exists for this reason; the harness should record it
alongside every result set and refuse to compare across differing banners
without saying so.

**Where the rig disagrees with both emulators, suspect the rig first.** Then the
probe. Then the emulator. Row 4 and row 7 are the canaries -- if a register move
or a bare `dbra` loop reads wrong, stop and fix the rig.

## 9. Phasing

| Phase | Contents | Unlocks |
|---|---|---|
| 0 **(done)** | Probe server developed against Copperline over `[serial] mode = "tcp"` | Whole software stack validated with no hardware |
| 1 | A500 + RS-232 + MCU reset line + Gotek + `hwrig` CLI | Every numeric timing probe on demand; rig self-validated against vAmiga/FS-UAE |
| 2 | Video capture wired in; `probe_golden` comparison | DDF/DIW/sprite band maps against real silicon |
| 3 | A1200 rig; keyboard injection | The AGA backlog, which has no independent reference today |
| 4 | Audio capture; logic analyser on Agnus/Denise | Paula cadence; chip-bus slot arbitration |

Phase 0 and 1 carry most of the value. Do not let the video work gate them.

## 10. Open questions

1. **Confirm the A500-first ordering.** The argument is self-validation
   (section 3). If the AGA backlog is the more pressing pain, phase 3 could
   move up -- at the cost of commissioning the rig against a target where a
   disagreement cannot be attributed.
2. **Gotek vs real floppies vs a second boot path.** Once the probe server is
   resident, the medium is loaded once per boot, so a Gotek is sufficient. But
   floppy-timing probes will eventually want a real drive with real disks.
3. **Does the probe server want a fast-RAM variant?** Code-fetch timing depends
   on where code runs (rows 13/14 exist for this). The server should probably
   be able to place a probe in chip, slow or fast RAM on request.
4. **How far to take CCP compatibility** -- full subset, or just enough that an
   agent can drive it with the same mental model.
5. **Do the 68060 rows (28-30) matter enough** to justify an accelerated big-box
   machine as a later phase? `timing-test/README.md:105` asks for exactly that
   calibration.
