Copperline - portable Windows build
====================================

Copperline is a cycle-driven Amiga emulator (OCS/ECS/AGA).

Running
-------
This is a portable build: no installation is required. Unzip it anywhere and
run copperline.exe. It needs no administrator rights and no Visual C++
Redistributable (the C runtime is linked statically into the executable).

On first launch Windows SmartScreen may show "Windows protected your PC"
because the executable is not code-signed. Click "More info", then
"Run anyway" to start it.

Boot ROM
--------
With no ROM of your own, Copperline boots the bundled AROS open-source
Kickstart replacement, found in the aros\ folder next to the executable. AROS
is freely redistributable; see aros\LICENSE. To use a real Kickstart instead,
point a config file at it, or load it at runtime from the menu
(Load Kickstart ROM...).

The CD32 profile also fits Copperline's bundled open Full Motion Video ROM
from the fmv\ folder. Set fmv_rom = "" in a config to leave that cartridge
unfitted.

Configuration
-------------
copperline.example.toml is a starting point. Copy it, edit the paths to your
own Kickstart ROM and disk/hard-disk images, and launch with:

    copperline.exe --config your-config.toml

Run "copperline.exe --help" for the full command-line surface.

Portable data
-------------
By default, quick-save slots and host preferences are kept under
%APPDATA%\copperline so they survive replacing or moving this folder. To keep
them inside this folder instead, create an empty file named portable.txt next
to copperline.exe and restart Copperline. Quick-save slots will then be in the
states subfolder; controller mappings, WHDLoad data and other host preferences
will also stay here. Delete portable.txt to return to %APPDATA%; existing files
are not moved automatically.

Bridged Ethernet
----------------
User-mode NAT works with no additional software. Direct bridged Ethernet
requires Npcap from https://npcap.com/; Copperline loads it only when a bridge
backend is selected. Use "copperline.exe --list-net-interfaces" to find the
exact adapter identifier. Wi-Fi bridging is best-effort because many access
points reject frames carrying the Amiga's separate source MAC address.

Troubleshooting
---------------
If Copperline crashes, it writes the crash details to copperline-crash.txt
next to copperline.exe (or, if that folder is read-only, to the current
directory or the system temporary directory). The file is replaced at the
first crash of each run, and any further crashes in the same run are
appended to it; please attach it when reporting a bug at
https://github.com/CopperlineHQ/Copperline/issues

Copperline is licensed under GPL-3.0-or-later; see LICENSE.txt.
