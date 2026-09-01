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
  the automatic warp phase, and any programmatic warp, and returns to real-time
  execution.
- **Programmatic warp:** A control-protocol client can engage warp at any time
  with `warp.set {"on": true}` (see [Control Protocol](../debugger/control.md)),
  and the guest program itself can call `warpmode(1)` / `warpmode(0)` through
  the [uaelib trap](#uaelib-trap) below -- around a slow loading or
  precalculation phase, say. Both mute live audio while engaged, exactly like
  the automatic phase; `warp.set {"on": false}`, `warpmode(0)`, or the
  shortcut return to real time.
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

(uaelib-trap)=
### WinUAE-compatible `uaelib` trap

WinUAE's boot ROM offers guest programs a small service entry point, the
"uaelib" trap at `$F0FF60`, and cross-compiler toolchains use it: the
vscode-amiga-debug template's `warpmode()`, `KPrintF()` and
`debug_register_*()` helpers all call it. Copperline answers the same ABI at
the same address, so code written for that template works unchanged.

The guest tests the first word at `$F0FF60` (`0x4EB9`, a `JSR`; the same test
also accepts WinUAE's A-line form `0xA00E`) and calls the address like a C
function, with the function number as the first stack argument; the result
comes back in D0:

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
| 82 | `uae-configuration`-style `"key value"` line | `warp true` / `warp false` (also `yes` / `no`) engages or releases warp. The template's `cpu_speed` and `*_cycle_exact` keys are accepted and ignored: the core is always cycle-exact. Returns 0, as WinUAE does. |
| 86 | Debug log string | Printed on the host console as `DBG: <text>` (the same channel as serial output) and streamed to control-protocol `debug` subscribers as `event.debug`. Returns 1. |
| 88 | `debug_cmd` multiplexer | `debug_register_bitmap` / `_palette` / `_copperlist` and `debug_unregister` are recorded and served by `debug.resources`; `debug_start_idle` / `debug_stop_idle` feed `debug.idle` and the `guest_idle_cck` field of `event.frame`. Overlay drawing and `debug_load` / `debug_save` are accepted no-ops (`debug_load` returns 0, not found). |
| others | version, disks, RTG, ... | Return 0 with no side effect; Copperline does not report a WinUAE version. |

- The trap is fitted by default; `[emulation] uaelib = false` leaves `$F0FF60`
  floating for a machine that must have nothing there.
- A CDTV's extended ROM occupies `$F00000` and hides the trap, as WinUAE's own
  relocation does on that machine.
- Without the trap, the template's `KPrintF` falls back to exec's
  `RawPutChar`, which reaches the host through the serial port: `KPrintF`
  output is visible either way.
- A warp the guest engages mutes live audio; `Cmd+W` / `Alt+W` ends it.
- The result latch is shared: a uaelib call made from an interrupt handler
  between another call's doorbell and its result read clobbers that call's
  D0.

## Kickstart compatibility

The generated `Startup-Sequence` relies on shell commands (`CD`, `FailAt`) present in
Kickstart 2.0 and newer (including the bundled AROS ROM).

On Kickstart 1.3, these commands emit error messages but the binary is still executed;
however, the working directory remains `SYS:`, meaning relative asset paths may fail to
resolve. Kickstart 1.2 lacks filesystem autoconfig support and cannot boot host-directory
volumes.
