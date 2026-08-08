# WHDLoad games

WHDLoad is the Amiga community's standard for running floppy games from a
hard disk: a game is "installed" once into a directory with a `.slave`
loader beside its data, and the WHDLoad program boots it from AmigaOS,
taking the machine over the way the original disk would have. Installed
games travel as `.lha` archives.

Copperline boots such a package directly:

```sh
copperline --whdload "Turrican.lha"
```

No Workbench disk, no hand-built hard-drive image, no startup-sequence of
your own. Copperline extracts the archive (once), synthesizes a minimal
boot volume around the real WHDLoad program, derives a suitable machine
from the slave itself, and boots. The same launch works from a
configuration file:

```toml
[whdload]
game = "Turrican.lha"
kickstarts = "/data/amiga/kickstarts"
```

or from the launcher's **Storage -> WHDLoad** page, or by dropping a
`.lha` file onto the window.

## What you need

- **The game package**: an `.lha` archive holding a `.slave` file and the
  game data (the shape every WHDLoad install produces). A directory
  containing the same tree also works and is mounted in place.
- **Kickstart images, from your own collection.** These are copyrighted
  and never ship with Copperline. Two distinct needs meet here:
  - The emulated machine itself boots best from a Kickstart 3.1 (40.068
    A1200) image, the canonical WHDLoad host. Without one the bundled
    AROS ROM is used instead, and while simple slaves run, many WHDLoad
    programs need the real Kickstart -- expect reduced compatibility.
  - Many slaves (OCS/ECS-era games especially) additionally load a raw
    Kickstart image at run time from `Devs:Kickstarts/` -- typically
    Kickstart 1.3 -- and refuse to start without it.
- **The WHDLoad support archives**, which release bundles already
  include: the unmodified WHDLoad distribution (`WHDLoad_usr.lha`,
  freeware since release 18.2) and the Soft-Kicker package
  (`skick346.lha`) whose `.RTB` relocation tables accompany the raw
  Kickstart images. Building from source, fetch them once with
  `tools/fetch-whdload.sh`; `COPPERLINE_WHDBOOT_DIR` points at a
  directory holding them if you keep them elsewhere.

Kickstart images are identified by **content, not filename**: Copperline
computes the same CRC-16 WHDLoad uses over each candidate file in the
`kickstarts` directory, after undoing the usual dump variations
(byte-swapped EPROM images, doubled 256 KiB dumps, Cloanto Amiga Forever
`rom.key` encryption -- put `rom.key` in the same directory). A file
called `KICK13.ROM` that is really Kickstart 1.2, or a 3.1 dump that is
actually the A600 revision, is recognized for what it is and staged under
its proper `Devs:Kickstarts/` name. If a slave demands an image you do
not have, the error names the file, size, and checksum it wants.

## What gets staged, and where saves live

Each game gets a directory in the **game library** (by default
`whdload/` inside the per-user configuration directory, e.g.
`~/.config/copperline/whdload/`):

```text
<library>/<Game>/
  boot/     the synthesized boot volume (WHDBoot:), regenerated each run:
            C/WHDLoad, S/Startup-Sequence, Devs/Kickstarts/*
  game/     the extracted package (WHDGame:), extracted once, then reused
```

Both are mounted live through the host-directory service
([`[[filesys]]`](configuration.md)), so everything the game writes --
savegames, highscores, configuration -- lands in `game/` on the host and
**persists across runs**. Delete a game's `game/` directory to force a
fresh extraction; delete a savegame file to undo a save. Passing a
directory as the game mounts that directory itself, so saves persist
there instead.

The generated `Startup-Sequence` runs
`WHDLoad <slave> Preload SplashDelay=0`. Extra WHDLoad options (see the
WHDLoad documentation for the full set) can be appended:

```toml
[whdload]
game = "Lotus2.lha"
args = "ButtonWait NoAutoVec"
```

## The derived machine

The slave header declares what the installed program needs: AGA, a 68020,
chip-memory size, expansion memory. Copperline boots every WHDLoad game
on the canonical WHDLoad host -- an A1200 (68EC020, AGA, 2 MiB chip) with
8 MiB of fast RAM -- which satisfies every slave requirement flag;
OCS/ECS games run under the slave's own hardware bending exactly as they
do on a real A1200.

Anything you set explicitly wins over the derivation: a `[machine]`
profile, `rom`, or `[memory]` sizes in the configuration (or their CLI
equivalents such as `--model` and `--fast`) are left untouched, so
`copperline --whdload game.lha --model A4000` boots the package on an
A4000 instead.

Everything composes with the rest of the CLI: `--screenshot-after`,
scripted input, save states, `--record-input` all work, so a WHDLoad game
is scriptable and deterministic like any other Copperline run.

## Configuration reference

```toml
[whdload]
game = "path/to/Game.lha"   # .lha archive or directory with a .slave
library = "..."             # game library; default: <config dir>/whdload
kickstarts = "..."          # directory scanned for Kickstart images
args = "..."                # extra WHDLoad command-line options
```

When `kickstarts` is not set, the directory of an explicit `rom` and
`<library>/Kickstarts` are scanned, and a `Kickstarts/` directory next to
the support archives is always tried last.

## Notes and limitations

- The boot volume runs the real WHDLoad, so WHDLoad's own behaviour
  applies: its splash window appears briefly, its quit key (default
  `*` on the numeric pad, or as the slave defines) exits back to the
  boot shell.
- Per-game tuning beyond the slave header (a title that wants NTSC, or
  custom controls) is what `args` and the explicit machine overrides are
  for; Copperline does not ship a per-game settings database.
- One game boots per run: `--whdload` builds the machine around the
  package. To browse a collection, keep packages in a directory and pick
  from the launcher.
