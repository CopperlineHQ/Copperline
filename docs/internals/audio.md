# Audio sink service (`src/audio/`)

`src/audio/mod.rs` defines the host-facing sinks (`AudioSink`, `NullSink`,
`CpalSink`, and `WavSink`). `src/audio/mux.rs` defines `AudioMux`, which
routes the master mix and individual sources to playback or WAV capture.
Mixing, LED filtering, volume, and stereo width are applied in
`Paula::push_mixed_frame` (`src/chipset/paula.rs`).

(why-a-mux-exists)=
## Mixing and capture

`Paula::audio` owns an `AudioMux`. The mixer calls `push_master` for the
final stereo output, `push_source` for individual devices, and
`push_source_channel` for Paula's four channels. The master sink selects
live playback (`CpalSink`), a mixed WAV (`WavSink`, `--audio-wav`), or no
output (`NullSink`, `--noaudio`). Optional stem writers capture the source
taps alongside it.

## The taps

`push_mixed_frame` pushes seven named sources, each at the point in the
signal chain described below (not necessarily the point that ends up in
the master mix -- see each entry):

| Source | Tap point | Notes |
|---|---|---|
| `paula` | Post-LED-filter, pre-drive/CD/MT-32/Coppersynth/Toccata/MHI | The pure Paula-channel sum |
| `paula` sub-channels `0`..`3` | `channel_mixed_sample(i)`, scaled | **Not** LED-filtered -- real hardware's filter sits after the channel mixer's summation, so a per-channel stem naturally excludes it |
| `drivesounds` | The synthesized drive-noise sample | Mono; written to a stem as `(sample, sample)` |
| `cdda` | Post `cd_muted` gate | Reflects audible content -- unlike the debugger's CD scope tap, which stays pre-mute for visibility |
| `mt32` | The in-process MT-32 synth frame | Silence (`0.0, 0.0`) once the serial sink has latched `synth_silent` |
| `coppersynth` | The in-process Coppersynth frame | Same tap and `synth_silent` latch as `mt32` -- the serial sink carries one synth at a time, and the stem is named for whichever it is |
| `toccata` | One frame popped from `ToccataAudioRing` | Already resampled to the mixer rate by the board's own tick; see [](toccata.md) |
| `mhi` | One frame popped from `MhiAudioRing` | Already resampled to the mixer rate by the board's own tick; see [](mhi.md) |

The **master** signal (`push_master`) is the final `out_left`/`out_right`
after master volume and stereo width. Live playback and `--audio-wav`
receive these same samples.

## Stem capture (`--audio-stems`)

`AudioMux::enable_stems(dir, granularities, sources)` opens
[`hound`](https://docs.rs/hound) WAV writers (the same stereo f32 @
44.1 kHz framing as `WavSink`, factored into `audio::open_wav_writer`)
for whichever files the selected `StemGranularity` values and registered
`SourceSpec`s imply:

- `Master` -- `DIR/master.wav`.
- `Source` -- `DIR/{id}.wav` for each registered source.
- `Channel` -- `DIR/{id}-{channel}.wav` for each named sub-channel of each
  registered source. Select `Channel` explicitly; `Source` does not include it.

`main.rs::configured_audio_stem_sources` selects sources once at startup:

- `paula` and `drivesounds` always register. Disabled drive sounds produce
  a silent stem.
- `cdda` registers for CD32/CDTV, a configured `[cd] image`, or a CD image
  attached through IDE, LIDE, or SCSI.
- `mt32` registers when selected as MIDI output and both ROMs are configured
  or remembered from menu selections.
- `coppersynth` registers when selected and the build includes the feature.
- `toccata` and `mhi` register when their boards are configured.

With `--load-state`, all seven sources register because restored hardware
can differ from the startup configuration. Unused sources produce silent
files. Registration does not change during capture: adding a source later
will not create a missing stem writer.

## Determinism

`Paula::advance_audio` schedules master and stem samples in emulated time.
Warp changes host pacing without changing that schedule.
`tests/audio_stems_determinism.rs` compares the output files from repeated
runs. Reproducibility still depends on repeatable source input; see the
[host boundary](architecture.md#determinism-and-the-host-boundary) and
[MHI's floating-point limits](mhi.md#copperline-implementation-notes).

## Savestates

`Paula::audio: AudioMux` is skipped by serde because it is host output.
`Bus::adopt_host_resources` moves the live mux, including open stem writers,
onto the restored Bus. A capture therefore continues in the same files
across a save-state load.

## Web build

`AudioMux` and the stem-writer code compile without the `frontend` feature.
The browser wrapper uses `WebAudioSink` as the master sink and does not
call `enable_stems`; `--audio-stems` is available through the native CLI.
