# Libretro / RetroArch

Copperline's libretro core runs Amiga floppy software, CD32 discs and WHDLoad
packages in RetroArch and other libretro frontends. It includes AROS boot ROMs
and uses the desktop emulator's CPU, chipset, renderer and audio mixer.

Supported content includes standard 880 KiB and 1760 KiB ADFs, ISO/CUE/CHD/NRG
CD images, M3U playlists, and WHDLoad LHA/LZH/ZIP packages. CUE audio tracks
can be BINARY or WAVE; this build does not enable MP3 track decoding. PAL and
NTSC are available. Extended ADFs, arbitrary hardfiles and desktop configuration files
are not accepted by this frontend.

## Build and install

From the repository root:

```sh
cargo build --manifest-path crates/copperline-libretro/Cargo.toml --release --locked
```

The output is under `crates/copperline-libretro/target/release/`:

| Platform | Build output | Installed core filename |
|---|---|---|
| Linux | `libcopperline_libretro.so` | `copperline_libretro.so` |
| macOS | `libcopperline_libretro.dylib` | `copperline_libretro.dylib` |
| Windows | `copperline_libretro.dll` | `copperline_libretro.dll` |

Copy the library to the frontend's cores directory using the installed name,
and copy `crates/copperline-libretro/copperline_libretro.info` to its core-info
directory. RetroArch shows these locations under **Settings → Directory**.
Load the Copperline core, then open a supported image or package through **Load Content**.
Starting the core without content boots AROS with an empty drive.

The repository's Libretro workflow builds packages for Linux, macOS and
Windows. Each package includes the core, its info file, the Copperline license,
AROS licensing and acknowledgement files, and the unmodified WHDLoad support
archives under `system/whdboot/`. Copy that directory into the frontend's system
directory to use WHDLoad. These are workflow artifacts;
installation through RetroArch's Online Updater is not configured.

## Machine and ROM options

The **Auto** machine option selects A500 for floppies, CD32 for discs and
A1200 with 8 MiB fast RAM for WHDLoad. You can also select A500, A1200 or CD32
explicitly. Other options select PAL/NTSC, AROS/Kickstart, floppy write
protection and netplay mode. Close and reload content after changing an option;
the frontend's Reset command resets the current machine without applying new
options. The core does not read the desktop's saved defaults or `copperline.toml`.

AROS is embedded in the library and needs no external files. For software that
needs a Commodore ROM, select **Kickstart** and put your ROM in the frontend's
system directory. The core first looks for the selected machine's ROM:
`kickstart-a500.rom`, `kickstart-a1200.rom` or `kickstart-cd32.rom`, then
`kickstart.rom`. CD32 also requires `kickstart-cd32-ext.rom` (the separate
extended ROM). Combined 1 MiB CD32 ROM files must be split before use.
A missing or invalid selected ROM produces an error; it does not fall back to
AROS. Commodore ROMs are not included.

The emulated RTC starts at 2000-01-01 00:00:00 UTC and advances with emulated
time; the host clock does not seed it.

## Controls

With both frontend ports set to **Automatic**, the first RetroPad controls
the joystick on Amiga port 2, and the first host mouse controls Amiga port 1.
On CD32, both automatic ports are CD32 gamepads. Outside netplay mode,
keyboard input is available; RetroArch's Game Focus mode passes keys
through without triggering its normal hotkeys.

| Frontend control | Amiga control |
|---|---|
| RetroPad D-pad | Joystick directions |
| RetroPad B (south button) | Fire |
| RetroPad A (east button) | Blue / second fire |
| RetroPad Y / X | CD32 green / yellow |
| RetroPad Start | CD32 play |
| RetroPad L / R | CD32 rewind / forward |
| Mouse left / right / middle | Mouse buttons |
| Left / right Shift, Alt | Corresponding Amiga modifier |
| Either Ctrl | Amiga Ctrl |
| Left / right Super or Meta | Left / right Amiga |
| Insert or Help | Amiga Help |

Frontend port 1 corresponds to Amiga port 2; frontend port 2 corresponds to
Amiga port 1. Select **Amiga joystick / CD32 pad** for both to use two gamepads. Select
**Amiga mouse** explicitly for both to use the frontend's first and second
mice; the frontend and its input driver must support multiple mice. A port
can also be disconnected. This version has no on-screen keyboard or
gamepad-to-mouse controls.

## Disk swapping and writes

An M3U playlist contains one image path per line, with paths relative to the
playlist's directory or absolute. Blank lines and lines beginning with `#`
are ignored. UTF-8 with an optional BOM and either Unix or Windows line
endings is accepted. A playlist can hold up to sixteen images. Use either floppies or CDs in a
playlist; mixing the two is rejected.

```text
Game Disk 1.adf
Game Disk 2.adf
Save Disk.adf
```

Use the frontend's **Disc Control** menu to eject, select an image, then
insert it. Selections use DF0 on Amiga models or the optical drive on CD32. Adding and replacing playlist images
also requires an ejected drive.

