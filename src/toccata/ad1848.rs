// SPDX-License-Identifier: GPL-3.0-or-later

//! AD1848 codec core, as wired into the MacroSystem Toccata.
//!
//! This is the self-contained chip model used by the Toccata board
//! (`src/toccata.rs`): the AD1848's indexed register file, the board's
//! 1024-byte playback FIFO, the board's own status/control register, and
//! the interrupt condition it feeds. It has no Zorro/bus coupling -- the
//! board wrapper owns address decode and autoconfig; this module owns only
//! register semantics.
//!
//! Modelled against WinUAE/amiberry's `sndboard.cpp` (identical in both
//! trees) as a behavioural oracle, not as a code source. The reference
//! reflects real board revisions with a wrong crystal soldered on some
//! units and MacroSystem's own reg-12 pinning that locks the codec out of
//! the CS4231 extensions -- see the field comments below for the specific
//! quirks preserved on purpose, not fixed as if they were bugs.
//!
//! Record is not implemented (that's M4): the record FIFO, the record
//! interrupt bit, and record-side register semantics exist as stubs that
//! never activate, since nothing ever sets `STATUS_FIFO_RECORD`.

use std::collections::VecDeque;

/// Board-space FIFO capacity in bytes (the IDT7202LA on the real board).
const FIFO_CAPACITY: usize = 1024;
/// Half-full/half-empty threshold, in bytes.
const FIFO_HALF: usize = FIFO_CAPACITY / 2;

/// Board control/status register bits (base+0x0000).
const STATUS_ACTIVE: u8 = 0x01;
const STATUS_RESET: u8 = 0x02;
const STATUS_FIFO_CODEC: u8 = 0x04;
const STATUS_FIFO_RECORD: u8 = 0x08;
const STATUS_FIFO_PLAY: u8 = 0x10;
const STATUS_RECORD_INTENA: u8 = 0x40;
const STATUS_PLAY_INTENA: u8 = 0x80;
/// Status-read bit 7: 1 = no interrupt pending (active low).
const STATUS_READ_INTREQ: u8 = 0x80;
/// Pending-IRQ bits, reused from the same bit numbers as the FIFO enables
/// above -- this is how the reference names them too, not a collision.
const STATUS_READ_PLAY_HALF: u8 = 0x08;
const STATUS_READ_RECORD_HALF: u8 = 0x04;

/// AD1848 rate-generator crystals (Hz). Both exist on real boards: some
/// units have the datasheet-specified 24.576 MHz part, some have a
/// 16.9344 MHz part that was apparently swapped in at the factory. Real
/// hardware only ever has one; the codec doesn't know or care which --
/// only the selected divider matters for the rate it produces.
const CRYSTALS: [u32; 2] = [24_576_000, 16_934_400];
/// Divider select, reg 8 bits 1-3.
const DIVIDERS: [u32; 8] = [3072, 1536, 896, 768, 448, 384, 512, 2560];

/// Coarse "one hsync line" period in colour clocks, used only to pace the
/// auto-calibration busy window (ACI). Not video-standard-exact -- the
/// window is ~20 lines wide and nothing reads it more precisely than
/// "busy" or "not busy".
const HSYNC_CCK: u32 = 227;

