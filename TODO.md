# Copperline TODO

## Current Status

- Copperline now uses the pure-Rust `m68k` CPU core. The Unicorn backend,
  forked QEMU patches, MOVEC/F-line hooks, and stopped-slice block
  estimator have been removed.
- Configurable CPU models are `68000`, `68010`, `68EC020`, `68020`,
  `68030`, `68040`, and `68060` (`[cpu] unimplemented` selects trap
  vs. native handling for the instructions the 68060 dropped). The
  68030 and 68040 MMUs are modelled with resumable fault frames.
  `cpu.fpu = true` fits a 68881/68882 on any 020+ (default-on for the
  full 68040 and the 68060, whose FPUs are on-die): the vendored
  core's FPU is a pure-Rust 80-bit extended-precision softfloat
  engine covering every operand format including packed decimal,
  extended-precision transcendentals (faithful to ~64 bits), real
  FSAVE/FRESTORE frames (NULL after reset), and the full conditional
  family. Verified against Kickstart detection + exec context
  switching on 1.3/2.05/3.1 and SysInfo's hardware report (68881).
- DiagROM 2.0 via `copperline.example.toml` is the minimum viable boot and
  first smoke test. The current `m68k` backend reaches the DiagROM menu
  setup path, including OVL, chipmem, raster detection, Copper/DMA
  setup, serial output, and screenshot capture.
- Kickstart 2.05 remains a useful OS/chipset regression target, but it
  should be run after the DiagROM smoke passes. Kickstart progress is
  expected to depend more on chipset, CIA, floppy, and rendering
  behavior than on basic CPU boot viability.
- Keyboard, mouse, CIA-A keyboard handshaking, keyboard reset, RTC,
  Paula serial, live/WAV audio, Copper scheduling, blitter basics, and
  OCS bitplane rendering all have focused coverage.
- Floppy support covers standard DD ADF, read-only ADZ/DMS/SCP, UAE
  extended ADF variants, gzip- or single-file-ZIP-packed images,
  track-timed read/write DMA, and writable sector/raw-track
  persistence for supported writable formats.
- IPF/CAPS protected-image support is an explicit non-goal for the
  built-in loader for now. IPF files start with a `CAPS` record; useful
  support should either be a direct IPF parser or an optional SPS/CAPS
  library integration, with licensing, platform packaging, and dynamic
  loading reviewed before it becomes a default build dependency.
- The unit test suite is the default fast regression gate. Image and
  vAmigaTS integration tests remain ignored by default because they
  require local ROM/disk assets and longer emulator runs.
- The emulator is no longer purely pragmatic about timing: the 68000
  path has a prefetch-queue model with per-access cycle billing
  (~99.6% timing-exact against SingleStepTests, see
  `crates/m68k/CYCLE_TIMING_GAP.md`), interrupt-recognition latency,
  and chip-bus slot arbitration with CPU/blitter/DMA contention. The
  020+ models add the on-chip instruction/data caches with calibrated
  bus costs rather than per-microcycle sequences, and all models run
  clean through the cputest harness (`crates/cputest-runner`). Paula
  audio follows the HRM DMA state machine. Remaining ECS/AGA gaps are
  recorded next to each subsystem in `docs/internals/` and should be
  tightened only when driven by a concrete regression.

## Standard Checks

Run these before declaring a CPU, memory, bus, IRQ, or chipset change
done. DiagROM comes first; Kickstart is a follow-up chipset/OS check.

1. `cargo test`
2. `cargo build --release`
3. `./target/release/copperline --noaudio --config copperline.example.toml --screenshot-after 5.0 /tmp/diag.png`
4. `cargo test --release --test probe_golden` (the golden renders in
   `timing-test/golden/`; a timing or render change that alters them must
   be re-blessed with `COPPERLINE_BLESS_GOLDEN=1`, see timing-test/README.md)
5. `cargo test --release --test image_regression -- --ignored --nocapture`
6. Kickstart 2.05 boot check, from a local config whose `rom` is a Kickstart 2.05 image: `./target/release/copperline --noaudio --config path/to/local.toml --screenshot-after 8.0 /tmp/ks.png`
7. Run any local/private smoke configs needed for the subsystem under review.
8. `git diff --check`

