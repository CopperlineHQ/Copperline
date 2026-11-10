# Security Policy

## Reporting

Report security issues privately. If the repository's **Security** tab offers
**Report a vulnerability**, use it. Otherwise open a public issue asking for a
private contact path and omit exploit details.

Do not attach copyrighted ROMs, disks, hard-disk images, CD images, or other
third-party assets to a report. A minimal config, log excerpt, crash backtrace,
or synthetic reproducer is preferred.

## Trust model

Copperline runs guest software that may be hostile, on behalf of a user who
chose what to run it with. What counts as a security issue follows from which
inputs are trusted with the user's own access to the host and which are not.

Trusted, with the access of the user who supplied them:

- **Configuration files and command-line options.** They name host files,
  directories, devices, serial ports, network backends and plugin modules.
  A `copperline.toml` in the working directory, or beside the executable, is
  loaded without being named; run Copperline from a directory whose contents
  you trust, or pass `--config`.
- **Save states.** A state restores a whole machine, and that includes the
  host bindings it was saved with: the paths of its hard-drive, floppy and CD
  images, its `[[filesys]]` host directories and whether they are writable,
  its plugin modules and their capabilities, and its network backend. Loading
  a state opens those, whatever the running configuration says, and the
  machine in it then runs code of the author's choosing. Load a state from
  someone else only as you would run a configuration file from them.
  Copperline logs a warning when a state names a host resource the running
  session had not opened.
- **Plugin modules** (WebAssembly boards) within the capabilities their
  manifest grants, and **keymap, shader and input-script files** the
  configuration names.

Not trusted:

- **Guest software and media**: Kickstart ROMs, floppy, hard-drive and CD
  images, WHDLoad packages, executables started with `--run`, and anything
  that code does once it is running. A guest must not be able to read or
  write host files outside the directories and images it was given, run code
  on the host, or reach the host network other than through a backend the
  configuration enabled.
- **Netplay peers, spectators and the signalling service.** A peer must not be
  able to do more than take part in the session it joined.
- **Remote content the application fetches** (release metadata, cover art,
  support archives).

A guest is expected to be able to do the following, and they are not
vulnerabilities on their own:

- Change or delete anything inside a writable `[[filesys]]` directory, the
  directory `--run` or `--whdload DIR` mounts (the program's own directory,
  read/write by default), or a writable disk image. A host symlink inside a
  mounted directory is followed wherever it points.
- With a network board set to `net = "nat"`, open outbound connections,
  including to services listening on the host's loopback interface through
  the gateway address.
- Stop the session or change its speed through the documented `uaelib` calls.
- Write bytes to the terminal Copperline was started from, through serial
  output sent to stdout and the `uaelib` debug text call. Only NUL is
  removed, so a guest can emit terminal control sequences; send that output
  to a file when running software you do not trust.
- Consume CPU time, and memory and disk space in proportion to the machine
  and media it was given.

Treat crashes or malformed-image panics as security issues only when they
cross one of these boundaries, such as arbitrary code execution on the host,
writes outside the selected local files, or one netplay participant affecting
another's host.

The remote-control interfaces (`--control`, `--gdb`, the MCP and DAP servers)
listen on the loopback interface unless told otherwise, and the control
protocol requires the session token. They give a connected client full
control of the emulator, including the host paths it opens; binding one to
another interface extends that trust to whoever can reach it.
