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

### Open A2091 ROM autoboot

`tests/a2091_boot.rs` fits the bundled clean-room A2091/A590 ROM in three
end-to-end boots. The asset-free AROS case boots a directory-built FFS volume;
the Kickstart 1.3 case boots an OFS volume and needs `KICK13.ROM`; the Kickstart
3.1 case boots a real RDB Workbench image through the WD33C93/DMAC path and
needs `KICK31.ROM` plus `AmigaSYS3PlusAGA-rdb.hdf`. Put those files in
`test-assets/` or `COPPERLINE_A2091_TEST_ASSETS`; asset-backed cases skip
cleanly when their inputs are absent. The 3.1 test asserts a non-blank desktop
and a diagnostic trace containing DMAC starts, 24-bit addresses, and delivered
INT2 status. Run all available cases with:

```sh
cargo test --release --test a2091_boot -- --ignored --nocapture
```

### Open CD32 FMV ROM

`tests/cd32_fmv_aros.rs` contains two Cannon Fodder paths. The AROS case uses
PR 1089 through CDXL-ordering commit `ebfc7d9`; the Kickstart case uses the
replacement ROM's own `cd32mpeg.device` and standard Mode-2 reader. Both assert sustained
MPEG decoding without malformed-stream recovery, full-colour 60-second
output, and non-silent stereo. The media and proprietary Kickstart remain
local; run both with:

```sh
COPPERLINE_FMV_AROS_DIR=/path/to/patched-aros-roms \
COPPERLINE_FMV_ROM=/path/to/copperline-fmv.rom \
COPPERLINE_FMV_CANNON_FODDER_CUE=/path/to/Cannon-Fodder.cue \
COPPERLINE_FMV_KICKSTART_ROM=/path/to/cd32-kickstart.rom \
COPPERLINE_FMV_KICKSTART_EXT_ROM=/path/to/cd32-extended.rom \
cargo test --release --test cd32_fmv_aros -- --ignored --nocapture
```

`tests/cd32_videocd.rs` has two CD32 Kickstart real-media regressions. The
first boots the committed guest probe, opens the cartridge's
`videocd.library` by its public LVO table, and checks the Philips Media Retail
Sampler '95 metadata. The second cold-boots the same disc into the player,
presses Red, checks sustained 352x240 decoding, presses Blue, and verifies the
menu returns. Build and bundle the ROM plus the probe first, then run:

```sh
COPPERLINE_FMV_ROM=/path/to/copperline-fmv.rom \
COPPERLINE_FMV_VIDEOCD_CUE=/path/to/Philips-Sampler.cue \
COPPERLINE_FMV_KICKSTART_ROM=/path/to/cd32-kickstart.rom \
COPPERLINE_FMV_KICKSTART_EXT_ROM=/path/to/cd32-extended.rom \
cargo test --release --test cd32_videocd -- --ignored --nocapture
```

### Modem end to end

`tests/modem_e2e.rs` closes the one gap `src/modem/`'s exhaustive unit
suite (a fake transport) cannot: the real hardware path between the AT
state machine and a guest -- Paula's SERPER-paced UART, the CIA-B
control-line overlay, and a real `serial.device` actually driving them. It
boots from a `[[filesys]]` mount holding the committed guest probe
(`guest/modem-test/modemtest`, built like `guest/hostfs-test`'s), lets a
real Startup-Sequence run it, and asserts the transcript it writes back:
`ATZ` answered `OK`, `ATDT` dialing a one-shot local TCP peer this test
spins up on an ephemeral port, `CONNECT`, the peer's greeting arriving at
the guest, and the guest's own line arriving at the peer.

Unlike the hostfs tests above, this one cannot use the bundled AROS ROM:
`serial.device` is not ROM-resident on real AmigaOS either (Kickstart
2.0+ loads it from `DEVS:serial.device` on demand via `LoadSeg` the first
time something opens it), and the bundled AROS build carries no
`serial.device` at all, ROM-resident or otherwise. It needs a local
Kickstart ROM (`KICK31.ROM`, anywhere in the asset directory or the repo
root) and a real `Devs/serial.device` driver file at
`test-assets/modem/Devs/serial.device` (copy one out of any Workbench
2.0+ install), and skips cleanly without either.

### WHDLoad boot

`tests/whdload_boot.rs` boots the committed, project-owned
`tests/assets/whdload/TestGame.lha` fixture through the full WHDLoad path
(`--whdload` staging, hostfs boot, the real WHDLoad binary handing control
to the fixture slave) and asserts the slave's solid-colour frame. It needs
the fetched support archives (`tools/fetch-whdload.sh` populates
`assets/whdboot/`) and a Kickstart 3.1 (40.068 A1200) image anywhere in the
asset directory -- identification is by content, the filename does not
matter -- and skips cleanly without either.

### copperhf.device integration matrix

`COPPERHF-DEVICE-PLAN.md`'s M6 splits the {RDB, RDB-less} x {OFS,
FFS-from-LSEG, PFS3-DS >4 GiB} x {Kickstart 1.3, 3.1, 3.2} matrix across two
files, alongside the earlier `tests/copperhf_device.rs` (M2),
`tests/copperhf_m4_guest.rs` (M4), `tests/copperhf_mounter.rs`/
`tests/copperhf_m5.rs` (M3/M5) suites, all of which need no local assets:

