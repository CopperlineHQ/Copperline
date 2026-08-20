# Coppersynth

Copperline can put a "General MIDI" sound module on the other end 
of the Amiga's MIDI cable. 

The module is Coppersynth, Copperline's own SoundFont
synthesizer in the style of a Roland Sound Canvas: sixteen parts, an
SC-55-style front panel, and an MT-32 translation layer in front, so a
game that talks to an MT-32 plays correctly **with no ROMs and no
configuration**.

Audio is mixed in beside the Amiga's own four channels, so a game that
plays MIDI music and Amiga sound effects gets both.

## Turning it on

The serial port has to be in MIDI mode with Coppersynth as its output. In
the launcher, that is the **I/O Ports** tab (its Serial Port page): set
**Device / Mode** to `MIDI`, then **MIDI output** to `Coppersynth`.
Choosing it reveals the rest of the rows: the SoundFont, the front panel,
and MT-32 mode. In a running session the same choice is under the menu's
**MIDI Out**, where Coppersynth is always offered. On the command line,
`--midi-out coppersynth` selects it and implies MIDI mode.

In the configuration file:

```toml
[serial]
mode = "midi"
midi_out = "coppersynth"
# coppersynth_soundfont = "/path/to/bank.sf2"  # override the built-in bank
# coppersynth_mt32_mode = "auto"               # auto, on, or off
# coppersynth_panel = true                     # start with the front panel shown
```

## `[serial]` keys

| Key | Values | Meaning |
|---|---|---|
| `midi_out` | `"coppersynth"` | Play to the built-in synthesizer instead of a host endpoint |
| `coppersynth_soundfont` | path | A bank to play instead of the built-in one (`.sf2`, or a `.zip` holding one) |
| `coppersynth_mt32_mode` | `"auto"`, `"on"`, `"off"` | How MT-32 traffic is translated (default `"auto"`) |
| `coppersynth_panel` | `true`/`false` | Show the front panel (default `false`) |

## SoundFonts

Coppersynth carries its own bank -- **GeneralUser GS** by S. Christian
Collins, an instrument library in its own right with the complete General
MIDI sound set, SFX bank and drum kits included, at a very reasonable
size -- and needs no extra files. To play a different one, set
`[serial] coppersynth_soundfont`,
use the launcher's **Browse**, or press the panel's **LOAD** button in a
running session; `.sf2` files and `.zip` archives containing one both
load, and the launcher's **Clear** (the menu's **Reset**) puts the
built-in bank back. A bank that does not fill all 128 programs keeps 
honest numbering: an unfilled slot shows its number and the name `Empty`, 
and playback falls back to the bank's default sound.

## MT-32 mode

Games that address an MT-32 -- uploading instruments over sysex,
expecting its patch numbers and drum map -- are translated to General
MIDI as they play. `auto` (the default) translates once MT-32 sysex is
seen and stands down on a GM or GS reset; `on` forces it; `off` never
translates. The mode can be changed live from the menu (**Coppersynth →
MT-32 Mode**) or at the front panel. When the loaded bank carries the GS
CM-64/32L drum kit, MT-32 rhythm selects it automatically.

## The Front Panel

![The Coppersynth front panel](../images/ui-preview-csynth-panel-strip.png)

**Front panel** in the launcher row, the menu toggle, or
`[serial] coppersynth_panel`
puts the module's fascia under the display: the backlit LCD with the part
values, the sixteen-part level meters, and the sound's name -- a game's
MT-32 instrument uploads via SysEx to show extra info. Buttons press with a
left click; a right click latches a button down, which is how multi-button
gestures are made. The module remembers its settings across power cycles,
like the real unit's battery does.

These button combinations / settings mostly reference an actual SC-55, so you
could also reference the official SC-55 MKII manual for more info on some of 
these settings and parameters. 

