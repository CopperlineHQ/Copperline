# mt32-rs

MT-32 sound module emulation in Rust: a port of the synthesis engine from
[Munt](https://github.com/munt/munt)'s `mt32emu`, reduced to what emulating
the module actually needs. ROM images go in as bytes, MIDI goes in as bytes,
stereo samples come out at the module's native 32 kHz. No file abstraction,
no C API layer, no host plumbing.

Written for the [Copperline](https://github.com/CopperlineHQ/Copperline)
Amiga emulator, which today carries the C++ engine vendored; this crate is
the replacement, pure Rust so it goes everywhere Copperline goes, including
the browser build the C++ engine cannot reach. It stands alone all the same:
nothing in it knows what an Amiga is.

## Fidelity

The bar is bit-identical output to Munt at the native sample rate. Munt is
the community's accepted yardstick for how close to real hardware the
emulation is -- the module's own DAC runs at 32 kHz, and Munt's integer
synthesis path reproduces it -- so matching Munt exactly is what "sounds
like an MT-32" means here, and it turns the question into one a test can
answer.

The `oracle` cargo feature builds the reference C++ engine (under
`oracle/munt`, byte-identical to the upstream commit below) into the test
harness, and the differential tests drive both engines with identical ROMs
and MIDI and compare the PCM sample by sample. A normal build of the crate
carries no C++ at all.

One substitution, made at compile time so the sources stay untouched: the
engine jitters its pitch-envelope timer with libc `rand()`, which is
process-global and platform-dependent -- either alone would break "the same
input renders the same output". The oracle build renames that call to a
defined generator in the harness (`oracle/README.md`), and the Rust engine
mirrors the same generator, so the jitter itself is ported rather than
averaged away.

## ROMs

The engine runs on control and PCM ROM images from a real unit. They are
Roland's copyright and are never distributed here; tests that need them are
`#[ignore]`d and look for a local pair under `MT32_RS_ROMS`
(`MT32_CONTROL.ROM` and `MT32_PCM.ROM`).

```sh
MT32_RS_ROMS=~/roms cargo test --features oracle -- --ignored
```

## Licence and provenance

LGPL-2.1-or-later, as a derivative work of `mt32emu`:

- Upstream: <https://github.com/munt/munt>, ported from commit
  `6e7c01fba7e1d50c8fa705834889fd0eac136075` (2026-06-22,
  `munt_2_8_2-10-g6e7c01fb`).
- Copyright (C) 2003-2026 Dean Beeler, Jerome Fisher, Sergey V. Mikayev and
  the contributors credited in `AUTHORS.txt`, whose work this is a
  translation of; Rust port copyright its own contributors.
- Licence texts: `LGPL21.txt`, and `GPL2.txt` which it refers to.

## Related work

[Moont](https://gitlab.gnome.org/geoffhill/moont) is an independent Rust
port of Munt's CM-32L emulation, LGPL-2.1-or-later like this crate, which
validates its output against Munt sample for sample -- proof the bar this
project sets is reachable. It emulates the CM-32L only; this crate exists
for the whole MT-32 family, elder quirks included, shaped for Copperline.
Where its solutions inform this code, credit goes in `AUTHORS.txt`.

## House style

Copperline is the north star: its comment voice, test naming, module shape
and gates apply here unchanged -- `cargo fmt --check`, `cargo clippy
--all-targets --all-features -- -D warnings`, and tests that read as
sentences. The crate is destined to sit inside Copperline the way the C++
engine does today, with the difference that this upstream is ours.

## Status

| Phase | What | State |
|---|---|---|
| 0 | Scaffold, licences, oracle harness | done |
| 1 | ROM identification and table extraction | done |
| 2 | Memory model, SysEx, display strings | done |
| 3 | The sound path (LA32, envelopes, parts, reverb, analog) | done: notes, reverb and all four analogue models render bit-identical |
| 4 | MIDI front end and the Copperline seam | done: stream parser, demo songs, dump replies |
| 5 | Copperline integration, wasm | -- |
