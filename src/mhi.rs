// SPDX-License-Identifier: GPL-3.0-or-later

//! MHI: a virtual MPEG (MPEG-1/2/2.5 Layer III) audio decoder board. See
//! `docs/internals/mhi.md` -- the contract this module implements exactly
//! -- for the register protocol, and `docs/internals/toccata.md`/
//! `docs/internals/audio.md` for the mixer-cadence pattern this module
//! reuses (the causal native-rate producer / non-causal mixer-rate
//! resampler split, and per-rate `Resampler` caching).
//!
//! ## Decoder choice
//!
//! Decoding uses [`minimp3-sys`](https://crates.io/crates/minimp3-sys), MIT-
//! licensed bindgen bindings (`build.rs` compiles the bundled C through the
//! `cc` crate -- clean on macOS/Linux/Windows, no vendored dependency and no
//! system minimp3) around lieff/minimp3.c itself (CC0). This module talks to
//! the raw `mp3dec_init`/`mp3dec_decode_frame` FFI directly (see
//! [`Decoder`]) rather than going through a higher-level wrapper crate: the
//! board needs to feed the decoder byte-queue-at-a-time from a doorbell-fed
//! bitstream and snapshot its cross-frame state for savestates, neither of
//! which a `Read`-based wrapper's ownership model fits.
//!
//! `mp3dec_t` is built here **without** `MINIMP3_FLOAT_OUTPUT`, i.e.
//! `mp3d_sample_t` is `int16_t`: the synthesis filterbank's own math is
//! still IEEE-754 float internally (there is no fully fixed-point mode in
//! upstream minimp3), but the *output* is quantized to int16 inside the C
//! library before this module ever sees it, exactly the "C float path"
//! every other minimp3 consumer relies on for platform-independent, bit-
//! exact decode -- ordinary `+`/`-`/`*`/`/` on `f32`/`f64` is IEEE-754-exact
//! on every target this project builds for (no x87, no fast-math), and
//! minimp3 uses no transcendental libm calls in its hot decode path (its
//! DCT/window tables are compile-time constants). This module then converts
//! the resulting `i16` PCM to `f32` the same way `Ad1848::produce_one_sample`
//! does (`f32::from(sample) / 32768.0`), so the board's own downstream
//! mixing/resampling stays in the project's usual float domain without
//! reintroducing any platform-dependent step.
//!
//! ## Savestates
//!
//! The decoder's cross-frame state (`mp3dec_t`'s MDCT overlap, QMF state,
//! and 511-byte bit reservoir) is genuine machine state -- restoring
//! mid-stream without it would audibly glitch the next frame or two. Rather
//! than reinterpret the raw FFI struct's bytes (a `size_of` canary can catch
//! a layout change but not silently-compatible-looking corruption), this
//! module keeps a field-for-field [`DecoderSnapshot`] shadow struct that
//! `serde` derives normally (via `serde-big-array` for the two oversized
//! float arrays) and copies to/from the live `mp3dec_t` by value -- a
//! genuine upstream field change fails to *compile* here instead of
//! deserializing into the wrong offsets. The board additionally keeps every
//! not-yet-decoded byte of every queued descriptor (`bitstream`/
//! `desc_lengths`) and the in-flight decoded frame's un-played sample tail
//! (`current_frame`), so a save/restore cycle reproduces an uninterrupted
//! run's output exactly -- proved by
//! `tests::savestate_round_trip_reproduces_an_uninterrupted_runs_output`.

use crate::audio::resample::Resampler;
use crate::audio::MIX_SAMPLE_RATE;
use crate::chipset::paula::{MhiAudioRing, PAULA_CLOCK_HZ};
use crate::zorro_device::{DeviceHost, ZorroDevice};
use std::collections::{HashMap, VecDeque};

// Register offsets (word-aligned, within the board's 64K window). See
// docs/internals/mhi.md's "Register map".
const OFF_VERSION: u32 = 0x00;
const OFF_CAPS: u32 = 0x02;
const OFF_STATUS: u32 = 0x04;
const OFF_CONTROL: u32 = 0x06;
const OFF_INTREQ: u32 = 0x08;
const OFF_INTENA: u32 = 0x0A;
const OFF_QUEUE_DEPTH: u32 = 0x0C;
const OFF_QUEUE_COUNT: u32 = 0x0E;
const OFF_DESC_ADDR_HI: u32 = 0x10;
const OFF_DESC_ADDR_LO: u32 = 0x12;
const OFF_DESC_LEN_HI: u32 = 0x14;
const OFF_DESC_LEN_LO: u32 = 0x16;
const OFF_DOORBELL: u32 = 0x18;
const OFF_COMPLETED_COUNT: u32 = 0x1A;
const OFF_PARAM_SELECT: u32 = 0x1C;
const OFF_PARAM_VALUE: u32 = 0x1E;

/// The register-protocol version this board implements (`docs/internals/
/// mhi.md`'s "Versioning"). Bumped 1 -> 2 by M4: the param latches'
/// documented semantic changed from "latched, otherwise inert" to
/// "affects the next produced sample" -- see that section's worked
/// v1->v2 example.
const VERSION: u16 = 2;

/// MPEG-1/2/2.5, Layer III, CBR, VBR-as-input (no seek), and (M4) param
/// latches applied to decoded PCM -- bits 0-6; see the `CAPS` table. Bits
/// 7-15 (Layer I/II, the 5/10-band EQ, a future capability) stay 0.
const CAPS: u16 = 0b0111_1111;

/// `QUEUE_DEPTH`'s fixed value.
const QUEUE_DEPTH: u16 = 16;

// INTREQ/INTENA bit layout.
const INT_BUFFER_DONE: u16 = 1 << 0;
const INT_OUT_OF_DATA: u16 = 1 << 1;
const INT_QUEUE_OVERFLOW: u16 = 1 << 2;

// CONTROL command values.
const CMD_PLAY: u16 = 1;
const CMD_PAUSE: u16 = 2;
const CMD_STOP: u16 = 3;

/// Number of defined param-latch indices (`0`=volume .. `6`=prefactor);
/// indices `7..=65535` are reserved -- read `0`, writes have no effect (see
/// "Param latches").
const PARAM_COUNT: usize = 7;
const PARAM_DEFAULTS: [u16; PARAM_COUNT] = [100, 50, 50, 50, 50, 0, 50];

/// An implementation-only safety bound on a single descriptor's DMA copy
/// (the spec's `DESC_LEN_*` pair is a full 32-bit byte count with no
/// protocol-level cap beyond Zorro II's 24-bit address space). 1 MiB is
/// generous against the "typical 32 KiB MP3 buffers" the spec's own
/// `QUEUE_DEPTH` rationale cites, while bounding a single misbehaving
/// doorbell write's host-side allocation.
const MAX_DESCRIPTOR_BYTES: u32 = 1024 * 1024;

/// Bound on how long (in Paula-clock ticks, ~0.1 s of emulated time) decode
/// may sit stalled on an incomplete trailing frame -- `consumed == 0`, not
/// enough buffered bytes to tell even a resync boundary -- with no further
/// bytes arriving to complete it, before giving up. Comfortably longer than
/// any real doorbell round-trip (the guest queueing the frame's remaining
/// bytes in its next descriptor), short enough not to visibly stall
/// playback when nothing more is ever coming (stream truly ended mid-frame,
/// or a deliberate underrun). See `reclaim_stalled_descriptor`.
const MAX_STALL_TICKS: u64 = (PAULA_CLOCK_HZ / 10) as u64;

/// A generous safety margin on the pre-resample sample queue -- see
/// `Toccata`'s identically-named constant; the native producer and mixer
/// consumer stay in near-lockstep in steady state, so this is a cap against
/// a stalled consumer, not a buffering requirement.
const DECODED_CAPACITY: usize = 4096;

/// Bytes offered to `mp3dec_decode_frame` per call: comfortably more than
/// the largest legal Layer III frame (1440 bytes at 320 kbps/32 kHz; MPEG-2
/// LSF frames are smaller still), so a full frame is always visible in one
/// call once its bytes are queued.
const MAX_FRAME_INPUT: usize = 4096;

/// Bound on how many resync attempts (calls into `mp3dec_decode_frame` that
/// find no valid frame -- junk/ID3 bytes, or a fake sync header that passes
/// minimp3's header check but fails Layer III decode) a single
/// `decode_next_frame` call performs before yielding back to its caller.
/// Real decoder firmware does not spend an unbounded slice of one
/// scheduling tick hunting for the next sync word either; it budgets a
/// bounded amount of resync work per tick and picks the search back up next
/// tick from wherever it left off. Because bytes scanned so far are already
/// popped from `bitstream` (and any descriptors they completed are already
/// latched in `pending_completes`) before this bound is checked, resuming
/// next tick is a continuation, not a rescan-from-scratch -- this is what
/// bounds a single tick's cost against a guest handing the board megabytes
/// of undecodable garbage while still letting genuine resync across a
/// corrupted frame boundary (a handful of skipped junk/ID3 bytes) complete
/// within one tick, exactly as it always has.
const MAX_RESYNC_ATTEMPTS_PER_TICK: u32 = 64;

/// `MINIMP3_MAX_SAMPLES_PER_FRAME`: 1152 samples * 2 channels, interleaved.
const MAX_SAMPLES_PER_FRAME: usize = 1152 * 2;

const MDCT_OVERLAP_LEN: usize = 2 * 9 * 32;
const QMF_STATE_LEN: usize = 15 * 2 * 32;
const RESERV_BUF_LEN: usize = 511;

/// The board's transport state (`STATUS`, `0x04`). Deliberately its own
/// numbering -- see the register map's note on why this does not match
/// `MHIF_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Status {
    Stopped,
    Playing,
    Paused,
    OutOfData,
}

impl Status {
    fn as_u16(self) -> u16 {
        match self {
            Status::Stopped => 0,
            Status::Playing => 1,
            Status::Paused => 2,
            Status::OutOfData => 3,
        }
    }
}

/// Field-for-field shadow of `minimp3_sys::mp3dec_t`, serde-derivable so a
/// savestate carries the decoder's cross-frame reservoir/filterbank state
/// without reinterpreting raw FFI bytes. See the module doc comment.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct DecoderSnapshot {
    #[serde(with = "serde_big_array::BigArray")]
    mdct_overlap: [f32; MDCT_OVERLAP_LEN],
    #[serde(with = "serde_big_array::BigArray")]
    qmf_state: [f32; QMF_STATE_LEN],
    reserv: i32,
    free_format_bytes: i32,
    header: [u8; 4],
    #[serde(with = "serde_big_array::BigArray")]
    reserv_buf: [u8; RESERV_BUF_LEN],
}

/// Thin safe wrapper around the raw `mp3dec_t`/`mp3dec_decode_frame` FFI
/// (see the module doc comment for why this talks to the raw bindings
/// rather than a higher-level decoder crate).
struct Decoder(minimp3_sys::mp3dec_t);

impl Decoder {
    fn new() -> Self {
        // SAFETY: `mp3dec_t` is a plain-old-data struct (float/int/byte
        // arrays, no pointers); zero is a valid bit pattern for every
        // field, and `mp3dec_init` only ever sets `header[0] = 0` (already
        // true after zeroing) -- see minimp3.c's own `mp3dec_init`.
        let mut raw: minimp3_sys::mp3dec_t = unsafe { std::mem::zeroed() };
        unsafe { minimp3_sys::mp3dec_init(&mut raw) };
        Decoder(raw)
    }