Guest writes update memory during emulation. When a disk is ejected or
content is closed, changed ADFs are written under `copperline/` in the
frontend's save directory. Names include a digest of the original image to
separate different disks with the same filename. The original ADFs remain
unchanged. CDs are read-only. CD32 EEPROM saves are kept in
`copperline/cd32.nvram` on normal close. Loading the same content picks up these saved copies.

At load time, the core copies every CD source file into a private temporary
directory and hashes the copies. This requires temporary disk space equal to
the source files, including every disc in a playlist. Emulation, reinsertion
and rollback use these copies even if the originals change or are removed.
The copies are deleted when content is closed.

If the frontend supplies no save directory, the content's directory is used;
for a no-content session the system directory is used instead. A save error
is reported; a failed eject leaves the disk inserted so saving can be retried.
Unloading always releases the machine, even if writing its saved copies fails.
Close content normally to save changes: a crash or forced process termination
can lose writes since the last eject.

## WHDLoad

Choose **Auto** or **A1200**, select **Kickstart**, and load the original
LHA/LZH/ZIP package. A1200 Kickstart 3.1 is recommended; AROS does not boot
all WHDLoad slaves. The core extracts the package into a temporary directory,
uses Copperline's existing slave detection and startup script, and builds
`WHDBoot:` and `WHDGame:` as private OFS hard disks. If an archive contains
multiple slaves, it uses the first in sorted order.

The frontend's system directory must contain:

```text
kickstart-a1200.rom
whdboot/WHDLoad_usr.lha
whdboot/skick346.lha
Kickstarts/              # additional Kickstart images required by slaves
```

The support archives are included in workflow packages. For a source build,
run `tools/fetch-whdload.sh` and copy `assets/whdboot/*.lha` into
`system/whdboot/`. WHDLoad identifies Kickstart images by content; images in
the system directory, `Kickstarts/`, and `whdboot/Kickstarts/` are considered.
Missing slave-required firmware is reported before boot.

Guest writes stay in memory until normal close, then changed sectors are
saved under `copperline/whdload/<digest>/`. Reloading the same package restores
its game saves, including after a support-archive or firmware change. Changing
the game volume itself starts a separate save set. The original archive is
unchanged.
Each generated volume is limited to 128 MiB; large packages also require
substantial save-state memory.

## RetroArch netplay

1. Install both the core library and its matching `.info` file. RetroArch uses
   the info file to recognize deterministic save-state support.
2. Both players select **Netplay mode → enabled**, then close and reload
   content. Use the same core build, machine/ROM/video options and original
   content. Use identical playlists with relative entries. WHDLoad also needs
   identical support archives and staged firmware.
3. Use gamepads on both frontend ports. Automatic uses two joysticks, or two
   CD32 pads on the CD32 machine. Keyboard and mouse input are disabled in this
   mode. Player 1 drives Amiga port 2; player 2 drives Amiga port 1.
4. Start hosting or connect through RetroArch's usual Netplay menu.

RetroArch handles networking, prediction, rollback and host-state transfer.
Local paths need not match: CDs resolve through content hashes and WHDLoad
volumes are built with fixed timestamps. The host's current disk writes and
CD32 EEPROM state travel in the initial checkpoint. Content and ROM files
are not transferred by this core; both players need their own copies.

Disk swaps and playlist edits are disabled in netplay mode. Saves made during
the session are temporary and are not written over either player's local
saves. Close content and disable netplay mode to resume ordinary persistence.
Netplay therefore currently suits games that run from one inserted image or
WHDLoad package. Rewind and run-ahead are separate frontend features and have
not been validated alongside netplay.

## Memory map, cheats and save RAM

The core describes the guest address map through the libretro memory-map
interface, so RetroArch's cheat search and memory viewer, and any
achievement or memory-inspection feature built on it, can address Amiga
memory directly. The map names each window by address space:

