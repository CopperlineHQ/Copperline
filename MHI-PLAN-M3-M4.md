# MHI virtual decoder board — implementation plan (M3–M4)

**M3 status (2026-08-17): WP3.1–WP3.5 all done on branch `mhi-m3-seek`.**
WP3.2 turned out to already be implemented as of M1–M2 (`cmd_stop` has
reset the decoder since the very first commit) — M3's actual work there
was proving it against real encoded content and documenting it, not
writing new logic. Delivered: 5 new unit tests in `src/mhi.rs`
(seek-entry resync, ID3v2-tag skip, VBR settle, STOP-state-reset,
golden-PCM regression), two committed fixtures under `tests/data/mhi/`
(`golden_tone_cbr64_mono.{mp3,pcm}`, `vbr_sweep.mp3`,
`golden_tone2_880hz_cbr64_mono.mp3`), a new guest probe
(`guest/mhi/test/mhiseek.c` + committed binary) proving a real
`MHIStop`→reposition→`MHIQueueBuffer` sequence end to end through the
emulator (`tests/mhi.rs`'s `mhi_m3_seek_switches_from_tone_a_to_tone_b_
across_a_stop`), and the `docs/internals/mhi.md` updates below (STOP row,
CAPS bit 5 rewording, new "Seek-entry hardening" section). `cargo test`
(lib + all `--ignored` mhi tests)/`clippy`/`fmt` all clean. M4 not started.

Follow-up to `MHI-PLAN.md` (M1–M2, merged as PR #472 on 2026-08-16). Each
milestone is its own branch off `main` per the maintainer's M1–M2 decision
that follow-ups stay separate: `mhi-m3-seek` and `mhi-m4-params`. Golden-CI
rides with M3 (it is test infrastructure both milestones need; landing it
first means M4 inherits regression coverage for free).

## Scope finding that reshapes M3

The MHI API has **no seek function**. The library jump table
(`guest/mhi/startup.c`) is exactly `MHIAllocDecoder`/`MHIFreeDecoder`/
`MHIQueueBuffer`/`MHIGetEmpty`/`MHIGetStatus`/`MHIPlay`/`MHIStop`/
`MHIPause`/`MHIQuery`/`MHISetParam`. Seeking is the *player's* job: AmigaAMP
seeks by `MHIStop` (which flushes the queue — the board already implements
this), repositioning its own file reads, and re-queueing buffers that now
begin at an arbitrary byte offset. So "M3 seek support" is not new registers
in the reserved `0x20`+ space — it is proving and hardening the board's
**mid-stream entry** path:

- Bitstream that starts at an arbitrary offset (mid-frame, inside an ID3v2
  tag, inside a Xing/VBRI header) must resync to the next frame sync
  without garbage output, for CBR and VBR alike. The bounded-resync cap
  from c864ba7 is the mechanism; M3 stress-tests it at seek entry points
  rather than only at stream start.
- After a STOP-flush-requeue cycle the decoder's cross-frame state
  (bit reservoir, filterbank memory) must not bleed the pre-seek stream
  into the post-seek output. Decide and document: does `CONTROL=STOP`
  reset decoder cross-frame state (proposed: yes — matches a hardware
  decoder losing its pipeline on stop, and is what a seeking player
  needs), or only the queue? M1–M2's spec text covers the queue flush but
  is silent on decoder state; writing it down is an M3 spec deliverable
  either way.
- VBR streams must produce correct audio from any frame-aligned entry
  point (the bit reservoir means the first 1–2 frames after a blind seek
  legitimately decode with missing reservoir bits — matching real
  decoders; the test asserts clean steady-state after a bounded settle,
  not bit-perfection at the seam).

`CAPS` bit 5 currently reads "VBR accepted as input (decodes correctly; no
seek support)". Open decision for the maintainer: after M3, either re-word
bit 5's documented meaning (doc change only — the bit's value doesn't move,
arguably not a protocol change) or assign a new bit 6 "seek-entry hardened"
and leave 5 as-is. Proposed: re-word bit 5; a capability bit that only ever
described a quality level, not a register behavior, can have its prose
tightened without a `VERSION` bump. If the maintainer reads the versioning
rules more strictly, bump `VERSION` to 2 in M3 and fold M4's bump (below)
into the same increment ordering.

## M3 — seek/VBR hardening + golden CI (`mhi-m3-seek`)

- **WP3.1 — Seek-entry unit tests** (`src/mhi.rs` tests): feed the board
  descriptor sequences that start (a) mid-frame, (b) inside an ID3v2 tag,
  (c) at a VBR frame boundary with a hot bit reservoir, (d) pure junk;
  assert bounded resync, no panic, correct `OUT_OF_DATA`/completion
  behavior, and (for c) clean steady-state output after the settle window.
  Stub-decoder tests for the state machine, real-minimp3 tests for the
  audio-content assertions.
- **WP3.2 — STOP decoder-state semantics**: implement whichever STOP
  decision the maintainer takes (proposed: STOP resets cross-frame decoder
  state; PAUSE keeps it — the spec already documents PAUSE's preservation
  explicitly, so the asymmetry is natural). Spec text in
  `docs/internals/mhi.md` updated in the same change.
- **WP3.3 — End-to-end seek scenario** (`tests/mhi.rs`, `#[ignore]`d like
  its siblings): scripted MHIplay (or a purpose-built test client in
  `guest/mhi/test/`) that plays, stops, re-queues from a later file
  offset, and plays again; `--audio-wav` capture shows the tone change at
  the expected emulated time, byte-identical across runs (RTC pinned).
  A VBR variant of the fixture MP3 joins `test-assets/mhi/` (NOTES.md
  recipe extended: same deterministic ffmpeg/lame synthesis, `-q` VBR
  mode).
