# cd32-probe: real-CD32 measurements from a burned CD-R

A tiny AmigaDOS program spliced into a copy of the Fightin' Spirit disc
image in place of `SYS:FSCD`, so the real machine boots with the game's
exact allocation history (same SetPatch, same CDInit, same show) and then
renders 30 measurement rows on screen as hex -- photograph the screen,
timing-test style. It never exits, so the startup-sequence's `SYS:Reset`
never runs and the display stays up.

It answers the three open calibration questions from the Fightin' Spirit
investigation (docs/internals/cpu.md, docs/internals/peripherals.md):

- does the game's uninitialized `OpenLibrary("freeanim.library")` version
  pass on real hardware, and what does the full show teardown release?
- what do real CD locates cost (Akiko seek model calibration)?
- what do unmapped/ROM/chip reads and writes cost per 16-bit bus cycle
  (slow-external class calibration), measured against the CIA E-clock?

## Build

```sh
./build.sh          # /opt/amiga m68k-amigaos-gcc; emits PROBE
python3 make-images.py "<path>/Fightin' Spirit ... (Track 01).bin" <outdir>
```

`make-images.py` emits into `<outdir>`:

| file | what | burn with |
|---|---|---|
| `fs-game-unpatched.iso` | the game's data track, untouched | `drutil` (data CD) |
| `cd32-probe.iso` | probe + 440 MB zero pad (full seek range) | `drutil` (data CD) |
| `cd32-probe-full.toc` + `cd32-probe-track01.iso` | probe track 1 + the game's 33 audio tracks: the burned TOC matches the original disc, so the boot runs the authentic show-vs-boot race | `cdrdao` |
| `cd32-probe-full.cue` (+ `-raw.bin`) | emulator-verification cue for the full variant | not burned |

Burn slow (4x-8x) on decent CD-R media; CD32 drives are usually fine with
CD-R but dislike fast burns.

```sh
drutil burn -noverify cd32-probe.iso            # data-only variant
brew install cdrdao                              # for the full variant
cd <outdir> && cdrdao write cd32-probe-full.toc  # full-fidelity variant
cdrdao write "<path>/Fightin' Spirit (Europe) (En,De,It).cue"  # the game itself
```

Which disc answers what:

- **full variant**: the layout/freeanim rows (00-13). Its TOC matches the
  game's, so CLITASK is the address the real game's buggy version check
  sees. The far-read rows time out by design (audio space).
- **data-only padded variant**: the CD locate rows (15-23) across the
  full stroke, and the bus rows. Its small TOC makes the CD32 boot early
  (before the show), so its layout rows describe the early-boot path,
  not the game's -- expected, not a defect.
- **unpatched game burn**: does the real CD32 cold-boot Fightin' Spirit
  at all, and the stock boot timeline (film it with a phone, with sound).

## Row map (all values 8-digit hex)

| row | label | meaning | Copperline (full variant) |
|---|---|---|---|
| 00 | CLITASK | FindTask(NULL): the boot CLI process address. Its low word decides the game's buggy freeanim version check (signed <= 40 passes) | 00025700 |
| 01 | LARGEST0 | AvailMem(CHIP\|LARGEST) at entry | 0011FE50 |
| 02 | FREE0 | AvailMem(CHIP) at entry | 0019A4C8 |
| 03 | ANIMPORT | FindPort("Startup Animation") | 001FD320 |
| 04 | FANFARE0 | FindTask("Fanfare") at entry | 00187020 |
| 05 | BUGOPEN | OpenLibrary(freeanim, version = CLITASK): the game's exact call. Nonzero here on real hardware = the game's bug passes there | 00000000 |
| 06 | BUGCLOSE | E-ticks its CloseLibrary blocked (teardown wait) | 0 |
| 07 | OLDOPEN | OldOpenLibrary(freeanim), only tried if BUGOPEN failed | 0000FD08 |
| 08 | OLDCLOSE | E-ticks its CloseLibrary blocked | ~00022CBC (2.0 s) |
| 09 | LARGEST1 | largest chip chunk after teardown | 001CB0B0 |
| 10 | FREE1 | chip free after teardown | 001CF450 |
| 11 | FANFARE1 | FindTask("Fanfare") after (0 = task exited) | 00000000 |
| 12 | EFREQ | ReadEClock frequency (PAL 709379) | 000AD303 |
| 13 | ENTRYTIM | E-ticks at probe entry since power-on (/EFREQ = seconds) | ~12.1 s |
| 14 | SKSW100 | CD_SEEK +100: driver-internal, no drive motion | 000001B3 |
| 15 | RD500 | min E-ticks, 1-sector read at +500 after repositioning to LBA 1000 | ~139 ms |
| 16 | RD1K | .. +1000 | ~139 ms |
| 17 | RD10K | .. +10000 (data only on the padded disc; times out on full) | ~158 ms |
| 18 | RD100K | .. +100000 | ~451 ms |
| 19 | RD200K | .. +200000 | ~542 ms |
| 20 | PL10K | CD_PLAYLSN 1 s of audio at LBA 11000, DoIO duration: value - 1 s = locate cost (audio discs only; errors on the padded disc) | ~1.01 s |
| 21 | PL100K | .. at 101000 | ~1.02 s |
| 22 | PL200K | .. at 201000 | ~1.02 s |
| 23 | RDRATE64 | E-ticks for 64 sequential sectors at LBA 1201 (as CDInit configured: 2x = 150/s) | 00049BD7 (426 ms) |
| 24 | URD | E-ticks, 65536x `CMP.W (A4)+,D2 / DBF` over $A80000 (unmapped FMV window) | 0000B363 (14.0 clk/iter) |
| 25 | ROMRD | .. over $F80000 (Kickstart ROM) | 000099CA (12.0 clk) |
| 26 | CHIPRD | .. over chip RAM (display on) | 0000CDEA (16.1 clk) |
| 27 | UWR | 65536x `MOVE.W D2,(A4)+` over $A80000 (unmapped write -- currently billed free in Copperline) | 00008030 (10.0 clk) |
| 28 | CHIPWR | .. over chip RAM | 0000670D (8.1 clk) |
| 29 | ULRD | 32768x `MOVE.L (A4)+,D1` over $A80000 | 00008CFx |

