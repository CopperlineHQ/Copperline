# The oracle

The reference C++ engine the differential tests measure this crate against.

- `munt/` is `libmt32emu`'s sources, byte-identical to upstream commit
  `6e7c01fba7e1d50c8fa705834889fd0eac136075` with the same four omissions
  Copperline's vendored copy makes (its `README.md` inside lists them).
  Nothing here is compiled into the crate: only the test harness, behind
  the `oracle` cargo feature, builds it.
- `shim.cpp` is the C surface the tests drive it through: open on ROM
  bytes, play, render, read the display and memory.

The one deliberate divergence is made by `build.rs`, not in the sources:
`rand` is renamed to the shim's `mt32_oracle_rand`, the C standard's
example LCG, reseeded on every open. The engine uses it to jitter the
pitch-envelope process timer as the hardware's timer jitters; libc's
`rand()` is process-global and differs by platform, which would make
identical runs differ. The Rust engine implements the same generator, so
both sides jitter identically and the comparison stays sample-exact.