- **WP3.4 — Golden CI**: today every integration test is gated on fetched
  local assets. Give MHI CI coverage that needs no fetch:
  - Commit a tiny self-generated CBR MP3 (~2–3 s, mono, low bitrate,
    a few KiB) under `tests/data/mhi/`. It is our own synthesized tone
    encoded locally — not a ROM, not a disk image, not a fetched binary —
    so the "local assets are never committed" rule does not bar it; the
    maintainer confirms this reading before anything lands (open
    decision). Fallback if declined: generate the MP3 in CI via a
    vendored encoder step, or scope golden-CI down to stub-decoder
    state-machine tests only.
  - A non-`#[ignore]` test decodes it through the full board path (real
    minimp3, no emulator boot — board-level, like the existing unit
    tests) and compares against a committed golden PCM hash. Catches
    minimp3 vendoring drift, resampler regressions, and pacing changes on
    every `cargo test`.
- **WP3.5 — Docs**: `docs/internals/mhi.md` seek-entry section + STOP
  semantics + CAPS bit 5 wording; `test-assets/mhi/NOTES.md` VBR recipe.
  Same-change rule as always.

## M4 — params become audible (`mhi-m4-params`)

The seven latches (volume, panning, bass, mid, treble, crossmixing,
prefactor — board indices 0–6) already exist, round-trip, clamp, and
serialize; M4 makes them act on decoded PCM. The register shape is frozen
by design ("M4 changes what a latch *does*, never where it lives").

- **WP4.1 — DSP design note first** (in `docs/internals/mhi.md`): exact,
  deterministic definitions before code —
  - Volume/prefactor: post-decode linear gains (map 0–100 to fixed
    curves; document the curve — MHI's own header says volume 100 = 0 dB,
    prefactor 50 = unity, so prefactor needs headroom above unity and a
    documented clip behavior).
  - Panning: constant-power or linear stereo placement (pick one,
    document it; proposed linear — it is what a cheap hardware decoder
    would do and is exactly reproducible).
  - Crossmixing: 0=stereo … 100=mono blend, plain per-sample lerp of L/R
    toward their mean.
  - Bass/mid/treble: a fixed three-band shelving/peaking filter bank at
    documented corner frequencies, biquads with integer-derived
    coefficients computed from the latch value — coefficients specified
    in the doc so another emulator can match them. All processing at the
    decoder's native output rate, *before* the resampler, so the filter
    state lives with the causal producer and the Toccata-pattern
    resampler split is untouched.
  - Ordering: decode → prefactor → tone filters → volume → pan →
    crossmix → FIFO → resampler (documented; order is audible, so it is
    part of the contract).
