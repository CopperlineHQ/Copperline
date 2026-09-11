# Audio sink service (`src/audio/`)

`src/audio/mod.rs` defines the host-facing sinks (`AudioSink`, `NullSink`,
`CpalSink`, and `WavSink`). `src/audio/mux.rs` defines `AudioMux`, which
routes the master mix and individual sources to playback or WAV capture.
Mixing, LED filtering, volume, and stereo width are applied in
`Paula::push_mixed_frame` (`src/chipset/paula.rs`).

## Getting Paula onto the host grid

Paula's four channels are a staircase: each holds its 8-bit sample for
`AUDxPER` colour clocks, so a channel steps at up to ~28.6 kHz and the
summed output only ever changes on the 3.546895 MHz colour-clock grid.
Landing that on the mixer's 44.1 kHz grid is a decimation by ~80.4, and
doing it by reading the staircase at each output instant would fold
everything above the mixer's Nyquist straight back into the audible band.
The staircase is rich up there: a full-scale 10 kHz square (two samples at
`AUDxPER` 177, what Amiga Test Kit's audio page plays) carries harmonics at
30, 50 and 70 kHz, which point sampling turns into tones at 14.0, 6.0 and
18.1 kHz -- inharmonic, so heard as ringing rather than brightness, and
loud, the worst of them only 10 dB under the tone itself.

So the mix runs in two stages, both in `Paula::advance_audio`:

1. **Exact integration to an oversample grid.** `PAULA_OVERSAMPLE` (4) times
   the mixer rate, so 176.4 kHz. `advance_audio` splits its span so that no
   oversample instant and no channel period expiry falls strictly inside a
   step (`cck_until_mix_input_changes`), which means each channel's
   contribution is constant across the step and `channel_area` can
   accumulate `sample * volume * clocks` exactly. Dividing by the interval
   width at each oversample instant is then a true box average of the
   staircase, not a sample of whichever step the instant landed on.
2. **Windowed-sinc decimation to the mixer grid.** One
   `audio::resample::Decimator` per channel, 4:1, sharing the 64-tap
   Blackman-windowed kernel `Resampler` uses. Per channel rather than per
   side because decimation is linear -- filtering each channel and summing
   is the same signal as filtering the sum -- which keeps the per-channel
   stem taps exactly consistent with the mix they add up to.

Neither stage touches the emulated timeline: `advance_audio` runs the same
channel state machine over the same colour clocks either way, and the extra
splitting only changes how often it is re-entered. The box stage needs the
oversample rate well above Paula's own; the sinc stage needs it low enough
that the kernel's transition band stays clear of the top of the audio band.
Four satisfies both, and puts the worst surviving image about 47 dB under
the tone. Raising the oversample rate or lengthening the kernel does not
measurably improve on it.

The genuine zero-order-hold images of Paula's own sample rate are *not*
filtered out, because they are real output: the 500 Hz sine test at
`AUDxPER` 177 keeps its images at 19.5 and 20.5 kHz, exactly as the
hardware produces them ahead of its analogue filter.

### Level

One channel's DAC level is `sample * volume`, -8192..8128. Two channels
reach each side, so full scale is both of them saturated at full volume --
and then the band-limited waveform overshoots the steps it came from, since
taking the harmonics away leaves something taller than the square that
carried them. The most any Paula channel can overshoot is set by its
shortest legal period: `AUDxPER` 124 holds a sample for a little over six
oversample intervals, and the most a run that long can draw out of the
kernel is 1.3241x the staircase. `PAULA_RECONSTRUCTION_HEADROOM` rounds
that to 1.33, and `PAULA_MIX_SCALE` divides it out, so nothing Paula can
play leaves [-1.0, 1.0] and nothing downstream has to clip it. A real
Amiga's analogue reconstruction filter overshoots its own DAC the same way;
this is just where a line input would have to be set.

Everything line-mixed alongside Paula (drive sounds, CD-DA, an in-process
synth, Toccata, MHI) is added after this scaling and keeps its level
relative to a Paula channel. Their sum with Paula is not separately limited,
so a CD32 playing a full-scale CD track under a full-scale module can still
ask for more than full scale.

### The LED filter sits after all of this

`StereoLedFilter` runs at the mixer rate, on the decimated sum, which is
where hardware puts it -- after the channel mixer's summation. That only
works because the decimation above is band-limited. Filtering a
point-sampled mix instead would attenuate the real tone while leaving
untouched the aliases that had already folded below the cutoff: with the
filter engaged, the 10 kHz test used to come out as a 2 kHz buzz nearly 6 dB
*louder* than the tone it was supposed to be attenuating.

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

The decimation state does ride the state: `channel_area`/`area_cck` carry
the partly integrated oversample interval, and each `Decimator` carries its
tap history (but not its kernel, which `Deserialize` rebuilds from the
factor, as `Resampler` does). `host_sample_acc` keeps the meaning and range
it always had -- colour clocks times the mixer rate, rolling at
`PAULA_CLOCK_HZ` -- with the oversample grid read off it as a finer
threshold, so the `PAUL` chunk needed no version bump. All three fields are
`#[serde(default)]`: a state written before the decimation existed resumes
with silent tap histories, which is a millisecond of filter warm-up and no
click.

## Web build

`AudioMux` and the stem-writer code compile without the `frontend` feature.
The browser wrapper uses `WebAudioSink` as the master sink and does not
call `enable_stems`; `--audio-stems` is available through the native CLI.