/// AD1848 register file, FIFO, and board control/status state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Ad1848 {
    /// Indexed registers 0-15 (indices 16-31 are the CS4231 extension,
    /// unreachable here since reg 12 is pinned to plain-AD1848 mode).
    regs: [u8; 16],
    /// The raw byte last written to the index port. Only its low nibble
    /// selects a register; the rest is stored but not modelled (MCE/TRD/
    /// INIT are not used by this board).
    index: u8,
    /// Board control/status (base+0x0000 write).
    status: u8,
    /// Pending, acknowledged-on-status-read interrupt bits.
    irq: u8,
    /// Edge-latched half-empty(play)/half-full(record) flags, cleared by
    /// FIFO port access rather than by the status-read acknowledge.
    fifo_half: u8,
    /// Which directions the codec has actually started (set by reg 9's
    /// enable bits, cleared wholesale by `codec_stop`).
    active: u8,
    /// Playback FIFO bytes, oldest first.
    fifo: VecDeque<u8>,
    /// The FIFO's length before the most recent drain, so the half-empty
    /// edge (crossing below `FIFO_HALF`) can be detected.
    fifo_len_before_drain: usize,
    /// The last decoded output sample, held across an underrun (real
    /// silicon repeats rather than snapping to zero).
    last_sample: (f32, f32),
    /// Countdown from 50 to 0, decremented roughly once per scanline once
    /// reg 9 bit 3 requests calibration. ACI (reg 11 bit 5) reads busy
    /// while `10 < autocal < 30`.
    autocal: u32,
    hsync_acc: u32,
    /// Left/right DAC output attenuation, recomputed from regs 6/7
    /// whenever they (or any of 2-7) are written.
    left_volume: f32,
    right_volume: f32,
    /// Format/rate latched at the last `codec_start()` (reg 9's
    /// stopped-to-started transition) -- not read live from reg 8.
    /// `produce_one_sample`/`bytes_per_sample`/`sample_rate_hz` all use
    /// these, matching the reference's own `codec_setup()`-on-transition
    /// model: a driver that reprograms reg 8 while already playing has no
    /// effect until the next start, so a rate/format change never splits
    /// a FIFO frame mid-stream on a different byte boundary.
    active_channels: usize,
    active_sixteen_bit: bool,
    active_rate_hz: u32,
}

impl Default for Ad1848 {
    fn default() -> Self {
        let mut regs = [0u8; 16];
        // Aux1/Aux2 inputs and the DAC output start muted (bit 7 set);
        // reg 9's power-on interface config is SDC (single DMA channel,
        // bit 4); reg 12 is pinned to 0x0A on this board from power-on.
        regs[2] = 0x80;
        regs[3] = 0x80;
        regs[4] = 0x80;
        regs[5] = 0x80;
        regs[6] = 0x80;
        regs[7] = 0x80;
        regs[9] = 0x10;
        regs[12] = 0x0a;
        // Never started, so nothing reads these yet -- initialized to
        // what power-on reg 8 (0x00) would decode to, for a sane default.
        let (active_channels, active_sixteen_bit, active_rate_hz) = decode_format(regs[8]);
        Self {
            regs,
            index: 0x40,
            status: 0,
            irq: 0,
            fifo_half: 0,
            active: 0,
            fifo: VecDeque::with_capacity(FIFO_CAPACITY),
            fifo_len_before_drain: 0,
            last_sample: (0.0, 0.0),
            autocal: 0,
            hsync_acc: 0,
            left_volume: 0.0,
            right_volume: 0.0,
            active_channels,
            active_sixteen_bit,
            active_rate_hz,
        }
    }
}

/// Decode reg 8 into (channels, 16-bit?, rate Hz) -- the reference's own
/// `codec_setup()`. Shared by the latch-on-start path and `Ad1848`'s
/// power-on default (which behaves as if `codec_setup()` ran once against
/// reg 8's own power-on value, though nothing plays until a real start).
fn decode_format(reg8: u8) -> (usize, bool, u32) {
    let channels = if reg8 & 0x10 != 0 { 2 } else { 1 };
    let sixteen_bit = reg8 & 0x40 != 0;
    let crystal = CRYSTALS[usize::from(reg8 & 1)];
    let divider = DIVIDERS[usize::from((reg8 >> 1) & 7)];
    let freq = crystal / divider;
    let rate_hz = (freq + 49) / 100 * 100;
    (channels, sixteen_bit, rate_hz)
}

