# Integration tests and their assets

`cargo test` runs the unit suite, which needs no external assets. The tests
in this directory are different: they drive the built emulator against
**local Kickstart ROMs and disk images that are not part of this
repository** (they are copyrighted and/or third-party). Every such test is
marked `#[ignore]`, so it never runs under a plain `cargo test`, and each
one checks for its assets first and **skips cleanly (passing) when they are
absent**. A contributor without the assets sees them no-op; they never fail
the build.

Run them, once the assets are in place, with:

```sh
cargo test --release --test image_regression -- --ignored --nocapture
```

## Where the assets are looked up

In order:

1. `COPPERLINE_TEST_ASSETS=/path/to/dir` if set.
2. `test-assets/` under the repo root, if it exists.
3. The repo root itself (legacy fallback).

`test-assets/` and all ROM/disk extensions are gitignored, so assets placed
there cannot be committed by accident. The example config stays in the repo;
the emulator is run with its working directory set to the asset directory,
and a config's relative `rom`/disk paths resolve there.

## What each test needs

The validation is property-based (region colour counts, distinct-colour
bounds, noise detection, perf budgets) -- there are **no committed reference
images**, so nothing copyrighted is stored and there are no brittle
baselines to maintain.

| Test | Assets (exact filenames) |
| --- | --- |
| `kickstart_boot_screen_has_expected_structure` | `kickstart205.rom` |
| `reset_dsksync_boot_regression_reaches_boot_display` | `KICK13.ROM` |
| `ocs_bpu7_ham_captures_*` (incl. live-audio variant) | `kickstart205.rom`, `DESiRE-InsideTheMachine.adf` |
| `dblpal_boot_presents_full_programmable_scan` | `KICK31.ROM`, `wb31-dblpal.adf` |
| `diagrom_menu_preserves_left_margin_text_columns` | `diagrom.rom` |
| `mmu_library_boot_and_muforce_hits_*` | `Kickstart v3.1 r40.68 (1993)(Commodore)(A1200)[!].rom`, `mmu-test.adf`, `mmu-libs.adf` |

## Obtaining the assets legally

- **Kickstart 1.3 / 2.05 / 3.1 ROMs** (`KICK13.ROM`, `kickstart205.rom`,
  `KICK31.ROM`) and a bootable **Workbench 3.1 floppy** (`wb31-dblpal.adf`,
  a WB3.1 boot disk configured for the DblPAL screen mode): licensed via
  [Cloanto Amiga Forever](https://www.amigaforever.com/).
- **DiagROM** (`diagrom.rom`): freely distributed from
  [diagrom.com](https://www.diagrom.com/).
- **Inside The Machine** (`DESiRE-InsideTheMachine.adf`): a scene demo by
  DESiRE, available from [pouet.net](https://www.pouet.net/) / Aminet.
- **MMU test disks** (`mmu-test.adf`, `mmu-libs.adf`): built locally by
  `tests/mmu-disks/make-disks.sh` from any Workbench 3.1 boot disk plus
  Thomas Richter's MMULib (fetched from Aminet by the script). The
  committed `tests/mmu-disks/lawbreaker` binary is built from the adjacent
  `lawbreaker.c` with `m68k-amigaos-gcc -noixemul -O`. These disks drive
  the issue #90 regression: mmu.library building and enabling real
  translation trees on the 030/040, lazy faults through the resumable
  bus-fault frames, and MuForce hit reporting.

The `*.U12` / `*.U13`-style files in the repo root are split EPROM dumps for
expansion-board ROMs (e.g. the A2091 SCSI boot ROM) used by other ignored
tests; they follow the same "never committed" rule.

## Tracked binary fixtures

The tracked `.bin` files are generated test programs, not ROM or disk images:

- `timing-test/boot.bin` is built from `timing-test/boot.asm`.
- `crates/m68k/tests/fixtures/extra/**/bin/*.bin` files are built from the
  adjacent assembly sources under sibling `src/` directories and are used by
  the vendored CPU core's tests.

Run the tracked-file audit in `RELEASE.md` before publishing a rewritten
public repository.

## vAmigaTS

`vamiga_ts.rs` is a separate ignored suite driven by `COPPERLINE_VAMIGATS_*`
env vars against a local [vAmigaTS](https://github.com/dirkwhoffmann/vAmigaTS)
checkout plus a Kickstart 1.3 ROM. See the README "vAmigaTS compatibility
runs" section.