    /// Decode at most one frame (skipping any leading junk/sync-search
    /// bytes minimp3 itself walks past) from `input`. Returns the number of
    /// bytes minimp3 advanced past (0 means "not enough data buffered to
    /// make progress -- wait for more") and, when a real audio frame was
    /// decoded (as opposed to skipped junk), its channel-interleaved i16
    /// samples, sample rate, and channel count.
    fn decode_frame(&mut self, input: &[u8]) -> (u32, Option<(Vec<i16>, u32, usize)>) {
        let mut pcm = [0i16; MAX_SAMPLES_PER_FRAME];
        let mut info: minimp3_sys::mp3dec_frame_info_t = unsafe { std::mem::zeroed() };
        // SAFETY: `input` outlives the call, `pcm` is sized for the
        // documented maximum, and `info` is a plain-old-data out-param.
        let samples = unsafe {
            minimp3_sys::mp3dec_decode_frame(
                &mut self.0,
                input.as_ptr(),
                input.len() as std::os::raw::c_int,
                pcm.as_mut_ptr(),
                &mut info,
            )
        };
        let consumed = info.frame_bytes.max(0) as u32;
        if samples <= 0 {
            return (consumed, None);
        }
        let channels = info.channels.max(1) as usize;
        let count = samples as usize * channels;
        (
            consumed,
            Some((pcm[..count].to_vec(), info.hz.max(0) as u32, channels)),
        )
    }

    fn snapshot(&self) -> DecoderSnapshot {
        DecoderSnapshot {
            mdct_overlap: flatten_mdct(&self.0.mdct_overlap),
            qmf_state: self.0.qmf_state,
            reserv: self.0.reserv,
            free_format_bytes: self.0.free_format_bytes,
            header: self.0.header,
            reserv_buf: self.0.reserv_buf,
        }
    }

    fn restore(snapshot: &DecoderSnapshot) -> Self {
        Decoder(minimp3_sys::mp3dec_t {
            mdct_overlap: unflatten_mdct(&snapshot.mdct_overlap),
            qmf_state: snapshot.qmf_state,
            reserv: snapshot.reserv,
            free_format_bytes: snapshot.free_format_bytes,
            header: snapshot.header,
            reserv_buf: snapshot.reserv_buf,
        })
    }
}

fn flatten_mdct(a: &[[f32; 288]; 2]) -> [f32; MDCT_OVERLAP_LEN] {
    let mut out = [0f32; MDCT_OVERLAP_LEN];
    out[..288].copy_from_slice(&a[0]);
    out[288..].copy_from_slice(&a[1]);
    out
}

fn unflatten_mdct(a: &[f32; MDCT_OVERLAP_LEN]) -> [[f32; 288]; 2] {
    let mut out = [[0f32; 288]; 2];
    out[0].copy_from_slice(&a[..288]);
    out[1].copy_from_slice(&a[288..]);
    out
}

impl serde::Serialize for Decoder {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.snapshot().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Decoder {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let snapshot = DecoderSnapshot::deserialize(deserializer)?;
        Ok(Decoder::restore(&snapshot))
    }
}

/// A decoded MPEG frame's samples, mid-playback position, and the
/// descriptor-completion count its decode drained (see `Mhi::finish_frame`).
#[derive(serde::Serialize, serde::Deserialize)]
struct CurrentFrame {
    samples: Vec<(f32, f32)>,
    idx: usize,
    rate: u32,
    /// How many queued descriptors had their last byte consumed while
    /// decoding this frame -- applied to `COMPLETED_COUNT`/`QUEUE_COUNT`/
    /// `INTREQ.BUFFER_DONE` only once this frame's samples finish playing
    /// out (see "Determinism and timing").
    completes: u32,
}

// ---------------------------------------------------------------------
// M4: the param-latch DSP chain (docs/internals/mhi.md's "M4: the DSP
// chain"). Runs in the causal native-rate producer, before the FIFO a
// non-causal resampler pulls from -- see that section and "Determinism
// and timing".
// ---------------------------------------------------------------------

/// Volume (index 0): `value / 100.0` -- `0` silence, `100` (default) unity.
fn volume_gain(value: u16) -> f32 {
    f32::from(value) / 100.0
}

/// Prefactor (index 6): `value / 50.0` -- `50` (default) unity, `100` is
/// `2.0` (+6.02 dB headroom).
fn prefactor_gain(value: u16) -> f32 {
    f32::from(value) / 50.0
}

/// Panning (index 1): a linear balance control (not a constant-power pan
/// law) -- `(gain_left, gain_right)`. Both are exactly `1.0` at `value =
/// 50` (default), so the default param set is a no-op on decoded audio.
fn pan_gains(value: u16) -> (f32, f32) {
    let p = f32::from(value) / 100.0;
    ((2.0 * (1.0 - p)).min(1.0), (2.0 * p).min(1.0))
}

/// Crossmixing (index 5): stereo-to-mono blend fraction, `value / 100.0`
/// -- `0` (default) leaves channels untouched, `100` collapses both to
/// the identical mono sum.
fn crossmix_fraction(value: u16) -> f32 {
    f32::from(value) / 100.0
}

fn apply_crossmix(left: f32, right: f32, mix: f32) -> (f32, f32) {
    let mono = (left + right) * 0.5;
    (
        left * (1.0 - mix) + mono * mix,
        right * (1.0 - mix) + mono * mix,
    )
}

/// A tone band's gain in dB from its `0`-`100` latch value: `50`
/// (default) is exactly `0.0` dB (identity filter).
fn band_gain_db(value: u16) -> f32 {
    (f32::from(value) - 50.0) / 50.0 * 12.0
}

