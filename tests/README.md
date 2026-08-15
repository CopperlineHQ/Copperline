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

## Contributor regression workflow

The fast gate for a CPU, memory, bus, IRQ, or chipset change is:

```sh
cargo test
cargo build --release
cargo test --release --test probe_golden
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
git diff --check
```

The golden renders live under `timing-test/golden/`. A hardware-model change
that intentionally alters them must be re-blessed with
`COPPERLINE_BLESS_GOLDEN=1 cargo test --release --test probe_golden`, with the
render differences reviewed as part of the change. Run the ignored image
suite above when the required local assets are available, followed by any
subsystem-specific private smoke configurations.

Promote a repeated manual smoke path into a focused unit test, an ignored
image regression, or a deterministic input script. Completed investigations
belong in commits and PR descriptions rather than a permanent done-log.

### Hostfs boot round trips

`hostfs_boot_aros_runs_a_guest_binary_and_writes_to_the_host` needs **no
local assets at all** (the bundled AROS ROM boots the mount), so it runs on
any checkout; `hostfs_boot_kick13_runs_a_guest_binary_and_writes_to_the_host` additionally
covers Kickstart 1.3's V34 boot path when a local `KICK13.ROM` is present.
Both boot a shell from a `[[filesys]]` host-directory mount, type `mkfile`
into it (the committed guest probe from `guest/hostfs-test/`), and assert
the file the probe creates arrives on the host side -- autoboot, handler
startup, LoadSeg off the volume, and a write back through it, end to end.

### ATAPI CD-ROM firmware compatibility

`tests/atapi_cd.rs` boots a `[lide]` Zorro II IDE board with LIV2's real
`cdfs.rom` filesystem driver against a genuine ISO9660 image on the ATAPI
slave slot, and checks the emulator's own diagnostic log for the real
driver's expected probe sequence (IDENTIFY DEVICE aborts on the ATAPI slot,
IDENTIFY PACKET DEVICE succeeds, PACKET data-in transfers follow) -- a
firmware-compatibility check that a synthetic-host unit test cannot give,
since it exercises Copperline's ATAPI protocol implementation against real
third-party driver code rather than against our own assumptions about it.
No bundled Amiga OS ships ATAPI support in its stock drivers (see
AGENTS.md), so this cannot be a "guest mounts the CD" test without also
bundling a period driver as an asset; `cdfs.rom` is exactly that. Needs a
local `lide.rom`, `cdfs.rom`, and a bootable hard-disk image under
`test-assets/lide/`, plus a host ISO-authoring tool (`hdiutil` on macOS,
`genisoimage`/`mkisofs` elsewhere); skips cleanly without any of these. The
protocol contract itself (chunked PIO, interrupt-reason register,
error/sense mapping, mixed disk+ATAPI buses) is covered without assets by
the unit tests in `src/ata.rs`.

### WHDLoad boot

`tests/whdload_boot.rs` boots the committed, project-owned
`tests/assets/whdload/TestGame.lha` fixture through the full WHDLoad path
(`--whdload` staging, hostfs boot, the real WHDLoad binary handing control
to the fixture slave) and asserts the slave's solid-colour frame. It needs
the fetched support archives (`tools/fetch-whdload.sh` populates
`assets/whdboot/`) and a Kickstart 3.1 (40.068 A1200) image anywhere in the
asset directory -- identification is by content, the filename does not
matter -- and skips cleanly without either.

### DiagROM smoke

DiagROM is the first asset-backed boot smoke before a Kickstart OS check:

```sh
./target/release/copperline --model A500 --noaudio \
  --screenshot-after 5 /tmp/diag.png /path/to/diagrom.rom
```

The run should produce serial diagnostics and a nonblank screenshot without
an unexpected CPU halt. On the current headless path the serial-enable prompt
appears around `t=14s`, times out around `t=18s`, and menu input is reliable
from `t=22s`. Useful DiagROM menu paths are `3`, `4`, `1` for the IRQ test and
`4`, `5`, then any key for the embedded graphics-test intro. Script these with
repeatable `--press-after` events rather than host-time input.

### Manual chipset regressions

These fixed regressions remain useful stress scenes when their underlying
hardware models change. The media and local configurations are private test
assets, never repository inputs or compatibility conditions:

- **Inside The Machine**: `18s` board sparks/electric effect; `60s` coherent
  tunnel runner rather than noise; `70s` face-light sprites clipped to the
  active window; `80s` Roto/HAM bottom rows without captured-row garbage;
  `90s` stable card traces; `127s` HAM torus/balls rather than vertical strips;
  and a falling-man handoff that has left the static figure for the next
  runner scene by `180s`. The `165s`/`180s` pair guards Copper frame reload,
  BLTPRI-clear CPU access, and the enabled-audio-DMA reservation window.
- **State of the Art**: dense hand/silhouette overlap should remain free of
  diagonal blitter edge-mask trails. Use it after changes to line mode,
  first/last-word masks, blitter completion, or bitplane capture.
- **Frontier**: retain stable output after bus, blitter, bitplane, or chipset
  timing changes.

