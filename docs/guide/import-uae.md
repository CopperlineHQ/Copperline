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
| Virtual `uae0` hardfiles | First available `[ide]` slot |
| Audio channel modes and filters | `[audio]` settings |
| Input port assignments | `[input]` port configurations |

## Important differences

- **Virtual UAE hardfiles vs. hardware IDE:** WinUAE's `uae` hardfile controller
  bypasses the guest's storage device drivers. Copperline's `[ide]` models the
  hardware Gayle / A4000 IDE controller directly. Large hardfiles (over 4 GB) require
  Kickstart versions and filesystems that support direct SCSI / 64-bit addressing,
  or should be attached via the `[lide]` controller.
- **Read-only hardfiles:** Copperline IDE hardfiles are read-write by default. To
  enforce read-only access, adjust filesystem permissions on the host file.
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
