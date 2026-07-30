# FloppyBridge (vendored)

Rob Smith's FloppyBridge, which is what lets a Copperline floppy bay drive a
real 3.5" drive over a DrawBridge, a Greaseweazle, or a Supercard Pro.

- Upstream: <https://github.com/RobSmithDev/FloppyDriveBridge>
- Vendored from commit `710fa15cb200303f8c4bde1c931786175f301a68` (2024-08-07),
  which reports itself as version 1.6.
- Licence: MPL-2.0 or GPL-2.0-or-later (see `MPL2.txt` and `GPL2.txt`), with
  the ABI headers under the Unlicense. Both are compatible with Copperline's
  GPL-3.0-or-later.

These sources are compiled straight into the emulator by `build.rs`, so
`cargo build` produces a binary that drives a real drive with nothing to
install and nothing to download. `src/floppybridge/` on the Rust side calls
into them directly.

## Updating

Copy `floppybridge/*` and `windows/FloppyBridge.{cpp,h}` plus `windows/resource.h`
from a newer upstream checkout into `src/`, then re-apply the changes below and
update the commit recorded above. They are small, confined to six files --
`ArduinoFloppyBridge.cpp`, `CommonBridgeTemplate.cpp`, `FloppyBridge.cpp`,
`GreaseWeazleBridge.cpp`, `SerialIO.cpp` and `ftdi.cpp`. All but one are build
or platform differences rather than changes in behaviour; the exception is the
auto-cache write fix in `CommonBridgeTemplate.cpp` below, which corrects a
real-disk corruption bug and should be dropped once upstream carries an
equivalent. Every other vendored file is byte-identical to the commit above.

`build.rs` picks the files it compiles by name; a release that adds or renames
a source file needs that list updated too, and will say so by failing to link.

## Local changes

Upstream builds these as a DLL from a Visual Studio project, which defines
`UNICODE` and compiles WinUAE's configuration dialogs in. Copperline compiles
them with `cc` into a non-UNICODE binary and drives them from its own launcher,
so a handful of Windows-only lines need adjusting. Every one is marked
`COPPERLINE:` in the source, or listed here.

`build.rs` also passes what upstream's own Visual Studio project sets and `cc`
does not: `NOMINMAX`, without which `windows.h`'s `min`/`max` macros turn every
`std::min(` in the sources into `std::(`, and `/EHsc`, without which MSVC
compiles the standard library's containers and threads with no unwinding and
warns that any exception would terminate. The one project setting deliberately
not matched is its Unicode character set -- see below.

`FLOPPYBRIDGE_NO_GUI`, defined by `build.rs`, excludes what Copperline does not
use: the Windows-only configuration dialogs and the update check, which belong
to upstream's DLL front-end and which Copperline never calls. Bridges are
configured through Copperline's own launcher and config file on every
platform.

### `FloppyBridge.cpp` -- the DLL front-end

On Windows this file also carries the native configuration dialogs, their
resource script, and a DnsQuery update check. Compiling them would drag in two
more source files and a `.rc` needing a resource compiler, for code that would
never run -- Copperline always asks for `BRIDGE_About` with update checking
off.

1. the `bridgeProfileListEditor.h` / `bridgeProfileEditor.h` / `WinDNS.h`
   includes and the `Dnsapi.lib` pragma;
2. `BridgeProfileListEditor::shouldAutoCheckForUpdates()` in `handleAbout`;
3. the DnsQuery update check itself, which on Windows now compiles to nothing
   rather than falling through to the Unix path below it;
4. the `BRIDGE_ShowConfigDialog` / profile-list dialog entry points;
5. `FindWindow` with `L""` literals becomes `FindWindowA` with narrow ones.

### `SerialIO.cpp` -- the blocking open on Linux

Opening a tty blocks until carrier detect is asserted, unless `CLOCAL` is
already set on the device. The sources set `CLOCAL` immediately after opening,
which is too late, so an interface that never raises DCD -- a Greaseweazle over
USB CDC among them -- hangs in `open()` for good, with nothing to report
because the call does not return. Upstream sidesteps this on macOS by opening
with `O_NDELAY` and nowhere else.

Copperline opens non-blocking everywhere and leaves the flag set, which is
exactly what upstream already does on macOS. Clearing it after the open looks
tidier -- reads would then wait on the `VMIN`/`VTIME` timeout rather than
returning empty -- but that timeout is configured by `configurePort()`, which
the interfaces call only *after* the open returns. In between, a blocking
descriptor carries the default `VMIN=1`/`VTIME=0`: wait for a byte, forever. The
handshake reads in that window, so a device that answers slowly wedges the open
instead of failing it.

### `CommonBridgeTemplate.cpp` -- a write while auto-cache holds the head

Background caching (`handleBackgroundCaching`) seeks the physical head and
selects surfaces without updating `m_actualCurrentCylinder` or
`m_actualFloppySide`; it raises `m_autocacheModifiedCurrentCylinder` so the
next reader restores the head first. `handleBackgroundDiskRead` checks the
flag; the `writeMFMData` handler did not, so with auto-cache enabled a queued
write could find the bookkeeping already "on" its target cylinder, skip the
seek, and lay the track down wherever the cacher had left the head --
corrupting an unrelated track of a real disk. The handler now restores the
cylinder and surface whenever the flag is up, exactly as the read path does.
This is a behavioural fix, not a build difference, and is worth carrying
upstream.

### Wide literals passed to TCHAR-generic Win32 calls

Upstream's project builds with `CharacterSet: Unicode`, so its `L""` literals
match the `*W` variants these calls resolve to. Copperline does not define
`UNICODE`, which would make the opposite mismatch possible wherever the sources
pass narrow strings, so each of these becomes an explicit `*A` call with a
narrow literal instead:

- `ArduinoFloppyBridge.cpp`: `RegQueryValueEx` / `RegSetValueEx` (2 calls)
- `GreaseWeazleBridge.cpp`: the same pair, twice over (4 calls)
- `ftdi.cpp`: `LoadLibrary("FTD2XX.DLL")`
- `SerialIO.cpp`: `SetupDiGetDeviceProperty` (2 calls). This one has no ANSI
  variant at all -- the SDK declares only `SetupDiGetDevicePropertyW` and
  defines the plain name as a macro for it under `UNICODE` -- so without that
  the identifier simply does not exist. The data it reads is wide either way.

`ADFBridge.cpp` and `floppybridge_lib.cpp` have more of these, and more `TCHAR`
use besides, but neither is compiled: the first backs a bridge onto an ADF
file, which Copperline's own image path already does, and the second is the
client-side loader for the shared build, which linking directly replaces.
