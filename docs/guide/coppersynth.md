# Coppersynth (General MIDI synthesizer)

Copperline includes [Coppersynth](https://github.com/CopperlineHQ/Coppersynth),
a built-in 16-part General MIDI / GS SoundFont synthesizer with a front panel
in the style of a Roland SC-55 and an automatic MT-32 translation layer.

Coppersynth's output is mixed with the Amiga's four Paula channels. It needs
no ROM files and no host MIDI software.

## Configuration

In the launcher:
1. Navigate to **I/O Ports -> Serial Port**.
2. Set **Device / Mode** to `MIDI`.
3. Set **MIDI output** to `Coppersynth`.
4. Optionally set the **SoundFont**, **Front panel**, and **MT-32 mode** rows.

In `copperline.toml`:

```toml
[serial]
mode = "midi"
midi_out = "coppersynth"
# coppersynth_soundfont = "/path/to/bank.sf2"  # Optional custom SoundFont
# coppersynth_mt32_mode = "auto"               # "auto", "on", or "off"
# coppersynth_panel = true                     # Show front panel on boot
```

Command-line usage:

```sh
copperline --model A1200 --midi-out coppersynth KICK31.ROM
```

## `[serial]` configuration keys

| Key | Values | Description |
|---|---|---|
| `midi_out` | `"coppersynth"` | Route MIDI output to built-in synthesizer |
| `coppersynth_soundfont` | File path | Path to `.sf2` SoundFont or `.zip` archive |
| `coppersynth_mt32_mode` | `"auto"`, `"on"`, `"off"` | MT-32 SysEx/patch translation mode (default: `"auto"`) |
| `coppersynth_panel` | `true` / `false` | Display virtual front panel (default: `false`) |

## SoundFonts

Coppersynth has the **GeneralUser GS** SoundFont by S. Christian Collins built
in, with the full General MIDI instrument set, sound effects, and drum kits.

To use another SoundFont (an `.sf2` file, or a `.zip` holding one):
- Set `coppersynth_soundfont = "/path/to/bank.sf2"` in your configuration, or
  pick it on the launcher's **SoundFont** row.
- Click **LOAD** on the front panel, or use **Serial Port -> Coppersynth ->
  SoundFont -> Load...** in the pop-up menu, during a session. **Reset** in
  the same submenu returns to the built-in bank.

With no `coppersynth_soundfont` set, Coppersynth looks for a replacement
bank in the file or directory named by `COPPERLINE_SOUNDFONT`, then for
`GeneralUser-GS.zip` or `GeneralUser-GS.sf2` next to the executable or in a
`share/copperline` directory beside or above it, and otherwise uses the
built-in bank.

## MT-32 translation mode

Many Amiga games with MIDI support target the Roland MT-32. With
`coppersynth_mt32_mode = "auto"` (the default):

- Coppersynth plays incoming MIDI as General MIDI until it sees MT-32 SysEx
  (Roland model ID `$16`). From then on it translates MT-32 patches, custom
  timbre uploads, and rhythm to General MIDI / GS instruments and drum
  mappings, and shows text the game writes to the MT-32 display.
- A GM System On or GS reset message switches translation back off.
- `"on"` translates from the first byte; `"off"` never translates.

## Virtual front panel

![The Coppersynth front panel](../images/ui-preview-csynth-panel-strip.png)

When enabled (`coppersynth_panel = true`), the virtual front panel appears
beneath the video display.

- **Left click:** Press a button.
- **Right click:** Latch a button down for multi-button combinations (up to
  four buttons).

### Primary controls

| Button | Function |
|---|---|
| **PART < >** | Select part (1-16) |
| **INSTRUMENT < >** | Select the current part's instrument |
| **LEVEL < >** | Adjust part volume |
| **PAN < >** | Adjust stereo panning |
| **REVERB < >** | Adjust reverb send level |
| **CHORUS < >** | Adjust chorus send level |
| **KEY SHIFT < >** | Transpose part pitch |
| **ALL** | While lit, LEVEL, PAN, REVERB, CHORUS and KEY SHIFT change all 16 parts at once, and MIDI CH sets the device ID (1-32) |
| **MUTE** | Mute the selected part (or all parts if `ALL` is lit) |
| **MIDI CH < >** | Set MIDI channel for current part (1-16 or Off) |
| **LOAD** | Choose another SoundFont |
| **VOLUME** | Main output level |
| **POWER** | Switch the synthesizer off or on |

### Multi-button combinations (Power on)

| Combination | Function |
|---|---|
| `ALL` + `MUTE` | Solo the selected part; with `ALL` lit, sound every part, muted ones included |
| `PART <` + `PART >` | Part parameter menu (bend range, vibrato, filter cutoff, portamento, and more) |
| `PART <` + `PART >` (with `ALL`) | System menu (master tune, reverb and chorus types, bar display and peak hold, receive switches) |
| `INSTRUMENT <` + `INSTRUMENT >` | Instrument variation bank selection |
| Both halves of `LEVEL`, `PAN`, `REVERB`, `CHORUS`, `KEY SHIFT` or `MIDI CH` | Show that setting for all 16 parts on the bar display |

### Multi-button combinations (Power off)

With power turned off, latch the buttons (right-click) and click `POWER`:

| Combination | Function |
|---|---|
| `INSTRUMENT <` + `POWER` | "Init MT-32?": `ALL` turns MT-32 translation on, `MUTE` turns it off |
| `INSTRUMENT >` + `POWER` | "Init GS?": `ALL` returns to the GS basic setting, `MUTE` carries on |
| `INSTRUMENT <` + `INSTRUMENT >` + `POWER` | "Init All?": `ALL` restores the factory settings and the built-in SoundFont |
| `PART <` + `PART >` + `POWER` | Demo mode: `ALL` plays, `MUTE` stops, `PART` picks the song |
| Both halves of `INSTRUMENT` and of `MIDI CH` + `POWER` | Version and credits screen |

Coppersynth keeps its settings in battery-backed memory, stored as
`coppersynth.nvram` in the NVRAM folder (`[paths] nvram`) when the synthesizer
is switched off or the session ends. `--factory` neither reads nor writes it.

## Runtime controls

While Coppersynth is the MIDI output, the pop-up menu's **Serial Port ->
Coppersynth** submenu toggles the **Front Panel**, loads or resets the
**SoundFont**, and sets the **MT-32 Mode** (**Auto**, **On**, or **Off**).
**Serial Port -> MIDI Out** switches between Coppersynth, the MT-32, and host
MIDI ports.

`COPPERLINE_COPPERSYNTH_CAPTURE=FILE` writes every byte the guest sends to
Coppersynth into `FILE`, for replaying a session's MIDI stream offline.

## Building without Coppersynth

To exclude Coppersynth when compiling from source, leave out the
`coppersynth` feature (see [Cargo features](getting-started.md#cargo-features)):

```sh
cargo build --release --no-default-features \
  --features "midi,frontend,wasm-boards,control,ctl-bin,import-uae-bin,net-nat,net-bridge,fluxbridge,mt32,cpu-jit,profile-stats,game-library,mhi,cd-mp3,cd32-fmv,gdb,dap,netplay-internet,host-serial"
```
