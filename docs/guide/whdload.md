# WHDLoad games

WHDLoad is the Amiga community's standard for running floppy games from a
hard disk: a game is "installed" once into a directory with a `.slave`
loader beside its data, and the WHDLoad program boots it from AmigaOS,
taking the machine over the way the original disk would have.

Copperline boots such a package directly:

```sh
copperline --whdload "Turrican.lha"
```

No Workbench disk, no hand-built hard-drive image, no startup-sequence of
your own. Copperline unpacks the package (once), synthesizes a minimal
boot volume around the real WHDLoad program, derives a suitable machine
from the slave itself, and boots. The same launch works from a
configuration file:

```toml
[whdload]
game = "Turrican.lha"
kickstarts = "/data/amiga/kickstarts"
```

or from the launcher's **WHDLoad** page, or by dropping a package onto
the window.

## Package formats

A package is any of three things, and nothing treats them differently:

- an **`.lha`** archive, which is what the installers publish;
- a **`.zip`**, which is what you get when a browser packs a folder;
- a **plain folder** holding the installed tree.

The `.slave` is what makes any of them a game, and it is searched for
rather than assumed -- a published `.lha` has it at the archive root, a
zip made from an unpacked one usually has it a folder or two down. Both
work.

macOS puts `__MACOSX/` and `._name` stubs in every zip it writes. One of
those is called `._Something.Slave`, and it would otherwise be found by
the slave search and booted instead of the real one, so they are ignored
everywhere.

`.lzh` is accepted as `.lha`, and `.slav` as `.slave`, for packages that
have been through a filesystem with a short-name limit.

Keeping the same game in two formats is a duplicate, not an error: it
lists twice and both play, each with its own unpacked copy and its own
saves. Delete one and press Refresh and it goes.

## What you need

- **The game package**, in one of the three shapes above.
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
  `tools/fetch-whdload.sh`, or press **Download** on the Configuration
  page; `COPPERLINE_WHDBOOT_DIR` points at a directory holding them if
  you keep them elsewhere.

  Set `whd_package` or `skick_package` to use your own copies instead.
  Naming one and leaving the other unset works; the unnamed one still
  comes from the search.

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
  game/     the unpacked package (WHDGame:), unpacked once, then reused
