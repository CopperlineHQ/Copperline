# Changelog

User-visible changes and upgrade notes for Copperline. New entries go under
**Unreleased** in the same change as the behaviour they describe. Entries
before this changelog was introduced are selected compatibility milestones;
see the [GitHub releases](https://github.com/CopperlineHQ/Copperline/releases)
for the earlier release notes.

## Unreleased

### Fixed

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
- Loading a save state logs a warning for each host file, directory, plugin
  module or network backend the state names that the running session had not
  opened. `SECURITY.md` now states which inputs are trusted: a save state has
  the same access to the host as a configuration file.
- Command-line help is written to stdout. Options accept `--flag=value`,
  misspelled options suggest close matches, and fatal desktop-launch errors
  appear in a native dialog.
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
