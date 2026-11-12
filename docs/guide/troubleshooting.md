# Troubleshooting and known limits

## Match the manual to the build

Check **About** before copying configuration from a manual. Its version string
matches the manual's label: the package version at an exact Git tag, or a
string such as `1.1.0+g12345678` for an untagged source build. The command
`copperline --version` prints only the package version, so it cannot
distinguish source revisions.

The online manual is replaced when a release is published; its banner names
the version and source revision it describes. A source snapshot can have
changes beyond the release with the same package version. The persistent
sidebar footer keeps that identity visible even when the banner has been
dismissed.

For an installed release, download the matching `Copperline-VERSION-manual.pdf`
from that version's [GitHub release](https://github.com/CopperlineHQ/Copperline/releases).
For a source build, build the manual from the same checkout as the executable.
The [changelog](../changelog.md) records configuration changes and save-state
compatibility. Features documented for a later build may be rejected by an
older executable.

## Configuration will not load

An `unknown field` error names a key the executable does not accept. Check
its spelling, its TOML table, and the manual version. A key belongs to the
last uncommented `[table]` header above it; commenting out a header does not
comment out its values. Compare with the `copperline.example.toml` shipped
with the same source revision. Do not copy a newer example wholesale into
an older installation.

These older spellings have specific compatibility behaviour:

| Older setting | Current setting or action | Compatibility |
|---|---|---|
| `[machine] model` | `[machine] profile` | Alias still accepted. `[cpu] model` continues to select the CPU. |
| `[audio] filter` | `[audio] audio_filter` | Alias still accepted. |
| `[floppy.dfN] bridge_speed` | `[floppy.dfN] replay_speed` | Alias still accepted; use `normal` or `fast` (100 or 200 also work). |
| `--floppy-bridge-speed DFN SPEED` | `--floppy-replay-speed DFN SPEED` | Alias still accepted. |
| `[lide] drives` | `[lide] drive0` through `drive3` | Old array still accepted; named slots override it. |
| `[floppy.dfN] bridge_auto_cache` | Remove the key. | Rejected. FluxBridge no longer does a background disk sweep. |
| `[emulation] speed` | Remove the key. | Accepted with a warning and ignored; it does not choose a timing model. |

