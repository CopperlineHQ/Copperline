# MHI virtual decoder board — implementation plan (M1–M2)

Working branch: `add-mhi-board` (fork `origin`, never `upstream`).
Source proposal: Copperline MHI Plugin proposal (Aug 2026). Decisions taken with the
maintainer on 2026-08-16:

- **In-tree Rust board** (like `src/toccata.rs`), NOT a WASM plugin. The published
  register spec still gets written bus-agnostically so other emulators can implement it.
- **Scope: M1–M2 only.** M1 = board autoconfigs, library opens, MHIQuery/alloc/free
  round-trip, doorbell+interrupt proven with a test harness. M2 = CBR MP3 plays in
  AmigaAMP end-to-end through the audio sink and appears deterministically in capture.
  VBR/seek/params/golden-CI (M3/M4) are follow-up branches.
- **Amiga library: port** the GPL-3 MHI front-end from BlitterStudio host-tools'
  `mhiuae.library` (github.com/BlitterStudio/host-tools — Copperline is GPL-3, licences
  align), replacing its `uae.resource` trap layer with our board-register access layer.
- **Assets fetched from the net** (Aminet MHI dev kit, AmigaAMP; MP3s generated locally
  with ffmpeg/lame). Local assets live under `test-assets/`, never committed. Fetched
  GPL source being *ported* (mhiuae front-end) does get committed under `guest/mhi/`
  with provenance noted in file headers.

## Architecture recap

- **Board** (`src/mhi.rs`): Zorro II slave, 64 KiB window, Copperline manufacturer ID
  5192/`0x1448`, next free product number (7 — check `docs/zorro.md` table). Mailbox
  register file of our own design: descriptor enqueue (Amiga address + length + doorbell),
  completion/reclaim register + INT2, transport command (play/pause/stop), status
  (playing/paused/out-of-data), param registers (volume/pan/tone — M4 applies them; M1–M2
  just latch them), read-only capability/version registers that answer `MHIQuery`.
  On doorbell the board slave-copies the bitstream out of emulated RAM via the
  `DeviceHost` DMA interface (it is *not* a bus master on the guest-visible level; the
  copy is an implementation detail like the A2091's).
- **Decoder**: minimp3 (CC0). Prefer the `minimp3` Rust crate family or vendored
  single-header C via a small `build.rs`/`cc` step — pick whichever keeps builds clean on
  macOS/Linux/Windows with no system deps; document the choice. MPEG-1/2/2.5 Layer III,
  CBR is the M2 target (don't break on VBR input, just no seek support).
- **Timing**: the board consumes bitstream at the decoded audio's emulated-time rate —
  1152-sample frames "play out" over the corresponding emulated cycles in `tick()`, and
  only then is the buffer marked consumed + INT2 raised. Client throttling stays
  faithful; warp mode stays deterministic.
- **Audio out**: decoded PCM goes to `AudioMux::push_source("mhi", l, r)` per emulated
  sample-out instant, resampled to the mix rate the same way Toccata does (reuse
  `src/audio/resample.rs`; note the causal-resampler + savestate-serialization pattern
  from the Toccata work — resampler state must serialize).
- **Savestates**: append a `BoardDevice` variant AT THE END of the enum, extend every
  forwarding match arm, bump `savestate::STATE_VERSION` with a comment. Decoder state
  across savestates: serialize the un-consumed compressed queue plus the decoder's
  cross-frame state (mp3dec bit-reservoir); if the raw decoder struct is not cleanly
  serializable, re-feed queued bitstream from the last frame-sync on restore — but the
  restored run must stay byte-identical to an uninterrupted one, which the integration
  test must prove.
- **Guest library** (`guest/mhi/`): `mhi_copperline.library`, ported mhiuae front-end +
  a small board-access layer (find board via `FindConfigDev(5192, <product>)`, register
  read/write, INT2 server converting completions into the client's signal). Built with
  the shared dockerized toolchain (`guest/toolchain.mk`), committed built artifact
  following the `guest/services`/`guest/hostsocket` precedent.
- **Config**: `[mhi] enabled = true` (+ `library` staging notes), CLI flag if the
  existing pattern has one (Toccata: check how `[toccata]` maps to flags), launcher
  I/O-tab toggle following Toccata's example, `copperline.example.toml` entry.

## Work packages

Ordered; WP1/WP2 are parallel-safe, WP3 depends on WP1, WP4 on WP2+WP3, WP5 on WP4,
WP6 runs alongside everything it documents.

- **WP1 — Assets & references** (no repo changes outside `test-assets/`):
  clone host-tools (mhiuae source incl. Amiga side); fetch the MHI developer kit
  (Aminet — search for `mhi` dev kit, includes + autodocs) and AmigaAMP (Aminet
  `mus/play`); generate CBR test MP3s (e.g. 44.1 kHz 128/320 kbps from a deterministic
  ffmpeg-synthesized tone/sweep). Record what landed where in `test-assets/README`-style
  notes if one exists (check conventions).
- **WP2 — Register spec** (`docs/internals/mhi.md`): the versioned, bus-agnostic
  mailbox spec (offsets within the autoconfigured window). Cover: descriptor queue
  depth and semantics, doorbell, completion ring/reclaim, transport commands, status
  transitions (incl. out-of-data), param latches, capability/version registers, interrupt
  ack protocol. This document is the contract WP3 and WP4 both implement against.
- **WP3 — Host board** (`src/mhi.rs` + wiring): `ZorroDevice` impl per WP2 spec;
  minimp3 integration; emulated-time pacing; `push_source("mhi", ...)` with resampling;
  `BoardDevice` variant + all match arms; `build_machine` wiring in `src/emulator.rs`;
  `[mhi]` config in `src/config.rs`; STATE_VERSION bump; unit tests next to the existing
  Zorro/Toccata ones (autoconfig identity, register round-trips, doorbell→completion→INT2
  sequencing with a stub decoder, pacing math, savestate round-trip).
- **WP4 — Guest library** (`guest/mhi/`): port mhiuae front-end onto the board-access
  layer; Makefile via `toolchain.mk`; builds reproducibly in docker; committed artifact.
  Keep board access isolated in one small file (the published-spec portability story).
- **WP5 — Integration & M2 verification** (`tests/mhi.rs` + headless runs):
  M1 harness test — boot a machine with the board, stage the library + a small test
  client (via `--run` or a staged volume), prove open/query/alloc/free and
  doorbell→interrupt round-trip. M2 — AmigaAMP plays a known CBR MP3 headlessly;
  `--audio-wav`/stems capture contains the decoded audio; same-input runs are
  byte-identical; savestate mid-playback resumes byte-identically. Integration tests
  needing fetched assets are `#[ignore]` per repo convention (`cargo test --release --
  --ignored`).
- **WP6 — Docs**: `docs/zorro.md` board section + product-ID table row,
  `docs/internals/mhi.md` (WP2 owns), `docs/guide/configuration.md` `[mhi]` section,
  `copperline.example.toml`. Same-change documentation rule from AGENTS.md applies.

## Constraints & conventions (from AGENTS.md — binding)

- Release builds for any emulation run; headless verification with scheduled
  screenshots/captures, never manual window sessions.
- `cargo test`, `cargo clippy`, `cargo fmt --check` all clean before declaring done.
- Hardware-first: the board models hardware behaviour; never branch on program identity.
- ROMs/disk images/fetched binaries stay local; never committed.
- Commit incrementally with clear messages on `add-mhi-board`; push only to `origin`.