/// One 2nd-order IIR section, Direct Form II transposed -- the same
/// structure and `process` shape as `crate::chipset::paula`'s
/// `BiquadLowPass`/`AnalogLedFilter`, reused here rather than inventing a
/// second filter-processing convention in the same codebase. Holds only
/// coefficients and state; coefficient derivation (RBJ Audio EQ Cookbook)
/// lives in the `*_coeffs` constructors below, per docs/internals/mhi.md's
/// "Tone filters: bass/mid/treble".
#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// Corner frequencies are clamped below `sample_rate_hz * 0.45` before
    /// coefficients are derived, so a low-sample-rate MPEG-2.5 stream
    /// never asks for a corner at or past Nyquist.
    fn clamp_corner(corner_hz: f32, sample_rate_hz: f32) -> f32 {
        corner_hz.min(sample_rate_hz * 0.45)
    }

    fn peaking(corner_hz: f32, q: f32, gain_db: f32, sample_rate_hz: f32) -> Self {
        let f0 = Self::clamp_corner(corner_hz, sample_rate_hz);
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * f0 / sample_rate_hz;
        let (sn, cs) = (w0.sin(), w0.cos());
        let alpha = sn / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cs;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cs;
        let a2 = 1.0 - alpha / a;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    fn low_shelf(corner_hz: f32, shelf_slope: f32, gain_db: f32, sample_rate_hz: f32) -> Self {
        let f0 = Self::clamp_corner(corner_hz, sample_rate_hz);
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * f0 / sample_rate_hz;
        let (sn, cs) = (w0.sin(), w0.cos());
        let sqrt_a = a.sqrt();
        let alpha = sn / 2.0 * ((a + 1.0 / a) * (1.0 / shelf_slope - 1.0) + 2.0).sqrt();

        let b0 = a * ((a + 1.0) - (a - 1.0) * cs + 2.0 * sqrt_a * alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cs);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cs - 2.0 * sqrt_a * alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cs + 2.0 * sqrt_a * alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cs);
        let a2 = (a + 1.0) + (a - 1.0) * cs - 2.0 * sqrt_a * alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    fn high_shelf(corner_hz: f32, shelf_slope: f32, gain_db: f32, sample_rate_hz: f32) -> Self {
        let f0 = Self::clamp_corner(corner_hz, sample_rate_hz);
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * f0 / sample_rate_hz;
        let (sn, cs) = (w0.sin(), w0.cos());
        let sqrt_a = a.sqrt();
        let alpha = sn / 2.0 * ((a + 1.0 / a) * (1.0 / shelf_slope - 1.0) + 2.0).sqrt();

        let b0 = a * ((a + 1.0) + (a - 1.0) * cs + 2.0 * sqrt_a * alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cs);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cs - 2.0 * sqrt_a * alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cs + 2.0 * sqrt_a * alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cs);
        let a2 = (a + 1.0) - (a - 1.0) * cs - 2.0 * sqrt_a * alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    #[allow(clippy::too_many_arguments)]
    fn normalized(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Replaces this section's coefficients with `new`'s, preserving `z1`/
    /// `z2` (the filter's own memory) -- a latch write recomputes what the
    /// filter *does* without discarding what it has already integrated,
    /// avoiding a click that a full reset would introduce.
    fn retune(&mut self, new: Biquad) {
        self.b0 = new.b0;
        self.b1 = new.b1;
        self.b2 = new.b2;
        self.a1 = new.a1;
        self.a2 = new.a2;
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }

    fn clear_state(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

const BASS_CORNER_HZ: f32 = 200.0;
const MID_CORNER_HZ: f32 = 1000.0;
const MID_Q: f32 = 1.0;
const TREBLE_CORNER_HZ: f32 = 4000.0;
const SHELF_SLOPE: f32 = 1.0;

/// The three-band bass/mid/treble filter bank, one independent `Biquad`
/// per channel per band (stereo image preserved except for whatever a
/// shelf/peak filter itself does to level). Recomputes coefficients only
/// when the latch values or the native sample rate actually change (both
/// are rare compared to per-sample processing), keeping each biquad's own
/// `z1`/`z2` memory intact across a recompute.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ToneFilterBank {
    bass_l: Biquad,
    bass_r: Biquad,
    mid_l: Biquad,
    mid_r: Biquad,
    treble_l: Biquad,
    treble_r: Biquad,
    /// `(bass_value, mid_value, treble_value, native_rate)` coefficients
    /// were last derived for; `None` before the first sample (forces the
    /// first `retune_if_stale` call to compute real coefficients rather
    /// than trusting the all-zero `Default` state).
    tuned_for: Option<(u16, u16, u16, u32)>,
}

impl ToneFilterBank {
    fn retune_if_stale(&mut self, bass: u16, mid: u16, treble: u16, rate: u32) {
        let key = (bass, mid, treble, rate);
        if self.tuned_for == Some(key) {
            return;
        }
        let rate_hz = rate as f32;
        let bass_coeffs =
            Biquad::low_shelf(BASS_CORNER_HZ, SHELF_SLOPE, band_gain_db(bass), rate_hz);
        let mid_coeffs = Biquad::peaking(MID_CORNER_HZ, MID_Q, band_gain_db(mid), rate_hz);
        let treble_coeffs =
            Biquad::high_shelf(TREBLE_CORNER_HZ, SHELF_SLOPE, band_gain_db(treble), rate_hz);
        self.bass_l.retune(bass_coeffs);
        self.bass_r.retune(bass_coeffs);
        self.mid_l.retune(mid_coeffs);
        self.mid_r.retune(mid_coeffs);
        self.treble_l.retune(treble_coeffs);
        self.treble_r.retune(treble_coeffs);
        self.tuned_for = Some(key);
    }

    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let left = self
            .treble_l
            .process(self.mid_l.process(self.bass_l.process(left)));
        let right = self
            .treble_r
            .process(self.mid_r.process(self.bass_r.process(right)));
        (left, right)
    }

    /// Zeroes every band's `z1`/`z2` memory without discarding its tuned
    /// coefficients (avoids an unnecessary recompute when the latch
    /// values that produced them haven't changed) -- used by `STOP`, so a
    /// stopped stream's filter ringing doesn't bleed into whatever plays
    /// next, the same reasoning as `STOP`'s resampler-history clear.
    fn clear_state(&mut self) {
        self.bass_l.clear_state();
        self.bass_r.clear_state();
        self.mid_l.clear_state();
        self.mid_r.clear_state();
        self.treble_l.clear_state();
        self.treble_r.clear_state();
    }
}

/// The MHI board: `ZorroDevice` glue and register-window decode around
/// [`Decoder`], plus the same two-cadence split as `Toccata` -- a causal
/// native-rate producer (`advance_native`) that paces decode and
/// descriptor completion at the decoded stream's own sample rate, and a
/// non-causal mixer-rate resampler (`advance_mixer`) that never calls back
/// into decode. See `docs/internals/mhi.md`'s "Determinism and timing".
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Mhi {
    intreq: u16,
    intena: u16,
    status: Status,

    // Descriptor-address/length staging latches (`DESC_ADDR_*`/`DESC_LEN_*`,
    // WO -- committed by a `DOORBELL` write; see "Descriptor queue and
    // doorbell"). Stored as independent halves so a byte write to one half
    // leaves the other untouched, matching "Access size and alignment".
    desc_addr_hi: u16,
    desc_addr_lo: u16,
    desc_len_hi: u16,
    desc_len_lo: u16,

    /// Byte length of each queued descriptor not yet fully consumed by
    /// decode, oldest first; the front entry's value is decremented as
    /// decode advances through it. `desc_lengths.len()` plus the current
    /// frame's still-pending `completes` (if any) is `QUEUE_COUNT`.
    desc_lengths: VecDeque<u32>,
    /// The concatenated bytes of every queued descriptor not yet consumed
    /// by decode, in enqueue order -- descriptor boundaries live in
    /// `desc_lengths`, not here, since one decoded frame's bytes may span
    /// two descriptors.
    bitstream: VecDeque<u8>,
    completed_count: u16,

    param_select: u16,
    params: [u16; PARAM_COUNT],

    decoder: Decoder,
    /// Descriptor-completion count decode has produced but not yet
    /// attached to a `CurrentFrame` (junk/ID3 bytes skipped between
    /// descriptor boundaries and the next real frame, or bytes that
    /// complete a descriptor with nothing left to decode at all -- see
    /// `handle_out_of_data`).
    pending_completes: u32,
    current_frame: Option<CurrentFrame>,
    /// Native (stream) sample rate accumulator, in units of colour-clocks *
    /// Hz -- the exact same shape as `Toccata::codec_acc`, at whatever rate
    /// `current_frame` (or the last decoded frame) reports.
    sample_acc: u64,
    /// The most recently decoded frame's sample rate, kept even after that
    /// frame finishes so the mixer has a resampler to reuse (and so an
    /// idle/never-played board has a defined ratio -- silence resamples to
    /// silence regardless of rate, since every `Resampler` kernel is unity
    /// gain at DC).
    native_rate: u32,
    /// Pre-resample samples `advance_native` has produced and
    /// `advance_mixer` hasn't consumed yet.
    decoded: VecDeque<(f32, f32)>,
    mixer_acc: u64,
    resamplers: HashMap<u32, Resampler>,
    /// Whether the most recent `decode_next_frame` call returned `None`
    /// because of an incomplete trailing frame (`consumed == 0`) rather
    /// than a genuinely empty bitstream or an exhausted resync budget --
    /// only that case should accumulate `stall_ticks`. Transient, not
    /// meaningful except immediately after a `decode_next_frame` call.
    #[serde(skip)]
    last_decode_stalled: bool,
    /// Emulated Paula-clock ticks accumulated while stalled this way; see
    /// `MAX_STALL_TICKS`.
    stall_ticks: u64,
    /// M4's bass/mid/treble filter bank -- see `docs/internals/mhi.md`'s
    /// "M4: the DSP chain".
    tone_filters: ToneFilterBank,
}

impl Mhi {
    pub fn new() -> Self {
        Self {
            intreq: 0,
            intena: 0,
            status: Status::Stopped,
            desc_addr_hi: 0,
            desc_addr_lo: 0,
            desc_len_hi: 0,
            desc_len_lo: 0,
            desc_lengths: VecDeque::new(),
            bitstream: VecDeque::new(),
            completed_count: 0,
            param_select: 0,
            params: PARAM_DEFAULTS,
            decoder: Decoder::new(),
            pending_completes: 0,
            current_frame: None,
            sample_acc: 0,
            native_rate: 44_100,
            decoded: VecDeque::new(),
            mixer_acc: 0,
            resamplers: HashMap::new(),
            last_decode_stalled: false,
            stall_ticks: 0,
            tone_filters: ToneFilterBank::default(),
        }
    }

    /// `QUEUE_COUNT`: descriptors enqueued but not yet fully played out.
    fn queue_count(&self) -> u32 {
        self.desc_lengths.len() as u32 + self.current_frame.as_ref().map_or(0, |f| f.completes)
    }

    fn merge_half(old: u16, off: u32, size: usize, value: u32) -> u16 {
        if size >= 2 {
            value as u16
        } else if off & 1 == 0 {
            (old & 0x00FF) | ((value as u16 & 0xFF) << 8)
        } else {
            (old & 0xFF00) | (value as u16 & 0xFF)
        }
    }

    fn word_read(&self, word_off: u32) -> u16 {
        match word_off {
            OFF_VERSION => VERSION,
            OFF_CAPS => CAPS,
            OFF_STATUS => self.status.as_u16(),
            OFF_INTREQ => self.intreq,
            OFF_INTENA => self.intena,
            OFF_QUEUE_DEPTH => QUEUE_DEPTH,
            OFF_QUEUE_COUNT => self.queue_count() as u16,
            OFF_COMPLETED_COUNT => self.completed_count,
            OFF_PARAM_SELECT => self.param_select,
            OFF_PARAM_VALUE => self.param_value(self.param_select),
            // CONTROL, DOORBELL, DESC_ADDR_*/DESC_LEN_* are WO: reads
            // return 0. Anything else is reserved: also 0.
            _ => 0,
        }
    }

    fn param_value(&self, index: u16) -> u16 {
        self.params.get(index as usize).copied().unwrap_or(0)
    }

    fn write_reg(&mut self, off: u32, size: usize, value: u32, host: &mut DeviceHost) {
        let word_off = off & !1;
        match word_off {
            OFF_INTREQ => {
                // Write-1-to-clear, exactly the bits present in the bytes
                // actually written -- see "Interrupts".
                let mask: u16 = if size >= 2 {
                    value as u16
                } else if off & 1 == 0 {
                    (value as u16 & 0xFF) << 8
                } else {
                    value as u16 & 0xFF
                };
                self.intreq &= !mask;
            }
            OFF_INTENA => self.intena = Self::merge_half(self.intena, off, size, value),
            OFF_CONTROL => {
                let cmd: u16 = if size >= 2 {
                    value as u16
                } else if off & 1 == 1 {
                    value as u16 & 0xFF
                } else {
                    (value as u16 & 0xFF) << 8
                };
                self.handle_control(cmd);
            }
            OFF_DESC_ADDR_HI => {
                self.desc_addr_hi = Self::merge_half(self.desc_addr_hi, off, size, value)
            }
            OFF_DESC_ADDR_LO => {
                self.desc_addr_lo = Self::merge_half(self.desc_addr_lo, off, size, value)
            }
            OFF_DESC_LEN_HI => {
                self.desc_len_hi = Self::merge_half(self.desc_len_hi, off, size, value)
            }
            OFF_DESC_LEN_LO => {
                self.desc_len_lo = Self::merge_half(self.desc_len_lo, off, size, value)
            }
            OFF_DOORBELL => self.handle_doorbell(host),
            OFF_PARAM_SELECT => {
                self.param_select = Self::merge_half(self.param_select, off, size, value)
            }
            OFF_PARAM_VALUE => {
                let old = self.param_value(self.param_select);
                let merged = Self::merge_half(old, off, size, value);
                if let Some(slot) = self.params.get_mut(self.param_select as usize) {
                    *slot = merged.min(100);
                }
                // Reserved indices: the write has no observable effect --
                // nothing is stored, so a readback still returns 0.
            }
            // RO registers (VERSION/CAPS/STATUS/QUEUE_DEPTH/QUEUE_COUNT/
            // COMPLETED_COUNT) and reserved offsets: writes discarded.
            _ => {}
        }
    }

    fn handle_control(&mut self, cmd: u16) {
        match cmd {
            CMD_PLAY => self.cmd_play(),
            CMD_PAUSE => self.cmd_pause(),
            CMD_STOP => self.cmd_stop(),
            _ => {} // 0 (no-op) and anything above 3: reserved/inert.
        }
    }

    fn cmd_play(&mut self) {
        match self.status {
            Status::Stopped if self.queue_count() == 0 => {
                self.status = Status::OutOfData;
                self.intreq |= INT_OUT_OF_DATA;
            }
            Status::Stopped | Status::Paused => self.status = Status::Playing,
            Status::Playing | Status::OutOfData => {} // no-op
        }
    }

    fn cmd_pause(&mut self) {
        if self.status == Status::Playing {
            self.status = Status::Paused;
        }
    }

    fn cmd_stop(&mut self) {
        self.status = Status::Stopped;
        self.desc_lengths.clear();
        self.bitstream.clear();
        self.current_frame = None;
        self.pending_completes = 0;
        self.completed_count = 0;
        self.sample_acc = 0;
        self.decoded.clear();
        // "Stop all decoding" -- a fresh transport session starts with a
        // fresh decoder, not one still carrying the discarded stream's
        // bit reservoir into whatever plays next.
        self.decoder = Decoder::new();
        // Same reasoning as `reset`'s identical clear: a cached per-rate
        // `Resampler`'s convolution history would otherwise bleed the
        // stopped stream's tail into whatever plays next at the same
        // native rate.
        self.resamplers.clear();
        self.last_decode_stalled = false;
        self.stall_ticks = 0;
        self.tone_filters.clear_state();
    }

    fn handle_doorbell(&mut self, host: &mut DeviceHost) {
        if self.queue_count() >= u32::from(QUEUE_DEPTH) {
            self.intreq |= INT_QUEUE_OVERFLOW;
            return;
        }
        let addr = (u32::from(self.desc_addr_hi) << 16) | u32::from(self.desc_addr_lo);
        // `DESC_LEN_*` is a full 32-bit byte count with no protocol-level
        // cap ("does not truncate") -- `MAX_DESCRIPTOR_BYTES` bounds only a
        // single DMA-read call's host-side allocation below, never the
        // descriptor's own length or how many bytes actually get copied.
        let len = (u32::from(self.desc_len_hi) << 16) | u32::from(self.desc_len_lo);
        if len == 0 {
            // A zero-byte descriptor has no bytes to decode and no audio to
            // play out, so there is no frame boundary left to hang its
            // completion off -- it finishes the instant it is accepted
            // rather than lingering in `desc_lengths` forever (nothing
            // would ever visit it: `decode_next_frame` only walks
            // `desc_lengths` while consuming bytes out of a non-empty
            // `bitstream`). It never touches `desc_lengths`/`bitstream`, so
            // `QUEUE_COUNT` does not move -- see the doorbell's status
            // handling below.
            self.completed_count = self.completed_count.wrapping_add(1);
            self.intreq |= INT_BUFFER_DONE;
        } else {
            self.desc_lengths.push_back(len);
            // Copy the full descriptor in bounded chunks -- never more than
            // `MAX_DESCRIPTOR_BYTES` allocated for any single `dma_read`
            // call -- so an oversized `DESC_LEN` still lands every byte in
            // `bitstream` rather than silently dropping the tail past a
            // truncation cap.
            let mut remaining = len;
            let mut chunk_addr = addr;
            while remaining > 0 {
                let chunk_len = remaining.min(MAX_DESCRIPTOR_BYTES);
                let mut buf = vec![0u8; chunk_len as usize];
                host.dma_read(chunk_addr, &mut buf);
                self.bitstream.extend(buf);
                chunk_addr = chunk_addr.wrapping_add(chunk_len);
                remaining -= chunk_len;
            }
            if self.status == Status::OutOfData {
                // "The board resumes playback automatically... with no
                // CONTROL=PLAY required" -- this is specifically the
                // `QUEUE_COUNT` 0->1 transition (see "Out-of-data
                // semantics"), so it applies only when this doorbell
                // actually enqueued a descriptor; a zero-length doorbell
                // (the `if` branch above) leaves `QUEUE_COUNT` at 0 and
                // must not flip `STATUS` back to `PLAYING` with nothing
                // queued to play -- that would both misreport a
                // `PLAYING` readback and raise a second, spurious
                // `OUT_OF_DATA` on the very next tick.
                self.status = Status::Playing;
            }
        }
    }

    /// Consume `n` bytes from the front of `bitstream` (already accounted
    /// for by the caller's decode call) and attribute them to queued
    /// descriptors in order. Returns how many descriptors had their last
    /// byte consumed.
    fn consume_bytes(&mut self, mut n: u32) -> u32 {
        for _ in 0..n {
            self.bitstream.pop_front();
        }
        let mut completed = 0;
        while n > 0 {
            let Some(front) = self.desc_lengths.front_mut() else {
                break;
            };
            let take = n.min(*front);
            *front -= take;
            n -= take;
            if *front == 0 {
                self.desc_lengths.pop_front();
                completed += 1;
            }
        }
        completed
    }

    /// Decode at most one real audio frame, skipping any junk minimp3
    /// walks past first. `None` means no frame is available right now --
    /// either the bitstream is empty, what's buffered isn't enough for
    /// minimp3 to make progress (should not arise with whole-buffer CBR
    /// descriptors, the M1-M2 target), or this call's bounded resync budget
    /// (`MAX_RESYNC_ATTEMPTS_PER_TICK`) ran out while still hunting for the
    /// next real frame -- the caller (`acquire_next_frame`, from
    /// `advance_native`) tries again next tick, continuing from exactly the
    /// bitstream position this call left behind. Sets `last_decode_stalled`
    /// so the caller can tell the "buffered bytes can't make progress"
    /// case apart from the other two, which is the only one
    /// `reclaim_stalled_descriptor` should ever fire for.
    fn decode_next_frame(&mut self) -> Option<CurrentFrame> {
        self.last_decode_stalled = false;
        for _ in 0..MAX_RESYNC_ATTEMPTS_PER_TICK {
            if self.bitstream.is_empty() {
                return None;
            }
            let take = self.bitstream.len().min(MAX_FRAME_INPUT);
            let mut buf = [0u8; MAX_FRAME_INPUT];
            for (i, b) in self.bitstream.iter().take(take).enumerate() {
                buf[i] = *b;
            }
            let (consumed, frame) = self.decoder.decode_frame(&buf[..take]);
            if consumed == 0 {
                self.last_decode_stalled = true;
                return None;
            }
            self.pending_completes += self.consume_bytes(consumed);
            if let Some((samples_i16, rate, channels)) = frame {
                let samples = to_stereo_f32(&samples_i16, channels);
                self.native_rate = rate;
                let completes = std::mem::take(&mut self.pending_completes);
                return Some(CurrentFrame {
                    samples,
                    idx: 0,
                    rate,
                    completes,
                });
            }
            // Junk/ID3 bytes, or a fake-sync-but-undecodable frame: loop to
            // try again with what remains, within this call's bound.
        }
        // Resync budget exhausted for this tick: yield back rather than
        // keep scanning. `pending_completes` and `bitstream`/`desc_lengths`
        // already reflect every byte scanned so far, so nothing is lost or
        // rescanned when the caller comes back next tick.
        None
    }

    /// Gives up on an incomplete trailing frame that has sat unconsumable
    /// for `MAX_STALL_TICKS` with no doorbell ever completing it: discards
    /// the leftover bytes and completes/reclaims every descriptor they
    /// belong to, the same "skip and complete" treatment "Undecodable
    /// bitstream content" gives bytes that are outright undecodable rather
    /// than merely incomplete. Keeps a permanently truncated tail (stream
    /// genuinely ended mid-frame, or a deliberate underrun) from wedging
    /// `STATUS`/`QUEUE_COUNT` forever.
    fn reclaim_stalled_descriptor(&mut self) {
        let leftover = self.bitstream.len() as u32;
        self.pending_completes += self.consume_bytes(leftover);
        self.handle_out_of_data();
    }

    fn finish_frame(&mut self, frame: &CurrentFrame) {
        if frame.completes > 0 {
            self.completed_count = self.completed_count.wrapping_add(frame.completes as u16);
            self.intreq |= INT_BUFFER_DONE;
        }
    }

    /// Applies the out-of-data transition (and flushes any completion the
    /// decode that discovered it produced) when nothing more can be
    /// decoded right now. Only actually changes `STATUS` when the queue is
    /// genuinely empty -- see "Out-of-data semantics".
    fn handle_out_of_data(&mut self) {
        let flush = std::mem::take(&mut self.pending_completes);
        if flush > 0 {
            self.completed_count = self.completed_count.wrapping_add(flush as u16);
            self.intreq |= INT_BUFFER_DONE;
        }
        if self.desc_lengths.is_empty() && self.status == Status::Playing {
            self.status = Status::OutOfData;
            self.intreq |= INT_OUT_OF_DATA;
        }
    }

    /// Ensure `current_frame` is populated when possible; applies
    /// out-of-data bookkeeping otherwise. Returns whether a frame is now
    /// current.
    fn acquire_next_frame(&mut self) -> bool {
        match self.decode_next_frame() {
            Some(f) => {
                self.current_frame = Some(f);
                true
            }
            None => {
                self.handle_out_of_data();
                false
            }
        }
    }

    /// Produces one native-rate sample: pulls the next raw decoded sample
    /// from `current_frame`, runs it through the M4 param DSP chain (see
    /// `docs/internals/mhi.md`'s "M4: the DSP chain" -- prefactor -> tone
    /// filters -> volume -> pan -> crossmix, in that fixed order), and
    /// pushes the result into `decoded` for the mixer-rate resampler to
    /// pull from later.
    fn produce_one_sample(&mut self) {
        let frame = self
            .current_frame
            .as_mut()
            .expect("produce_one_sample requires a current frame");
        let (raw_l, raw_r) = frame.samples[frame.idx];
        let rate = frame.rate;
        frame.idx += 1;
        let frame_finished = frame.idx >= frame.samples.len();

        let params = self.params;
        self.tone_filters
            .retune_if_stale(params[2], params[3], params[4], rate);
        let pre = prefactor_gain(params[6]);
        let (l, r) = self.tone_filters.process(raw_l * pre, raw_r * pre);
        let vol = volume_gain(params[0]);
        let (gain_l, gain_r) = pan_gains(params[1]);
        let (l, r) = apply_crossmix(
            l * vol * gain_l,
            r * vol * gain_r,
            crossmix_fraction(params[5]),
        );

        if self.decoded.len() < DECODED_CAPACITY {
            self.decoded.push_back((l, r));
        }
        if frame_finished {
            let finished = self.current_frame.take().unwrap();
            self.finish_frame(&finished);
            self.acquire_next_frame();
        }
    }

    /// Causal native-rate cadence: paces decode and descriptor completion
    /// at the decoded stream's own sample rate. See `Toccata::advance_codec`
    /// for the identically-shaped exact-ratio accumulator this mirrors.
    fn advance_native(&mut self, cck: u32) {
        if self.status != Status::Playing {
            return;
        }
        if self.current_frame.is_none() {
            if !self.acquire_next_frame() {
                if self.last_decode_stalled {
                    self.stall_ticks += u64::from(cck);
                    if self.stall_ticks >= MAX_STALL_TICKS {
                        self.stall_ticks = 0;
                        self.reclaim_stalled_descriptor();
                    }
                } else {
                    self.stall_ticks = 0;
                }
                return;
            }
            self.stall_ticks = 0;
        }
        let rate = self.current_frame.as_ref().unwrap().rate;
        self.sample_acc += u64::from(cck) * u64::from(rate);
        while self.sample_acc >= u64::from(PAULA_CLOCK_HZ) && self.current_frame.is_some() {
            self.sample_acc -= u64::from(PAULA_CLOCK_HZ);
            self.produce_one_sample();
        }
    }

    /// Non-causal mixer-rate cadence: resamples already-produced native
    /// samples onto the mixer grid and pushes each frame into `ring`. Never
    /// calls back into decode -- see `Toccata::advance_mixer`'s doc
    /// comment for why that separation matters. While not playing (or
    /// simply starved), `decoded` is empty and the refill closure below
    /// returns exact silence, never a held-over last sample -- see
    /// "Out-of-data semantics"' "no repeat-last-sample holdover". While
    /// `PAUSED`, "halts ... audio output" means exactly that -- emit
    /// silence without touching `decoded` or advancing any `Resampler`, so
    /// whatever backlog was already decoded stays queued (not dropped) and
    /// the resampler's convolution history stays exactly where playback
    /// left it, ready for a gapless `PLAY` resume.
    fn advance_mixer(&mut self, cck: u32, ring: &mut MhiAudioRing) {
        self.mixer_acc += u64::from(cck) * u64::from(MIX_SAMPLE_RATE);
        while self.mixer_acc >= u64::from(PAULA_CLOCK_HZ) {
            self.mixer_acc -= u64::from(PAULA_CLOCK_HZ);
            if self.status == Status::Paused {
                ring.push_frame(0.0, 0.0);
                continue;
            }
            let rate = self.native_rate;
            let Self {
                decoded,
                resamplers,
                ..
            } = self;
            let resampler = resamplers
                .entry(rate)
                .or_insert_with(|| Resampler::new(rate, MIX_SAMPLE_RATE));
            let (left, right) = resampler.next(|| decoded.pop_front().unwrap_or((0.0, 0.0)));
            ring.push_frame(left, right);
        }
    }
}

fn to_stereo_f32(samples: &[i16], channels: usize) -> Vec<(f32, f32)> {
    if channels <= 1 {
        samples
            .iter()
            .map(|&s| {
                let v = f32::from(s) / 32768.0;
                (v, v)
            })
            .collect()
    } else {
        samples
            .chunks_exact(2)
            .map(|pair| (f32::from(pair[0]) / 32768.0, f32::from(pair[1]) / 32768.0))
            .collect()
    }
}

impl Default for Mhi {
    fn default() -> Self {
        Self::new()
    }
}

impl ZorroDevice for Mhi {
    fn read(&mut self, off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        let word = self.word_read(off & !1);
        match size {
            2 => u32::from(word),
            1 if off & 1 == 0 => u32::from(word >> 8),
            1 => u32::from(word & 0xFF),
            // Longword access is undefined at the protocol level; return
            // the word value rather than fault.
            _ => u32::from(word),
        }
    }

    fn write(&mut self, off: u32, size: usize, value: u32, host: &mut DeviceHost) {
        self.write_reg(off, size, value, host);
    }

    fn tick(&mut self, cck: u32, host: &mut DeviceHost) {
        self.advance_native(cck);
        self.advance_mixer(cck, host.mhi_audio());
    }

    fn int2_line(&self) -> bool {
        (self.intreq & self.intena) != 0
    }

    fn is_idle(&self) -> bool {
        self.status != Status::Playing
    }

    fn reset(&mut self) {
        self.intreq = 0;
        self.intena = 0;
        self.status = Status::Stopped;
        self.desc_addr_hi = 0;
        self.desc_addr_lo = 0;
        self.desc_len_hi = 0;
        self.desc_len_lo = 0;
        self.desc_lengths.clear();
        self.bitstream.clear();
        self.completed_count = 0;
        self.param_select = 0;
        self.params = PARAM_DEFAULTS;
        self.decoder = Decoder::new();
        self.pending_completes = 0;
        self.current_frame = None;
        self.sample_acc = 0;
        self.native_rate = 44_100;
        self.decoded.clear();
        self.mixer_acc = 0;
        // See Toccata::reset's doc comment: a cached resampler's history
        // buffer would otherwise bleed a few pre-reset frames into the
        // freshly reset silence.
        self.resamplers.clear();
        self.last_decode_stalled = false;
        self.stall_ticks = 0;
        // Same reasoning: a stale filter's z1/z2 memory would otherwise
        // ring a few pre-reset samples into the freshly reset silence,
        // even though `params` above already went back to their flat
        // defaults (a `Default` bank + `tuned_for: None` forces
        // `retune_if_stale` to genuinely recompute on the very next
        // sample rather than trusting an unrelated stale cache key).
        self.tone_filters = ToneFilterBank::default();
    }

    fn kind(&self) -> &'static str {
        "mhi"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;

    fn memory(chip_bytes: usize) -> Memory {
        Memory {
            chip_ram: vec![0; chip_bytes],
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom: Vec::new(),
            overlay: false,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        }
    }

    fn host_with_rings<'a>(
        mem: &'a mut Memory,
        cd_audio: &'a mut crate::chipset::paula::CdAudioRing,
        toccata_audio: &'a mut crate::chipset::paula::ToccataAudioRing,
        mhi_audio: &'a mut MhiAudioRing,
    ) -> DeviceHost<'a> {
        DeviceHost::for_slot_with_audio(mem, 0, cd_audio, toccata_audio, mhi_audio)
    }

    /// Encode one CBR (128 kbps, 44100 Hz, stereo) Layer III MPEG-1 frame by
    /// hand-assembling a minimal valid frame header and an all-zero side-
    /// info/main-data body. An all-zero body is not a corner-cutting
    /// shortcut: it is the only way to hand-author a *guaranteed-valid*
    /// Layer III frame without a real encoder (arbitrary/pseudo-random main
    /// data almost always desyncs the Huffman decode -- confirmed
    /// empirically against minimp3 itself -- and minimp3 then reports it as
    /// unusable junk, `samples == 0`, rather than as a completed frame).
    /// All-zero side info decodes to `big_values == 0`/`part2_3_length ==
    /// 0`, i.e. a frame with no spectral lines at all -- digital silence,
    /// but a genuinely *decoded* one (`samples == 1152`, a real
    /// `frame_bytes` consumed from the bitstream, the QMF/MDCT overlap
    /// state genuinely advanced) rather than skipped junk. That is exactly
    /// what these tests need: real frame boundaries, real byte consumption,
    /// real pacing -- payload *audibility* is not part of any claim they
    /// make (the payload seed only nudges header/reserved bits that don't
    /// change decode validity, so consecutive frames stay distinguishable
    /// at the register/bitstream level without one of them landing on an
    /// invalid encoding).
    fn mp3_frame(bitrate_kbps: u32, payload_seed: u8) -> Vec<u8> {
        // MPEG-1 Layer III header: sync (11) | version=11 (MPEG1) | layer=01
        // (Layer III) | protection=1 (no CRC) | bitrate index | sample-rate
        // index=00 (44100) | padding=0 | private=0 | mode=00 (stereo) |
        // mode-extension=00 | copyright=0 | original=0 | emphasis=00.
        let bitrate_index: u32 = match bitrate_kbps {
            32 => 1,
            64 => 5,
            128 => 9,
            _ => 9,
        };
        let b0 = 0xFFu8;
        let b1 = 0xFBu8; // 1111 1011: version 11, layer 01, protection 1
        let b2 = (bitrate_index as u8) << 4; // sr idx 00, no pad (both 0, folded into the shift)
                                             // Byte 3's low bit is half of the emphasis field -- a playback
                                             // hint the decoder itself ignores, so it is genuinely don't-care to
                                             // decode validity. Folding the seed in here (rather than into the
                                             // side info/main data body, which must stay all-zero to keep
                                             // decoding validly -- see the doc comment above) is enough to make
                                             // otherwise-identical frames distinguishable byte-for-byte in the
                                             // bitstream queue, keeping mode = stereo (bits 7-6 = 0) intact.
        let b3 = payload_seed & 0x01;
        let frame_len = (144 * (bitrate_kbps * 1000)) / 44100;
        let frame_len = frame_len.max(24) as usize;
        let mut frame = vec![0u8; frame_len];
        frame[0] = b0;
        frame[1] = b1;
        frame[2] = b2;
        frame[3] = b3;
        frame
    }

    /// A `mp3_frame`-shaped header (so minimp3's header check passes and it
    /// commits to decoding a whole frame's worth of bytes) with a
    /// pseudo-random, non-zero body -- per `mp3_frame`'s own doc comment,
    /// arbitrary main data almost always desyncs the Huffman decode, so
    /// minimp3 reports the frame as unusable junk (`samples == 0`) while
    /// still consuming the header-declared frame length. This is exactly
    /// the "fake sync header that passes minimp3's header checks but fails
    /// Layer III decode" shape the resync-budget bug describes.
    fn corrupt_frame(bitrate_kbps: u32, seed: u8) -> Vec<u8> {
        let mut frame = mp3_frame(bitrate_kbps, seed);
        let mut s = seed;
        for b in frame.iter_mut().skip(4) {
            s = s.wrapping_mul(37).wrapping_add(11);
            *b = s | 1; // never all-zero (all-zero body decodes as valid silence)
        }
        frame
    }

    /// A run of back-to-back `corrupt_frame`s, at least `min_bytes` long --
    /// a bitstream with no decodable frame anywhere in it.
    fn garbage_stream(min_bytes: usize, seed: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(min_bytes + 512);
        let mut s = seed;
        while out.len() < min_bytes {
            out.extend_from_slice(&corrupt_frame(128, s));
            s = s.wrapping_add(1);
        }
        out
    }

    fn stage_and_ring(board: &mut Mhi, host: &mut DeviceHost, addr: u32, bytes: &[u8]) {
        board.write(OFF_DESC_ADDR_HI, 2, addr >> 16, host);
        board.write(OFF_DESC_ADDR_LO, 2, addr & 0xFFFF, host);
        board.write(OFF_DESC_LEN_HI, 2, (bytes.len() as u32) >> 16, host);
        board.write(OFF_DESC_LEN_LO, 2, (bytes.len() as u32) & 0xFFFF, host);
        board.write(OFF_DOORBELL, 2, 0, host);
    }

    #[test]
    fn version_caps_and_queue_depth_are_fixed() {
        let mut board = Mhi::new();
        let mut mem = memory(0x100);
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(OFF_VERSION, 2, &mut host), 2);
        assert_eq!(board.read(OFF_CAPS, 2, &mut host), 0x7F);
        assert_eq!(board.read(OFF_QUEUE_DEPTH, 2, &mut host), 16);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 0); // STOPPED
    }

    #[test]
    fn wo_registers_read_zero_and_ro_registers_discard_writes() {
        let mut board = Mhi::new();
        let mut mem = memory(0x100);
        let mut host = DeviceHost::new(&mut mem);
        board.write(OFF_DOORBELL, 2, 0xBEEF, &mut host);
        assert_eq!(board.read(OFF_DOORBELL, 2, &mut host), 0);
        board.write(OFF_CONTROL, 2, 1, &mut host); // PLAY -- also exercises WO write not discarded
        board.write(OFF_STATUS, 2, 0xFFFF, &mut host); // RO: discarded
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 3); // OUT_OF_DATA (queue empty)
        board.write(OFF_QUEUE_DEPTH, 2, 0, &mut host); // RO: discarded
        assert_eq!(board.read(OFF_QUEUE_DEPTH, 2, &mut host), 16);
    }

    #[test]
    fn reserved_offsets_read_zero_and_discard_writes() {
        let mut board = Mhi::new();
        let mut mem = memory(0x100);
        let mut host = DeviceHost::new(&mut mem);
        for off in [0x20u32, 0x100, 0xFFFE] {
            board.write(off, 2, 0xFFFF, &mut host);
            assert_eq!(board.read(off, 2, &mut host), 0, "offset {off:#x}");
        }
    }

    #[test]
    fn intreq_write_1_to_clear_only_touches_set_bits() {
        let mut board = Mhi::new();
        board.intreq = INT_BUFFER_DONE | INT_QUEUE_OVERFLOW;
        let mut mem = memory(0x100);
        let mut host = DeviceHost::new(&mut mem);
        board.write(OFF_INTREQ, 2, u32::from(INT_BUFFER_DONE), &mut host);
        assert_eq!(
            board.read(OFF_INTREQ, 2, &mut host),
            u32::from(INT_QUEUE_OVERFLOW)
        );
        // Writing 0 never sets a bit.
        board.write(OFF_INTREQ, 2, 0, &mut host);
        assert_eq!(
            board.read(OFF_INTREQ, 2, &mut host),
            u32::from(INT_QUEUE_OVERFLOW)
        );
    }

    #[test]
    fn byte_write_only_changes_that_half_of_a_stored_register() {
        let mut board = Mhi::new();
        let mut mem = memory(0x100);
        let mut host = DeviceHost::new(&mut mem);
        board.write(OFF_INTENA, 2, 0x1234, &mut host);
        board.write(OFF_INTENA, 1, 0xAB, &mut host); // high byte only
        assert_eq!(board.read(OFF_INTENA, 2, &mut host), 0xAB34);
        board.write(OFF_INTENA + 1, 1, 0xCD, &mut host); // low byte only
        assert_eq!(board.read(OFF_INTENA, 2, &mut host), 0xABCD);
    }

    #[test]
    fn int2_line_is_the_level_sensitive_and_of_intreq_and_intena() {
        let mut board = Mhi::new();
        board.intreq = INT_BUFFER_DONE;
        board.intena = 0;
        assert!(!board.int2_line());
        board.intena = INT_BUFFER_DONE;
        assert!(board.int2_line());
    }

    #[test]
    fn param_latches_round_trip_and_clamp_and_reserved_indices_read_zero() {
        let mut board = Mhi::new();
        let mut mem = memory(0x100);
        let mut host = DeviceHost::new(&mut mem);
        // Default volume is 100.
        board.write(OFF_PARAM_SELECT, 2, 0, &mut host);
        assert_eq!(board.read(OFF_PARAM_VALUE, 2, &mut host), 100);
        // Panning (index 1), set then clamp above 100.
        board.write(OFF_PARAM_SELECT, 2, 1, &mut host);
        board.write(OFF_PARAM_VALUE, 2, 250, &mut host);
        assert_eq!(board.read(OFF_PARAM_VALUE, 2, &mut host), 100);
        // Reserved index: writes latch nothing, reads are 0.
        board.write(OFF_PARAM_SELECT, 2, 42, &mut host);
        board.write(OFF_PARAM_VALUE, 2, 77, &mut host);
        assert_eq!(board.read(OFF_PARAM_VALUE, 2, &mut host), 0);
    }

    #[test]
    fn doorbell_enqueues_a_descriptor_by_copying_amiga_memory() {
        let mut board = Mhi::new();
        let mut mem = memory(0x400);
        let payload = mp3_frame(128, 0x11);
        mem.chip_ram[0x10..0x10 + payload.len()].copy_from_slice(&payload);
        let mut host = DeviceHost::new(&mut mem);
        stage_and_ring(&mut board, &mut host, 0x10, &payload);
        assert_eq!(board.read(OFF_QUEUE_COUNT, 2, &mut host), 1);
        assert_eq!(board.bitstream.len(), payload.len());
        assert_eq!(board.bitstream.iter().copied().collect::<Vec<_>>(), payload);
    }

    #[test]
    fn doorbell_overflow_sets_the_diagnostic_bit_and_drops_the_descriptor() {
        let mut board = Mhi::new();
        let mut mem = memory(0x200);
        let mut host = DeviceHost::new(&mut mem);
        for _ in 0..16 {
            stage_and_ring(&mut board, &mut host, 0, &[0u8; 4]);
        }
        assert_eq!(board.read(OFF_QUEUE_COUNT, 2, &mut host), 16);
        assert_eq!(
            board.read(OFF_INTREQ, 2, &mut host) & u32::from(INT_QUEUE_OVERFLOW),
            0
        );
        stage_and_ring(&mut board, &mut host, 0, &[0u8; 4]); // 17th: dropped
        assert_eq!(board.read(OFF_QUEUE_COUNT, 2, &mut host), 16);
        assert_ne!(
            board.read(OFF_INTREQ, 2, &mut host) & u32::from(INT_QUEUE_OVERFLOW),
            0
        );
    }

    #[test]
    fn control_stop_flushes_the_queue_and_resets_counters() {
        let mut board = Mhi::new();
        let mut mem = memory(0x400);
        let payload = mp3_frame(128, 1);
        mem.chip_ram[0..payload.len()].copy_from_slice(&payload);
        let mut host = DeviceHost::new(&mut mem);
        stage_and_ring(&mut board, &mut host, 0, &payload);
        board.completed_count = 5; // pretend some history
        board.write(OFF_CONTROL, 2, CMD_STOP as u32, &mut host);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 0); // STOPPED
        assert_eq!(board.read(OFF_QUEUE_COUNT, 2, &mut host), 0);
        assert_eq!(board.read(OFF_COMPLETED_COUNT, 2, &mut host), 0);
    }

    #[test]
    fn control_play_from_stopped_with_empty_queue_reports_out_of_data() {
        let mut board = Mhi::new();
        let mut mem = memory(0x100);
        let mut host = DeviceHost::new(&mut mem);
        board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 3); // OUT_OF_DATA
        assert_ne!(
            board.read(OFF_INTREQ, 2, &mut host) & u32::from(INT_OUT_OF_DATA),
            0
        );
    }

    #[test]
    fn control_pause_only_takes_effect_while_playing() {
        let mut board = Mhi::new();
        let mut mem = memory(0x400);
        let mut host = DeviceHost::new(&mut mem);
        board.write(OFF_CONTROL, 2, CMD_PAUSE as u32, &mut host); // from STOPPED
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 0);

        let payload = mp3_frame(128, 2);
        mem.chip_ram[0..payload.len()].copy_from_slice(&payload);
        let mut host = DeviceHost::new(&mut mem);
        stage_and_ring(&mut board, &mut host, 0, &payload);
        board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 1); // PLAYING
        board.write(OFF_CONTROL, 2, CMD_PAUSE as u32, &mut host);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 2); // PAUSED
    }

    #[test]
    fn out_of_data_auto_resumes_on_the_next_doorbell() {
        let mut board = Mhi::new();
        let mut mem = memory(0x200);
        let mut host = DeviceHost::new(&mut mem);
        board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 3); // OUT_OF_DATA

        let payload = mp3_frame(128, 3);
        mem.chip_ram[0..payload.len()].copy_from_slice(&payload);
        let mut host = DeviceHost::new(&mut mem);
        stage_and_ring(&mut board, &mut host, 0, &payload);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 1); // PLAYING again
    }

    #[test]
    fn playing_a_known_frame_paces_completion_over_the_right_number_of_ccks() {
        let mut board = Mhi::new();
        let mut mem = memory(0x400);
        let payload = mp3_frame(128, 7);
        mem.chip_ram[0..payload.len()].copy_from_slice(&payload);
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        {
            let mut host =
                host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
            stage_and_ring(&mut board, &mut host, 0, &payload);
            board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
            assert_eq!(board.read(OFF_STATUS, 2, &mut host), 1);

            // 1152 samples at 44100 Hz: not yet complete a moment in.
            ZorroDevice::tick(&mut board, 10, &mut host);
            assert_eq!(board.read(OFF_COMPLETED_COUNT, 2, &mut host), 0);

            // The whole frame's worth of emulated time (1152 * PAULA_CLOCK_HZ
            // / 44100 ccks) definitely elapses the descriptor's completion.
            let frame_cck = (1152u64 * u64::from(PAULA_CLOCK_HZ)) / 44_100 + 10;
            ZorroDevice::tick(&mut board, frame_cck as u32, &mut host);
        }
        assert_eq!(board.completed_count, 1);
        assert_ne!(board.intreq & INT_BUFFER_DONE, 0);
        assert_eq!(board.queue_count(), 0);
    }

    #[test]
    fn silence_while_not_playing_has_no_last_sample_holdover() {
        let mut board = Mhi::new();
        let mut mem = memory(0x400);
        let payload = mp3_frame(128, 9);
        mem.chip_ram[0..payload.len()].copy_from_slice(&payload);
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        let mut host = host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
        stage_and_ring(&mut board, &mut host, 0, &payload);
        board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
        for _ in 0..2000 {
            ZorroDevice::tick(&mut board, 64, &mut host);
        }
        board.write(OFF_CONTROL, 2, CMD_PAUSE as u32, &mut host);
        for _ in 0..500 {
            ZorroDevice::tick(&mut board, 64, &mut host);
        }
        let (l, r) = mhi_audio.next_sample();
        assert_eq!((l, r), (0.0, 0.0));
    }

    #[test]
    fn reset_clears_stale_resampler_history_and_queue_state() {
        let mut board = Mhi::new();
        let mut mem = memory(0x400);
        let payload = mp3_frame(128, 11);
        mem.chip_ram[0..payload.len()].copy_from_slice(&payload);
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        let mut host = host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
        stage_and_ring(&mut board, &mut host, 0, &payload);
        board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
        for _ in 0..2000 {
            ZorroDevice::tick(&mut board, 64, &mut host);
        }

        board.reset();
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 0);
        assert_eq!(board.read(OFF_QUEUE_COUNT, 2, &mut host), 0);
        assert!(board.resamplers.is_empty());
        ZorroDevice::tick(&mut board, 64, &mut host);
        assert_eq!(mhi_audio.next_sample(), (0.0, 0.0));
    }

    /// Regression test for the "STOP doesn't clear resamplers" bug: `STOP`
    /// must clear cached `Resampler`s exactly like `reset()` does, or a
    /// fresh stream at the same native rate reuses the stopped stream's
    /// resampler and leaks its convolution history into the new audio.
    #[test]
    fn stop_clears_stale_resampler_history_like_reset_does() {
        let mut board = Mhi::new();
        let mut mem = memory(0x100);
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        let mut host = host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);

        // Feed a loud, non-silent native-rate "signal" directly into the
        // pre-resample queue and pump it through the mixer cadence so the
        // cached 44100 Hz resampler accumulates genuine convolution
        // history. (Bypassing decode here, rather than staging a decodable
        // "loud" MP3 frame, is deliberate -- see `mp3_frame`'s doc comment
        // on why arbitrary non-silent payloads are not decodable; this test
        // is about the resampler cache, not about decode.)
        board.native_rate = 44_100;
        for _ in 0..512 {
            board.decoded.push_back((0.9, -0.9));
        }
        for _ in 0..2000 {
            ZorroDevice::tick(&mut board, 64, &mut host);
        }
        assert!(
            board.resamplers.contains_key(&44_100),
            "setup should have primed a cached resampler with history"
        );
        host.mhi_audio().clear(); // drain the pre-stop signal so it can't be mistaken for residue below

        board.cmd_stop();
        assert!(
            board.resamplers.is_empty(),
            "STOP must clear cached resamplers exactly like reset() does"
        );

        // Silence-onset material at the same native rate: with a fresh
        // resampler this must be exact digital silence from the first
        // sample, not a decaying tail of the pre-stop signal.
        board.native_rate = 44_100;
        for _ in 0..512 {
            board.decoded.push_back((0.0, 0.0));
        }
        ZorroDevice::tick(&mut board, 64, &mut host);
        let (l, r) = host.mhi_audio().next_sample();
        assert_eq!(
            (l, r),
            (0.0, 0.0),
            "post-STOP silence must not carry residue from the pre-stop signal"
        );
    }

    /// Regression test for the "zero-length doorbell status blip" bug: a
    /// zero-length descriptor enqueues nothing (`QUEUE_COUNT` never moves),
    /// so it must not resurrect `STATUS` from `OUT_OF_DATA` to `PLAYING`,
    /// and must not cause a duplicate `OUT_OF_DATA` on the next tick.
    #[test]
    fn zero_length_doorbell_completes_immediately_without_a_status_blip() {
        let mut board = Mhi::new();
        let mut mem = memory(0x400);
        let mut host = DeviceHost::new(&mut mem);

        board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 3); // OUT_OF_DATA, empty queue
        board.write(OFF_INTREQ, 2, u32::from(INT_OUT_OF_DATA), &mut host); // ack

        // A zero-length descriptor: completes immediately (COMPLETED_COUNT
        // advances, BUFFER_DONE fires) but enqueues nothing.
        stage_and_ring(&mut board, &mut host, 0, &[]);
        assert_eq!(board.read(OFF_COMPLETED_COUNT, 2, &mut host), 1);
        assert_ne!(
            board.read(OFF_INTREQ, 2, &mut host) & u32::from(INT_BUFFER_DONE),
            0
        );
        assert_eq!(board.read(OFF_QUEUE_COUNT, 2, &mut host), 0);
        assert_eq!(
            board.read(OFF_STATUS, 2, &mut host),
            3,
            "a zero-length descriptor enqueues nothing, so STATUS must stay OUT_OF_DATA"
        );

        // The next tick must not re-raise OUT_OF_DATA: STATUS was already
        // OUT_OF_DATA and never left it, so this would be a duplicate.
        board.write(OFF_INTREQ, 2, u32::from(INT_BUFFER_DONE), &mut host); // ack
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        let mut host2 =
            host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
        ZorroDevice::tick(&mut board, 64, &mut host2);
        assert_eq!(
            board.read(OFF_INTREQ, 2, &mut host2) & u32::from(INT_OUT_OF_DATA),
            0,
            "no new OUT_OF_DATA transition should fire for a status that never left OUT_OF_DATA"
        );

        // A real descriptor still auto-resumes exactly as before.
        let payload = mp3_frame(128, 0x22);
        mem.chip_ram[0..payload.len()].copy_from_slice(&payload);
        let mut host3 =
            host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
        stage_and_ring(&mut board, &mut host3, 0, &payload);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host3), 1); // PLAYING again
    }

    /// Regression test for the "unbounded resync wedge" bug: a descriptor
    /// made entirely of fake-sync-header garbage (valid MP3 headers whose
    /// bodies fail Layer III decode) must not let `decode_next_frame` spin
    /// through the whole descriptor in a single tick -- the resync budget
    /// bounds each tick's work -- yet the descriptor must still complete
    /// (and STATUS must still reach OUT_OF_DATA) within a bounded number of
    /// further ticks. A valid frame staged right after a corrupted region
    /// must still resync and decode correctly.
    #[test]
    fn garbage_descriptor_drains_over_bounded_ticks_and_resync_still_finds_a_real_frame() {
        let mut board = Mhi::new();
        let mut mem = memory(0x20000);
        // Comfortably more than MAX_RESYNC_ATTEMPTS_PER_TICK frames' worth
        // of undecodable bytes, so a single tick's bounded resync budget
        // cannot possibly walk through all of it in one `decode_next_frame`
        // call.
        let garbage = garbage_stream(64 * 1024, 0x55);
        mem.chip_ram[0..garbage.len()].copy_from_slice(&garbage);
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        {
            let mut host =
                host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
            stage_and_ring(&mut board, &mut host, 0, &garbage);
            board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
            assert_eq!(board.read(OFF_STATUS, 2, &mut host), 1); // PLAYING

            ZorroDevice::tick(&mut board, 10, &mut host);
            assert_eq!(
                board.read(OFF_STATUS, 2, &mut host),
                1,
                "a single tick's bounded resync budget must not resolve the whole \
                 garbage descriptor in one call"
            );
            assert!(
                board.queue_count() > 0,
                "the garbage descriptor must not fully drain in a single tick"
            );

            // Enough further ticks for the bounded resync to work all the
            // way through the garbage descriptor.
            let mut reached_out_of_data = false;
            for _ in 0..5000 {
                ZorroDevice::tick(&mut board, 10, &mut host);
                if board.read(OFF_STATUS, 2, &mut host) == 3 {
                    reached_out_of_data = true;
                    break;
                }
            }
            assert!(
                reached_out_of_data,
                "an all-garbage descriptor must still complete and reach OUT_OF_DATA \
                 within a bounded number of ticks"
            );
            assert_eq!(board.queue_count(), 0);
            assert_eq!(board.completed_count, 1);
            assert_ne!(board.intreq & INT_BUFFER_DONE, 0);
            assert_ne!(board.intreq & INT_OUT_OF_DATA, 0);
        }

        // A valid frame staged right after a corrupted region must still be
        // found by resync and decoded (this must not regress: corrupt-
        // frame resync is a documented, still-supported case).
        let mut mixed = garbage_stream(2048, 0xAA);
        mixed.extend_from_slice(&mp3_frame(128, 0x99));
        mem.chip_ram[0..mixed.len()].copy_from_slice(&mixed);
        let mut host = host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
        stage_and_ring(&mut board, &mut host, 0, &mixed);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 1); // auto-resumed to PLAYING

        // Resync itself (finding the real frame past the garbage) takes at
        // most a handful of ticks; playing that frame's 1152 samples out at
        // 44100 Hz (so its descriptor genuinely completes) takes far more
        // emulated time, so give this loop the same generous cck-per-tick
        // budget `playing_a_known_frame_paces_completion_over_the_right_
        // number_of_ccks` uses.
        let mut completed_second = false;
        for _ in 0..2000 {
            ZorroDevice::tick(&mut board, 64, &mut host);
            if board.completed_count == 2 {
                completed_second = true;
                break;
            }
        }
        assert!(
            completed_second,
            "a valid frame following a corrupted region must still decode and complete"
        );
    }

    /// Regression test for the "PAUSE keeps draining audio" bug: `PAUSE`
    /// must halt audio output immediately, not merely halt decode while
    /// still draining whatever was already sitting in the pre-resample
    /// `decoded` backlog out to the mixer ring.
    #[test]
    fn pause_leaves_the_decoded_backlog_untouched_and_emits_silence() {
        let mut board = Mhi::new();
        let mut mem = memory(0x400);
        let payload = mp3_frame(128, 0x44);
        mem.chip_ram[0..payload.len()].copy_from_slice(&payload);
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        let backlog = {
            let mut host =
                host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
            stage_and_ring(&mut board, &mut host, 0, &payload);
            board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);

            // Drive the native producer directly, bypassing the mixer, to
            // build up a guaranteed nonzero pre-resample backlog in
            // `decoded` -- stopping well short of the full frame so the
            // descriptor is not yet exhausted and STATUS stays PLAYING
            // (not OUT_OF_DATA, which this test is not about).
            let frame_cck = (1152u64 * u64::from(PAULA_CLOCK_HZ)) / 44_100;
            board.advance_native((frame_cck / 2) as u32);
            assert_eq!(board.read(OFF_STATUS, 2, &mut host), 1); // still PLAYING
            let backlog = board.decoded.len();
            assert!(backlog > 0, "test setup needs a nonzero decoded backlog");

            board.write(OFF_CONTROL, 2, CMD_PAUSE as u32, &mut host);
            for _ in 0..200 {
                ZorroDevice::tick(&mut board, 64, &mut host);
            }
            backlog
        };
        assert_eq!(
            board.decoded.len(),
            backlog,
            "PAUSE must not drain the decoded backlog"
        );
        // Every sample the mixer pushed (or the ring's own empty-queue
        // default) while paused must be silence.
        for _ in 0..300 {
            assert_eq!(
                mhi_audio.next_sample(),
                (0.0, 0.0),
                "PAUSE must halt audio output immediately"
            );
        }
    }

    /// Regression test for the "DESC_LEN silently truncates" bug: a
    /// descriptor longer than the DMA chunk cap (`MAX_DESCRIPTOR_BYTES`)
    /// must still land every byte in `bitstream`, not just the first
    /// chunk's worth -- the register contract says `DESC_LEN` "does not
    /// truncate".
    #[test]
    fn doorbell_copies_a_descriptor_larger_than_the_dma_chunk_cap_without_truncating() {
        let mut board = Mhi::new();
        let total_len = MAX_DESCRIPTOR_BYTES as usize + 4096;
        let mut payload = vec![0u8; total_len];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut mem = memory(total_len);
        mem.chip_ram.copy_from_slice(&payload);
        let mut host = DeviceHost::new(&mut mem);
        stage_and_ring(&mut board, &mut host, 0, &payload);
        assert_eq!(
            board.bitstream.len(),
            total_len,
            "a descriptor past MAX_DESCRIPTOR_BYTES must not be truncated"
        );
        assert_eq!(board.bitstream.iter().copied().collect::<Vec<_>>(), payload);
        assert_eq!(board.desc_lengths.front().copied(), Some(total_len as u32));
    }

    /// Regression test for the "stalled trailing frame never reclaimed"
    /// bug: a final queued descriptor whose tail is a truncated MP3 frame
    /// (fewer bytes than minimp3 needs to make any decode progress, so
    /// `decode_frame` reports `consumed == 0` on every attempt) must not
    /// wedge `STATUS == PLAYING`/`QUEUE_COUNT == 1` forever when no further
    /// doorbell ever arrives to complete it.
    #[test]
    fn a_frame_split_at_the_last_queued_buffer_eventually_reclaims_the_descriptor() {
        let mut board = Mhi::new();
        let mut mem = memory(0x200);
        let full = mp3_frame(128, 0x55);
        let truncated = &full[..full.len() / 2];
        mem.chip_ram[0..truncated.len()].copy_from_slice(truncated);
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        let mut host = host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
        stage_and_ring(&mut board, &mut host, 0, truncated);
        board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
        assert_eq!(board.read(OFF_STATUS, 2, &mut host), 1); // PLAYING

        // Comfortably more emulated time than MAX_STALL_TICKS, with no
        // further doorbell ever completing the truncated frame.
        let ticks = (MAX_STALL_TICKS / 64) as usize + 100;
        let mut reached_out_of_data = false;
        for _ in 0..ticks {
            ZorroDevice::tick(&mut board, 64, &mut host);
            if board.read(OFF_STATUS, 2, &mut host) == 3 {
                reached_out_of_data = true;
                break;
            }
        }
        assert!(
            reached_out_of_data,
            "an unconsumable trailing frame must eventually be reclaimed rather than \
             wedging STATUS/QUEUE_COUNT forever"
        );
        assert_eq!(board.read(OFF_QUEUE_COUNT, 2, &mut host), 0);
        assert_eq!(board.completed_count, 1);
        assert_ne!(board.intreq & INT_OUT_OF_DATA, 0);
    }

    #[test]
    fn savestate_round_trip_reproduces_an_uninterrupted_runs_output() {
        let mut mem = memory(0x1000);
        let f1 = mp3_frame(128, 0x10);
        let f2 = mp3_frame(128, 0x40);
        let f3 = mp3_frame(128, 0x70);
        let mut all = f1.clone();
        all.extend_from_slice(&f2);
        all.extend_from_slice(&f3);
        mem.chip_ram[0..all.len()].copy_from_slice(&all);

        let mut board = Mhi::new();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        {
            let mut host =
                host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
            // Stage each encoded frame as its own descriptor (not one
            // descriptor spanning all three) -- a single combined
            // descriptor can only ever complete once no matter how much
            // playback time elapses, which would cap `completed_count` at
            // 1 forever and make the "multiple descriptors completed in
            // lockstep" claim below unprovable.
            let mut addr = 0u32;
            for frame in [&f1, &f2, &f3] {
                stage_and_ring(&mut board, &mut host, addr, frame);
                addr += frame.len() as u32;
            }
            board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
            // Partway through the first frame -- codec_acc and mixer_acc
            // both left with genuine mid-accumulation remainders.
            ZorroDevice::tick(&mut board, 137, &mut host);
        }

        let bytes = bincode::serialize(&board).unwrap();
        let mut resumed: Mhi = bincode::deserialize(&bytes).unwrap();

        let mut mem_a = memory(0x1000);
        let mut cd_a = crate::chipset::paula::CdAudioRing::default();
        let mut toc_a = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_a = MhiAudioRing::default();
        let mut host_a = host_with_rings(&mut mem_a, &mut cd_a, &mut toc_a, &mut mhi_a);
        let mut mem_b = memory(0x1000);
        let mut cd_b = crate::chipset::paula::CdAudioRing::default();
        let mut toc_b = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_b = MhiAudioRing::default();
        let mut host_b = host_with_rings(&mut mem_b, &mut cd_b, &mut toc_b, &mut mhi_b);

        // Enough emulated time for all three ~1152-sample/44100 Hz frames
        // (~92,654 ccks each) to finish playing out and complete their
        // descriptors, well within `MhiAudioRing`'s 4096-frame cap.
        for _ in 0..5000 {
            ZorroDevice::tick(&mut board, 61, &mut host_a);
            ZorroDevice::tick(&mut resumed, 61, &mut host_b);
        }

        let frames_a: Vec<_> = (0..2500).map(|_| mhi_a.next_sample()).collect();
        let frames_b: Vec<_> = (0..2500).map(|_| mhi_b.next_sample()).collect();
        assert_eq!(
            frames_a, frames_b,
            "a state resumed mid-stream must reproduce an uninterrupted run's output exactly"
        );
        // `mp3_frame`'s frames are valid-but-silent (see its doc comment),
        // so the claim this test proves is not "the audio matches" (it is
        // trivially all zero either way) but that decode/playback genuinely
        // progressed through multiple real frame boundaries -- multiple
        // descriptors completed, in lockstep, on both sides -- which is
        // exactly the timing/queue/bitstream state a broken savestate
        // (dropped `current_frame` position, a re-decoded-from-scratch
        // `desc_lengths`/`bitstream`, ...) would desynchronize.
        assert!(
            board.completed_count >= 2,
            "the setup should have made real decode progress"
        );
        assert_eq!(
            board.completed_count, resumed.completed_count,
            "descriptor completion must also stay in lockstep"
        );
    }

    // ---- M4: the param-latch DSP chain (MHI-PLAN-M3-M4.md WP4.5) ----

    #[test]
    fn dsp_helper_functions_produce_exact_expected_gains() {
        assert_eq!(volume_gain(100), 1.0);
        assert_eq!(volume_gain(0), 0.0);
        assert_eq!(volume_gain(50), 0.5);

        assert_eq!(prefactor_gain(50), 1.0);
        assert_eq!(prefactor_gain(0), 0.0);
        assert_eq!(prefactor_gain(100), 2.0);

        assert_eq!(pan_gains(50), (1.0, 1.0));
        assert_eq!(pan_gains(0), (1.0, 0.0));
        assert_eq!(pan_gains(100), (0.0, 1.0));
        assert_eq!(pan_gains(75), (0.5, 1.0));
        assert_eq!(pan_gains(25), (1.0, 0.5));

        assert_eq!(crossmix_fraction(0), 0.0);
        assert_eq!(crossmix_fraction(100), 1.0);

        assert_eq!(apply_crossmix(1.0, -1.0, 0.0), (1.0, -1.0));
        assert_eq!(apply_crossmix(1.0, -1.0, 1.0), (0.0, 0.0));
        assert_eq!(apply_crossmix(2.0, 0.0, 0.5), (1.5, 0.5));

        assert_eq!(band_gain_db(50), 0.0);
        assert_eq!(band_gain_db(0), -12.0);
        assert_eq!(band_gain_db(100), 12.0);
    }

    /// Precomputed reference coefficients (independent Python
    /// implementation of the same RBJ Cookbook formulas
    /// `docs/internals/mhi.md`'s "Tone filters: bass/mid/treble" section
    /// documents) for one peaking and one low-shelf case -- proves this
    /// module's `Biquad` constructors match the documented math, not just
    /// that they're internally self-consistent. Tolerance accounts for f64
    /// (Python) vs. `f32` (this module) precision, not an algorithm
    /// difference.
    #[test]
    fn biquad_coefficients_match_independently_precomputed_reference_values() {
        let peaking = Biquad::peaking(1000.0, 1.0, 12.0, 44100.0);
        let expected = [1.1024303, -1.9117108, 0.8288492, -1.9117108, 0.9312795];
        let got = [peaking.b0, peaking.b1, peaking.b2, peaking.a1, peaking.a2];
        for (g, e) in got.iter().zip(expected.iter()) {
            assert!((g - e).abs() < 1e-4, "got {got:?}, expected {expected:?}");
        }

        let low_shelf = Biquad::low_shelf(200.0, 1.0, -12.0, 44100.0);
        let expected = [0.9859057, -1.9436854, 0.9581753, -1.9430958, 0.9446706];
        let got = [
            low_shelf.b0,
            low_shelf.b1,
            low_shelf.b2,
            low_shelf.a1,
            low_shelf.a2,
        ];
        for (g, e) in got.iter().zip(expected.iter()) {
            assert!((g - e).abs() < 1e-4, "got {got:?}, expected {expected:?}");
        }
    }

    /// `Biquad::peaking`/`low_shelf`/`high_shelf` at `gain_db = 0.0` (the
    /// default param value's mapping, `band_gain_db(50) == 0.0`) must be
    /// mathematically exact identity filters (RBJ Cookbook property: at
    /// `A = 1`, `b1 == a1` and `b2 == a2` bit-for-bit, since both are
    /// computed from the identical expression -- see this module's
    /// `Biquad` doc comment) -- process a sequence of varied, non-trivial
    /// samples and every output must equal its input exactly.
    #[test]
    fn biquad_at_zero_db_is_an_exact_identity_filter() {
        for ctor in [
            Biquad::peaking(1000.0, 1.0, 0.0, 44100.0),
            Biquad::low_shelf(200.0, 1.0, 0.0, 44100.0),
            Biquad::high_shelf(4000.0, 1.0, 0.0, 44100.0),
        ] {
            let mut filter = ctor;
            for input in [0.0f32, 1.0, -1.0, 0.37, -0.82, 0.001, -0.999] {
                assert_eq!(filter.process(input), input);
            }
        }
    }

    /// The M4 DSP chain at every param's documented default (`PARAM_
    /// DEFAULTS`) must be an exact no-op: `produce_one_sample` fed a
    /// known, non-trivial stereo sample through a freshly constructed
    /// board (defaults) must push that exact sample into `decoded`
    /// unchanged. This is the property the panning/crossmix/tone-filter
    /// formulas were each deliberately chosen around (see their doc
    /// comments), proven here at the whole-chain level rather than one
    /// param at a time.
    #[test]
    fn default_params_are_an_exact_no_op_on_produced_samples() {
        let mut board = Mhi::new();
        board.status = Status::Playing;
        board.current_frame = Some(CurrentFrame {
            samples: vec![(0.6, -0.3)],
            idx: 0,
            rate: 44_100,
            completes: 0,
        });
        board.produce_one_sample();
        assert_eq!(board.decoded.pop_back(), Some((0.6, -0.3)));
    }

    #[test]
    fn volume_latch_scales_both_channels_exactly() {
        let mut board = Mhi::new();
        board.status = Status::Playing;
        board.params[0] = 50; // volume: gain 0.5
        board.current_frame = Some(CurrentFrame {
            samples: vec![(0.8, -0.4)],
            idx: 0,
            rate: 44_100,
            completes: 0,
        });
        board.produce_one_sample();
        let (l, r) = board.decoded.pop_back().unwrap();
        assert!((l - 0.4).abs() < 1e-6);
        assert!((r - -0.2).abs() < 1e-6);
    }

    #[test]
    fn prefactor_latch_scales_both_channels_exactly() {
        let mut board = Mhi::new();
        board.status = Status::Playing;
        board.params[6] = 100; // prefactor: gain 2.0
        board.current_frame = Some(CurrentFrame {
            samples: vec![(0.25, -0.1)],
            idx: 0,
            rate: 44_100,
            completes: 0,
        });
        board.produce_one_sample();
        let (l, r) = board.decoded.pop_back().unwrap();
        assert!((l - 0.5).abs() < 1e-6);
        assert!((r - -0.2).abs() < 1e-6);
    }

    #[test]
    fn hard_pan_left_and_right_silence_the_opposite_channel_exactly() {
        for (pan_value, expect_left, expect_right) in [(0u16, 1.0f32, 0.0f32), (100, 0.0, 1.0)] {
            let mut board = Mhi::new();
            board.status = Status::Playing;
            board.params[1] = pan_value;
            board.current_frame = Some(CurrentFrame {
                samples: vec![(1.0, 1.0)],
                idx: 0,
                rate: 44_100,
                completes: 0,
            });
            board.produce_one_sample();
            let (l, r) = board.decoded.pop_back().unwrap();
            assert_eq!(l, expect_left);
            assert_eq!(r, expect_right);
        }
    }

    #[test]
    fn full_crossmix_makes_both_channels_identical_exactly() {
        let mut board = Mhi::new();
        board.status = Status::Playing;
        board.params[5] = 100; // crossmix: full mono
        board.current_frame = Some(CurrentFrame {
            samples: vec![(1.0, -0.5)],
            idx: 0,
            rate: 44_100,
            completes: 0,
        });
        board.produce_one_sample();
        let (l, r) = board.decoded.pop_back().unwrap();
        assert_eq!(l, r);
        assert!((l - 0.25).abs() < 1e-6); // (1.0 + -0.5) / 2
    }

    /// A sustained bass boost's DC gain must be measurably above unity (and
    /// a sustained cut measurably below), proving the filter bank's
    /// `retune_if_stale` wiring actually reaches decoded audio -- not just
    /// that the standalone `Biquad` constructors compute plausible
    /// coefficients (the two tests above already cover that in isolation).
    /// A single sample can't show this (a biquad's state starts at zero),
    /// so this drives many identical samples to let the IIR reach its DC
    /// steady state.
    #[test]
    fn sustained_bass_boost_and_cut_measurably_change_dc_gain() {
        fn steady_state_left_gain(bass_value: u16) -> f32 {
            let mut board = Mhi::new();
            board.status = Status::Playing;
            board.params[2] = bass_value;
            let mut last = 0.0;
            for _ in 0..2000 {
                board.current_frame = Some(CurrentFrame {
                    samples: vec![(1.0, 1.0)],
                    idx: 0,
                    rate: 44_100,
                    completes: 0,
                });
                board.produce_one_sample();
                last = board.decoded.pop_back().unwrap().0;
            }
            last
        }
        let boost = steady_state_left_gain(100);
        let cut = steady_state_left_gain(0);
        assert!(
            boost > 1.05,
            "expected a measurable bass boost, got {boost}"
        );
        assert!(cut < 0.95, "expected a measurable bass cut, got {cut}");
    }

    /// Extends `savestate_round_trip_reproduces_an_uninterrupted_runs_
    /// output`'s claim to M4: mid-playback param state (non-default latch
    /// values) and hot filter memory (built up by a boosted band, not the
    /// all-zero state a freshly constructed `ToneFilterBank` starts with)
    /// must both round-trip through a savestate and keep producing
    /// identical output afterward.
    #[test]
    fn savestate_round_trip_preserves_hot_param_and_filter_state() {
        let mut mem = memory(0x1000);
        let f1 = mp3_frame(128, 0x10);
        let f2 = mp3_frame(128, 0x40);
        mem.chip_ram[0..f1.len()].copy_from_slice(&f1);
        mem.chip_ram[f1.len()..f1.len() + f2.len()].copy_from_slice(&f2);

        let mut board = Mhi::new();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = MhiAudioRing::default();
        {
            let mut host =
                host_with_rings(&mut mem, &mut cd_audio, &mut toccata_audio, &mut mhi_audio);
            stage_and_ring(&mut board, &mut host, 0, &f1);
            stage_and_ring(&mut board, &mut host, f1.len() as u32, &f2);
            board.write(OFF_PARAM_SELECT, 2, 2, &mut host); // select bass
            board.write(OFF_PARAM_VALUE, 2, 100, &mut host); // full boost
            board.write(OFF_PARAM_SELECT, 2, 0, &mut host); // select volume
            board.write(OFF_PARAM_VALUE, 2, 70, &mut host);
            board.write(OFF_CONTROL, 2, CMD_PLAY as u32, &mut host);
            // Partway into the first frame -- the bass filter's z1/z2
            // memory is genuinely hot by now (mp3_frame's silent payload
            // still runs every sample through the boosted bass biquad,
            // building up real filter state even though the audible
            // content is zero).
            ZorroDevice::tick(&mut board, 137, &mut host);
        }

        let bytes = bincode::serialize(&board).unwrap();
        let mut resumed: Mhi = bincode::deserialize(&bytes).unwrap();
        assert_eq!(resumed.params[2], 100, "bass latch must round-trip");
        assert_eq!(resumed.params[0], 70, "volume latch must round-trip");

        let mut mem_a = memory(0x1000);
        let mut cd_a = crate::chipset::paula::CdAudioRing::default();
        let mut toc_a = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_a = MhiAudioRing::default();
        let mut host_a = host_with_rings(&mut mem_a, &mut cd_a, &mut toc_a, &mut mhi_a);
        let mut mem_b = memory(0x1000);
        let mut cd_b = crate::chipset::paula::CdAudioRing::default();
        let mut toc_b = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_b = MhiAudioRing::default();
        let mut host_b = host_with_rings(&mut mem_b, &mut cd_b, &mut toc_b, &mut mhi_b);

        for _ in 0..3000 {
            ZorroDevice::tick(&mut board, 61, &mut host_a);
            ZorroDevice::tick(&mut resumed, 61, &mut host_b);
        }
        let frames_a: Vec<_> = (0..1500).map(|_| mhi_a.next_sample()).collect();
        let frames_b: Vec<_> = (0..1500).map(|_| mhi_b.next_sample()).collect();
        assert_eq!(
            frames_a, frames_b,
            "hot param/filter state resumed mid-stream must reproduce an uninterrupted \
             run's output exactly"
        );
    }
}
