// SPDX-License-Identifier: GPL-3.0-or-later

//! MacroSystem Toccata: a Zorro II AD1848 sound board with a stock,
//! open-source AHI driver (`toccata.audio`), so AHI-aware Amiga software
//! gets 16-bit sound with no board-specific driver work of Copperline's
//! own. See `docs/internals/toccata.md`.
//!
//! `ad1848` is the register-accurate codec/FIFO core, modelled against
//! WinUAE/amiberry's `sndboard.cpp` as a behavioural oracle. This module is
//! the board wrapper: autoconfig (`BoardSpec::toccata`, `src/zorro.rs`),
//! address decode within the board's 64 KB Zorro II I/O window, the
//! `ZorroDevice` glue, and the mixer cadence -- resampling the codec's own
//! programmed rate to the mixer's fixed rate and pushing frames into
//! Paula's `ToccataAudioRing`. See `docs/internals/audio.md` for how that
//! ring fits into the wider mixer/stem-capture picture.

pub mod ad1848;

use crate::audio::resample::Resampler;
use crate::audio::MIX_SAMPLE_RATE;
use crate::chipset::paula::PAULA_CLOCK_HZ;
use crate::zorro_device::{DeviceHost, ZorroDevice};
use ad1848::Ad1848;
use std::collections::{HashMap, VecDeque};

/// A generous safety margin on the pre-resample sample queue between the
/// codec's own cadence and the mixer's -- the two stay near-lockstep in
/// steady state (see `advance_codec`'s doc comment), so this is a cap
/// against a stalled consumer, not a buffering requirement.
const DECODED_CAPACITY: usize = 4096;

/// One of the four port families the board's 64 KB window decodes to, by
/// address bit pattern (heavily mirrored -- see `decode_port`).
enum Port {
    /// base+0x0000: board control/status.
    Status,
    /// base+0x2000: the playback (and, later, record) FIFO data port.
    Fifo,
    /// base+0x6001 (odd bytes only): AD1848 register index.
    Ad1848Index,
    /// base+0x6801 (odd bytes only): AD1848 register data.
    Ad1848Data,
}

/// Decode a board-relative offset into one of the four port families, by
/// the same address-line pattern the reference decodes (A14/A13/A11/A0;
/// A15, A12, and A10..A1 are don't-cares, so each port mirrors across
/// several KB of the window). `None` is open bus within the board's own
/// window -- reads as 0, writes are dropped -- distinct from the chain's
/// open bus (0xFF) outside any configured board.
fn decode_port(off: u32) -> Option<Port> {
    let off = off & 0xffff;
    let a14 = off & 0x4000 != 0;
    let a13 = off & 0x2000 != 0;
    let a11 = off & 0x0800 != 0;
    let a0 = off & 0x0001 != 0;
    if !a14 && !a13 && !a11 {
        Some(Port::Status)
    } else if !a14 && a13 && !a11 {
        Some(Port::Fifo)
    } else if a14 && a13 && !a11 && a0 {
        Some(Port::Ad1848Index)
    } else if a14 && a13 && a11 && a0 {
        Some(Port::Ad1848Data)
    } else {
        None
    }
}

