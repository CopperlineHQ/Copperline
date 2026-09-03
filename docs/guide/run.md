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
(For configurations booting from media rather than `--run`, warp boot is also available via
`--warp-boot` / `--warp-until`; see [Configuration](configuration.md).)
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
  the automatic warp phase and every programmatic warp hold at once, returning
  to real-time execution.
- **Programmatic warp:** A control-protocol client (`warp.set {"on": true}`, see
  [Control Protocol](../debugger/control.md)), a GDB client (`monitor warp on`,
  see [GDB](../debugger/gdb.md)), and the guest program itself (`warpmode(1)` /
  `warpmode(0)` through the [uaelib trap](#uaelib-trap) below, e.g. during heavy
  computation or asset loading) each hold warp independently. All mute live
  audio while engaged; `warp.set {"on": false}`, `monitor warp off`, or
  `warpmode(0)` release only that holder, real time returns when the last hold
  goes, and the shortcut returns to real time regardless.
- **Physical floppy drives:** If a physical floppy drive (FluxBridge) is attached, warp
  mode is disabled to match the physical drive rate.
- **Headless mode:** Headless capture runs (`--screenshot-after`, `--dump-frames`) run
  unthrottled by default and work with `--run`.

## Debugging

When launched with `--gdb`, Copperline halts execution at the entry point of the loaded
program before the first instruction runs:

```sh
copperline --run build/hello --gdb :2345
m68k-amiga-elf-gdb hello.elf -ex "target remote :2345" -ex continue
```

When halted, the GDB stub reports the base address of the first hunk for symbol loading
via `add-symbol-file`. The GDB monitor command `monitor segments` lists all hunk addresses.

The [control protocol](../debugger/control.md) gets the same break-at-entry: with
`--run`, `--control` and `--control-gui` arm a one-shot `loadseg` stop for the
program, and `segments.list` reports its hunk addresses at that stop. Scripts can
also arm their own `loadseg` breakpoint to catch every load.

For an IDE, the [Debug Adapter Protocol](../debugger/dap.md) server does all of
this by itself: a VS Code (or nvim-dap) launch configuration naming the program
starts Copperline, stops at the entry point, and debugs by source line from the
executable's own debug information.

(uaelib-trap)=
### WinUAE-compatible `uaelib` trap

WinUAE's boot ROM provides guest programs with a lightweight service interface,
the "uaelib" trap at `$F0FF60`. Cross-compiler toolchains and templates (such
as `vscode-amiga-debug`) use this trap for helpers like `warpmode()`, `KPrintF()`,
and `debug_register_*()`. Copperline implements the same ABI at the same address,
allowing code written for that template to work unmodified.

Guest code checks the instruction word at `$F0FF60` (`0x4EB9` for a `JSR`, or
WinUAE's A-line `0xA00E`) and invokes the address as a C function, passing the
function index as the first stack parameter and receiving the return value in D0:

```c
long (*UaeConf)(long fn, int index, const char *param, int len, char *out, int outlen)
    = (long (*)(long, int, const char *, int, char *, int))0xf0ff60;
if (*(UWORD *)UaeConf == 0x4eb9 || *(UWORD *)UaeConf == 0xa00e) {
    char out;
    UaeConf(82, -1, "warp true", 0, &out, 1);   /* warpmode(1) */
}
```

| Function | WinUAE meaning | Copperline |
|---|---|---|
| 82 | `uae-configuration`-style `"key value"` line | `warp true` / `warp false` (also `yes` / `no`) toggles warp mode. Parameters like `cpu_speed` and `*_cycle_exact` are accepted as no-ops. Returns 0. |
| 86 | Debug log string | Printed to the host console as `DBG: <text>` (shared with serial output), streamed to control-protocol `debug` subscribers as `event.debug`, and mirrored into the debugger console. Returns 1. |
| 88 | `debug_cmd` multiplexer | `debug_register_bitmap` / `_palette` / `_copperlist` and `debug_unregister` register guest assets, viewable in the Frame Analyzer (Resources and Memory tabs), searchable via `palette.dump` / `copper.list`, and listed with the console `DBGRES` command; `debug_start_idle` / `debug_stop_idle` report guest idle time in `debug.idle` and `event.frame.guest_idle_cck`. Overlay drawing (`debug_clear` / `debug_rect` / `debug_filled_rect` / `debug_text` on a 768x576 virtual canvas) renders on screen in the window (excluded from captures and recordings); `debug_load` / `debug_save` are accepted no-ops (`debug_load` returns 0). |
| others | version, disks, RTG, ... | Return 0 with no side effects. |

- Enabled by default; set `[emulation] uaelib = false` to leave `$F0FF60` unmapped.
- A CDTV extended ROM occupies `$F00000` and covers this address space.
- Without the trap, `KPrintF` falls back to Exec `RawPutChar` and emits over the serial port.
- Guest-initiated warp mutes live audio; `warpmode(0)` releases the guest's hold, and `Cmd+W` / `Alt+W` ends every hold.
- The return latch is shared: uaelib calls from interrupt handlers between a main-thread doorbell write and result read may overwrite D0.

## Kickstart compatibility

The generated `Startup-Sequence` relies on shell commands (`CD`, `FailAt`) present in
Kickstart 2.0 and newer (including the bundled AROS ROM).

On Kickstart 1.3, these commands emit error messages but the binary is still executed;
however, the working directory remains `SYS:`, meaning relative asset paths may fail to
resolve. Kickstart 1.2 lacks filesystem autoconfig support and cannot boot host-directory
volumes.
