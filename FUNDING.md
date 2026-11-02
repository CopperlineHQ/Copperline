# Funding Copperline

Copperline is free software under the GPL, developed in the open. It will
stay that way: nothing here buys a feature, a private build, or a place in
the queue. What funding buys is time, and the hardware to check the emulator
against the real thing.

## Where to give

| Platform | Link | Notes |
| --- | --- | --- |
| Patreon | [patreon.com/cw/Copperline](https://www.patreon.com/cw/Copperline) | Copperline-specific, monthly, tiers from GBP 3 |
| GitHub Sponsors | [github.com/sponsors/linuxjedi](https://github.com/sponsors/linuxjedi) | Monthly or one-off, no platform fee |
| Ko-fi | [ko-fi.com/linuxjedi](https://ko-fi.com/linuxjedi) | One-off, no account needed |
| PayPal | [paypal.me/linuxjedi](https://paypal.me/linuxjedi) | One-off |

Patreon is the one to pick for ongoing support of Copperline itself; the
others are personal links covering all of Andrew "LinuxJedi" Hutchings'
open-source work, including
[AmigaRGBtoHDMI](https://github.com/LinuxJedi/AmigaRGBtoHDMI).

Patrons whose tier includes a credit are thanked by name in
[CREDITS.md](CREDITS.md), in the app's About window, and on
[copperline.dev](https://copperline.dev/).

## First goal: signed binaries

Windows is done: release builds are signed through the SignPath
Foundation's open-source programme (free code signing provided by
[SignPath.io](https://about.signpath.io/), certificate by
[SignPath Foundation](https://signpath.org/)).

The macOS disk image is still only ad-hoc signed. It works, but Gatekeeper
refuses the first launch until you right-click-Open. That is a poor first
impression of a project whose whole pitch is that it boots straight to a
working Amiga.

Fixing it is not a code problem, it is a subscription problem. Signing and
notarizing on macOS needs Apple Developer Program membership (USD 99/year).
Recurring support is what makes committing to that sensible, which is why
Patreon is the tier that moves this along fastest.

Reaching it means downloads that open on a double-click, on every platform,
with no quarantine incantations in the README.

## What else it pays for

- Development time on the emulator, the documentation, and the packaging
  for macOS, Linux, and the web.
- Real Amiga hardware and peripherals to measure against. Copperline models
  chip behaviour rather than individual titles, so hardware-derived timing
  numbers (the sort the `timing-test/` disk produces) are what keeps the
  models honest.
- Test media, capture equipment, and the hosting behind
  [copperline.dev](https://copperline.dev/).

## Other ways to help

Money is not the only useful contribution, and often not the most useful
one:

- File a good bug report. A hardware-focused reproduction, the config, and
  the emulated timestamp are worth more than a video.
- Improve the documentation under `docs/`, or the guides on the website.
- Send a patch. [`CONTRIBUTING.md`](CONTRIBUTING.md) has the ground rules;
  the short version is that fixes describe 68000/Agnus/Denise/Paula/CIA
  behaviour, never a particular game or demo.
- Compare Copperline against real hardware. Measurements from a machine on
  your desk resolve arguments that no amount of emulator archaeology can.
- Come and talk about it on [Discord](https://discord.gg/HDTjt3tYAC).

## Please do not send

Kickstart ROMs, disk images, hard-disk images, or CD images. They are
copyrighted, they cannot be committed, and they are not needed to report a
problem. See [`CONTRIBUTING.md`](CONTRIBUTING.md).
