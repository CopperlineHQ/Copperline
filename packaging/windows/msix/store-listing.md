# Microsoft Store listing

The text of the Store listing, kept here so it is versioned with the package
and reviewed when it changes. Paste each section into the matching field on
the submission's **Store listing** page; the field limits in the headings are
Partner Center's own.

Two rules run through all of it. Copperline emulates hardware and ships no
copyrighted ROM, game or demo, and the listing must never suggest otherwise
-- a reviewer looking at an emulator is looking for exactly that. And nothing
claims a relationship with the Amiga trademark that does not exist.

## Product name

```
Copperline
```

## Short description (1000 characters)

```
A cycle-driven Commodore Amiga emulator. Copperline models the Amiga custom
chipset, the 680x0 CPU and the chip bus on one clock timeline, so software
that depends on precise hardware timing behaves as it did on the real
machines. It boots out of the box with the bundled open-source AROS Kickstart
replacement, so no ROM file is needed to start.
```

## Description (10,000 characters)

```
Copperline is an emulator for the Commodore Amiga, covering the OCS, ECS and
AGA chipsets and machines from the A1000 to the A4000, including the CDTV
and the CD32.

It is built around the hardware rather than around individual titles. The
68000 CPU, the Agnus, Denise and Paula custom chips, the CIAs, the floppy
controller and the contention on the chip bus all advance on a single clock
timeline, which is what lets demos and games that rely on exact timing behave
the way they did on the real machines.

BOOTS WITH NO ROM

Copperline includes AROS, the open-source Kickstart replacement, and boots it
by default. You can start the emulator and have a working Amiga without
hunting for a ROM image first. Original Commodore Kickstart ROMs from 1.3
through 3.1, the CDTV and CD32 extended ROMs, and DiagROM are all supported
when you have them.

MACHINES

A1000, A500, A500 Plus, A600, A1200, A3000, A4000, CDTV and CD32 profiles,
with OCS, ECS and AGA display paths including HAM, HAM8 and AGA fetch modes.
CPUs from the 68000 to the 68060, with optional FPU and MMU, and configurable
chip, fast and slow RAM.

STORAGE AND MEDIA

Floppy images in ADF, ADZ, DMS, IPF and SCP, and real floppy drives through a
Greaseweazle. IDE and SCSI hard disks, virtual hard disks, CD-ROM images, and
mounting a folder on your PC as an Amiga hard disk. WHDLoad packages boot
directly, with saves kept per game.

SOUND AND DISPLAY

Four-channel Paula audio, RTG graphics cards, host MIDI in and out, and
built-in Roland MT-32 and General MIDI synthesis. The display can be shown
flat or through CRT shaders with authentic tube geometry, and the emulator
records screenshots, animated clips and WAV audio.

TWO-PLAYER NETPLAY

Play with a friend over the internet. The connection is encrypted and runs
directly between the two machines wherever the networks allow it.

FOR DEVELOPERS

A full CPU and chipset debugger with breakpoints, watchpoints, reverse
stepping, live Kickstart and AROS symbols, a frame analyzer that shows DMA
contention cycle by cycle, and VCD waveform export. Source-level debugging
works over GDB or the Debug Adapter Protocol, with a VS Code extension.
Everything the emulator does can be driven from scripts through a JSON-RPC
control protocol, and headless runs are deterministic, so the same inputs
produce the same frames every time.

WHAT IS NOT INCLUDED

Copperline ships no commercial Kickstart ROM, game, demo or disk image, and
does not download any. To run commercial Amiga software you need your own
copies of it.

Copperline is free software under the GNU General Public License v3. The
source, the issue tracker and the documentation are at
https://github.com/CopperlineHQ/Copperline.

Amiga and Commodore are trademarks of their respective owners. Copperline is
an independent project, not affiliated with or endorsed by any trademark
holder.
```

## App features (up to 20 entries, 200 characters each)

