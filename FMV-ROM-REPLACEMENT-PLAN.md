# Open-source CD32 FMV ROM replacement: project plan

Status: GAMES MVP AND VIDEO CD PLAYER/AUTOBOOT IMPLEMENTED. Written 2026-08-26 after the investigation that diagnosed why
the original `cd32fmv.rom` breaks CD boot under the bundled AROS ROM (see
"Ground truth" below). Work started 2026-08-30 after AROS pull request 1089
provided the Cannon Fodder `cd32mpeg.device` subset and the required Mode-2
continuous-CD path.

The implemented `fmv-rom/` milestone is now a standalone, open 256 KiB
cartridge ROM. It contains the Cannon Fodder-compatible `cd32mpeg.device`, a
clean-room `videocd.library`, a Video CD player/autoboot strap, a real DiagArea
resident entry, the chip bring-up path, and a standard Mode-2 `CD_READ`
streamer for Kickstart 3.1. AROS PR 1089 supplies its matching system-ROM
driver and intentionally skips cartridge diagnostics; both paths use the same
valid but empty CL450 firmware container because Copperline does not execute
uploaded CL450 IMEM/TMEM. No Commodore code or microcode is copied.

The milestone was validated on 2026-08-30 under both CD32 Kickstart 3.1 and an
AROS ROM built from PR 1089 through chronological-CDXL commit `ebfc7d9`, plus
the local Cannon Fodder CD. The guest initialized both decoder paths and Copperline
presented the 352x288 live-action introduction with non-silent stereo audio.
A deterministic AROS 60-second run decoded more than 1,000 frames; the
Kickstart resident decoded 735 before the intro completed and gameplay began.
Neither run reset a malformed MPEG stream, both produced full-colour video and
non-silent stereo. The AROS patch is required because a re-armed
high PBX slot can overtake an older low-slot sector; draining each snapshot by
raw-sector MSF restores chronological CDXL delivery.

## Goal

Build an open-source, freely redistributable replacement for the Commodore
CD32 Full Motion Video cartridge ROM (`cd32fmv.rom`, exactly 256 KiB), so that:

1. FMV-aware CD32 titles (e.g. Cannon Fodder's MPEG intro) play their video
   under Copperline without the proprietary ROM image.
2. The module works under BOTH the real CD32 Kickstart 3.1 (rev 40.60) and the
   bundled AROS ROM.
3. Copperline bundles it in `assets/fmv/` and fits it by default on CD32,
   making FMV a zero-asset emulator feature (`fmv_rom = ""` opts out).

### Non-goals (initially)

- Real-hardware support. The C-Cube CL450 needs proprietary microcode uploaded
  at init; an open ROM cannot ship it. Copperline's emulation stores IMEM/TMEM
  uploads but never executes them (`cl_imem`/`cl_tmem` in `src/cd32_fmv.rs`
  are write-only stores; MPEG is decoded host-side via plmpeg/Symphonia), so a
  dummy upload works under emulation. Real-cartridge support would need a
  "bring your own microcode" side-load (WHDLoad-kickstart style) and is a
  later, optional phase.
- Replacing `cd.device`. The host system's driver (Kickstart's or AROS's) is
  used for all CD access. This is a deliberate architectural difference from
  the original ROM and is what makes the replacement AROS-safe (see below).

## Ground truth from the 2026-08-25 investigation

All of this was verified by disassembly of the original ROM, AROS source
reading, serial-debug capture, and live CCP memory walks. Memory file:
`fmv-rom-displaces-aros-cd-device.md` in the Claude project memory.

### What the original 256 KiB ROM contains

Romtag inventory (offsets are within the ROM file; the board ROM maps at the
board base, normally $200000):

| ROM offset | Resident            | Ver | Pri | rt_Flags | Init      |
|-----------:|---------------------|----:|----:|----------|-----------|
| $00282     | cd.device           | 40  | 8   | $01 COLDSTART | $2002D8 |
| $03604     | cd32mpeg.device     | 105 | 5   | $80 AUTOINIT  | $20364A |
| $0BCD8     | cdstrap             | 40  | -58 | $01 COLDSTART | $20BD60 |
| $0E428     | videocd.library     | 40  | 0   | $81 AUTOINIT+COLDSTART | $20E46C |
| $0FD4C     | mpegplayer.library  | 40  | 0   | $80 AUTOINIT  | $20FD98 |

Plus: the DiagArea + loader (below), the CL450 microcode data blob, and the
module code itself. `mpegplayer.library` is the biggest module (~46 KiB) and
is the API that titles call.

### The diag/boot mechanism, fully decoded

- Autoconfig identity comes from the BOARD (in Copperline:
  `BoardSpec::cd32_fmv` in `src/zorro.rs` -- Zorro II, manufacturer 514,
  product $6A, serial $0028001E, 1 MiB, ERTF_DIAGVALID with
  er_InitDiagVec = $0080, ERFF_MEMSPACE, no-shutup).
- DiagArea at ROM offset $80. Header bytes (verified):
  `90 00 0038 001E 001A 000E` = da_Config $90 (DAC_WORDWIDE | DAC_CONFIGTIME),
  da_Size $38, da_DiagPoint $1E, da_BootPoint $1A, da_Name $0E
  ("config_mpeg"). BootPoint is `moveq #0,d0; rts` (unused).
- DiagPoint stub (called with A0=board base, A2=DiagArea copy in RAM,
  A3=ConfigDev, A5=ExpansionBase, A6=ExecBase; both Kickstart and AROS use
  this convention -- see AROS `arch/m68k-amiga/diag/diag.c`):
  1. `cmpa.l #$200000,a0` -- FAILS unless the board configured at exactly
     $200000 (so the board must stay first on the Zorro chain; Copperline
     prepends it, `emulator.rs` "must be first in the chain").
  2. Writes $1000 to board+$40000 (IO bank; bit meaning unknown, presumed
     board enable -- Copperline just latches `io_reg`).
  3. Jumps into the board ROM at base+$B8 (executes from ROM, not the copy).
- The code at +$B8 merges the ROM's romtags into `SysBase->ResModules`
  (ExecBase offset 300):
  - Scans ROM $2001BE..$240000 for RTC_MATCHWORD ($4AFC) with valid
    rt_MatchTag self-pointer, following rt_EndSkip.
  - For each found tag, walks ResModules (an array of Resident pointers where
    an entry with bit 31 set is a link to another array segment, 0 ends it):
    - Same rt_Name found in list: REPLACE the list entry in place when
      ROM version > list version, or versions equal and ROM pri >= list pri;
      otherwise skip the ROM tag entirely (found-flag also suppresses
      insertion).
    - Name not found: AllocMem(12, MEMF_PUBLIC|MEMF_CLEAR|MEMF_REVERSE) a
      3-slot node {new tag, displaced entry, link-back | bit31}, and patch
      the insertion-point slot to `node | bit31`. This is the classic AOS
      diag-ROM splice; an in-progress InitCode iteration follows it
      correctly (AROS `rom/exec/initcode.c` handles RESLIST_NEXT = bit 31
      on m68k).
  - Calls SumKickData (exec LVO -612), stores the result in
    ExecBase->KickCheckSum (offset 554), and returns 0. The subsequent
    "->failed" in AROS's debug log is BY DESIGN: the DiagArea copy is
    discarded because all the work already happened via the ResModules
    merge. Kickstart behaves identically.

### Why the original ROM breaks the AROS ROM (and the replacement must not)

AROS's CD32 `cd.device` (AROS `arch/m68k-amiga/devs/cd/cd.conf`) is version
40.0, residentpri 5. The cartridge's `cd.device` is version 40, pri 8 -> same
version, higher pri -> the merge REPLACES AROS's driver with Commodore's
KS-era one. Commodore's driver initialises fine under AROS (verified in the
live DeviceList) but never creates AROS's CDFS boot node
(`cdRegisterVolume` -> `AddBootNode`), and the also-spliced Commodore
`cdstrap` cannot boot a CD in an AROS world -> dosboot sits on the
insert-media screen. Separate fix tracked for AROS upstream: bump AROS's
cd.device resident version above 40. The replacement ROM ships no
`cd.device`. Its version 41 `cdstrap` is reached only on Kickstart because
AROS deliberately skips cartridge diagnostics; it claims Video CDs and chains
the displaced system strap for every other disc.