Error markers in CD rows: `EExxxxxx` = io_Error xx, `DDxxxxxx` = device
open failed, `CCCCCCCC` = 6-8 s watchdog timeout (the op was aborted; a
burned disc can never hang the probe).

Clocks per iteration = value / EFREQ * 14_180_000 / 65536 (32768 for
row 29). Loops run from chip RAM with the I-cache on and interrupts
disabled; CD rows run with interrupts enabled, min of 3 reps, with a
1-sector read back to LBA 1000 between reps (CD_SEEK does not move the
real head).

## Real-hardware results (2026-08-28, full-fidelity disc, PAL CD32)

Transcribed from the first real run (Sony CRT photo). This column drove
the calibration commits in src/cpu.rs, src/bus.rs and src/akiko.rs:

| row | real CD32 | derived | Copperline after calibration |
|---|---|---|---|
| 00 CLITASK | 001F3170 | show fully torn down before startup-sequence; address byte-identical to Copperline's clean-path CLI | 00025700 (show still resident at entry) |
| 01 LARGEST0 | 001B9CE0 | 1.73 MB largest free AT ENTRY | 0011FE50 (1.12 MB) |
| 04 FANFARE0 | 00000000 | Fanfare already exited | 00187020 (alive) |
| 05 BUGOPEN | 00000000 | the game's uninitialized-version call FAILS on real hardware too -- and does not matter there | 00000000 |
| 07/08 OLDOPEN/CLOSE | 0000FDC0 / 0015D0D0 | correct teardown call blocks 2.02 s even with nothing left to free | 0000FD08 / ~2.1 s |
| 13 ENTRYTIM | 009AE1B5 | startup-sequence reaches FSCD at 14.31 s | 12.07 s |
| 15 RD500 | 0002BD6D | 253 ms | 284 ms |
| 16 RD1K | 00034284 | 301 ms | 333 ms |
| 20-22 PLxxx | 184911/1D3090/1EF0BA | 2.24/2.70/2.86 s incl. 1 s of audio: far locates flatten ~1.4-1.9 s | play path does not yet pay locates |
| 23 RDRATE64 | 00049E4C | 150.0 sectors/s at 2x | 150.1/s |
| 24 URD | 000099D7 | 12.01 clk/iter == ROM pace | 12.00 |
| 25 ROMRD | 000099D4 | 12.0 | 12.0 |
| 26 CHIPRD | 0000CE0D | 16.09 | 16.08 |
| 27 UWR | 0000669F | 8.01 (posted, == chip write) | 8.0 |
| 28 CHIPWR | 00006715 | 8.05 | 8.05 |
| 29 ULRD | 00006038 | 15.03 | 13.9 |

The cold-boot video (IMG_1814.mov, 2026-08-28) closed the loop: power-on
at ~1.0 s video time, Kickstart grey until ~12.5 s, the boot screen (no
fly-in show, no chime -- the show never runs with a bootable disc in the
drive), game intro from ~23.5 s, and the unpatched game running on real
hardware. That decomposition produced the spin-up gate and the
dump-exclusive command hold in src/akiko.rs; after them the emulated
full-fidelity probe reads ENTRYTIM 14.36 s vs the real 14.31 s, with the
layout rows (CLITASK $1F317x, FANFARE0 0, FREE0 within ~200 bytes)
matching the real column above.

The padded data-only disc's run (second CRT photo, 2026-08-28) completed
the locate curve: RD500 254 ms, RD1K 433 ms (301 ms on the full disc at
the same distance -- real mechanism spread), RD10K 579 ms, RD100K
1.023 s, RD200K 1.365 s; the PL rows returned io_Error 36 (the firmware
refuses to play data sectors, now modelled), and the bus rows repeated
the first disc's values within 1-2 E-ticks. The seek model is a
piecewise-linear fit through those anchors; the emulated data-disc run
reads ENTRYTIM 13.18 s vs the real 13.27 s, and RD10K/100K/200K within
5% of the measured column. Nothing further is outstanding from real
hardware.

## Interpreting against Copperline

Every row above has the emulator's value on the same disc image
(`--insert-cd-after 0 probe/cd32-probe-full.cue` / `cd32-probe.iso`,
`--screenshot-after 100`). Real-hardware divergence is calibration
signal:

- BUGOPEN nonzero on real HW = the real boot layout passes the game's
  version check; CLITASK tells us the address to match.
- RD/PL deltas vs the emulator calibrate `akiko.rs` seek_delay.
- URD/UWR/ULRD vs ROMRD/CHIPRD calibrate the slow-external billing in
  `cpu.rs` (and settle the unbilled unmapped-write class).
