# Bundled HRTMon cartridge image

`hrtmon.rom` is HRTMon 2.39, the Action-Replay-style system monitor by
Alain Malek, maintained by Bert Jahn (wepl) and contributors, assembled in
its UAE cartridge configuration from the upstream source at
<https://github.com/wepl/hrtmon> (commit
`3af8d5105f1e01ecc7961475568826e564995068`). Copperline serves it when a
config fits the cartridge (`[cartridge] model = "hrtmon"`, or
`--cartridge hrtmon`) without naming a `rom` of its own (see
`src/romsearch.rs`); a `rom = "..."` path replaces it with any image that
carries `HRT!` at offset 0 or 4.

The build recipe, the exact vasm flags and the five-line build-time patch
are in the adjacent repository directory `hrtmon-rom/`; rebuild and refresh
this artifact with:

```sh
./hrtmon-rom/build.sh bundle
```

- Size: 199,300 bytes.
- SHA-256: `184c9b12e7e83749d817b1c2692e5a86dffb84dc1bcda0d8c752650dfafe3d74`.

HRTMon is licensed under the GNU General Public License, version 2 or (at
your option) any later version; see `LICENSE` beside this file for the
notice and the full license text. Copperline itself is GPL-3.0-or-later
(the repository root `LICENSE`).
