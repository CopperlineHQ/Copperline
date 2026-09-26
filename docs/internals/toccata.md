# Toccata and the AD1848 model

Copperline's Toccata is a hardware model of the MacroSystem Zorro II sound
board, not an AHI-aware audio API: the guest enumerates the real
manufacturer/product identity, programs the AD1848 codec's indexed
registers, pushes/pulls bytes through the board's own 1024-byte FIFO, and
polls the board's status/control register exactly as the stock
`toccata.audio` AHI driver does. See [](../zorro) for the autoconfig window
and [](audio.md) for how the board's output joins the mixer and stem capture.

The model uses WinUAE/amiberry's `sndboard.cpp` (byte-identical in both
trees) as a **behavioural oracle**: register offsets, bit semantics, the
FIFO's threshold and byte-order quirks, and the interrupt condition follow
what that reference does, not the AD1848 datasheet alone. Several of the
board's most consequential behaviours (reg 12 pinned to plain-AD1848 mode,
the FIFO's little-endian-vs-big-endian byte order, underrun repeating
rather than silencing) are MacroSystem-specific or emulator-specific
choices that the datasheet would not predict.

## Implemented controller surface

`Ad1848` (`src/toccata/ad1848.rs`) owns all chip state: the 16 reachable
indexed registers, the 1024-byte play FIFO, the board's own status/control
register, the auto-calibration countdown, and DAC output volume. It has no
Zorro/bus coupling. `Toccata` (`src/toccata.rs`) is the board wrapper:
autoconfig identity (`BoardSpec::toccata` in `src/zorro.rs`), the 64 KB
window's four-port address decode, and the mixer-rate cadence.

- **Autoconfig**: manufacturer 18260 (MacroSystem), product 12, Zorro II,
  single 64 KB I/O window, no autoboot ROM (the real board's `romtype` is
  `ROMTYPE_NOT`).
- **Register window**: status/control, FIFO data, and the AD1848
  index/data ports are decoded by address-line pattern (`A14`/`A13`/`A11`/
  `A0`), matching the reference's own decode rather than exact-address
  matching. Each port mirrors across several KB of the window, and the
  AD1848 ports only respond on odd byte addresses. Anything the pattern
  does not match is open bus within the board's own window (reads 0, writes
  drop), distinct from the Zorro chain's open bus (0xFF) outside any
  configured board. Word and longword accesses decompose into successive
  big-endian byte accesses at the same address; the board has no native
  word port.
- **Status/control register**: a write with bit 1 (reset) set stops the
  codec and clears the register and any pending interrupt, but leaves the
  FIFO alone. A write of exactly `0x01` is the FIFO-flush idiom. A read
  returns bit 7 low while an interrupt is pending, with the pending
  play/record half bits in bits 3 and 2, and acknowledges them (see
  Interrupt below).
- **AD1848**: reg 12 is pinned to `0x0A` regardless of what is written,
  which locks the codec to plain-AD1848 mode: no CS4231 extensions, and,
  since format bits 5 and 7 of reg 8 are never decoded, no u-law/A-law --
  only 8-bit unsigned and 16-bit signed linear PCM. Reg 8's crystal-select
  bit and three divider bits give 16 combinations: the AD1848's 14
  documented rates (5512.5-48000 Hz) plus two 24.576 MHz combinations
  (about 54.9 and 64 kHz) that the reference also accepts. Each rate is
  rounded to a multiple of 100 Hz the way the reference rounds it, so
  22050 Hz plays at 22000 Hz. The codec latches format and rate when reg
  9's play/record enables go from stopped to started, so reprogramming
  reg 8 mid-playback has no effect until the next start. Reg 6/7 are DAC
  output attenuation, applied to every produced sample including underrun
  repeats.
- **FIFO**: 1024 bytes, half-empty threshold 512, edge-triggered on the
  downward crossing only. Bytes written while the status register's
  play-FIFO enable (bit 4) is clear are dropped. A write to a full FIFO is
  also dropped silently, with no error flag: the board's IDT7202LA FIFO
  cannot overflow. Underrun discards any partial frame and repeats the last
  decoded sample rather than emitting silence: real silicon holds its last
  DAC value, and the reference models that exactly.
- **16-bit byte order**: a native `move.w` decomposes into big-endian byte
  pokes at the FIFO port, but the FIFO reads back little-endian, so writing
  word `0x1234` is heard by the codec as `0x3412`. Real Toccata drivers
  byte-swap around this hardware quirk, and Copperline does not "correct"
  it -- see `sixteen_bit_stereo_is_little_endian_and_byte_swapped_on_write`
  in `src/toccata/ad1848.rs`.

## Mixer cadence and resampling

The codec's own programmed rate is independent of Copperline's fixed
44.1 kHz mixer rate (established by the audio sink service, [](audio.md)).
`Toccata::tick` runs **two** independent exact-ratio accumulators, each
the same shape as `Paula::advance_audio`'s: `advance_codec` at the
AD1848's own active rate, and `advance_mixer` at the mixer's fixed rate.
The split is deliberate. A polyphase windowed-sinc resampler
(`src/audio/resample.rs`, shared with the MT-32 engine, the MHI board and
the CD32 FMV cartridge) is inherently non-causal: it needs taps on both
sides of the output instant and primes by pulling a full window's worth
of input before its first output. If the resampler pulled straight from
the chip (calling `Ad1848::produce_one_sample` from inside its own
`refill` closure), its first use after startup or a rate change would
drain dozens of FIFO bytes and evaluate the half-empty/interrupt
condition all at once, long before a real codec would reach them. The
resampler's lookahead would then leak into hardware-visible timing.