```
Boots out of the box with the bundled open-source AROS Kickstart replacement, no ROM file required
OCS, ECS and AGA chipsets, with A1000, A500, A500 Plus, A600, A1200, A3000, A4000, CDTV and CD32 profiles
Cycle-driven Copper, blitter and chip-bus timing, so timing-sensitive demos and games behave correctly
68000 to 68060 CPUs, with optional FPU and MMU, and configurable chip, fast and slow RAM
Floppy images in ADF, ADZ, DMS, IPF and SCP, plus real drives through a Greaseweazle
IDE and SCSI hard disks, CD-ROM images, and mounting a folder on your PC as an Amiga hard disk
WHDLoad game packages boot directly, with per-game saves
Two-player netplay over an encrypted peer-to-peer connection
Save states with thumbnails, and deterministic input recording and replay
CRT display shaders with authentic tube geometry, plus screenshot, GIF and WAV capture
Roland MT-32 and General MIDI synthesis, and host MIDI in and out
Full CPU and chipset debugger with reverse stepping and a cycle-level frame analyzer
Source-level debugging over GDB or DAP, with a VS Code extension
Scriptable from the command line and over a JSON-RPC control protocol
Free software under the GNU GPL v3
```

## What's new in this version (1500 characters)

```
First Microsoft Store release, from Copperline 0.20.0.

Two-player netplay, on the desktop and in the browser. A new debugging
workspace that puts the Amiga display and the inspectors side by side. Save
state thumbnails, GIF clips, new peripherals, and improvements to Paula
audio.

Save states made with 0.19 and earlier need recreating. Disk images and
ordinary in-game saves are unaffected.

The full release notes are at https://copperline.dev/news/
```

## Search terms (up to 7, 30 characters each)

```
amiga
amiga emulator
commodore
a1200
cd32
whdload
retro computing
```

## Copyright and trademark info

```
Copyright (c) Andrew Hutchings and the Copperline contributors. Free software
under the GNU General Public License v3. Amiga and Commodore are trademarks
of their respective owners; Copperline is an independent project, not
affiliated with or endorsed by any trademark holder.
```

## Additional license terms

The Store's Standard Application License Terms do not fit a GPL program, and
Partner Center lets a submission supply its own. Use:

```
Copperline is free software licensed under the GNU General Public License,
version 3 or later. These terms replace the Standard Application License
Terms in their entirety. The full licence text and the complete corresponding
source code are at https://github.com/CopperlineHQ/Copperline.

This package also contains other free software, including the AROS Kickstart
replacement and the bundled expansion ROMs, under their own licences. Those
licences ship inside the application folder.
```

## Contact and links

| Field | Value |
|---|---|
| Privacy policy URL | https://copperline.dev/privacy/ |
| Website | https://copperline.dev/ |
| Support contact info | https://github.com/CopperlineHQ/Copperline/issues |

## Screenshots

At least one is required. They must be **PNG, at least 1366x768**, which is
why the 1224x768 JPEGs in `posts/release-0.20.0/screenshots/` cannot be
reused. Two further things to watch for in a Windows Store listing: those
existing captures show macOS window chrome, which reads oddly on a Windows
listing, and they show a third-party demo, which is somebody else's
copyrighted work being used to advertise a product.

Capture them on Windows from real sessions, of software you are entitled to
show. Headless capture gives a clean image with no window chrome at all:

```pwsh
# The Amiga display on its own, scaled to clear the size minimum.
copperline --config machine.toml --noaudio --screenshot-after 30 shot.png
```

A headless screenshot is one Amiga frame (about 716x540), so it needs
doubling to clear 1366x768; any image editor will do it, and nearest-neighbour
keeps the pixels crisp. For the debugger and the launcher, capture the window
itself on Windows at 1920x1080.

A good set, in the order they should appear:

1. The emulator running something, full display.
2. The launcher, showing the machine profiles.
3. The debugging workspace, display and inspectors side by side.
4. The frame analyzer, showing cycle-level DMA contention.
5. WHDLoad direct boot or the game library.

## Age rating and declarations

The rating comes from the IARC questionnaire, not from this file. Answer it
about Copperline itself, which contains no content of its own: no violence,
no in-app purchases, no advertising, and no user-to-user content sharing.
Netplay connects two players who have exchanged an invitation code, with no
chat and no matchmaking service.

On the **Product declarations** page, note that the optional Ethernet
bridging feature can use Npcap, a non-Microsoft driver, when the user
installs it themselves. Everything else in the package is self-contained.
