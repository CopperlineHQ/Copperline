# WHDLoad support

WHDLoad lets floppy-based Amiga games and demos be installed and run from
hard disk. Copperline launches WHDLoad packages directly, with no Workbench
installation or hardfile to prepare.

```sh
copperline --whdload "Turrican.lha"
```

Packages can also be specified in configuration files:

```toml
[whdload]
game = "Turrican.lha"
kickstarts = "/data/amiga/kickstarts"
```

You can also launch packages from the launcher's **WHDLoad** pages or by
dropping an archive onto the running emulator window.

Copperline stages two host directories and mounts them as live volumes: a boot
volume (`WHDBoot:`) holding the WHDLoad program, a generated
`S/Startup-Sequence` and the staged Kickstart images, and the game itself
(`WHDGame:`). The guest reads and writes both directly, so the game's saves
land on the host.

## Supported package formats

Copperline supports three package formats:

- **`.lha` / `.lzh` archives** (standard WHDLoad distribution archives)
- **`.zip` archives**
- **Unpacked directories** containing installed game data and a `.slave` loader

Copperline searches nested folders for the `.slave` file. When a package holds
more than one, it boots the shallowest (then the first alphabetically).

## Requirements

### Kickstart ROMs

Kickstart ROMs are proprietary, so Copperline does not bundle them:

- **Boot ROM:** WHDLoad games generally boot using Kickstart 3.1 (40.068 A1200).
  If no Kickstart ROM is provided, Copperline falls back to the bundled AROS ROM;
  some titles boot under AROS, but many require a real Kickstart image.
- **Relocated Kickstart images (SKick):** Many OCS and ECS WHDLoad slaves load a
  second Kickstart image (typically 1.3) into memory during startup. These
  images need the `.RTB` relocation tables from the SKick archive, which
  Copperline stages beside them in `Devs:Kickstarts/`.

Point `kickstarts` (or the launcher's **Kickstart ROMs** row) at your ROM
directory. Without it, Copperline looks in the directory of the configured
`rom`, in `Kickstarts/` under the save-data directory, and in `Kickstarts/`
beside the WHDLoad support archives. It identifies ROMs by their size and
WHDLoad CRC-16 checksum, so file names do not matter. Byte-swapped dumps and
Cloanto Amiga Forever images (encrypted `.rom` files with an accompanying
`rom.key`) are supported. If a slave names a Kickstart image that none of
these directories holds, the launch fails with the image's name, size and
CRC.

### WHDLoad support archives

Booting a package needs two freely redistributable archives: WHDLoad itself
(`WHDLoad_usr.lha`) and SKick (`skick346.lha`). Pre-built releases include
both. In a source build, run `tools/fetch-whdload.sh` once, or press
**Download** beside the empty **WHDLoad package** and **SKick package** rows
on the launcher's WHDLoad **Settings...** page. `whd_package` and
`skick_package` point at your own copies instead.

Copperline looks for the archives in the directory named by
`COPPERLINE_WHDBOOT_DIR`, then in `whdboot/` next to the executable (or in
the macOS app bundle's `Resources/whdboot/` or a Homebrew
`share/copperline/whdboot/`), then in `whdload/support/` in the Copperline
host data folder, where **Download** saves them, and finally in
`assets/whdboot/`, where the fetch script puts them.

## Saves and extracted files

An archive is extracted once, and the game's own files (high scores, save
games) persist in the extracted copy. Each package gets a directory under
`whdload/save/` in the Copperline host data folder
(`~/Documents/Copperline` on macOS and Linux,
`%USERPROFILE%\Documents\Copperline` on Windows, or the application folder in
a [portable installation](ui.md#where-files-go)); `library` moves it
elsewhere. The directory is named after the archive: without its extension
for an `.lha`, with it for a `.zip`. It holds `game/`, the extracted package
mounted as `WHDGame:`, and `boot/`, the boot volume regenerated on every
launch. An installation that already keeps unpacked games directly under
`whdload/`, as older versions did, carries on using that directory.

To reset a game's state and re-extract it, delete its directory under
`whdload/save/`. A game launched from an unpacked folder is mounted in place,
so its saves are written back to that folder.

Copperline starts each game with
`WHDLoad "<slave>" Preload SplashDelay=0`. `args` appends further WHDLoad
options to that command line:

```toml
[whdload]
game = "Lotus2.lha"
args = "ButtonWait NoAutoVec"
```

## Machine selection

With `machine_type = "auto"` (the default), Copperline supplies an A1200
profile, 8 MiB of fast RAM, and a staged Kickstart 3.1 (40.068 A1200) image as
the boot ROM, wherever the configuration has not already chosen them. An A1200
satisfies every slave requirement flag. If the slave says it needs more than
8 MiB of expansion memory, Copperline logs a warning; configure a larger
machine if the game then refuses to start.

Explicit CLI overrides or configuration options take precedence:

```sh
copperline --whdload game.lha --model A4000
```

The launcher's **Machine type** row offers the same two choices:

- **Auto:** Derives the machine from the slave header, as above.
- **Copperline:** Boots the package on the machine your configuration
  describes, unchanged.

Standard features such as `--screenshot-after`, input recording, and save states
work normally during WHDLoad sessions.

## Library management in the launcher

The launcher's WHDLoad **Library** page lists your collection with cover art
and metadata. Set **Game library** on the **Settings...** page to your
collection folder; subfolders are searched too.

- **Refresh:** Re-reads the game folder for added or removed packages.
- **Scan:** Matches the packages against the [OpenRetro](https://openretro.org)
  database for title, release year, developer, publisher, player count, and box
  art. Downloading the database needs an OpenRetro account, entered on the
  **OpenRetro** row of the **Settings...** page; later scans without one reuse
  the cached copy.
- **Update:** Opens a dialog to edit the selected game's metadata or give it
  your own PNG cover art.
- **Favourite:** Ticks a game into the favourites list.

When a collection holds a game more than once, each package's file name is
shown as its version to tell them apart.

To remove the WHDLoad pages from the launcher, set `enabled = false` in
`[whdload]` or turn off the launcher's **A/V & Emu -> Emulation -> WHDLoad**
row. `--whdload` and `[whdload] game` still boot games either way.

## Configuration reference

```toml
[whdload]
game = "path/to/Game.lha"   # .lha, .zip, or directory
library = "..."             # save directory (default: <host data>/whdload/save)
kickstarts = "..."          # Kickstart ROMs directory
args = "..."                # additional WHDLoad arguments
machine_type = "auto"       # "auto" or "copperline"
whd_package = "..."         # custom WHDLoad archive path
skick_package = "..."       # custom SKick archive path

# Launcher only
enabled = true              # false removes the WHDLoad pages
games = "..."               # the folder the Library page lists
library_db = "..."          # default: <host data>/whdload/support/launcher.db
library_cache = "..."       # covers and database snapshot;
                            # default: <host data>/whdload/support/cache
```

## Operational notes

- The default quit key is numeric keypad `*` unless customized by the slave.
- User-supplied cover images must be PNG format.
- One WHDLoad package can be booted per instance. An explicit `--run` takes
  precedence over a game remembered in `[whdload] game`.
- Under [netplay](netplay.md), a game's saves last for the session only.
