# Warp launch

`--run` boots straight into an ordinary Amiga executable on the host --
no disk image, no Workbench, no support assets -- and makes the boot feel
almost instant. It is aimed at development: build a program with an Amiga
cross-toolchain, then

```sh
copperline --run build/hello
copperline --run build/hello --run-args "-level 2"
```

and the program is running seconds later. Because the program's own
directory is mounted live, the edit-build-run loop needs nothing between
iterations: rebuild and launch again.

## What it does

Two host directories are mounted through the services board (the same
live mounts as [`[[filesys]]`](configuration.md#filesys-mounts)):

- a generated boot volume `RunBoot:` (boot priority 6, beating DF0:),
  containing only an `S/Startup-Sequence` that makes the program's
  directory current and runs the program by absolute path -- regenerated
  on every launch;
- the program's parent directory as `RunProg:`, writable, mounted in
  place: the guest reads the executable the host just built, and
  anything it writes lands next to it on the host.

Nothing else is derived. The machine is whatever the configuration and
CLI flags say -- by default the bundled AROS ROM on the default machine --
so `--model`, `--cpu`, `--fast`, a Kickstart path, and every other flag
layer as usual:

```sh
copperline --model A1200 --fast 8M KICK31.ROM --run build/demo
```

## The warp

A windowed `--run` session boots at warp speed: emulation runs unpaced
(still cycle-exact -- warp changes pacing, never behavior) with live
audio muted, and the boot flies by with the status bar showing warp
engaged. The moment the guest OS loads the target program -- observed
via the LoadSeg tracker, before its first instruction -- pacing and
audio snap back to real time. A cold boot that takes tens of emulated
seconds passes in wall-clock seconds.

Details worth knowing:

- If the program is never loaded (a crash mid-boot, a name mismatch),
  the warp gives up after 60 emulated seconds and drops to real time so
  the failed boot is watchable.
- Toggling warp manually cancels the launch phase: one press means
  normal-speed, audible emulation.
- A machine with a physical floppy drive attached (FluxBridge) refuses
  to run unpaced; `--run` still works, at normal speed.
- Headless capture runs (`--screenshot-after`, `--dump-frames`) are
  already unthrottled end to end; `--run` composes with them without any
  pacing change.

## Debugging the program

With `--gdb`, the session stops the moment the guest loads the program,
before its first instruction -- the natural "break at entry" for source
debugging (see [GDB remote debugging](../debugger/gdb.md)):

```sh
copperline --run build/hello --gdb :2345
m68k-amiga-elf-gdb hello.elf -ex "target remote :2345" -ex continue
```

The stop prints the first hunk's address for `add-symbol-file`, and
`monitor segments` lists every hunk.

Scripts using the [control protocol](../debugger/control.md) can wait on
the same moment with `break.add {"kind": "loadseg", "name": "hello"}`.

## Kickstart notes

The generated Startup-Sequence uses `CD` and `FailAt`, internal shell
commands from Kickstart 2.0 on (the bundled AROS ROM included).
Kickstart 1.3 prints an unknown-command line for each but still runs the
program -- only its current directory stays `SYS:`, so a program loading
assets by relative path wants KS 2.0+ or AROS. Kickstart 1.2 lacks the
expansion-ROM hook entirely, so host-directory volumes cannot boot there
(see [host directories as volumes](configuration.md#filesys-mounts)).
