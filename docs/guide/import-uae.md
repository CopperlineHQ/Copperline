# Importing UAE configurations

The `copperline-import-uae` utility converts WinUAE, Amiberry, and FS-UAE
configuration files into Copperline's TOML format:

```sh
copperline-import-uae --from amiberry --in ~/Amiberry/Configurations/a1200.uae \
  --out a1200.toml
copperline --config a1200.toml
```

## Supported formats

`--from`, `--in`, and `--out` are all required. `--from` accepts three source
formats:
- `winuae`: WinUAE configuration files (`.uae`).
- `amiberry`: Amiberry configuration files (`.uae`).
- `fsuae`: FS-UAE configuration files (`.fs-uae`).

## Conversion output and annotations

Nothing in the source file is silently dropped. The generated TOML file is
annotated:

- **Approximated settings:** A setting that changed shape on the way in (for
  example a memory size or a controller mapping) carries an inline comment
  explaining what it became.
- **Unmapped settings:** A comment block at the end of the file lists every
  source setting that did not translate cleanly, split into approximated
  settings and settings with no Copperline equivalent (such as host GUI
  options or UAE-only devices), plus any line that is not a `key=value`
  setting.
- **Validation:** The converter checks the result the same way Copperline
  loads a configuration, and still writes the file if that check fails,
  printing what to fix by hand. Validation opens the media the configuration
  names, so a ROM or disk image that exists only on the machine that wrote the
  source file stops the check there; the converter says so in a note rather
  than as an error. It prints a summary of how many settings were approximated
  or not translated.

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
| SCSI controllers (A2091, A4091, A3000) | `[scsi] controller` |
| CD image (`cdimage0`) | `[cd] image` |
| RTG board (Picasso II/II+, Graffity Z2/Z3 only) | `[rtg]` |
| Toccata, RTC, `uae_hide_autoconfig` | `[toccata]`, `[machine] rtc`, `identify` |
| Serial port (`TCP://host:port`, or a host device) | `[serial]` |
| Audio channel modes and filters | `[audio]` settings |
| Input port assignments | `[input]` port configurations |
| Fullscreen and status bar | `[display]` |

The table describes the WinUAE/Amiberry mapper. The FS-UAE mapper covers the
model, CPU, memory, chipset and video standard, Kickstart, floppy drives, and
hard drives; everything else in an FS-UAE file is listed as not translated.

## Important differences

- **Virtual controller hardfiles map onto `[copperhf]`:** WinUAE/Amiberry's `uaeN`
  controller and FS-UAE's unnamed default controller both mean the same thing --
  the emulator's own `uaehf.device`, a virtual hardfile board with no
  real-hardware counterpart. Copperline's `[copperhf]` (`copperhf.device`) is its
  direct equivalent, so these drives translate exactly: the trailing digit in `uaeN`
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
- **Read-only hardfiles:** IDE, SCSI, and `copperhf` hardfile images are opened
  read-write, so the converter cannot preserve UAE's read-only setting, and the
  generated file says so. Removing host write permission makes the image fail
  to open instead. Use a disposable copy when the original must be preserved.
  `[[filesys]]` directory mounts (whose read-only flag is carried over) and
  physical `[[host_disk]]` attachments have their own read-only settings.
- **Relative paths:** Relative paths in UAE configurations are kept as written.
  You may need to update them to match your current working directory.
- **Host-specific settings:** Display window dimensions, host vsync options, and host
  keybindings are not imported.

## Getting the converter

Every release package includes `copperline-import-uae`;
[Command-line tools](getting-started.md#command-line-tools) lists where each
package puts it. A default Cargo build also builds it (the `import-uae-bin`
feature). To build just this binary:

```sh
cargo build --release --bin copperline-import-uae
```