Logs from these runs should not contain unexpected `halt`, exception, or
invalid-memory warnings unless the test deliberately exercises that path.

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
| `hostfs_boot_aros_runs_a_guest_binary_and_writes_to_the_host` | *(none)* |
| `hostfs_boot_kick13_runs_a_guest_binary_and_writes_to_the_host` | `KICK13.ROM` |
| `ocs_bpu7_ham_captures_*` (incl. live-audio variant) | `kickstart205.rom`, `DESiRE-InsideTheMachine.adf` |
| `dblpal_boot_presents_full_programmable_scan` | `KICK31.ROM`, `wb31-dblpal.adf` |
| `diagrom_menu_preserves_left_margin_text_columns` | `diagrom.rom` |
| `mmu_library_boot_and_muforce_hits_*` | `Kickstart v3.1 r40.68 (1993)(Commodore)(A1200)[!].rom`, `mmu-test.adf`, `mmu-libs.adf` |
| `picasso2_workbench_opens_640x480x8` | `Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom`, `p96-picasso2.hdf` (WB3.1 + Picasso96, default 640x480x8 screen) |
| `picasso2_workbench_opens_640x480x16` | `Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom`, `p96-picasso2-16.hdf` (same, default 640x480x16 screen) |
| `picasso2plus_workbench_opens_with_gd5428_revision` | `Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom`, `p96-picasso2.hdf` (same installation booted against the Picasso II+ identity) |
| `picasso2_p96cts_reports_all_modes_clean` | `Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom`, `p96-picasso2-cts.hdf` (startup runs p96cts at 8/16/24 bpp and writes `P96OUT:p96cts.result`) |
| `graffity_z2_workbench_opens_640x480x8` / `graffity_z3_workbench_opens_640x480x8` | `Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom`, `p96-graffity.hdf` (WB + Picasso96 with `Graffity.card`, default 640x480x8 screen) |
| `chd_cd32_disc_serves_iso9660_data_and_smooth_audio` | `Pinball Fantasies (EU).chd` (a chdman v5 CD32 disc with a MODE1_RAW data track and CD audio tracks) |

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
- **Picasso II/II+ HDFs** are local Workbench 3.1 installations with Picasso96's
  `PicassoII.card` driver (Picasso96 is freeware, from Aminet
  `driver/gfx/Picasso96.lha`). The boots use an A4000 over motherboard IDE
  because the generic `KICK31.ROM` has no big-box SCSI/IDE driver. The
  `-cts` image's startup sequence runs p96cts for
  its 8-, 16-, and 24-bit mode set and writes `PASS` to
  `P96OUT:p96cts.result`; on failure it leaves the suite's diff images in that
  host-mounted output directory.
- **The Graffity HDF** (`p96-graffity.hdf`) is the same kind of local
  Workbench + Picasso96 installation, with `Graffity.card` from the same
  Aminet Picasso96 archive installed in `Libs/Picasso96/`. Converting an
  existing PicassoII-configured installation needs three guest-side edits
  (all patchable from the host with a hex editor or a short script, since
  the fields are fixed-size NUL-terminated strings): a
  `Devs/Monitors/Graffity` monitor (copy the generic Picasso96 monitor
  binary; in its `.info`, change the `BOARDTYPE=PicassoII` tooltype to
  `BOARDTYPE=Graffity`, fixing up the 4-byte big-endian length prefix
  before the string), and the `Devs/Picasso96Settings` file's BDNM
  board-name field plus its `PicassoII:WxH` mode-name strings renamed to
  `Graffity:...`. Picasso96's own `Picasso96/debug/CheckBoards` is a good
  smoke check: it reports the claimed board's name, chip, and memory base,
  and brings the RTG screen to front for about two seconds.

The `*.U12` / `*.U13`-style files in the repo root are split EPROM dumps for
expansion-board ROMs (e.g. the A2091 SCSI boot ROM) used by other ignored
tests; they follow the same "never committed" rule.

## Tracked binary fixtures

The tracked `.bin` files are generated test programs, not ROM or disk images:

- `timing-test/*.bin` files (the boot block plus the `ddfprobe-*`,
  `bltprobe-*`, `audprobe-*`, `clxprobe`, `regprobe-*`, and `sprprobe-*`
  probe programs) are each built from the adjacent `.asm` source, and
  `timing-test/golden/*.png` are their blessed reference renders (see
  `timing-test/README.md` "CI golden renders").
- `assets/services/services_rom.bin` is the guest-side host-filesystem
  handler built from `guest/services/`.
Run the tracked-file audit in `RELEASE.md` before publishing a rewritten
public repository.

Unlike the suites above, `probe_golden.rs` (the golden-render suite for
those probes) needs no external assets: it boots the bundled AROS ROM and
runs in CI on every push.

## vAmigaTS

`vamiga_ts.rs` is a separate ignored suite driven by `COPPERLINE_VAMIGATS_*`
env vars against a local [vAmigaTS](https://github.com/dirkwhoffmann/vAmigaTS)
checkout plus a Kickstart 1.3 ROM. See the README "vAmigaTS compatibility
runs" section.