`advance_codec` is what actually drains the FIFO and evaluates the
half-empty/interrupt condition (the same per-sample unit as the
reference's `audio_state_sndboard_toccata`), paced causally by `cck` at
the codec's own rate. Each produced sample is queued into `decoded`, a
plain FIFO of raw pre-resample frames. `advance_mixer` pulls from that
passive queue through the resampler, never from the chip directly, and
repeats the chip's last known sample (via the side-effect-free
`Ad1848::peek_last_sample`) if `decoded` is momentarily empty. The
resampler's non-causal lookahead therefore only shapes the interpolated
*waveform*; it can never reorder when a FIFO byte drains or an interrupt
is raised. Resamplers are cached per codec rate (`Toccata`'s `resamplers`
map, at most 16 entries, one per crystal/divider combination), so
returning to an already-programmed rate never rebuilds its kernel table.

Produced frames are pushed into `ToccataAudioRing` (`src/chipset/paula.rs`,
alongside `CdAudioRing` and `MhiAudioRing`), from which `push_mixed_frame`
pops one frame per mixer tick. This is a plain per-frame pop, not a rate
conversion, since the board has already resampled. Unlike CD-DA's bursty
per-sector delivery, the board's tick cadence matches the mixer's, so
both the `decoded` queue and the ring stay near-empty in steady state.
Their fixed capacities (4096 frames each) are a safety margin against a
stalled consumer, not a buffering requirement.

The debugger's Audio tab gives a fitted board its own row, built from
`Toccata::debug_status`: playing or idle, the rate and format latched at
the last codec start, and the FIFO fill level, with an oscilloscope of the
board's mixer source and a mute toggle for it.

A cached resampler's `history` buffer holds the last 64 input frames it
convolves over. `Toccata::reset()` -- called on every system reset
(keyboard reset, the CPU's RESET instruction, or a power cycle), like
every other board's `ZorroDevice::reset()` -- therefore zeroes both
accumulators, clears `decoded`, and clears the whole resampler cache, not
just `Ad1848`'s own registers and FIFO. Otherwise up to 64 frames of
pre-reset audio would bleed through the stale kernel window into what
should be post-reset silence. The test
`reset_clears_stale_resampler_history_so_silence_follows_immediately` in
`src/toccata.rs` covers this.

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

(toccata-determinism)=
## Determinism

Every board-side computation -- register writes, FIFO drains, interrupt
evaluation, the resampler's phase -- is driven purely by `tick`'s `cck`
argument or by CPU register accesses, both already deterministic inputs.
Nothing reads wall-clock time, so a Toccata-fitted machine is warp-safe
and reproducible exactly like the rest of the emulated audio path (see
the [audio chapter's determinism section](audio.md#audio-determinism)): two
runs of the same scripted scenario produce byte-identical `toccata.wav`
stem captures.

(toccata-save-states)=
## Save states

`Toccata` derives `Serialize`/`Deserialize` directly (`Box`ed as
`BoardDevice::Toccata`, like `Picasso2`) with no `#[serde(skip)]` fields.
`codec_acc` and `decoded` are genuine machine state for the reason given
above. The `resamplers` cache only shapes the waveform, never FIFO/IRQ
timing, but it is serialized too, via `Resampler`'s own manual
`Serialize`/`Deserialize` in `src/audio/resample.rs`. Its derived
`kernels` table is rebuilt from `l`/`m` on load rather than stored, since
it is a pure function of the reduced rate ratio. A save-state load
therefore reproduces an uninterrupted run's *output* exactly, not just
its FIFO/IRQ timing: a resumed run's `toccata.wav` stem is byte-identical
to what an uninterrupted run would have produced at the same point
(`savestate_round_trip_reproduces_an_uninterrupted_runs_output` in
`src/toccata.rs`).

The cache is a `HashMap`, so the order in which its entries serialize is
not stable between processes. The restored state is the same either way,
but two state files of the same machine need not be byte-identical. That
is why netplay, whose peers must produce byte-identical checkpoints,
refuses a Toccata-fitted machine (`src/netplay/mod.rs`).

## Out of scope

- **Record** (the board's capture FIFO/interrupt path) is only an inert
  stub. The record port exists and acknowledges the record-half interrupt
  bit, but reads return 0 and the record half-full latch is never set, so
  the record interrupt never fires.
- **The "Paula/CD audio mixer" board setting** (reg 2-5's AUX1/AUX2 input
  gain feeding the board's own analog mixer) is not modelled: Copperline's
  Paula and CD-DA already reach the master mix directly, so replicating
  the board's own internal mixing would add no user-visible capability.
- The launcher's **I/O Ports** tab (Audio category) has a plain
  fit/don't-fit toggle for the board. Host-side audio capture and backend
  options (`--audio-wav`, `--audio-stems`, device selection) are not
  exposed there and stay command-line/config-file only.