## Hardware contract (what the guest driver must program)

Primary reference: `src/cd32_fmv.rs` is an executable specification of the
exact register subset that must be driven; `docs/internals/peripherals.md`
section "CD32 Full Motion Video module" is the prose version. Public
datasheets exist for both chips: "C-Cube CL450 MPEG Video Decoder User's
Manual" and the LSI Logic L64111 datasheet.

Board window (1 MiB Zorro II, normally $200000):

| Offset    | Bank |
|-----------|------|
| +$000000  | 256 KiB ROM |
| +$040000  | board status/control word (IO bank) |
| +$050000  | L64111 MPEG audio decoder registers |
| +$060000  | CL450 bitstream/data port (CMEM FIFO) |
| +$070000  | CL450 host registers |
| +$080000  | 512 KiB module RAM |

Key facts (all mirrored in `cd32_fmv.rs` constants):

- IO bank read is active-low IRQ status: bit 15 = CL450 IRQ pending (low),
  bit 14 = L64111 IRQ pending (low), bit 11 = CL450 FIFO status. IO writes
  latch a control word: bit 14 enables the CL450 video overlay
  (IO_CL450_VIDEO), bit 9 is L64111 mute. The original diag writes $1000 at
  config time.
- The board asserts INT2 (PORTS) while either chip has an unmasked pending
  interrupt (`int2_line`). Drivers hang an INT2 server on it and must read
  chip-side status to dismiss.
- CL450: command mailbox is HMEM words written via HOST registers, kicked by
  HOST_NEWCMD ($56). Command set the emulator implements: SET_BLANK $030F,
  SET_BORDER $0407, SET_COLOR_MODE $0111, SET_INTERRUPT_MASK $0104,
  SET_THRESHOLD $0103, SET_VIDEO_FORMAT $0105, SET_WINDOW $0406,
  DISPLAY_STILL $000C, PAUSE $000E, PLAY $000D, SCAN $000A, SINGLE_STEP
  $000B, SLOW_MOTION $0109, ACCESS_SCR $8312, FLUSH_BITSTREAM $8102,
  INQUIRE_BUFFER_FULLNESS $8001, NEW_PACKET $0408, RESET $8000. Interrupt
  causes: RDY (bit 10), PIC_D (bit 6), SEQ_V (bit 3), UND (bit 8); status is
  surfaced through HMEM word $0A. Sequence/picture geometry is readable from
  DRAM registers (H_SIZE $12, V_SIZE $13, PICTURE_RATE $14, TIME_CODE
  $17/$18) via the HOST_RADDR/HOST_RDATA window. Bitstream data is fed as
  NEW_PACKET commands + word writes to the +$060000 data port.
  Microcode upload path (CPU_IADDR/CPU_IMEM, CPU_TADDR/CPU_TMEM, then
  CPU_CONTROL bit 0 to start the internal CPU) must be exercised for
  authenticity, but content is ignored by the emulator: upload zeros or a
  tiny stub and document it.
- L64111: 32 word registers at +$050000 (reg index = (offset>>1) & 31):
  data port, 3 control regs, 2 int status regs (read-to-clear) + masks,
  params, presentation-time regs, CB status/read/write. Audio output is
  resampled onto the Paula mixer clock host-side; the driver configures
  stream selection and reads/acknowledges interrupts.
- Video path: the cartridge keys its analogue output over the native RGB
  where the Amiga picture is at the dark clamp level -- there is NO
  Denise/Lisa genlock programming. The replacement therefore does not touch
  chipset registers for compositing; it only needs the CL450 window/border/
  blank commands and the IO video-enable bit. The CL450's 704-pixel line maps
  to the TV aperture (`docs/internals/peripherals.md`).