| Address space | Window | Notes |
|---|---|---|
| `chip` | Chip RAM at `$000000` | Also `RETRO_MEMORY_SYSTEM_RAM` |
| `slow` | Trapdoor slow RAM at `$C00000` | A500 profile only |
| `fast` | Zorro II/III and motherboard fast RAM banks | At their autoconfig bases |
| `rom` | Kickstart or AROS at `$F80000`; an extended ROM at `$E00000` (AROS's second image, or the CD32 extended ROM) | Read-only |

Every window is big-endian, as the 68000 sees it. A bank that is not
naturally aligned is split into aligned power-of-two blocks: the 8 MiB fast
RAM of the WHDLoad machine appears as blocks at `$200000`, `$400000` and
`$800000`. Fast RAM windows are only known once the guest's boot ROM has
autoconfigured the boards, so the map is announced when content loads and
again when a bank appears or moves: a few frames into the boot, after a
Reset (which returns the boards to unconfigured), and after a state load.
The host addresses behind the windows do not change during a session,
including across state loads and resets, so pointers taken from the map or
from `retro_get_memory_data` stay valid until the content is closed.
Mirror images of small chip RAM banks inside the 2 MiB chip window are not
described; only the fitted bank is.

`retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM)` returns chip RAM. On the
CD32 machine `RETRO_MEMORY_SAVE_RAM` returns the 1 KiB EEPROM, so the
frontend also keeps it as an ordinary `.srm` file next to the content.
The core still writes `copperline/cd32.nvram` on close; RetroArch loads its
`.srm` after the core loads that file, so when both exist and differ the
`.srm` wins, and on a normal close both hold the same bytes. Save RAM is
not exposed in netplay mode, where saves stay temporary. Video RAM is not
exposed separately: Amiga display memory is chip RAM.

Cheats use the plain Amiga trainer format: a hexadecimal address, a colon,
and a hexadecimal value whose length selects the width.

```text
0A1234:05              byte
0A1234:1234            word
0A1234:12345678        long
0A1234:05+0A1240:FFFF  several pokes in one cheat
```

Addresses take up to eight hexadecimal digits and can name any RAM bank,
including fast RAM once it is configured. An enabled cheat is written into
RAM once after every emulated frame, like a resident trainer poke, so a
game that rewrites the location each frame sees the cheat value between
frames. Disabling a cheat stops the pokes but does not restore the earlier
value. Writes to addresses outside fitted RAM (custom registers, ROM,
unconfigured boards) do nothing. A code that does not parse is reported
through the frontend's log and ignored; the remaining cheats still apply.
Cheats are not part of save states; the frontend re-applies its own list.
They are local pokes, so netplay peers must enable identical cheats or
their machines diverge.

A netplay session publishes no memory map, and its save RAM reads as
absent. RetroArch offers neither the cheat search nor the memory viewer
while a session runs, and holding the RAM banks at fixed host addresses
would cost a copy of every bank on each of the rollbacks a session makes
several of per frame. Leave netplay to run a game and use the memory tools
in a single-player session.

## Save states and presentation

Frontend save states include the machine, writable floppy and hard-disk data, the
selected image and eject state, controller selections, and pending mouse and
keyboard input. They also retain pending audio samples outside netplay mode and the video aperture
classification. Restoring a state also restores disk contents. Those contents
become the persistent copies on the next eject or normal close.

States require the same machine, boot ROM, original media, write-protection
and netplay options. Version 0.2 uses a new libretro envelope around the shared
chunked machine format, including CPU rollback latches. States from the first
libretro version must be recreated. They cannot be opened directly as desktop
`.clstate` files.

The frontend receives a fixed capacity for each loaded session: 64 MiB plus
space for every sector of a WHDLoad volume to change. Unused bytes are zeroed
and compress well in frontend files. RetroArch keeps several checkpoints for
rollback, so memory usage can be significant, particularly for large WHDLoad
packages. CD contents are referenced, not copied into each checkpoint.

The frontend owns pacing, video output and audio output. Each `retro_run`
advances one hardware video field. Refresh information follows Agnus's actual
timing, including interlace and programmable totals, using Copperline's
emulated colour-clock frequency. It is not rounded to exactly 50 or 60 Hz.
The output uses XRGB8888 pixels, 4:3 display aspect and 44.1 kHz stereo audio.
Standard screens use the shared TV aperture; programmable scans use the
shared full-field presentation. Interlaced output uses line doubling, without
field history or phosphor effects. Frontend shaders can be applied normally.

## Development checks

```sh
cargo fmt --manifest-path crates/copperline-libretro/Cargo.toml --check
cargo clippy --manifest-path crates/copperline-libretro/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path crates/copperline-libretro/Cargo.toml --profile ci --locked
python3 tools/check-libretro.py crates/copperline-libretro/target/release/libcopperline_libretro.dylib --screenshot /tmp/libretro.png
```

Use the matching library suffix on Linux or Windows. The Rust test frontend
boots a 68000 probe that paints a raster pattern and plays a Paula tone. It
compares machine state, framebuffer and audio against direct headless
execution for A500/A1200 and both video standards, then repeats execution after
a state restore. It also checks disk persistence and inactive disk restoration.
The Python frontend loads the actual shared library, exercises its C ABI,
and verifies audio/video replay after save/load. It can write a PNG capture
without a display or audio device. API calls and callbacks run synchronously
on the frontend's emulation thread.

The CD32 tests transfer states between different host directories and cover
all pad buttons. The WHDLoad boot test needs the support archives and an A1200
Kickstart ROM; point `COPPERLINE_LIBRETRO_TEST_SYSTEM` at a prepared system
directory and run:

```sh
cargo test --manifest-path crates/copperline-libretro/Cargo.toml --profile ci whdload_ -- --ignored
```

On Linux, `tools/check-libretro-netplay.py /path/to/copperline_libretro.so`
runs two real RetroArch peers with separate content directories and X displays,
independent controller inputs and delayed loopback traffic. It checks every
frame for desynchronization. Install `retroarch`, `xvfb` and `libxtst6` first.