Replace an alias rather than adding the current spelling beside it. Both
spellings together are a duplicate-field error. For CPU speed, use
[`[cpu]`](configuration.md#id-cpu); for running faster than
wall-clock, use the [warp settings](ui.md#warp-settings).

An `invalid escape sequence` error on Windows often means a path contains
backslashes inside TOML double quotes. Use literal single quotes, for example
`rom = 'C:\Kickstarts\KICK31.ROM'`, or forward slashes. See
[Paths on Windows](configuration.md#paths-on-windows).

If a different machine starts than expected, check which configuration was
loaded. The order is explicit `--config`, `./copperline.toml`, a config beside
the executable, then the launcher's saved default. `--factory` bypasses the
last two; it does **not** bypass `--config` or `./copperline.toml`. See the
[configuration reference](configuration.md) for platform-specific locations
and [relative media paths](configuration.md#relative-media-paths).

## Isolate a boot, display or audio problem

Run from a folder without a `copperline.toml` to capture the bundled AROS
boot on a known A500 profile:

```sh
copperline --factory --model A500 --noaudio \
  --screenshot-after 10 boot.png
```

This exits after ten **emulated** seconds and needs no window or audio device.
The resulting PNG checks the core's output independently of host presentation.
Then repeat with your explicit `--config` and media. Keep the failing config
and a minimal timed input script when reporting a difference. The
[headless guide](headless.md) describes scheduled inputs and captures.

- **No window / no Vulkan adapter on Linux:** install a working Vulkan driver
  or the Mesa software driver described in
  [Linux installation](getting-started.md#vulkan-is-required-on-linux).
  A successful headless capture does not verify the window's GPU backend.
- **Audio crackles or the session falls behind:** use a release build, restore
  normal warp settings, and compare with the default fixed CPU clock and JIT
  disabled. The [performance overlay](ui.md#performance-overlay) separates
  emulation cost from presentation. A silent headless run does not measure
  live audio delivery.
- **Boot media is absent or a disk does not boot:** check the resolved path,
  the machine profile, and whether the selected drive is enabled. CDTV/CD32
  profiles have no floppy drive by default. WHDLoad additionally needs its
  [support archives and Kickstart setup](whdload.md).

## A save state will not load

Save states have a container version and separate subsystem chunk versions;
these numbers are distinct from the Copperline release version.

| State format | Current reader behaviour |
|---|---|
| Flat states from Copperline 0.19 and earlier (container 1-80) | Refused; boot the guest again and create a new state. |
| Chunked container 81 | Accepted without the later thumbnail/metadata card. |
| Chunked container 82 | Current container. Required chunks and their versions still need to be supported by the reader. |
| Older subsystem chunk | Upgraded only when a migration exists. A missing migration fails with the chunk name and versions. |
| Newer subsystem chunk | Refused. Use the build that wrote it or a compatible newer build. |

For example, 1.1.0 reads old CIA version-1 chunks by migration and writes
CIA version 2. That does not make the resulting states readable by 1.0.x.
Forward compatibility is not guaranteed merely because the container version
is the same. Preserve a copy before trying a state in another build.

The post-1.1.0 external-drive ID fix is another one-way boundary even though
the container and chunk versions did not change: current maintenance builds
read 1.1.0 states, but 1.1.0 cannot read their states. Its floppy decoder
requires `external_id_mode`, which newer states no longer write. An error
naming that missing field calls for a newer reader or a new snapshot made by
the older executable.

The reader must also have support for every expansion board in the state.
A file that loads in one feature build may fail in another. The
[format reference](../internals/savestate.md#versioning) describes the checks.
`copperline-ctl state-info FILE.clstate` inspects metadata without launching
an emulator; it does not prove every machine chunk can be restored.

A state restores its saved machine, including RAM, ROM and chipset, even
when the current config selects something else. It is **not a disk backup**:
hard-drive and CD images reopen by path, and later writes to a host disk or
mounted folder are not undone. Keep the original backing images available.
Physical drives, host networking and other live services also prevent a full
timeline rewind. The host paths and mounts a state names are opened on load
whatever the current config says, so only load states from a source you
would accept a config file from. See [save states](ui.md#save-states) and the
[determinism boundaries](../internals/savestate.md#determinism-boundaries).

## Known emulation limits

These limits describe hardware behaviour, rather than a compatibility promise
for individual programs. The linked subsystem chapters give the details.

| Hardware or mode | Limit |
|---|---|
| 68020/030/040/060 timing | The later CPUs do not have a complete pipeline/overlap timing model. Instruction correctness and measured bus timing do not imply cycle-exact accelerator execution. [CPU timing](../internals/cpu.md#id-68020-timing). |
| JIT and CPU `speed = "fastest"` | JIT batches execution; `fastest` also adapts the CPU clock to spare host time. Use the default fixed clock and JIT disabled for precise timing investigations. [CPU execution modes](../internals/cpu.md#the-batch-jit-execution-mode). |
| External 68851 MMU on a 68020 | Not modelled. The on-chip MMUs on the full 68030/040/060 are separate supported devices; EC variants omit them. [CPU configuration](configuration.md#id-cpu). |
| A2024 / UHRES dual-scan output | Not emulated. [Chipset gaps](../internals/chipset.md#known-aga-ecs-gaps-and-non-goals). |
| External genlock | Physical genlock input/output is not emulated. This does not remove the CD32 FMV module's internal digital video compositing. [Chipset gaps](../internals/chipset.md#known-aga-ecs-gaps-and-non-goals). |
| AGA display fetch | Sub-unit DDF stop effects beyond whole fetch-unit completion are not modelled. [Chipset gaps](../internals/chipset.md#known-aga-ecs-gaps-and-non-goals). |
| CDTV CD+G | Subcode payload delivery is not implemented; CD audio playback is separate. [CDTV](../internals/peripherals.md#cdtv-cdtv-rs-cdrom-rs). |
| Toccata recording | The capture FIFO is a stub: reads return zero and no record half-full interrupt fires. Playback is implemented. [Toccata scope](../internals/toccata.md#out-of-scope). |

Browser and libretro builds also have host integration limits; see the
[browser guide](browser.md) and [libretro guide](libretro.md) for the available
features in each frontend.

## Reporting a reproducible problem

Include the Copperline version (and source revision for a development build),
host OS, selected machine profile, config, exact command and timed inputs,
and the first emulated time at which the result differs. Attach a screenshot
and the relevant stderr log. A panic also writes `copperline-crash.txt`; see
[logging and crash reports](getting-started.md#logging-and-crash-reports).
Describe the ROM/media and their checksums without uploading copyrighted
images. State whether the problem reproduces with JIT disabled and whether
it occurs headlessly, in the window, or both.
