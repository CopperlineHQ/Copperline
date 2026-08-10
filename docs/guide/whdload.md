# WHDLoad games

WHDLoad is the Amiga community's standard for running floppy games from a
hard disk. A game is installed once into a directory holding a `.slave`
loader beside its data, and the WHDLoad program boots it from AmigaOS,
taking the machine over the way the original disk would have.

Copperline boots such a package directly:

```sh
copperline --whdload "Turrican.lha"
```

There is no Workbench disk to prepare and no hard-drive image to build.
Copperline unpacks the package, builds a boot volume around the WHDLoad
program, works out a suitable machine from the package itself, and boots.

The same launch works from a configuration file:

```toml
[whdload]
game = "Turrican.lha"
kickstarts = "/data/amiga/kickstarts"
```

from the launcher's **WHDLoad** page, or by dropping a package onto the
window.

## Package formats

A package can be any of three things, and they all work the same way:

- an **`.lha`** archive, which is what most installers produce;
- a **`.zip`**, which is what you get when a browser packs a folder;
- a **plain folder** holding an installed game.

Copperline finds the `.slave` wherever it sits, so an archive that keeps
the game a folder or two down needs no unpacking first. `.lzh` is
accepted in place of `.lha`, and `.slav` in place of `.slave`.

The same game kept in two formats is listed twice, and both entries play.
Each has its own unpacked copy and its own saves.

## What you need

**Kickstart images from your own collection.** These are copyrighted and
do not ship with Copperline. They are wanted for two separate reasons:

- The emulated machine boots best from a Kickstart 3.1 (40.068 A1200)
  image. Without one Copperline falls back to the bundled AROS ROM; simple
  games still run, but many WHDLoad titles need the real Kickstart.
- Many games -- OCS and ECS titles especially -- load a Kickstart image of
  their own at run time, usually 1.3, and refuse to start without it.

Point **Kickstart ROMs** (or `kickstarts`) at the directory holding them.
The file names do not matter: Copperline identifies each image by its
contents, so a file called `KICK13.ROM` that is really Kickstart 1.2 is
recognised for what it is. Encrypted Cloanto Amiga Forever images work if
`rom.key` sits in the same directory. If a game asks for an image you do
not have, the error names it.

**The WHDLoad support files.** Release builds already include them.
Building from source, either press **Download** on the Settings page or
run `tools/fetch-whdload.sh` once. To use copies you already have, set
`whd_package` and `skick_package`; naming one and leaving the other unset
is fine.

## Saves and unpacked games

Each game gets a directory under `whdload/save/` in your configuration
directory, for example `~/.config/copperline/whdload/save/Turrican/`.
Everything the game writes -- savegames, high scores, its own settings --
lands there and stays across runs.

Delete a game's directory to unpack it fresh. Launching a plain folder
uses that folder directly, so its saves stay with it.

Extra WHDLoad options can be passed through:

```toml
[whdload]
game = "Lotus2.lha"
args = "ButtonWait NoAutoVec"
```

See the WHDLoad documentation for the full set.

## Which machine a game boots on

By default Copperline reads what the game needs from the package and boots
it on a suitable machine -- an A1200 with 8 MiB of fast RAM, which is what
WHDLoad games expect. OCS and ECS titles run correctly on it, as they do
on real hardware.

Anything you set yourself wins. A `[machine]` profile, a `rom`, or
`[memory]` sizes in your configuration are left alone, so
`copperline --whdload game.lha --model A4000` boots the game on an A4000.

**Machine type** on the Settings page chooses between the two:

- **Auto** takes the machine from the game.
- **Copperline** boots it on the machine your configuration describes.

Pressing it shows which one you have chosen.

Everything else about the run behaves normally: `--screenshot-after`,
scripted input, save states and `--record-input` all work.

## The Library page

The launcher's **WHDLoad** entry opens on the Library, which lists the
games in your collection with their cover art and details, and keeps a
second list of favourites.

