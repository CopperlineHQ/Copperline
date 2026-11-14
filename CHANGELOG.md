# Changelog

User-visible changes and upgrade notes for Copperline. New entries go under
**Unreleased** in the same change as the behaviour they describe. Entries
before this changelog was introduced are selected compatibility milestones;
see the [GitHub releases](https://github.com/CopperlineHQ/Copperline/releases)
for the earlier release notes.

## Unreleased

### Added

- `--terminal` shows and drives the machine in a terminal that speaks the
  Kitty graphics and keyboard protocols (kitty, Ghostty, WezTerm) instead of
  a window, including over SSH with no display server. F12 is the host key;
  `--terminal-fps` caps the frame rate and `--terminal-log` sets the log file
  (see docs/guide/terminal.md). The terminal is given back however the
  session ends, including on SIGTERM, SIGHUP, SIGINT and SIGQUIT (exit status
  128 plus the signal number), and the default log file is always a new,
  owner-only file.
- CD subcode R-W data (CD+G graphics) reaches the CD32 and the CDTV from
  images that stored it: a CloneCD-style `.sub` file beside the image, a cue
  sheet `CDG` track, or a CHD with raw subcode. The CDTV drive now delivers
  subcode bytes at all, behind SCOR, while a disc plays.
- `[[tape]]` puts a SCSI tape drive on the `[scsi]` controller. The
  cartridge is a host directory with one file per tape file, or a single
  host file; the files hold only the data, so `tar` archives are tape files
  as they stand (see docs/guide/configuration.md).

### Changed

- `[cpu] jit` runs the `[copperhf]` device driver and the `[[filesys]]`
  handlers at CPU speed: their boot ROMs are mapped as local memory, like
  Kickstart, instead of costing an expansion-bus access for every opcode.
  Filesystem-heavy work on those volumes takes less than half the emulated
  time it did. The precise core is unchanged.
- The browser, libretro and publisher-kit player frontends are now members
  of the repository's Cargo workspace. They share the root `Cargo.lock` and
  build into the root `target/` directory, so a source-built libretro core
  is at `target/release/` rather than `crates/copperline-libretro/target/release/`.
- `[[filesys]]`, `--run` and WHDLoad host-directory volumes carry a fixed
  date, 1 January 1978, instead of the host time their handler started, so
  identical runs no longer vary with the host clock. A machine with no
  battery clock that boots from a host directory starts its clock at that
  date; fit an RTC, or seed one with `rtc_time` / `--rtc-time`, for another.

### Fixed

- `copperhf.device` implements `AbortIO`. A held `TD_ADDCHANGEINT` is
  released with `IOERR_ABORTED`, so a task that exits without
  `TD_REMCHANGEINT` no longer waits forever, and a read or board-answered
  command still queued on the board is withdrawn. Writes already on their
  way to the medium complete normally.
- A ZIP-wrapped floppy image whose file name and extra field together run
  past 64 KiB no longer panics a build with overflow checks enabled; it is
  refused as release builds already refused it.
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
  configuration.
- A `[[filesys]]` mount holds at most 96 open files and 32 unfinished
  directory scans, refuses negative `Read()`/`Write()` lengths and deletion
  of its own root, and logs an out-of-memory guest buffer once per request
  instead of once per byte.
- A `copperhf.device` read or write whose buffer is not guest RAM fails
  before any host I/O instead of first allocating and reading the full
  request length.
- A TCP serial or modem peer that stops reading no longer freezes the
  emulator: a write that cannot progress for one second drops the link.
  Input the guest has not read yet is limited to 64 KiB per link.
- The browser build stops downloading a linked disk image as soon as it
  passes the 64 MiB limit, including when the server sends no length.
- Save states carry the CPU's interrupt-sample latches and the chip data
  bus value, so a resumed run takes an interrupt that was recognisable at
  the save point on the same instruction as an uninterrupted run.
- The libretro core no longer publishes a chip-RAM pointer during netplay,
  where rollbacks replace the RAM buffers and a cached pointer would dangle.
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
  existing installations, and preserve settings and saves on uninstall.
- The manual identifies its package version and source revision, and includes
  troubleshooting, known limits and compatibility notes.

### Compatibility

- No configuration key is removed or renamed by the changes above. Save-state
  container and subsystem versions are unchanged from 1.1.0, and this build
  reads 1.1.0 states. Compatibility is one-way: 1.1.0 cannot read states saved
  after the external-drive ID fix, because its floppy decoder requires the
  `external_id_mode` field that newer states no longer write.

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
