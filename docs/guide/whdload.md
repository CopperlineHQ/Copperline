# WHDLoad support

WHDLoad allows floppy-based Amiga games and demos to be installed and run from
hard disk. Copperline can launch WHDLoad packages directly without requiring a
pre-installed Workbench environment or manual hardfile setup.

```sh
copperline --whdload "Turrican.lha"
```

Packages can also be specified in configuration files:

```toml
[whdload]
game = "Turrican.lha"
kickstarts = "/data/amiga/kickstarts"
```

You can also launch packages from the interactive launcher's **WHDLoad** tab
or by dragging and dropping an archive into the running emulator window.

## Supported package formats

Copperline supports three package formats:

- **`.lha` / `.lzh` archives** (standard WHDLoad distribution archives)
- **`.zip` archives**
- **Unpacked directories** containing installed game data and a `.slave` loader

Archives with nested folders are searched automatically to find the `.slave` file.

## Requirements

### Kickstart ROMs

Because Kickstart ROMs are proprietary Commodore software, they are not bundled
with Copperline:

- **Boot ROM:** WHDLoad games generally boot using Kickstart 3.1 (40.068 A1200).
  If no Kickstart ROM is provided, Copperline falls back to the bundled AROS ROM;
  while some titles will boot under AROS, many require an authentic Kickstart image.
- **Relocated Kickstart images (SKick):** Many OCS and ECS WHDLoad slaves load a
  secondary Kickstart image (typically 1.3) into memory during startup.

Point `kickstarts` (or the launcher's **Kickstart ROMs** directory) to your ROM directory.
Copperline identifies ROMs by cryptographic hash rather than filename, so filename
conventions do not matter. Cloanto Amiga Forever ROM images (including encrypted `.rom`
files with an accompanying `rom.key`) are supported.

### WHDLoad runtime binaries

Pre-built releases include necessary WHDLoad runtime components. When building
from source, run `tools/fetch-whdload.sh` once or click **Download** in the launcher's
settings tab. Custom packages can be specified via `whd_package` and `skick_package`.

## Saves and extracted files

Extracted game data and persistent files (such as high scores and save files)
are stored under `whdload/save/<GameName>/` in your user configuration directory
(or locally next to the binary if portable mode is enabled).

To reset a game's state and re-extract it, delete its directory under `whdload/save/`.
When launching directly from a folder, save data is written back to that folder.

Custom WHDLoad arguments can be passed via the configuration:

```toml
[whdload]
game = "Lotus2.lha"
args = "ButtonWait NoAutoVec"
```

## Machine selection

By default (`machine_type = "auto"`), Copperline configures a standard A1200
profile with 8 MiB fast RAM, which satisfies the vast majority of WHDLoad packages.

Explicit CLI overrides or configuration options take precedence:

```sh
copperline --whdload game.lha --model A4000
```

In the launcher settings:
- **Auto:** Automatically chooses machine parameters suitable for WHDLoad.
- **Copperline:** Uses the specific machine model defined in your current configuration.

Standard features such as `--screenshot-after`, input recording, and save states
operate normally during WHDLoad sessions.

## Library management in the launcher

The launcher includes a **WHDLoad** library browser with cover art and metadata:

Set **Game library** in settings to your game collection folder (subdirectories are
scanned recursively).

- **Refresh:** Rescans the game folder for newly added or removed archives.
- **Scan:** Queries the [OpenRetro](https://openretro.org) database to fetch title,
  release year, developer, publisher, player count, and box art. An OpenRetro login
  can be configured in launcher settings.
- **Update:** Opens a dialog to edit metadata or assign custom PNG cover art.
- **Favourite:** Toggles a game's starred status in your favourites list.

When multiple versions of a game exist in a collection, version numbers or disk
tags from filenames are shown to distinguish them.

To disable the launcher's WHDLoad browser page, set `enabled = false` in `[whdload]`
or toggle **A/V & Emu -> Emulation -> WHDLoad** in the UI menu.

## Configuration reference

```toml
[whdload]
game = "path/to/Game.lha"   # .lha, .zip, or directory
library = "..."             # save directory (default: <config>/whdload/save)
kickstarts = "..."          # Kickstart ROMs directory
args = "..."                # additional WHDLoad arguments
machine_type = "auto"       # "auto" or "copperline"
whd_package = "..."         # custom WHDLoad archive path
skick_package = "..."       # custom SKick archive path

# Launcher UI settings
enabled = true              # show WHDLoad tab in launcher
games = "..."               # root folder for games library
library_db = "..."          # metadata database path
library_cache = "..."       # downloaded metadata cache path
```

## Operational notes

- The WHDLoad splash screen displays during boot according to standard WHDLoad behavior.
  The default quit key is numeric keypad `*` unless customized by the slave.
- User-supplied cover images must be PNG format.
- One WHDLoad package can be booted per instance.
