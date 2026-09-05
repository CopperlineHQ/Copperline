# Importing UAE configurations

The `copperline-import-uae` utility converts existing WinUAE, Amiberry, and
FS-UAE configurations into Copperline's TOML format:

```sh
copperline-import-uae --from amiberry --in ~/Amiberry/Configurations/a1200.uae \
  --out a1200.toml
copperline --config a1200.toml
```

## Supported formats

The `--from` parameter accepts three source format types:
- `winuae`: WinUAE configuration files (`.uae`).
- `amiberry`: Amiberry configuration files (`.uae`).
- `fsuae`: FS-UAE configuration files (`Config.fs-uae`).

## Conversion output and annotations

The converter validates the output against Copperline's configuration schema and
annotates the generated TOML file:

- **Approximated settings:** When a source option cannot be mapped with identical
  semantics, an inline comment explains how it was approximated (e.g., memory size
  limits or controller mapping).
- **Unsupported settings:** Unmapped settings (such as host-specific GUI options or
  virtual UAE-only devices) are listed in a summary comment block at the end of the file.
- **Media validation:** The tool verifies that referenced ROMs and disk images exist.
  If an image path is missing on the host, a warning note is emitted.

## Mapped settings overview

| UAE setting | Copperline equivalent |
|---|---|
| Model and CPU / FPU | `[machine] profile`, `[cpu]` model and FPU |
| Memory sizes (Chip, Slow, Fast, Z3, Motherboard) | `[memory]` sections |
| Chipset revision, PAL/NTSC | `[chipset]` revision and video |
| Kickstart ROM paths | `rom`, `extended_rom` |
| Floppy drives and disk swap playlists | `[floppy]` and `[floppy.dfN] paths` |
| `filesystem2=` directory mounts | `[[filesys]]` directory mounts |
| Built-in IDE (`ide0`, `ide1`) | `[ide] master`, `slave` |
| Board IDE / SCSI hardfiles (`ide1_alfapower`, `scsiN`) | `[lide]` and `[scsi]` units |
| Virtual `uaeN` hardfiles / FS-UAE's default hard-drive controller | `[copperhf] unitN` |
| Audio channel modes and filters | `[audio]` settings |
| Input port assignments | `[input]` port configurations |

## Important differences

- **Virtual controller hardfiles map onto `[copperhf]`:** WinUAE/Amiberry's `uaeN`
  controller and FS-UAE's unnamed default controller both mean the same thing --
  the emulator's own `uaehf.device`, a zero-cost virtual hardfile board with no
  real-hardware counterpart. Copperline's `[copperhf]` (`copperhf.device`) is the
  exact analogue, so these drives translate exactly: the trailing digit in `uaeN`
  becomes `[copperhf] unitN`, and FS-UAE's default-controller drives take
  successive `[copperhf]` units in the order they appear. `[copperhf]` has seven
  units (0-6); a `uaeN` whose number is out of range or already taken is
  renumbered to the first free unit instead of being dropped, and this is noted
  in the generated file. Only once all seven units are already used does a
  virtual-controller hardfile fall back onto a real port -- WinUAE/Amiberry
  hardfiles fall back to `[ide]` (inheriting the Kickstart IDE port's size
  limits, called out with the image's measured size where the file can be
  found), and FS-UAE hardfiles are flagged for manual placement on `[scsi]` or
  `[lide]`.
- **Read-only hardfiles:** Regular IDE/SCSI/`copperhf` hardfile images are opened
  read-write; the converter cannot preserve UAE's read-only setting. Removing
  host write permission makes the image fail to open. Use a disposable copy
  when the original must be preserved. Live `[[filesys]]` directory mounts and
  physical `[[host_disk]]` attachments have their own read-only settings.
- **Relative paths:** Relative paths in UAE configurations are preserved as written.
  You may need to update paths to match your current working directory.
- **Host-specific settings:** Display window dimensions, host vsync options, and host
  keybindings are not imported.

## Building the converter

The import utility is built by default during standard Cargo builds. To build the
binary standalone:

```sh
cargo build --release --bin copperline-import-uae
```
