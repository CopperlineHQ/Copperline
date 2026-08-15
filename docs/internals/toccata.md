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
`Toccata::tick` runs its own exact-ratio accumulator -- identical in shape
to `Paula::advance_audio`'s own -- and on each mixer-rate frame boundary
pulls one sample through a polyphase windowed-sinc resampler
(`src/audio/resample.rs`, shared with the MT-32 engine) that converts the
AD1848's rate to the mixer's.

The resampler's pull API (`next(refill)`) does double duty: `refill()` is
called exactly once per actual codec-rate sample the resampler needs, so
it *is* the per-sample unit the reference's own `audio_state_sndboard_toccata`
callback performs -- draining the FIFO, applying volume, evaluating the
half-empty/interrupt condition. No separate codec-rate accumulator exists
in the board's own state; the resampler's phase counter is the cadence.
Resamplers are cached per codec rate (`Toccata`'s `resamplers` map, at most
14 entries, the AD1848's legal rate count) so returning to an
already-programmed rate never rebuilds its kernel table.

Produced frames push into `ToccataAudioRing` (`src/chipset/paula.rs`,
alongside `CdAudioRing`), which `push_mixed_frame` pops one frame from per
mixer tick -- a plain per-frame pop, not a rate conversion, since the
board already resampled before pushing. Unlike CD-DA's bursty per-sector
delivery, the board's own tick cadence matches the mixer's, so the ring
stays near-empty in steady state; its fixed capacity is a safety margin
against a stalled consumer, not a buffering requirement.

A cached resampler's `history` buffer holds the last ~64 input frames it
convolves over, so `Toccata::reset()` (a guest CPU reset, matching every
other in-tree board's `ZorroDevice::reset()` -- a full hardware reinit)
also zeroes the mixer accumulator and clears the whole resampler cache,
not just `Ad1848`'s own registers/FIFO. Without that, a few dozen
milliseconds of pre-reset audio would bleed through the stale kernel
window into what should be post-reset silence -- covered by
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

## What's out of scope for M1-M3

- **Record** (the board's capture FIFO/interrupt path) is modelled only as
  inert stubs: the record port exists and acknowledges correctly, but
  never has data, since nothing ever sets `STATUS_FIFO_RECORD`.
- **The "Paula/CD audio mixer" board setting** (reg 2-5's AUX1/AUX2 input
  gain feeding the board's own analog mixer) is not modelled: Copperline's
  Paula and CD-DA already reach the master mix directly, so replicating
  the board's own internal mixing would add no user-visible capability.
- The launcher's machine-configuration screen does not yet have a Toccata
  row; `[toccata] enabled = true` in a config file loaded directly (`--config`)
  works, but loading that config into the launcher and saving from there
  drops the setting until a GUI row is added.