- CD streaming: sector delivery is via the host `cd.device` (Akiko
  underneath). FMV titles stream MODE2/2336 tracks. Critical boundary
  behavior already modeled emulator-side: READ DATA's end MSF is exclusive
  and the PBX path retains one final position-bearing frame at the boundary;
  the streaming driver uses the on-disc MSF to notice when the next request
  falls outside the current stream, stop, and reseek. The replacement's
  streamer must implement the same stop-and-reseek discipline.

## Architecture of the replacement

Modules to build (resident names MUST match the original where titles look
them up by name; the first two are the completed games MVP):

1. `config_mpeg` DiagArea + loader. Insertion-only ResModules splice (no
   same-name replacement logic at all), same calling convention and node
   layout as decoded above, SumKickData + return 0. Must tolerate any board
   base (drop the $200000 assumption -- use A0/ConfigDev like a well-behaved
   diag ROM; Copperline will still put it at $200000).
2. `cd32mpeg.device` -- exec device wrapping the CL450 + L64111: init/reset,
   dummy microcode upload, NEW_PACKET bitstream feeding, play/pause/stop/
   still, INT2 server, audio config + unmute, video enable/window.
   Version 41 (must exceed the original's numbers is NOT required since the
   two ROMs never coexist, but 41+ follows AROS convention and wins any
   accidental comparison).
3. Future compatibility: `mpegplayer.library` -- the higher-level API on top
   of `cd32mpeg.device` + host `cd.device` streaming. Cannon Fodder opens the
   device directly, so this is not part of the completed games MVP.
4. `videocd.library` + a movie-disc boot/player UI and a `cdstrap` equivalent
   for autobooting movie discs on a bare CD32. Implemented in Phase 6.

Explicitly NOT shipped: `cd.device`.

Image layout: 256 KiB exactly (Copperline validates the size --
`config/validate.rs`; `fmv_rom` is CD32-profile-only). DiagArea at $80 to
match the board's er_InitDiagVec. Keep a version/BUILD id string near the
top. Pad with $FF.

Toolchain (all already installed on the dev machine):

- `/opt/amiga/bin/m68k-amigaos-gcc` (bebbo gcc 6.5.0b) + vasm
  (`vasmm68k_mot`) + `m68k-amigaos-objdump` for verification.
- `ira` (in /opt/amiga/bin) -- Amiga reassembler, ideal for Phase 0
  disassembly of the original modules.
- `fd2pragma`/`fd2sfd` for generating headers from the recovered .fd files.
- Known toolchain quirks: see memory `opt-amiga-toolchain-quirks.md` and the
  zz9k project notes (`-fcommon`, ENV: gotchas).

Suggested location: new top-level `fmv-rom/` directory in the Copperline
repo, structured like `timing-test/` (guest asm/C + Makefile + committed
build product is NOT appropriate here though -- the ROM is a build artifact;
commit sources only, build in CI). Split to a standalone CopperlineHQ repo
once it stabilises.

Licensing discipline: clean-room. Disassembly of the original ROM is for
interface discovery only (LVOs, structures, sequencing, register writes).
Keep an interface-notes document (`fmv-rom/NOTES-api.md`) recording observed
facts separately from any implementation. Never copy code or the CL450
microcode. The original ROM stays a local asset (`~/Amiga/cd32fmv.rom`),
never committed.

## Phases

### Phase 0 -- API recovery and tracing -- COMPLETE FOR CANNON FODDER

Deliverable: `fmv-rom/NOTES-api.md` with (a) the mpegplayer.library and
cd32mpeg.device LVO tables and structures actually used by titles, (b) per-
title call sequences with arguments, (c) how each title DETECTS the module
(OpenLibrary vs FindConfigDev(514,$6A) vs OpenDevice), (d) the register-level
init/play sequences the original driver performs against both chips.

How:

- Disassemble the original modules with `ira` (romtag offsets in the table
  above give exact module boundaries via rt_EndSkip).