/// The Toccata board: autoconfig glue and register-window decode around
/// the chip-only [`Ad1848`] core, plus two independent emulated-time
/// cadences -- see `advance_codec`/`advance_mixer`.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Toccata {
    ad1848: Ad1848,
    /// Codec-rate accumulator, in units of colour-clocks * (the codec's
    /// own active sample rate). One raw sample is due each time this
    /// reaches PAULA_CLOCK_HZ -- see `advance_codec`. Genuine machine
    /// state: it (indirectly, through when `produce_one_sample` gets
    /// called) determines exactly when the FIFO drains and interrupts
    /// raise, so it must survive a save-state load unchanged.
    codec_acc: u64,
    /// Raw, pre-resample samples `advance_codec` has produced and
    /// `advance_mixer` hasn't consumed yet. Also genuine machine state,
    /// for the same reason as `codec_acc` -- see `advance_codec`'s doc
    /// comment for why this exists at all rather than the mixer pulling
    /// straight from the chip.
    decoded: VecDeque<(f32, f32)>,
    /// Mixer-rate accumulator, in units of colour-clocks * MIX_SAMPLE_RATE
    /// -- the exact same accumulator shape `Paula::advance_audio` uses.
    /// One output frame is due each time this reaches PAULA_CLOCK_HZ.
    mixer_acc: u64,
    /// One resampler per codec rate this session has used (at most 14, the
    /// AD1848's legal rate count), so switching back to an already-seen
    /// rate never rebuilds its kernel table. After the codec/mixer cadence
    /// split above, a resampler's phase/history only shape the
    /// interpolated *waveform*, never FIFO/IRQ timing -- but they are
    /// still genuine machine state for output-exactness purposes, so this
    /// serializes (via `Resampler`'s own `Serialize`/`Deserialize`, which
    /// rebuilds the derived kernel table rather than storing it) instead
    /// of being `#[serde(skip)]`: a save-state load must reproduce an
    /// uninterrupted run's output exactly, not just its FIFO/IRQ timing.
    resamplers: HashMap<u32, Resampler>,
}

/// Snapshot of the board's codec state for the debugger's Audio tab: the
/// playback state, the format and rate latched at the last codec start, and
/// the board FIFO's fill level.
#[derive(Debug, Clone, Copy)]
pub struct ToccataDebug {
    pub playing: bool,
    pub rate_hz: u32,
    pub channels: usize,
    pub sixteen_bit: bool,
    pub fifo_len: usize,
    pub fifo_capacity: usize,
}

impl Toccata {
    pub fn new() -> Self {
        Self {
            ad1848: Ad1848::new(),
            codec_acc: 0,
            decoded: VecDeque::new(),
            mixer_acc: 0,
            resamplers: HashMap::new(),
        }
    }

    /// Codec-state snapshot for the debugger's Audio tab.
    pub fn debug_status(&self) -> ToccataDebug {
        let (channels, sixteen_bit) = self.ad1848.active_format();
        ToccataDebug {
            playing: self.ad1848.play_active(),
            rate_hz: self.ad1848.sample_rate_hz(),
            channels,
            sixteen_bit,
            fifo_len: self.ad1848.fifo_len(),
            fifo_capacity: self.ad1848.fifo_capacity(),
        }
    }

    /// Advance the codec's own native-rate cadence by `cck` colour
    /// clocks, producing raw (pre-resample) samples into `decoded`. This
    /// -- not the resampler below -- is what actually drains the FIFO and
    /// evaluates the half-empty/interrupt condition, paced causally by
    /// emulated time via its own exact-ratio accumulator (the same shape
    /// as `Paula::advance_audio`'s, just at the codec's rate instead of
    /// the mixer's).
    ///
    /// This exists as a separate step from `advance_mixer` specifically
    /// so `produce_one_sample`'s side effects are never invoked from
    /// inside the resampler's own pull: a windowed-sinc kernel is
    /// inherently non-causal (it needs taps on *both* sides of the output
    /// instant, and primes by pulling a full window's worth of input
    /// before its first output), so if it called `produce_one_sample`
    /// directly, its first use after startup or a rate change would drain
    /// dozens of FIFO bytes and raise interrupts all at once, decades
    /// ahead of when a real codec would reach them. Producing samples
    /// here first, in true chronological order, and letting the resampler
    /// only ever pull from the resulting passive queue (or repeat the
    /// chip's last known sample with no side effects, see
    /// `advance_mixer`) keeps that lookahead confined to the waveform,
    /// where it belongs.
    fn advance_codec(&mut self, cck: u32) {
        if !self.ad1848.play_active() {
            return;
        }
        let rate = self.ad1848.sample_rate_hz();
        self.codec_acc += u64::from(cck) * u64::from(rate);
        while self.codec_acc >= u64::from(PAULA_CLOCK_HZ) {
            self.codec_acc -= u64::from(PAULA_CLOCK_HZ);
            let sample = self.ad1848.produce_one_sample();
            if self.decoded.len() < DECODED_CAPACITY {
                self.decoded.push_back(sample);
            }
        }
    }