| Button | Function |
|---|---|
| **PART < >** | Selects a part |
| **INSTRUMENT < >** | Changes the timbre/sound for the selected part (drum kits on a drum part) |
| **LEVEL < >** | Volume ceiling |
| **PAN < >** | Pans the part left or right |
| **REVERB < >** | Reverb DSP level |
| **CHORUS < >** | Chorus DSP level |
| **KEY SHIFT < >** | Transposes the part |
| **ALL** | Lit, sets all of the above parameters for every part |
| **MUTE** | Silences the shown part (all of them, with **ALL** lit) |
| **MIDI CH < >** | Sets the MIDI channel (1-16, Off) for the selected part; with **ALL** lit, the SysEx Device ID (1-32) |
| **VOLUME** (knob) | The module's main output level, separate from anything MIDI sends |
| **POWER** | Switches the module off and on |

### Button combinations (POWER ON)

Coppersynth front panel has various features accessed with multi-button
combinations. The table below will separate these into ones which are
accessed with the unit powered on.

```
Hold = Right Click
Press = Left Click
```

| Combination | Reaches |
|---|---|
| Hold `ALL` + `MUTE` (either order, hold or press) | Solo the selected PART -- the MUTE lamp blinks. Press `MUTE` to disable solo |
| Hold `PART <` + `PART >` | The part settings menu, opening on Part Mode (Norm/Drum). `MUTE` steps forward through the settings (Bend Range, Vib. Rate, Cutoff Freq, Portamento and the rest), `ALL` steps back, `INSTRUMENT` < or > changes the value, `PART` < or > moves between parts. Press both `PART` buttons again to leave. These can also be controlled via standard MIDI CC/NRPN commands |
| Hold `PART <` + `PART >` with `ALL` lit | The system menu: master tune, the Reverb and Chorus DSP types, the meter display styles, reception switches and Back Up. Same controls as above |
| Hold `INSTRUMENT <` + `INSTRUMENT >` | Variation select: `INSTRUMENT` < or > walks the SoundFont's variation banks for the part's instrument. Press both again to leave |
| Hold both `LEVEL`, `REVERB`, `KEY SHIFT`, `INSTRUMENT`, `PAN`, `CHORUS` or `MIDI CH` buttons | Holding both of any of those buttons at the same time will show their value in the Midi "equaliser" section across all 16 parts |

### Button combinations (POWER OFF)

The following button combinations expect the unit to first be powered off.

| Combination | Reaches |
|---|---|
| Hold `INSTRUMENT <`, Press `POWER` | MT-32 Mode. `MUTE` disables, `ALL` enables |
| Hold `INSTRUMENT >`, Press `POWER` | Init GS: returns every setting to the GS standard. `MUTE` cancels, `ALL` confirms |
| Hold `INSTRUMENT <` + `INSTRUMENT >`, Press `POWER` | Init All: the factory preset, including the default SoundFont. `MUTE` cancels, `ALL` confirms |
| Hold `PART <` + `PART >`, Press `POWER` | Demo sequences. Press `ALL` to play, `MUTE` to stop, `PART` buttons to skip song, `ALL` + `MUTE` to leave |
| Hold both `INSTRUMENT` + both `MIDI CH` | Show version info + credits, `MUTE` or `ALL` skips |

## Building without it

The `coppersynth` Cargo feature, on by default, compiles the synth in.
To build without it:

```sh
cargo build --release --no-default-features \
  --features "midi,frontend,wasm-boards,control,ctl-bin,net-nat,net-bridge,fluxbridge,mt32,cpu-jit,profile-stats,game-library,mhi"
```

This is the normal desktop feature set with only `coppersynth` omitted.
The launcher rows and the MIDI Out entry disappear, and a configuration
naming `midi_out = "coppersynth"` is refused with a warning that says
what to rebuild with.

## Coppersynth and the MT-32

Both modules can be configured; one is on the cable at a time. The MT-32
is the real instrument, bit-exact, and needs its ROMs; Coppersynth is the
module for playing without them, and its translation aims to be close
rather than identical -- General MIDI instruments standing in for the
MT-32's. Switching between them from the menu's **MIDI Out** needs no
restart.
