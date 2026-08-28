# Coppersynth (General MIDI synthesizer)

Copperline includes Coppersynth, an integrated 16-part General MIDI / GS SoundFont
synthesizer with an interactive Roland SC-55 style front panel and an automatic
MT-32 translation layer.

Coppersynth mixes its output alongside the Amiga's four Paula audio channels
without requiring external ROM files or MIDI daemon software.

## Configuration

In the launcher:
1. Navigate to **I/O Ports -> Serial Port**.
2. Set **Device / Mode** to `MIDI`.
3. Set **MIDI output** to `Coppersynth`.
4. Configure SoundFont, MT-32 translation mode, and front panel options as desired.

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

Coppersynth includes the **GeneralUser GS** SoundFont by S. Christian Collins by
default, providing complete General MIDI instruments, SFX, and drum sets.

To use an alternate SoundFont:
- Set `coppersynth_soundfont = "/path/to/bank.sf2"` in your configuration.
- Click **LOAD** on the virtual front panel during an active session.
- Select a SoundFont file from the launcher settings tab.

## MT-32 translation mode

Many Amiga games with MIDI support target the Roland MT-32 specifically. When
`coppersynth_mt32_mode = "auto"` is active:

- MT-32 SysEx messages and patch numbers are automatically translated to corresponding
  General MIDI / GS instruments and drum mappings.
- If standard General MIDI or GS reset messages are received, translation mode
  disengages automatically.
- Translation can be explicitly forced (`"on"`) or disabled (`"off"`).

## Virtual front panel

![The Coppersynth front panel](../images/ui-preview-csynth-panel-strip.png)

When enabled (`coppersynth_panel = true`), the virtual front panel appears
beneath the video display.

- **Left click:** Press a button.
- **Right click:** Latch/hold a button down for multi-button shortcuts.

### Primary controls

| Button | Function |
|---|---|
| **PART < >** | Select part (1-16) |
| **INSTRUMENT < >** | Select timbre/instrument for current part |
| **LEVEL < >** | Adjust part volume |
| **PAN < >** | Adjust stereo panning |
| **REVERB < >** | Adjust reverb send level |
| **CHORUS < >** | Adjust chorus send level |
| **KEY SHIFT < >** | Transpose part pitch |
| **ALL** | When active, edits apply to all 16 parts simultaneously |
| **MUTE** | Mute the selected part (or all parts if `ALL` is lit) |
| **MIDI CH < >** | Set MIDI channel for current part (1-16 or Off) |
| **VOLUME** | Main output level |
| **POWER** | Power cycle the synthesizer engine |

### Multi-button combinations (Power on)

| Combination | Function |
|---|---|
| `ALL` + `MUTE` | Solo the selected part |
| `PART <` + `PART >` | Part parameter edit menu (Bend range, vibrato, filter cutoff, portamento) |
| `PART <` + `PART >` (with `ALL`) | System setup menu (Master tune, reverb/chorus DSP types, meter modes) |
| `INSTRUMENT <` + `INSTRUMENT >` | Instrument variation bank selection |

### Multi-button combinations (Power off)

With power turned off, hold combinations (right-click) and click `POWER`:

| Combination | Function |
|---|---|
| `INSTRUMENT <` + `POWER` | Toggle MT-32 translation mode |
| `INSTRUMENT >` + `POWER` | Reset parameters to GS factory standard |
| `INSTRUMENT <` + `INSTRUMENT >` + `POWER` | Master factory reset (restores default SoundFont) |
| `PART <` + `PART >` + `POWER` | Play built-in demo sequences |
| `INSTRUMENT` + `MIDI CH` + `POWER` | Display version and credits info |

## Building without Coppersynth

To exclude Coppersynth when compiling from source:

```sh
cargo build --release --no-default-features \
  --features "midi,frontend,wasm-boards,control,ctl-bin,import-uae-bin,net-nat,net-bridge,fluxbridge,mt32,cpu-jit,profile-stats,game-library,mhi,cd-mp3,cd32-fmv,gdb"
```