- Trace real titles against the ORIGINAL ROM under Copperline. This is the
  decisive advantage: runs are deterministic, so a trace is a reproducible
  artifact. Recipe:
  - Machine: CD32 KS 3.1 + `fmv_rom` (configs below). Titles in
    `~/Amiga/fmv/`: Cannon Fodder (game using FMV intro; known-good at 45s,
    see baseline), Video Creator (FMV-enhanced app), Philips Media Retail
    Sampler '95 (VideoCD-style content for Phase 6).
  - Use CCP breakpoints on the original ROM entry points (init vectors in
    the romtag table) and `COPPERLINE_DBG_*` traces to log LVO calls +
    register args. `--save-state-after` just before the FMV sequence to
    iterate cheaply. NOTE from prior sessions: CCP stepping and DBG_BREAK
    perturb the emulated timeline; DBG_WATCH and pure traces are
    transparent. CCP `mem.read` does NOT dispatch into Zorro device windows
    (board space reads $FF) -- read board-resident structures via traces or
    guest-side probes instead.
  - Also capture which `cd.device` commands the original mpegplayer issues
    (breakpoints on the cd.device BeginIO of the ROM's own driver), since
    the replacement will issue the equivalents against the HOST driver.
- Answer explicitly: does any known title program the CL450/L64111 registers
  directly, bypassing the libraries? (If yes, those titles already work with
  any ROM present and constrain nothing.)

### Phase 1 -- ROM scaffold + diag loader -- COMPLETE

Deliverable: a reproducible 256 KiB image with a real DiagArea entry and the
autoinit `cd32mpeg.device` resident. The resident's match/end-skip pointers
are relocated into the configured board ROM and its BSS is placed in reserved
module RAM.

Acceptance:

- Under AROS PR 1089 + replacement + Cannon Fodder: the game boots and runs
  with AROS's system-ROM `cd32mpeg.device`. The PR deliberately skips the
  cartridge diagnostic so an original Commodore cartridge cannot replace
  AROS's `cd.device`; the replacement still supplies the open firmware
  container read by the AROS driver.
- Under KS 3.1 + replacement: the expansion diagnostic installs
  `cd32mpeg.device` 41.0 from the cartridge and Cannon Fodder opens it.
- Regression: both no-FMV baselines unchanged.

### Phase 2 -- cd32mpeg.device: chip bring-up -- COMPLETE

Deliverable: the device opens, resets both chips, uploads dummy microcode,
and can play a raw MPEG-1 stream fed from a file/RAM buffer (no CD streaming
yet): video visible via the overlay, audio audible via the L64111 path.

How: develop the playback engine as a plain Amiga executable first and run
it with `--run prog` (stages a boot volume, mounts the build directory live,
composes with all headless flags + `--gdb`/CCP loadseg breaks). Fold it into
the device once it works. Test stream: extract one MPEG stream from the
Cannon Fodder track 2 (MODE2/2336) or use any small MPEG-1 system stream.

Acceptance (all headless, deterministic):

- `--screenshot-after` captures showing decoded frames composited in the TV
  aperture with border/window controls honoured.
- `--audio-wav` capture with MPEG audio present (compare against the same
  content played via the original ROM for A/V sanity, not bit-exactness).
- INT2 service loop survives buffer underrun (UND) and end-of-stream.
- `cargo test cd32_fmv` (emulator-side suite) still green -- if the
  replacement needs chip behavior the model lacks, extend `cd32_fmv.rs`
  WITH a regression test, datasheet reference, and hardware-first wording;
  never special-case the replacement ROM.

### Phase 3 -- title-facing streaming -- COMPLETE FOR CANNON FODDER

The recovered evidence showed Cannon Fodder talks directly to
`cd32mpeg.device`; no `mpegplayer.library` LVO is needed for this games MVP.
The device streams from the host `cd.device`, and Cannon Fodder plays its FMV
intro under KS 3.1 with the replacement ROM. Broader `mpegplayer.library`
compatibility belongs with the optional Video CD/player work if a traced title
requires it.

Notes:

- Streaming discipline: continuous READ DATA, watch the on-disc MSF, detect
  out-of-stream requests, stop + reseek (see peripherals.md paragraph on the
  boundary PBX frame -- the emulator keeps the final position frame exactly
  so a driver written this way works).
- A/V sync: characterize in Phase 0 what the original does with SCR
  (ACCESS_SCR) and the L64111 presentation-time registers; start with
  video-master + audio resample-follow, refine against the original ROM's
  output timing.
- Under AROS: gate on the separate AROS work below; KS 3.1 is the primary
  target for this phase.

Acceptance: deterministic frame-dump comparison of the intro under
original-ROM vs replacement-ROM runs (`--dump-frames`, same timestamps;
expect similar content, not bit-identical), plus the full title matrix in
Phase 4.

### Phase 4 -- title matrix + regressions -- MVP COMPLETE, ONGOING

- Matrix: every FMV-aware title obtainable (Cannon Fodder, Video Creator,
  Liberation-era titles, the Philips sampler), each x {KS3.1, AROS} x
  {replacement ROM}, headless screenshots at known-good timestamps.
- The ignored, asset-gated `tests/cd32_fmv_aros.rs` regressions boot Cannon
  Fodder with the replacement ROM under both Kickstart and AROS, require
  sustained clean MPEG frames, and check full-colour output plus non-silent
  stereo capture.
- Keep the three baseline recipes from the 2026-08-25 investigation as
  fixed regression points (below).

### Phase 5 -- AROS-side enablement -- DRAFT PR VALIDATED

1. Upstream AROS: bump `arch/m68k-amiga/devs/cd/cd.conf` version above 40
   (fixes the ORIGINAL ROM's collision too; independent of this project but
   shares the test rig). Local AROS tree: `~/Programming/Git/AmigaMe/aros-build/AROS`;
   prior upstreaming flow in memory files (AROS PRs #1018, #1034, #1051, #1063).
2. Draft PR 1089 supplies the required Mode-2/`CD_READXL` path. Copperline's
   fix for the original numeric PBX drain is incorporated in the PR as commit
   `ebfc7d9`; it orders each snapshot by raw-sector MSF so normal DMA/task
   interleaving cannot swap adjacent sectors. Run the rest of the Phase 4
   matrix under AROS as more titles become available.

### Phase 6 -- VideoCD player + movie-disc boot -- COMPLETE

The clean-room resident `videocd.library` preserves the observed six-vector
ABI, classifies a Video CD through `cd.device`, parses INFO.VCD/ENTRIES.VCD and
the TOC, and returns allocated disc/track tag lists. A guest probe under CD32
Kickstart validates the Philips sampler's two tracks and 45 entry points.

The version 41 `cdstrap` replaces the extended-ROM version 40 entry, claims
only Video CDs, and chains the displaced init routine for every other disc. A
cold-booted Video CD starts a 320x256 track menu. Up/down select, Red submits
an asynchronous `MPEGCMD_PLAYLSN`, and Blue aborts playback and returns to the
menu. The Philips sampler regression decodes its first 352x240 stream and
verifies both the playing frame and restored menu; the Cannon Fodder
Kickstart regression proves the non-Video-CD fallback remains intact.

Real-hardware support still requires a separate user-supplied CL450 microcode
design because the bundled empty container is intentionally emulator-only.

### Phase 7 -- Copperline bundling -- COMPLETE

Ship the built ROM in `assets/fmv/` with a romsearch-style lookup, make
`fmv_rom` default to it on CD32 profiles (explicit path still wins), update
`docs/guide/configuration.md`, `docs/internals/peripherals.md`, launcher ROM
page, and the Flatpak/AppImage packaging lists.

## Baseline regression recipes (from the 2026-08-25 session)

Configs (adjust paths; `--factory` pins away from any saved default config;
`COPPERLINE_AROS_DIR` is needed whenever cwd is not the repo root):

```sh
# A: AROS + FMV ROM + Cannon Fodder. With the ORIGINAL ROM this sticks on
# the AROS insert-media screen (the diagnosed collision). With the
# REPLACEMENT ROM the game must boot.
COPPERLINE_AROS_DIR=<repo>/assets/aros ./target/release/copperline --factory \
  --config aros-fmv.toml --noaudio --serial stdout \
  --screenshot-after 90 /tmp/a.png

# B: AROS, no FMV: game boots (control, must never regress).
# C: KS3.1 + FMV: FMV intro visibly playing by 45s (original-ROM reference;
#    the replacement should reach the same state).
```

`aros-fmv.toml` shape (top-level `fmv_rom`, `[machine] profile = "CD32"`,
`[chipset] revision = "AGA"`, `[cd] image = <cue>`); KS variant adds `rom` and
`extended_rom`. Reference assets on the dev machine: `~/Amiga/cd32fmv.rom`,
CD32 KS 3.1 + extended ROM in `~/Amiga/`, titles in `~/Amiga/fmv/`.

AROS serial debug (expansion/diag/romboot prints) arrives on the default
`--serial stdout`; the expected diag lines for a working board are
`Found board ...: mfg=514 prod=106 ... diag=00000080`, `da_Config=90`,
`Call boot rom ... ->failed` (by design), then `romtaginit done`.

## Risks and open questions

- mpegplayer.library semantics beyond the traced titles (callback/hook
  details, error paths). Mitigation: the consumer set is tiny; implement the
  observed subset, return sane errors elsewhere, keep NOTES-api.md as the
  contract.
- SCR/PTS sync fidelity. The emulator's CL450 model is command-level; if a
  title depends on fine SCR behavior the model lacks, extend the model with
  a regression (hardware-first: cite the CL450 manual).
- The AROS CDXL-ordering fix is PR 1089 commit `ebfc7d9`; retain the real-media
  regression because adjacent-sector swaps are timing-sensitive, and track
  the draft PR until it lands upstream.
- Titles detecting the module by probing ROM contents (e.g. checksumming or
  reading strings at fixed offsets) rather than by API -- would surface in
  Phase 0/4; handle case by case, never by title-keyed branches.
- The $1000 IO write at diag time: meaning unknown; replicate it and note it.
- Legal: interface reimplementation from observation is standard clean-room
  practice, but keep provenance notes; exclude microcode entirely.

## Effort summary

| Phase | Work |
|-------|------|
| 0 API recovery | 1-2 weeks |
| 1 Scaffold + diag | days |
| 2 cd32mpeg.device | 2-4 weeks |
| 3 mpegplayer.library | 3-6 weeks |
| 4 Title matrix | 1-2 weeks |
| 5 AROS enablement | ~1 week + upstream |
| 6 VideoCD player | 3+ weeks (optional) |
| 7 Bundling | days (optional) |

Games-only MVP (0-3 for the Cannon Fodder subset): about a month focused.
Solid general replacement (0-5): 2-3 months.

## References

- `src/cd32_fmv.rs` -- executable spec of the board/chips as emulated
  (banks, IO bits, CL450 commands + interrupts, L64111 registers, microcode
  stored-not-executed).
- `src/zorro.rs` `BoardSpec::cd32_fmv` -- autoconfig identity + diag vector.
- `docs/internals/peripherals.md` "CD32 Full Motion Video module" -- prose
  hardware model incl. analogue keying, TV-aperture mapping, READ DATA
  boundary contract, Akiko drive-protocol ground truth.
- `docs/guide/configuration.md` -- `fmv_rom` user surface.
- AROS sources: `arch/m68k-amiga/diag/diag.c` (diag calling convention),
  `arch/m68k-amiga/romboot/romboot.c` (post-diag romtag init),
  `rom/exec/initcode.c` (ResModules iteration), `arch/m68k-amiga/devs/cd/`
  (AROS cd.device; version bump target). Local tree:
  `~/Programming/Git/AmigaMe/aros-build/AROS`.
- Datasheets: C-Cube CL450 User's Manual; LSI L64111 datasheet.
- Original ROM (interface reference only, never commit): `~/Amiga/cd32fmv.rom`.
- Session memory: `fmv-rom-displaces-aros-cd-device.md` (root-cause record,
  incl. the CCP-blind-to-Zorro-windows gotcha).
