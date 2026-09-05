# Roland MT-32 emulation

Copperline includes built-in Roland MT-32 emulation via
[mt32-rs](https://github.com/CopperlineHQ/mt32-rs), a Rust port of Munt's
`mt32emu`. Emulated MIDI audio is mixed alongside the Amiga's native four-channel
Paula output.

An interactive virtual front panel can be displayed beneath the video output.

## ROM requirements

To use MT-32 emulation, you must provide two ROM images:

1. **Control ROM** (e.g., v1.07 or v2.07)
2. **PCM ROM** (sample data)

Supported modules include MT-32 (revisions 1 and 2), CM-32L, LAPC-I, and CM-32LN.

### Example ROM hashes

| Filename | Version | SHA-1 Hash |
|---|---|---|
| `mt32_ctrl_2_07.rom` | v2.07 | `47b52adefedaec475c925e54340e37673c11707c` |
| `MT32_CONTROL.ROM` | v1.07 | `b083518fffb7f66b03c23b7eb4f868e62dc5a987` |
| `MT32_PCM.ROM` | All | `f6b1eebc4b2d200ec6d3d21d51325d5b48c60252` |

ROMs are verified by size and SHA-1 checksum upon loading.

## Configuration

In the launcher:
1. Navigate to **I/O Ports -> Serial Port**.
2. Set **Device / Mode** to `MIDI`.
3. Set **MIDI output** to `MT-32`.
4. Select paths for **Control ROM** and **PCM ROM**.
5. Optionally enable **Front panel** and select an LCD display style.

In `copperline.toml`:

```toml
[serial]
mode = "midi"
midi_out = "mt32"
mt32_control_rom = "/path/to/MT32_CONTROL.ROM"
mt32_pcm_rom = "/path/to/MT32_PCM.ROM"
mt32_panel = true          # Show front panel (default: false)
mt32_lcd = "mt32"          # Display style: mt32, superjv, sseries, or oled
```

Command-line usage:

```sh
./target/release/copperline --model A1200 KICK31.ROM \
  --midi-out mt32 \
  --mt32-control-rom MT32_CONTROL.ROM \
  --mt32-pcm-rom MT32_PCM.ROM \
  --mt32-panel
```

## `[serial]` configuration keys

| Key | Values | Description |
|---|---|---|
| `midi_out` | `"mt32"` | Route serial MIDI output to built-in MT-32 |
| `midi_in` | `"mt32"` | Wire MT-32 MIDI OUT back to serial input |
| `mt32_control_rom` | File path | Path to Control ROM image |
| `mt32_pcm_rom` | File path | Path to PCM ROM image |
| `mt32_panel` | `true` / `false` | Display virtual front panel (default: `false`) |
| `mt32_lcd` | `"mt32"`, `"superjv"`, `"sseries"`, `"oled"` | LCD appearance style (default: `"mt32"`) |

(patch-editors)=
### Patch editors and librarians

Software such as patch editors and librarians (e.g., Caged Artist MT-32 Editor)
send SysEx requests and expect responses. Set `midi_in = "mt32"` to route the
virtual synthesizer's MIDI output back into the Amiga's serial port.

## Virtual front panel

![The MT-32 front panel](../images/ui-preview-mt32-panel-strip.png)

When `mt32_panel = true` is set, a reproduction of the MT-32 hardware panel
is shown below the video output:

- **Left click:** Press a button.
- **Right click:** Latch/hold a button down (used for multi-button combinations).

### Panel controls

| Control | Function |
|---|---|
| **PART 1-5, PART R** | Select part to configure |
| **SOUND GROUP** | Cycle sound group (Preset A, Preset B, Memory, Rhythm) |
| **SOUND** | Select patch number (0-63) |
| **VOLUME** | Adjust volume for selected part (0-100) |
| **MASTER VOLUME** | Adjust overall synthesizer output level |
| **Dial** | Adjust parameter values (click or drag) |
| **POWER** | Power cycle the synthesizer engine |

### Multi-button combinations (Powered on)

Hold `MASTER VOLUME` (right-click), then click the corresponding button:

| Combination | Function |
|---|---|
| `MASTER VOLUME` + `GROUP` | Adjust master tuning (427.5 to 452.6 Hz) |
| `MASTER VOLUME` + `VOLUME` | Select reverb mode (0-3) |
| `MASTER VOLUME` + `SOUND` | Set MIDI device unit number |
| `MASTER VOLUME` + `PART 1, 2, 3` | Select parts 6, 7, and 8 |
| `MASTER VOLUME` + `PART 4` | Overflow assign mode (confirm with `PART 1`) |
| `MASTER VOLUME` + `PART 5` | Remap parts 1-8 to MIDI channels 1-8 (confirm with `PART 1`) |
| `MASTER VOLUME` + `PART R` | System master reset (confirm with `PART 1`) |

### Startup combinations (Power off)

With power off, right-click button combination and click `POWER`:

| Combination | Function |
|---|---|
| `MASTER VOLUME` + `POWER` | Built-in demo sequence (requires v2.x ROM) |
| `1` + `3` + `MASTER VOLUME` + `POWER` | ROM test / firmware version display |

### LCD display styles

| Value | Appearance | Preview |
|---|---|---|
| `mt32` | Classic 1987 green-backlit LCD | ![MT-32 LCD](../images/ui-preview-mt32-lcd-mt32.png) |
| `superjv` | Orange-backlit display | ![SuperJV LCD](../images/ui-preview-mt32-lcd-superjv.png) |
| `sseries` | Fluorescent display styling | ![SSeries LCD](../images/ui-preview-mt32-lcd-sseries.png) |
| `oled` | High-contrast white OLED styling | ![OLED LCD](../images/ui-preview-mt32-lcd-oled.png) |

(mt32-window)=
## Runtime controls

In an active emulation window, the top menu bar under **Serial Port** allows
switching MIDI output between MT-32, Coppersynth, or host MIDI ports without
restarting emulation.

ROM images can be loaded or changed dynamically from the **Serial Port -> MT-32**
menu.

## Building without MT-32 support

To exclude MT-32 support when compiling from source:

```sh
cargo build --release --no-default-features \
  --features "midi,frontend,wasm-boards,control,ctl-bin,import-uae-bin,net-nat,net-bridge,fluxbridge,coppersynth,cpu-jit,profile-stats,game-library,mhi,cd-mp3,cd32-fmv,gdb,dap"
```

## Troubleshooting

- **`MT-32: missing ROM(s)`:** One or both ROM paths are missing or invalid.
- **`MT-32: invalid ROM(s)`:** Provided files failed SHA-1 verification.
- **Diagnostic logging:** Set `COPPERLINE_MT32_DEBUG=1` or `COPPERLINE_MIDI_DEBUG=1`
  in the environment to log MIDI message flow.
