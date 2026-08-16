# Toccata and the AD1848 model

Copperline's Toccata is a hardware model of the MacroSystem Zorro II sound
board, not an AHI-aware audio API: the guest enumerates the real
manufacturer/product identity, programs the AD1848 codec's indexed
registers, pushes/pulls bytes through the board's own 1024-byte FIFO, and
polls the board's status/control register exactly as the stock
`toccata.audio` AHI driver does. See [](../zorro) for the autoconfig window
and [](audio) for how the board's output joins the mixer and stem capture.

Modelled against WinUAE/amiberry's `sndboard.cpp` (byte-identical in both
trees) as a **behavioural oracle**: register offsets, bit semantics, the
FIFO's threshold and byte-order quirks, and the interrupt condition are
transcribed from what that reference does, not from the AD1848 datasheet
alone -- several of the board's most consequential behaviours (reg 12
pinned to plain-AD1848 mode, the FIFO's little-endian-vs-big-endian byte
order, underrun repeating rather than silencing) are MacroSystem-specific
or emulator-specific choices a datasheet alone would not predict.

## Implemented controller surface

`Ad1848` (`src/toccata/ad1848.rs`) owns all chip state: the 16 reachable
indexed registers, the 1024-byte play FIFO, the board's own status/control
register, the auto-calibration countdown, and DAC output volume. It has no
Zorro/bus coupling -- `Toccata` (`src/toccata.rs`) is the board wrapper:
autoconfig identity (`BoardSpec::toccata`), the 64 KB window's four-port
address decode, and the mixer-rate cadence.

- **Autoconfig**: manufacturer 18260 (MacroSystem), product 12, Zorro II,
  single 64 KB I/O window, no autoboot ROM (the real board's `romtype` is
  `ROMTYPE_NOT`).
- **Register window**: status/control, FIFO data, and the AD1848
  index/data ports are decoded by address-line pattern (`A14`/`A13`/`A11`/
  `A0`), matching the reference's own decode rather than exact-address
  matching -- each port mirrors across several KB of the window, and the
  AD1848 ports only respond on odd byte addresses. Anything the pattern
  doesn't match is open bus within the board's own window (reads 0, writes
  drop), distinct from the Zorro chain's open bus (0xFF) outside any
  configured board.
- **AD1848**: reg 12 is pinned to `0x0A` regardless of what's written,
  which locks the codec to plain-AD1848 mode -- no CS4231 extensions, and
  since format bits 5/7 of reg 8 are never decoded, no µ-law/A-law, only
  8-bit unsigned linear and 16-bit signed linear PCM. Reg 8's crystal-select
  and divider bits produce the codec's 14 legal rates (5512-48000 Hz,
  rounded to the nearest 100 Hz the way the reference rounds); reg 6/7 are
  DAC output attenuation, applied to every produced sample including
  underrun repeats.
- **FIFO**: 1024 bytes, half-empty threshold 512, edge-triggered on the
  downward crossing only. Overflow silently drops bytes (matching the
  IDT7202LA's own can't-overflow guarantee). Underrun repeats the last
  decoded sample rather than emitting silence -- real silicon holds its
  last DAC value, and the reference models that exactly.
- **16-bit byte order**: a native `move.w` decomposes into big-endian byte
  pokes at the FIFO port, but the FIFO reads back little-endian, so writing
  word `0x1234` is actually heard by the codec as `0x3412`. This is a real
  hardware quirk real Toccata drivers byte-swap around, not something
  Copperline "corrects" -- see the test named for it in
  `src/toccata/ad1848.rs`.

## Mixer cadence and resampling

The codec's own programmed rate is independent of Copperline's fixed
44.1 kHz mixer rate (established by the audio sink service, [](audio)).
`Toccata::tick` runs **two** independent exact-ratio accumulators --
each the same shape as `Paula::advance_audio`'s own -- rather than one:
`advance_codec` at the AD1848's own active rate, and `advance_mixer` at
the mixer's fixed rate. Splitting them is deliberate, not incidental: a
polyphase windowed-sinc resampler (`src/audio/resample.rs`, shared with
the MT-32 engine) is inherently non-causal, since it needs taps on both
sides of the output instant and primes by pulling a full window's worth
of input before its first output. If the resampler pulled straight from
the chip (calling `Ad1848::produce_one_sample` directly from inside its
own `refill` closure), its first use after startup or a rate change
would drain dozens of FIFO bytes and evaluate the half-empty/interrupt
condition all at once, decades ahead of when a real codec would reach
them -- the resampler's own lookahead would leak into hardware-state
timing correctness.