Expected results:

- Unit tests pass.
- DiagROM reaches the menu setup path, writes serial diagnostics, and
  saves a nonblank screenshot without a CPU halt.
- Image regressions pass when local ROM assets are available.
- Kickstart shows the 2.05 boot screen with correct colors, floppy
  shape, and 4:3 aspect when the chipset regression is being checked.
- Inside The Machine should show a coherent tunnel runner scene at
  `t=60s`, not random noise. For deeper Copper/video checks, also dump
  `t=18s` for the board spark/electric effects, `t=70s` for the
  face-lights sprite clipping, `t=80s` for the Roto/HAM bottom rows,
  and `t=90s` for later tunnel/card-trace visuals. The falling-man
  handoff should not freeze on a static falling figure at `t=165s`; by
  `t=180s` it should have reached the next runner scene.
- Logs should not show unexpected `halt`, `excp`, or invalid-memory
  warnings unless the test intentionally exercises that path.

## Useful Commands

| What | Command |
| ---- | ------- |
| Build | `cargo build --release` |
| Unit tests | `cargo test` |
| Image regressions | `cargo test --release --test image_regression -- --ignored --nocapture` |
| Blitter tests only | `cargo test --release blitter` |
| DiagROM first smoke | `./target/release/copperline --noaudio --config copperline.example.toml --screenshot-after 5.0 /tmp/diag.png` |
| Kickstart 2.05 screenshot | create a local config whose `rom` is a Kickstart 2.05 image, then run `./target/release/copperline --noaudio --config path/to/local.toml --screenshot-after 8.0 /tmp/ks.png` |
| AmigaTestKit ADF screenshot | create a local config that sets `rom` and `[floppy.df0]`, then run `./target/release/copperline --noaudio --config path/to/local.toml --screenshot-after 2.0 /tmp/atk.png` |
| Local OCS HAM/Copper regression dumps | use a local/private config and dump the relevant windows with `--dump-start SECS --dump-count N --dump-frames /tmp/name` |

DiagROM key timing:

- The serial-enable prompt is visible around `t=14s` and times out
  around `t=18s` on the current headless/noaudio path. Start menu key
  presses at `t >= 22s`.

Raw key shortcuts:

- `--press-after SECS KEY` now sends a short press/release sequence.
  `KEY` can be a raw number or a name such as `ctrl`, `lalt`, `lami`,
  `f1`, `esc`, `left`, etc.
- `--key-after SECS KEY MS` holds a key for `MS` milliseconds before
  sending the release byte.
- Digits: `0x01` = `1`, `0x02` = `2`, ..., `0x09` = `9`,
  `0x0A` = `0`.
- Useful paths:
  - IRQ test: `3`, `4`, `1`
  - Graphics test picture: `4`, `5`, then any key for the embedded
    gfxC intro prompt

## Open Work

Only genuinely-remaining work is listed here. Closed audit work lives in
git history, not as a done-log in this file.

1. **CPU exception/interrupt test coverage.** `src/cpu.rs` covers reset
   vectors, Line-F, illegal/privilege violations, and autovector/RTE
   paths. Still missing: Line-A, stopped-CPU wakeup, an explicit 24-bit
   address-masking test on `68EC020`, and a `68040`-as-LC040 snippet (the
   `cpu_type_for_model` mapping exists but is untested).

2. **POTGO RC modelling.** `write_potgo`/`read_potgor`/`tick_pot_hsync`
   (`src/chipset/paula.rs`) model output loopback with open-drain pin
   sense, the START reset/restart, and a per-scan-line counter charge
   paced by horizontal sync. Remaining: an RC-style charge curve from
   real pot resistance and chip-revision bits.

