# Bare-ROM launch commands

These original GPL-3.0-or-later 68000 hunk executables provide the commands
used by Copperline's generated `--run` Startup-Sequence on Kickstart 1.3.
They require no C runtime, Workbench files, or APIs introduced after V33.
Copperline embeds the checked-in executables and writes them into `RunBoot:C/`.
Newer shells can use their internal commands with the same names.

| Executable | Supported launcher syntax |
| --- | --- |
| `FailAt` | One nonnegative decimal return-code threshold |
| `CD` | One directory path, optionally quoted; changes the actual current lock |
| `Stack` | One decimal byte count, at least 2048; rounded up to longwords |
| `Echo` | One optionally quoted string (or an empty line); writes to `Output()` |
| `Execute` | One script path on a CLI without an enclosing script, as created by `Run` |

These are launcher helpers, not complete replacements for the Workbench
utilities. The parser accepts AmigaDOS `*` escapes and rejects malformed,
oversized, or extra arguments with return code 10. Decimal overflow is
rejected. `CD` rejects files and failed locks without changing directory;
it updates the CLI's display name only when the new name fits the existing
BSTR length, whose allocation capacity is not exposed by the public ABI.
Shell redirection supplies Echo's output file. No ROM identification or
emulator trap is involved.

`Execute` hands the file to the guest CLI, which executes and closes it.
It rejects nested scripts and does not implement parameter substitution;
its purpose is the generated detached-launch script, including on ROMs
such as 3.1 where `Execute` is still an external command.

The bundle does not implement `Run` or `EndCLI`, so
`--run-detach` continues to require Kickstart 2.0+ or AROS. A Kickstart 1.2
machine cannot autoboot Copperline's host-directory volumes.

Rebuild using the shared pinned cross-compiler container:

```sh
make -C guest/run-tools
cargo test --release --test run_boot -- --ignored
cargo test --release --test dap_stdio dap_binds_source_breakpoints_on_kick13 -- --ignored
```

Keep each executable with its source when committing a rebuild. The `probe`
fixture records the CLI failure threshold, default and actual command stack,
background/console state, and raw arguments in a relative `FROM-GUEST` file.
It deliberately returns 20: successful completion-marker output demonstrates
that `FailAt` changed the real CLI state. Local ROMs are read from
`test-assets/` or `COPPERLINE_TEST_ASSETS` and never distributed with the tools.
