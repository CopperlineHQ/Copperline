# Munt mt32emu (vendored)

`libmt32emu`, the synthesiser engine behind the Munt project, which is what
lets Copperline answer Paula's MIDI stream as a Roland MT-32 without any
hardware or any host MIDI plumbing.

- Upstream: <https://github.com/munt/munt>
- Vendored from commit `6e7c01fba7e1d50c8fa705834889fd0eac136075` (2026-06-22),
  which describes itself as `munt_2_8_2-10-g6e7c01fb`.
- Licence: LGPL-2.1-or-later (see `LGPL21.txt`, and `GPL2.txt` for the GPL it
  refers to). Compatible with Copperline's GPL-3.0-or-later, which is how it
  can be linked in directly. Authors are credited in `AUTHORS.txt`.
- The Roland ROMs the engine runs on are **not** here and never will be: they
  are Roland's copyright, and the user supplies their own.

These sources are compiled straight into the emulator by `build.rs`, so
`cargo build` produces a binary that speaks MT-32 with nothing to install and
nothing to download. `src/mt32/` on the Rust side calls into them through the
library's own C API.

## What was left out

Everything here is byte-identical to that commit. Four things upstream ships
were simply not copied, none of which the engine needs to run:

- `src/test/` -- the doctest unit suite, which needs doctest and tests
  upstream's own code rather than ours.
- `src/srchelper/SoxrAdapter.*` and `src/srchelper/SamplerateAdapter.*` --
  optional resampler back ends needing libsoxr or libsamplerate. `build.rs`
  defines `MT32EMU_WITH_INTERNAL_RESAMPLER`, so the built-in one under
  `src/srchelper/srctools/` is used and there are no external dependencies.
- `CMakeLists.txt` and the `cmake/` helpers -- `build.rs` compiles the sources
  directly, as it does for FloppyBridge.
- `src/config.h.in` is kept for reference, but CMake is what would fill it in.
  `config.h` beside it is Copperline's static answer: a static build, no
  version tagging, both API flavours available.

## Updating

Copy `mt32emu/src/*` from a newer upstream checkout over `src/`, restore the
omissions above, and update the commit recorded here. `build.rs` picks the
files it compiles by name, so a release that adds or renames a source needs
that list updated too; a release that changes `config.h.in` needs `config.h`
re-checked against it.