    /// Advance the mixer-rate cadence by `cck` colour clocks, resampling
    /// already-produced codec samples onto the mixer grid and pushing
    /// each produced frame into `ring`. Exact-ratio accumulator, so it
    /// never drifts against `Paula::advance_audio`'s own. Pulls only from
    /// `decoded` (or repeats the chip's last known sample via the
    /// side-effect-free `peek_last_sample`) -- never calls back into
    /// `ad1848.produce_one_sample` directly, so the resampler's own
    /// lookahead/priming can never advance FIFO/IRQ state out of order.
    fn advance_mixer(&mut self, cck: u32, ring: &mut crate::chipset::paula::ToccataAudioRing) {
        self.mixer_acc += u64::from(cck) * u64::from(MIX_SAMPLE_RATE);
        while self.mixer_acc >= u64::from(PAULA_CLOCK_HZ) {
            self.mixer_acc -= u64::from(PAULA_CLOCK_HZ);
            let rate = self.ad1848.sample_rate_hz();
            let fallback = self.ad1848.peek_last_sample();
            // Disjoint field borrows: the resampler cache and the passive
            // decoded-sample queue it pulls from via the refill closure.
            let Self {
                decoded,
                resamplers,
                ..
            } = self;
            let resampler = resamplers
                .entry(rate)
                .or_insert_with(|| Resampler::new(rate, MIX_SAMPLE_RATE));
            let (left, right) = resampler.next(|| decoded.pop_front().unwrap_or(fallback));
            ring.push_frame(left, right);
        }
    }

    fn read_byte(&mut self, off: u32) -> u8 {
        match decode_port(off) {
            Some(Port::Status) => self.ad1848.read_status(),
            Some(Port::Fifo) => self.ad1848.read_fifo_byte(),
            Some(Port::Ad1848Index) => self.ad1848.read_index(),
            Some(Port::Ad1848Data) => self.ad1848.read_data(),
            None => 0,
        }
    }

    fn write_byte(&mut self, off: u32, v: u8) {
        match decode_port(off) {
            Some(Port::Status) => self.ad1848.write_control(v),
            Some(Port::Fifo) => self.ad1848.write_fifo_byte(v),
            Some(Port::Ad1848Index) => self.ad1848.write_index(v),
            Some(Port::Ad1848Data) => self.ad1848.write_data(v),
            None => {}
        }
    }
}

impl Default for Toccata {
    fn default() -> Self {
        Self::new()
    }
}

impl ZorroDevice for Toccata {
    fn read(&mut self, off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        // The board decomposes any wider access into successive byte
        // pokes/peeks at the same address, big-endian, exactly as the
        // 68k bus presents them -- there is no native word/long port.
        let mut value = 0u32;
        for i in 0..size as u32 {
            value = (value << 8) | u32::from(self.read_byte(off + i));
        }
        value
    }

    fn write(&mut self, off: u32, size: usize, value: u32, _host: &mut DeviceHost) {
        for i in 0..size as u32 {
            let shift = 8 * (size as u32 - 1 - i);
            self.write_byte(off + i, (value >> shift) as u8);
        }
    }

    fn tick(&mut self, cck: u32, host: &mut DeviceHost) {
        self.ad1848.advance_cck(cck);
        self.advance_codec(cck);
        self.advance_mixer(cck, host.toccata_audio());
    }

    fn int6_line(&self) -> bool {
        self.ad1848.int6_pending()
    }

