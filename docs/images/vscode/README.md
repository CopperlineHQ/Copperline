# VS Code screenshots

These are original, unedited desktop captures from the Copperline debugging
showcase on 5 September 2026. They are committed at capture resolution so
readers can open the images to inspect small text.

- `01`–`04`: Bartman source debugging and captured-frame views of its bundled
  Amiga C template, using Copperline with the integration fixes developed
  during the session.
- `05`: Copperline's native DAP adapter, demonstrating a two-file relocatable
  ELF/elf2hunk program with DWARF, variables and instruction step-back.
- `06`: The final Bartman/Copperline combination, showing the inferred Copper
  bitmap and Copper list. Bartman revision:
  `7d18d370a030da7d365238e0d700f2be303214ce`; Copperline startup fix:
  `9d1ebc5f4d247d514f111a20f767231c35ff53b3` (merged in `3d334a11`).

The Bartman demo uses PAL A500, 68000, 1 MiB chip RAM, 512 KiB slow RAM and
the bundled AROS ROM. Music was disabled; the graphics came from Bartman's
template, including its Abyss artwork. The live window's debug overlay was
customized for the showcase. The native DAP fixture was built with `-g -O0`;
the graphics template retained its optimized build.

Recreate the interaction with `docs/debugger/vscode.md` and
`docs/debugger/vscode-bartman.md`. Source lines, addresses, local paths,
profile values and the desktop layout can differ between builds. Unlike
headless framebuffer fixtures, these desktop images also include IDE state
and are not byte-for-byte regeneration tests. No ROMs or guest disk images
are included here.