Point **Game library** on the Settings page at the folder holding your
games. Sub-folders are searched too, so a collection filed by letter or by
genre works, and `.lha` files, zips and folders can be mixed together.
Clearing this setting empties the list.

Three buttons control the list:

- **Refresh** re-reads the folder and picks up games you have added or
  removed. Details already found are kept.
- **Scan** looks up each game on [OpenRetro](https://openretro.org) and
  fills in its name, year, publisher, developer, number of players and
  cover art. It tells you how many entries it updated.
- **Update** opens the details editor for the selected game.

Tick **Favourite** to add a game to the favourites list; **Remove** takes
it off again. Favourites are kept per game file, so one release of a game
can be starred without starring the others.

Select a game and press **Run** to play it.

Once there are twenty games or more, a row of letter buttons appears
above the list. Click one to jump to the first game starting with it;
letters your collection has no games under are greyed.

### Where the details come from

**Scan** matches each of your games against the OpenRetro database, mostly
by name. Collections name their files in every imaginable way, so the
match allows for the usual differences: capitals, spaces and underscores,
version numbers in the file name, and so on. Most games are recognised.

Some are not, and a few are matched to the wrong game. Use **Update** to
correct anything the scan got wrong. Once you have edited a game's
details, later scans leave it alone -- including after you rename the file.

Scanning needs an OpenRetro account, which is free. Sign in with **Log
in** on the Settings page. Cover art does not need an account.

### The details editor

**Update** opens a dialog for the selected game: name, year, publisher,
developer, players, version, and the cover art. Click the art to choose a
PNG of your own; it is scaled to fit.

**Save** keeps your changes, **Clear** empties the fields, and **Cancel**
leaves the game as it was. Clearing a game's details and saving hands it
back to the scan.

### Versions

Collections often hold the same game more than once -- different releases,
an AGA version, a translation. These all match the same database entry, so
after a scan they appear as identical-looking rows.

Where that happens, Copperline fills in a **Version** for each of them
from the file name, so you can tell them apart. Edit it in **Update** to
something clearer -- "CD32 v1.1", say -- and that is what the page shows.
Games your collection holds only once have no version unless you give them
one.

### Turning it off

**A/V & Emu -> Emulation -> WHDLoad** removes the WHDLoad entry from the
launcher. Games still boot from `--whdload` and from `[whdload] game`.

## Configuration reference

```toml
[whdload]
game = "path/to/Game.lha"   # .lha, .zip, or a folder holding the game
library = "..."             # unpacked games and saves; default: <config>/whdload/save
kickstarts = "..."          # directory holding your Kickstart images
args = "..."                # extra WHDLoad command-line options
machine_type = "auto"       # or "copperline" to boot on this machine
whd_package = "..."         # your own WHDLoad_usr.lha
skick_package = "..."       # your own skick*.lha

# Launcher only.
enabled = true              # false removes the WHDLoad page
games = "..."               # the folder the Library page lists
library_db = "..."          # default: <config>/whdload/support/launcher.db
library_cache = "..."       # default: <config>/whdload/support/cache
```

With `kickstarts` unset, Copperline looks beside an explicit `rom`, in
`<library>/Kickstarts`, and finally beside the support files.

`library_db` holds your library: one entry per game, with its details,
your favourites and any edits you have made. `library_cache` holds what a
scan downloaded. The cache can be deleted at any time and is rebuilt by
the next scan; deleting the library loses your favourites and edits.

## Notes and limitations

- The boot volume runs the real WHDLoad, so its own behaviour applies: the
  splash window appears briefly, and its quit key (`*` on the numeric pad
  unless the game says otherwise) exits back to the boot shell.
- Cover art you supply must be a PNG.
- Copperline has no per-game settings database. A game that wants
  something unusual -- NTSC timing, particular controls -- is handled with
  `args` and the machine settings.
- One game boots per run.