    fn reset(&mut self) {
        self.ad1848.reset();
        self.codec_acc = 0;
        self.decoded.clear();
        self.mixer_acc = 0;
        // Each cached resampler's own history buffer holds the last ~64
        // pre-reset input frames; leaving it in place would let a few
        // moments of pre-reset audio bleed into the freshly-reset silence
        // as the kernel window slides past the reset boundary. Clearing
        // the whole cache is simpler than adding a per-resampler reset
        // and costs nothing but a one-time kernel-table rebuild next use.
        self.resamplers.clear();
    }

    fn kind(&self) -> &'static str {
        "toccata"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;

    fn memory() -> Memory {
        Memory {
            chip_ram: vec![0; 0x100],
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

    #[test]
    fn status_port_reaches_the_control_register() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(0x0000, 1, 0x04, &mut host); // STATUS_FIFO_CODEC
                                                 // The status port is destructive-read (IRQ ack), but with no IRQ
                                                 // pending it just reports "no interrupt" (bit 7 set).
        assert_eq!(board.read(0x0000, 1, &mut host), 0x80);
    }

    #[test]
    fn status_port_mirrors_across_the_dont_care_address_bits() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        for mirror in [0x0000u32, 0x1000, 0x8000, 0x9000] {
            board.write(mirror, 1, 0x00, &mut host);
            assert_eq!(board.read(mirror, 1, &mut host), 0x80, "mirror {mirror:#x}");
        }
    }

