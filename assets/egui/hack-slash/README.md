# Hack Slash Regular

This is the Hack Regular font bundled by `epaint_default_fonts` 0.35.0, with
only U+0030 (`0`) replaced by Source Foundry's forward-slash design. Character
advances, line metrics, all other glyphs, and their hinting are preserved. The
replacement zero is converted from cubic to quadratic curves with a maximum
error of one font unit (the font uses 2,048 units per em). It has no TrueType
hinting instructions.

The family is renamed **Hack Slash** to distinguish the modified font. Its
licences and credits are in `../THIRD_PARTY_FONTS.txt`, which also ships in
packaged builds. `LICENSE.txt` is the unmodified alt-hack licence.

## Sources

- Base: `fonts/Hack-Regular.ttf` from
  [epaint_default_fonts 0.35.0](https://crates.io/crates/epaint_default_fonts/0.35.0),
  Hack 3.003.
- Replacement: `glyphs/u0030-forwardslash/regular/zero.glif` from
  [source-foundry/alt-hack](https://github.com/source-foundry/alt-hack/tree/9fab7328cfc208046cb7f3d713ec97a724d8219a/glyphs/u0030-forwardslash),
  revision `9fab7328cfc208046cb7f3d713ec97a724d8219a`. The GLIF is included unmodified.

SHA-256 checksums:

```text
15f55cc0c85a2988d2b4b3a8cdb5d77fdfbaf319e1bb5309d725db9818fb7125  Hack-Regular.ttf
42a26975f5588546de20b04f2ab3c05a5add45ba6952efd4d155ea5ab3d43a24  zero.glif
7961b8c43b17917248f3cfaef8db712dc44edf2956415228fdee8d609438186c  HackSlash-Regular.ttf
```

## Rebuild

Normal Copperline builds use the checked-in TTF and need no Python packages.
To reproduce it, install `fonttools==4.64.0` in a Python virtual environment,
then run:

```sh
python build.py /path/to/epaint_default_fonts-0.35.0/fonts/Hack-Regular.ttf
```

The base font is available in Cargo's registry cache after `cargo fetch`.
The script rejects a different base and verifies that only the zero outline
changed, with every glyph advance preserved. Font timestamps come from the
base font, so repeated builds with the pinned FontTools version are identical.