3. **CIA gaps.**
   - CIA-A physical port restrictions: disk-sense bits PA5-PA2 must stay
     inputs even when DDRA marks them as outputs.
   - Parallel-port / Centronics behaviour: CIA-B PRA strobe on data write,
     CIA-A FLAG acknowledge, PB6/PB7 timer-output handshake, and defined
     behaviour for the dormant CIA-B SDR/CNT pins.
   - CIA timer/TOD sub-cycle edges (next-E-clock TAHI/TBHI load, delayed
     TOD carry, PB6/PB7 pulse width, `PC` pulse timing) are sub-cycle phase
     delays the instruction-paced core cannot model faithfully; deferred to
     any future cycle-exact CPU work rather than faked. (TOD write-stop and
     the $000000 alarm reset are already correct, with tests.)

4. **Memory-map tests.** Chip/slow/fast RAM sizes are config-selectable and
   `src/cpu.rs` covers the boot overlay, ROM write-protect, slow-RAM bus
   contention, chip-window aliasing by Agnus reach, and the small-box
   custom-register mirror. Remaining: fast-RAM mirror coverage and an
   explicit A500 Rev.6 512K+512K layout regression.

5. **Keep the test matrix maintainable.** Promote a repeated manual smoke
   path into a fast unit test, an ignored image regression, or a scripted
   command here. Record completed audit work in commits and PR
   descriptions, never as a done-log in this file.

## Known software regression watch

These titles exercise specific timing/rendering edges; keep them in the
practical visual regression set when touching the named subsystems.

- **Inside The Machine** (local/private config): `18s` board
  spark + chip electric effect; `60s` tunnel runner scene (not noise);
  `70s` face-lights sprites stay inside the active window; `80s` Roto/HAM
  bottom rows free of captured-row garbage; `90s` stable card-trace
  visuals; `127s` HAM torus/balls (not vertical strips or C2P corruption).
  The Roto/torus parts use OCS lowres `BPU=7` (four DMA bitplanes plus
  fixed `BPL5DAT`/`BPL6DAT` HAM modifier latches) -- a good captured-row
  vs source-row-reconstruction regression. The falling-man handoff
  (`165s`/`180s`) guards chip-bus arbitration: BLTPRI-clear blits must
  leave the CPU's regular bus phase available, and enabled audio DMA must
  reserve its early-line window even with no word request for a slot.
  Still open: compare Copper location-register behaviour against WinUAE or
  hardware, and promote stable captures into ignored image regressions once
  robust reference images are chosen (earlier demo-timeline image tests
  drifted and were removed).
- **State of the Art**: much cleaner after the blitter/bitplane capture
  work, but dense hand/silhouette overlap can still show diagonal edge-mask
  trails. Next step is a whole-buffer comparison against WinUAE or hardware
  with blitter completion timestamps.
- **Frontier**: bus, blitter, bitplane, and timing changes have all
  affected its visible output; keep it in the set when changing chipset
  timing.

## Notes Worth Keeping

### Kickstart 2.05 ExecBase Layout

These offsets were observed from Kickstart 2.05 and do not match later
AmigaOS SDK headers:

| Field | Offset from ExecBase | Notes |
| ----- | -------------------- | ----- |
| `ThisTask` | `+$114` | Current task/process |
| `ResModules` | `+$12C` | APTR-terminated Resident pointer table |
| `LibList` head | `+$17A` | Library list |
| five list heads | `+$1B2..+$1F2` | TaskReady/TaskWait/etc. |

Observed resident/library details:

- `OpenLibrary` LVO -552 at ROM `$F819AE`
- `InitCode` LVO -72 at ROM `$F80F4C`
- `InitResident` LVO -102 at ROM `$F80F86`
- `graphics.library` resident header at ROM `$FA8C28`
- `graphics.library` base in chip RAM around `$2A50`
- `expansion.library` base in chip RAM around `$A44`

### Debug Probe Gotchas

- `M68kMachine` owns the CPU core and `CpuBus`; CPU-visible behavior
  should generally be tested through executed ROM snippets rather than
  by mutating `Bus` state directly.
- Keep address-space assumptions explicit. `68000` and `68EC020` use a
  24-bit external address mask; `68020`, `68030`, and `68040` use the
  32-bit mask in the CPU bus adapter.
- Exception and interrupt regressions are easiest to diagnose with tiny
  programs that assert final `PC`, `SR`, `A7`, stack frame contents, and
  Paula/CIA interrupt latches.