```

The library defaults to `whdload/save/`, beside the `whdload/support/`
the archives live in. An installation that already has games directly
under `whdload/` -- where this used to be -- carries on using them where
they are, since that is where its saves are.

Both are mounted live through the host-directory service
([`[[filesys]]`](configuration.md)), so everything the game writes --
savegames, highscores, configuration -- lands in `game/` on the host and
**persists across runs**. Delete a game's `game/` directory to force a
fresh unpack; delete a savegame file to undo a save. Passing a folder as
the game mounts that folder itself, so saves persist there instead.

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
A4000 instead. `machine_type = "copperline"` makes that the rule rather
than the exception: the package boots on whatever machine the
configuration describes.

Everything composes with the rest of the CLI: `--screenshot-after`,
scripted input, save states, `--record-input` all work, so a WHDLoad game
is scriptable and deterministic like any other Copperline run.

## The Library page

The launcher's **WHDLoad** entry opens on the Library: the packages in
your game folder, with their metadata and cover art, and a second list of
favourites. **Configuration** is the other half, on the same nav row.

The cover frame is one size for every game, so the writing under it starts
on the same line each time. Amiga box art is portrait almost without
exception; the rare landscape scan is letterboxed into the frame rather
than being stretched to fill it.

Point **Game library** at a folder of packages. It is searched all the
way down, so a collection filed by letter or by genre works, and it can
hold `.lha` files, zips and folders mixed together.

Nothing scans on its own. A folder of several thousand packages takes
long enough to read that a page which did it whenever you opened a tab
would be a page that stalls. Three buttons say when:

- **Refresh** re-reads the folder. Fast: it lists files and reads back
  what the last scan resolved. Metadata already worked out is kept.
- **Scan** resolves metadata against [OpenRetro](https://openretro.org).
  It syncs only what has changed since last time, matches every package,
  and fetches the cover art it does not already have. It says which of
  three things happened -- the database was already up to date, *N*
  entries were updated, or you are not signed in and the cached copy was
  used.
- **Update** opens the metadata editor for the selected game.

Progress and failures appear on the status line at the bottom; the log
has the detail. A scan can be stopped at any point and keeps what it had
resolved, and pressing Run stops it.

### Matching

A package is matched to a catalogue entry by the SHA-1 of its slave where
that works, and by name otherwise. In practice OpenRetro's WHDLoad file
lists were imported in 2015 and slaves are revised with every release, so
almost nothing matches by digest today -- the digest is computed anyway
because it is also how a package is recognised after it has been renamed.

Name matching handles what installers and cataloguers disagree about:
case, separators, `CamelCase`, the `_v1.5_1234` version stamp, a leading
or trailing "The", `&` against "and", and roman numerals either way round.
Failing an exact match it will accept a catalogue title whose words all
appear in order in the package name, or one within 80% of it by edit
distance. On a 2252-package library that matches 92%.

It is best effort, and it is occasionally wrong. That is what the
metadata editor is for.

### Signing in to OpenRetro

**Log in** on the Configuration page. An account is free, and it is only
needed to sync the game database -- cover art is public.

Nothing is stored. The password is traded for a token and wiped, the
token lives as long as the launcher is open, and **Log out** hands it
back to the service. The connection is HTTPS.

### The metadata editor

**Update** opens a dialog for the selected game: name, year, publisher,
developer, players, and the cover art. Clicking the art picks a PNG,
which is scaled to the size the fetched covers are.

**Save** marks the entry as yours, and scans leave it alone from then on
-- including after the file is renamed, because the entry is recognised
by its slave's digest rather than its name. **Clear** empties the fields;
saving an empty entry hands it back to the scan. **Cancel** changes
nothing.

### Versions

Collections carry the same game several times over -- `1.0`, `1.1`,
`CD32`, a German release -- and there is no standard to how installers
name them, so every one of them matches the same catalogue entry and the
list shows a run of rows that read alike.

**Version** is a metadata field like any other, except that the catalogue
has no opinion about it. Where the library holds a named game under one
title more than once, the page shows the package's own file name without
its extension -- `.lha` against `.zip` is how it was packed, not which
release it is. Edit it in **Update** to whatever tells them apart --
"CD32 v1.1" -- and that is shown instead.

A game held once, with nothing typed, has no version and no row. Neither
has one the scan could not name: a file name under a row that says
nothing else is not the answer to which release it is. Two
lines is what the column shows and what the field accepts.

### Turning it off

**A/V & Emu -> Emulation -> WHDLoad** is on by default. Off, the
navigation entry goes and the pages behind it do nothing at all: no
database read, no cover worker, no scan.

It does not stop a game booting. `--whdload` and `[whdload] game` are
explicit instructions and still do what they say, so scripts and headless
runs are unaffected.

## Configuration reference

```toml
[whdload]
game = "path/to/Game.lha"   # .lha, .zip, or a folder with a .slave
library = "..."             # unpacked games and saves; default: <config>/whdload/save
kickstarts = "..."          # directory scanned for Kickstart images
args = "..."                # extra WHDLoad command-line options
machine_type = "auto"       # or "copperline" to boot on this machine
whd_package = "..."         # your own WHDLoad_usr.lha
skick_package = "..."       # your own skick*.lha

# Launcher only.
enabled = true              # false removes the WHDLoad page entirely
games = "..."               # the folder the Library page lists
library_db = "..."          # default: <config>/whdload/support/launcher.db
library_cache = "..."       # default: <config>/whdload/support/cache
```

When `kickstarts` is not set, the directory of an explicit `rom` and
`<library>/Kickstarts` are scanned, and a `Kickstarts/` directory next to
the support archives is always tried last.

`library_db` is the scanned library: one entry a package, with the
metadata resolved for it. `library_cache` is what a scan downloaded --
the snapshot of the online database, and the cover art. Deleting the
cache costs a download; deleting the library costs a scan.

## Notes and limitations

- The boot volume runs the real WHDLoad, so WHDLoad's own behaviour
  applies: its splash window appears briefly, its quit key (default
  `*` on the numeric pad, or as the slave defines) exits back to the
  boot shell.
- Cover art you supply must be a PNG. Copperline carries no JPEG decoder.
- Per-game tuning beyond the slave header (a title that wants NTSC, or
  custom controls) is what `args` and the explicit machine overrides are
  for; Copperline does not ship a per-game settings database.
- One game boots per run: `--whdload` builds the machine around the
  package.