- `tests/copperhf_m6.rs` (default CI, no assets) --
  `lseg_filesystem_loads_into_filesystem_resource` builds a hand-crafted
  RDB (RDSK + PART + FSHD + LSEG chain) around
  `guest/copperhf-test/lsegfix` -- a tiny, self-checking three-hunk
  fixture binary (generated by the adjacent `gen_lsegfix.py`; its own
  header comment documents the exact byte layout, including a
  deliberately truncated hunk and an odd-word `HUNK_RELOC32SHORT`
  padding case) -- and drives `guest/copperhf-test/chftest_m6` (built by
  `guest/copperhf-test/Makefile`) over `--run` to verify `guest/copperhf/
  mounter.c`'s FSHD/LSEG loader end to end: the `FileSystem.resource`
  entry it creates, the loaded seglist's chain length, actually calling
  the seglist's entry point (the relocation self-test), and the mounted
  `DeviceNode`'s patched `SegList`/`GlobalVec` fields. This is the
  default-CI stand-in for the FFS-from-LSEG axis -- a synthetic fixture,
  not a real filesystem binary, so no licensed asset is needed.
- `tests/copperhf_kickstarts.rs` (`#[ignore]`d, real ROM assets) -- the
  matrix against actual Kickstart 1.3/3.1/3.2:
  - **Plain OFS**, {RDB, RDB-less}, one test per ROM. 3.1/3.2 (the ROM
    shell has `Echo` built in from Kickstart 2.0 on) reuse
    `tests/copperhf_mounter.rs`'s exact bootmark-file trick: a
    Startup-Sequence that echoes a marker string to `SYS:bootmark`,
    verified by walking the mounted volume's own directory structure
    afterwards (not a raw byte search -- see that file's header comment
    for why a byte search would be a false positive). Machine shape:
    A1200/AGA for 3.1 and 3.2, matching `tests/image_regression.rs`'s own
    KICK31 config. **1.3 has no ROM-resident `Echo`** (disk-based `C:`
    commands only, and this fixture carries none), so it is verified
    instead by an exact-byte golden screenshot
    (`tests/golden/copperhf/*.png`, `COPPERLINE_BLESS_GOLDEN=1` to
    (re)generate) of the CLI prompt an *empty* Startup-Sequence leaves
    open on the mounted volume -- weaker than a structural marker check,
    but the only option that stages no extra binary onto the volume;
    machine shape matches `tests/image_regression.rs`'s own KICK13
    config (A500-shaped OCS/68000, no `[machine]` override needed). This
    is also the **first** copperhf test to boot Kickstart 1.3 from a
    copperhf unit at all, so a 1.3 failure here may be a genuine,
    previously-unexercised bug in the V34 `AddDosNode`/`eb_MountList`
    fallback path in `guest/copperhf/mounter.c` -- this machine has no
    `KICK13.ROM`, so that path is reviewed but **locally unverified**.
  - **FFS-from-LSEG**, Kickstart 3.1 plus a bundled-AROS variant
    (`aros_ffs_from_lseg_...`, still `#[ignore]`d for the FastFileSystem
    asset but needing no ROM -- AROS's dos.library starting a real
    Commodore FFS through the FSHD/LSEG chain is its own compatibility
    data point). The 3.1 row is the gate (`COPPERHF-DEVICE-
    PLAN.md` actually asks for). Needs `test-assets/copperhf/
    FastFileSystem`, a real FastFileSystem binary. Design note: Kickstart
    3.1's ROM already has DOS\0 (OFS) and DOS\1 (FFS) resident in
    `FileSystem.resource`, so tagging the partition/FSHD DOS\1 would make
    `guest/copperhf/mounter.c`'s own "already resident, skip the LSEG
    load" check (see its header comment) short-circuit the very path this
    test means to exercise. DOS\3 (FFS + international mode) is *not*
    ROM-resident on 3.1 -- the entire reason FastFileSystem replacement
    binaries existed on real Workbench 3.1 disks -- so the partition and
    FSHD are both tagged DOS\3 instead, which forces the mounter to
    actually load and run the real binary. The DOS\3 on-disk block
    layout is identical to plain FFS (only filename hashing differs, and
    this fixture's filenames are all plain ASCII), and `src/dirfs.rs::
    build_image` already supports building it directly via
    `FileSystem { ffs: true, variant: Variant::Intl }` -- no bespoke FFS
    emitter was needed. Verified the same bootmark way as the OFS axis.
  - **PFS3-DS beyond 4 GiB**, Kickstart 3.1 plus a bundled-AROS variant
    (`aros_pfs3_...`, `#[ignore]`d for the pfs3aio asset only). Needs
    `test-assets/copperhf/pfs3aio`, the real PFS3 "all-in-one" binary.
    **Deliberately narrow scope**: this project has no host-side tool to
    *format* a PFS3 volume (`src/dirfs.rs` only ever implemented plain
    OFS/FFS, and PFS3's own `Format` is a Workbench-disk command this
    project does not carry as a test asset), so the test cannot stage a
    mounted, usable PFS3 volume the way the OFS/FFS-from-LSEG axes do.
    What it actually proves: a sparse (`File::set_len`) >4 GiB partition,
    tagged `PDS\3` and carrying the real `pfs3aio` binary through an
    FSHD/LSEG chain exactly like the FFS-from-LSEG axis, attached
    alongside an ordinary bootable OFS unit, does not crash, hang, or
    otherwise prevent Kickstart 3.1 from completing a normal boot --
    i.e. copperhf's TD64/NSD command path and >4 GiB sparse backing file
    survive a real `pfs3.handler` actually starting up and probing the
    unit. It does **not** assert `NDOS`/`PDS:` volume state and does
    **not** drive a `Format`. A fuller manual smoke test -- attach the
    same fixture to a real Workbench 3.1 install with PFS3 already set up
    from Prefs, `Format` the partition by hand, and confirm it mounts and
    is usable past the 4 GiB mark -- is the sturdier follow-up this
    automated test does not attempt; there is no tracked recipe for it
    yet since it needs a hand-prepared Workbench install, not just a
    dropped-in asset file.

  This machine has only `KICK31.ROM` locally: the 3.1 plain-OFS axes
  above (`kick31_ofs_rdb_autoboots_and_writes_bootmark`/
  `kick31_ofs_rdbless_autoboots_and_writes_bootmark`) actually run and
  pass here. Every other test in this file (1.3, 3.2, FFS-from-LSEG,
  PFS3) is missing its asset on this machine and is only verified to
  **skip cleanly** -- run them for real once the assets below are
  available:

  | Asset | Used by |
  | --- | --- |
  | `KICK13.ROM` | the 1.3 OFS axes (golden-screenshot verified) |
  | `KICK31.ROM` | the 3.1 OFS, FFS-from-LSEG, and PFS3 axes |
  | `KICK32.ROM` | the 3.2 OFS axes |
  | `test-assets/copperhf/FastFileSystem` | the FFS-from-LSEG axis |
  | `test-assets/copperhf/pfs3aio` | the PFS3-DS >4 GiB axis |

  Run the whole file with:

  ```sh
  cargo test --release --test copperhf_kickstarts -- --ignored --nocapture
  ```

  Building `tests/copperhf_m6.rs` did exactly the job it exists to do:
  its first runs caught three real, previously-unexercised
  `guest/copperhf/mounter.c` bugs (forward-referencing relocations,
  odd-aligned FileSysStartupMsg after an even-length partition name,
  `ADNF_STARTPROC`/`ConfigDev` passed for non-bootable partitions), all
  fixed at the site there -- plus one fixture-design lesson worth
  keeping: AmigaDOS starts a mounted DeviceNode's `dn_SegList` as a real
  handler process on first reference, so the fixture implements a
  minimal DOS-packet loop and the probe verifies relocation correctness
  through `Lock("CHFM6:")`'s `IoErr()` rather than by calling the
  seglist raw (`guest/copperhf-test/gen_lsegfix.py` and `chftest_m6.c`
  document the mechanism).

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
| `cannon_fodder_streams_cleanly_through_the_aros_open_rom` | PR 1089 through commit `ebfc7d9` AROS main/ext ROMs, generated `copperline-fmv.rom`, Cannon Fodder CUE and tracks |
| `cannon_fodder_streams_cleanly_through_the_standalone_kickstart_rom` | CD32 Kickstart 3.1 main/ext ROMs, generated `copperline-fmv.rom`, Cannon Fodder CUE and tracks; supplied through the `COPPERLINE_FMV_*` variables above |
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
| `nrg_cd32_disc_serves_iso9660_data_and_smooth_audio` | `30 Games Compilation CD (2005)(Stuermer, A.).nrg` (a Nero 5 DAO CD32 disc with one MODE1/2048 data track and nine CD audio tracks) |
| `toccata_ahi_driver_recognizes_the_board` | `Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom`, `toccata-ahi.hdf` (WB3.1 + AHI 4.18 with `toccata.audio` staged into `Devs/AHI` and Unit 0 set to Toccata) |
| `zz9k_sdk_tools_pass_on_zorro_ii` / `zz9k_sdk_tools_pass_on_zorro_iii` | `zz9k/C/zz9k-{info,hash,chacha,aead,irqtest}` -- the unmodified ZZ9000 SDK m68k tools, built from the zz9000-sdk revision pinned in `docs/internals/zz9k.md` (build recipe in `tests/zz9k_sdk_tools.rs`'s module comment) |

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

- **Toccata/AHI HDF** (`toccata-ahi.hdf`) is a local Workbench 3.1
  installation with **AHI 4.18** (freeware, from Aminet's
  `mus/misc/AHIUser418.lha`) installed, plus its bundled `toccata.audio`
  driver (already shipped in that same archive's `AHI/User/Devs/AHI/`
  directory -- both the 68020+ build and the `.000` 68000 build -- along
  with the `AHI/User/Devs/AudioModes/TOCCATA` mode descriptor AHI prefs
  needs). To build one: install AHI onto a WB3.1 HDF as normal, copy
  `toccata.audio` and `AudioModes/TOCCATA` from the archive into the
  installed `Devs/AHI` and `Devs/AudioModes`, boot with `[toccata] enabled
  = true`, open `AHI` in Prefs, select Unit 0 = Toccata, and save so the
  choice persists in `ENV:Sys/ahi.prefs` (copy forward to `ENVARC:` too).
  The A4000/motherboard-IDE boot shape matches the Picasso II HDFs above,
  for the same reason (no big-box IDE driver in the generic `KICK31.ROM`).

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
- `guest/zz9kprobe/zz9kprobe` is the zz9k crypto board's guest conformance
  probe built from `guest/zz9kprobe/` (the vendored ZZ9000 SDK transport
  plus the probe source); `tests/zz9k.rs` boots it on the bundled AROS ROM
  with no external assets.
- `guest/uaelib-test/uaelibtest` is the uaelib-trap probe (the
  vscode-amiga-debug template's `warpmode`/`KPrintF`/`debug_*` helpers as
  written) built from `guest/uaelib-test/`; `tests/image_regression.rs`
  boots it with `--run` on the bundled AROS ROM, with no external assets.
Run the tracked-file audit in `RELEASE.md` before publishing a rewritten
public repository.

Unlike the suites above, `probe_golden.rs` (the golden-render suite for
those probes) needs no external assets: it boots the bundled AROS ROM and
runs in CI on every push. `hrtmon_freeze.rs` is asset-free the same way
(bundled AROS plus the bundled HRTMon cartridge image) and release-only:
it freezes the booted machine through the library and through the binary's
`--freeze-after`, and checks the monitor's `entered` flag, its screen, and
that a state taken inside the monitor resumes inside it.

`audio_stems_determinism.rs` also needs no external assets (bundled AROS,
an empty default DF0) -- it is `#[ignore = "runs the emulator"]` rather
than release-only-gated like `probe_golden.rs`, so run it explicitly:

```sh
cargo test --release --test audio_stems_determinism -- --ignored
```

## vAmigaTS

`vamiga_ts.rs` is a separate ignored suite driven by `COPPERLINE_VAMIGATS_*`
env vars against a local [vAmigaTS](https://github.com/dirkwhoffmann/vAmigaTS)
checkout plus a Kickstart 1.3 ROM. See the README "vAmigaTS compatibility
runs" section.
