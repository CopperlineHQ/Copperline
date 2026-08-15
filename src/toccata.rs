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
use std::collections::HashMap;

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
/// the chip-only [`Ad1848`] core, plus the mixer-rate cadence.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Toccata {
    ad1848: Ad1848,
    /// Mixer-rate accumulator, in units of colour-clocks * MIX_SAMPLE_RATE
    /// -- the exact same accumulator shape `Paula::advance_audio` uses.
    /// One output frame is due each time this reaches PAULA_CLOCK_HZ.
    mixer_acc: u64,
    /// One resampler per codec rate this session has used (at most 14, the
    /// AD1848's legal rate count), so switching back to an already-seen
    /// rate never rebuilds its kernel table. Purely a host-side cache, not
    /// machine state -- rebuilt lazily after a savestate load.
    #[serde(skip)]
    resamplers: HashMap<u32, Resampler>,
}

impl Toccata {
    pub fn new() -> Self {
        Self {
            ad1848: Ad1848::new(),
            mixer_acc: 0,
            resamplers: HashMap::new(),
        }
    }

    /// Advance the mixer-rate cadence by `cck` colour clocks, resampling
    /// the codec's own programmed rate onto the mixer grid and pushing
    /// each produced frame into `ring`. Exact-ratio accumulator, so it
    /// never drifts against `Paula::advance_audio`'s own.
    fn advance_mixer(&mut self, cck: u32, ring: &mut crate::chipset::paula::ToccataAudioRing) {
        self.mixer_acc += u64::from(cck) * u64::from(MIX_SAMPLE_RATE);
        while self.mixer_acc >= u64::from(PAULA_CLOCK_HZ) {
            self.mixer_acc -= u64::from(PAULA_CLOCK_HZ);
            // Disjoint field borrows: the resampler cache and the chip it
            // pulls from via the refill closure below.
            let Self {
                ad1848, resamplers, ..
            } = self;
            let rate = ad1848.sample_rate_hz();
            let resampler = resamplers
                .entry(rate)
                .or_insert_with(|| Resampler::new(rate, MIX_SAMPLE_RATE));
            let (left, right) = resampler.next(|| ad1848.produce_one_sample());
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
        self.advance_mixer(cck, host.toccata_audio());
    }

    fn int6_line(&self) -> bool {
        self.ad1848.int6_pending()
    }

    fn reset(&mut self) {
        self.ad1848.reset();
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

    /// Colour clocks needed to cross one mixer-rate (44.1 kHz) frame
    /// boundary from a zero accumulator: `ceil(PAULA_CLOCK_HZ / MIX_SAMPLE_RATE)`.
    fn one_mixer_frame_cck() -> u32 {
        PAULA_CLOCK_HZ.div_ceil(MIX_SAMPLE_RATE)
    }

    #[test]
    fn tick_at_the_mixer_native_rate_reaches_the_ring_as_a_near_pass_through() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut host =
            DeviceHost::for_slot_with_audio(&mut mem, 0, &mut cd_audio, &mut toccata_audio);

        board.write(0x0000, 1, 0x14, &mut host); // ACTIVE | FIFO_PLAY
        board.write(0x6001, 1, 8, &mut host);
        board.write(0x6801, 1, 0x0b, &mut host); // divider idx 5, crystal1 -> 44100 Hz, mono 8-bit
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
    fn tick_advances_the_autocalibration_countdown() {
        let mut board = Toccata::new();
        let mut mem = memory();
        let mut cd_audio = crate::chipset::paula::CdAudioRing::default();
        let mut toccata_audio = crate::chipset::paula::ToccataAudioRing::default();
        let mut host =
            DeviceHost::for_slot_with_audio(&mut mem, 0, &mut cd_audio, &mut toccata_audio);

        board.write(0x6001, 1, 9, &mut host);
        board.write(0x6801, 1, 0x08, &mut host); // request calibration only
        board.write(0x6001, 1, 11, &mut host);
        assert_eq!(board.read(0x6801, 1, &mut host) & 0x20, 0); // not yet busy

        ZorroDevice::tick(&mut board, 227 * 25, &mut host);

        assert_ne!(board.read(0x6801, 1, &mut host) & 0x20, 0); // busy window
    }
}