`advance_codec` is what actually drains the FIFO and evaluates the
half-empty/interrupt condition (the reference's own
`audio_state_sndboard_toccata` per-sample body, transplanted verbatim),
paced causally by `cck` at the codec's own rate; each produced sample is
queued into `decoded`, a plain FIFO of raw pre-resample frames.
`advance_mixer` pulls from that passive queue through the resampler --
never from the chip directly -- repeating the chip's last known sample
(via the side-effect-free `Ad1848::peek_last_sample`) if `decoded` is
momentarily empty. This keeps the resampler's non-causal lookahead
confined to shaping the interpolated *waveform*; it can never reorder
when a FIFO byte drains or an interrupt raises. Resamplers are cached per
codec rate (`Toccata`'s `resamplers` map, at most 14 entries, the
AD1848's legal rate count) so returning to an already-programmed rate
never rebuilds its kernel table.

Produced frames push into `ToccataAudioRing` (`src/chipset/paula.rs`,
alongside `CdAudioRing`), which `push_mixed_frame` pops one frame from per
mixer tick -- a plain per-frame pop, not a rate conversion, since the
board already resampled before pushing. Unlike CD-DA's bursty per-sector
delivery, the board's own tick cadence matches the mixer's, so both the
`decoded` queue and the ring stay near-empty in steady state; their fixed
capacities are a safety margin against a stalled consumer, not a
buffering requirement.

A cached resampler's `history` buffer holds the last ~64 input frames it
convolves over, so `Toccata::reset()` (a guest CPU reset, matching every
other in-tree board's `ZorroDevice::reset()` -- a full hardware reinit)
zeroes both accumulators, clears `decoded`, and clears the whole
resampler cache, not just `Ad1848`'s own registers/FIFO. Without that, a
few dozen milliseconds of pre-reset audio would bleed through the stale
kernel window into what should be post-reset silence -- covered by
`reset_clears_stale_resampler_history_so_silence_follows_immediately` in
`src/toccata.rs`.

## Interrupt

INT6/EXTER, level-sensitive (`Toccata::int6_line` reads `Ad1848::int6_pending`).
The condition requires the codec to have been started (reg 9's playback/
record enable bits), the board's own `STATUS_FIFO_CODEC` gate, the
relevant direction's FIFO-enable bit, that direction's INTENA bit, and the
edge-latched half-empty/half-full flag -- all evaluated once per produced
sample. Reading the status register acknowledges (clears) pending
interrupt bits; the half-empty/half-full latch itself is cleared by FIFO
port access instead, so a status-read ack with the latch still set
re-raises the interrupt on the next produced sample.

## Determinism

Every board-side computation -- register writes, FIFO drains, interrupt
evaluation, the resampler's phase -- is driven purely by `tick`'s `cck`
argument or by CPU register accesses, both already deterministic inputs.
Nothing reads wall-clock time, so a Toccata-fitted machine is warp-safe
and reproducible exactly like the rest of the emulated audio path (see
[](audio)'s determinism section) -- two runs of the same scripted
scenario produce byte-identical `toccata.wav` stem captures.

## Savestates

`Toccata` derives `Serialize`/`Deserialize` directly (`Box`ed as
`BoardDevice::Toccata`, like `Picasso2`) with no `#[serde(skip)]` fields:
`codec_acc`/`decoded` are genuine machine state for the reason given
above, and the `resamplers` cache -- despite only shaping the waveform,
never FIFO/IRQ timing -- is serialized too, via `Resampler`'s own manual
`Serialize`/`Deserialize` in `src/audio/resample.rs` (its derived
`kernels` table is rebuilt from `l`/`m` on load rather than stored, since
it is a pure function of the reduced rate ratio). This makes a
save-state load reproduce an uninterrupted run's *output* exactly, not
just its FIFO/IRQ timing -- a resumed run's `toccata.wav` stem is
byte-identical to what an uninterrupted run would have produced at the
same point, covered by
`savestate_round_trip_reproduces_an_uninterrupted_runs_output` in
`src/toccata.rs`.

## What's out of scope for M1-M3

- **Record** (the board's capture FIFO/interrupt path) is modelled only as
  inert stubs: the record port exists and acknowledges correctly, but
  never has data, since nothing ever sets `STATUS_FIFO_RECORD`.
- **The "Paula/CD audio mixer" board setting** (reg 2-5's AUX1/AUX2 input
  gain feeding the board's own analog mixer) is not modelled: Copperline's
  Paula and CD-DA already reach the master mix directly, so replicating
  the board's own internal mixing would add no user-visible capability.
- The launcher's **I/O Ports** tab (Sound section) has a plain fit/don't-fit
  toggle for the board; host-side audio capture/backend options
  (`--audio-wav`, `--audio-stems`, device selection) are not exposed there
  and stay command-line/config-file only.