    #[test]
    fn fifo_port_reaches_the_play_fifo() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(0x0000, 1, 0x14, &mut host); // ACTIVE | FIFO_PLAY
        board.write(0x2000, 1, 0x42, &mut host);
        assert_eq!(board.ad1848.fifo_len_for_test(), 1);
    }

    #[test]
    fn ad1848_index_and_data_ports_reach_the_register_file() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(0x6001, 1, 0x06, &mut host); // select DAC output attenuation L
        board.write(0x6801, 1, 0x00, &mut host); // full volume, unmuted
        assert_eq!(board.read(0x6001, 1, &mut host), 0x06);
        assert_eq!(board.read(0x6801, 1, &mut host), 0x00);
    }

    #[test]
    fn even_addresses_in_the_ad1848_region_are_open_bus() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(0x6000, 1, 0xff, &mut host); // even: neither index nor data
        assert_eq!(board.read(0x6000, 1, &mut host), 0);
    }

    #[test]
    fn unmapped_region_is_open_bus_within_the_window() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(0x4000, 1, 0xff, &mut host);
        assert_eq!(board.read(0x4000, 1, &mut host), 0);
    }

    #[test]
    fn a_16bit_write_decomposes_into_two_big_endian_fifo_pokes() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(0x0000, 1, 0x14, &mut host); // ACTIVE | FIFO_PLAY
        board.write(0x2000, 2, 0x1234, &mut host);
        assert_eq!(board.ad1848.fifo_len_for_test(), 2);
    }

    #[test]
    fn reset_clears_the_chip_and_kind_identifies_the_board() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(0x0000, 1, 0x04, &mut host);
        board.reset();
        assert_eq!(board.read(0x0000, 1, &mut host), 0x80);
        assert_eq!(board.kind(), "toccata");
    }

    #[test]
    fn savestate_round_trip_reproduces_an_uninterrupted_runs_output() {
        // The determinism-critical claim: a state saved and reloaded
        // mid-stream must produce exactly the frames an uninterrupted run
        // would have -- both the resampler (its phase/history now
        // genuine serialized state) and codec_acc/decoded (which pace
        // produce_one_sample()) must round-trip exactly.
        let mut mem = memory();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = crate::chipset::paula::MhiAudioRing::default();
        let mut host = DeviceHost::for_slot_with_audio(
            &mut mem,
            0,
            &mut cd_audio,
            &mut toccata_audio,
            &mut mhi_audio,
        );

        let mut board = Toccata::new();
        board.write(0x0000, 1, 0x14, &mut host); // ACTIVE | FIFO_PLAY
        board.write(0x6001, 1, 8, &mut host);
        board.write(0x6801, 1, 0x0b, &mut host); // 44100 Hz, mono 8-bit
        latch_format(&mut board, &mut host);
        board.write(0x6001, 1, 6, &mut host);
        board.write(0x6801, 1, 0x00, &mut host);
        board.write(0x6001, 1, 7, &mut host);
        board.write(0x6801, 1, 0x00, &mut host);
        for byte in [0x40, 0x80, 0xc0, 0xff, 0x20] {
            board.write(0x2000, 1, byte, &mut host);
        }
        // Advance partway -- not a whole number of mixer frames, so both
        // codec_acc and mixer_acc are left with genuine mid-accumulation
        // remainders, not conveniently zeroed.
        ZorroDevice::tick(&mut board, 137, &mut host);

        let bytes = bincode::serialize(&board).unwrap();
        let mut resumed: Toccata = bincode::deserialize(&bytes).unwrap();

        // Continue both from here with identical further input and
        // compare every produced frame.
        let mut mem_a = memory();
        let mut cd_a = crate::chipset::paula::CdAudioRing::default();
        let mut ring_a = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_a = crate::chipset::paula::MhiAudioRing::default();
        let mut host_a =
            DeviceHost::for_slot_with_audio(&mut mem_a, 0, &mut cd_a, &mut ring_a, &mut mhi_a);
        let mut mem_b = memory();
        let mut cd_b = crate::chipset::paula::CdAudioRing::default();
        let mut ring_b = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_b = crate::chipset::paula::MhiAudioRing::default();
        let mut host_b =
            DeviceHost::for_slot_with_audio(&mut mem_b, 0, &mut cd_b, &mut ring_b, &mut mhi_b);

        for byte in [0x30, 0x90, 0x10, 0xe0] {
            board.write(0x2000, 1, byte, &mut host_a);
            resumed.write(0x2000, 1, byte, &mut host_b);
        }
        for _ in 0..500 {
            ZorroDevice::tick(&mut board, 61, &mut host_a);
            ZorroDevice::tick(&mut resumed, 61, &mut host_b);
        }

        // 500 ticks of 61 cck each is ~379 mixer frames at 44.1 kHz;
        // draining well past that is safe since extra reads past the end
        // just yield the ring's own matching (0.0, 0.0) "empty" fallback
        // on both sides.
        let frames_a: Vec<_> = (0..700).map(|_| ring_a.next_sample()).collect();
        let frames_b: Vec<_> = (0..700).map(|_| ring_b.next_sample()).collect();
        assert!(
            frames_a.iter().any(|&f| f != (0.0, 0.0)),
            "the setup should have produced audio"
        );
        assert_eq!(
            frames_a, frames_b,
            "a state resumed mid-stream must reproduce an uninterrupted run's output exactly"
        );
    }

    /// Colour clocks needed to cross one mixer-rate (44.1 kHz) frame
    /// boundary from a zero accumulator: `ceil(PAULA_CLOCK_HZ / MIX_SAMPLE_RATE)`.
    fn one_mixer_frame_cck() -> u32 {
        PAULA_CLOCK_HZ.div_ceil(MIX_SAMPLE_RATE)
    }

    /// Latch whatever format/rate is currently programmed in reg 8 by
    /// driving reg 9 through a clean stopped-to-started transition, the
    /// way a real driver does: program the format, then enable playback.
    /// `advance_codec` only runs while the codec is active, and it only
    /// ever uses the format/rate latched at the last such transition --
    /// see `Ad1848::codec_start`'s doc comment.
    fn latch_format(board: &mut Toccata, host: &mut DeviceHost) {
        board.write(0x6001, 1, 9, host);
        board.write(0x6801, 1, 0x10, host); // stop (SDC bit only)
        board.write(0x6801, 1, 0x11, host); // play enable: rising edge
    }

    #[test]
    fn tick_at_the_mixer_native_rate_reaches_the_ring_as_a_near_pass_through() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = crate::chipset::paula::MhiAudioRing::default();
        let mut host = DeviceHost::for_slot_with_audio(
            &mut mem,
            0,
            &mut cd_audio,
            &mut toccata_audio,
            &mut mhi_audio,
        );

        board.write(0x0000, 1, 0x14, &mut host); // ACTIVE | FIFO_PLAY
        board.write(0x6001, 1, 8, &mut host);
        board.write(0x6801, 1, 0x0b, &mut host); // divider idx 5, crystal1 -> 44100 Hz, mono 8-bit
        latch_format(&mut board, &mut host);
        board.write(0x6001, 1, 6, &mut host);
        board.write(0x6801, 1, 0x00, &mut host); // DAC L unmuted, full scale
        board.write(0x6001, 1, 7, &mut host);
        board.write(0x6801, 1, 0x00, &mut host); // DAC R unmuted, full scale
        board.write(0x2000, 1, 0xff, &mut host); // one 8-bit sample: (255-128)/128

        ZorroDevice::tick(&mut board, one_mixer_frame_cck(), &mut host);

        let (l, r) = toccata_audio.next_sample();
        // 44100 Hz codec into a 44100 Hz mixer is an identity resample
        // (l=m=1 in the polyphase ratio), so the frame should be very
        // close to the raw decoded sample, not zero and not wildly off.
        assert!(l > 0.9, "left sample too quiet: {l}");
        assert!(r > 0.9, "right sample too quiet: {r}");
    }

    #[test]
    fn advance_codec_does_not_drain_the_fifo_before_its_own_cadence_elapses() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = crate::chipset::paula::MhiAudioRing::default();
        let mut host = DeviceHost::for_slot_with_audio(
            &mut mem,
            0,
            &mut cd_audio,
            &mut toccata_audio,
            &mut mhi_audio,
        );

        board.write(0x0000, 1, 0x14, &mut host); // ACTIVE | FIFO_PLAY
        board.write(0x6001, 1, 8, &mut host);
        board.write(0x6801, 1, 0x0f, &mut host); // divider idx 7, crystal1 -> 6600 Hz, mono 8-bit
        latch_format(&mut board, &mut host);
        for _ in 0..10 {
            board.write(0x2000, 1, 0x80, &mut host);
        }
        assert_eq!(board.ad1848.fifo_len_for_test(), 10);

        // One colour clock is nowhere near a 6.6 kHz sample period (about
        // 537 cck), so the FIFO must be completely untouched -- not
        // drained by a resampler's ~64-tap priming burst pulling
        // produce_one_sample() directly and far ahead of emulated time.
        // (Regression test for the earlier design, where the resampler's
        // refill closure called produce_one_sample() itself.)
        ZorroDevice::tick(&mut board, 1, &mut host);
        assert_eq!(
            board.ad1848.fifo_len_for_test(),
            10,
            "the FIFO must only drain as the codec's own cadence elapses, never all at once"
        );
    }

    #[test]
    fn reprogramming_reg8_while_active_does_not_change_the_active_frame_width() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = crate::chipset::paula::MhiAudioRing::default();
        let mut host = DeviceHost::for_slot_with_audio(
            &mut mem,
            0,
            &mut cd_audio,
            &mut toccata_audio,
            &mut mhi_audio,
        );

        board.write(0x0000, 1, 0x14, &mut host); // ACTIVE | FIFO_PLAY
        board.write(0x6001, 1, 8, &mut host);
        board.write(0x6801, 1, 0x0b, &mut host); // 44100 Hz, mono 8-bit
        latch_format(&mut board, &mut host);

        // Reprogram to stereo 16-bit (4 bytes/sample) *without* a
        // stop/start transition -- the reference only re-runs its own
        // codec_setup() on that transition, so this must have no effect
        // on the still-running stream.
        board.write(0x6001, 1, 8, &mut host);
        board.write(0x6801, 1, 0x5b, &mut host); // stereo + 16-bit, same rate bits

        // One byte matches the still-active mono-8-bit frame width (1
        // byte), not the newly-written-but-unlatched stereo/16-bit width
        // (4 bytes) -- so it must be consumed as a complete frame.
        board.write(0x2000, 1, 0xff, &mut host);
        ZorroDevice::tick(&mut board, one_mixer_frame_cck(), &mut host);
        assert_eq!(
            board.ad1848.fifo_len_for_test(),
            0,
            "the still-active mono 8-bit format should consume the single byte, \
             not wait for a 4-byte stereo/16-bit frame the unlatched reg8 write implies"
        );
    }

    #[test]
    fn reset_clears_stale_resampler_history_so_silence_follows_immediately() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = crate::chipset::paula::MhiAudioRing::default();
        let mut host = DeviceHost::for_slot_with_audio(
            &mut mem,
            0,
            &mut cd_audio,
            &mut toccata_audio,
            &mut mhi_audio,
        );

        board.write(0x0000, 1, 0x14, &mut host); // ACTIVE | FIFO_PLAY
        board.write(0x6001, 1, 8, &mut host);
        board.write(0x6801, 1, 0x0b, &mut host); // 44100 Hz, mono 8-bit
        latch_format(&mut board, &mut host);
        board.write(0x6001, 1, 6, &mut host);
        board.write(0x6801, 1, 0x00, &mut host); // DAC L/R unmuted, full scale
        board.write(0x6001, 1, 7, &mut host);
        board.write(0x6801, 1, 0x00, &mut host);

        // Fill the resampler's ~64-tap history with a loud, constant tone
        // by feeding one loud sample per output frame for well over a
        // kernel window's worth of ticks, checking the last one is loud.
        let mut last = (0.0, 0.0);
        for _ in 0..100 {
            board.write(0x2000, 1, 0xff, &mut host);
            ZorroDevice::tick(&mut board, one_mixer_frame_cck(), &mut host);
            last = host.toccata_audio().next_sample();
        }
        assert!(
            last.0 > 0.9,
            "setup didn't actually produce loud audio: {last:?}"
        );

        board.reset();

        // Reprogram the *same* 44.1 kHz rate reset just cleared and
        // re-latch it, so a stale cache entry (keyed by rate) would be
        // reused rather than sidestepped by a rate change -- but feed no
        // FIFO data, so the freshly reset (silent) codec is the only
        // thing driving output. This tick should produce exact silence --
        // not a fading tail of the pre-reset tone leaking through a
        // resampler whose kernel window still remembers it.
        board.write(0x6001, 1, 8, &mut host);
        board.write(0x6801, 1, 0x0b, &mut host);
        latch_format(&mut board, &mut host);
        ZorroDevice::tick(&mut board, one_mixer_frame_cck(), &mut host);
        let sample = host.toccata_audio().next_sample();
        assert_eq!(
            sample,
            (0.0, 0.0),
            "stale resampler history bled pre-reset audio into post-reset silence"
        );
    }

    #[test]
    fn tick_advances_the_autocalibration_countdown() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut mhi_audio = crate::chipset::paula::MhiAudioRing::default();
        let mut host = DeviceHost::for_slot_with_audio(
            &mut mem,
            0,
            &mut cd_audio,
            &mut toccata_audio,
            &mut mhi_audio,
        );

        board.write(0x6001, 1, 9, &mut host);
        board.write(0x6801, 1, 0x08, &mut host); // request calibration only
        board.write(0x6001, 1, 11, &mut host);
        assert_eq!(board.read(0x6801, 1, &mut host) & 0x20, 0); // not yet busy

        ZorroDevice::tick(&mut board, 227 * 25, &mut host);

        assert_ne!(board.read(0x6801, 1, &mut host) & 0x20, 0); // busy window
    }
}