- **WP4.2 — Implementation** (`src/mhi.rs`): apply the chain in the
  causal producer; latch changes take effect at the next produced sample
  (document that — no ramping/zipper suppression in v1, matching the
  cheap-hardware model; note it as a possible later refinement). Filter
  state (biquad memories) joins the savestate — mid-playback param state
  must round-trip like everything else; extend
  `savestate_round_trip_reproduces_an_uninterrupted_runs_output` to run
  with non-default params and hot filter state.
- **WP4.3 — Protocol/version surface**: making documented-inert latches
  audible changes documented semantics → bump `VERSION` to 2 per the
  spec's own strict rule, and add `CAPS` bit (proposed bit 6) "param
  latches are applied to output". The guest library keys its `MHIQuery`
  answers for the seven tone/volume/output flags on that CAPS bit rather
  than hardcoding — so the same library binary answers correctly on an
  old (inert-latch) board and a new one, and the M1–M2 "no rebuild
  needed" promise survives in spirit: a rebuild happens, but the rebuilt
  library also still works on old boards. 5/10-band EQ flags stay
  `MHIF_UNSUPPORTED` (indices 7+ remain reserved).
- **WP4.4 — Guest library** (`guest/mhi/`): `MHIQuery` reads CAPS bit 6;
  rebuild via the dockerized toolchain; recommit the built artifact per
  the established precedent.
- **WP4.5 — Verification**: unit tests assert exact DSP math (a known
  input block through each param at several latch values against
  precomputed expected output — the doc's coefficient tables are the
  oracle); an end-to-end `#[ignore]` test scripts `MHISetParam` calls
  mid-playback (volume drop, hard pan) and asserts the capture's channel
  energies change at the expected emulated instants, byte-identical
  across runs; the golden-CI test from WP3.4 gains a with-params variant.
- **WP4.6 — Docs**: `docs/internals/mhi.md` param-latch section rewritten
  from "latched, inert" to the full DSP contract; `VERSION`/`CAPS`
  tables; `MHIQuery` answer table flips; changelog-style note in the
  versioning section showing v1→v2 as the worked example it currently
  lacks.

## Explicitly out of scope (both milestones)

- 5/10-band EQ (`MHIP_BAND*`, `MHIP_MIDBASS`/`MHIP_MIDHIGH`) — reserved
  indices 7+, future `VERSION`.
- The M1–M2 leftovers tracked separately: AmigaAMP-proper end-to-end
  verification (staged-Workbench work, own branch), the mid-decode
  savestate-resume residual (pipeline-level, documented in
  `tests/mhi.rs`), the Toccata/MHI audio-ring + `advance_mixer`
  duplication cleanup, and launcher rows for the wider audio surface.
- MPEG-4/AAC (`MHIQ_MPEG4`), Layers I/II — CAPS answers unchanged.

## Open decisions for the maintainer (blocking order)

1. **Committed golden MP3** (WP3.4): may a few-KiB self-synthesized MP3
   live in the repo? (Recommended: yes.)
2. **STOP decoder-state semantics** (WP3.2): STOP resets cross-frame
   decoder state? (Recommended: yes.)
3. **CAPS bit 5 wording vs. new bit** after M3 (recommended: re-word,
   no `VERSION` bump in M3; the M4 bump to 2 carries the CAPS bit 6
   addition).
4. **Panning law** (WP4.1): linear vs. constant-power (recommended:
   linear).

## Constraints & conventions (unchanged from AGENTS.md / MHI-PLAN.md)

- Release builds for emulation runs; headless verification only.
- `cargo test`, `cargo clippy`, `cargo fmt --check` clean before done.
- Hardware-first: never branch on program identity.
- Fetched ROMs/binaries stay local; ported/committed source carries
  provenance headers.
- Every spec-visible change updates `docs/internals/mhi.md` in the same
  change; RTC pinned (`--rtc-time`) in every determinism test.
