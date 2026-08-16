# Coppersynth

Copperline can put a General MIDI sound module on the other end of the
Amiga's MIDI cable. The module is Coppersynth, Copperline's own SoundFont
synthesizer in the style of a Roland Sound Canvas: sixteen parts, an
SC-55-style front panel, and an MT-32 translation layer in front, so a
game that talks to an MT-32 plays correctly with no ROMs and no
configuration.

Audio is mixed in beside the Amiga's own four channels, so a game that
plays MIDI music and Amiga sound effects gets both.

## Turning it on

The serial port has to be in MIDI mode with Coppersynth as its output. In
the launcher, that is the **I/O Ports** tab, Serial section: set
**Device / Mode** to `MIDI`, then **MIDI output** to `Coppersynth`.
Choosing it reveals the rest of the rows: the soundfont, the front panel,
and MT-32 mode. In a running session the same choice is under the menu's
**MIDI Out**, where Coppersynth is always offered. On the command line,
`--midi-out gm` selects it and implies MIDI mode.

In the configuration file:

```toml
[serial]
mode = "midi"
midi_out = "gm"

[gm]
# soundfont = "/path/to/bank.sf2"   # override the built-in bank
# mt32_mode = "auto"                # auto, on, or off
# panel = true                      # start with the front panel shown
```

## Soundfonts

Coppersynth carries its own bank -- **GeneralUser GS** by S. Christian
Collins, an instrument library in its own right with the complete General
MIDI sound set, SFX bank and drum kits included, at a very reasonable
size -- and needs no files. To play a different one, set `[gm] soundfont`,
use the launcher's **Browse**, or press the panel's **LOAD** button in a
running session; `.sf2` files and `.zip` archives containing one both
load, and **Reset** puts the built-in bank back. A bank with defects
(loop points past its data are common in rips) is repaired at load rather
than refused, and the log says what the mending cost. A bank that does
not fill all 128 programs keeps honest numbering: an unfilled slot shows
its number and the name `Empty`, and playback falls back to the bank's
default sound.

## MT-32 mode

Games that address an MT-32 -- uploading instruments over sysex,
expecting its patch numbers and drum map -- are translated to General
MIDI as they play. `auto` (the default) translates once MT-32 sysex is
seen and stands down on a GM or GS reset; `on` forces it; `off` never
translates. The mode can be changed live from the menu (**Coppersynth →
MT-32 Mode**) or at the front panel. When the loaded bank carries the GS
CM-64/32L drum kit, MT-32 rhythm selects it automatically.

## The front panel

**Front panel** in the launcher row, the menu toggle, or `[gm] panel`
puts the module's fascia under the display: the backlit LCD with the part
values, the sixteen-part level meters, and the sound's name -- a game's
MT-32 instrument uploads show under their own names. Buttons press with a
left click; a right click latches a button down, which is how two-button
gestures are made: latch one half of a pair and click the other to view
that setting across all parts, or latch **ALL** and click **MUTE** to
monitor (solo) the shown part.

- **PART ◄ ►** selects a part; **INSTRUMENT ◄ ►** changes its sound (on
  part 10, the drum kit). A change made at the panel holds against the
  game until power-off.
- **LEVEL** is a ceiling on the part's volume, whatever the game sends.
  **PAN**, **REVERB**, **CHORUS**, **KEY SHIFT** and **MIDI CH** edit the
  shown part; with **ALL** lit they set every part at once.
- **MUTE** silences the shown part (all of them, with **ALL** lit). Held
  ◄ ► buttons repeat, faster the longer they are held.
- The **VOLUME** knob is the module's output level, separate from
  anything the game sends. **POWER** switches the module off and on;
  games' sysex display messages appear on the LCD and stay until a button
  is pressed.

Two start-up gestures, made by latching buttons on a switched-off unit
and pressing **POWER**: both **INSTRUMENT** halves restore the built-in
soundfont; **INSTRUMENT ►** asks `MT-32, Sure?` -- **ALL** turns MT-32
mode on, **MUTE** turns it off.

## Coppersynth and the MT-32

Both modules can be configured; one is on the cable at a time. The MT-32
is the real instrument, bit-exact, and needs its ROMs; Coppersynth is the
module for playing without them, and its translation aims to be close
rather than identical -- General MIDI instruments standing in for the
MT-32's. Switching between them from the menu's **MIDI Out** needs no
restart.
