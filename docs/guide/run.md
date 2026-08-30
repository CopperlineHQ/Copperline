# Direct executable launching (`--run`)

The `--run` flag allows you to boot Copperline directly into an Amiga executable
located on your host filesystem without preparing a disk image or Workbench installation.
This is particularly useful when developing with an Amiga cross-compiler toolchain:

```sh
copperline --run build/hello
copperline --run build/hello --run-args "-level 2"
```

## How it works

When `--run` is used, Copperline mounts two virtual filesystem volumes using the
host filesystem interface:

1. **`RunBoot:`** (Boot priority 6) -- A dynamically generated boot volume containing
   an `S/Startup-Sequence` that sets the current directory, launches the specified
   executable, and records a completion marker when the program exits. This volume
   is created in a per-process temporary staging directory.
2. **`RunProg:`** (Read/Write) -- The host directory containing the target executable.
   The guest loads the binary directly from this volume, and any output files written
   by the program are saved to the same host directory.

Other machine settings are configured normally via configuration files or CLI flags.
By default, the bundled AROS Kickstart replacement is used on the standard machine profile:

```sh
copperline --model A1200 --fast 8M KICK31.ROM --run build/demo
```

## Fast-forward boot (Warp mode)

In interactive windowed sessions, `--run` automatically enables warp mode during boot.
(For a configuration that boots from its own media rather than through
`--run`, the same idea is available as `--warp-boot` / `--warp-until`; see
[Configuration](configuration.md).)
The emulator runs unthrottled with audio muted until the guest OS loads the executable
(tracked at the `LoadSeg` call before executing the first instruction). Once loaded,
emulation and audio immediately return to normal real-time playback.

Additional operational notes:

- **Early termination:** If the program completes execution quickly, the `Startup-Sequence`
  detects exit and disables warp mode.
- **Boot timeouts:** If the program fails to load within 60 emulated seconds (for example,
  due to a crash during OS initialization), warp mode disengages so the system state can
  be inspected.
- **File naming:** Target filenames must use printable ASCII characters without quotes (`"`),
  colons (`:`), or slashes (`/`). Spaces in executable names are supported and quoted automatically.
- **Manual override:** Pressing the warp toggle shortcut (`Cmd+W` / `Alt+W`) cancels
  the automatic warp phase and returns to real-time execution.
- **Physical floppy drives:** If a physical floppy drive (FluxBridge) is attached, warp
  mode is disabled to match the physical drive rate.
- **Headless mode:** Headless capture runs (`--screenshot-after`, `--dump-frames`) run
  unthrottled by default and work seamlessly with `--run`.

## Debugging

When launched with `--gdb`, Copperline halts execution at the entry point of the loaded
program before the first instruction runs:

```sh
copperline --run build/hello --gdb :2345
m68k-amiga-elf-gdb hello.elf -ex "target remote :2345" -ex continue
```

When halted, the GDB stub reports the base address of the first hunk for symbol loading
via `add-symbol-file`. The GDB monitor command `monitor segments` lists all hunk addresses.

Scripts using the [Control Protocol](../debugger/control.md) can also wait for program
load events using a `loadseg` breakpoint.

## Kickstart compatibility

The generated `Startup-Sequence` relies on shell commands (`CD`, `FailAt`) present in
Kickstart 2.0 and newer (including the bundled AROS ROM).

On Kickstart 1.3, these commands emit error messages but the binary is still executed;
however, the working directory remains `SYS:`, meaning relative asset paths may fail to
resolve. Kickstart 1.2 lacks filesystem autoconfig support and cannot boot host-directory
volumes.
