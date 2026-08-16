# Audio sink service (`src/audio/`)

`src/audio/` is a directory module: `mod.rs` holds the host-facing sink
implementations (`AudioSink`, `NullSink`, `CpalSink`, `WavSink`), and
`mux.rs` holds `AudioMux`, the fan-out point every emulated audio producer
feeds. This page describes the mux/stem-capture layer; the underlying
mixing arithmetic (channel routing, the LED filter, master volume, stereo
width) is unchanged and still lives in `Paula::push_mixed_frame`
(`src/chipset/paula.rs`; see [](chipset) for the Paula audio model).

## Why a mux exists

Every audio-producing device -- Paula's four channels, floppy drive
noises, CD-DA, the in-process MT-32 synth -- has always been summed inline
inside `push_mixed_frame` and handed to a single `AudioSink`: live
playback (`CpalSink`), a mixed-master WAV capture (`WavSink`,
`--audio-wav`), or nothing (`NullSink`, `--noaudio`). `AudioMux` sits
between that summation and the sink, without changing it: `Paula::audio`
is now an `AudioMux` instead of a bare `Box<dyn AudioSink>`, and
`push_mixed_frame`'s tail calls `push_master`/`push_source`/
`push_source_channel` instead of pushing straight into the sink. This is
what lets `--audio-stems` capture the same signal at finer granularity
with no change to the mixing math, and is the seam boards register
through as just another named source: the MacroSystem Toccata
(`src/toccata.rs`, [](toccata)) was the first, feeding a `"toccata"` tap
the same way CD-DA and MT-32 do; the MHI virtual MPEG decoder board
(`src/mhi.rs`, [](mhi)) feeds a `"mhi"` tap the same way.

## The taps

`push_mixed_frame` pushes six named sources, each at the point in the
signal chain described below (not necessarily the point that ends up in
the master mix -- see each entry):

| Source | Tap point | Notes |
|---|---|---|
| `paula` | Post-LED-filter, pre-drive/CD/MT-32/Toccata/MHI | The pure Paula-channel sum |
| `paula` sub-channels `0`..`3` | `channel_mixed_sample(i)`, scaled | **Not** LED-filtered -- real hardware's filter sits after the channel mixer's summation, so a per-channel stem naturally excludes it |
| `drivesounds` | The synthesized drive-noise sample | Mono; written to a stem as `(sample, sample)` |
| `cdda` | Post `cd_muted` gate | Reflects audible content -- unlike the debugger's CD scope tap, which stays pre-mute for visibility |
| `mt32` | The in-process MT-32 synth frame | Silence (`0.0, 0.0`) once the serial sink has latched `synth_silent` |
| `toccata` | One frame popped from `ToccataAudioRing` | Already resampled to the mixer rate by the board's own tick; see [](toccata) |
| `mhi` | One frame popped from `MhiAudioRing` | Already resampled to the mixer rate by the board's own tick; see [](mhi) |

The **master** signal (`push_master`) is the final `out_left`/`out_right`
after master volume and stereo width -- unchanged from what every sink has
always received, live or `--audio-wav`. This is a structural guarantee,
not a tested-and-hoped-for one: nothing about the mixing arithmetic
changed when the mux was introduced, only what happens to the two f32
values once they're computed, so a `--audio-wav` capture from before and
after the mux is byte-identical.

## Stem capture (`--audio-stems`)

`AudioMux::enable_stems(dir, granularities, sources)` opens
[`hound`](https://docs.rs/hound) WAV writers (the same stereo f32 @
44.1 kHz framing as `WavSink`, factored into `audio::open_wav_writer`)
for whichever files the selected `StemGranularity` values and registered
`SourceSpec`s imply:

- `Master` -- `DIR/master.wav`.
- `Source` -- `DIR/{id}.wav` for each registered source.
- `Channel` -- `DIR/{id}-{channel}.wav` for each named sub-channel of each
  registered source. Deliberately never implied by `Source` alone.

**Source registration is a one-time, config-driven decision**, made in
`main.rs::configured_audio_stem_sources` right after the machine is built,
not inferred from whether a source stays silent during the run. `paula`
and `drivesounds` always register (`drivesounds` simply produces a silent
stem when `[audio] floppy_sounds = false`, the same as a disabled
`DriveSounds` today); `cdda` and `mt32` register only when this run's
config plausibly produces them:

- `cdda`: a CD32/CDTV machine profile, a configured CD image (`[cd]
  image` / `cd_image_path`), or a CD image on any `[ide]`/`[lide]`/
  `[scsi]` drive slot (the same `is_cd_image_path` test
  `open_ide_target`/`open_scsi_target` use to attach one as an
  ATAPI/SCSI CD-ROM).
- `mt32`: `[serial] midi_out = "mt32"` with both `mt32_control_rom` and
  `mt32_pcm_rom` set.

This is a **heuristic, not an exhaustive detector** -- a CD swapped into
an initially empty drive mid-run (`--insert-cd-after`, a control-protocol
`media` command) is missed, and `cdda.wav` simply won't be written for
that run even though the hardware ends up producing CD audio. Replacing
the config test with a query against the actually-built machine is a
reasonable follow-up if it turns out to matter in practice; it was kept
config-side for this milestone since getting it exactly right needs
reaching into feature-gated MIDI/MT-32 construction code that doesn't
otherwise need to be duplicated in `main.rs`.

A source's stem writer, once opened, is fed unconditionally by
`push_source`/`push_source_channel` -- there is no per-frame silence
detection or file cleanup. The one-time registration above is the entire
mechanism; `AudioMux` itself doesn't know or care why a source was or
wasn't registered.

## Determinism

Every push into `AudioMux` -- master or stem -- originates from
`Paula::advance_audio`'s emulated-color-clock accumulator, exactly like
the pre-mux mixing path. Nothing about stem capture depends on wall-clock
time, so it is warp-safe (a warped run renders the same sample stream
faster, not differently) and reproducible: two runs of the same scenario
produce byte-identical stem directories, which is what
`tests/audio_stems_determinism.rs` checks by reading every file from two
runs' output directories and comparing the bytes directly.

## Savestates

`Paula::audio: AudioMux` is `#[serde(skip)]`, same as the `Box<dyn
AudioSink>` it replaced -- host output (live or file-backed) is not part
of the emulated machine. `Bus::adopt_host_resources` moves the whole
`AudioMux` (master sink *and* any open stem writers) across a save-state
load with `std::mem::swap`, so a capture in progress keeps writing into
the same open file handles across a `--load-state`, exactly as a
`--audio-wav` capture already did before this change.

## Web build

`AudioMux` and the stem-writer code are platform-agnostic and compile
unconditionally, same as `WavSink` (`hound`/`std::fs` were never gated
behind the `frontend` feature). `--audio-stems` is a native-only CLI
flag; the web frontend (`crates/copperline-web`) simply never calls
`enable_stems`, so `WebAudioSink` runs as the mux's master sink with no
stem writers, unaffected.
