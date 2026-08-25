# Importing a WinUAE, Amiberry or FS-UAE configuration

`copperline-import-uae` converts an existing emulator configuration into
Copperline's own TOML, so a machine you already have set up somewhere else
becomes a starting point rather than something to rebuild by hand:

```sh
copperline-import-uae --from amiberry --in ~/Amiberry/Configurations/a1200.uae \
  --out a1200.toml
copperline --config a1200.toml
```

`--from` takes `winuae`, `amiberry` or `fsuae`. WinUAE and Amiberry share
one flat `key=value` format and one converter; FS-UAE's `Config.fs-uae` is
a different vocabulary with its own.

The tool is a converter, not a compatibility layer. It translates what
Copperline has an equivalent for, and **says so, in the file it writes,
about everything else** -- nothing from the source is dropped silently. The
result always parses, and is validated exactly the way `copperline
--config` would load it before being written, so a mistake in the mapping
surfaces here rather than at the next boot. That validation opens the media
the config names, so importing on a machine that does not hold the source's
images stops at the first missing file and says so as a `note:` -- copy the
images across (or fix the paths) and re-run to have the rest checked.

## Reading the result

Two kinds of remark end up in the generated file. Settings that translated
only approximately are commented at the line they produced:

```toml
[memory]
# from chipmem_size=8 as 512K blocks; source specified 4M of chip RAM,
# clamped to Copperline's 2M ceiling
chip = "2M"
```

Anything that did not translate is listed in a comment block at the end,
split by why:

```toml
# --- Settings from the source config that were not translated ---
# Approximated (semantics differ -- verify by hand):
#   hardfile2 = rw,DH0:AmigaVision.hdf,0,0,0,512,0,,uae0  (WinUAE's own `uae`
#     virtual hard-drive controller has no Copperline equivalent; ...)
# Unsupported (no Copperline equivalent):
#   gfx_vsyncmode = 0  (not yet recognized by this converter ...)
```

The split matters. **Approximated** means a setting reached the machine but
not exactly as written, so it is worth a look. **Unsupported** means nothing
was emitted for it at all. Some of those are named individually with a
reason -- `cpu_speed`, `uaeserial`, `sound_volume` and friends are concepts
Copperline genuinely has no knob for, and are called out so they read as
"considered and skipped" rather than "the converter has not got to this
yet".

A remark that belongs to no particular setting -- typically about something
the source config never said -- appears as a `# Note:` line instead.

## What comes across

Machine model, CPU and FPU, chip/slow/fast/Zorro III/motherboard RAM,
chipset revision and PAL/NTSC, Kickstart ROM, floppy drives (including
FS-UAE's multi-disk swap list, which becomes a
[`[floppy.df0] paths`](configuration.md) playlist), hard drives and
directory mounts, CD images, the RTC, audio channel mode and filter,
joystick port devices, the `[lide]`, `[scsi]` and `[toccata]` boards, and
the launcher's own file-dialog directories.

Storage is where the two formats differ most, and where the conversion
does the most work:

| Source | Becomes |
|---|---|
| `filesystem2=rw,DH0:Workbench:/path,0` | a `[[filesys]]` host-directory mount |
| `hardfile2=...,,ide1_alfapower` | `[lide] drive1`, on the AT-Bus 2008 personality |
| `hardfile2=...,,scsi3` | `[scsi] unit3` |
| `hardfile2=...,,ide0` / `ide1` | `[ide] master` / `slave` |
| `hardfile2=...,,uae0` | the first free `[ide]` slot (see below) |
| FS-UAE `hard_drive_0` + `_controller` | `[ide]` or `[scsi]`, per the controller |
| FS-UAE `floppy_image_0..N` | a `[floppy.df0] paths` swap list |

## Things worth checking afterwards

**A `uae` hardfile is not a like-for-like move.** WinUAE's `uae` virtual
hard-drive controller is served by the emulator's own driver and sidesteps
the guest's storage stack. Copperline's `[ide]` is the machine's real
Gayle/A4000 port, so the image goes through the Kickstart ROM's
`scsi.device` and inherits its size limits -- 3.1 and earlier cannot
address past about 4GB, and older filesystems stop sooner. An image that
was fine in WinUAE can fail to mount here. AmigaVision's ~10GB drive is
exactly this case. Where the converter can find the image it says how big
it is; either way it points at [`[lide]`](configuration.md), whose modern
`lide.device` autoboots large drives under any Kickstart including 1.3.

**A drive's unit number is its port, not its position in the file.** WinUAE
numbers the built-in IDE 0-3 (two channels); `ide0` and `ide1` are the
Amiga's own port as master and slave, which is all `[ide]` has, so a
hardfile on unit 2 or 3 is flagged for `[scsi]` or `[lide]` rather than
quietly taking a port that is already spoken for. The same number picks the
`[lide]` slot for a board drive.

**Read-only hardfiles come across writable.** Copperline's hard-drive ports
have no per-image write protection, so a `ro` `hardfile2` is listed as
unsupported and the guest can write to the image; protect the file at the
host filesystem if that matters. Directory mounts are unaffected --
`[[filesys]]` has its own `readonly`.

**Relative paths are not rewritten.** Emulators resolve them against their
own install directories, which Copperline does not share, so a config full
of bare filenames will need absolute paths (or to be run from the right
working directory). FS-UAE's `$HOME`/`$BASE`/`$CONFIG` variables are
flagged for the same reason: FS-UAE expands them, Copperline takes paths
literally.

**A machine the source never named** falls back to Copperline's default,
which is a stock A500. The converter does not write a `[machine] profile`
it was not told, but it does leave a `# Note:` saying which machine you
ended up with and how to change it.

**Nothing about the host is translated** -- window size, scaling filters,
vsync mode, JIT tuning, input device assignments and hotkeys are all
either Copperline's own settings or things it does not model. Set those up
once on this side.

## Building it

`copperline-import-uae` builds with Copperline itself as part of a normal
release build, and installs alongside it from the Homebrew formula. To
build only the converter:

```sh
cargo build --release --bin copperline-import-uae
```

It is gated behind the `import-uae-bin` feature, on by default; a build
that does not want it can turn the feature off.
