# In-repo reference documents

Timing behaviour is documented next to the code, backed by named
regression tests. Consult and update these documents when changing the
corresponding model. The Copper and blitter timing models (fetch cadence,
MOVE write boundary, WAIT/SKIP edge cases, the per-slot blitter FSM,
mid-blit register classification, area fill, ECS extensions, and the known
residuals) are documented in [](timing.md), which also covers the real-time
pacing model (`cycles` vs `instructions`); the 68000 prefetch queue and the
020+ cache model are in [](cpu.md). Full ECS, the A600/A1200 machine
profiles and Gayle, and the AGA display path are implemented; their
remaining gaps are recorded next to the subsystem they belong to
([](chipset.md), [](video.md), and [](cpu.md)). The remaining reference
material lives in the repository:

`timing-test/`
: Not a document but the measurement tool behind several of them: a
  bootable disk that times CPU/chip-bus operations against the CIA
  E-clock, comparable across Copperline, vAmiga, FS-UAE, and real
  hardware. The directory also holds the golden-render probe bootblocks
  that `tests/probe_golden.rs` checks; `timing-test/README.md` describes
  both, row by row.

`../index.md`
: The public project overview ([](../index.md)), including the
  hardware-first compatibility principle: model the chip behaviour instead
  of branching on individual software titles.

`README.md`
: The repository front page: feature summary, installation, and build
  instructions.

`AGENTS.md` (also `CLAUDE.md`, a symlink to it)
: Guidance for AI coding agents and scripted use: building, headless
  verification, scripted input, save states, the control protocol, and the
  rules for changes to Copperline itself.

`CONTRIBUTING.md` and `SECURITY.md`
: The contribution rules (hardware-first changes, no copyrighted assets,
  the asset-free checks to run) and how to report security issues.

`RELEASE.md`
: The release checklist: version bump, checks, and packaging for
  Homebrew, AppImage, Windows, the macOS disk image, and the
  libretro cores.

`CREDITS.md`, `CHANGELOG.md`, and `FUNDING.md`
: Contributors and sponsors, the release changelog (also rendered in
  [](../changelog.md)), and where project funding goes.

`docs/internals/jit-performance.md`
: Reproducible performance measurement methodology and the CI AROS
  performance gate ([Measuring performance](jit-performance.md)).

`docs/toaster/README.md` and `crates/flyerclip/FORMAT.md`
: Recovered hardware specifications for the Video Toaster 4000 video-slot card
  and NewTek Video Flyer (DVE, video path, key alignment, protocol), and the
  VTASC / Flyer clip container format reference.

`docs/internals/a500-*.md` and `docs/internals/m68000-divider-review-2026-10-15.md`
: Hardware-measurement notes from real PAL A500 probe campaigns (CPU/bus
  timing, divider review, IRQ focus, blitter/Copper/refresh arbitration,
  CIA output/serial timing, bitplane pointer collisions).

`copperline.example.toml`
: The commented configuration reference, the companion to
  [](../guide/configuration.md). `a1000.example.toml`,
  `picasso2.example.toml`, and `graffity.example.toml` are ready-made
  machine configurations for the A1000 bootstrap and the two RTG boards.

`tests/README.md`
: The integration-test asset contract: what each asset-gated test needs,
  where assets are looked up, and how to obtain them legally.

`COPPERHF-DEVICE-PLAN.md`
: The M1-M6 milestone plan for `copperhf.device`, which its source
  comments and test names refer to (see [](copperhf.md)).

`HARDWARE-RIG-PLAN.md`
: The plan for a hardware-in-the-loop reference rig that runs the
  `timing-test/` probe server on a real Amiga, with the status of the
  tooling already built (`tools/hwrig/`).

## Debugger ABI reference data

The live ROM symbol resolver uses only public ABI names from the AROS module
configuration files, generated into `assets/symbols/amigaos-lvo.tsv` by
`tools/generate-amigaos-lvos.py`. The current table is from AROS commit
`d13e9e537f9e6f53e5fc255899c0e234be5d5ee2`, also pinned in its header;
`assets/symbols/LICENSE.AROS` carries the AROS Public License 1.1. It contains
module names, LVO numbers, and public function names, never ROM addresses.
The generator excludes entries explicitly marked as AROS-only extensions in
private ABI slots, because those slot numbers are not portable to classic
Kickstart libraries.

At runtime, Copperline obtains addresses from Exec's active library/device
lists and their negative `JMP abs.l` vectors, and obtains other ROM module
names and bounds from the live resident-tag list. This is intentionally the
entire address model: no Kickstart checksum/address map or undocumented
private-entry byte signature is accepted. A documented private signature can
be added later as a guarded pattern, but public `RawDoFmt` and `RawPutChar`
already resolve through their Exec LVOs.