impl Ad1848 {
    pub fn new() -> Self {
        let mut chip = Self::default();
        chip.recalc_volume();
        chip
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The play FIFO's current byte count, for the board wrapper's own
    /// address-decode tests (`src/toccata.rs`) -- not otherwise exposed,
    /// since nothing outside this chip needs to know FIFO occupancy.
    #[cfg(test)]
    pub(crate) fn fifo_len_for_test(&self) -> usize {
        self.fifo.len()
    }

    // -----------------------------------------------------------------
    // AD1848 index/data ports (base+0x6001 / base+0x6801)
    // -----------------------------------------------------------------

    pub fn write_index(&mut self, v: u8) {
        self.index = v;
    }

    pub fn read_index(&self) -> u8 {
        self.index
    }

    /// Only the low 4 bits ever select a register on this board: reg 12
    /// is pinned to plain-AD1848 mode, so the CS4231 index-mask-31
    /// extension (indices 16-31) is never reachable.
    fn effective_index(&self) -> usize {
        usize::from(self.index & 0x0f)
    }

    pub fn write_data(&mut self, v: u8) {
        let idx = self.effective_index();
        match idx {
            8 => {
                // The format-extension bit (0x80, CS4231 big-endian mode)
                // is meaningless with reg 12 pinned off this board, so it
                // is forced clear before storing -- matching the
                // reference rather than leaving stray state a driver
                // could misread back.
                self.regs[8] = v & !0x80;
            }
            9 => self.write_reg9(v),
            12 => self.regs[12] = 0x0a, // hardwired regardless of what's written
            2..=7 => {
                self.regs[idx] = v;
                self.recalc_volume();
            }
            _ => self.regs[idx] = v,
        }
    }

    pub fn read_data(&self) -> u8 {
        let idx = self.effective_index();
        if idx == 11 {
            let busy = self.autocal > 10 && self.autocal < 30;
            if busy {
                self.regs[11] | 0x20
            } else {
                self.regs[11] & !0x20
            }
        } else {
            self.regs[idx]
        }
    }

    fn write_reg9(&mut self, v: u8) {
        let old = self.regs[9];
        if v & 0x08 != 0 {
            self.autocal = 50;
        }
        if old & 0x03 == 0 && v & 0x03 != 0 {
            self.codec_start();
        } else if old & 0x03 != 0 && v & 0x03 == 0 {
            self.codec_stop();
        }
        if v & 0x01 != 0 {
            self.active |= STATUS_FIFO_PLAY;
        } else {
            self.active &= !STATUS_FIFO_PLAY;
        }
        if v & 0x02 != 0 {
            self.active |= STATUS_FIFO_RECORD;
        } else {
            self.active &= !STATUS_FIFO_RECORD;
        }
        self.regs[9] = v;
    }

    fn codec_start(&mut self) {
        // Latch the currently-programmed format/rate; see the doc comment
        // on the active_* fields for why this happens here and not on
        // every produced sample.
        let (channels, sixteen_bit, rate_hz) = decode_format(self.regs[8]);
        self.active_channels = channels;
        self.active_sixteen_bit = sixteen_bit;
        self.active_rate_hz = rate_hz;
    }

    fn codec_stop(&mut self) {
        self.active = 0;
    }

    /// Whether the codec has been started for playback (reg 9 bit 0's
    /// last stopped-to-started transition). The board wrapper's own
    /// codec-rate cadence only runs while this is true -- a real codec
    /// isn't converting anything, and has nothing to latch a format
    /// from, until it has actually been started.
    pub fn play_active(&self) -> bool {
        self.active & STATUS_FIFO_PLAY != 0
    }

    /// The last decoded output sample, at its current DAC volume, with no
    /// side effects (does not drain the FIFO, does not touch the
    /// half-empty latch or the interrupt condition). Used as the
    /// resampler's fallback when it asks for more input frames than the
    /// codec's own causally-paced cadence (`Toccata::advance_codec`) has
    /// produced yet, so a resampler's internal lookahead/priming can
    /// never pull `produce_one_sample`'s side effects out of order.
    ///
    /// Only valid while the codec is actively playing: once stopped,
    /// `advance_codec` no longer runs at all, so without this guard the
    /// resampler would keep pulling this same fallback and hold the final
    /// decoded sample as a constant DC offset for the rest of the capture
    /// -- forever, not just across one underrun. A real codec's DAC line
    /// may hold that voltage too, but nothing downstream of it (the mixer,
    /// the master mix, a stem capture) should still be observing a stopped
    /// channel's last note.
    pub fn peek_last_sample(&self) -> (f32, f32) {
        if !self.play_active() {
            return (0.0, 0.0);
        }
        let (l, r) = self.last_sample;
        (l * self.left_volume, r * self.right_volume)
    }

    /// Bytes currently buffered in the playback FIFO, for the debugger.
    pub fn fifo_len(&self) -> usize {
        self.fifo.len()
    }

    /// The playback FIFO's capacity in bytes, for the debugger.
    pub fn fifo_capacity(&self) -> usize {
        FIFO_CAPACITY
    }

    /// The format latched at the last codec start, as (channels, 16-bit?),
    /// for the debugger.
    pub fn active_format(&self) -> (usize, bool) {
        (self.active_channels, self.active_sixteen_bit)
    }

    // -----------------------------------------------------------------
    // Board control/status (base+0x0000)
    // -----------------------------------------------------------------

    pub fn write_control(&mut self, v: u8) {
        let mut v = v;
        if v & STATUS_RESET != 0 {
            self.codec_stop();
            self.status = 0;
            self.irq = 0;
            v = 0;
        }
        // The FIFO-flush idiom is a write of *exactly* 0x01 -- not
        // "ACTIVE plus anything else" -- so it only ever fires from this
        // specific byte value, matching the reference precisely.
        if v == STATUS_ACTIVE {
            self.fifo.clear();
            self.fifo_len_before_drain = 0;
            self.status = 0;
            self.irq = 0;
            self.fifo_half = 0;
        }
        self.status = v;
    }

    /// Reading status acknowledges every pending interrupt bit.
    pub fn read_status(&mut self) -> u8 {
        let mut v = STATUS_READ_INTREQ;
        if self.irq != 0 {
            v &= !STATUS_READ_INTREQ;
            v |= self.irq;
            self.irq = 0;
        }
        v
    }

    pub fn int6_pending(&self) -> bool {
        self.irq != 0
    }

    // -----------------------------------------------------------------
    // FIFO port (base+0x2000)
    // -----------------------------------------------------------------

    /// One byte pushed into the play FIFO. The board decomposes word/long
    /// writes into successive byte pokes at this same port (big-endian
    /// order), so a 16-bit sample write already arrives here one byte at
    /// a time -- callers do not assemble words themselves.
    pub fn write_fifo_byte(&mut self, v: u8) {
        if self.status & STATUS_FIFO_PLAY != 0 && self.fifo.len() < FIFO_CAPACITY {
            self.fifo.push_back(v);
        }
        // Real silicon can't overflow (per the FIFO's own datasheet), so
        // a full FIFO just silently drops the byte -- no error flag.
        self.irq &= !STATUS_READ_PLAY_HALF;
        self.fifo_half &= !STATUS_FIFO_PLAY;
    }

    /// The record FIFO is never filled (record is M4), so a read always
    /// returns stale/zero data; the port still exists and still
    /// acknowledges the record-half interrupt bit on access, matching
    /// hardware, so a driver polling it before record support lands sees
    /// consistent (if silent) behaviour.
    pub fn read_fifo_byte(&mut self) -> u8 {
        self.irq &= !STATUS_READ_RECORD_HALF;
        self.fifo_half &= !STATUS_FIFO_RECORD;
        0
    }

    // -----------------------------------------------------------------
    // Playback: format, cadence, and one produced sample
    // -----------------------------------------------------------------

    fn bytes_per_sample(&self) -> usize {
        (if self.active_sixteen_bit { 2 } else { 1 }) * self.active_channels
    }

    /// The codec's *active* (latched-at-start) playback rate, in Hz,
    /// rounded to the nearest 100 Hz the way the reference does. Two of
    /// the sixteen crystal/divider combinations (24.576 MHz with divider
    /// 448 or 384) fall outside a real AD1848's documented rate table but
    /// are not rejected here, matching the reference's own permissiveness.
    pub fn sample_rate_hz(&self) -> u32 {
        self.active_rate_hz
    }

    fn recalc_volume(&mut self) {
        self.left_volume = Self::dac_attenuation(self.regs[6]);
        self.right_volume = Self::dac_attenuation(self.regs[7]);
    }

    /// Reg 6/7 (DAC output attenuation): 6-bit value plus a mute bit.
    /// 0 is full scale, 63 is minimum before mute; bit 7 mutes outright.
    fn dac_attenuation(v: u8) -> f32 {
        if v & 0x80 != 0 {
            return 0.0;
        }
        let steps = 64 - u32::from(v & 0x3f);
        (steps * 512) as f32 / 32768.0
    }

    /// Advance the auto-calibration countdown by `cck` colour clocks.
    /// Independent of the playback cadence below -- ACI is a coarse,
    /// line-paced status bit, not a sample-rate one.
    pub fn advance_cck(&mut self, cck: u32) {
        if self.autocal == 0 {
            return;
        }
        self.hsync_acc += cck;
        while self.hsync_acc >= HSYNC_CCK && self.autocal > 0 {
            self.hsync_acc -= HSYNC_CCK;
            self.autocal -= 1;
        }
    }

    /// Produce one stereo sample at the codec's own programmed rate:
    /// drain `bytes_per_sample` bytes from the FIFO (or repeat the last
    /// sample on underrun), apply DAC volume, update the half-empty
    /// latch, and re-evaluate the interrupt condition. This is the exact
    /// per-sample unit the reference's own audio callback produces --
    /// callers drive it once per codec-rate tick (see `Toccata::tick`),
    /// not once per mixer-rate tick.
    pub fn produce_one_sample(&mut self) -> (f32, f32) {
        self.fifo_len_before_drain = self.fifo.len();
        let need = self.bytes_per_sample();
        if self.fifo.len() >= need {
            let mut bytes = [0u8; 4];
            for slot in bytes.iter_mut().take(need) {
                *slot = self.fifo.pop_front().expect("checked len above");
            }
            self.last_sample = self.decode_sample(&bytes[..need]);
        } else {
            // Underrun: any partial residue is discarded (it can never
            // complete a sample), and the last good sample repeats.
            self.fifo.clear();
        }
        self.raise_half_empty_if_crossed();
        self.evaluate_interrupt();
        let (l, r) = self.last_sample;
        (l * self.left_volume, r * self.right_volume)
    }

    /// Decode one already-drained frame's worth of FIFO bytes. 8-bit
    /// samples are unsigned linear PCM (AD1848's native 8-bit format);
    /// 16-bit samples are signed linear PCM, little-endian in the FIFO
    /// (the board always writes/reads this way -- the CS4231 big-endian
    /// mode this bit would otherwise select is unreachable, see
    /// `write_data`'s reg-8 handling).
    fn decode_sample(&self, bytes: &[u8]) -> (f32, f32) {
        if self.active_sixteen_bit {
            let l = i16::from_le_bytes([bytes[0], bytes[1]]);
            let r = if self.active_channels == 2 {
                i16::from_le_bytes([bytes[2], bytes[3]])
            } else {
                l
            };
            (f32::from(l) / 32768.0, f32::from(r) / 32768.0)
        } else {
            let l = (f32::from(bytes[0]) - 128.0) / 128.0;
            let r = if self.active_channels == 2 {
                (f32::from(bytes[1]) - 128.0) / 128.0
            } else {
                l
            };
            (l, r)
        }
    }

    fn raise_half_empty_if_crossed(&mut self) {
        if self.fifo.len() < FIFO_HALF && self.fifo_len_before_drain >= FIFO_HALF {
            self.fifo_half |= STATUS_FIFO_PLAY;
        }
    }

    fn evaluate_interrupt(&mut self) {
        if self.active == 0 || self.status & STATUS_FIFO_CODEC == 0 {
            return;
        }
        if self.fifo_half & STATUS_FIFO_PLAY != 0
            && self.status & STATUS_PLAY_INTENA != 0
            && self.status & STATUS_FIFO_PLAY != 0
        {
            self.irq |= STATUS_READ_PLAY_HALF;
        }
        if self.fifo_half & STATUS_FIFO_RECORD != 0
            && self.status & STATUS_RECORD_INTENA != 0
            && self.status & STATUS_FIFO_RECORD != 0
        {
            self.irq |= STATUS_READ_RECORD_HALF;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_word_be(chip: &mut Ad1848, word: u16) {
        chip.write_fifo_byte((word >> 8) as u8);
        chip.write_fifo_byte(word as u8);
    }

    fn enable_play(chip: &mut Ad1848) {
        chip.write_control(STATUS_ACTIVE | STATUS_FIFO_CODEC | STATUS_FIFO_PLAY);
    }

    /// Latch whatever format/rate is currently in reg 8 by driving reg 9
    /// through a clean stopped-to-started transition (`codec_start`),
    /// matching what a real driver does: program the format, then enable
    /// playback. Always writes a stop first so a second call in the same
    /// test is a genuine rising edge, not a no-op repeat of an
    /// already-set bit.
    fn latch_format(chip: &mut Ad1848) {
        chip.write_index(9);
        chip.write_data(0x10); // stop (SDC bit only, no play/record enable)
        chip.write_data(0x11); // play enable: rising edge -> codec_start()
    }

    #[test]
    fn reset_defaults_mute_dac_and_aux_and_pin_reg12() {
        let chip = Ad1848::new();
        assert_eq!(chip.regs[6], 0x80);
        assert_eq!(chip.regs[7], 0x80);
        assert_eq!(chip.regs[12], 0x0a);
        assert_eq!(chip.left_volume, 0.0);
        assert_eq!(chip.right_volume, 0.0);
    }

    #[test]
    fn index_and_data_round_trip() {
        let mut chip = Ad1848::new();
        chip.write_index(0x06);
        assert_eq!(chip.read_index(), 0x06);
        chip.write_data(0x00); // full volume, unmuted
        assert_eq!(chip.read_data(), 0x00);
        assert!(chip.left_volume > 0.0);
    }

    #[test]
    fn reg12_is_always_pinned_regardless_of_what_is_written() {
        let mut chip = Ad1848::new();
        chip.write_index(12);
        chip.write_data(0xff);
        assert_eq!(chip.read_data(), 0x0a);
        chip.write_data(0x00);
        assert_eq!(chip.read_data(), 0x0a);
    }

    #[test]
    fn reg8_format_extension_bit_is_forced_off() {
        let mut chip = Ad1848::new();
        chip.write_index(8);
        chip.write_data(0xff);
        assert_eq!(chip.read_data(), !0x80);
    }

    #[test]
    fn reg8_rate_table_matches_the_reference() {
        let mut chip = Ad1848::new();
        // v = (divider_index << 1) | crystal_bit, matching write_data's
        // `(regs[8] >> 1) & 7` divider select and `regs[8] & 1` crystal
        // select decode.
        // Each check writes reg 8 then latches it via a stop/start
        // transition (see `latch_format`): sample_rate_hz() reports the
        // *active* rate, which only updates on codec_start().
        let set_reg8 = |chip: &mut Ad1848, divider_index: u8, crystal_bit: u8| {
            chip.write_index(8);
            chip.write_data((divider_index << 1) | crystal_bit);
            latch_format(chip);
        };
        set_reg8(&mut chip, 0, 0); // crystal0 (24576000) / div 3072
        assert_eq!(chip.sample_rate_hz(), 8000);
        set_reg8(&mut chip, 7, 1); // crystal1 (16934400) / div 2560 = 6615 -> 6600
        assert_eq!(chip.sample_rate_hz(), 6600);
        set_reg8(&mut chip, 5, 1); // crystal1 / div 384 = 44100
        assert_eq!(chip.sample_rate_hz(), 44100);
        set_reg8(&mut chip, 6, 0); // crystal0 / div 512 = 48000
        assert_eq!(chip.sample_rate_hz(), 48000);
    }

    #[test]
    fn status_write_reset_zeroes_status_and_irq_but_not_the_fifo() {
        let mut chip = Ad1848::new();
        enable_play(&mut chip);
        chip.write_fifo_byte(0x42);
        chip.write_control(STATUS_RESET);
        assert_eq!(chip.status, 0);
        assert_eq!(chip.irq, 0);
        assert_eq!(
            chip.fifo.len(),
            1,
            "reset must not clear buffered FIFO data"
        );
    }

    #[test]
    fn status_write_of_exactly_one_is_the_fifo_flush_idiom() {
        let mut chip = Ad1848::new();
        enable_play(&mut chip);
        chip.write_fifo_byte(0x42);
        chip.write_control(STATUS_ACTIVE); // == 0x01 exactly
        assert!(chip.fifo.is_empty());
        assert_eq!(chip.fifo_half, 0);

        // ACTIVE combined with any other bit does NOT trigger the flush.
        let mut chip2 = Ad1848::new();
        enable_play(&mut chip2);
        chip2.write_fifo_byte(0x42);
        chip2.write_control(STATUS_ACTIVE | STATUS_FIFO_CODEC);
        assert_eq!(chip2.fifo.len(), 1);
    }

    #[test]
    fn status_read_is_the_interrupt_acknowledge() {
        let mut chip = Ad1848::new();
        chip.irq = STATUS_READ_PLAY_HALF;
        assert_eq!(chip.read_status(), STATUS_READ_PLAY_HALF);
        assert_eq!(chip.irq, 0);
        assert_eq!(chip.read_status(), STATUS_READ_INTREQ);
    }

    #[test]
    fn fifo_write_is_dropped_when_play_is_not_enabled() {
        let mut chip = Ad1848::new();
        chip.write_control(STATUS_ACTIVE | STATUS_FIFO_CODEC); // no FIFO_PLAY bit
        chip.write_fifo_byte(0x99);
        assert!(chip.fifo.is_empty());
    }

    #[test]
    fn fifo_overflow_silently_drops_bytes() {
        let mut chip = Ad1848::new();
        enable_play(&mut chip);
        for i in 0..FIFO_CAPACITY + 10 {
            chip.write_fifo_byte(i as u8);
        }
        assert_eq!(chip.fifo.len(), FIFO_CAPACITY);
    }

    #[test]
    fn half_empty_is_edge_triggered_on_the_downward_crossing() {
        let mut chip = Ad1848::new();
        enable_play(&mut chip);
        chip.write_index(8);
        chip.write_data(0x00); // mono 8-bit: 1 byte/sample
        chip.write_index(6);
        chip.write_data(0x00); // unmute DAC L
        chip.write_index(7);
        chip.write_data(0x00); // unmute DAC R
        for _ in 0..FIFO_CAPACITY {
            chip.write_fifo_byte(0x80);
        }
        // Draining down to exactly half should not yet cross (>= half).
        for _ in 0..FIFO_HALF {
            chip.produce_one_sample();
        }
        assert_eq!(chip.fifo_half & STATUS_FIFO_PLAY, 0);
        // One more sample crosses below half.
        chip.produce_one_sample();
        assert_ne!(chip.fifo_half & STATUS_FIFO_PLAY, 0);
    }

    #[test]
    fn underrun_repeats_the_last_sample_without_advancing() {
        let mut chip = Ad1848::new();
        enable_play(&mut chip);
        chip.write_index(8);
        chip.write_data(0x00); // mono 8-bit
        chip.write_index(6);
        chip.write_data(0x00);
        chip.write_index(7);
        chip.write_data(0x00);
        chip.write_fifo_byte(0xff); // one full sample: (255-128)/128 ~= 0.992
        let first = chip.produce_one_sample();
        assert!(first.0 > 0.9);
        let repeated = chip.produce_one_sample(); // FIFO now empty: underrun
        assert_eq!(repeated, first);
    }

    #[test]
    fn peek_last_sample_falls_silent_once_the_codec_stops() {
        let mut chip = Ad1848::new();
        enable_play(&mut chip);
        chip.write_index(8);
        chip.write_data(0x00); // mono 8-bit
        chip.write_index(6);
        chip.write_data(0x00);
        chip.write_index(7);
        chip.write_data(0x00);
        latch_format(&mut chip); // reg 9 rising edge -> codec_start(), play_active() true
        chip.write_fifo_byte(0xff); // one full sample: (255-128)/128 ~= 0.992
        let last = chip.produce_one_sample();
        assert!(last.0 > 0.9);
        assert!(chip.play_active());
        assert_eq!(chip.peek_last_sample(), last);
        // Stopping playback (reg 9 bit 0 cleared) must not leave the
        // resampler's fallback holding this sample as a permanent DC
        // offset.
        chip.write_index(9);
        chip.write_data(0x10); // stop: SDC bit only, play/record enable cleared
        assert!(!chip.play_active());
        assert_eq!(chip.peek_last_sample(), (0.0, 0.0));
    }

    #[test]
    fn play_active_clears_when_only_play_enable_drops_with_capture_still_set() {
        // Regression for a latched-active bug: `write_reg9` used to only
        // clear `active` (and so `play_active()`) when both PEN and CEN
        // dropped to zero together, via the full `codec_stop()` path. A
        // driver that disables playback (PEN) while leaving capture (CEN)
        // enabled -- 0x03 -> 0x02 -- never took that path, so
        // `play_active()` stayed stuck true and the mixer fallback kept
        // emitting the final sample forever, exactly like the bug this
        // whole fix addresses.
        let mut chip = Ad1848::new();
        chip.write_index(9);
        chip.write_data(0x03); // PEN + CEN both enabled
        assert!(chip.play_active());
        chip.write_data(0x02); // PEN cleared, CEN still set
        assert!(!chip.play_active());
    }

    #[test]
    fn sixteen_bit_stereo_is_little_endian_and_byte_swapped_on_write() {
        let mut chip = Ad1848::new();
        enable_play(&mut chip);
        chip.write_index(8);
        chip.write_data(0x50); // stereo (0x10) + 16-bit (0x40)
        latch_format(&mut chip);
        chip.write_index(6);
        chip.write_data(0x00);
        chip.write_index(7);
        chip.write_data(0x00);
        // A native `move.w #0x1234` decomposes into big-endian byte pokes
        // at the FIFO port (0x12 then 0x34), but the FIFO is read back
        // little-endian (fifo[i+1]<<8 | fifo[i]) -- so the codec actually
        // sees 0x3412, not 0x1234. This is the documented real-hardware
        // quirk a driver must byte-swap 16-bit samples to work around
        // before writing them; it is not a bug to "fix" here.
        write_word_be(&mut chip, 0x1234);
        write_word_be(&mut chip, 0x0001);
        let (l, r) = chip.produce_one_sample();
        assert!((l - f32::from(0x3412u16 as i16) / 32768.0).abs() < 1e-6);
        assert!((r - f32::from(0x0100u16 as i16) / 32768.0).abs() < 1e-6);
    }

    #[test]
    fn interrupt_needs_codec_active_gate_and_both_direction_enables() {
        let mut chip = Ad1848::new();
        chip.write_index(8);
        chip.write_data(0x00);
        chip.write_index(9);
        chip.write_data(0x01); // playback enable -> codec_start, active bit set
                               // FIFO_CODEC gate missing: no interrupt even with the FIFO drained dry.
        chip.write_control(STATUS_ACTIVE | STATUS_PLAY_INTENA | STATUS_FIFO_PLAY);
        chip.produce_one_sample();
        assert!(!chip.int6_pending());
        // Now with the gate and both enables present, an underrun/half-empty
        // condition raises the interrupt.
        chip.write_control(
            STATUS_ACTIVE | STATUS_FIFO_CODEC | STATUS_PLAY_INTENA | STATUS_FIFO_PLAY,
        );
        chip.fifo_half |= STATUS_FIFO_PLAY;
        chip.produce_one_sample();
        assert!(chip.int6_pending());
    }

    #[test]
    fn aci_reads_busy_only_during_the_calibration_window() {
        let mut chip = Ad1848::new();
        chip.write_index(9);
        chip.write_data(0x08); // bit3 only: request calibration, no start/stop edge
        chip.write_index(11);
        assert_eq!(chip.read_data() & 0x20, 0); // not yet busy (autocal == 50)
        chip.advance_cck(HSYNC_CCK * 25); // into the 10..30 busy window
        assert_ne!(chip.read_data() & 0x20, 0);
        chip.advance_cck(HSYNC_CCK * 25); // past it
        assert_eq!(chip.read_data() & 0x20, 0);
    }

    #[test]
    fn savestate_round_trips_register_and_fifo_state() {
        let mut chip = Ad1848::new();
        enable_play(&mut chip);
        chip.write_index(6);
        chip.write_data(0x10);
        chip.write_fifo_byte(0xaa);
        chip.write_fifo_byte(0xbb);
        let bytes = bincode::serialize(&chip).unwrap();
        let resumed: Ad1848 = bincode::deserialize(&bytes).unwrap();
        assert_eq!(resumed.regs[6], 0x10);
        assert_eq!(resumed.fifo.len(), 2);
    }
}
