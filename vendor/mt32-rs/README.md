# mt32-rs

Roland MT-32 sound module emulation in safe Rust: a port of the synthesis
engine from [Munt](https://github.com/munt/munt)'s `mt32emu`, reduced to
what emulating the module actually needs. ROM images go in as bytes, MIDI
goes in as bytes, stereo samples come out. No file abstraction, no C API
layer, no host plumbing, no `unsafe`.

Written for the [Copperline](https://github.com/CopperlineHQ/Copperline)
Amiga emulator, which vendors this crate and emulates its MT-32 through it;
this repository is the upstream. It stands alone all the same: nothing in
it knows what an Amiga is. A sibling of
[FluxBridge](https://github.com/CopperlineHQ/FluxBridge), the same
treatment given to FloppyDriveBridge.

## Status

The whole sound path -- LA32, envelopes, voice allocation, the Boss reverb, 
and all four of the analogue output models -- renders bit-identically to Munt, 
proven by the differential harness below across every control ROM the module 
family shipped: both MT-32 generations, the CM-32L/LAPC-I, and the CM-32LN. 

The MIDI stream parser, the ROM demonstration songs, and the dump replies on 
the module's MIDI OUT are in. A minute of module time renders in about half a 
second on one core, slightly ahead of the reference engine.

## Use

```rust
use mt32_rs::{analog::AnalogMode, engine::Engine, midi::Parser};

let mut engine = Engine::open_with_analog(&control_rom, &pcm_rom, AnalogMode::Accurate)
    .expect("a known ROM pair");
let mut parser = Parser::new();

// Raw bytes, straight off the wire: running status, SysEx and all.
parser.parse(&midi_bytes, &mut engine);

// Stereo frames at the analogue model's rate -- 48 kHz for Accurate,
// the module's native 32 kHz for DigitalOnly.
let mut frames = vec![(0i16, 0i16); 4096];
engine.render(&mut frames);

// Everything on the module's MIDI OUT is an answer: dump replies to
// the RQ1 requests a librarian or patch editor sends.
let replies = engine.take_midi_out();
```

## Features

- The full module family: MT-32 (both generations), CM-32L, LAPC-I and
  CM-32LN, each firmware's quirks preserved down to its arithmetic bugs.
- The analogue output stage in the reference's four models, from the bare
  digital stream to the accurate circuit with the mirror spectra the real
  converter let through.
- The front door a serial line feeds: a MIDI byte-stream parser tolerant
  of everything a guest machine emits.
- The LCD and MIDI MESSAGE lamp, the demonstration songs the
  second-generation ROMs carry, and the RQ1-to-DT1 dump replies the
  reference engine never implemented.
- Deterministic: the same input renders the same output, byte for byte,
  on every run. Even the firmware's pitch-envelope jitter is a defined
  generator rather than platform noise.

## Fidelity

The bar is bit-identical output to Munt at the native sample rate. Munt
is the community's accepted yardstick for how close to real hardware the
emulation is -- the module's own DAC runs at 32 kHz, and Munt's integer
synthesis path reproduces it -- so matching Munt exactly is what "sounds
like an MT-32" means here, and it turns the question into one a test can
answer.

The `oracle` cargo feature builds the reference C++ engine (under
`oracle/munt`, byte-identical to the upstream commit below) into the test
harness, and the differential tests drive both engines with identical
ROMs and MIDI and compare the PCM sample by sample. A normal build of the
crate carries no C++ at all.

One substitution, made at compile time so the sources stay untouched: the
engine jitters its pitch-envelope timer with libc `rand()`, which is
process-global and platform-dependent -- either alone would break "the
same input renders the same output". The oracle build renames that call
to a defined generator in the harness (`oracle/README.md`), and the Rust
engine mirrors the same generator, so the jitter itself is ported rather
than averaged away.

## ROMs

The engine runs on control and PCM ROM images from a real unit. They are
Roland's copyright and are never distributed here; tests that need them
are `#[ignore]`d and look for a local pair under `MT32_RS_ROMS`
(`MT32_CONTROL.ROM` and `MT32_PCM.ROM`).

```sh
MT32_RS_ROMS=~/roms cargo test --features oracle -- --ignored
```

## Related work

[Moont](https://gitlab.gnome.org/geoffhill/moont) is an independent Rust
port of Munt's CM-32L emulation, LGPL-2.1-or-later like this crate, which
validates its output against Munt sample for sample -- proof the bar this
project sets is reachable. It emulates the CM-32L only; this crate exists
for the whole MT-32 family, elder quirks included. Where its solutions
inform this code, credit goes in `AUTHORS.txt`.

## Licence and provenance

LGPL-2.1-or-later, as a derivative work of `mt32emu`:

- Upstream: <https://github.com/munt/munt>, ported from commit
  `6e7c01fba7e1d50c8fa705834889fd0eac136075` (2026-06-22,
  `munt_2_8_2-10-g6e7c01fb`).
- Copyright (C) 2003-2026 Dean Beeler, Jerome Fisher, Sergey V. Mikayev
  and the contributors credited in `AUTHORS.txt`, whose work this is a
  translation of; Rust port copyright its own contributors.
- Licence texts: `LGPL21.txt`, and `GPL2.txt` which it refers to.
